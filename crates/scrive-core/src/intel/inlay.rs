//! Inlay hints: short labels a language service places between buffer
//! characters, such as `: i32` after a binding or `name:` before an argument.
//! They are not buffer text. A host builds [`Hint`]s, pairs each with the
//! offset it was computed for ([`Placed`]), and installs the set with
//! [`Document::set_inlays`](crate::Document::set_inlays). From then on the
//! document moves every hint with the token it annotates.
//!
//! The model holds only what layout and gestures need. Tooltips, locations and
//! edits stay with the host, which finds them again by [`Key`].

pub mod interaction;
pub mod request;

pub use interaction::Interaction;
pub use request::Request;

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use crate::buffer::{Buffer, Revision};
use crate::coords::Bias;
use crate::decorations::{EmptyPolicy, Stickiness};
use crate::movement::is_word_char;

/// What a hint annotates, which decides the neighbour it sticks to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// A type annotation, following the token before it (`x: i32`).
    Type,
    /// A parameter name, preceding the argument after it (`n: 5`).
    Parameter,
    /// Anything else: the host's placement, and failing that the text
    /// around it, decide its side.
    Other,
}

/// Which neighbour a hint annotates, before the document has seen the text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Placement {
    /// The token ending at the hint's offset.
    Suffix,
    /// The token starting at the hint's offset.
    Prefix,
    /// Decided against the text at install: [`Side::Suffix`] after a word
    /// character, or before whitespace, a line end or one of `) ] } , ; .`;
    /// else [`Side::Prefix`].
    Auto,
}

/// Which neighbour an installed hint annotates. Text typed at a
/// [`Suffix`](Self::Suffix) hint's offset lands before it; at a
/// [`Prefix`](Self::Prefix) hint's offset, after it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    /// Annotates the token ending at its offset.
    Suffix,
    /// Annotates the token starting at its offset.
    Prefix,
}

/// Blank cells on either side of a label. Padding is editor background, not
/// part of the label.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Padding {
    /// One blank cell before the label.
    pub left: bool,
    /// One blank cell after the label.
    pub right: bool,
}

/// Whether a label part leads somewhere when activated.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Link {
    /// The host holds a location for this part.
    Jumps,
    /// The part is plain text.
    None,
}

/// Whether the host can turn a hint into buffer text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Insert {
    /// The host holds edits that insert this hint.
    Available,
    /// The hint is display-only.
    Unavailable,
}

/// A hint's identity, minted by the host and opaque to the core. The host
/// finds a hint's tooltip, locations and edits again by its key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Key(u64);

/// One piece of a hint's label. Its text never holds a control character.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Part {
    text: String,
    link: Link,
}

/// One inlay hint: what it annotates, its label, and its key. Immutable once
/// installed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Hint {
    kind: Kind,
    placement: Placement,
    label: Vec<Part>,
    padding: Padding,
    key: Key,
    insert: Insert,
    width: u32,
}

/// A hint paired with the byte offset it was computed for. This is what a host
/// hands to [`Document::set_inlays`](crate::Document::set_inlays).
#[derive(Clone, Debug)]
pub struct Placed {
    offset: u32,
    hint: Hint,
}

/// An installed hint's place in the document's inlay store: the hint, the
/// side it annotates, and its position in the set it arrived in. Only the
/// store creates one, and none ever leaves it.
#[derive(Clone, Debug)]
pub struct Anchor {
    hint: Arc<Hint>,
    side: Side,
    anchored: bool,
    index: u32,
}

/// One hint as it renders: where, on which side of that offset, and what.
#[derive(Clone, Debug)]
pub struct Shown {
    offset: u32,
    side: Side,
    index: u32,
    hint: Arc<Hint>,
}

/// What a display cell on an inlay hint holds.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum At {
    /// A label part.
    Label {
        /// The hint's key.
        key: Key,
        /// Index of the part in the hint's label.
        part: u32,
        /// The buffer offset the hint renders at.
        offset: u32,
        /// Whether the part leads somewhere.
        link: Link,
        /// Whether the host can insert the hint as text.
        insert: Insert,
        /// The part's display cells on the row.
        cells: Range<u32>,
    },
    /// A padding cell: editor background, so neither a hover or link target
    /// nor the word beneath it.
    Padding {
        /// The hint's key.
        key: Key,
        /// The buffer offset the hint renders at.
        offset: u32,
    },
}

/// The result of installing a hint set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// The set was current and replaced the previous one.
    Applied {
        /// How many hints were installed.
        count: usize,
    },
    /// The set was computed for another revision; nothing changed.
    Stale {
        /// The document's revision when the set was refused.
        current: Revision,
    },
}

/// Why a hint could not be built.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum Error {
    /// Every part of the label is empty or whitespace.
    #[error("an inlay hint label needs visible text")]
    EmptyLabel,
}

impl Key {
    /// The key with the host's raw identity `raw`.
    #[must_use]
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }
}

impl Part {
    /// A label part. Control characters in `text` (tabs and newlines included)
    /// become spaces, so every char occupies exactly one cell.
    #[must_use]
    pub fn new(text: impl Into<String>, link: Link) -> Self {
        let text = text
            .into()
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        Self { text, link }
    }

    /// The part's text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether the part leads somewhere.
    #[must_use]
    pub fn link(&self) -> Link {
        self.link
    }
}

impl Hint {
    /// A hint of `kind` labelled `label`, unpadded and display-only.
    ///
    /// # Errors
    ///
    /// [`Error::EmptyLabel`] when no part has visible text.
    pub fn new(kind: Kind, label: Vec<Part>, key: Key) -> Result<Self, Error> {
        if label.iter().all(|part| part.text.trim().is_empty()) {
            return Err(Error::EmptyLabel);
        }
        let padding = Padding::default();
        Ok(Self {
            kind,
            placement: default_placement(kind),
            width: width(&label, padding),
            label,
            padding,
            key,
            insert: Insert::Unavailable,
        })
    }

    /// The same hint with `padding`, which counts towards the width.
    #[must_use]
    pub fn padding(mut self, padding: Padding) -> Self {
        self.padding = padding;
        self.width = width(&self.label, padding);
        self
    }

    /// The same hint with `insert`.
    #[must_use]
    pub fn insert(mut self, insert: Insert) -> Self {
        self.insert = insert;
        self
    }

    /// The same hint, annotating the neighbour `placement` names instead of
    /// the one its kind implies.
    #[must_use]
    pub fn placement(mut self, placement: Placement) -> Self {
        self.placement = placement;
        self
    }

    /// What the hint annotates.
    #[must_use]
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The label's parts, in display order.
    #[must_use]
    pub fn parts(&self) -> &[Part] {
        &self.label
    }

    /// The blank cells around the label.
    #[must_use]
    pub fn padded(&self) -> Padding {
        self.padding
    }

    /// The host's key for this hint.
    #[must_use]
    pub fn key(&self) -> Key {
        self.key
    }

    /// Whether the host can insert this hint as text.
    #[must_use]
    pub fn insertable(&self) -> bool {
        self.insert == Insert::Available
    }

    /// Cells the hint occupies: its padding plus one cell per char of its
    /// label.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }
}

impl Placed {
    /// `hint`, computed for byte `offset` of the revision it will be
    /// installed at.
    #[must_use]
    pub fn new(offset: u32, hint: Hint) -> Self {
        Self { offset, hint }
    }

    /// The offset the hint was computed for.
    #[must_use]
    pub fn offset(&self) -> u32 {
        self.offset
    }

    /// The hint.
    #[must_use]
    pub fn hint(&self) -> &Hint {
        &self.hint
    }

    pub(crate) fn into_parts(self) -> (u32, Hint) {
        (self.offset, self.hint)
    }
}

impl Anchor {
    /// Anchor every hint in `placed` to the token it annotates in `buffer`:
    /// each hint's range, anchor and stickiness for the inlay store, in
    /// `placed` order.
    pub(crate) fn install(
        buffer: &Buffer,
        placed: Vec<Placed>,
    ) -> Vec<(Range<u32>, Self, Stickiness)> {
        let wanted: Vec<(u32, Side, Hint)> = placed
            .into_iter()
            .map(|placed| {
                let (offset, hint) = placed.into_parts();
                let offset = buffer.clip_offset(offset, Bias::Left);
                let side = match hint.placement {
                    Placement::Suffix => Side::Suffix,
                    Placement::Prefix => Side::Prefix,
                    Placement::Auto => auto_side(buffer, offset),
                };
                (offset, side, hint)
            })
            .collect();
        let mixed = mixed_offsets(&wanted);
        wanted
            .into_iter()
            .enumerate()
            .map(|(index, (offset, side, hint))| {
                // The LSP spec shows hints at one position in response order,
                // and only a shared side keeps that order next to the caret.
                let side = if mixed.contains(&offset) {
                    Side::Suffix
                } else {
                    side
                };
                place(buffer, offset, side, Arc::new(hint), index as u32)
            })
            .collect()
    }

    /// An anchor whose range covers the token its hint annotates.
    pub(crate) fn token(hint: Arc<Hint>, side: Side, index: u32) -> Self {
        Self {
            hint,
            side,
            anchored: true,
            index,
        }
    }

    /// A zero-width anchor, for a hint with no token beside it on its line.
    pub(crate) fn point(hint: Arc<Hint>, side: Side, index: u32) -> Self {
        Self {
            hint,
            side,
            anchored: false,
            index,
        }
    }

    pub(crate) fn hint(&self) -> &Hint {
        &self.hint
    }

    pub(crate) fn side(&self) -> Side {
        self.side
    }

    pub(crate) fn index(&self) -> u32 {
        self.index
    }

    pub(crate) fn is_anchored(&self) -> bool {
        self.anchored
    }

    /// A hint anchored to a token goes with it; a zero-width one stays until
    /// the next set replaces it.
    pub(crate) fn empty_policy(&self) -> EmptyPolicy {
        if self.anchored {
            EmptyPolicy::Drop
        } else {
            EmptyPolicy::Keep
        }
    }

    /// Where the hint renders on the row spanning bytes `row_start..=row_end`
    /// with text `line` (no newline), given its stored `range`, or `None`
    /// when it renders on another row. A suffix renders on the row holding
    /// its range start, a prefix on the row holding its range end, so each
    /// hint renders on exactly one row.
    pub(crate) fn render_offset(
        &self,
        range: Range<u32>,
        row_start: u32,
        row_end: u32,
        line: &str,
    ) -> Option<u32> {
        debug_assert_eq!(
            row_end - row_start,
            line.len() as u32,
            "the row's bounds are its line's"
        );
        let on_row = |offset: u32| (row_start..=row_end).contains(&offset);
        match self.side {
            Side::Suffix => on_row(range.start).then(|| range.end.min(row_end)),
            Side::Prefix => on_row(range.end).then(|| {
                let first_non_blank = row_end - line.trim_start().len() as u32;
                range.start.max(range.end.min(first_non_blank))
            }),
        }
    }

    /// This hint as rendered at `offset`.
    pub(crate) fn shown(&self, offset: u32) -> Shown {
        Shown {
            offset,
            side: self.side,
            index: self.index,
            hint: Arc::clone(&self.hint),
        }
    }
}

impl Shown {
    /// The hint's key.
    #[must_use]
    pub fn key(&self) -> Key {
        self.hint.key
    }

    /// The byte offset the hint renders at.
    #[must_use]
    pub fn offset(&self) -> u32 {
        self.offset
    }

    /// Which side of [`offset`](Self::offset) the hint annotates.
    #[must_use]
    pub fn side(&self) -> Side {
        self.side
    }

    /// The hint.
    #[must_use]
    pub fn hint(&self) -> &Hint {
        &self.hint
    }

    /// Render order: by offset; at one offset prefixes before suffixes, each
    /// in the order the set arrived in.
    pub(crate) fn render_order(&self) -> (u32, bool, u32) {
        (self.offset, self.side == Side::Suffix, self.index)
    }
}

fn width(label: &[Part], padding: Padding) -> u32 {
    let text: u32 = label
        .iter()
        .map(|part| part.text.chars().count() as u32)
        .sum();
    u32::from(padding.left) + text + u32::from(padding.right)
}

/// The neighbour a hint of `kind` annotates unless its host says otherwise.
fn default_placement(kind: Kind) -> Placement {
    match kind {
        Kind::Type => Placement::Suffix,
        Kind::Parameter => Placement::Prefix,
        Kind::Other => Placement::Auto,
    }
}

/// `Auto`'s rule: a suffix after a word or before a closer, separator or line
/// end; else a prefix. Measured against every hint kind rust-analyzer emits:
/// an adjustment such as `&*` before `&`, `*`, `(`, `"`, `[` or `|` annotates
/// the expression after it.
fn auto_side(buffer: &Buffer, offset: u32) -> Side {
    let word_before = buffer.char_before(offset).is_some_and(is_word_char);
    let closes = buffer
        .char_at(offset)
        .is_none_or(|c| c.is_whitespace() || matches!(c, ')' | ']' | '}' | ',' | ';' | '.'));
    if word_before || closes {
        Side::Suffix
    } else {
        Side::Prefix
    }
}

/// The fetch offsets where hints want both sides.
fn mixed_offsets(wanted: &[(u32, Side, Hint)]) -> HashSet<u32> {
    let mut first: HashMap<u32, Side> = HashMap::new();
    let mut mixed = HashSet::new();
    for &(offset, side, _) in wanted {
        if *first.entry(offset).or_insert(side) != side {
            mixed.insert(offset);
        }
    }
    mixed
}

fn place(
    buffer: &Buffer,
    offset: u32,
    side: Side,
    hint: Arc<Hint>,
    index: u32,
) -> (Range<u32>, Anchor, Stickiness) {
    match side {
        Side::Suffix => match token_ending_at(buffer, offset) {
            Some(start) => (
                start..offset,
                Anchor::token(hint, side, index),
                Stickiness::GrowsOnlyAfter,
            ),
            None => (
                offset..offset,
                Anchor::point(hint, side, index),
                Stickiness::GrowsOnlyAfter,
            ),
        },
        Side::Prefix => match token_starting_at(buffer, offset) {
            Some(end) => (
                offset..end,
                Anchor::token(hint, side, index),
                Stickiness::GrowsOnlyBefore,
            ),
            None => (
                offset..offset,
                Anchor::point(hint, side, index),
                Stickiness::NeverGrows,
            ),
        },
    }
}

/// Where the token ending at `offset` starts: the word there, else the one
/// char before it. `None` at a line or document start.
fn token_ending_at(buffer: &Buffer, offset: u32) -> Option<u32> {
    let mut start = offset;
    while let Some(c) = buffer.char_before(start).filter(|&c| is_word_char(c)) {
        start -= c.len_utf8() as u32;
    }
    if start < offset {
        return Some(start);
    }
    buffer
        .char_before(offset)
        .filter(|&c| c != '\n')
        .map(|c| offset - c.len_utf8() as u32)
}

/// Where the token starting at `offset` ends: the word there, else the one
/// char after it. `None` at a line or document end.
fn token_starting_at(buffer: &Buffer, offset: u32) -> Option<u32> {
    let mut end = offset;
    while let Some(c) = buffer.char_at(end).filter(|&c| is_word_char(c)) {
        end += c.len_utf8() as u32;
    }
    if end > offset {
        return Some(end);
    }
    buffer
        .char_at(offset)
        .filter(|&c| c != '\n')
        .map(|c| offset + c.len_utf8() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(text: &str) -> Vec<Part> {
        vec![Part::new(text, Link::None)]
    }

    /// A tab or newline in a label would break the one-cell-per-char grid,
    /// so every control character becomes a space.
    #[test]
    fn control_characters_in_a_label_become_spaces() {
        assert_eq!(
            Part::new("a\tb\nc\u{7}", Link::None).text(),
            "a b c ",
            "control chars are spaces"
        );
    }

    /// An installed hint always has visible text, so it always has width.
    #[test]
    fn a_label_without_visible_text_is_rejected() {
        for parts in [
            vec![],
            label(""),
            label(" \t"),
            vec![Part::new("", Link::None), Part::new("\n", Link::None)],
        ] {
            assert_eq!(
                Hint::new(Kind::Type, parts, Key::new(1)),
                Err(Error::EmptyLabel),
                "no visible text"
            );
        }
        let kept = Hint::new(
            Kind::Type,
            vec![Part::new("", Link::None), Part::new("x", Link::None)],
            Key::new(1),
        )
        .expect("one visible part is enough");
        assert_eq!(kept.parts().len(), 2, "empty parts keep their index");
    }

    /// Width is padding plus one cell per scalar value, whatever its byte length.
    #[test]
    fn width_counts_padding_and_scalar_values() {
        let hint = Hint::new(
            Kind::Other,
            vec![Part::new("ab", Link::None), Part::new("é😀", Link::Jumps)],
            Key::new(1),
        )
        .expect("visible");
        assert_eq!(hint.width(), 4, "two parts, four chars");
        assert_eq!(
            hint.padding(Padding {
                left: true,
                right: true
            })
            .width(),
            6,
            "padding adds a cell each side"
        );
    }

    /// A suffix renders at its range end, clamped to the row its range starts
    /// on; a prefix at the first non-blank of the row its range ends on; and
    /// neither renders on any other row.
    #[test]
    fn render_offset_clamps_each_side_to_its_own_row() {
        let hint = || Arc::new(Hint::new(Kind::Other, label("h"), Key::new(1)).expect("visible"));
        // "    foo()\n    " after Enter at the end of row 0: the suffix range grew over the newline.
        let suffix = Anchor::token(hint(), Side::Suffix, 0);
        assert_eq!(
            suffix.render_offset(8..14, 0, 9, "    foo()"),
            Some(9),
            "clamped to the end of its start row"
        );
        assert_eq!(
            suffix.render_offset(8..14, 10, 14, "    "),
            None,
            "not on the row its range ends on"
        );
        // "    foo(\n    a)" after Enter before `a`: the prefix range starts on row 0.
        let prefix = Anchor::token(hint(), Side::Prefix, 0);
        assert_eq!(
            prefix.render_offset(8..14, 9, 15, "    a)"),
            Some(13),
            "at the first non-blank of its end row"
        );
        assert_eq!(
            prefix.render_offset(8..14, 0, 8, "    foo("),
            None,
            "not on the row its range starts on"
        );
        assert_eq!(
            prefix.render_offset(8..9, 0, 10, "    foo(a)"),
            Some(8),
            "on one row it renders at its start"
        );
        let point = Anchor::point(hint(), Side::Prefix, 0);
        assert_eq!(
            point.render_offset(4..4, 0, 4, "foo("),
            Some(4),
            "a point renders where it sits"
        );
    }

    /// Type and parameter hints default to their fixed sides and other hints
    /// to `Auto`; padding never changes that, and `.placement` overrides it.
    #[test]
    fn placement_defaults_from_the_kind_and_can_be_overridden() {
        let hint = |kind| Hint::new(kind, label("h"), Key::new(1)).expect("visible");
        assert_eq!(
            hint(Kind::Type).placement,
            Placement::Suffix,
            "type hints follow their token"
        );
        assert_eq!(
            hint(Kind::Parameter).placement,
            Placement::Prefix,
            "parameter hints precede theirs"
        );
        assert_eq!(
            hint(Kind::Other).placement,
            Placement::Auto,
            "other hints wait for the text"
        );
        let padded = hint(Kind::Other).padding(Padding {
            left: false,
            right: true,
        });
        assert_eq!(
            padded.placement,
            Placement::Auto,
            "padding is the host's to read, not core's"
        );
        let placed = hint(Kind::Other).placement(Placement::Prefix);
        assert_eq!(
            placed.placement,
            Placement::Prefix,
            "the host's placement wins"
        );
    }

    /// `Auto` resolves against the text at the hint's offset, as measured on
    /// rust-analyzer's adjustment, lifetime, discriminant and drop hints.
    #[test]
    fn auto_side_reads_the_neighbouring_chars() {
        let cases = [
            (
                "let r = &s;",
                8,
                Side::Prefix,
                "`&*` before `&s` annotates the expression after",
            ),
            (
                "fn foo() {}",
                6,
                Side::Suffix,
                "`<'0>` after `foo` annotates the name",
            ),
            (
                "enum E { A, B }",
                10,
                Side::Suffix,
                "`= 0` after `A` annotates the variant",
            ),
            (
                "{ w }\n",
                5,
                Side::Suffix,
                "a drop hint at `}` before a line end annotates the block",
            ),
            (
                "let (x, y) = p;",
                4,
                Side::Prefix,
                "a binding-mode `&` before `(` annotates the pattern",
            ),
            (
                "let g = ||f;",
                10,
                Side::Prefix,
                "an adjustment after `||` annotates `f`",
            ),
            ("x.y", 1, Side::Suffix, "before `.` annotates what precedes"),
            (
                "a",
                1,
                Side::Suffix,
                "the buffer end annotates what precedes",
            ),
        ];
        for (text, at, side, why) in cases {
            let buffer = Buffer::new(text).expect("plain text loads");
            assert_eq!(auto_side(&buffer, at), side, "{why}");
        }
    }
}
