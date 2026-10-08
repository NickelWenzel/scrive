//! The negotiated position encoding and the conversions between LSP positions and scrive byte
//! offsets.
//!
//! LSP counts a position's `character` in the negotiated unit (utf-8 bytes, utf-16 code units or
//! utf-32 scalar values); scrive counts bytes. Conversions walk one line's text as `&str` chunks,
//! either a rope snapshot's own chunks or a slice of plain text, so nothing is materialized.

use core::ops::Range;

use lsp_types::{Position, PositionEncodingKind};
use scrive_core::{Bias, Point, Snapshot};

/// How LSP positions count characters on a line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Encoding {
    /// utf-8 code units — bytes, scrive's own unit.
    Utf8,
    /// utf-16 code units. The LSP default when nothing was negotiated.
    #[default]
    Utf16,
    /// utf-32 code units — Unicode scalar values.
    Utf32,
}

impl Encoding {
    /// The encoding a server chose in `capabilities.positionEncoding`. Absent or unknown means
    /// utf-16, the protocol default.
    #[must_use]
    pub fn negotiate(chosen: Option<&PositionEncodingKind>) -> Self {
        match chosen {
            Some(kind) if *kind == PositionEncodingKind::UTF8 => Encoding::Utf8,
            Some(kind) if *kind == PositionEncodingKind::UTF32 => Encoding::Utf32,
            _ => Encoding::Utf16,
        }
    }

    /// The protocol name of this encoding, for `general.positionEncodings`.
    #[must_use]
    pub fn kind(self) -> PositionEncodingKind {
        match self {
            Encoding::Utf8 => PositionEncodingKind::UTF8,
            Encoding::Utf16 => PositionEncodingKind::UTF16,
            Encoding::Utf32 => PositionEncodingKind::UTF32,
        }
    }

    /// The byte offset of `position` in `snapshot`, clamped: a line at or past `line_count` is
    /// the document end, a character past the line end is the line end, and a position inside a
    /// character snaps to its start.
    #[must_use]
    pub fn offset(self, snapshot: &Snapshot, position: Position) -> u32 {
        // `point_to_offset` clamps a row past the end to the last line, which would land at a
        // column of the last line instead of the document end.
        if position.line >= snapshot.line_count() {
            return snapshot.len();
        }
        let start = snapshot.point_to_offset(Point::new(position.line, 0));
        let end = snapshot.point_to_offset(Point::new(position.line, u32::MAX));
        match self {
            Encoding::Utf8 => {
                snapshot.clip_offset(start + position.character.min(end - start), Bias::Left)
            }
            Encoding::Utf16 | Encoding::Utf32 => {
                start + self.bytes(snapshot.chunks(start..end), position.character)
            }
        }
    }

    /// The LSP position of byte `offset` in `snapshot`. An offset past the end clamps to the end;
    /// one inside a character snaps to its start.
    #[must_use]
    pub fn position(self, snapshot: &Snapshot, offset: u32) -> Position {
        let offset = snapshot.clip_offset(offset, Bias::Left);
        let point = snapshot.offset_to_point(offset);
        let character = match self {
            Encoding::Utf8 => point.col,
            Encoding::Utf16 | Encoding::Utf32 => {
                self.units(snapshot.chunks(offset - point.col..offset))
            }
        };
        Position::new(point.row, character)
    }

    /// The byte span of an LSP range in `snapshot`. Each end clamps as in
    /// [`offset`](Self::offset); an inverted range collapses to its end.
    #[must_use]
    pub fn span(self, snapshot: &Snapshot, range: lsp_types::Range) -> Range<u32> {
        let start = self.offset(snapshot, range.start);
        let end = self.offset(snapshot, range.end);
        start.min(end)..end
    }

    /// The LSP range of byte span `span` in `snapshot`. Each end clamps as in
    /// [`position`](Self::position).
    #[must_use]
    pub fn range(self, snapshot: &Snapshot, span: Range<u32>) -> lsp_types::Range {
        lsp_types::Range::new(
            self.position(snapshot, span.start),
            self.position(snapshot, span.end),
        )
    }

    /// The byte offset of `position` in plain `text`, clamped like [`offset`](Self::offset). A
    /// `\r\n` counts as one line break, so the end of a CRLF line is before its `\r`.
    #[must_use]
    pub fn text_offset(self, text: &str, position: Position) -> usize {
        let Some(line) = line_bounds(text, position.line) else {
            return text.len();
        };
        let content = &text[line.clone()];
        let bytes = match self {
            Encoding::Utf8 => {
                floor_char_boundary(content, (position.character as usize).min(content.len()))
            }
            Encoding::Utf16 | Encoding::Utf32 => self.bytes([content], position.character) as usize,
        };
        line.start + bytes
    }

    /// The byte span of an LSP range in plain `text`. Each end clamps as in
    /// [`text_offset`](Self::text_offset); an inverted range collapses to its end.
    #[must_use]
    pub fn text_span(self, text: &str, range: lsp_types::Range) -> Range<usize> {
        let start = self.text_offset(text, range.start);
        let end = self.text_offset(text, range.end);
        start.min(end)..end
    }

    /// Code units `c` occupies in this encoding.
    fn width(self, c: char) -> u32 {
        match self {
            Encoding::Utf8 => c.len_utf8() as u32,
            Encoding::Utf16 => c.len_utf16() as u32,
            Encoding::Utf32 => 1,
        }
    }

    /// Bytes spanned by the first `units` code units of `chunks`. A count that ends inside a
    /// character (half a surrogate pair, part of a utf-8 sequence) snaps left to that character's
    /// start; a count past the end stops at the end.
    pub(crate) fn bytes<'a>(self, chunks: impl IntoIterator<Item = &'a str>, units: u32) -> u32 {
        let mut bytes = 0;
        let mut left = units;
        for chunk in chunks {
            if left == 0 {
                break;
            }
            // An ASCII character is one unit in every encoding.
            if chunk.is_ascii() {
                let take = left.min(chunk.len() as u32);
                bytes += take;
                left -= take;
                continue;
            }
            for c in chunk.chars() {
                let width = self.width(c);
                if width > left {
                    return bytes;
                }
                bytes += c.len_utf8() as u32;
                left -= width;
                if left == 0 {
                    return bytes;
                }
            }
        }
        bytes
    }

    /// Code units in `chunks`.
    pub(crate) fn units<'a>(self, chunks: impl IntoIterator<Item = &'a str>) -> u32 {
        chunks
            .into_iter()
            .map(|chunk| {
                if chunk.is_ascii() {
                    return chunk.len() as u32;
                }
                match self {
                    Encoding::Utf8 => chunk.len() as u32,
                    Encoding::Utf16 => chunk.encode_utf16().count() as u32,
                    Encoding::Utf32 => chunk.chars().count() as u32,
                }
            })
            .sum()
    }
}

/// The content bytes of line `line` in `text` (without its `\n` or a `\r\n`), or `None` when the
/// text has fewer lines.
fn line_bounds(text: &str, line: u32) -> Option<Range<usize>> {
    let mut start = 0;
    for _ in 0..line {
        start += text[start..].find('\n')? + 1;
    }
    let end = text[start..].find('\n').map_or(text.len(), |at| start + at);
    let end = if text[start..end].ends_with('\r') {
        end - 1
    } else {
        end
    };
    Some(start..end)
}

/// The largest char boundary in `text` at or below `at`.
fn floor_char_boundary(text: &str, mut at: usize) -> usize {
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use scrive_core::Document;

    use super::*;

    /// Bytes: `a`=0, `é`=1..3, `€`=3..6, `😀`=6..10, `b`=10, end=11.
    const MIXED: &str = "aé€😀b";

    const ALL: [Encoding; 3] = [Encoding::Utf8, Encoding::Utf16, Encoding::Utf32];

    fn snapshot(text: &str) -> Snapshot {
        Document::new(text).expect("fixture loads").snapshot()
    }

    fn at(line: u32, character: u32) -> Position {
        Position::new(line, character)
    }

    fn assert_offsets(encoding: Encoding, text: &str, rows: &[((u32, u32), u32)]) {
        let snap = snapshot(text);
        for &((line, character), offset) in rows {
            assert_eq!(
                encoding.offset(&snap, at(line, character)),
                offset,
                "{encoding:?} ({line},{character}) in {text:?} should be byte {offset}",
            );
        }
    }

    fn assert_positions(encoding: Encoding, text: &str, rows: &[(u32, (u32, u32))]) {
        let snap = snapshot(text);
        for &(offset, (line, character)) in rows {
            assert_eq!(
                encoding.position(&snap, offset),
                at(line, character),
                "{encoding:?} byte {offset} in {text:?} should be ({line},{character})",
            );
        }
    }

    /// utf-8 characters are bytes; a count inside a multi-byte character snaps to its start.
    #[test]
    fn utf8_positions_convert_and_snap_left() {
        assert_offsets(
            Encoding::Utf8,
            MIXED,
            &[
                ((0, 0), 0),
                ((0, 1), 1),
                ((0, 2), 1),
                ((0, 3), 3),
                ((0, 6), 6),
                ((0, 7), 6),
                ((0, 10), 10),
                ((0, 11), 11),
                ((0, 99), 11),
            ],
        );
    }

    /// utf-16 counts a non-BMP character as two units, so offsets after the emoji shift by two.
    #[test]
    fn utf16_positions_count_surrogate_pairs() {
        assert_offsets(
            Encoding::Utf16,
            MIXED,
            &[
                ((0, 1), 1),
                ((0, 2), 3),
                ((0, 3), 6),
                ((0, 5), 10),
                ((0, 6), 11),
            ],
        );
    }

    /// utf-32 counts every scalar value as one unit.
    #[test]
    fn utf32_positions_count_scalar_values() {
        assert_offsets(
            Encoding::Utf32,
            MIXED,
            &[
                ((0, 1), 1),
                ((0, 2), 3),
                ((0, 3), 6),
                ((0, 4), 10),
                ((0, 5), 11),
            ],
        );
    }

    /// A position between the halves of a surrogate pair lands before the pair.
    #[test]
    fn mid_surrogate_position_snaps_left() {
        assert_offsets(Encoding::Utf16, MIXED, &[((0, 4), 6)]);
    }

    /// A line that does not exist is the document end, not a column of the last line.
    #[test]
    fn line_at_or_past_line_count_clamps_to_document_end() {
        assert_offsets(Encoding::Utf16, "", &[((1, 0), 0)]);
        assert_offsets(
            Encoding::Utf16,
            "\n\n",
            &[((1, 0), 1), ((2, 0), 2), ((3, 0), 2)],
        );
        assert_offsets(Encoding::Utf16, "a\nbc", &[((5, 0), 4)]);
    }

    /// The largest representable position is the document end in every encoding.
    #[test]
    fn maximal_position_clamps_to_document_end() {
        for encoding in ALL {
            assert_offsets(encoding, "a\nbc", &[((u32::MAX, u32::MAX), 4)]);
        }
    }

    /// A character past the line end stops before the line's `\n`.
    #[test]
    fn character_past_line_end_clamps_to_line_end() {
        for encoding in ALL {
            assert_offsets(encoding, "a\nbc", &[((0, 9), 1), ((1, 9), 4)]);
        }
    }

    /// A range whose start is after its end is the empty span at its end.
    #[test]
    fn inverted_range_collapses_to_its_end() {
        let snap = snapshot("abcd");
        assert_eq!(
            Encoding::Utf16.span(&snap, lsp_types::Range::new(at(0, 3), at(0, 1))),
            1..1,
            "(0,3)..(0,1) collapses to 1..1",
        );
    }

    /// Byte offsets convert to the character count of the negotiated unit.
    #[test]
    fn offsets_convert_back_to_positions() {
        assert_positions(
            Encoding::Utf16,
            MIXED,
            &[(10, (0, 5)), (6, (0, 3)), (11, (0, 6))],
        );
        assert_positions(Encoding::Utf32, MIXED, &[(10, (0, 4))]);
        assert_positions(Encoding::Utf8, MIXED, &[(10, (0, 10))]);
        assert_positions(Encoding::Utf16, "a\nbc", &[(3, (1, 1))]);
    }

    /// An offset inside a character counts from that character's start.
    #[test]
    fn mid_character_offset_snaps_left_before_conversion() {
        assert_positions(Encoding::Utf16, MIXED, &[(7, (0, 3))]);
        assert_positions(Encoding::Utf8, MIXED, &[(2, (0, 1))]);
    }

    /// A line longer than one rope chunk converts the same as a short one.
    #[test]
    fn conversion_walks_across_rope_chunk_boundaries() {
        let wide = "é".repeat(2000) + "x";
        assert_offsets(Encoding::Utf16, &wide, &[((0, 2000), 4000), ((0, 2001), 4001)]);
        assert_positions(Encoding::Utf16, &wide, &[(4000, (0, 2000))]);
        assert_offsets(Encoding::Utf16, &"a".repeat(3000), &[((0, 2500), 2500)]);
    }

    /// Disk text may end lines with `\r\n`; the `\r` is not part of the line.
    #[test]
    fn text_offsets_treat_crlf_as_one_line_break() {
        let text = "ab\r\ncd";
        assert_eq!(
            Encoding::Utf16.text_offset(text, at(0, 9)),
            2,
            "(0,9) stops before the \\r"
        );
        assert_eq!(
            Encoding::Utf16.text_offset(text, at(1, 1)),
            5,
            "(1,1) counts from after the \\n"
        );
        assert_eq!(
            Encoding::Utf16.text_span(text, lsp_types::Range::new(at(0, 0), at(1, 2))),
            0..6,
            "(0,0)..(1,2) spans the whole text",
        );
    }

    /// A line past the text's last line is the text end.
    #[test]
    fn text_line_past_the_end_clamps_to_text_end() {
        for (line, offset) in [(1, 2), (2, 2)] {
            assert_eq!(
                Encoding::Utf16.text_offset("a\n", at(line, 0)),
                offset,
                "({line},0) in \"a\\n\" should be byte {offset}",
            );
        }
    }

    /// utf-16 is what every server supports, so it is the fallback.
    #[test]
    fn absent_or_unknown_encoding_negotiates_utf16() {
        assert_eq!(
            Encoding::negotiate(None),
            Encoding::Utf16,
            "absent means utf-16"
        );
        assert_eq!(
            Encoding::negotiate(Some(&"utf-7".into())),
            Encoding::Utf16,
            "unknown means utf-16",
        );
    }

    /// Every encoding we advertise is recognised when a server picks it.
    #[test]
    fn advertised_kinds_negotiate_back_to_themselves() {
        for encoding in ALL {
            assert_eq!(
                Encoding::negotiate(Some(&encoding.kind())),
                encoding,
                "{encoding:?} negotiates back to itself",
            );
        }
    }
}
