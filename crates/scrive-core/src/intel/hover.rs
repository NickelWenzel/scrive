//! Hover — the [`Hover`] trait the app satisfies, the plain data it returns,
//! and the [`HoverRequest`] an editor records when no provider is set. A
//! provider answers on the mouse-idle tick and the card shows the same frame.
//! An async answer lands only under the request's ticket, so a card for a word
//! the pointer has left, or for text that has changed, is dropped.

use core::ops::Range;

use crate::intel::ticket::Ticket;
use crate::{DocId, Point};

/// Mouse-idle delay before a hover query fires, in milliseconds. Tuned to the
/// ~300 ms mainstream editors use, so the popup feels neither jumpy nor
/// sluggish. The widget arms one query when the pointer rests over a word this
/// long without moving.
pub const HOVER_IDLE_DELAY_MS: u64 = 300;

/// The provider seam the integrating app implements to answer hover queries.
pub trait Hover {
    /// The doc for the word under the pointer, or `None` when there is none (or
    /// the pointer is not over a word) — the hover popup stays closed.
    fn hover(&mut self, cx: &HoverCx) -> Option<HoverInfo>;
}

/// A revision-stamped hover request — everything the provider may read.
#[derive(Clone, Debug)]
pub struct HoverCx {
    /// Which document the request is for.
    pub doc: DocId,
    /// The document revision the request was snapshotted at.
    pub revision: u64,
    /// The point under the pointer, clipped to a valid char boundary.
    pub position: Point,
    /// Absolute byte range of the word under the pointer, computed with
    /// `is_completion_word_char` — empty ⇒ the query is skipped.
    pub word: Range<u32>,
    /// The preceding source text, back the same number of lines as
    /// `CompletionCx`, giving the classifier the context the spec lookup needs
    /// (dotted receiver, in-call position).
    pub lookback: String,
}

/// A resolved hover: the markdown to render and the word it describes.
#[derive(Clone, Debug)]
pub struct HoverInfo {
    /// The card's text, in the small markdown grammar the hover card renders:
    /// - `**` toggles bold, outside code; a single `*` is literal;
    /// - `` ` `` toggles inline code; inside code `**` is literal;
    /// - `\*`, `` \` `` and `\\` are the literal characters in every style,
    ///   code included; any other `\` is literal;
    /// - each line renders as one line; nothing else is markup.
    ///
    /// Build text that must show verbatim (a diagnostic message, plain-text
    /// docs, a code line's content) with [`escape_markdown`].
    pub markdown: String,
    /// The word range the doc describes — the popup anchors here and the widget
    /// re-tests pointer containment against it for dismissal.
    pub range: Range<u32>,
}

/// A hover request an editor records for an async source when no synchronous
/// [`Hover`] provider is set. Answer it through the editor's `set_hover` with
/// `ticket`; `None` means no docs. A card of the diagnostics under the pointer
/// shows meanwhile, and the answer's docs join it.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct HoverRequest {
    /// The ticket the reply must carry.
    pub ticket: Ticket,
    /// The byte offset the pointer rested over.
    pub offset: u32,
    /// The word under the pointer (never empty: an empty word asks nothing).
    pub word: Range<u32>,
}

impl HoverRequest {
    /// A request for docs on `word`, asked at `offset` under `ticket`.
    #[must_use]
    pub fn new(ticket: Ticket, offset: u32, word: Range<u32>) -> Self {
        Self { ticket, offset, word }
    }
}

/// Escape `text` so the hover card shows it verbatim: every `\`, `*` and `` ` ``
/// gets a backslash (the grammar is on [`HoverInfo::markdown`]). A message like
/// `expected *mut T` then never switches the card to bold.
#[must_use]
pub fn escape_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '*' | '`') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exactly the three markup characters are escaped, each with one
    /// backslash.
    #[test]
    fn escape_markdown_backslashes_exactly_the_markup_chars() {
        for (raw, escaped) in [
            ("plain", "plain"),
            ("a*b", "a\\*b"),
            ("**x**", "\\*\\*x\\*\\*"),
            ("`c`", "\\`c\\`"),
            ("a\\b", "a\\\\b"),
            ("expected *mut T, found `&T`", "expected \\*mut T, found \\`&T\\`"),
            ("", ""),
        ] {
            assert_eq!(escape_markdown(raw), escaped, "{raw:?}");
        }
    }
}
