//! [`TreeSitterDef`], a validated tree-sitter grammar and highlights query,
//! and the whole-document pass that highlights with one.

use std::fmt;
use std::sync::Arc;

use tree_sitter::{Language, LanguageError, Parser, Query, QueryCursor};
use tree_sitter_language::LanguageFn;

use super::capture_paint;
use super::{HighlightSpan, SpanStyle, TokenTheme};

/// A tree-sitter grammar paired with its highlights query, both checked at
/// construction: a `TreeSitterDef` always parses and always queries.
///
/// `Clone` is cheap (the compiled query is shared).
///
/// ```
/// # use scrive_core::TreeSitterDef;
/// let def = TreeSitterDef::new(tree_sitter_rust::LANGUAGE, tree_sitter_rust::HIGHLIGHTS_QUERY)?;
/// # Ok::<(), scrive_core::highlight::TreeSitterError>(())
/// ```
///
/// Captures are colored by the [`TokenTheme`] capture rules, by name.
///
/// # wasm32-unknown-unknown
///
/// The grammar crate compiles C. Grammars generated with the tree-sitter CLI
/// 0.26 template or later build for wasm32-unknown-unknown as they are. An
/// older grammar crate (tree-sitter-rust 0.24, for one) fails on
/// `stdlib.h` unless the build points its C compiler at the libc headers
/// `tree-sitter-language` ships:
///
/// ```sh
/// export CFLAGS_wasm32_unknown_unknown="-isystem <tree-sitter-language>/wasm/include"
/// ```
///
/// where `<tree-sitter-language>` is that crate's source directory (its
/// `manifest_path` in `cargo metadata`, minus `Cargo.toml`).
#[derive(Clone)]
pub struct TreeSitterDef {
    language: Language,
    query: Arc<Query>,
}

/// Why a grammar and highlights query don't make a [`TreeSitterDef`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TreeSitterError {
    /// The grammar was generated for an ABI this tree-sitter runtime can't
    /// load.
    #[error("tree-sitter grammar ABI version {version} is not supported")]
    IncompatibleVersion {
        /// The grammar's ABI version.
        version: usize,
    },
    /// The runtime accepts the grammar's ABI version but can't parse with
    /// it.
    #[error("tree-sitter grammar can't be used for parsing")]
    NotParseable,
    /// The highlights query doesn't compile against the grammar.
    #[error("invalid highlights query at {row}:{column}: {message}")]
    Query {
        /// Zero-based row in the query source.
        row: usize,
        /// Zero-based byte column in the query source.
        column: usize,
        /// What kind of mistake it is.
        kind: QueryErrorKind,
        /// Tree-sitter's description.
        message: String,
    },
}

/// The kind of mistake in a highlights query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryErrorKind {
    /// Malformed query syntax.
    Syntax,
    /// A node type the grammar doesn't have.
    NodeType,
    /// A field name the grammar doesn't have.
    Field,
    /// A predicate names a capture the pattern doesn't define.
    Capture,
    /// A malformed predicate.
    Predicate,
    /// A pattern whose structure the grammar can never produce.
    Structure,
    /// The query and the grammar disagree on the language.
    Language,
}

impl fmt::Debug for TreeSitterDef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TreeSitterDef")
            .field("abi_version", &self.language.abi_version())
            .field("patterns", &self.query.pattern_count())
            .finish_non_exhaustive()
    }
}

impl TreeSitterDef {
    /// Pair a grammar crate's `LANGUAGE` with a highlights query (usually
    /// the crate's `HIGHLIGHTS_QUERY`).
    pub fn new(language: LanguageFn, highlights: &str) -> Result<Self, TreeSitterError> {
        let language = Language::new(language);
        Parser::new().set_language(&language).map_err(|e| match e {
            LanguageError::Version(version) => TreeSitterError::IncompatibleVersion { version },
            // The other variant, `Wasm`, exists only under tree-sitter's
            // wasmtime feature and means the same thing: unusable.
            _ => TreeSitterError::NotParseable,
        })?;
        let query = Query::new(&language, highlights).map_err(|e| TreeSitterError::Query {
            row: e.row,
            column: e.column,
            kind: query_error_kind(e.kind),
            message: e.message,
        })?;
        Ok(Self { language, query: Arc::new(query) })
    }

    /// A parser set to this grammar.
    pub(crate) fn parser(&self) -> Parser {
        let mut parser = Parser::new();
        parser.set_language(&self.language).expect("`new` checked the language loads");
        parser
    }

    /// The highlights query.
    pub(crate) fn query(&self) -> &Query {
        &self.query
    }

    /// The theme's style for each of the query's captures, by capture index.
    pub(crate) fn styles(&self, theme: &TokenTheme) -> Vec<Option<SpanStyle>> {
        self.query.capture_names().iter().map(|name| theme.resolve(name)).collect()
    }
}

/// Highlight `text` (LF-only) from a fresh parse: one `Vec` per line,
/// including the trailing empty final line, like
/// [`Highlighter::highlight`](super::Highlighter::highlight). The oracle the
/// incremental tree-sitter backend is tested against.
#[cfg_attr(not(test), expect(dead_code, reason = "the incremental backend's tests are the caller"))]
pub(crate) fn highlight_whole(def: &TreeSitterDef, theme: &TokenTheme, text: &str) -> Vec<Vec<HighlightSpan>> {
    let tree = def.parser().parse(text, None).expect("a parse with no cancellation always finishes");
    let mut cursor = QueryCursor::new();
    let captures = capture_paint::collect(&mut cursor, def.query(), tree.root_node(), text.as_bytes());
    let len = u32::try_from(text.len()).expect("documents are addressed with u32 offsets");
    let row_starts: Vec<u32> = std::iter::once(0)
        .chain(memchr::memchr_iter(b'\n', text.as_bytes()).map(|i| i as u32 + 1))
        .chain(std::iter::once(len + 1))
        .collect();
    capture_paint::paint_rows(captures, &row_starts, &def.styles(theme))
}

fn query_error_kind(kind: tree_sitter::QueryErrorKind) -> QueryErrorKind {
    match kind {
        tree_sitter::QueryErrorKind::Syntax => QueryErrorKind::Syntax,
        tree_sitter::QueryErrorKind::NodeType => QueryErrorKind::NodeType,
        tree_sitter::QueryErrorKind::Field => QueryErrorKind::Field,
        tree_sitter::QueryErrorKind::Capture => QueryErrorKind::Capture,
        tree_sitter::QueryErrorKind::Predicate => QueryErrorKind::Predicate,
        tree_sitter::QueryErrorKind::Structure => QueryErrorKind::Structure,
        tree_sitter::QueryErrorKind::Language => QueryErrorKind::Language,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlight::Rgba;

    const RED: Rgba = Rgba { r: 0xff, g: 0, b: 0, a: 0xff };
    const GREEN: Rgba = Rgba { r: 0, g: 0xff, b: 0, a: 0xff };
    const BLUE: Rgba = Rgba { r: 0, g: 0, b: 0xff, a: 0xff };

    fn plain(fg: Rgba) -> SpanStyle {
        SpanStyle { fg, bold: false, italic: false }
    }

    fn span(range: core::ops::Range<u32>, fg: Rgba) -> HighlightSpan {
        HighlightSpan { range, style: plain(fg) }
    }

    fn rust(query: &str) -> TreeSitterDef {
        TreeSitterDef::new(tree_sitter_rust::LANGUAGE, query).unwrap()
    }

    fn theme(captures: &[(&str, Rgba)]) -> TokenTheme {
        captures.iter().fold(TokenTheme::builder(), |b, &(name, fg)| b.capture(name, plain(fg))).build()
    }

    #[test]
    fn a_keyword_gets_its_color() {
        let def = rust(tree_sitter_rust::HIGHLIGHTS_QUERY);
        let rows = highlight_whole(&def, &theme(&[("keyword", RED)]), "fn main() {}");
        assert_eq!(rows, vec![vec![span(0..2, RED)]]);
    }

    #[test]
    fn an_escape_overrides_its_string() {
        let def = rust(tree_sitter_rust::HIGHLIGHTS_QUERY);
        let rows = highlight_whole(&def, &theme(&[("string", GREEN), ("escape", RED)]), r#"f("a\nb");"#);
        assert_eq!(rows, vec![vec![span(2..4, GREEN), span(4..6, RED), span(6..8, GREEN)]]);
    }

    #[test]
    fn the_earlier_pattern_wins_on_the_same_node() {
        let def = rust("(identifier) @keyword (identifier) @string");
        let rows = highlight_whole(&def, &theme(&[("keyword", RED), ("string", GREEN)]), "x;");
        assert_eq!(rows, vec![vec![span(0..1, RED)]]);
    }

    #[test]
    fn the_child_wins_over_a_parent_with_the_same_range() {
        // `#[a]`: the `attribute` node and its `identifier` child both span `a`.
        let colors = theme(&[("keyword", RED), ("string", GREEN)]);
        for query in ["(attribute) @string (identifier) @keyword", "(identifier) @keyword (attribute) @string"] {
            let rows = highlight_whole(&rust(query), &colors, "#[a]\nfn f() {}");
            assert_eq!(rows[0], vec![span(2..3, RED)], "{query}");
        }
    }

    #[test]
    fn an_unstyled_capture_falls_through_to_a_later_pattern() {
        // tree-sitter-rust captures `Foo` as `@constructor` before `@function`.
        let def = rust(tree_sitter_rust::HIGHLIGHTS_QUERY);
        let rows = highlight_whole(&def, &theme(&[("function", RED)]), "Foo(1);");
        assert_eq!(rows, vec![vec![span(0..3, RED)]]);
        let rows = highlight_whole(&def, &theme(&[("function", RED), ("constructor", BLUE)]), "Foo(1);");
        assert_eq!(rows, vec![vec![span(0..3, BLUE)]]);
    }

    #[test]
    fn a_block_comment_splits_per_row() {
        let def = rust(tree_sitter_rust::HIGHLIGHTS_QUERY);
        let rows = highlight_whole(&def, &theme(&[("comment", GREEN)]), "/* a\n\nb */ x;\n");
        assert_eq!(rows, vec![vec![span(0..4, GREEN)], vec![], vec![span(0..4, GREEN)], vec![]]);
    }

    #[test]
    fn eq_and_match_predicates_filter_captures() {
        let def = rust(r#"((identifier) @keyword (#eq? @keyword "foo")) ((identifier) @string (#match? @string "^B"))"#);
        let rows = highlight_whole(&def, &theme(&[("keyword", RED), ("string", GREEN)]), "foo; Bar; baz;");
        assert_eq!(rows, vec![vec![span(0..3, RED), span(5..8, GREEN)]]);
    }

    #[test]
    fn a_bad_query_reports_its_position() {
        let err = TreeSitterDef::new(tree_sitter_rust::LANGUAGE, "(identifier) @a\n  (no_such_node) @b").unwrap_err();
        let TreeSitterError::Query { row, column, kind, .. } = err else { panic!("{err:?}") };
        assert_eq!((row, column, kind), (1, 3, QueryErrorKind::NodeType));
    }

    #[test]
    fn empty_text_is_one_empty_row() {
        let def = rust(tree_sitter_rust::HIGHLIGHTS_QUERY);
        assert_eq!(highlight_whole(&def, &theme(&[("keyword", RED)]), ""), vec![Vec::<HighlightSpan>::new()]);
    }
}
