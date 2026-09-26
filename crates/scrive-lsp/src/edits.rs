//! Edit hygiene: a server's `TextEdit`s as one batch an `edit_grouped` transaction accepts.
//!
//! Servers send CRLF inside `newText`, replace a whole document to change three characters, and
//! list edits in any order. Applied verbatim, that moves every caret and squiggle in the file and
//! can split a UTF-8 sequence, so each batch is converted (clamping), normalized to LF, line-diffed
//! when it replaces the whole document, trimmed to what changes on char boundaries, stripped of
//! no-ops and sorted.

use std::ops::Range;

use scrive_core::{EditOp, Point, Snapshot};

use crate::Encoding;

/// The longest line-level edit script the diff computes before it falls back to one trimmed edit.
/// The trace takes about `MAX_D² / 2` words (4 MB at 1000), and the time is
/// `O((N + M) · MAX_D)`.
const MAX_D: usize = 1000;

/// `edits` as one hygienic batch against `snapshot`.
pub(crate) fn hygiene(
    snapshot: &Snapshot,
    encoding: Encoding,
    edits: &[lsp_types::TextEdit],
) -> Vec<EditOp> {
    let mut ops: Vec<EditOp> = edits
        .iter()
        .map(|edit| EditOp::new(encoding.span(snapshot, edit.range), lf(&edit.new_text)))
        .collect();
    let last_line_start = snapshot.point_to_offset(Point::new(snapshot.line_count() - 1, 0));
    // Formatters answer with one edit over `0:0 → N:0` or `0:0 → last:len`.
    if let [op] = ops.as_slice() {
        if op.range.start == 0 && op.range.end >= last_line_start {
            ops = line_diff(&snapshot.slice(op.range.clone()), &op.text);
        }
    }
    let mut ops: Vec<EditOp> = ops
        .into_iter()
        .map(|op| trim(&snapshot.slice(op.range.clone()), op))
        .filter(|op| !(op.range.is_empty() && op.text.is_empty()))
        .collect();
    // Stable, so tied inserts keep the server's order. After the trim, which moves starts.
    ops.sort_by_key(|op| (op.range.start, op.range.end));
    ops
}

/// `text` with `\r\n` and lone `\r` as `\n`, the buffer's only line break.
fn lf(text: &str) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text.to_owned()
    }
}

/// `op` narrowed to the bytes that change. `old` is the text `op` replaces. The common prefix and
/// suffix back off to char boundaries in both texts, since `é` and `è` share their lead byte.
fn trim(old: &str, op: EditOp) -> EditOp {
    let new = op.text.as_str();
    let mut prefix = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(prefix) || !new.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let (old, new) = (&old[prefix..], &new[prefix..]);
    let mut suffix = old
        .bytes()
        .rev()
        .zip(new.bytes().rev())
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix) {
        suffix -= 1;
    }
    let start = op.range.start + prefix as u32;
    EditOp::new(
        start..op.range.end - suffix as u32,
        &new[..new.len() - suffix],
    )
}

/// `old` → `new` as line hunks, with offsets into `old`. Common leading and trailing lines are cut
/// first, so the Myers pass only sees the changed middle. Past [`MAX_D`] the middle is one edit.
fn line_diff(old: &str, new: &str) -> Vec<EditOp> {
    let a: Vec<&str> = old.split_inclusive('\n').collect();
    let b: Vec<&str> = new.split_inclusive('\n').collect();
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (a_mid, b_mid) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    let mut starts = Vec::with_capacity(a.len() + 1);
    let mut at = 0;
    for line in &a {
        starts.push(at);
        at += line.len() as u32;
    }
    starts.push(at);
    let old_at = |line: usize| starts[prefix + line];
    match script(a_mid, b_mid) {
        Some(hunks) => hunks
            .into_iter()
            .map(|hunk| {
                EditOp::new(
                    old_at(hunk.old.start)..old_at(hunk.old.end),
                    b_mid[hunk.new].concat(),
                )
            })
            .collect(),
        None => vec![EditOp::new(old_at(0)..old_at(a_mid.len()), b_mid.concat())],
    }
}

/// One replaced run: old lines `old` become new lines `new`, as half-open line indices.
struct Hunk {
    old: Range<usize>,
    new: Range<usize>,
}

/// One edit in a Myers path.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Consumes an old line.
    Delete,
    /// Consumes a new line.
    Insert,
}

/// Myers' greedy shortest edit script from `a` to `b`, as hunks, or `None` past [`MAX_D`].
/// `rows[d]` holds the furthest `x` on diagonals `k = -d, -d+2, …, d` after `d` edits: only
/// diagonals with `d`'s parity are reachable, which halves the trace.
fn script(a: &[&str], b: &[&str]) -> Option<Vec<Hunk>> {
    let (n, m) = (a.len(), b.len());
    let mut rows: Vec<Vec<usize>> = Vec::new();
    for d in 0..=(n + m).min(MAX_D) {
        let mut row = vec![0; d + 1];
        for (i, slot) in row.iter_mut().enumerate() {
            let k = 2 * i as isize - d as isize;
            let mut x = match rows.last() {
                None => 0,
                Some(prev) => match step(prev, d, k) {
                    Step::Insert => at(prev, d - 1, k + 1),
                    Step::Delete => at(prev, d - 1, k - 1) + 1,
                },
            };
            let mut y = (x as isize - k) as usize;
            while x < n && y < m && a[x] == b[y] {
                x += 1;
                y += 1;
            }
            *slot = x;
            if x >= n && y >= m {
                rows.push(row);
                return Some(hunks(&rows, n, m));
            }
        }
        rows.push(row);
    }
    None
}

/// The furthest `x` on diagonal `k` in `row`, the row for `d` edits.
fn at(row: &[usize], d: usize, k: isize) -> usize {
    row[((k + d as isize) / 2) as usize]
}

/// How diagonal `k` is reached at `d` edits from `prev`, the row for `d - 1`: an insertion steps
/// down from `k + 1`, a deletion right from `k - 1`.
fn step(prev: &[usize], d: usize, k: isize) -> Step {
    let edits = d as isize;
    if k == -edits || (k != edits && at(prev, d - 1, k - 1) < at(prev, d - 1, k + 1)) {
        Step::Insert
    } else {
        Step::Delete
    }
}

/// The path through `rows` back from `(n, m)`, with adjacent steps merged into hunks.
fn hunks(rows: &[Vec<usize>], n: usize, m: usize) -> Vec<Hunk> {
    let (mut x, mut y) = (n, m);
    let mut steps = Vec::with_capacity(rows.len());
    for d in (1..rows.len()).rev() {
        let k = x as isize - y as isize;
        let prev = &rows[d - 1];
        let step = step(prev, d, k);
        let from_k = match step {
            Step::Insert => k + 1,
            Step::Delete => k - 1,
        };
        let from_x = at(prev, d - 1, from_k);
        let from_y = (from_x as isize - from_k) as usize;
        steps.push((from_x, from_y, step));
        x = from_x;
        y = from_y;
    }
    let mut hunks: Vec<Hunk> = Vec::new();
    for (x, y, step) in steps.into_iter().rev() {
        let (dx, dy) = match step {
            Step::Insert => (0, 1),
            Step::Delete => (1, 0),
        };
        match hunks.last_mut() {
            Some(hunk) if hunk.old.end == x && hunk.new.end == y => {
                hunk.old.end += dx;
                hunk.new.end += dy;
            }
            _ => hunks.push(Hunk {
                old: x..x + dx,
                new: y..y + dy,
            }),
        }
    }
    hunks
}

#[cfg(test)]
mod tests {
    use lsp_types::{Position, TextEdit};
    use scrive_core::Document;

    use super::*;

    fn snapshot(text: &str) -> Snapshot {
        Document::new(text).expect("fixture loads").snapshot()
    }

    fn edit(start: (u32, u32), end: (u32, u32), text: &str) -> TextEdit {
        TextEdit::new(
            lsp_types::Range::new(Position::new(start.0, start.1), Position::new(end.0, end.1)),
            text.to_owned(),
        )
    }

    /// `text` with ascending, disjoint `ops` applied.
    fn apply(text: &str, ops: &[EditOp]) -> String {
        let mut out = text.to_owned();
        for op in ops.iter().rev() {
            out.replace_range(op.range.start as usize..op.range.end as usize, &op.text);
        }
        out
    }

    /// A trim that would split a shared lead byte backs off to the character.
    #[test]
    fn format_edit_replacing_e_acute_with_e_grave_trims_to_one_char() {
        assert_eq!(
            hygiene(
                &snapshot("café\n"),
                Encoding::Utf16,
                &[edit((0, 0), (0, 4), "cafè")]
            ),
            vec![EditOp::new(3..5, "è")],
            "only the accented character is replaced, whole",
        );
        assert_eq!(
            hygiene(
                &snapshot("café\n"),
                Encoding::Utf16,
                &[edit((0, 3), (0, 4), "è")]
            ),
            vec![EditOp::new(3..5, "è")],
            "utf-16 column 4 is byte 5",
        );
    }

    /// A batch in descending order comes out ascending by `(start, end)`.
    #[test]
    fn descending_edit_batch_is_sorted_by_start_then_end() {
        assert_eq!(
            hygiene(
                &snapshot("a\nb\nc\n"),
                Encoding::Utf8,
                &[edit((2, 0), (2, 1), "C"), edit((0, 0), (0, 1), "A")],
            ),
            vec![EditOp::new(0..1, "A"), EditOp::new(4..5, "C")],
            "sorted ascending",
        );
        assert_eq!(
            hygiene(
                &snapshot("ab"),
                Encoding::Utf8,
                &[edit((0, 1), (0, 2), "Z"), edit((0, 1), (0, 1), "X")],
            ),
            vec![EditOp::new(1..1, "X"), EditOp::new(1..2, "Z")],
            "an insert sorts before a replace at the same start",
        );
    }

    /// Two inserts at one offset keep the order the server listed them in.
    #[test]
    fn tied_inserts_keep_the_servers_order() {
        assert_eq!(
            hygiene(
                &snapshot("abc\n"),
                Encoding::Utf8,
                &[edit((0, 1), (0, 1), "X"), edit((0, 1), (0, 1), "Y")],
            ),
            vec![EditOp::new(1..1, "X"), EditOp::new(1..1, "Y")],
            "the sort is stable",
        );
    }

    /// The buffer is LF-only, so every line break in new text arrives as `\n`.
    #[test]
    fn crlf_and_lone_cr_in_new_text_become_lf() {
        assert_eq!(
            hygiene(
                &snapshot("x\n"),
                Encoding::Utf8,
                &[edit((0, 1), (0, 1), "\r\ny\r\n")]
            ),
            vec![EditOp::new(1..1, "\ny\n")],
            "CRLF becomes LF",
        );
        assert_eq!(
            hygiene(
                &snapshot("ab"),
                Encoding::Utf8,
                &[edit((0, 0), (0, 2), "a\rb")]
            ),
            vec![EditOp::new(1..1, "\n")],
            "a lone CR becomes LF, and the rest trims away",
        );
    }

    /// A formatter's whole-document replacement lands as the lines that changed, each trimmed.
    #[test]
    fn whole_document_edit_becomes_per_line_hunks() {
        let doc = snapshot("a  \nb\nc  \n");
        let expected = vec![EditOp::new(1..3, ""), EditOp::new(7..9, "")];
        assert_eq!(
            hygiene(&doc, Encoding::Utf8, &[edit((0, 0), (3, 0), "a\nb\nc\n")]),
            expected,
            "only the trailing blanks go",
        );
        assert_eq!(
            hygiene(&doc, Encoding::Utf8, &[edit((0, 0), (99, 0), "a\nb\nc\n")]),
            expected,
            "an end past the last line clamps to the document end",
        );
        assert_eq!(
            hygiene(
                &snapshot("a\nb\n"),
                Encoding::Utf8,
                &[edit((0, 0), (0, 1), "A")]
            ),
            vec![EditOp::new(0..1, "A")],
            "an edit ending before the last line is not whole-document",
        );
    }

    /// A diff longer than the cap is not searched; the changed middle becomes one edit.
    #[test]
    fn line_diff_past_the_myers_cap_falls_back_to_the_trimmed_edit() {
        let old: String = (0..1100).map(|i| format!("o{i}\n")).collect();
        let new: String = (0..1100).map(|i| format!("n{i}\n")).collect();
        let ops = hygiene(
            &snapshot(&old),
            Encoding::Utf8,
            &[edit((0, 0), (1100, 0), &new)],
        );
        assert_eq!(ops.len(), 1, "one edit past the cap");
        assert_eq!(apply(&old, &ops), new, "the edit still yields the new text");
        let partly: String = (0..1100u32)
            .map(|i| {
                if i.is_multiple_of(3) {
                    format!("n{i}\n")
                } else {
                    format!("o{i}\n")
                }
            })
            .collect();
        let ops = hygiene(
            &snapshot(&old),
            Encoding::Utf8,
            &[edit((0, 0), (1100, 0), &partly)],
        );
        assert_eq!(
            ops.len(),
            367,
            "within the cap, every changed line is its own hunk"
        );
        assert_eq!(apply(&old, &ops), partly, "the hunks yield the new text");
    }

    /// Line hunks, trimmed, rebuild the new text from the old and never overlap.
    #[test]
    fn line_diff_hunks_rebuild_the_new_text() {
        /// Up to 8 lines over `a b c é`, with or without a final newline.
        fn text(next: &mut impl FnMut() -> u64) -> String {
            const WORDS: [&str; 4] = ["a", "b", "c", "é"];
            let lines = (next() % 9) as usize;
            let mut text = (0..lines)
                .map(|_| WORDS[(next() % 4) as usize])
                .collect::<Vec<_>>()
                .join("\n");
            if next().is_multiple_of(2) {
                text.push('\n');
            }
            text
        }
        let mut state: u64 = 7;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..2000 {
            let old = text(&mut next);
            let new = text(&mut next);
            let mut ops: Vec<EditOp> = line_diff(&old, &new)
                .into_iter()
                .map(|op| trim(&old[op.range.start as usize..op.range.end as usize], op))
                .collect();
            ops.sort_by_key(|op| (op.range.start, op.range.end));
            assert!(
                ops.windows(2).all(|w| w[0].range.end <= w[1].range.start),
                "ops for {old:?} → {new:?} are disjoint: {ops:?}",
            );
            assert_eq!(apply(&old, &ops), new, "ops rebuild {new:?} from {old:?}");
        }
    }

    /// An edit that replaces text with itself is no edit at all.
    #[test]
    fn edit_that_changes_nothing_is_dropped() {
        assert_eq!(
            hygiene(
                &snapshot("abc"),
                Encoding::Utf8,
                &[edit((0, 0), (0, 3), "abc")]
            ),
            vec![],
            "a no-op is dropped",
        );
    }
}
