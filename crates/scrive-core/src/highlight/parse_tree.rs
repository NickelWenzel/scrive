//! The tree-sitter highlight backend: an incrementally reparsed syntax tree,
//! queried for spans only on the window rows whose highlighting may have
//! changed.

use core::ops::Range;

use tree_sitter::{InputEdit, Parser, QueryCursor, Tree};

use super::capture_paint;
use super::dirty_ranges::DirtyRanges;
use super::rope_text::{self, RopeText};
use super::tree_sitter_def::TreeSitterDef;
use super::{padded_highlight_window, HighlightSpan, SpanStyle, TokenTheme};
use super::{HIGHLIGHT_MAX_WINDOW_ROWS, HIGHLIGHT_WINDOW_SLACK};
use crate::buffer::Buffer;
use crate::coords::Point;
use crate::patch::Edit;
use crate::transaction::Committed;

/// The tree-sitter cache. It keeps the document's syntax tree, edited on
/// every commit and reparsed on the next [`Cache::tokenize`], and spans for
/// the retention window only (the same window the line-state cache keeps,
/// through [`padded_highlight_window`]).
///
/// A window row is queried again when it has no spans (edited, or newly in
/// the window) or is dirty: its old spans are kept as a stale fallback after
/// a theme swap or when the reparse reports its syntax changed. Dirt is
/// tracked for window rows only; a row outside the window has nothing to
/// repaint.
pub(super) struct Cache {
    parser: Parser,
    def: TreeSitterDef,
    /// The theme's style per query capture index.
    styles: Vec<Option<SpanStyle>>,
    /// `None` until the first parse.
    tree: Option<Tree>,
    /// The tree is behind the buffer: edited through the commits since the
    /// last parse, but not yet reparsed.
    reparse: bool,
    /// Window rows whose spans are stale.
    dirty: DirtyRanges,
    n_lines: u32,
    /// The rows last handed to [`Cache::set_window`], before padding.
    aim: Range<u32>,
    win: Range<u32>,
    win_spans: Vec<Option<Vec<HighlightSpan>>>,
    cursor: QueryCursor,
    #[cfg(test)]
    parses: u32,
}

impl core::fmt::Debug for Cache {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Cache")
            .field("lines", &self.n_lines)
            .field("window", &self.win)
            .field("parsed", &self.tree.is_some())
            .field("reparse", &self.reparse)
            .field("dirty", &self.dirty)
            .finish_non_exhaustive()
    }
}

impl Cache {
    /// A cache for an `n_lines` document, nothing parsed yet, its window at
    /// the document top.
    pub(super) fn new(def: TreeSitterDef, theme: &TokenTheme, n_lines: u32) -> Self {
        let win = 0..(2 * HIGHLIGHT_WINDOW_SLACK).min(n_lines);
        Self {
            parser: def.parser(),
            styles: def.styles(theme),
            def,
            tree: None,
            reparse: true,
            dirty: DirtyRanges::default(),
            n_lines,
            aim: win.clone(),
            win_spans: vec![None; win.len()],
            win,
            cursor: QueryCursor::new(),
            #[cfg(test)]
            parses: 0,
        }
    }

    /// Shift the cache through `committed`, whose edits `buffer` already
    /// holds; `spans` are its line splices
    /// ([`line_splices`](super::splice::line_splices)).
    ///
    /// The tree is edited once per patch edit, in ascending order, each in
    /// the tree's coordinates at that point: the edits before it are already
    /// applied, so its start and new end are post-commit offsets (`edit.new`)
    /// and its old end is `edit.new.start + edit.old.len()`. Positions before
    /// the new end are final, so the buffer supplies them; the old end
    /// position is the start advanced over the replaced text, which only the
    /// paired inverse op (`inverse_ops()[i]` undoes `edits()[i]`) still holds.
    pub(super) fn on_commit(&mut self, buffer: &Buffer, committed: &Committed, spans: &[(u32, u32, u32)]) {
        if let Some(tree) = self.tree.as_mut() {
            let edits = committed.patch().edits();
            let inverse = committed.inverse_ops();
            debug_assert_eq!(edits.len(), inverse.len(), "a commit carries one inverse op per edit");
            for (edit, inv) in edits.iter().zip(inverse) {
                tree.edit(&input_edit(buffer, edit, &inv.text));
            }
        }
        self.reparse = true;
        let delta: i64 = spans.iter().map(|&(_, o, n)| i64::from(n) - i64::from(o)).sum();
        self.n_lines = (i64::from(self.n_lines) + delta) as u32;
        self.shift_window(spans);
        self.dirty.apply_splices(spans);
        self.clip_dirty();
    }

    /// Reparse if the buffer moved on, then query the window rows that need
    /// it, at most `max_lines` rows; returns how many rows were queried.
    ///
    /// The work is bounded by the window, so unlike the line-state cache
    /// there is no target row to stop at.
    pub(super) fn tokenize(&mut self, buffer: &Buffer, max_lines: u32) -> u32 {
        if self.reparse {
            self.parse(buffer);
        }
        let end = self.win.end.min(self.n_lines);
        let mut work = 0;
        let mut from = self.win.start;
        while work < max_lines {
            let Some(start) = (from..end).find(|&r| self.needs_query(r)) else { break };
            let mut stop = start + 1;
            while stop < end && stop - start < max_lines - work && self.needs_query(stop) {
                stop += 1;
            }
            self.query(buffer, start..stop);
            work += stop - start;
            from = stop;
        }
        work
    }

    /// The first window row [`Cache::tokenize`] would query, or the window
    /// top while a reparse is due; `None` when idle.
    pub(super) fn pending(&self) -> Option<u32> {
        let next = self.first_needing_query();
        if self.reparse {
            Some(next.unwrap_or(self.win.start.min(self.n_lines - 1)))
        } else {
            next
        }
    }

    /// The spans of `row`, possibly stale, or `None` if it isn't queried or
    /// retained.
    pub(super) fn line_spans(&self, row: u32) -> Option<&[HighlightSpan]> {
        self.win_spans[self.win_idx(row)?].as_deref()
    }

    pub(super) fn line_count(&self) -> u32 {
        self.n_lines
    }

    pub(super) fn window_aim(&self) -> Range<u32> {
        self.aim.clone()
    }

    /// Aim the window at the viewport `rows`. Rows entering it are queried
    /// on the next [`Cache::tokenize`].
    pub(super) fn set_window(&mut self, rows: Range<u32>) {
        self.aim = rows.clone();
        let win = padded_highlight_window(rows, self.n_lines);
        if win == self.win {
            return;
        }
        let mut spans = vec![None; win.len()];
        for row in win.start.max(self.win.start)..win.end.min(self.win.end) {
            spans[(row - win.start) as usize] = self.win_spans[(row - self.win.start) as usize].take();
        }
        self.win = win;
        self.win_spans = spans;
        self.clip_dirty();
    }

    /// Restyle with `theme`. The tree is unchanged, so only the window is
    /// queried again; its old spans show until then.
    pub(super) fn set_theme(&mut self, theme: &TokenTheme) {
        self.styles = self.def.styles(theme);
        self.dirty.insert_range(self.win.start..self.win.end.min(self.n_lines));
    }

    fn parse(&mut self, buffer: &Buffer) {
        let rope = buffer.rope();
        let old = self.tree.take();
        let new = self
            .parser
            .parse_with_options(&mut |byte, _| rope_text::chunk_from(rope, byte), old.as_ref(), None)
            .expect("a parse with no cancellation always finishes");
        #[cfg(test)]
        {
            self.parses += 1;
        }
        let window = self.win.start..self.win.end.min(self.n_lines);
        match old {
            // `changed_ranges` misses a text-only change inside a multi-row
            // node whose structure didn't change, so a `#match?` / `#eq?` on
            // that node can leave stale spans on its rows outside the edit.
            // Harmless for tree-sitter-rust, whose predicates sit on
            // single-row tokens; a grammar that needs it would invalidate the
            // edited ancestor's full row extent.
            Some(old) => {
                for r in old.changed_ranges(&new) {
                    let (start, end) = (r.start_point, r.end_point);
                    // A range ending at column 0 doesn't reach its end row.
                    let past_end = end.row as u32 + u32::from(end.column > 0 || end.row == start.row);
                    let rows = start.row as u32..past_end;
                    self.dirty.insert_range(rows.start.max(window.start)..rows.end.min(window.end));
                }
            }
            None => self.dirty.insert_range(window),
        }
        self.tree = Some(new);
        self.reparse = false;
    }

    /// Query and paint window rows `rows`, which the tree is current for.
    fn query(&mut self, buffer: &Buffer, rows: Range<u32>) {
        let tree = self.tree.as_ref().expect("`tokenize` parses before it queries");
        let len = buffer.len();
        let row_starts: Vec<u32> = (rows.start..=rows.end)
            .map(|r| if r < self.n_lines { buffer.point_to_offset(Point::new(r, 0)) } else { len + 1 })
            .collect();
        let bytes = row_starts[0]..row_starts[row_starts.len() - 1] - 1;
        // An empty range would query the whole document (tree-sitter reads an
        // end of 0 as unbounded); the one empty row it covers has no spans.
        let captures = if bytes.is_empty() {
            Vec::new()
        } else {
            self.cursor.set_byte_range(bytes.start as usize..bytes.end as usize);
            let rope = RopeText(buffer.rope());
            capture_paint::collect(&mut self.cursor, self.def.query(), tree.root_node(), rope)
        };
        let painted = capture_paint::paint_rows(captures, &row_starts, &self.styles);
        for (row, spans) in rows.clone().zip(painted) {
            let i = (row - self.win.start) as usize;
            self.win_spans[i] = Some(spans);
        }
        self.dirty.clear_range(rows);
    }

    fn needs_query(&self, row: u32) -> bool {
        self.win_spans[(row - self.win.start) as usize].is_none() || self.dirty.contains(row)
    }

    fn first_needing_query(&self) -> Option<u32> {
        let end = self.win.end.min(self.n_lines);
        let gap = (self.win.start..end).find(|&r| self.win_spans[(r - self.win.start) as usize].is_none());
        match (self.dirty.first(), gap) {
            (Some(d), Some(g)) => Some(d.min(g)),
            (d, g) => d.or(g),
        }
    }

    fn win_idx(&self, row: u32) -> Option<usize> {
        (self.win.contains(&row) && row < self.n_lines).then(|| (row - self.win.start) as usize)
    }

    /// Drop dirt outside the window, where no spans are kept to repaint.
    fn clip_dirty(&mut self) {
        self.dirty.clear_range(0..self.win.start);
        self.dirty.clear_range(self.win.end..u32::MAX);
    }

    /// Move the window through the line splices, keeping it on the same
    /// text: rows outside every edit keep their spans at their shifted rows,
    /// edited rows lose theirs, and the window grows or shrinks with the
    /// edits inside it (capped at [`HIGHLIGHT_MAX_WINDOW_ROWS`]). `n_lines`
    /// is already the post-commit count.
    fn shift_window(&mut self, spans: &[(u32, u32, u32)]) {
        let mut pref: Vec<i64> = Vec::with_capacity(spans.len() + 1);
        pref.push(0);
        for &(_, o, n) in spans {
            pref.push(pref[pref.len() - 1] + i64::from(n) - i64::from(o));
        }
        // A pre-edit row outside every edit moves by the edits above it.
        let first_not_above = |pre: u32| spans.partition_point(|s| s.0 + s.1 <= pre);
        let post = |pre: u32| (i64::from(pre) + pref[first_not_above(pre)]) as u32;
        let edited = |pre: u32| {
            let i = first_not_above(pre);
            i < spans.len() && pre >= spans[i].0
        };

        let old = self.win.clone();
        let new_start = post(old.start).min(self.n_lines);
        if old.is_empty() {
            self.win = new_start..new_start;
            self.win_spans.clear();
            return;
        }
        let mut last = new_start;
        let mut survivors = Vec::with_capacity(self.win_spans.len());
        for (pre, sp) in old.clone().zip(std::mem::take(&mut self.win_spans)) {
            if !edited(pre) {
                last = last.max(post(pre));
                survivors.push((post(pre), sp));
            }
        }
        for (j, &(ps, o, n)) in spans.iter().enumerate() {
            if n > 0 && ps < old.end && ps + o > old.start {
                last = last.max((i64::from(ps) + pref[j]) as u32 + n - 1);
            }
        }
        let end = (last + 1).min(new_start + HIGHLIGHT_MAX_WINDOW_ROWS).min(self.n_lines);
        let mut win_spans = vec![None; (end - new_start) as usize];
        for (row, sp) in survivors {
            if (new_start..end).contains(&row) {
                win_spans[(row - new_start) as usize] = sp;
            }
        }
        self.win = new_start..end;
        self.win_spans = win_spans;
    }

    #[cfg(test)]
    fn parse_count(&self) -> u32 {
        self.parses
    }
}

/// The tree edit for one patch edit; see [`Cache::on_commit`] for the
/// coordinates.
fn input_edit(buffer: &Buffer, edit: &Edit, old_text: &str) -> InputEdit {
    let start = buffer.offset_to_point(edit.new.start);
    InputEdit {
        start_byte: edit.new.start as usize,
        old_end_byte: (edit.new.start + (edit.old.end - edit.old.start)) as usize,
        new_end_byte: edit.new.end as usize,
        start_position: ts_point(start),
        old_end_position: ts_point(advance(start, old_text)),
        new_end_position: ts_point(buffer.offset_to_point(edit.new.end)),
    }
}

/// The point `text` ends at when it starts at `from`.
fn advance(from: Point, text: &str) -> Point {
    let bytes = text.as_bytes();
    match memchr::memrchr(b'\n', bytes) {
        Some(last) => {
            let rows = memchr::memchr_iter(b'\n', bytes).count() as u32;
            Point::new(from.row + rows, (bytes.len() - last - 1) as u32)
        }
        None => Point::new(from.row, from.col + bytes.len() as u32),
    }
}

fn ts_point(p: Point) -> tree_sitter::Point {
    tree_sitter::Point { row: p.row as usize, column: p.col as usize }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlight::splice::line_splices;
    use crate::highlight::tree_sitter_def::{highlight_tree, highlight_whole};
    use crate::highlight::{Rgba, HIGHLIGHT_MAX_LINES_PER_CALL};
    use crate::transaction::{apply, EditOp};

    const KEYWORD: Rgba = Rgba { r: 0xff, g: 0, b: 0, a: 0xff };
    const STRING: Rgba = Rgba { r: 0, g: 0xff, b: 0, a: 0xff };
    const COMMENT: Rgba = Rgba { r: 0x80, g: 0x80, b: 0x80, a: 0xff };
    const FUNCTION: Rgba = Rgba { r: 0, g: 0, b: 0xff, a: 0xff };
    const TYPE: Rgba = Rgba { r: 0xff, g: 0xff, b: 0, a: 0xff };

    fn plain(fg: Rgba) -> SpanStyle {
        SpanStyle { fg, bold: false, italic: false }
    }

    fn theme() -> TokenTheme {
        TokenTheme::builder()
            .capture("keyword", plain(KEYWORD))
            .capture("string", plain(STRING))
            .capture("comment", plain(COMMENT))
            .capture("function", plain(FUNCTION))
            .capture("type", plain(TYPE))
            .capture("escape", plain(KEYWORD))
            .build()
    }

    fn def() -> TreeSitterDef {
        TreeSitterDef::new(tree_sitter_rust::LANGUAGE, tree_sitter_rust::HIGHLIGHTS_QUERY).unwrap()
    }

    /// A buffer and the cache over it, edited the way `Document` edits.
    struct Fixture {
        buffer: Buffer,
        cache: Cache,
        theme: TokenTheme,
    }

    impl Fixture {
        fn new(text: &str) -> Self {
            let buffer = Buffer::new(text).unwrap();
            let theme = theme();
            let cache = Cache::new(def(), &theme, buffer.line_count());
            Self { buffer, cache, theme }
        }

        fn edit(&mut self, ops: Vec<EditOp>) {
            let committed = apply(&mut self.buffer, ops).unwrap();
            if committed.patch().edits().is_empty() {
                return;
            }
            let spans = line_splices(&self.buffer, &committed);
            self.cache.on_commit(&self.buffer, &committed, &spans);
            assert_eq!(self.cache.line_count(), self.buffer.line_count());
            if let Some(tree) = &self.cache.tree {
                // The edited tree, before any reparse: every node the edits
                // didn't touch already sits at its post-commit points.
                self.assert_points_match_bytes(tree, |node| !node.has_changes());
            }
        }

        /// Tokenize until idle; returns the rows queried.
        fn drive(&mut self) -> u32 {
            let mut work = 0;
            for _ in 0..1_000 {
                if self.cache.pending().is_none() {
                    return work;
                }
                work += self.cache.tokenize(&self.buffer, HIGHLIGHT_MAX_LINES_PER_CALL);
            }
            panic!("the cache never went idle");
        }

        /// Checks the idle cache: every node of its tree sits at the points
        /// its bytes give (which a wrong tree edit breaks), and every
        /// retained row equals a whole-document highlight from that tree and,
        /// when the text parses without errors, from a fresh parse. Returns
        /// how many rows were compared.
        ///
        /// Error recovery after an incremental reparse may build a different
        /// tree than a fresh parse of the same text, so the fresh oracle is
        /// only binding on error-free text.
        fn assert_matches_oracle(&self) -> usize {
            assert_eq!(self.cache.pending(), None, "check an idle cache");
            let tree = self.cache.tree.as_ref().expect("an idle cache has parsed");
            self.assert_points_match_bytes(tree, |_| true);
            let text = self.buffer.text();
            let def = &self.cache.def;
            let ours = highlight_tree(def, &self.theme, tree, &text);
            let fresh = (!tree.root_node().has_error()).then(|| highlight_whole(def, &self.theme, &text));
            assert_eq!(ours.len() as u32, self.buffer.line_count());
            let mut checked = 0;
            for (row, expected) in ours.iter().enumerate() {
                if let Some(spans) = self.cache.line_spans(row as u32) {
                    assert_eq!(spans, expected.as_slice(), "row {row}");
                    if let Some(fresh) = &fresh {
                        assert_eq!(spans, fresh[row].as_slice(), "row {row} against a fresh parse");
                    }
                    checked += 1;
                }
            }
            checked
        }

        fn assert_points_match_bytes(&self, tree: &Tree, check: impl Fn(&tree_sitter::Node) -> bool) {
            let mut cursor = tree.walk();
            loop {
                let node = cursor.node();
                let ends = [(node.start_byte(), node.start_position()), (node.end_byte(), node.end_position())];
                for (byte, point) in ends.into_iter().filter(|_| check(&node)) {
                    let expected = ts_point(self.buffer.offset_to_point(byte as u32));
                    assert_eq!(point, expected, "{} at byte {byte}", node.kind());
                }
                if cursor.goto_first_child() || cursor.goto_next_sibling() {
                    continue;
                }
                loop {
                    if !cursor.goto_parent() {
                        return;
                    }
                    if cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
        }

        fn offset(&self, row: u32, col: u32) -> u32 {
            self.buffer.point_to_offset(Point::new(row, col))
        }
    }

    fn sample(rows: usize) -> String {
        (0..rows)
            .map(|i| match i % 7 {
                0 => format!("fn f{i}() {{"),
                1 => format!("    let s = \"text {i}\\n\";"),
                2 => "    // a note".to_string(),
                3 => format!("    let t: Vec<u8> = g{i}(s);"),
                4 => "}".to_string(),
                5 => format!("struct S{i};"),
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_fresh_document_matches_the_oracle() {
        let mut f = Fixture::new(&sample(300));
        f.drive();
        assert_eq!(f.assert_matches_oracle(), 300);
        assert_eq!(f.cache.parse_count(), 1);
    }

    #[test]
    fn an_in_place_edit_reconverges() {
        let mut f = Fixture::new(&sample(100));
        f.drive();
        // `fn f7() {` becomes `struct f7() {`, a syntax error the tree recovers from.
        let at = f.offset(7, 0);
        f.edit(vec![EditOp::new(at..at + 2, "struct")]);
        assert_eq!(f.cache.line_spans(7), None, "the edited row lost its stale spans");
        assert!(f.cache.pending().is_some());
        f.drive();
        assert_eq!(f.assert_matches_oracle(), 100);
    }

    #[test]
    fn opening_a_block_comment_comments_out_the_rows_below_and_closing_reverts() {
        let mut f = Fixture::new(&sample(40));
        f.drive();
        let before: Vec<_> = (0..40).map(|r| f.cache.line_spans(r).unwrap().to_vec()).collect();
        let at = f.offset(10, 0);
        f.edit(vec![EditOp::insert(at, "/*")]);
        f.drive();
        f.assert_matches_oracle();
        for row in 11..40 {
            let spans = f.cache.line_spans(row).unwrap();
            assert!(spans.iter().all(|s| s.style.fg == COMMENT), "row {row} is commented out: {spans:?}");
        }
        f.edit(vec![EditOp::delete(at..at + 2)]);
        f.drive();
        f.assert_matches_oracle();
        let after: Vec<_> = (0..40).map(|r| f.cache.line_spans(r).unwrap().to_vec()).collect();
        assert_eq!(after, before);
    }

    /// Fragments that open and close strings, comments and blocks, so
    /// random edits keep changing what the rows below mean.
    const FRAGMENTS: &[&str] = &[
        "\n", "/*", "*/", "\"", "{", "}", "fn ", "x", "let y = 1;\n", "// c", "struct T;\n", "'a'", "\\n",
        "r#\"", "\"#", "(", ")", "  ", "u8",
    ];

    fn rng(seed: u64) -> impl FnMut(usize) -> usize {
        let mut state = seed;
        move |bound: usize| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((state >> 33) as usize) % bound.max(1)
        }
    }

    fn random_op(f: &Fixture, rand: &mut impl FnMut(usize) -> usize, from: u32, to: u32) -> EditOp {
        let at = from + rand((to - from + 1) as usize) as u32;
        let del = (rand(12) as u32).min(to - at);
        let text: String = (0..rand(3)).map(|_| FRAGMENTS[rand(FRAGMENTS.len())]).collect();
        debug_assert!(at + del <= f.buffer.len());
        EditOp::new(at..at + del, text)
    }

    /// Seeded edits, window moves and budget-starved drives in any order:
    /// once idle, every retained row equals the whole-document oracle,
    /// wherever the window lands.
    #[test]
    fn randomized_edits_and_window_moves_match_the_oracle() {
        let mut f = Fixture::new(&sample(1_500));
        let mut rand = rng(0xDEAD_BEEF);
        for step in 0..150 {
            match rand(4) {
                0 | 1 => {
                    let op = random_op(&f, &mut rand, 0, f.buffer.len());
                    f.edit(vec![op]);
                }
                2 => {
                    let s = rand(f.buffer.line_count() as usize) as u32;
                    f.cache.set_window(s..(s + 40).min(f.buffer.line_count()));
                }
                _ => {
                    f.cache.tokenize(&f.buffer, 8 + rand(120) as u32);
                }
            }
            if step % 25 == 24 {
                f.drive();
                f.assert_matches_oracle();
            }
        }
        f.drive();
        assert!(f.assert_matches_oracle() >= 40);
        let n = f.buffer.line_count();
        for probe in [0, n / 3, 2 * n / 3, n.saturating_sub(45)] {
            f.cache.set_window(probe..(probe + 40).min(n));
            f.drive();
            f.assert_matches_oracle();
            for row in probe..(probe + 40).min(n) {
                assert!(f.cache.line_spans(row).is_some(), "probe {probe}: row {row} is retained");
            }
        }
    }

    /// The multi-caret analogue: each commit holds several disjoint edits,
    /// so later edits' tree coordinates depend on the earlier ones.
    #[test]
    fn randomized_multi_edit_commits_match_the_oracle() {
        let mut f = Fixture::new(&sample(1_200));
        let mut rand = rng(0x1234_5678_9ABC_DEF0);
        for step in 0..100 {
            if rand(4) == 3 {
                f.cache.tokenize(&f.buffer, 8 + rand(120) as u32);
                continue;
            }
            let len = f.buffer.len();
            let mut ops = Vec::new();
            let mut from = rand(200) as u32;
            for _ in 0..1 + rand(5) {
                if from >= len {
                    break;
                }
                let to = (from + 400).min(len);
                let op = random_op(&f, &mut rand, from, to);
                from = op.range.end + 1 + rand(400) as u32;
                ops.push(op);
            }
            // Shuffled: `apply` sorts the batch, and the cache must follow
            // the patch's order, not the caller's.
            if ops.len() > 1 && rand(2) == 0 {
                ops.reverse();
            }
            f.edit(ops);
            if step % 20 == 19 {
                f.drive();
                f.assert_matches_oracle();
            }
        }
        f.drive();
        assert!(f.assert_matches_oracle() >= 40);
        let n = f.buffer.line_count();
        for probe in [0, n / 2, n.saturating_sub(45)] {
            f.cache.set_window(probe..(probe + 40).min(n));
            f.drive();
            f.assert_matches_oracle();
        }
    }

    /// A multi-edit commit that keeps `sample` text valid Rust, built from
    /// rows picked at random: an item or statement inserted at a row start,
    /// or a whole statement, comment or struct row deleted.
    fn valid_ops(f: &Fixture, rand: &mut impl FnMut(usize) -> usize) -> Vec<EditOp> {
        let n = f.buffer.line_count();
        let mut rows: Vec<u32> = (0..1 + rand(5)).map(|_| rand(n as usize) as u32).collect();
        rows.sort_unstable();
        rows.dedup();
        let mut ops = Vec::new();
        for row in rows {
            let line = f.buffer.line(row);
            let start = f.offset(row, 0);
            let next = if row + 1 < n { f.offset(row + 1, 0) } else { f.buffer.len() };
            let top_level = line.is_empty() || line.starts_with("fn ") || line.starts_with("struct ");
            let statement = line.starts_with("    let") || line.starts_with("    //");
            let deletable = row + 1 < n && (statement || line.starts_with("struct "));
            if deletable && rand(3) == 0 {
                ops.push(EditOp::delete(start..next));
            } else if top_level {
                const ITEMS: &[&str] = &["struct Q;\n", "fn h() { let z = \"a\\tb\"; }\n", "/* c\n d */\n", "\n"];
                ops.push(EditOp::insert(start, ITEMS[rand(ITEMS.len())]));
            } else if statement {
                const STATEMENTS: &[&str] = &["    let q = 1;\n", "    /* x */\n", "    let r = \"\\\"\";\n"];
                ops.push(EditOp::insert(start, STATEMENTS[rand(STATEMENTS.len())]));
            }
        }
        ops
    }

    /// On text that always parses cleanly, the incrementally reparsed tree
    /// must highlight exactly like a fresh parse: the tree-edit coordinates
    /// are right for every edit of a multi-edit commit.
    #[test]
    fn randomized_valid_multi_edit_commits_match_a_fresh_parse() {
        let mut f = Fixture::new(&sample(700));
        f.drive();
        let mut rand = rng(0x5EED);
        for step in 0..120 {
            let ops = valid_ops(&f, &mut rand);
            f.edit(ops);
            if step % 3 == 0 {
                f.cache.set_window({
                    let s = rand(f.buffer.line_count() as usize) as u32;
                    s..(s + 40).min(f.buffer.line_count())
                });
            }
            f.drive();
            assert!(!f.cache.tree.as_ref().unwrap().root_node().has_error(), "step {step}: the text stays valid");
            f.assert_matches_oracle();
        }
    }

    #[test]
    fn several_commits_before_a_reparse_match_the_oracle() {
        let mut f = Fixture::new(&sample(200));
        f.drive();
        let mut rand = rng(42);
        for _ in 0..30 {
            for _ in 0..1 + rand(4) {
                let op = random_op(&f, &mut rand, 0, f.buffer.len());
                f.edit(vec![op]);
            }
            f.drive();
            f.assert_matches_oracle();
        }
    }

    #[test]
    fn a_theme_swap_recolors_without_a_reparse() {
        let mut f = Fixture::new("fn main() {}");
        f.drive();
        let old = f.cache.line_spans(0).unwrap().to_vec();
        assert_eq!(old[0].style.fg, KEYWORD);
        let green = TokenTheme::builder().capture("keyword", plain(STRING)).build();
        f.cache.set_theme(&green);
        f.theme = green;
        assert_eq!(f.cache.line_spans(0), Some(old.as_slice()), "the old colors show until requeried");
        assert!(f.cache.pending().is_some());
        f.drive();
        f.assert_matches_oracle();
        assert_eq!(f.cache.line_spans(0).unwrap()[0].style.fg, STRING);
        assert_eq!(f.cache.parse_count(), 1, "the tree was reused");
    }

    #[test]
    fn perf_canary_a_keystroke_requeries_only_rows_near_the_edit() {
        let mut f = Fixture::new(&sample(3_000));
        f.cache.set_window(100..140);
        f.drive();
        // Type into an identifier in view: the syntax around it is unchanged.
        let at = f.offset(119, 12);
        f.edit(vec![EditOp::insert(at, "z")]);
        let work = f.drive();
        assert!(work <= 3, "a keystroke requeried {work} rows");
        f.assert_matches_oracle();
        // A keystroke outside the window queries nothing.
        let at = f.offset(2_500, 12);
        f.edit(vec![EditOp::insert(at, "z")]);
        assert_eq!(f.drive(), 0);
    }

    #[test]
    fn a_window_beyond_an_edit_keeps_its_spans_shifted() {
        let mut f = Fixture::new(&sample(2_000));
        f.cache.set_window(1_000..1_040);
        f.drive();
        let row = f.cache.line_spans(1_020).unwrap().to_vec();
        // Two lines inserted far above the window.
        f.edit(vec![EditOp::insert(0, "struct A;\nstruct B;\n")]);
        assert_eq!(f.cache.line_spans(1_022), Some(row.as_slice()), "spans moved with their text");
        f.drive();
        f.assert_matches_oracle();
    }

    #[test]
    fn advance_counts_rows_and_the_last_rows_bytes() {
        assert_eq!(advance(Point::new(3, 4), ""), Point::new(3, 4));
        assert_eq!(advance(Point::new(3, 4), "abc"), Point::new(3, 7));
        assert_eq!(advance(Point::new(3, 4), "a\nbc\nxyz"), Point::new(5, 3));
        assert_eq!(advance(Point::new(3, 4), "a\n"), Point::new(4, 0));
    }
}

