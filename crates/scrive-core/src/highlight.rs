//! Syntax highlighting: a GUI-free public API over two private backends, one
//! per cargo feature. Backend types are named only inside this module, apart
//! from tree-sitter's `LanguageFn` handoff, so the GUI crate never has to pin a
//! backend's version.
//!
//! The core is language-agnostic. The app supplies the grammar and the theme,
//! and scrive-core ships neither.
//!
//! # Backends
//!
//! | feature | default | grammar | the app supplies |
//! |---------|---------|---------|------------------|
//! | `syntect` | on | `SyntaxDef::from_sublime_syntax` | a `.sublime-syntax` file |
//! | `tree-sitter` | off | `TreeSitterDef::new` | a grammar crate's `LANGUAGE` and a highlights query |
//!
//! Either converts into a [`Grammar`], which
//! [`Document::set_syntax`](crate::Document::set_syntax) takes, so the backend
//! is picked per document. Both produce the same [`HighlightSpan`] rows. With
//! neither feature a [`Grammar`] can't be built, and a
//! [`Document`](crate::Document) never highlights.
//!
//! ```
//! # #[cfg(feature = "syntect")]
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use scrive_core::{Document, SyntaxDef, TokenTheme};
//!
//! let grammar = SyntaxDef::from_sublime_syntax(
//!     "name: Demo\nscope: source.demo\ncontexts:\n  main:\n    - match: '\\bfn\\b'\n      scope: keyword.demo\n",
//! )?;
//! let mut doc = Document::new("fn main() {}\n")?;
//! doc.set_syntax(grammar, TokenTheme::builder().build());
//! doc.tokenize_highlight(doc.buffer().line_count());
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "syntect"))]
//! # fn main() {}
//! ```
//!
//! ```
//! # #[cfg(feature = "tree-sitter")]
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use scrive_core::{Document, TokenTheme, TreeSitterDef};
//!
//! let grammar = TreeSitterDef::new(tree_sitter_rust::LANGUAGE, tree_sitter_rust::HIGHLIGHTS_QUERY)?;
//! let mut doc = Document::new("fn main() {}\n")?;
//! doc.set_syntax(grammar, TokenTheme::builder().build());
//! // A first parse can take several calls; drive until there's nothing left.
//! while doc.highlight_frontier().is_some() {
//!     doc.tokenize_highlight(doc.buffer().line_count());
//! }
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "tree-sitter"))]
//! # fn main() {}
//! ```
//!
//! # Themes and captures
//!
//! One [`TokenTheme`] colors both backends. Build it from a `.tmTheme` with
//! `TokenTheme::from_tm_theme` (feature `syntect`), or in code with
//! [`TokenTheme::builder`]. A tree-sitter capture name resolves to its own
//! style, else to its longest dotted prefix's: `@function.method` takes
//! `function`'s style unless it has one of its own
//! ([`TokenTheme::resolve`]). [`TokenTheme::vocabulary`] lists the standard
//! captures. They map to TextMate scopes, so a builder theme colors syntect
//! too, and a `.tmTheme` colors tree-sitter captures.
//!
//! When captures overlap, the innermost node wins, and on one node the
//! query's earliest pattern wins, as in tree-sitter-highlight and the queries
//! grammar crates ship. One difference: a capture the theme doesn't style
//! doesn't claim its node, so a later pattern or the enclosing node's color
//! shows through. A partial theme still colors as much as it can.
//!
//! # The incremental engine
//!
//! Production reads go through the incremental engine a
//! [`Document`](crate::Document) owns once
//! [`set_syntax`](crate::Document::set_syntax) attaches a [`Grammar`]. An
//! edit shifts it in place, it converges lazily
//! ([`tokenize_highlight`](crate::Document::tokenize_highlight)), and a theme
//! change invalidates every line ([`set_theme`](crate::Document::set_theme)).
//! Untokenized lines return `None` and render in the default style, never an
//! error or a stall. Each drive tokenizes at most
//! [`HIGHLIGHT_MAX_LINES_PER_CALL`] lines and, under tree-sitter, advances the
//! parse by at most `HIGHLIGHT_MAX_PARSE_CHECKS_PER_CALL`, so no single call
//! stalls a frame. Under syntect, `Highlighter::highlight` tokenizes a whole
//! document top to bottom on each call. It is correct but O(lines), and the
//! tests use it as the convergence oracle.
//!
//! Spans live only in a window around the viewport
//! ([`set_highlight_window`](crate::Document::set_highlight_window)). Outside
//! it, the syntect backend keeps sparse checkpoints
//! (`HIGHLIGHT_CHECKPOINT_STRIDE`) so every row stays re-derivable: a fully
//! swept document holds `O(window + lines/stride)` line states, not
//! `O(lines)`. The tree-sitter backend keeps its parse tree instead, which
//! grows with the document. With tree-sitter-rust it measured 28 times the
//! text's size on scrive's own sources and 36 times on the bench corpus
//! (`benches/LEDGER.md`).
//!
//! While the code has syntax errors, tree-sitter's incremental reparse can
//! recover differently from a fresh parse of the same text, so the colors
//! can differ from what reloading the file shows. They agree again once the
//! text parses cleanly.

use core::ops::Range;

use crate::buffer::Buffer;
use crate::transaction::Committed;

#[cfg(feature = "syntect")]
use syntect::highlighting::{FontStyle, HighlightState, Highlighter as SyntectHighlighter, Style};
#[cfg(feature = "syntect")]
use syntect::parsing::{ParseState, ScopeStack, SyntaxDefinition, SyntaxReference, SyntaxSet, SyntaxSetBuilder};

#[cfg(feature = "tree-sitter")]
mod capture_paint;
#[cfg(any(feature = "syntect", feature = "tree-sitter"))]
mod dirty_ranges;
mod grammar;
#[cfg(feature = "syntect")]
mod line_state;
#[cfg(feature = "tree-sitter")]
mod parse_tree;
#[cfg(feature = "tree-sitter")]
mod rope_text;
pub(crate) mod splice;
pub mod token_theme;
#[cfg(feature = "tree-sitter")]
mod tree_sitter_def;
mod vocabulary;

#[cfg(feature = "syntect")]
use line_state::{tokenize_line, LineState};
pub use grammar::Grammar;
#[cfg(feature = "syntect")]
pub use line_state::{tokenize_segment, HighlightEngine, SegmentBoundary, SegmentStart, SegmentTokens};
pub use token_theme::TokenTheme;
#[cfg(feature = "syntect")]
pub use token_theme::ThemeError;
#[cfg(feature = "tree-sitter")]
pub use tree_sitter_def::{QueryErrorKind, TreeSitterDef, TreeSitterError};

/// A GUI-free color; scrive-iced maps it to `iced::Color` at render time.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Rgba {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
    /// Alpha.
    pub a: u8,
}

/// Resolved inline style for one run — the theme is consulted at tokenize time
/// (syntect's highlight iterator already yields resolved styles), so nothing
/// downstream re-touches the backend.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct SpanStyle {
    /// Foreground color.
    pub fg: Rgba,
    /// Bold.
    pub bold: bool,
    /// Italic.
    pub italic: bool,
}

/// One styled run within a single buffer line. `range` is **bytes within the
/// line** (not document offsets), so it survives line-local repair and drops
/// straight onto a display chunk; byte→cell conversion happens only at render.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HighlightSpan {
    /// Byte range within the line.
    pub range: Range<u32>,
    /// The run's resolved style.
    pub style: SpanStyle,
}

/// Failed to parse an injected `.sublime-syntax` grammar.
#[cfg(feature = "syntect")]
#[derive(Debug, thiserror::Error)]
#[error("invalid .sublime-syntax grammar: {0}")]
pub struct SyntaxError(String);

/// A parsed grammar. The app supplies a validated `.sublime-syntax` definition;
/// scrive-core ships no grammar of its own and stays language-agnostic.
#[cfg(feature = "syntect")]
pub struct SyntaxDef {
    set: SyntaxSet,
    name: String,
}

#[cfg(feature = "syntect")]
impl SyntaxDef {
    /// Parse an injected `.sublime-syntax` grammar (LF-only lines, no trailing
    /// newline in the regexes).
    pub fn from_sublime_syntax(s: &str) -> Result<Self, SyntaxError> {
        let syntax =
            SyntaxDefinition::load_from_str(s, false, None).map_err(|e| SyntaxError(e.to_string()))?;
        let name = syntax.name.clone();
        let mut builder = SyntaxSetBuilder::new();
        builder.add(syntax);
        Ok(Self { set: builder.build(), name })
    }

    /// The syntax reference to parse with — the one injected grammar.
    fn reference(&self) -> &SyntaxReference {
        self.set
            .find_syntax_by_name(&self.name)
            .or_else(|| self.set.syntaxes().first())
            .expect("the injected grammar is in its own set")
    }
}

/// Convert a syntect `(Style, byte range)` to a public [`HighlightSpan`] — the
/// one place a resolved style crosses out of syntect.
#[cfg(feature = "syntect")]
fn span_from(style: Style, range: Range<usize>) -> HighlightSpan {
    HighlightSpan { range: range.start as u32..range.end as u32, style: style_from(style) }
}

/// The public [`SpanStyle`] of a resolved syntect `Style`.
#[cfg(feature = "syntect")]
fn style_from(style: Style) -> SpanStyle {
    SpanStyle {
        fg: Rgba {
            r: style.foreground.r,
            g: style.foreground.g,
            b: style.foreground.b,
            a: style.foreground.a,
        },
        bold: style.font_style.contains(FontStyle::BOLD),
        italic: style.font_style.contains(FontStyle::ITALIC),
    }
}

/// A whole-document highlighter over a grammar + theme. Owns both; syntect stays
/// private behind it.
#[cfg(feature = "syntect")]
pub struct Highlighter {
    syntax: SyntaxDef,
    theme: TokenTheme,
}

#[cfg(feature = "syntect")]
impl Highlighter {
    /// A highlighter over an injected grammar and theme.
    #[must_use]
    pub fn new(syntax: SyntaxDef, theme: TokenTheme) -> Self {
        Self { syntax, theme }
    }

    /// Tokenize `text` (LF-only) into per-line spans — one `Vec` per line
    /// (including the trailing empty final line), each span a byte range within
    /// its line. State is carried line to line, so multi-line constructs resolve.
    #[must_use]
    pub fn highlight(&self, text: &str) -> Vec<Vec<HighlightSpan>> {
        let syntect = SyntectHighlighter::new(self.theme.syntect());
        let mut state = LineState {
            parse: ParseState::new(self.syntax.reference()),
            highlight: HighlightState::new(&syntect, ScopeStack::new()),
        };
        text.split('\n')
            .map(|line| {
                let (spans, next) = tokenize_line(&syntect, &self.syntax.set, &state, line);
                state = next;
                spans
            })
            .collect()
    }
}

/// Per-call tokenize budget, expressed as an op count — deterministic and
/// testable, unlike wall-clock. At ~2–20 µs of syntect per line this is
/// roughly 0.5–5 ms per call, so one drive can never stall a frame however
/// far a state-changing cascade wants to run; the idle sweep resumes where
/// the budget stopped.
pub const HIGHLIGHT_MAX_LINES_PER_CALL: u32 = 256;

/// Per-call tree-sitter parse budget, in progress-callback checks. The parser
/// checks in about every 100 parse operations, so this is an op count too:
/// deterministic, and free of the wall clock, which wasm32-unknown-unknown
/// lacks. Bytes parsed would mismeasure the work, since a reparse skips over
/// reused subtrees. With tree-sitter-rust in a release build, a call took
/// 2.2–2.6 ms at the median and at most 4.7 ms (`benches/LEDGER.md`). A parse
/// past the budget resumes on the next
/// [`Document::tokenize_highlight`](crate::Document::tokenize_highlight).
#[cfg(feature = "tree-sitter")]
pub const HIGHLIGHT_MAX_PARSE_CHECKS_PER_CALL: u32 = 100;

/// Sparse-checkpoint stride, in rows: outside the retention window the cache
/// keeps one end state per this many tokenized rows (plus each budget-stop
/// resume point), so any row's start state is re-derivable by tokenizing at
/// most a stride forward from the checkpoint above it. Memory per fully-swept
/// document: `lines / stride` states instead of `lines`.
#[cfg(feature = "syntect")]
pub const HIGHLIGHT_CHECKPOINT_STRIDE: u32 = 256;

/// Retention slack, in rows, kept on EACH side of the window handed to
/// [`Document::set_highlight_window`](crate::Document::set_highlight_window) —
/// scrolling within the slack costs nothing; beyond it, evicted rows refill
/// from the nearest checkpoint (budgeted).
pub const HIGHLIGHT_WINDOW_SLACK: u32 = 512;

/// Hard cap on the retention window's total length, in rows. A collapsed
/// mega-fold makes the reported visible range span the fold's hidden interior;
/// capping the window keeps retention bounded there instead of re-growing to
/// O(document). Rows past the cap render in the fallback style until scrolled
/// to (which re-aims the window at them); a fold hiding fewer than ~3k rows
/// still fits entirely.
pub const HIGHLIGHT_MAX_WINDOW_ROWS: u32 = 4096;

/// The retention/paint window for a viewport: `viewport` padded by
/// [`HIGHLIGHT_WINDOW_SLACK`] on each side, its length capped at
/// [`HIGHLIGHT_MAX_WINDOW_ROWS`], and clamped to `[0, n_lines]`. The **one**
/// owner of this formula:
/// [`Document::set_highlight_window`](crate::Document::set_highlight_window)
/// (what the cache *retains*) and any parallel-highlight worker pool (what it speculatively
/// *paints* and *tokenizes*) must aim at the SAME rows, so both call this
/// instead of re-deriving it — an integrator running the core's speculative
/// tokenizer on worker threads cannot drift from the core's retention rule. The
/// cap is load-bearing: a collapsed mega-fold makes the reported visible range
/// span the fold's hidden interior, and without it retention (or a pool's
/// synchronous speculate) would re-grow to O(document).
#[must_use]
pub fn padded_highlight_window(viewport: Range<u32>, n_lines: u32) -> Range<u32> {
    let start = viewport.start.saturating_sub(HIGHLIGHT_WINDOW_SLACK);
    let end = viewport
        .end
        .saturating_add(HIGHLIGHT_WINDOW_SLACK)
        .min(start.saturating_add(HIGHLIGHT_MAX_WINDOW_ROWS))
        .min(n_lines)
        .max(start);
    start..end
}

/// The document-owned incremental highlight cache: one facade over the
/// backend its [`Grammar`] selects, so `Document` drives every backend the same
/// way. Each method keeps the contract of the backend's own (`line_state::Cache`
/// with the `syntect` feature, `parse_tree::Cache` with `tree-sitter`). With
/// neither feature `Backend` is empty and no cache can exist, which is what
/// lets `Document` keep its highlight methods without a cfg.
#[derive(Debug)]
pub(crate) struct HighlightCache {
    backend: Backend,
}

#[derive(Debug)]
enum Backend {
    #[cfg(feature = "syntect")]
    Lines(line_state::Cache),
    #[cfg(feature = "tree-sitter")]
    Tree(parse_tree::Cache),
}

// The matches below are on the place `self.backend`, not on `&self.backend`:
// the compiler accepts an empty match only on the former.
#[cfg_attr(
    not(any(feature = "syntect", feature = "tree-sitter")),
    expect(unused_variables, reason = "no backend takes the arguments")
)]
impl HighlightCache {
    /// A cache sized to `buffer`, every line dirty, its window at the
    /// document top.
    pub(crate) fn new(grammar: Grammar, theme: TokenTheme, buffer: &Buffer) -> Self {
        match grammar.0 {
            #[cfg(feature = "syntect")]
            grammar::Inner::Syntect(def) => {
                Self { backend: Backend::Lines(line_state::Cache::new(def, theme, buffer.line_count())) }
            }
            #[cfg(feature = "tree-sitter")]
            grammar::Inner::TreeSitter(def) => {
                Self { backend: Backend::Tree(parse_tree::Cache::new(def, &theme, buffer.line_count())) }
            }
        }
    }

    /// Shift the cache through `committed`, whose edits `buffer` already holds.
    /// Only each edit's own lines are invalidated, not the first-to-last
    /// covering range, so a scattered multi-caret edit neither over-invalidates
    /// the lines between nor drags the window off the viewport.
    pub(crate) fn on_commit(&mut self, buffer: &Buffer, committed: &Committed) {
        if committed.patch().edits().is_empty() {
            return;
        }
        let spans = splice::line_splices(buffer, committed);
        debug_assert_eq!(
            spans.iter().map(|&(_, o, n)| i64::from(n) - i64::from(o)).sum::<i64>(),
            i64::from(buffer.line_count()) - i64::from(self.line_count()),
            "per-edit line deltas must sum to the buffer's line-count change",
        );
        match self.backend {
            #[cfg(feature = "syntect")]
            Backend::Lines(ref mut c) => c.on_commit_patch(&spans),
            #[cfg(feature = "tree-sitter")]
            Backend::Tree(ref mut c) => c.on_commit(buffer, committed, &spans),
        }
    }

    /// Tokenize toward row `target` (inclusive), at most `max_lines` lines;
    /// returns how many were tokenized. The tree-sitter backend works only
    /// within the window and ignores `target`.
    #[cfg_attr(
        not(feature = "syntect"),
        expect(unused_variables, reason = "only the syntect backend walks toward `target`")
    )]
    pub(crate) fn tokenize(&mut self, buffer: &Buffer, target: u32, max_lines: u32) -> u32 {
        match self.backend {
            #[cfg(feature = "syntect")]
            Backend::Lines(ref mut c) => c.tokenize_until(target, max_lines, |r| buffer.line(r)),
            #[cfg(feature = "tree-sitter")]
            Backend::Tree(ref mut c) => c.tokenize(buffer, max_lines),
        }
    }

    /// The next row [`HighlightCache::tokenize`] would work on; `None` when idle.
    pub(crate) fn pending(&self) -> Option<u32> {
        match self.backend {
            #[cfg(feature = "syntect")]
            Backend::Lines(ref c) => c.pending(),
            #[cfg(feature = "tree-sitter")]
            Backend::Tree(ref c) => c.pending(),
        }
    }

    /// The spans of `row`, or `None` if it isn't tokenized or retained.
    pub(crate) fn line_spans(&self, row: u32) -> Option<&[HighlightSpan]> {
        match self.backend {
            #[cfg(feature = "syntect")]
            Backend::Lines(ref c) => c.line_spans(row),
            #[cfg(feature = "tree-sitter")]
            Backend::Tree(ref c) => c.line_spans(row),
        }
    }

    /// Aim the retention window at the viewport `rows`.
    pub(crate) fn set_window(&mut self, rows: Range<u32>) {
        match self.backend {
            #[cfg(feature = "syntect")]
            Backend::Lines(ref mut c) => c.set_window(rows),
            #[cfg(feature = "tree-sitter")]
            Backend::Tree(ref mut c) => c.set_window(rows),
        }
    }

    /// The rows last handed to [`HighlightCache::set_window`].
    pub(crate) fn window_aim(&self) -> Range<u32> {
        match self.backend {
            #[cfg(feature = "syntect")]
            Backend::Lines(ref c) => c.window_aim(),
            #[cfg(feature = "tree-sitter")]
            Backend::Tree(ref c) => c.window_aim(),
        }
    }

    /// Swap the theme; every line repaints on the following tokenizes.
    pub(crate) fn set_theme(&mut self, theme: TokenTheme) {
        match self.backend {
            #[cfg(feature = "syntect")]
            Backend::Lines(ref mut c) => c.set_theme(theme),
            #[cfg(feature = "tree-sitter")]
            Backend::Tree(ref mut c) => c.set_theme(&theme),
        }
    }

    /// The line count the cache is sized for.
    pub(crate) fn line_count(&self) -> u32 {
        match self.backend {
            #[cfg(feature = "syntect")]
            Backend::Lines(ref c) => c.line_count(),
            #[cfg(feature = "tree-sitter")]
            Backend::Tree(ref c) => c.line_count(),
        }
    }

    /// A handle for off-thread segment tokenization, if the backend has one.
    #[cfg(feature = "syntect")]
    pub(crate) fn engine(&self) -> Option<HighlightEngine> {
        match self.backend {
            Backend::Lines(ref c) => Some(c.engine()),
            #[cfg(feature = "tree-sitter")]
            Backend::Tree(_) => None,
        }
    }

    /// Ingest an off-thread segment computed at the current revision; `false`
    /// if the backend takes none.
    #[cfg(feature = "syntect")]
    pub(crate) fn absorb(&mut self, seg: SegmentTokens, verified: bool) -> bool {
        match self.backend {
            Backend::Lines(ref mut c) => {
                c.absorb(seg, verified);
                true
            }
            #[cfg(feature = "tree-sitter")]
            Backend::Tree(_) => false,
        }
    }
}

#[cfg(all(test, feature = "syntect"))]
mod tests {
    use super::*;

    // A one-rule grammar: the word `kw` is a keyword.
    pub(super) const GRAMMAR: &str = "%YAML 1.2\n\
        ---\n\
        name: Test\n\
        scope: source.test\n\
        contexts:\n\
        \x20 main:\n\
        \x20   - match: '\\bkw\\b'\n\
        \x20     scope: keyword.control.test\n";

    // A minimal theme: default white, keyword red.
    pub(super) const THEME: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>name</key><string>Test</string>
<key>settings</key><array>
<dict><key>settings</key><dict><key>background</key><string>#000000</string><key>foreground</key><string>#ffffff</string></dict></dict>
<dict><key>scope</key><string>keyword</string><key>settings</key><dict><key>foreground</key><string>#ff0000</string></dict></dict>
</array></dict></plist>"#;

    pub(super) fn highlighter() -> Highlighter {
        Highlighter::new(
            SyntaxDef::from_sublime_syntax(GRAMMAR).expect("grammar parses"),
            TokenTheme::from_tm_theme(THEME).expect("theme parses"),
        )
    }

    #[test]
    fn keyword_gets_the_keyword_color() {
        let lines = highlighter().highlight("kw x");
        assert_eq!(lines.len(), 1);
        // The `kw` at bytes 0..2 is red; something else is white.
        let red = Rgba { r: 0xff, g: 0, b: 0, a: 0xff };
        let white = Rgba { r: 0xff, g: 0xff, b: 0xff, a: 0xff };
        let kw = lines[0].iter().find(|s| s.range == (0..2)).expect("a span at 0..2");
        assert_eq!(kw.style.fg, red);
        assert!(lines[0].iter().any(|s| s.style.fg == white), "non-keyword text is default white");
    }

    #[test]
    fn builder_theme_colors_syntect_tokens_through_the_vocabulary() {
        let red = Rgba { r: 0xff, g: 0, b: 0, a: 0xff };
        let white = Rgba { r: 0xff, g: 0xff, b: 0xff, a: 0xff };
        let theme = TokenTheme::builder()
            .foreground(white)
            .capture("keyword", SpanStyle { fg: red, bold: true, italic: false })
            .capture("not.in.vocabulary", SpanStyle { fg: red, bold: false, italic: false })
            .build();
        let syntax = SyntaxDef::from_sublime_syntax(GRAMMAR).unwrap();
        let lines = Highlighter::new(syntax, theme).highlight("kw x");
        let kw = lines[0].iter().find(|s| s.range == (0..2)).expect("a span at 0..2");
        assert_eq!(kw.style, SpanStyle { fg: red, bold: true, italic: false });
        let rest: Vec<_> = lines[0].iter().filter(|s| s.range.start >= 2).collect();
        assert!(!rest.is_empty());
        assert!(rest.iter().all(|s| s.style == SpanStyle { fg: white, bold: false, italic: false }));
    }

    #[test]
    fn builder_theme_lets_the_deeper_capture_win_a_shared_scope_under_syntect() {
        const ESCAPE_GRAMMAR: &str = "%YAML 1.2\n\
            ---\n\
            name: Test\n\
            scope: source.test\n\
            contexts:\n\
            \x20 main:\n\
            \x20   - match: '\\\\n'\n\
            \x20     scope: constant.character.escape.test\n";
        let red = Rgba { r: 0xff, g: 0, b: 0, a: 0xff };
        let green = Rgba { r: 0, g: 0xff, b: 0, a: 0xff };
        let deeper = ("string.escape", SpanStyle { fg: green, bold: false, italic: false });
        let shallower = ("escape", SpanStyle { fg: red, bold: false, italic: false });
        // Syntect keeps the first equally specific item, so only the
        // shallower-first order catches a missing depth sort.
        for [first, second] in [[deeper, shallower], [shallower, deeper]] {
            let theme = TokenTheme::builder().capture(first.0, first.1).capture(second.0, second.1).build();
            let syntax = SyntaxDef::from_sublime_syntax(ESCAPE_GRAMMAR).unwrap();
            let lines = Highlighter::new(syntax, theme).highlight("\\n");
            let escape = lines[0].iter().find(|s| s.range == (0..2)).expect("a span at 0..2");
            assert_eq!(escape.style.fg, green, "{} styled first", first.0);
        }
    }

    #[test]
    fn per_line_spans_and_a_trailing_empty_line() {
        let lines = highlighter().highlight("kw\nx\n");
        assert_eq!(lines.len(), 3); // "kw", "x", "" (trailing)
        assert!(lines[0].iter().any(|s| s.range == (0..2))); // "kw" on line 0
        assert!(lines[2].is_empty()); // the empty final line has no spans
    }
}
