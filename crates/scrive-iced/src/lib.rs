//! `scrive-iced` — the iced 0.14 integration for the scrive code editor.
//!
//! Turns [`scrive_core`] into an on-screen widget: a direct
//! `iced::advanced::Widget` (deliberately *not* a `canvas::Program`, because
//! only the low-level widget API exposes the `operate()` hook needed to join
//! iced's focus/operation protocol), with a gutter, N-caret selections, syntect
//! or tree-sitter (the `tree-sitter` feature) highlighting, diagnostic
//! squiggles, a completion popup, and hover.
//!
//! # Two tiers
//!
//! - [`CodeEditor`] — **start here.** The batteries-included tier: it owns a
//!   [`scrive_core::Document`] and runs the highlighting, find, focus, and
//!   language-intelligence plumbing internally, so integrating is three wires
//!   ([`update`](CodeEditor::update), [`view`](CodeEditor::view),
//!   [`subscription`](CodeEditor::subscription)) plus registering
//!   [`required_fonts`] at startup. Highlighting is coloured at load with no
//!   scroll needed; the find bar, selection, undo, and folding are on by default.
//!   See `examples/minimal.rs`.
//! - [`Editor`] — the low-level *controlled* widget: it renders a `Document` and
//!   emits semantic [`Action`]s the application applies by hand. Full control,
//!   all the plumbing on you. See `examples/scratch.rs`.
//!
//! The minimal integration:
//!
//! ```no_run
//! use iced::time::Instant;
//! use iced::{Element, Subscription, Task};
//! use scrive_iced::{CodeEditor, Event};
//!
//! struct App { editor: CodeEditor }
//!
//! #[derive(Debug, Clone)]
//! enum Message { Editor(Event) }
//!
//! impl App {
//!     fn new() -> Self { Self { editor: CodeEditor::new("fn main() {}\n") } }
//!     // `now` comes from iced: run the app with `iced::application::timed`.
//!     fn update(&mut self, m: Message, now: Instant) -> Task<Message> {
//!         match m { Message::Editor(e) => self.editor.update(e, now).map(Message::Editor) }
//!     }
//!     fn view(&self) -> Element<'_, Message> { self.editor.view().map(Message::Editor) }
//!     fn subscription(&self) -> Subscription<Message> {
//!         self.editor.subscription().map(Message::Editor)
//!     }
//! }
//! ```
//!
//! # Highlighting
//!
//! [`CodeEditor::language`] takes either backend's grammar, and the cargo
//! features of the same names forward to `scrive-core`:
//!
//! | feature | default | grammar | adds |
//! |---------|---------|---------|------|
//! | `syntect` | on | `scrive_core::SyntaxDef` (`.sublime-syntax`) | documents of 2 MiB or more tokenize on worker threads, natively |
//! | `tree-sitter` | off | `scrive_core::TreeSitterDef` (grammar crate + highlights query) | the parse runs on the UI thread, a budgeted slice per frame |
//! | `lsp` | off | | `scrive_iced::lsp`, the language-server bridge |
//!
//! ```no_run
//! # #[cfg(feature = "syntect")]
//! # fn syntect(sublime_syntax: &str) -> Result<(), Box<dyn std::error::Error>> {
//! use scrive_core::SyntaxDef;
//! use scrive_iced::CodeEditor;
//!
//! let editor = CodeEditor::new("fn main() {}\n").language(SyntaxDef::from_sublime_syntax(sublime_syntax)?);
//! # Ok(())
//! # }
//! ```
//!
//! ```no_run
//! # #[cfg(feature = "tree-sitter")]
//! # fn tree_sitter() -> Result<(), Box<dyn std::error::Error>> {
//! use scrive_core::TreeSitterDef;
//! use scrive_iced::CodeEditor;
//!
//! let grammar = TreeSitterDef::new(tree_sitter_rust::LANGUAGE, tree_sitter_rust::HIGHLIGHTS_QUERY)?;
//! let editor = CodeEditor::new("fn main() {}\n").language(grammar);
//! # Ok(())
//! # }
//! ```
//!
//! [`scrive_dark_theme`] is the default theme in every build. A host's own
//! [`TokenTheme`](scrive_core::TokenTheme), from a `.tmTheme` or
//! `TokenTheme::builder()`, goes to [`CodeEditor::theme`] and colors either
//! backend.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod clipboard;
pub mod code_editor;
pub mod editor;
mod geo;
#[cfg(feature = "syntect")]
mod highlight_pool;
pub mod metrics;
pub mod popup;

pub use code_editor::{CodeEditor, Event};
pub use editor::{default_autoscroll_margin, Action, Editor, Wake, SCROLLBAR_WIDTH};
pub use metrics::Metrics;

/// The Language Server Protocol bridge, re-exported so a host names one crate:
/// `scrive_iced::lsp::Client`, `scrive_iced::lsp::client::Events`, `scrive_iced::lsp::update`.
/// Needs the `lsp` feature.
#[cfg(feature = "lsp")]
pub use scrive_lsp as lsp;

/// The bundled [Codicon](https://github.com/microsoft/vscode-codicons) icon font
/// (v0.0.45) — VS Code's own UI glyph set. The host application **must** load
/// these bytes into iced's font system at startup (e.g.
/// `iced::application(..).fonts([scrive_iced::CODICON_FONT])`); after that the
/// widget's fold-gutter chevrons and any app chrome can draw glyphs in the
/// [`CODICON`] font. Icons © Microsoft, CC BY 4.0 (see `assets/CODICON-LICENSE.md`).
pub const CODICON_FONT: &[u8] = include_bytes!("../assets/codicon.ttf");

/// The [`iced::Font`] handle for the bundled [`CODICON_FONT`] (family `"codicon"`).
pub const CODICON: iced::Font = iced::Font::new("codicon");

/// Fira Code, the editor's text font on wasm32: a browser gives iced no system
/// fonts, so [`Font::MONOSPACE`](iced::Font::MONOSPACE) would resolve to
/// nothing there. Its programming ligatures (`->`, `=>`, `!=`, …) render within
/// a highlight token. [`required_fonts`] includes it on wasm32 only. SIL Open
/// Font License 1.1 (see `assets/FIRA-CODE-LICENSE.md`).
pub const FIRA_CODE_FONT: &[u8] = include_bytes!("../assets/fira-code.ttf");

/// The editor's default text font: [`iced::Font::MONOSPACE`] natively, the
/// bundled [`FIRA_CODE_FONT`] on wasm32.
pub const DEFAULT_FONT: iced::Font =
    if cfg!(target_arch = "wasm32") { iced::Font::new("Fira Code") } else { iced::Font::MONOSPACE };

/// Every font the widget needs registered in iced's font system at startup —
/// register them all and the fold-gutter chevrons and find-bar icons render;
/// omit one and its glyphs fall back to per-machine tofu. One owner so an
/// integrator can load the whole set instead of enumerating it by hand
/// ([`CODICON_FONT`], plus the [`FIRA_CODE_FONT`] text font on wasm32):
/// `app.fonts(scrive_iced::required_fonts().iter().copied())`.
#[must_use]
pub fn required_fonts() -> &'static [&'static [u8]] {
    #[cfg(target_arch = "wasm32")]
    return &[CODICON_FONT, FIRA_CODE_FONT];
    #[cfg(not(target_arch = "wasm32"))]
    &[CODICON_FONT]
}

/// The bundled **Scrive Dark** syntax theme — an original, MIT-licensed dark
/// theme (the one that shipped with 0.1.0). It is the sensible default so a host
/// gets colored text from a grammar alone, without supplying a `.tmTheme`: the
/// batteries-included editor tier applies it unless the host overrides it with
/// another [`TokenTheme`](scrive_core::TokenTheme).
///
/// The theme is compiled in, so parsing it cannot fail at runtime — a malformed
/// asset is a packaging bug the crate's own tests catch, not a caller error.
/// That is why this returns the theme directly rather than a `Result`. Without
/// the `syntect` feature the same colors are built in code, as capture styles.
#[must_use]
pub fn scrive_dark_theme() -> scrive_core::TokenTheme {
    #[cfg(feature = "syntect")]
    return scrive_core::TokenTheme::from_tm_theme(include_str!("../assets/scrive-dark.tmTheme"))
        .expect("bundled Scrive Dark theme parses");
    #[cfg(not(feature = "syntect"))]
    scrive_dark_captures()
}

/// Scrive Dark as capture styles, for the build that can't parse the
/// `.tmTheme`. A test keeps it equal to the asset.
#[cfg(any(test, not(feature = "syntect")))]
fn scrive_dark_captures() -> scrive_core::TokenTheme {
    use scrive_core::{Rgba, SpanStyle};
    const fn style(hex: u32, italic: bool) -> SpanStyle {
        let fg = Rgba { r: (hex >> 16) as u8, g: (hex >> 8) as u8, b: hex as u8, a: 0xff };
        SpanStyle { fg, bold: false, italic }
    }
    const COMMENT: SpanStyle = style(0x6A6F7A, true);
    const KEYWORD: SpanStyle = style(0xEC6A88, false);
    const TYPE: SpanStyle = style(0x4FB6C7, false);
    const FUNCTION: SpanStyle = style(0xE0B658, false);
    const STRING: SpanStyle = style(0xA3C76D, false);
    const CONSTANT: SpanStyle = style(0xB08CE0, false);
    const ATTRIBUTE: SpanStyle = style(0xC99668, false);
    const PUNCTUATION: SpanStyle = style(0x9AA0AB, false);
    // Every styled vocabulary capture, spelled out rather than left to prefix
    // fallback, so the comparison with the asset covers each one.
    const CAPTURES: &[(&str, SpanStyle)] = &[
        ("attribute", ATTRIBUTE),
        ("comment", COMMENT),
        ("constant.builtin", CONSTANT),
        ("constructor", FUNCTION),
        ("escape", CONSTANT),
        ("function", FUNCTION),
        ("function.builtin", FUNCTION),
        ("function.macro", FUNCTION),
        ("function.method", FUNCTION),
        ("keyword", KEYWORD),
        ("number", CONSTANT),
        ("operator", KEYWORD),
        ("punctuation", PUNCTUATION),
        ("punctuation.bracket", PUNCTUATION),
        ("punctuation.delimiter", PUNCTUATION),
        ("string", STRING),
        ("string.escape", CONSTANT),
        ("string.special", STRING),
        ("type", TYPE),
        ("type.builtin", TYPE),
    ];
    CAPTURES
        .iter()
        .fold(scrive_core::TokenTheme::builder().foreground(style(0xDFE1E6, false).fg), |b, &(capture, span)| {
            b.capture(capture, span)
        })
        .build()
}

/// Codicon glyph codepoints scrive draws. Names and values are from the codicon
/// `mapping.json` (verified against v0.0.45); the private-use-area codepoints are
/// only meaningful rendered in the [`CODICON`] font.
pub mod icon {
    /// `chevron-right` (U+EAB6) — the collapsed-fold gutter indicator, and the
    /// find bar's collapsed replace-row toggle.
    pub const CHEVRON_RIGHT: char = '\u{eab6}';
    /// `chevron-down` (U+EAB4) — the expanded-fold gutter indicator, and the
    /// find bar's expanded replace-row toggle.
    pub const CHEVRON_DOWN: char = '\u{eab4}';
    /// `arrow-up` (U+EAA1) — find "previous match".
    pub const ARROW_UP: char = '\u{eaa1}';
    /// `arrow-down` (U+EA9A) — find "next match".
    pub const ARROW_DOWN: char = '\u{ea9a}';
    /// `close` (U+EA76) — find "close".
    pub const CLOSE: char = '\u{ea76}';
    /// `replace` (U+EB3D) — find "replace this match".
    pub const REPLACE: char = '\u{eb3d}';
    /// `replace-all` (U+EB3C) — find "replace every match".
    pub const REPLACE_ALL: char = '\u{eb3c}';
    /// `case-sensitive` (U+EAB1) — the find bar's `Aa` option toggle.
    pub const CASE_SENSITIVE: char = '\u{eab1}';
    /// `preserve-case` (U+EB2E) — the replace bar's `AB` option toggle.
    pub const PRESERVE_CASE: char = '\u{eb2e}';
    /// `whole-word` (U+EB7E) — the find bar's `ab|` option toggle.
    pub const WHOLE_WORD: char = '\u{eb7e}';
    /// `regex` (U+EB38) — the find bar's `.*` option toggle.
    pub const REGEX: char = '\u{eb38}';
    /// `list-selection` (U+EB85) — the find bar's "find in selection" toggle.
    /// The codicon set names this glyph `list-selection`; `selection` is an
    /// alias for it, and is what VS Code calls the same button.
    pub const SELECTION: char = '\u{eb85}';
}

#[cfg(test)]
mod tests {
    /// The compiled-in Scrive Dark theme must parse — the `expect` in
    /// [`super::scrive_dark_theme`] would otherwise panic in every host that
    /// takes the default. This is the packaging guard the doc comment promises.
    #[test]
    fn bundled_scrive_dark_theme_parses() {
        let _ = super::scrive_dark_theme();
    }

    #[cfg(feature = "syntect")]
    #[test]
    fn scrive_dark_captures_match_the_tm_theme() {
        let asset = super::scrive_dark_theme();
        let built = super::scrive_dark_captures();
        for capture in scrive_core::TokenTheme::vocabulary() {
            assert_eq!(built.resolve(capture), asset.resolve(capture), "{capture}");
        }
        assert_eq!(built.foreground(), asset.foreground());
    }
}
