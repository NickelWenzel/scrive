//! Syntax highlighting — a GUI-free public surface over a private syntect
//! engine. **Syntect is named only inside this module**; nothing from it
//! crosses the `pub` line, so the GUI crate never has to pin syntect's version.
//!
//! The core is **language-agnostic**: the grammar is an app-supplied
//! `.sublime-syntax`, injected as text (`SyntaxDef::from_sublime_syntax`), and
//! the theme an app-supplied `.tmTheme` (`TokenTheme::from_tm_theme`) or
//! capture styles built in code (`TokenTheme::builder`); scrive-core ships
//! neither.
//!
//! # Two entry points
//!
//! [`Highlighter::highlight`] tokenizes the whole document top-to-bottom on each
//! call (state carried line to line) — correct but O(lines), used as the
//! convergence oracle in tests. Production reads go through [`HighlightCache`],
//! the incremental engine: geometric shift on edit ([`HighlightCache::on_commit`]),
//! lazy end-state convergence ([`HighlightCache::tokenize_until`]), and full
//! invalidation on theme change ([`HighlightCache::set_theme`]). Untokenized
//! lines return `None` and render in the default style — never an error or a
//! stall. Each drive tokenizes at most [`HIGHLIGHT_MAX_LINES_PER_CALL`] lines,
//! so no single call can stall a frame however far a cascade wants to run.
//!
//! Retention is **virtualized** so RAM does not grow with the idle sweep:
//! spans + per-line states live only in a window around the viewport
//! ([`HighlightCache::set_window`]); everywhere else, sparse checkpoints
//! ([`HIGHLIGHT_CHECKPOINT_STRIDE`]) keep every row re-derivable. A fully swept
//! document holds `O(window + lines/stride)`, not `O(lines)` — see
//! [`HighlightCache`] for the mechanics.

use core::ops::Range;

use syntect::highlighting::{FontStyle, HighlightState, Highlighter as SyntectHighlighter, Style};
use syntect::parsing::{ParseState, ScopeStack, SyntaxDefinition, SyntaxReference, SyntaxSet, SyntaxSetBuilder};

mod dirty_ranges;
mod line_state;
pub(crate) mod splice;
pub mod token_theme;
mod vocabulary;

use line_state::{tokenize_line, LineState};
pub use line_state::{
    tokenize_segment, HighlightCache, HighlightEngine, SegmentBoundary, SegmentStart, SegmentTokens,
};
pub use token_theme::{ThemeError, TokenTheme};

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
/// downstream re-touches syntect.
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
#[derive(Debug, thiserror::Error)]
#[error("invalid .sublime-syntax grammar: {0}")]
pub struct SyntaxError(String);

/// A parsed grammar. The app supplies a validated `.sublime-syntax` definition;
/// scrive-core ships no grammar of its own and stays language-agnostic.
pub struct SyntaxDef {
    set: SyntaxSet,
    name: String,
}

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
fn span_from(style: Style, range: Range<usize>) -> HighlightSpan {
    HighlightSpan { range: range.start as u32..range.end as u32, style: style_from(style) }
}

/// The public [`SpanStyle`] of a resolved syntect `Style`.
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
pub struct Highlighter {
    syntax: SyntaxDef,
    theme: TokenTheme,
}

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

/// Sparse-checkpoint stride, in rows: outside the retention window the cache
/// keeps one end state per this many tokenized rows (plus each budget-stop
/// resume point), so any row's start state is re-derivable by tokenizing at
/// most a stride forward from the checkpoint above it. Memory per fully-swept
/// document: `lines / stride` states instead of `lines`.
pub const HIGHLIGHT_CHECKPOINT_STRIDE: u32 = 256;

/// Retention slack, in rows, kept on EACH side of the window handed to
/// [`HighlightCache::set_window`] — scrolling within the slack costs nothing;
/// beyond it, evicted rows refill from the nearest checkpoint (budgeted).
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
/// owner of this formula: [`HighlightCache::set_window`] (what the cache
/// *retains*) and any parallel-highlight worker pool (what it speculatively
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

#[cfg(test)]
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
