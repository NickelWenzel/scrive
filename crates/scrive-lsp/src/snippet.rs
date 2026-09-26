//! LSP snippets lowered to the subset scrive's snippet engine accepts: variables become their
//! default (or nothing), a repeated index becomes plain text after its first occurrence, nesting
//! is flattened into the outer default, choices become their first option, and transforms are
//! dropped.

use std::collections::HashMap;

use scrive_core::{InsertText, Snippet};

/// Lowers an LSP snippet body. A body that is not a well-formed LSP snippet is inserted raw; one
/// whose lowering scrive still rejects (an index past `u16`) is inserted as its plain text.
pub(crate) fn lower(body: &str) -> InsertText {
    let mut lowering = Lowering {
        chars: body.chars().collect(),
        at: 0,
        seen: HashMap::new(),
        snippet: String::new(),
        plain: String::new(),
    };
    if lowering.top().is_err() {
        return InsertText::Plain(body.to_owned());
    }
    match Snippet::parse(&lowering.snippet) {
        Ok(_) => InsertText::Snippet(lowering.snippet),
        Err(_) => InsertText::Plain(lowering.plain),
    }
}

/// The body is not a well-formed LSP snippet.
struct Malformed;

/// One parsed element.
enum Element {
    Text(String),
    Tabstop(u32),
    /// A placeholder or a choice: its index and flattened default.
    Placeholder(u32, String),
    /// A variable's default text, empty when it has none.
    Variable(String),
}

/// Where an element sits, which decides whether an unescaped `}` is text.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Context {
    Top,
    /// Inside `${…:`, where an unescaped `}` ends the default.
    Nested,
}

struct Lowering {
    chars: Vec<char>,
    at: usize,
    /// The first occurrence's text per index; later occurrences insert it as plain text.
    seen: HashMap<u32, String>,
    /// The body in scrive's snippet syntax.
    snippet: String,
    /// The body with every stop replaced by its text.
    plain: String,
}

impl Lowering {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.at + ahead).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.at += 1;
        Some(c)
    }

    fn top(&mut self) -> Result<(), Malformed> {
        while self.peek().is_some() {
            let element = self.element(Context::Top)?;
            self.emit(element);
        }
        Ok(())
    }

    /// Writes a top-level element to both outputs.
    fn emit(&mut self, element: Element) {
        match element {
            Element::Text(text) | Element::Variable(text) => self.literal(&text),
            Element::Tabstop(index) => match self.seen.get(&index).cloned() {
                Some(mirror) => self.literal(&mirror),
                None => {
                    self.seen.insert(index, String::new());
                    self.snippet.push_str(&format!("${index}"));
                }
            },
            Element::Placeholder(index, text) => match self.seen.get(&index).cloned() {
                Some(mirror) => self.literal(&mirror),
                None => {
                    self.snippet.push_str(&format!("${{{index}:"));
                    escape_into(&mut self.snippet, &text);
                    self.snippet.push('}');
                    self.plain.push_str(&text);
                    self.seen.insert(index, text);
                }
            },
        }
    }

    fn literal(&mut self, text: &str) {
        escape_into(&mut self.snippet, text);
        self.plain.push_str(text);
    }

    /// The flattened text of a nested `any*`, consuming its closing `}`.
    fn nested(&mut self) -> Result<String, Malformed> {
        let mut text = String::new();
        loop {
            match self.peek() {
                None => return Err(Malformed),
                Some('}') => {
                    self.at += 1;
                    return Ok(text);
                }
                Some(_) => {
                    let element = self.element(Context::Nested)?;
                    text.push_str(&self.flatten(element));
                }
            }
        }
    }

    /// An element's text inside another default. Its index is not registered: a flattened stop
    /// is no longer a stop.
    fn flatten(&self, element: Element) -> String {
        match element {
            Element::Text(text) | Element::Variable(text) | Element::Placeholder(_, text) => text,
            Element::Tabstop(index) => self.seen.get(&index).cloned().unwrap_or_default(),
        }
    }

    fn element(&mut self, context: Context) -> Result<Element, Malformed> {
        match (self.peek(), self.peek_at(1)) {
            (Some('$'), Some(c)) if c.is_ascii_digit() => {
                self.at += 1;
                Ok(Element::Tabstop(self.int()?))
            }
            (Some('$'), Some('{')) => {
                self.at += 2;
                self.braced()
            }
            (Some('$'), Some(c)) if is_name_start(c) => {
                self.at += 1;
                self.name();
                Ok(Element::Variable(String::new()))
            }
            _ => Ok(Element::Text(self.text(context))),
        }
    }

    /// Literal text up to the next element or, nested, the closing `}`. It always consumes at
    /// least one character, because `element` calls it only where no element starts.
    fn text(&mut self, context: Context) -> String {
        let mut text = String::new();
        while let Some(c) = self.peek() {
            match c {
                '\\' if matches!(self.peek_at(1), Some('$' | '}' | '\\')) => {
                    text.push(self.chars[self.at + 1]);
                    self.at += 2;
                }
                '}' if context == Context::Nested => break,
                '$' if !text.is_empty() && self.starts_element() => break,
                c => {
                    text.push(c);
                    self.at += 1;
                }
            }
        }
        text
    }

    fn starts_element(&self) -> bool {
        matches!(self.peek_at(1), Some(c) if c == '{' || c.is_ascii_digit() || is_name_start(c))
    }

    /// After `${`.
    fn braced(&mut self) -> Result<Element, Malformed> {
        if self.peek().is_some_and(|c| c.is_ascii_digit()) {
            let index = self.int()?;
            match self.bump() {
                Some('}') => Ok(Element::Tabstop(index)),
                Some(':') => Ok(Element::Placeholder(index, self.nested()?)),
                Some('|') => Ok(Element::Placeholder(index, self.choice()?)),
                Some('/') => self.transform().map(|()| Element::Tabstop(index)),
                _ => Err(Malformed),
            }
        } else if self.peek().is_some_and(is_name_start) {
            self.name();
            match self.bump() {
                Some('}') => Ok(Element::Variable(String::new())),
                Some(':') => Ok(Element::Variable(self.nested()?)),
                Some('/') => self.transform().map(|()| Element::Variable(String::new())),
                _ => Err(Malformed),
            }
        } else {
            Err(Malformed)
        }
    }

    /// The first option of `a,b|}` after `${N|`.
    fn choice(&mut self) -> Result<String, Malformed> {
        let mut first = String::new();
        let mut in_first = true;
        loop {
            let c = match self.bump() {
                None => return Err(Malformed),
                Some('\\') if matches!(self.peek(), Some('$' | '}' | '\\' | ',' | '|')) => {
                    self.bump().ok_or(Malformed)?
                }
                Some(',') => {
                    in_first = false;
                    continue;
                }
                Some('|') => {
                    return match self.bump() {
                        Some('}') => Ok(first),
                        _ => Err(Malformed),
                    }
                }
                Some(c) => c,
            };
            if in_first {
                first.push(c);
            }
        }
    }

    /// Skips `regex/format/options}` after `${N/` or `${VAR/`. A format may hold `${1:/upcase}`,
    /// whose `/` does not end the format.
    fn transform(&mut self) -> Result<(), Malformed> {
        self.skip_past('/')?;
        loop {
            match self.bump() {
                None => return Err(Malformed),
                Some('\\') => {
                    self.bump();
                }
                Some('$') if self.peek() == Some('{') => self.skip_past('}')?,
                Some('/') => break,
                Some(_) => {}
            }
        }
        self.skip_past('}')
    }

    fn skip_past(&mut self, end: char) -> Result<(), Malformed> {
        loop {
            match self.bump() {
                None => return Err(Malformed),
                Some('\\') => {
                    self.bump();
                }
                Some(c) if c == end => return Ok(()),
                Some(_) => {}
            }
        }
    }

    fn int(&mut self) -> Result<u32, Malformed> {
        let start = self.at;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.at += 1;
        }
        self.chars[start..self.at]
            .iter()
            .collect::<String>()
            .parse()
            .map_err(|_| Malformed)
    }

    fn name(&mut self) {
        while self
            .peek()
            .is_some_and(|c| c == '_' || c.is_ascii_alphanumeric())
        {
            self.at += 1;
        }
    }
}

fn is_name_start(c: char) -> bool {
    c == '_' || c.is_ascii_alphabetic()
}

/// Escapes the characters scrive's snippet grammar treats as syntax.
fn escape_into(out: &mut String, text: &str) {
    for c in text.chars() {
        if matches!(c, '$' | '}' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snippet(text: &str) -> InsertText {
        InsertText::Snippet(text.to_owned())
    }

    fn assert_lowers(rows: &[(&str, InsertText)]) {
        for (input, expected) in rows {
            assert_eq!(&lower(input), expected, "lowering {input:?}");
        }
    }

    /// Every snippet row in this module, so the parse check covers them all.
    const SNIPPETS: &[&str] = &[
        "foo($1, ${2})$0",
        "${1:a${2:b}c}",
        "$1 = $1;",
        "${1:x} + $1",
        "${1|one,two|}",
        "${1|a\\,b,c|}",
        "${TM_FILENAME:main.rs}",
        "$TM_SELECTED_TEXT!",
        "${CLIPBOARD}",
        "${1/(.*)/${1:/upcase}/g}x",
        "\\$x \\} \\\\",
        "a}b",
        "cost: $",
    ];

    /// Simple and final stops keep their positions; `${N}` becomes `$N`.
    #[test]
    fn tab_stops_and_final_stop_pass_through() {
        assert_lowers(&[("foo($1, ${2})$0", snippet("foo($1, $2)$0"))]);
    }

    /// A placeholder inside a default contributes its text, and only the outer stop remains.
    #[test]
    fn nested_placeholders_flatten_to_text() {
        assert_lowers(&[("${1:a${2:b}c}", snippet("${1:abc}"))]);
    }

    /// scrive rejects a repeated index, so later occurrences insert the first one's text.
    #[test]
    fn mirrors_become_text_after_the_first() {
        assert_lowers(&[
            ("$1 = $1;", snippet("$1 = ;")),
            ("${1:x} + $1", snippet("${1:x} + x")),
        ]);
    }

    /// A choice becomes a placeholder holding its first option, escapes resolved.
    #[test]
    fn choices_become_their_first_option() {
        assert_lowers(&[
            ("${1|one,two|}", snippet("${1:one}")),
            ("${1|a\\,b,c|}", snippet("${1:a,b}")),
        ]);
    }

    /// The client resolves no variables, so each becomes its default or nothing.
    #[test]
    fn variables_become_their_default_or_nothing() {
        assert_lowers(&[
            ("${TM_FILENAME:main.rs}", snippet("main.rs")),
            ("$TM_SELECTED_TEXT!", snippet("!")),
            ("${CLIPBOARD}", snippet("")),
        ]);
    }

    /// A transform is skipped, including a format holding its own `${…}`.
    #[test]
    fn transforms_are_dropped() {
        assert_lowers(&[("${1/(.*)/${1:/upcase}/g}x", snippet("$1x"))]);
    }

    /// Literal `$`, `}` and `\` come out escaped for scrive, however they came in.
    #[test]
    fn escapes_survive_lowering() {
        assert_lowers(&[
            ("\\$x \\} \\\\", snippet("\\$x \\} \\\\")),
            ("a}b", snippet("a\\}b")),
            ("cost: $", snippet("cost: \\$")),
        ]);
    }

    /// A body that is not a well-formed LSP snippet is inserted as the server sent it.
    #[test]
    fn unterminated_element_falls_back_to_the_raw_body() {
        for input in ["${1:abc", "${x", "${1|a"] {
            assert_eq!(
                lower(input),
                InsertText::Plain(input.to_owned()),
                "{input:?} is raw"
            );
        }
    }

    /// An index scrive cannot hold survives lowering; the parse check then inserts plain text.
    #[test]
    fn lowering_scrive_rejects_falls_back_to_plain_text() {
        assert_lowers(&[("x${70000:y}z", InsertText::Plain("xyz".to_owned()))]);
    }

    /// Whatever lowering calls a snippet, scrive's parser accepts.
    #[test]
    fn every_lowered_snippet_parses_with_scrive() {
        for input in SNIPPETS {
            let InsertText::Snippet(out) = lower(input) else {
                panic!("{input:?} lowers to a snippet")
            };
            assert!(Snippet::parse(&out).is_ok(), "{out:?} parses");
        }
    }
}
