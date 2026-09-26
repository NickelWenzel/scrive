//! Server markdown lowered to what scrive renders: plain text for completion and signature
//! documentation, and the hover card's markdown subset.

use lsp_types::{Documentation, MarkupContent, MarkupKind};
use scrive_core::intel::hover::escape_markdown;

/// One source line after block-level lowering.
enum Line<'a> {
    /// A line inside a fenced code block, verbatim.
    Code(&'a str),
    /// Any other kept line.
    Text(&'a str),
}

/// Markdown as plain text: code lines verbatim; in other lines `**` and backticks removed, links
/// and images reduced to their text, headings' `#`s dropped and backslash escapes resolved.
/// Single `*` and `_` stay, since they are more often literal (`a * b`, `snake_case`, `__init__`)
/// than emphasis.
pub(crate) fn to_plain(markdown: &str) -> String {
    lines(markdown)
        .map(|line| match line {
            Line::Code(code) => code.to_owned(),
            Line::Text(text) => plain_inline(heading(text).unwrap_or(text)),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Markdown in the hover card's subset (the grammar on [`scrive_core::HoverInfo::markdown`]):
/// fenced blocks become one code span per line, thematic breaks are dropped, links and images
/// become their text and headings become bold. Inline code and `**bold**` pass through. Backslash
/// escapes of other characters resolve to the character; the card's own (`\*`, `` \` ``, `\\`)
/// are kept.
pub(crate) fn to_hover(markdown: &str) -> String {
    lines(markdown)
        .map(|line| match line {
            Line::Code(code) => code_line(code),
            Line::Text(text) => match heading(text) {
                Some(title) => format!("**{}**", hover_inline(title)),
                None => hover_inline(text),
            },
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `code` in the hover card's subset, one code span per line.
pub(crate) fn code_lines(code: &str) -> String {
    code.lines().map(code_line).collect::<Vec<_>>().join("\n")
}

/// Completion documentation as plain text. A bare string is plain text per the spec.
pub(crate) fn documentation(documentation: &Documentation) -> String {
    match documentation {
        Documentation::String(text)
        | Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::PlainText,
            value: text,
        }) => text.clone(),
        Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }) => to_plain(value),
    }
}

/// The kept lines of `markdown`. Fence lines (```` ``` ```` or `~~~`, any info string) are
/// dropped and toggle code; an unterminated fence runs to the end. Thematic breaks outside code
/// are dropped.
fn lines(markdown: &str) -> impl Iterator<Item = Line<'_>> {
    let mut fence: Option<char> = None;
    markdown.lines().filter_map(move |line| {
        let trimmed = line.trim_start();
        let opener = if trimmed.starts_with("```") {
            Some('`')
        } else if trimmed.starts_with("~~~") {
            Some('~')
        } else {
            None
        };
        match (fence, opener) {
            (None, Some(c)) => {
                fence = Some(c);
                None
            }
            (Some(open), Some(c)) if open == c => {
                fence = None;
                None
            }
            (Some(_), _) => Some(Line::Code(line)),
            (None, None) if is_rule(trimmed) => None,
            (None, None) => Some(Line::Text(line)),
        }
    })
}

/// `---`, `***` or `___`: three or more of one mark, spaces allowed between them.
fn is_rule(line: &str) -> bool {
    let mut marks = line.chars().filter(|c| !c.is_whitespace());
    let Some(first) = marks.next() else {
        return false;
    };
    matches!(first, '-' | '*' | '_') && {
        let rest: Vec<char> = marks.collect();
        rest.len() >= 2 && rest.iter().all(|&c| c == first)
    }
}

/// The title of an ATX heading (`# Title` → `Title`: one to six hashes, then a space), or `None`
/// when `line` is not one.
fn heading(line: &str) -> Option<&str> {
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    line[hashes..]
        .strip_prefix(' ')
        .filter(|_| (1..=6).contains(&hashes))
}

/// One code line as a code span, an empty line as an empty line.
fn code_line(line: &str) -> String {
    if line.is_empty() {
        String::new()
    } else {
        format!("`{}`", escape_markdown(line))
    }
}

/// Inline lowering for the hover card.
fn hover_inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if let Some((label, after)) = link(rest) {
            out.push_str(&hover_inline(label));
            rest = after;
            continue;
        }
        match c {
            '`' => match rest[1..].find('`') {
                // A code span's content is literal in CommonMark, backslashes included, so it is
                // escaped whole for the card.
                Some(end) => {
                    out.push('`');
                    out.push_str(&escape_markdown(&rest[1..1 + end]));
                    out.push('`');
                    rest = &rest[end + 2..];
                }
                // An unclosed backtick is literal; left bare it would code-format the rest.
                None => {
                    out.push_str("\\`");
                    rest = &rest[1..];
                }
            },
            '\\' => match rest[1..].chars().next() {
                Some(escaped @ ('*' | '`' | '\\')) => {
                    out.push('\\');
                    out.push(escaped);
                    rest = &rest[2..];
                }
                Some(escaped) if escaped.is_ascii_punctuation() => {
                    out.push(escaped);
                    rest = &rest[2..];
                }
                // A backslash that escapes nothing is literal, and must not escape what follows.
                _ => {
                    out.push_str("\\\\");
                    rest = &rest[1..];
                }
            },
            c => {
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    out
}

/// Inline lowering for plain text.
fn plain_inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if let Some((label, after)) = link(rest) {
            out.push_str(&plain_inline(label));
            rest = after;
            continue;
        }
        match c {
            '\\' => match rest[1..].chars().next() {
                Some(escaped) if escaped.is_ascii_punctuation() => {
                    out.push(escaped);
                    rest = &rest[2..];
                }
                _ => {
                    out.push('\\');
                    rest = &rest[1..];
                }
            },
            '`' => rest = &rest[1..],
            '*' if rest.starts_with("**") => rest = &rest[2..],
            c => {
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    out
}

/// `[label](target)` or `![label](target)` at the start of `text`: the label and the text after
/// the closing `)`. Brackets do not nest.
fn link(text: &str) -> Option<(&str, &str)> {
    let text = text.strip_prefix('!').unwrap_or(text);
    let text = text.strip_prefix('[')?;
    let close = text.find(']')?;
    let after = text[close + 1..].strip_prefix('(')?;
    let end = after.find(')')?;
    Some((&text[..close], &after[end + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_plain(rows: &[(&str, &str)]) {
        for (input, expected) in rows {
            assert_eq!(to_plain(input), *expected, "lowering {input:?}");
        }
    }

    /// Fence lines go; the code between them stays verbatim.
    #[test]
    fn fenced_code_keeps_its_lines() {
        assert_plain(&[
            ("```rust\nlet x = 1;\n```\nafter", "let x = 1;\nafter"),
            ("~~~\n**kept**\n~~~", "**kept**"),
            ("```\nunterminated", "unterminated"),
        ]);
    }

    /// Bold and code markers are syntax, not text.
    #[test]
    fn bold_and_code_markers_are_removed() {
        assert_plain(&[("**Note**: use `x`", "Note: use x")]);
    }

    /// Links and images keep only their label.
    #[test]
    fn links_and_images_become_their_text() {
        assert_plain(&[
            ("see [docs](http://a) ![logo](l.png)", "see docs logo"),
            ("wow! [a]", "wow! [a]"),
        ]);
    }

    /// Horizontal rules carry nothing in plain text.
    #[test]
    fn thematic_breaks_are_dropped() {
        assert_plain(&[("a\n---\nb", "a\nb"), ("a\n* * *\nb", "a\nb")]);
    }

    /// An escaped punctuation character is that character.
    #[test]
    fn backslash_escapes_become_literals() {
        assert_plain(&[("\\*lit\\* \\_x", "*lit* _x"), ("a\\b", "a\\b")]);
    }

    /// ATX heading markers are dropped.
    #[test]
    fn headings_lose_their_hashes() {
        assert_plain(&[("## Title\nbody", "Title\nbody"), ("#nospace", "#nospace")]);
    }

    /// Single underscores are identifiers far more often than emphasis.
    #[test]
    fn snake_case_underscores_survive() {
        assert_plain(&[("call __init__ or a_b", "call __init__ or a_b")]);
    }

    /// Only markdown documentation is lowered; strings and plaintext markup are already plain.
    #[test]
    fn documentation_kinds_lower_to_plain_text() {
        let markup = |kind| {
            Documentation::MarkupContent(MarkupContent {
                kind,
                value: "**a**".to_owned(),
            })
        };
        for (doc, expected) in [
            (Documentation::String("**a**".to_owned()), "**a**"),
            (markup(MarkupKind::PlainText), "**a**"),
            (markup(MarkupKind::Markdown), "a"),
        ] {
            assert_eq!(documentation(&doc), expected, "{doc:?}");
        }
    }

    fn assert_hover(rows: &[(&str, &str)]) {
        for (input, expected) in rows {
            assert_eq!(to_hover(input), *expected, "lowering {input:?}");
        }
    }

    /// Fence lines go; each code line becomes a code span.
    #[test]
    fn fences_become_code_lines() {
        assert_hover(&[("```rust\nfn f() -> u8\n```", "`fn f() -> u8`")]);
    }

    /// Tilde fences count, and an unterminated fence runs to the end.
    #[test]
    fn tilde_and_unterminated_fences_become_code_lines() {
        assert_hover(&[("~~~\nx\n~~~", "`x`"), ("```\ny", "`y`")]);
    }

    /// Markup characters in code are escaped, so the span shows them verbatim.
    #[test]
    fn code_lines_escape_their_text() {
        let expected = format!("`{}`", escape_markdown("a`b*c\\d"));
        assert_hover(&[("```\na`b*c\\d\n```", expected.as_str())]);
    }

    /// Horizontal rules have no place in the card.
    #[test]
    fn thematic_breaks_are_stripped_from_hover() {
        assert_hover(&[("a\n---\nb", "a\nb")]);
    }

    /// Links and images keep only their label.
    #[test]
    fn links_become_their_text_in_hover() {
        assert_hover(&[("see [Vec](https://doc) and ![i](x.png)", "see Vec and i")]);
    }

    /// Bold and inline code are the card's own markup.
    #[test]
    fn bold_and_inline_code_pass_through() {
        assert_hover(&[
            ("**bold** and `code`", "**bold** and `code`"),
            ("`a\\*b`", "`a\\\\\\*b`"),
        ]);
    }

    /// An unclosed backtick shows as a backtick instead of code-formatting the rest of the line.
    #[test]
    fn unclosed_backtick_is_escaped() {
        assert_hover(&[("a ` b", "a \\` b")]);
    }

    /// Escapes the card does not know resolve to their character; its own stay escaped.
    #[test]
    fn foreign_escapes_resolve_and_ours_are_kept() {
        assert_hover(&[("a\\_b \\*c\\*", "a_b \\*c\\*")]);
    }

    /// A heading is bold, the only emphasis the card renders.
    #[test]
    fn headings_become_bold() {
        assert_hover(&[("# Title", "**Title**"), ("#nospace", "#nospace")]);
    }

    /// A backslash before a letter is literal in markdown, and stays literal in the card.
    #[test]
    fn lone_backslash_is_escaped() {
        assert_hover(&[("C:\\path", "C:\\\\path"), ("end\\", "end\\\\")]);
    }
}
