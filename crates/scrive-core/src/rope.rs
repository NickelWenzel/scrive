//! The text rope: a [`ropey::Rope`] behind scrive's byte-offset, LF-only API.
//! Backs [`crate::buffer::Buffer`]. Reads are O(log n) and the clone is O(1)
//! (an `Arc` bump), the structural-sharing snapshot the highlight sweep rides.
//!
//! Byte offsets are `str`-style: every public offset is (or is clamped to) a char
//! boundary, and chunk boundaries are char boundaries.
//!
//! **Line model.** Rows are delimited by `\n` alone. ropey's line metric agrees
//! only while its `cr_lines`/`unicode_lines` features are off; cargo unifies
//! features across a build, so any crate that enables ropey's defaults would turn
//! U+2028, VT, FF, NEL and CR into line breaks for scrive too. [`lf_only`] checks
//! this once and every constructor asserts it, so such a build fails loudly at the
//! first rope instead of desyncing rows from `\n`.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::OnceLock;

use crate::coords::Point;

/// [`Rope::edit_many`] rebuilds the rope in one pass instead of splicing each
/// edit once the batch has more than one edit per this many document bytes.
/// Measured (release, 1–10 MiB of code): a splice costs ~0.4–0.9 µs, a rebuild
/// ~0.4 ms per MiB, so they break even near 500 edits per MiB.
const REBUILD_BYTES_PER_EDIT: u64 = 2048;

/// A rope of UTF-8 text. LF-only line model; `Clone` is O(1).
///
/// `len` and `newlines` cache ropey's root totals, which it re-sums from the
/// root's children on every call.
#[derive(Clone, Debug)]
pub struct Rope {
    text: ropey::Rope,
    len: u32,
    newlines: u32,
}

impl Default for Rope {
    fn default() -> Self {
        Rope::from_ropey(ropey::Rope::new())
    }
}

/// Whether ropey breaks lines on `\n` only in this build (see the module docs).
fn lf_only() -> bool {
    static LF_ONLY: OnceLock<bool> = OnceLock::new();
    *LF_ONLY.get_or_init(|| ropey::Rope::from_str("\r\u{b}\u{c}\u{85}\u{2028}\u{2029}").len_lines() == 1)
}

impl Rope {
    fn from_ropey(text: ropey::Rope) -> Rope {
        assert!(
            lf_only(),
            "ropey was built with `cr_lines`/`unicode_lines` (its default features): some crate \
             in this build enables them, which breaks scrive's LF-only line model"
        );
        let mut rope = Rope { text, len: 0, newlines: 0 };
        rope.refresh_totals();
        rope
    }

    fn refresh_totals(&mut self) {
        self.len = self.text.len_bytes() as u32;
        self.newlines = (self.text.len_lines() - 1) as u32;
    }

    /// Build from `s` (already LF-only).
    #[must_use]
    pub fn from_str(s: &str) -> Rope {
        Rope::from_ropey(ropey::Rope::from_str(s))
    }

    /// Total byte length.
    #[must_use]
    pub fn len(&self) -> u32 {
        self.len
    }

    /// Number of lines = `\n` count + 1 (always ≥ 1).
    #[must_use]
    pub fn line_count(&self) -> u32 {
        self.newlines + 1
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The text in byte range `range` (clamped to the end). `Borrowed` when it sits
    /// in one chunk, `Owned` across chunks.
    #[must_use]
    pub fn slice(&self, range: Range<u32>) -> Cow<'_, str> {
        let end = range.end.min(self.len);
        let start = range.start.min(end);
        if start == end {
            return Cow::Borrowed("");
        }
        let (chunk, cs) = self.chunk_at(start);
        if end <= cs + chunk.len() as u32 {
            return Cow::Borrowed(&chunk[(start - cs) as usize..(end - cs) as usize]);
        }
        let mut out = String::with_capacity((end - start) as usize);
        out.push_str(&chunk[(start - cs) as usize..]);
        let mut pos = cs + chunk.len() as u32;
        let (chunks, ..) = self.text.chunks_at_byte(pos as usize);
        for chunk in chunks {
            let take = ((end - pos) as usize).min(chunk.len());
            out.push_str(&chunk[..take]);
            pos += take as u32;
            if pos == end {
                break;
            }
        }
        Cow::Owned(out)
    }

    /// The chunk holding the start of `row` (`row < line_count`) and the row's
    /// byte offset — one descent by line break.
    fn row_start(&self, row: u32) -> (&str, u32, u32) {
        let (chunk, cs, _, cl) = self.text.chunk_at_line_break(row as usize);
        let cs = cs as u32;
        if row == 0 {
            return (chunk, cs, 0);
        }
        // The chunk holds the row-th `\n`; `cl` of them precede it.
        let nth = row as usize - cl - 1;
        let nl = memchr::memchr_iter(b'\n', chunk.as_bytes()).nth(nth).expect("the chunk holds the row's `\\n`");
        (chunk, cs, cs + nl as u32 + 1)
    }

    /// Byte offset of the first `\n` in `from..`, capped at `from + limit` and at
    /// the doc end, starting the search in `chunk` (which begins at `cs` and holds
    /// `from`, or ends exactly there).
    fn line_end_from<'a>(&'a self, mut chunk: &'a str, mut cs: u32, from: u32, limit: u32) -> u32 {
        let cap = from.saturating_add(limit).min(self.len);
        let mut pos = from;
        loop {
            let chunk_end = cs + chunk.len() as u32;
            let stop = cap.min(chunk_end);
            if pos < stop {
                let hay = &chunk.as_bytes()[(pos - cs) as usize..(stop - cs) as usize];
                if let Some(i) = memchr::memchr(b'\n', hay) {
                    return pos + i as u32;
                }
                pos = stop;
            }
            if pos >= cap {
                return cap;
            }
            (chunk, cs) = self.chunk_at(chunk_end);
        }
    }

    /// One row's text, excluding its trailing `\n`. Out-of-range → `""`.
    #[must_use]
    pub fn line(&self, row: u32) -> Cow<'_, str> {
        if row >= self.line_count() {
            return Cow::Borrowed("");
        }
        let (chunk, cs, start) = self.row_start(row);
        let end = self.line_end_from(chunk, cs, start, u32::MAX);
        if end <= cs + chunk.len() as u32 {
            Cow::Borrowed(&chunk[(start - cs) as usize..(end - cs) as usize])
        } else {
            self.slice(start..end)
        }
    }

    /// Byte length of one row's text (excludes `\n`). Out-of-range → 0.
    #[must_use]
    pub fn line_len(&self, row: u32) -> u32 {
        if row >= self.line_count() {
            return 0;
        }
        let (chunk, cs, start) = self.row_start(row);
        self.line_end_from(chunk, cs, start, u32::MAX) - start
    }

    /// Byte offset → `Point`, clamping past-the-end to the doc end. Byte-based (a
    /// col is a byte count), so a mid-char offset still maps sanely.
    #[must_use]
    pub fn byte_to_point(&self, offset: u32) -> Point {
        let offset = offset.min(self.len);
        let (chunk, cs, _, cl) = self.text.chunk_at_byte(offset as usize);
        let within = &chunk.as_bytes()[..offset as usize - cs];
        match memchr::memrchr(b'\n', within) {
            Some(last) => {
                let row = cl + memchr::memchr_iter(b'\n', within).count();
                Point::new(row as u32, (within.len() - last - 1) as u32)
            }
            // The row starts in an earlier chunk.
            None => Point::new(cl as u32, offset - self.text.line_to_byte(cl) as u32),
        }
    }

    /// `Point` → byte offset, clamping the row to the last line and the col to that
    /// row's byte length.
    #[must_use]
    pub fn point_to_offset(&self, point: Point) -> u32 {
        let row = point.row.min(self.newlines);
        let (chunk, cs, start) = self.row_start(row);
        self.line_end_from(chunk, cs, start, point.col)
    }

    /// The chunk containing byte `offset` (the right-hand chunk at an exact
    /// boundary, the last chunk at the end), and that chunk's start offset.
    /// Empty rope → `("", 0)`.
    #[must_use]
    pub fn chunk_at(&self, offset: u32) -> (&str, u32) {
        let (chunk, cs, _, _) = self.text.chunk_at_byte(offset.min(self.len) as usize);
        (chunk, cs as u32)
    }

    /// Whether `offset` sits on a char boundary (`0` and `len` do).
    #[must_use]
    pub fn is_char_boundary(&self, offset: u32) -> bool {
        if offset == 0 || offset >= self.len {
            return offset <= self.len;
        }
        let (chunk, cs) = self.chunk_at(offset);
        chunk.is_char_boundary((offset - cs) as usize)
    }

    /// Visit each chunk's text in order — the cold-path whole-document walk
    /// (serialize).
    pub fn for_each_chunk(&self, f: impl FnMut(&str)) {
        self.text.chunks().for_each(f);
    }

    /// The chunks in order. Borrows the rope; no copy.
    pub fn chunks(&self) -> impl Iterator<Item = &str> {
        self.text.chunks()
    }

    /// Replace byte `range` with `text` (already LF-only), O(log n + |text|).
    pub fn replace(&mut self, range: Range<u32>, text: &str) {
        let start = range.start.min(self.len);
        let end = range.end.min(self.len).max(start);
        let char_start = self.text.byte_to_char(start as usize);
        if end > start {
            let char_end = self.text.byte_to_char(end as usize);
            self.text.remove(char_start..char_end);
        }
        if !text.is_empty() {
            self.text.insert(char_start, text);
        }
        self.refresh_totals();
    }

    /// Apply MANY disjoint edits (`(byte range, replacement)`, sorted ascending by
    /// start, non-overlapping, in-bounds, all LF-only) — the batched twin of N
    /// [`Self::replace`]s. A batch dense enough that per-edit splices would cost
    /// more than copying the document rebuilds the rope in one pass instead.
    pub fn edit_many(&mut self, edits: &[(Range<u32>, &str)]) {
        if edits.len() as u64 * REBUILD_BYTES_PER_EDIT > u64::from(self.len) {
            self.rebuild_with(edits);
        } else {
            // Descending, so earlier offsets stay valid.
            for (range, text) in edits.iter().rev() {
                self.replace(range.clone(), text);
            }
        }
    }

    fn rebuild_with(&mut self, edits: &[(Range<u32>, &str)]) {
        let mut out = ropey::RopeBuilder::new();
        let mut chunks = self.text.chunks();
        let (mut chunk, mut cs, mut pos) = ("", 0u32, 0u32);
        let gaps = edits.iter().map(|(r, t)| (r.start, r.end, *t)).chain([(self.len, self.len, "")]);
        for (start, end, text) in gaps {
            // Copy the untouched text `pos..start`, then skip the replaced `start..end`.
            while pos < end {
                if pos == cs + chunk.len() as u32 {
                    cs += chunk.len() as u32;
                    chunk = chunks.next().expect("edits are in bounds");
                }
                let stop = end.min(cs + chunk.len() as u32);
                if pos < start {
                    let copy_end = stop.min(start);
                    out.append(&chunk[(pos - cs) as usize..(copy_end - cs) as usize]);
                }
                pos = stop;
            }
            out.append(text);
        }
        *self = Rope::from_ropey(out.finish());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plain `String` with the documented semantics, the oracle every rope read
    /// is checked against.
    struct Model(String);

    impl Model {
        fn line_starts(&self) -> Vec<usize> {
            std::iter::once(0).chain(memchr::memchr_iter(b'\n', self.0.as_bytes()).map(|i| i + 1)).collect()
        }

        fn point(&self, off: usize) -> Point {
            point_in(&self.line_starts(), off)
        }
    }

    fn point_in(line_starts: &[usize], off: usize) -> Point {
        let row = line_starts.partition_point(|&s| s <= off) - 1;
        Point::new(row as u32, (off - line_starts[row]) as u32)
    }

    fn check_all(rope: &Rope, model: &Model, ctx: &str) {
        let text = &model.0;
        assert_eq!(rope.len() as usize, text.len(), "{ctx}: len");
        assert_eq!(rope.line_count() as usize, model.line_starts().len(), "{ctx}: line_count");
        assert_eq!(rope.slice(0..rope.len()), text.as_str(), "{ctx}: text");
        let lines: Vec<&str> = text.split('\n').collect();
        for row in 0..rope.line_count() + 2 {
            let want = lines.get(row as usize).copied().unwrap_or("");
            assert_eq!(rope.line(row), want, "{ctx}: line {row}");
            assert_eq!(rope.line_len(row) as usize, want.len(), "{ctx}: line_len {row}");
        }
        let starts = model.line_starts();
        for off in (0..=text.len()).filter(|&o| text.is_char_boundary(o)) {
            let p = rope.byte_to_point(off as u32);
            assert_eq!(p, point_in(&starts, off), "{ctx}: byte_to_point {off}");
            assert_eq!(rope.point_to_offset(p) as usize, off, "{ctx}: round-trip {off}");
        }
    }

    /// Text spanning many ropey chunks, with long and short lines and multibyte
    /// chars.
    fn multi_chunk_text() -> String {
        (0..300).map(|i| format!("{}λ{i}\n", "x".repeat(i % 97))).collect()
    }

    #[test]
    fn reads_match_a_string_model() {
        for text in ["", "\n", "a", "a\n", "fn main() {\n    let x = 42;\n}\n\nfin", &multi_chunk_text()] {
            let rope = Rope::from_str(text);
            check_all(&rope, &Model(text.to_string()), &format!("{:?}", &text[..text.len().min(20)]));
        }
    }

    #[test]
    fn reads_clamp() {
        let rope = Rope::from_str(&multi_chunk_text());
        let model = Model(multi_chunk_text());
        let last = rope.line_count() - 1;
        assert_eq!(rope.byte_to_point(u32::MAX), model.point(model.0.len()));
        assert_eq!(rope.point_to_offset(Point::new(u32::MAX, 0)), rope.len() - rope.line_len(last));
        assert_eq!(rope.point_to_offset(Point::new(3, u32::MAX)), rope.point_to_offset(Point::new(4, 0)) - 1);
        assert_eq!(rope.slice(rope.len() - 2..u32::MAX), "9\n");
    }

    #[test]
    fn slice_matches_the_model_across_chunks() {
        let text = multi_chunk_text();
        let rope = Rope::from_str(&text);
        assert!(rope.chunks().count() > 4, "the text must span several chunks");
        let mut state = 0x9E37_79B9u32;
        for _ in 0..500 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let a = rand_boundary(&text, state);
            let b = a + rand_boundary(&text[a..], state.rotate_left(16));
            assert_eq!(rope.slice(a as u32..b as u32), &text[a..b]);
        }
    }

    /// The line model is `\n` alone, whatever features ropey was built with.
    #[test]
    fn only_lf_breaks_lines() {
        assert!(lf_only(), "ropey's `cr_lines`/`unicode_lines` are on in this build");
        let rope = Rope::from_str("a\u{2028}b\u{b}c\u{c}d\u{85}e\u{2029}f\rg");
        assert_eq!(rope.line_count(), 1);
        assert_eq!(rope.byte_to_point(rope.len()).row, 0);
        assert_eq!(rope.line(0).len() as u32, rope.len());
    }

    // Byte offset of a random char boundary in `s`, biased across the doc.
    fn rand_boundary(s: &str, r: u32) -> usize {
        let mut off = (r as usize) % (s.len() + 1);
        while !s.is_char_boundary(off) {
            off -= 1;
        }
        off
    }

    #[test]
    fn matches_the_model_under_random_edits() {
        let mut model = Model(multi_chunk_text());
        let mut rope = Rope::from_str(&model.0);
        let inserts = ["", "x", "hello", "\n", "a\nb\n", "  ", "λ", "→ok", &"long line\n".repeat(150)];
        let mut state = 0x1234_5678u32;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        for step in 0..400 {
            let text = &model.0;
            let a = rand_boundary(text, next());
            let span = if next() % 8 == 0 { text.len() } else { 64 };
            let b = a + rand_boundary(&text[a..(a + span).min(text.len())], next());
            let ins = inserts[(next() as usize) % inserts.len()];
            rope.replace(a as u32..b as u32, ins);
            model.0.replace_range(a..b, ins);
            if step % 20 == 0 {
                check_all(&rope, &model, &format!("step {step}"));
            } else {
                assert_eq!(rope.slice(0..rope.len()), model.0.as_str(), "step {step}: text");
                assert_eq!(rope.line_count() as usize, model.line_starts().len(), "step {step}: lines");
            }
        }
    }

    #[test]
    fn edit_many_matches_the_model_on_both_paths() {
        // 26 KB: 8 edits splice, 2000 rebuild.
        let base = "the quick brown fox\njumps over the lazy dog\n".repeat(600);
        let inserts: &[&str] = &["", "x", "hello", "\n", "a\nb\n", "  ", "λ", "→ok"];
        let mut state = 0x00C0_FFEEu32;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        for trial in 0..300 {
            let len = base.len();
            let k = 1 + next() as usize % if trial % 2 == 0 { 8 } else { 2000 };
            let mut edits: Vec<(Range<u32>, &str)> = Vec::new();
            let mut cursor = 0usize;
            for _ in 0..k {
                if cursor >= len {
                    break;
                }
                let s = (cursor + next() as usize % (2 * (len - cursor) / k + 1)).min(len);
                let cap = if next() % 4 == 0 { 2000 } else { 24 };
                let e = s + next() as usize % (cap.min(len - s) + 1);
                edits.push((s as u32..e as u32, inserts[next() as usize % inserts.len()]));
                cursor = e + 1;
            }
            let mut rope = Rope::from_str(&base);
            rope.edit_many(&edits);
            let mut model = Model(base.clone());
            for (r, t) in edits.iter().rev() {
                model.0.replace_range(r.start as usize..r.end as usize, t);
            }
            assert_eq!(rope.slice(0..rope.len()), model.0.as_str(), "trial {trial}");
            assert_eq!(rope.len() as usize, model.0.len(), "trial {trial}: len");
            assert_eq!(rope.line_count() as usize, model.line_starts().len(), "trial {trial}: lines");
            if trial % 25 == 0 {
                check_all(&rope, &model, &format!("trial {trial}"));
            }
        }
    }

    #[test]
    fn chunk_at_is_right_biased_and_ends_on_the_last_chunk() {
        let rope = Rope::from_str(&multi_chunk_text());
        assert_eq!(Rope::default().chunk_at(0), ("", 0));
        let mut cs = 0;
        for chunk in rope.chunks() {
            assert_eq!(rope.chunk_at(cs), (chunk, cs));
            cs += chunk.len() as u32;
        }
        let (last, last_start) = rope.chunk_at(rope.len());
        assert_eq!(last_start + last.len() as u32, rope.len());
    }

    #[test]
    fn char_boundaries() {
        let rope = Rope::from_str("aλb\nc→d");
        let s = "aλb\nc→d";
        for off in 0..=s.len() + 1 {
            assert_eq!(rope.is_char_boundary(off as u32), s.is_char_boundary(off), "@{off}");
        }
    }

    #[test]
    fn serialize_walk_is_verbatim() {
        let text = multi_chunk_text();
        let rope = Rope::from_str(&text);
        let mut out = String::new();
        rope.for_each_chunk(|c| out.push_str(c));
        assert_eq!(out, text);
    }
}

