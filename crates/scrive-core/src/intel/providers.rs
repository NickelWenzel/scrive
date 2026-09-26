//! The completion provider contract: the `Completions` trait the app
//! satisfies, plus the plain-data types it exchanges. Every type here is inert
//! data — no editor internal is reachable from a provider.

use core::ops::Range;

use crate::transaction::EditOp;
use crate::{DocId, Point};

/// Lookback window for `CompletionCx` (and, later, signature help), in lines —
/// the single named source (no bare `40` / "~40" scattered around). The
/// classifier sanitizes and scans at most this many lines ending at the caret.
pub const LOOKBACK_LINES: u32 = 40;

/// What caused a completion request — the classifier's dispatch ladder keys off
/// this.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CompletionTrigger {
    /// A completion-word char extended the current word.
    Typed(char),
    /// One of the trigger set `( , = : . ␣` that opens completion at a new spot.
    TriggerChar(char),
    /// Ctrl+Space, or the `retrigger` flag on a just-accepted item.
    Manual,
}

/// A revision-stamped request context — everything a provider may read. Complete
/// by construction: the provider never touches the document.
#[derive(Clone, Debug)]
pub struct CompletionCx {
    /// Which document the request is for (multi-tab guard — replies never cross).
    pub doc: DocId,
    /// The document revision the request was snapshotted at.
    pub revision: u64,
    /// Caret, clipped to a char boundary so a byte offset never splits a glyph.
    pub position: Point,
    /// Absolute byte range of the completion word under the caret (empty at a
    /// boundary), computed with [`is_completion_word_char`] — the default
    /// replace range on accept.
    pub word: Range<u32>,
    /// Raw text from the start of row `position.row − (LOOKBACK_LINES − 1)`
    /// (clamped to 0) up to and truncated at the caret. Deliberately
    /// **unsanitized** — stripping comments and strings is the classifier's own
    /// first step, so it receives the text verbatim.
    pub lookback: String,
    /// What caused this request (drives the classifier's dispatch ladder).
    pub trigger: CompletionTrigger,
}

/// The completion seam. **Synchronous by contract** (see the module docs): the
/// widget calls `complete()` in `update()` and opens/refreshes the popup from
/// the returned `Vec` the same frame — no async reply, no revision guard. A
/// genuinely slow provider would motivate an async variant of the seam; the
/// synchronous contract holds until one is needed.
pub trait Completions {
    /// Produce the completion items for `cx`. Called synchronously in the
    /// widget's `update()`; an empty `Vec` closes the popup.
    fn complete(&mut self, cx: &CompletionCx) -> Vec<CompletionItem>;
}

/// The kind of a completion item — drives the popup's per-row icon/label color.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CompletionKind {
    /// A language keyword (`if`, `for`, `return`).
    Keyword,
    /// A multi-line construct/snippet (`for x { … }`).
    Construct,
    /// A named parameter of the enclosing call (`param=`).
    Param,
    /// A value/enum member in value position.
    Value,
    /// A type name (`u8`, `String`).
    Type,
    /// A declared event or signal name.
    Event,
    /// A field of an event/struct receiver.
    Field,
    /// A user-declared symbol (a variable, function, or type).
    Symbol,
    /// A method on a dotted receiver (`obj.method`).
    Method,
}

/// What an accepted item inserts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InsertText {
    /// Inserted verbatim.
    Plain(String),
    /// LSP placeholder syntax (`$0`, `${1:name}`); parsed by the snippet engine
    /// **at accept time**, never earlier — items are inert data until accepted.
    Snippet(String),
}

/// One structured completion item. `#[non_exhaustive]` with a `new()` + `with_*`
/// builder chain, so adding a field never breaks an app that builds these:
/// construct with [`CompletionItem::new`], then layer on optional
/// detail/doc/replace/sort-key/retrigger/filter/additional/signature-after.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct CompletionItem {
    /// The text shown in the popup row (and the default filter/insert text).
    pub label: String,
    /// The item's category (drives the row icon/color).
    pub kind: CompletionKind,
    /// Short type/shape hint, rendered right-aligned in the popup row.
    pub detail: Option<String>,
    /// Plain-text documentation, rendered as a plain block in the popup;
    /// markdown rendering lives in the hover popup, not here.
    pub doc: Option<String>,
    /// What accepting the item inserts (plain text or a snippet).
    pub insert: InsertText,
    /// Absolute byte range to replace; `None` = the word range recomputed live
    /// at accept time (the word may have grown since the request).
    pub replace: Option<Range<u32>>,
    /// Tier prefix + label, sorted lexicographically: `0_` params (required
    /// before optional), `1_` symbols, `2_` constructs, `3_` keywords. Defaults
    /// to the label when not set explicitly.
    pub sort_key: String,
    /// When set, accepting the item fires exactly one `Manual` re-request after
    /// the transaction commits (e.g. `param=` reopens the popup in value
    /// position).
    pub retrigger: bool,
    /// The text the popup filters on when it differs from the label (a
    /// language server's `filterText`, e.g. `printf` for a `•printf(…)`
    /// label). `None` filters on the label. See [`matches`](Self::matches).
    pub filter: Option<String>,
    /// Edits elsewhere in the document that accepting the item also applies
    /// (an auto-import at the top, say), in the same coordinates as
    /// [`replace`](Self::replace). They commit in the same transaction as the
    /// insertion, so one undo reverts both. An edit touching the replaced word
    /// is dropped.
    pub additional: Vec<EditOp>,
    /// When set, accepting the item asks for signature help as if `(` had just
    /// been typed, for items that insert a call's opening paren.
    pub signature_after: bool,
}

impl CompletionItem {
    /// A minimal item: label, kind, and insertion. `sort_key` defaults to the
    /// label; detail/doc/replace/filter default absent; `retrigger` and
    /// `signature_after` false; no additional edits.
    #[must_use]
    pub fn new(label: impl Into<String>, kind: CompletionKind, insert: InsertText) -> Self {
        let label = label.into();
        Self {
            sort_key: label.clone(),
            label,
            kind,
            detail: None,
            doc: None,
            insert,
            replace: None,
            retrigger: false,
            filter: None,
            additional: Vec::new(),
            signature_after: false,
        }
    }

    /// Convenience: a plain-text item whose insertion equals its label.
    #[must_use]
    pub fn plain(label: impl Into<String>, kind: CompletionKind) -> Self {
        let label = label.into();
        Self::new(label.clone(), kind, InsertText::Plain(label))
    }

    /// Set the right-aligned type/shape hint.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Set the plain-text documentation block.
    #[must_use]
    pub fn with_doc(mut self, doc: impl Into<String>) -> Self {
        self.doc = Some(doc.into());
        self
    }

    /// Override the replace range (default = the live word range at accept time).
    #[must_use]
    pub fn with_replace(mut self, replace: Range<u32>) -> Self {
        self.replace = Some(replace);
        self
    }

    /// Set the sort key (tier prefix + label); defaults to the label.
    #[must_use]
    pub fn with_sort_key(mut self, sort_key: impl Into<String>) -> Self {
        self.sort_key = sort_key.into();
        self
    }

    /// Mark the item to fire one `Manual` re-request after acceptance commits.
    #[must_use]
    pub fn with_retrigger(mut self, retrigger: bool) -> Self {
        self.retrigger = retrigger;
        self
    }

    /// Filter on `filter` instead of the label.
    #[must_use]
    pub fn with_filter(mut self, filter: impl Into<String>) -> Self {
        self.filter = Some(filter.into());
        self
    }

    /// Edits elsewhere that accepting the item also applies (see
    /// [`additional`](Self::additional)).
    #[must_use]
    pub fn with_additional(mut self, additional: Vec<EditOp>) -> Self {
        self.additional = additional;
        self
    }

    /// Ask for signature help after accepting the item.
    #[must_use]
    pub fn with_signature_after(mut self, signature_after: bool) -> Self {
        self.signature_after = signature_after;
        self
    }

    /// Whether the item survives filtering by `word`: its filter text (or,
    /// without one, its label) starts with `word`, folding ASCII case. This is
    /// the one filtering rule every popup refilter and list reuse applies.
    #[must_use]
    pub fn matches(&self, word: &str) -> bool {
        prefix_matches(self.filter.as_deref().unwrap_or(&self.label), word)
    }
}

/// Completion-word predicate: `alphanumeric | '_' | '-'`. Deliberately **wider**
/// than the cursor-movement word classifier (where `-` is punctuation): kebab
/// keywords (`read-only`, `multi-word-name`) must filter as one word. This
/// one `pub fn` is the single seam if the two predicates ever need to converge.
#[must_use]
pub fn is_completion_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

/// Whether `label` begins with `prefix`, folding ASCII case only. Compares the
/// raw bytes (`as_bytes` never panics on a non-char-boundary split, and
/// `eq_ignore_ascii_case` leaves multi-byte UTF-8 exact), so no lowercased copy
/// is allocated — the hot completion-filter primitive.
fn prefix_matches(label: &str, prefix: &str) -> bool {
    let (l, p) = (label.as_bytes(), prefix.as_bytes());
    l.len() >= p.len() && l[..p.len()].eq_ignore_ascii_case(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_word_char_covers_kebab_identifiers() {
        assert!(is_completion_word_char('a'));
        assert!(is_completion_word_char('Z'));
        assert!(is_completion_word_char('9'));
        assert!(is_completion_word_char('_'));
        assert!(is_completion_word_char('-'), "kebab keywords filter as one word");
        assert!(!is_completion_word_char(' '));
        assert!(!is_completion_word_char('('));
        assert!(!is_completion_word_char('.'));
        assert!(!is_completion_word_char('='));
    }

    #[test]
    fn completion_item_builder_defaults_and_overrides() {
        let item = CompletionItem::plain("if", CompletionKind::Keyword);
        assert_eq!(item.label, "if");
        assert_eq!(item.insert, InsertText::Plain("if".into()));
        assert_eq!(item.sort_key, "if", "sort_key defaults to the label");
        assert!(item.detail.is_none() && item.doc.is_none() && item.replace.is_none());
        assert!(!item.retrigger);
        assert!(item.filter.is_none(), "filters on the label by default");
        assert!(item.additional.is_empty(), "no additional edits by default");
        assert!(!item.signature_after, "no signature follow-up by default");

        let built = CompletionItem::new("size", CompletionKind::Param, InsertText::Snippet("size=${1:8}".into()))
            .with_detail("u8")
            .with_doc("the block size")
            .with_sort_key("0_size")
            .with_replace(4..9)
            .with_retrigger(true)
            .with_filter("sz")
            .with_additional(vec![EditOp::insert(0, "use x;\n")])
            .with_signature_after(true);
        assert_eq!(built.detail.as_deref(), Some("u8"));
        assert_eq!(built.doc.as_deref(), Some("the block size"));
        assert_eq!(built.sort_key, "0_size");
        assert_eq!(built.replace, Some(4..9));
        assert!(built.retrigger);
        assert!(matches!(built.insert, InsertText::Snippet(_)));
        assert_eq!(built.filter.as_deref(), Some("sz"));
        assert_eq!(built.additional, vec![EditOp::insert(0, "use x;\n")]);
        assert!(built.signature_after);
    }

    /// `matches` reads the filter text when one is set, else the label, and
    /// folds ASCII case.
    #[test]
    fn matches_uses_the_filter_text_when_set() {
        let filtered = CompletionItem::plain("•printf(…)", CompletionKind::Symbol).with_filter("printf");
        assert!(filtered.matches("pri"), "the filter text is the prefix source");
        assert!(filtered.matches("PRI"), "ASCII case folds");
        assert!(!filtered.matches("•"), "the label is ignored once a filter is set");

        let plain = CompletionItem::plain("foo", CompletionKind::Symbol);
        assert!(plain.matches("FO"), "the label is the fallback prefix source");
        assert!(plain.matches(""), "an empty word matches everything");
        assert!(!plain.matches("x"), "a non-prefix does not match");
    }
}
