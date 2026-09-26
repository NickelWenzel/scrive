//! Completion replies converted to scrive items.

use core::ops::Range;
use std::borrow::Cow;
use std::collections::HashMap;

use lsp_types::{
    CompletionContext, CompletionItemKind, CompletionTextEdit, Documentation, InsertTextFormat,
};
use scrive_core::{CompletionItem, CompletionKind, EditOp, InsertText, Snapshot};
use serde_json::Value;

use crate::{markdown, snippet, Encoding};

/// What a pending completion request asked.
#[derive(Clone, Debug)]
pub(crate) struct Query {
    /// The request word; its end is the caret the request was made at.
    pub(crate) word: Range<u32>,
    pub(crate) context: CompletionContext,
}

/// A converted item, before the work only a shown item needs: snippet lowering and
/// documentation.
#[derive(Debug)]
pub(crate) struct Candidate {
    item: CompletionItem,
    /// `item.insert` holds the raw LSP snippet body, lowered only when the item is shown.
    snippet: bool,
    documentation: Option<Documentation>,
}

/// A decoded `textDocument/completion` result, its items still undecoded.
#[derive(Debug)]
pub(crate) struct Reply {
    items: Vec<Value>,
}

/// One reply's conversion against the request snapshot. Item edit ranges almost always lie on
/// the request line and share one range, so that line is materialized once and its offsets are
/// memoized by character.
struct Conversion<'a> {
    encoding: Encoding,
    snapshot: &'a Snapshot,
    word: Range<u32>,
    row: u32,
    line_start: u32,
    line: Cow<'a, str>,
    memo: HashMap<u32, u32>,
}

impl Reply {
    /// `null`, an item array, or a `CompletionList`; `None` for anything else. Items stay
    /// `Value`s, so an item that does not decode costs only itself.
    pub(crate) fn decode(value: Value) -> Option<Self> {
        match value {
            Value::Null => Some(Self { items: Vec::new() }),
            Value::Array(items) => Some(Self { items }),
            Value::Object(mut list) => match list.remove("items") {
                Some(Value::Array(items)) => Some(Self { items }),
                _ => None,
            },
            _ => None,
        }
    }
}

impl Candidate {
    /// The item as shown: snippet lowered and documentation flattened to plain text.
    fn finish(&self) -> CompletionItem {
        let mut item = self.item.clone();
        if self.snippet {
            if let InsertText::Plain(body) = &self.item.insert {
                item.insert = snippet::lower(body);
            }
        }
        if let Some(documentation) = &self.documentation {
            item = item.with_doc(markdown::documentation(documentation));
        }
        item
    }
}

impl<'a> Conversion<'a> {
    fn new(encoding: Encoding, snapshot: &'a Snapshot, word: Range<u32>) -> Self {
        let point = snapshot.offset_to_point(word.end);
        Self {
            encoding,
            snapshot,
            row: point.row,
            line_start: word.end - point.col,
            line: snapshot.line(point.row),
            word,
            memo: HashMap::new(),
        }
    }

    fn candidate(&mut self, item: lsp_types::CompletionItem) -> Candidate {
        let snippet = item.insert_text_format == Some(InsertTextFormat::SNIPPET);
        let (text, edit) = match item.text_edit {
            Some(CompletionTextEdit::Edit(edit)) => (edit.new_text, self.span(edit.range)),
            Some(CompletionTextEdit::InsertAndReplace(edit)) => {
                (edit.new_text, self.span(edit.insert))
            }
            None => (item.insert_text.unwrap_or_else(|| item.label.clone()), None),
        };
        let sort_key = item.sort_text.unwrap_or_else(|| item.label.clone());
        let mut out = CompletionItem::new(item.label, kind(item.kind), InsertText::Plain(text))
            .with_sort_key(sort_key);
        if let Some(detail) = item.detail {
            out = out.with_detail(detail);
        }
        if let Some(filter) = item.filter_text {
            out = out.with_filter(filter);
        }
        if let Some(replace) = edit.filter(|span| *span != self.word) {
            out = out.with_replace(replace);
        }
        let additional: Vec<EditOp> = item
            .additional_text_edits
            .unwrap_or_default()
            .into_iter()
            .map(|edit| EditOp::new(self.encoding.span(self.snapshot, edit.range), edit.new_text))
            .collect();
        if !additional.is_empty() {
            out = out.with_additional(additional);
        }
        match item
            .command
            .as_ref()
            .map(|command| command.command.as_str())
        {
            Some("editor.action.triggerSuggest") => out = out.with_retrigger(true),
            Some("editor.action.triggerParameterHints") => out = out.with_signature_after(true),
            _ => {}
        }
        Candidate {
            item: out,
            snippet,
            documentation: item.documentation,
        }
    }

    /// The byte span of an edit range that lies on the request line and contains the caret;
    /// `None` sends the item back to the request word.
    fn span(&mut self, range: lsp_types::Range) -> Option<Range<u32>> {
        if range.start.line != self.row || range.end.line != self.row {
            return None;
        }
        let end = self.offset(range.end.character);
        let start = self.offset(range.start.character).min(end);
        (start <= self.word.end && self.word.end <= end).then_some(start..end)
    }

    fn offset(&mut self, character: u32) -> u32 {
        if let Some(&offset) = self.memo.get(&character) {
            return offset;
        }
        let offset = self.line_start + self.encoding.bytes([self.line.as_ref()], character);
        self.memo.insert(character, offset);
        offset
    }
}

/// Converts a reply's items against the request snapshot, where `word` is the request word.
/// Items that do not decode are skipped.
pub(crate) fn convert(
    encoding: Encoding,
    snapshot: &Snapshot,
    word: Range<u32>,
    reply: Reply,
) -> Vec<Candidate> {
    let mut conversion = Conversion::new(encoding, snapshot, word);
    reply
        .items
        .into_iter()
        .filter_map(|value| serde_json::from_value::<lsp_types::CompletionItem>(value).ok())
        .map(|item| conversion.candidate(item))
        .collect()
}

/// The candidates that match `word`, as shown.
pub(crate) fn answer(candidates: &[Candidate], word: &str) -> Vec<CompletionItem> {
    candidates
        .iter()
        .filter(|candidate| candidate.item.matches(word))
        .map(Candidate::finish)
        .collect()
}

/// LSP's 25 kinds folded onto scrive's popup categories. scrive's `Param` has no LSP source.
fn kind(kind: Option<CompletionItemKind>) -> CompletionKind {
    match kind {
        Some(CompletionItemKind::KEYWORD) => CompletionKind::Keyword,
        Some(CompletionItemKind::SNIPPET) => CompletionKind::Construct,
        Some(CompletionItemKind::METHOD) => CompletionKind::Method,
        Some(CompletionItemKind::FIELD | CompletionItemKind::PROPERTY) => CompletionKind::Field,
        Some(
            CompletionItemKind::CLASS
            | CompletionItemKind::INTERFACE
            | CompletionItemKind::STRUCT
            | CompletionItemKind::ENUM
            | CompletionItemKind::TYPE_PARAMETER,
        ) => CompletionKind::Type,
        Some(
            CompletionItemKind::VALUE
            | CompletionItemKind::ENUM_MEMBER
            | CompletionItemKind::CONSTANT
            | CompletionItemKind::COLOR
            | CompletionItemKind::UNIT,
        ) => CompletionKind::Value,
        Some(CompletionItemKind::EVENT) => CompletionKind::Event,
        _ => CompletionKind::Symbol,
    }
}

#[cfg(test)]
mod tests {
    use scrive_core::Document;
    use serde_json::json;

    use super::*;

    fn snapshot(text: &str) -> Snapshot {
        Document::new(text).expect("fixture loads").snapshot()
    }

    fn range(start: (u32, u32), end: (u32, u32)) -> Value {
        json!({"start": {"line": start.0, "character": start.1}, "end": {"line": end.0, "character": end.1}})
    }

    /// The candidates for `items` requested at `word` in `text`, over utf-16.
    fn candidates(text: &str, word: Range<u32>, items: Value) -> Vec<Candidate> {
        let reply = Reply::decode(items).expect("fixture decodes");
        convert(Encoding::Utf16, &snapshot(text), word, reply)
    }

    /// The one item `item` converts to, as shown.
    fn one(text: &str, word: Range<u32>, item: Value) -> CompletionItem {
        let [candidate] = candidates(text, word, json!([item]))
            .try_into()
            .expect("one candidate");
        candidate.finish()
    }

    /// clangd labels items with a bullet and filters on `filterText`; a text edit over exactly
    /// the word leaves the replace range live.
    #[test]
    fn clangd_bullet_item_filters_on_filter_text_and_keeps_the_word_range() {
        let item = one(
            "int x;\npri",
            7..10,
            json!({
                "label": "•printf(const char *restrict, ...)", "kind": 3, "detail": "int",
                "filterText": "printf", "sortText": "3f2b1c3cprintf", "insertTextFormat": 2,
                "textEdit": {"range": range((1, 0), (1, 3)), "newText": "printf(${1:const char *restrict format, ...})"},
            }),
        );
        assert!(item.matches("pri"), "the filter text matches the word");
        assert_eq!(
            item.replace, None,
            "an edit over the word keeps the live word"
        );
        assert_eq!(item.sort_key, "3f2b1c3cprintf", "sortText is the sort key");
        assert_eq!(item.kind, CompletionKind::Symbol, "a function is a symbol");
        assert_eq!(item.detail.as_deref(), Some("int"), "detail is kept");
        assert_eq!(
            item.insert,
            InsertText::Snippet("printf(${1:const char *restrict format, ...})".to_owned()),
            "the snippet body is lowered for scrive"
        );
    }

    /// Additional edits convert against the request snapshot, like the main edit.
    #[test]
    fn additional_text_edits_convert_against_the_request_snapshot() {
        let item = one(
            "int x;\npri",
            7..10,
            json!({"label": "printf", "additionalTextEdits": [
                {"range": range((0, 0), (0, 0)), "newText": "#include <stdio.h>\n"},
            ]}),
        );
        assert_eq!(
            item.additional,
            vec![EditOp::insert(0, "#include <stdio.h>\n")],
            "the auto-import lands at the top"
        );
    }

    /// `triggerParameterHints` is how servers ask for signature help after a call's `(`.
    #[test]
    fn trigger_parameter_hints_command_sets_signature_after() {
        let item = one(
            "f",
            0..1,
            json!({"label": "f", "command": {"title": "", "command": "editor.action.triggerParameterHints"}}),
        );
        assert!(item.signature_after, "signature help follows the accept");
        assert!(!item.retrigger, "completion does not reopen");
    }

    /// `triggerSuggest` reopens completion after the accept.
    #[test]
    fn trigger_suggest_command_sets_retrigger() {
        let item = one(
            "f",
            0..1,
            json!({"label": "f", "command": {"title": "", "command": "editor.action.triggerSuggest"}}),
        );
        assert!(item.retrigger, "completion reopens");
        assert!(!item.signature_after, "no signature help");
    }

    /// One malformed item costs only itself.
    #[test]
    fn undecodable_item_is_skipped() {
        let items = candidates("", 0..0, json!([{"label": 5}, {"label": "ok"}]));
        let labels: Vec<_> = items.iter().map(|c| c.item.label.as_str()).collect();
        assert_eq!(labels, ["ok"], "the bad item is skipped");
    }

    /// The client does not advertise insert/replace, but a server that sends one anyway gets its
    /// insert range.
    #[test]
    fn insert_replace_edit_uses_its_insert_range() {
        let item = one(
            "fobar",
            0..2,
            json!({"label": "foo", "textEdit": {"newText": "foo", "insert": range((0, 0), (0, 2)), "replace": range((0, 0), (0, 5))}}),
        );
        assert_eq!(item.replace, None, "the insert range is the word");
        assert_eq!(
            item.insert,
            InsertText::Plain("foo".to_owned()),
            "newText is inserted"
        );
    }

    /// A multi-line edit range cannot be a completion range; the word replaces it.
    #[test]
    fn edit_range_spanning_lines_falls_back_to_the_word() {
        let item = one(
            "int x;\npri",
            7..10,
            json!({"label": "p", "textEdit": {"range": range((0, 0), (1, 3)), "newText": "p"}}),
        );
        assert_eq!(item.replace, None, "the word replaces a multi-line range");
    }

    /// A range the caret is outside of would edit text the user is not completing.
    #[test]
    fn edit_range_not_containing_the_caret_falls_back_to_the_word() {
        let item = one(
            "int x;\npri",
            7..10,
            json!({"label": "p", "textEdit": {"range": range((1, 0), (1, 1)), "newText": "p"}}),
        );
        assert_eq!(
            item.replace, None,
            "the word replaces a range away from the caret"
        );
    }

    /// A range wider than the word on the request line is honoured.
    #[test]
    fn edit_range_other_than_the_word_is_kept_as_replace() {
        let item = one(
            "a.pr",
            2..4,
            json!({"label": "->print", "textEdit": {"range": range((0, 1), (0, 4)), "newText": "->print"}}),
        );
        assert_eq!(
            item.replace,
            Some(1..4),
            "the server's range replaces the dot too"
        );
    }

    /// Without a text edit, `insertText` is inserted, and without that the label.
    #[test]
    fn missing_text_edit_inserts_insert_text_or_label() {
        let items = candidates(
            "",
            0..0,
            json!([{"label": "a", "insertText": "b"}, {"label": "c"}]),
        );
        let inserts: Vec<_> = items.iter().map(|c| c.item.insert.clone()).collect();
        assert_eq!(
            inserts,
            [
                InsertText::Plain("b".to_owned()),
                InsertText::Plain("c".to_owned())
            ],
            "insertText, then the label"
        );
    }

    /// Server ordering survives through the sort key.
    #[test]
    fn sort_text_becomes_the_sort_key_and_defaults_to_the_label() {
        let items = candidates(
            "",
            0..0,
            json!([{"label": "a", "sortText": "0002"}, {"label": "b"}]),
        );
        let keys: Vec<_> = items.iter().map(|c| c.item.sort_key.as_str()).collect();
        assert_eq!(keys, ["0002", "b"], "sortText, then the label");
    }

    /// Every LSP kind lands in one of scrive's categories.
    #[test]
    fn item_kinds_map_to_scrive_kinds() {
        for (lsp, scrive) in [
            (Some(CompletionItemKind::KEYWORD), CompletionKind::Keyword),
            (Some(CompletionItemKind::SNIPPET), CompletionKind::Construct),
            (Some(CompletionItemKind::METHOD), CompletionKind::Method),
            (Some(CompletionItemKind::FIELD), CompletionKind::Field),
            (Some(CompletionItemKind::PROPERTY), CompletionKind::Field),
            (Some(CompletionItemKind::CLASS), CompletionKind::Type),
            (Some(CompletionItemKind::INTERFACE), CompletionKind::Type),
            (Some(CompletionItemKind::STRUCT), CompletionKind::Type),
            (Some(CompletionItemKind::ENUM), CompletionKind::Type),
            (
                Some(CompletionItemKind::TYPE_PARAMETER),
                CompletionKind::Type,
            ),
            (Some(CompletionItemKind::VALUE), CompletionKind::Value),
            (Some(CompletionItemKind::ENUM_MEMBER), CompletionKind::Value),
            (Some(CompletionItemKind::CONSTANT), CompletionKind::Value),
            (Some(CompletionItemKind::COLOR), CompletionKind::Value),
            (Some(CompletionItemKind::UNIT), CompletionKind::Value),
            (Some(CompletionItemKind::EVENT), CompletionKind::Event),
            (Some(CompletionItemKind::FUNCTION), CompletionKind::Symbol),
            (Some(CompletionItemKind::VARIABLE), CompletionKind::Symbol),
            (Some(CompletionItemKind::MODULE), CompletionKind::Symbol),
            (Some(CompletionItemKind::TEXT), CompletionKind::Symbol),
            (None, CompletionKind::Symbol),
        ] {
            assert_eq!(kind(lsp), scrive, "{lsp:?}");
        }
    }

    /// All three result shapes decode; anything else is an error.
    #[test]
    fn null_array_and_list_replies_decode() {
        for (value, count) in [
            (json!(null), 0),
            (json!([{"label": "a"}]), 1),
            (
                json!({"isIncomplete": true, "items": [{"label": "a"}, {"label": "b"}]}),
                2,
            ),
        ] {
            let reply = Reply::decode(value.clone()).expect("decodes");
            assert_eq!(reply.items.len(), count, "{value} has {count} items");
        }
        for value in [json!({"items": 3}), json!("x")] {
            assert!(Reply::decode(value.clone()).is_none(), "{value} is refused");
        }
    }

    /// A shown item has its snippet lowered and its markdown documentation as plain text.
    #[test]
    fn finishing_lowers_snippets_and_flattens_documentation() {
        let item = one(
            "",
            0..0,
            json!({"label": "f", "insertText": "f(${1:x})$0", "insertTextFormat": 2,
                "documentation": {"kind": "markdown", "value": "**Bold** `code`"}}),
        );
        assert_eq!(
            item.insert,
            InsertText::Snippet("f(${1:x})$0".to_owned()),
            "the snippet is lowered"
        );
        assert_eq!(
            item.doc.as_deref(),
            Some("Bold code"),
            "markdown is flattened"
        );
    }

    /// Only items matching the word are shown.
    #[test]
    fn answer_keeps_only_matching_items() {
        let items = candidates(
            "",
            0..0,
            json!([{"label": "print"}, {"label": "other"}, {"label": "x", "filterText": "prx"}]),
        );
        let shown: Vec<_> = answer(&items, "pr").into_iter().map(|i| i.label).collect();
        assert_eq!(shown, ["print", "x"], "label or filter text must match");
    }
}
