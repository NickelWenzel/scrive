//! [`TokenTheme`], the one theme both highlight backends color with: syntect
//! reads its `Theme`, and tree-sitter captures resolve through its capture
//! rules.

use std::io::Cursor;
use std::str::FromStr;

use syntect::highlighting::{
    Color, FontStyle, Highlighter as SyntectHighlighter, ScopeSelectors, StyleModifier, Theme,
    ThemeItem, ThemeSet, ThemeSettings,
};
use syntect::parsing::Scope;

use super::vocabulary;
use super::{style_from, Rgba, SpanStyle};

/// Failed to parse a `.tmTheme`.
#[derive(Debug, thiserror::Error)]
#[error("invalid .tmTheme: {0}")]
pub struct ThemeError(String);

/// A parsed highlight theme. `Clone` is cheap (a small style table), so an
/// integrating widget can retain a theme and re-apply it across a document
/// reload or grammar swap.
///
/// Tree-sitter captures are styled by name, with a dotted name falling back to
/// its longest styled prefix: `function.method` takes `function`'s style
/// unless it has its own. The standard capture vocabulary, which also maps to
/// TextMate scopes for syntect, is `attribute`, `comment`, `constant`,
/// `constant.builtin`, `constructor`, `escape`, `function`,
/// `function.builtin`, `function.macro`, `function.method`, `keyword`,
/// `label`, `module`, `number`, `operator`, `property`, `punctuation`,
/// `punctuation.bracket`, `punctuation.delimiter`, `string`, `string.escape`,
/// `string.special`, `tag`, `type`, `type.builtin`, `variable`,
/// `variable.builtin` and `variable.parameter`.
#[derive(Clone)]
pub struct TokenTheme {
    theme: Theme,
    rules: Vec<Rule>,
    foreground: Option<Rgba>,
}

/// Builds a [`TokenTheme`] from capture styles in code; see
/// [`TokenTheme::builder`].
#[derive(Clone, Debug, Default)]
pub struct Builder {
    foreground: Option<Rgba>,
    rules: Vec<Rule>,
}

#[derive(Clone, Debug)]
struct Rule {
    capture: String,
    style: SpanStyle,
}

impl TokenTheme {
    /// Parse a `.tmTheme` (plist). Syntect highlighting uses it exactly as
    /// written; tree-sitter captures in the standard vocabulary take the
    /// style the theme gives their TextMate scopes, and a capture the theme
    /// leaves at its default foreground stays plain text.
    pub fn from_tm_theme(s: &str) -> Result<Self, ThemeError> {
        let theme = ThemeSet::load_from_reader(&mut Cursor::new(s))
            .map_err(|e| ThemeError(e.to_string()))?;
        let rules = derive_rules(&theme);
        let foreground = theme.settings.foreground.map(rgba);
        Ok(TokenTheme { theme, rules, foreground })
    }

    /// Start a theme built from capture styles, for a host that defines its
    /// colors in code instead of shipping a `.tmTheme`.
    #[must_use]
    pub fn builder() -> Builder {
        Builder::default()
    }

    /// The color of text no capture styles, if the theme sets one. A renderer
    /// may use it for plain text.
    #[must_use]
    pub fn foreground(&self) -> Option<Rgba> {
        self.foreground
    }

    /// The syntect theme the line-state backend highlights with.
    pub(super) fn syntect(&self) -> &Theme {
        &self.theme
    }

    /// The style for a tree-sitter capture name: its own rule, else the rule
    /// of its longest dotted prefix. `none` and names starting with `_` are
    /// never styled, following the tree-sitter highlight query convention.
    #[cfg_attr(
        not(any(test, feature = "tree-sitter")),
        expect(dead_code, reason = "the tree-sitter backend is the caller")
    )]
    pub(crate) fn resolve(&self, capture: &str) -> Option<SpanStyle> {
        if capture == "none" || capture.starts_with('_') {
            return None;
        }
        let mut name = capture;
        loop {
            if let Some(rule) = self.rules.iter().find(|r| r.capture == name) {
                return Some(rule.style);
            }
            name = &name[..name.rfind('.')?];
        }
    }
}

impl Builder {
    /// The color of text no capture styles. Syntect paints it on every
    /// unstyled run; tree-sitter leaves unstyled text to the renderer's own
    /// default.
    #[must_use]
    pub fn foreground(mut self, fg: Rgba) -> Self {
        self.foreground = Some(fg);
        self
    }

    /// Style the capture `name` (and every dotted child without a style of
    /// its own). Styling a name again replaces its style. A name outside the
    /// standard vocabulary listed on [`TokenTheme`] styles tree-sitter
    /// captures only, since it has no TextMate scopes for syntect to match.
    ///
    /// Under syntect a capture styles its vocabulary scopes, and where two
    /// captures share a scope (`escape` and `string.escape`) the more dotted
    /// one wins, whatever the call order. Across capture families the two
    /// backends can still differ: `keyword` colors syntect's
    /// `keyword.operator`, while tree-sitter's `@operator` stays plain until
    /// `operator` is styled. A theme from [`TokenTheme::from_tm_theme`]
    /// doesn't have this gap, since its capture rules are derived from the
    /// same scopes syntect matches.
    #[must_use]
    pub fn capture(mut self, name: impl Into<String>, style: SpanStyle) -> Self {
        let name = name.into();
        match self.rules.iter_mut().find(|r| r.capture == name) {
            Some(rule) => rule.style = style,
            None => self.rules.push(Rule { capture: name, style }),
        }
        self
    }

    /// Finish the theme.
    #[must_use]
    pub fn build(self) -> TokenTheme {
        // Syntect keeps the first of two equally specific selectors, so the
        // more dotted capture goes first.
        let mut by_depth: Vec<&Rule> = self.rules.iter().collect();
        by_depth.sort_by_key(|rule| std::cmp::Reverse(rule.capture.matches('.').count()));
        let scopes = by_depth
            .into_iter()
            .filter_map(|rule| {
                let scopes = vocabulary::scopes(&rule.capture)?;
                let selectors = ScopeSelectors::from_str(&scopes.join(", "))
                    .expect("vocabulary scopes are valid selectors");
                Some(ThemeItem { scope: selectors, style: style_modifier(rule.style) })
            })
            .collect();
        let theme = Theme {
            settings: ThemeSettings {
                foreground: self.foreground.map(color),
                ..ThemeSettings::default()
            },
            scopes,
            ..Theme::default()
        };
        TokenTheme { theme, rules: self.rules, foreground: self.foreground }
    }
}

/// One rule per vocabulary capture whose first representative scope with a
/// non-default style sets it.
fn derive_rules(theme: &Theme) -> Vec<Rule> {
    let syntect = SyntectHighlighter::new(theme);
    let plain = syntect.get_default();
    vocabulary::VOCABULARY
        .iter()
        .filter_map(|&(capture, scopes)| {
            let style = scopes.iter().find_map(|scope| {
                let scope = Scope::new(scope).expect("vocabulary scopes are valid scopes");
                let style = syntect.style_for_stack(&[scope]);
                let styled = style.foreground != plain.foreground || style.font_style != plain.font_style;
                styled.then_some(style)
            })?;
            Some(Rule { capture: capture.to_owned(), style: style_from(style) })
        })
        .collect()
}

/// A rule's full style, so a capture that is neither bold nor italic also
/// clears a font style an enclosing scope set, the way a capture style does.
fn style_modifier(style: SpanStyle) -> StyleModifier {
    let mut font_style = FontStyle::empty();
    font_style.set(FontStyle::BOLD, style.bold);
    font_style.set(FontStyle::ITALIC, style.italic);
    StyleModifier { foreground: Some(color(style.fg)), background: None, font_style: Some(font_style) }
}

fn color(c: Rgba) -> Color {
    Color { r: c.r, g: c.g, b: c.b, a: c.a }
}

fn rgba(c: Color) -> Rgba {
    Rgba { r: c.r, g: c.g, b: c.b, a: c.a }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Rgba = Rgba { r: 0xff, g: 0, b: 0, a: 0xff };
    const GREEN: Rgba = Rgba { r: 0, g: 0xff, b: 0, a: 0xff };

    fn plain(fg: Rgba) -> SpanStyle {
        SpanStyle { fg, bold: false, italic: false }
    }

    fn rgb(hex: u32) -> Rgba {
        Rgba { r: (hex >> 16) as u8, g: (hex >> 8) as u8, b: hex as u8, a: 0xff }
    }

    #[test]
    fn a_capture_falls_back_to_its_longest_styled_prefix() {
        let theme = TokenTheme::builder()
            .capture("function", plain(RED))
            .capture("function.method", plain(GREEN))
            .build();
        assert_eq!(theme.resolve("function"), Some(plain(RED)));
        assert_eq!(theme.resolve("function.builtin"), Some(plain(RED)));
        assert_eq!(theme.resolve("function.method"), Some(plain(GREEN)));
        assert_eq!(theme.resolve("function.method.call"), Some(plain(GREEN)));
        assert_eq!(theme.resolve("functional"), None);
        assert_eq!(theme.resolve("keyword"), None);
    }

    #[test]
    fn none_and_underscore_captures_are_never_styled() {
        let theme = TokenTheme::builder()
            .capture("none", plain(RED))
            .capture("_private", plain(RED))
            .build();
        assert_eq!(theme.resolve("none"), None);
        assert_eq!(theme.resolve("_private"), None);
        assert_eq!(theme.resolve("_private.child"), None);
    }

    #[test]
    fn styling_a_capture_again_replaces_its_style() {
        let theme =
            TokenTheme::builder().capture("keyword", plain(RED)).capture("keyword", plain(GREEN)).build();
        assert_eq!(theme.resolve("keyword"), Some(plain(GREEN)));
    }

    #[test]
    fn every_vocabulary_scope_parses() {
        for (capture, scopes) in vocabulary::VOCABULARY {
            ScopeSelectors::from_str(&scopes.join(", ")).unwrap_or_else(|e| panic!("{capture}: {e:?}"));
            for scope in *scopes {
                Scope::new(scope).unwrap_or_else(|e| panic!("{capture}: {e:?}"));
            }
        }
    }

    /// The content of scrive-iced's `assets/scrive-dark.tmTheme`.
    const SCRIVE_DARK: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>name</key><string>Scrive Dark</string>
  <key>settings</key>
  <array>
    <dict><key>settings</key><dict>
      <key>background</key><string>#1C1E24</string>
      <key>foreground</key><string>#DFE1E6</string>
    </dict></dict>
    <dict><key>scope</key><string>comment</string>
      <key>settings</key><dict><key>foreground</key><string>#6A6F7A</string><key>fontStyle</key><string>italic</string></dict></dict>
    <dict><key>scope</key><string>keyword, keyword.control, keyword.operator, storage.modifier</string>
      <key>settings</key><dict><key>foreground</key><string>#EC6A88</string></dict></dict>
    <dict><key>scope</key><string>storage.type, support.type, entity.name.type</string>
      <key>settings</key><dict><key>foreground</key><string>#4FB6C7</string></dict></dict>
    <dict><key>scope</key><string>entity.name.function, support.function</string>
      <key>settings</key><dict><key>foreground</key><string>#E0B658</string></dict></dict>
    <dict><key>scope</key><string>string, string.quoted</string>
      <key>settings</key><dict><key>foreground</key><string>#A3C76D</string></dict></dict>
    <dict><key>scope</key><string>constant.numeric, constant.language, constant.character</string>
      <key>settings</key><dict><key>foreground</key><string>#B08CE0</string></dict></dict>
    <dict><key>scope</key><string>meta.attribute</string>
      <key>settings</key><dict><key>foreground</key><string>#C99668</string></dict></dict>
    <dict><key>scope</key><string>punctuation</string>
      <key>settings</key><dict><key>foreground</key><string>#9AA0AB</string></dict></dict>
  </array>
</dict>
</plist>"#;

    #[test]
    fn tm_theme_derives_a_rule_for_every_styled_vocabulary_capture() {
        let expected: &[(&str, Option<u32>)] = &[
            ("attribute", Some(0xC99668)),
            ("comment", Some(0x6A6F7A)),
            ("constant", None),
            ("constant.builtin", Some(0xB08CE0)),
            ("constructor", Some(0xE0B658)),
            ("escape", Some(0xB08CE0)),
            ("function", Some(0xE0B658)),
            ("function.builtin", Some(0xE0B658)),
            ("function.macro", Some(0xE0B658)),
            ("function.method", Some(0xE0B658)),
            ("keyword", Some(0xEC6A88)),
            ("label", None),
            ("module", None),
            ("number", Some(0xB08CE0)),
            ("operator", Some(0xEC6A88)),
            ("property", None),
            ("punctuation", Some(0x9AA0AB)),
            ("punctuation.bracket", Some(0x9AA0AB)),
            ("punctuation.delimiter", Some(0x9AA0AB)),
            ("string", Some(0xA3C76D)),
            ("string.escape", Some(0xB08CE0)),
            ("string.special", Some(0xA3C76D)),
            ("tag", None),
            ("type", Some(0x4FB6C7)),
            ("type.builtin", Some(0x4FB6C7)),
            ("variable", None),
            ("variable.builtin", None),
            ("variable.parameter", None),
        ];
        let names: Vec<&str> = expected.iter().map(|(name, _)| *name).collect();
        let vocabulary: Vec<&str> = vocabulary::VOCABULARY.iter().map(|(name, _)| *name).collect();
        assert_eq!(names, vocabulary, "the table covers the vocabulary in order");

        let theme = TokenTheme::from_tm_theme(SCRIVE_DARK).unwrap();
        let derived: Vec<(&str, Option<Rgba>)> = vocabulary
            .iter()
            .map(|&name| (name, theme.rules.iter().find(|r| r.capture == name).map(|r| r.style.fg)))
            .collect();
        let expected: Vec<(&str, Option<Rgba>)> =
            expected.iter().map(|&(name, hex)| (name, hex.map(rgb))).collect();
        assert_eq!(derived, expected);
        assert!(theme.resolve("comment").unwrap().italic, "the font style carries over");
        assert_eq!(theme.foreground(), Some(rgb(0xDFE1E6)));
    }
}
