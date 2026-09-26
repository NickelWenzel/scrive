//! Hover replies converted to scrive's hover card.

use core::ops::Range;

use lsp_types::{HoverContents, LanguageString, MarkedString, MarkupContent, MarkupKind};
use scrive_core::intel::hover::escape_markdown;
use scrive_core::{HoverInfo, Snapshot};

use crate::{markdown, Encoding};

/// What a pending hover request asked.
#[derive(Clone, Debug)]
pub(crate) struct Query {
    /// The byte the pointer rested over.
    pub(crate) offset: u32,
    /// The word under the pointer: the card's range when the server gives none that fits.
    pub(crate) word: Range<u32>,
}

/// The card for `hover`, converted against the request snapshot, or `None` when the server said
/// nothing.
pub(crate) fn convert(
    encoding: Encoding,
    snapshot: &Snapshot,
    query: &Query,
    hover: lsp_types::Hover,
) -> Option<HoverInfo> {
    let markdown = contents(hover.contents);
    if markdown.trim().is_empty() {
        return None;
    }
    // The widget dismisses the card once the pointer leaves this range, so it must contain the
    // byte the pointer rests on.
    let range = hover
        .range
        .map(|range| encoding.span(snapshot, range))
        .filter(|span| span.contains(&query.offset))
        .unwrap_or_else(|| query.word.clone());
    Some(HoverInfo { markdown, range })
}

fn contents(contents: HoverContents) -> String {
    match contents {
        HoverContents::Scalar(marked) => marked_string(marked),
        HoverContents::Array(list) => list
            .into_iter()
            .map(marked_string)
            .filter(|part| !part.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }) => markdown::to_hover(&value),
        HoverContents::Markup(MarkupContent {
            kind: MarkupKind::PlainText,
            value,
        }) => escape_markdown(&value),
    }
}

fn marked_string(marked: MarkedString) -> String {
    match marked {
        MarkedString::String(text) => markdown::to_hover(&text),
        MarkedString::LanguageString(LanguageString { value, .. }) => markdown::code_lines(&value),
    }
}

#[cfg(test)]
mod tests {
    use scrive_core::Document;
    use serde_json::{json, Value};

    use super::*;

    /// The card for a reply over `let value = 1;` with the pointer on `value`.
    fn card(contents: Value, range: Option<((u32, u32), (u32, u32))>) -> Option<HoverInfo> {
        let doc = Document::new("let value = 1;").expect("fixture loads");
        let mut hover = json!({"contents": contents});
        if let Some((start, end)) = range {
            hover["range"] = json!({
                "start": {"line": start.0, "character": start.1},
                "end": {"line": end.0, "character": end.1},
            });
        }
        let query = Query {
            offset: 5,
            word: 4..9,
        };
        convert(
            Encoding::Utf16,
            &doc.snapshot(),
            &query,
            serde_json::from_value(hover).expect("fixture decodes"),
        )
    }

    fn markdown(contents: Value) -> String {
        card(contents, None).expect("a card").markdown
    }

    /// Plain text shows verbatim, markup characters included.
    #[test]
    fn plaintext_contents_are_escaped() {
        assert_eq!(
            markdown(json!({"kind": "plaintext", "value": "a*b `c`"})),
            escape_markdown("a*b `c`"),
            "plain text is escaped"
        );
    }

    /// Markdown is lowered to the card's subset.
    #[test]
    fn markdown_contents_go_through_to_hover() {
        assert_eq!(
            markdown(json!({"kind": "markdown", "value": "[Vec](u) is **big**"})),
            "Vec is **big**",
            "links lose their target, bold stays"
        );
    }

    /// Each non-empty part of a `MarkedString` array is its own paragraph.
    #[test]
    fn marked_string_array_is_joined_with_blank_lines() {
        assert_eq!(
            markdown(json!(["one", {"language": "rust", "value": "fn f()"}, ""])),
            "one\n\n`fn f()`",
            "parts are separated by a blank line, empty ones dropped"
        );
    }

    /// A language string is code, one span per line.
    #[test]
    fn language_string_becomes_code_lines() {
        assert_eq!(
            markdown(json!({"language": "rust", "value": "a\n\nb"})),
            "`a`\n\n`b`",
            "each line is a code span, blank lines stay blank"
        );
    }

    /// A reply with nothing to show shows no card.
    #[test]
    fn empty_contents_convert_to_none() {
        for contents in [
            json!(""),
            json!([]),
            json!({"kind": "markdown", "value": "  "}),
        ] {
            assert!(card(contents.clone(), None).is_none(), "{contents}");
        }
    }

    /// A server range under the pointer is the card's range.
    #[test]
    fn server_range_containing_the_offset_is_kept() {
        let info = card(json!("x"), Some(((0, 4), (0, 9)))).expect("a card");
        assert_eq!(info.range, 4..9, "the server's range is kept");
    }

    /// A server range the pointer is not in, an empty one included, would dismiss the card at
    /// once, so the word stands in.
    #[test]
    fn server_range_missing_the_offset_falls_back_to_the_word() {
        for range in [((0, 10), (0, 11)), ((0, 5), (0, 5))] {
            let info = card(json!("x"), Some(range)).expect("a card");
            assert_eq!(info.range, 4..9, "{range:?} falls back to the word");
        }
    }

    /// Without a server range the card covers the word.
    #[test]
    fn missing_range_falls_back_to_the_word() {
        let info = card(json!("x"), None).expect("a card");
        assert_eq!(info.range, 4..9, "the word is the range");
    }
}
