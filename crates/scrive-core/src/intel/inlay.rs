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

use crate::buffer::Revision;

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
}

fn width(label: &[Part], padding: Padding) -> u32 {
    let text: u32 = label
        .iter()
        .map(|part| part.text.chars().count() as u32)
        .sum();
    u32::from(padding.left) + text + u32::from(padding.right)
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
}
