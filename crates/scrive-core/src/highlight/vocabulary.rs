//! The standard tree-sitter capture names and the TextMate scopes that stand
//! for each, the bridge that lets one [`TokenTheme`](super::TokenTheme) color
//! both backends.

/// Each standard capture with its representative TextMate scopes, most typical
/// first. A dotted child's scopes are either disjoint from its parent's or
/// more specific (`function.builtin` → `support.function.builtin` under
/// `function`'s `support.function`), never equal, so a synthesized syntect
/// theme ranks a parent and its children the way the longest-prefix capture
/// lookup does. Across families scopes can nest (`operator`'s
/// `keyword.operator` sits under `keyword`), and there the backends differ.
pub(super) const VOCABULARY: &[(&str, &[&str])] = &[
    ("attribute", &["meta.attribute", "entity.other.attribute-name"]),
    ("comment", &["comment"]),
    ("constant", &["constant"]),
    ("constant.builtin", &["constant.language"]),
    ("constructor", &["entity.name.function.constructor"]),
    ("escape", &["constant.character.escape"]),
    ("function", &["entity.name.function", "support.function"]),
    ("function.builtin", &["support.function.builtin"]),
    ("function.macro", &["entity.name.function.macro"]),
    ("function.method", &["entity.name.function.method"]),
    ("keyword", &["keyword", "storage.modifier"]),
    ("label", &["entity.name.label"]),
    ("module", &["entity.name.namespace"]),
    ("number", &["constant.numeric"]),
    ("operator", &["keyword.operator"]),
    ("property", &["variable.other.member"]),
    ("punctuation", &["punctuation"]),
    ("punctuation.bracket", &["punctuation.section"]),
    ("punctuation.delimiter", &["punctuation.separator", "punctuation.terminator"]),
    ("string", &["string"]),
    ("string.escape", &["constant.character.escape"]),
    ("string.special", &["string.regexp", "string.other"]),
    ("tag", &["entity.name.tag"]),
    ("type", &["entity.name.type", "storage.type", "support.type"]),
    ("type.builtin", &["storage.type.primitive", "support.type.primitive"]),
    ("variable", &["variable"]),
    ("variable.builtin", &["variable.language"]),
    ("variable.parameter", &["variable.parameter"]),
];

/// The representative scopes of `capture`, if it is in the vocabulary.
#[cfg(feature = "syntect")]
pub(super) fn scopes(capture: &str) -> Option<&'static [&'static str]> {
    VOCABULARY.iter().find(|(name, _)| *name == capture).map(|(_, scopes)| *scopes)
}
