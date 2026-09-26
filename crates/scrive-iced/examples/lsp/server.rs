//! A scripted language server for the `lsp` example.
//!
//! It speaks raw JSON-RPC values, like the far end of a pipe, and answers at once: each reply is
//! queued the moment its request arrives, and the example's transport delivers them one per
//! `Message::Deliver`. Canned: `initialize`, completion, signature help, and hover (on `greet`).
//! Computed from the text the client sent: diagnostics (trailing whitespace), definition
//! (`fn <word>(` in any document), rename (whole-word, every document) and formatting (strip
//! trailing whitespace). Columns are bytes: the initialize result picks `utf-8` positions.

use std::collections::{BTreeMap, VecDeque};

use serde_json::{json, Value};

use scrive_iced::lsp;

/// The capabilities: incremental sync, every provider the bridge speaks, and byte columns.
const INITIALIZE: &str = r#"{
    "capabilities": {
        "positionEncoding": "utf-8",
        "textDocumentSync": { "openClose": true, "change": 2 },
        "completionProvider": { "triggerCharacters": ["."] },
        "signatureHelpProvider": { "triggerCharacters": ["(", ","] },
        "hoverProvider": true,
        "definitionProvider": true,
        "renameProvider": true,
        "documentFormattingProvider": true
    },
    "serverInfo": { "name": "scrive-demo", "version": "0.4.0" }
}"#;

/// The completion list. `greet` is a snippet that asks for signature help once accepted, and
/// `println!` filters by `println`.
const COMPLETION: &str = r#"{
    "isIncomplete": false,
    "items": [
        {
            "label": "greet",
            "kind": 3,
            "detail": "fn(name: &str) -> String",
            "documentation": "Builds a greeting for `name`.",
            "insertTextFormat": 2,
            "insertText": "greet(${1:name})",
            "command": { "title": "Signature help", "command": "editor.action.triggerParameterHints" }
        },
        { "label": "farewell", "kind": 3, "detail": "fn(name: &str) -> String" },
        { "label": "println!", "kind": 3, "filterText": "println", "insertTextFormat": 2, "insertText": "println!(\"$1\")" },
        { "label": "String", "kind": 22 }
    ]
}"#;

/// One signature. The parameter label is a UTF-16 offset pair: `greet(` is 6 units, and
/// `name: &str` covers 6..16.
const SIGNATURE_HELP: &str = r#"{
    "signatures": [
        {
            "label": "greet(name: &str) -> String",
            "documentation": { "kind": "markdown", "value": "Builds a greeting for `name`." },
            "parameters": [{ "label": [6, 16] }]
        }
    ],
    "activeSignature": 0,
    "activeParameter": 0
}"#;

/// The hover card for `greet`: a fenced signature, a rule, and prose.
const HOVER: &str = r#"{
    "contents": {
        "kind": "markdown",
        "value": "```rust\nfn greet(name: &str) -> String\n```\n---\nBuilds a greeting for `name`."
    }
}"#;

/// The server's state: each document as the client last described it, and the replies not yet
/// delivered.
#[derive(Default)]
pub struct Scripted {
    documents: BTreeMap<String, Document>,
    outbox: VecDeque<lsp::Message>,
}

struct Document {
    version: i64,
    text: String,
}

impl Scripted {
    /// Take one message from the client, and queue whatever the server says back.
    pub fn receive(&mut self, message: &lsp::Message) {
        let wire = serde_json::to_value(message).expect("envelopes serialize");
        // A response to a server request: this server never asks, so there's nothing to match.
        let Some(method) = wire.get("method").and_then(Value::as_str) else {
            return;
        };
        let params = &wire["params"];
        match wire.get("id") {
            Some(id) => {
                let result = self.answer(method, params);
                self.push(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
            }
            None => self.notified(method, params),
        }
    }

    /// The next queued reply or notification, in order.
    pub fn next(&mut self) -> Option<lsp::Message> {
        self.outbox.pop_front()
    }

    /// Whether everything the server said has been delivered.
    pub fn is_idle(&self) -> bool {
        self.outbox.is_empty()
    }

    /// The text the server holds for `uri`, which is what the client's syncs built.
    #[cfg(test)]
    pub fn text(&self, uri: &str) -> Option<&str> {
        self.documents
            .get(uri)
            .map(|document| document.text.as_str())
    }

    fn answer(&self, method: &str, params: &Value) -> Value {
        match method {
            "initialize" => canned(INITIALIZE),
            "textDocument/completion" => canned(COMPLETION),
            "textDocument/signatureHelp" => canned(SIGNATURE_HELP),
            "textDocument/hover" => match self.word_at(params) {
                Some("greet") => canned(HOVER),
                _ => Value::Null,
            },
            "textDocument/definition" => self.definition(params),
            "textDocument/rename" => self.rename(params),
            "textDocument/formatting" => self.format(params),
            // `shutdown`, and anything unscripted.
            _ => Value::Null,
        }
    }

    fn notified(&mut self, method: &str, params: &Value) {
        let uri = params["textDocument"]["uri"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        match method {
            "textDocument/didOpen" => {
                let document = Document {
                    version: params["textDocument"]["version"]
                        .as_i64()
                        .unwrap_or_default(),
                    text: params["textDocument"]["text"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                };
                self.documents.insert(uri.clone(), document);
                self.publish(&uri);
            }
            "textDocument/didChange" => {
                let Some(document) = self.documents.get_mut(&uri) else {
                    return;
                };
                document.version = params["textDocument"]["version"]
                    .as_i64()
                    .unwrap_or(document.version);
                // Changes apply in order, each against the text the previous one left.
                for change in params["contentChanges"].as_array().into_iter().flatten() {
                    let text = change["text"].as_str().unwrap_or_default();
                    match change.get("range") {
                        Some(range) => {
                            let range = offset(&document.text, &range["start"])
                                ..offset(&document.text, &range["end"]);
                            document.text.replace_range(range, text);
                        }
                        None => document.text = text.to_owned(),
                    }
                }
                self.publish(&uri);
            }
            "textDocument/didClose" => {
                self.documents.remove(&uri);
            }
            // `initialized`, `$/cancelRequest` (every reply is already queued), `exit`.
            _ => {}
        }
    }

    /// Publish a trailing-whitespace warning for every line that has some.
    fn publish(&mut self, uri: &str) {
        let Some(document) = self.documents.get(uri) else {
            return;
        };
        let diagnostics: Vec<Value> = document
            .text
            .split('\n')
            .enumerate()
            .filter_map(|(line, text)| {
                let end = text.trim_end().len();
                (end < text.len()).then(|| {
                    json!({
                        "range": {
                            "start": { "line": line, "character": end },
                            "end": { "line": line, "character": text.len() }
                        },
                        "severity": 2,
                        "source": "demo",
                        "message": "trailing whitespace"
                    })
                })
            })
            .collect();
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": uri, "version": document.version, "diagnostics": diagnostics }
        });
        self.push(notification);
    }

    /// The identifier under `params`' position.
    fn word_at(&self, params: &Value) -> Option<&str> {
        let document = self
            .documents
            .get(params["textDocument"]["uri"].as_str()?)?;
        let text = document.text.as_str();
        let at = offset(text, &params["position"]);
        let start = text[..at]
            .char_indices()
            .rev()
            .take_while(|&(_, c)| is_word(c))
            .last()
            .map_or(at, |(i, _)| i);
        let end = text[at..]
            .char_indices()
            .find(|&(_, c)| !is_word(c))
            .map_or(text.len(), |(i, _)| at + i);
        (start < end).then(|| &text[start..end])
    }

    /// The name in the first `fn <word>(` of any document.
    fn definition(&self, params: &Value) -> Value {
        let Some(word) = self.word_at(params) else {
            return Value::Null;
        };
        let needle = format!("fn {word}(");
        for (uri, document) in &self.documents {
            if let Some(at) = document.text.find(&needle) {
                let name = at + "fn ".len();
                return json!({
                    "uri": uri,
                    "range": { "start": position(&document.text, name), "end": position(&document.text, name + word.len()) }
                });
            }
        }
        Value::Null
    }

    /// Every whole-word occurrence of the identifier, in every document, as versioned
    /// `documentChanges`.
    fn rename(&self, params: &Value) -> Value {
        let (Some(word), Some(new_name)) = (self.word_at(params), params["newName"].as_str())
        else {
            return Value::Null;
        };
        let changes: Vec<Value> = self
            .documents
            .iter()
            .filter_map(|(uri, document)| {
                let edits: Vec<Value> = occurrences(&document.text, word)
                    .map(|at| {
                        json!({
                            "range": { "start": position(&document.text, at), "end": position(&document.text, at + word.len()) },
                            "newText": new_name
                        })
                    })
                    .collect();
                (!edits.is_empty()).then(|| json!({ "textDocument": { "uri": uri, "version": document.version }, "edits": edits }))
            })
            .collect();
        json!({ "documentChanges": changes })
    }

    /// Trailing whitespace stripped, sent the way many formatters send it: one edit over the
    /// whole document. The client diffs it down to the lines that changed.
    fn format(&self, params: &Value) -> Value {
        let Some(document) = params["textDocument"]["uri"]
            .as_str()
            .and_then(|uri| self.documents.get(uri))
        else {
            return Value::Null;
        };
        let lines: Vec<&str> = document.text.split('\n').collect();
        let formatted = lines
            .iter()
            .map(|line| line.trim_end())
            .collect::<Vec<_>>()
            .join("\n");
        if formatted == document.text {
            return json!([]);
        }
        // The end line is one past the last line, which the client clamps to the end of the
        // document.
        json!([{
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": lines.len(), "character": 0 } },
            "newText": formatted
        }])
    }

    fn push(&mut self, envelope: Value) {
        self.outbox.push_back(
            serde_json::from_value(envelope).expect("the script writes valid envelopes"),
        );
    }
}

fn canned(payload: &str) -> Value {
    serde_json::from_str(payload).expect("canned payloads are valid JSON")
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Byte offsets of `word` in `text` where it stands as a whole identifier.
fn occurrences<'a>(text: &'a str, word: &'a str) -> impl Iterator<Item = usize> + 'a {
    text.match_indices(word)
        .map(|(at, _)| at)
        .filter(move |&at| {
            let before = text[..at].chars().next_back().is_some_and(is_word);
            let after = text[at + word.len()..].chars().next().is_some_and(is_word);
            !before && !after
        })
}

/// The position of byte `at`, with byte columns.
fn position(text: &str, at: usize) -> Value {
    let line = text[..at].matches('\n').count();
    let character = at - text[..at].rfind('\n').map_or(0, |i| i + 1);
    json!({ "line": line, "character": character })
}

/// The byte offset of `position`, clamped to its line and to the end of the text.
fn offset(text: &str, position: &Value) -> usize {
    let line = position["line"].as_u64().unwrap_or_default() as usize;
    let character = position["character"].as_u64().unwrap_or_default() as usize;
    let start: usize = text.split_inclusive('\n').take(line).map(str::len).sum();
    let rest = &text[start..];
    start + character.min(rest.find('\n').unwrap_or(rest.len()))
}
