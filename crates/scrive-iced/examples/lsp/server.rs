//! A scripted language server for the `lsp` example.
//!
//! It speaks raw JSON-RPC values on the server end of an `lsp_server::Connection::memory()`
//! pair. `step()` answers every message waiting on the connection and returns without blocking,
//! so the app calls it on the UI thread after every update, natively and in the browser.
//! Canned: `initialize`, completion, signature help, and hover (on `greet`).
//! Computed from the text the client sent: diagnostics (trailing whitespace), definition
//! (`fn <word>(` in any document), rename (whole-word, every document), formatting (strip
//! trailing whitespace), and inlay hints: a type hint after `let x = f(…)` for a function some
//! document defines, a parameter hint before each call's first argument, and their tooltips
//! through `inlayHint/resolve`. Columns are bytes: the initialize result picks `utf-8` positions.

use std::collections::BTreeMap;

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
        "documentFormattingProvider": true,
        "inlayHintProvider": { "resolveProvider": true }
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

/// Where the scripted standard library's `String` lives. No editor opens it, so a jump to it
/// comes back as `Jump::Unopened`.
const STRING_URI: &str = "file:///demo/std/string.rs";

/// The server's state: each document as the client last described it, and its end of the
/// connection.
pub struct Scripted {
    connection: lsp::lsp_server::Connection,
    documents: BTreeMap<String, Document>,
}

struct Document {
    version: i64,
    text: String,
}

/// A function some document defines as `fn name(param: …) -> Returns`.
struct Signature<'a> {
    name: &'a str,
    param: &'a str,
    returns: &'a str,
    /// Where the parameter's name is, as an LSP location.
    param_location: Value,
}

impl Scripted {
    /// A server on `connection`'s end.
    pub fn new(connection: lsp::lsp_server::Connection) -> Self {
        Self {
            connection,
            documents: BTreeMap::new(),
        }
    }

    /// Answers everything the client has sent so far, and returns whether there was anything.
    /// It never blocks, so it runs on the UI thread in a browser too.
    pub fn step(&mut self) -> bool {
        let mut stepped = false;
        while let Ok(message) = self.connection.receiver.try_recv() {
            stepped = true;
            self.receive(&serde_json::to_value(message).expect("lsp-server messages serialize"));
        }
        stepped
    }

    /// Take one message from the client, and send whatever the server says back.
    fn receive(&mut self, wire: &Value) {
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
            "textDocument/inlayHint" => self.inlay_hints(params),
            "inlayHint/resolve" => resolve(params),
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

    /// Every `fn name(param: …) -> Returns` in any document whose first parameter and return
    /// type are on the `fn` line.
    fn signatures(&self) -> Vec<Signature<'_>> {
        let mut found = Vec::new();
        for (uri, document) in &self.documents {
            let text = document.text.as_str();
            for (at, _) in text.match_indices("fn ") {
                let line_end = text[at..].find('\n').map_or(text.len(), |i| at + i);
                let name_at = at + "fn ".len();
                let name = word(text, name_at);
                let param_at = name_at + name.len() + "(".len();
                if name.is_empty() || !text[name_at + name.len()..].starts_with('(') {
                    continue;
                }
                let Some(arrow) = text[param_at..line_end].find(") -> ") else {
                    continue;
                };
                let param = word(text, param_at);
                let returns = word(text, param_at + arrow + ") -> ".len());
                if param.is_empty() || returns.is_empty() {
                    continue;
                }
                let range = json!({
                    "start": position(text, param_at),
                    "end": position(text, param_at + param.len())
                });
                found.push(Signature {
                    name,
                    param,
                    returns,
                    param_location: json!({ "uri": uri, "range": range }),
                });
            }
        }
        found
    }

    /// The hints for one document. The request's range is ignored: the client drops what falls
    /// outside the span it asked for.
    fn inlay_hints(&self, params: &Value) -> Value {
        let Some(document) = params["textDocument"]["uri"]
            .as_str()
            .and_then(|uri| self.documents.get(uri))
        else {
            return Value::Null;
        };
        let text = document.text.as_str();
        let signatures = self.signatures();
        let mut hints = Vec::new();
        for signature in &signatures {
            for at in occurrences(text, signature.name) {
                let open = at + signature.name.len();
                let defines = text[..at].ends_with("fn ");
                if defines || !text[open..].starts_with('(') || text[open + 1..].starts_with(')') {
                    continue;
                }
                hints.push(parameter_hint(text, open + 1, signature));
            }
        }
        for (at, _) in text.match_indices("let ") {
            let name_at = at + "let ".len();
            let name_end = name_at + word(text, name_at).len();
            let line_end = text[name_end..].find('\n').map_or(text.len(), |i| name_end + i);
            let rest = &text[name_end..line_end];
            // `let x: T = …` already states its type.
            if name_end == name_at || !rest.starts_with(" = ") {
                continue;
            }
            let called = signatures
                .iter()
                .find(|signature| rest.contains(&format!("{}(", signature.name)));
            if let Some(signature) = called {
                hints.push(type_hint(text, name_end, signature));
            }
        }
        Value::Array(hints)
    }

    fn push(&mut self, envelope: Value) {
        let message =
            serde_json::from_value(envelope).expect("the script writes valid lsp-server messages");
        // The client end is gone: nobody is left to answer.
        let _ = self.connection.sender.send(message);
    }
}

/// `param:` before a call's first argument; the name links to the parameter.
fn parameter_hint(text: &str, at: usize, signature: &Signature<'_>) -> Value {
    json!({
        "position": position(text, at),
        "kind": 2,
        "label": [
            { "value": signature.param, "location": signature.param_location },
            { "value": ":" }
        ],
        "paddingRight": true,
        "data": {
            "tooltip": format!("The `{}` parameter of `{}`.", signature.param, signature.name)
        }
    })
}

/// `: Returns` after a `let` name, insertable; `String` links into the unopened standard library.
fn type_hint(text: &str, at: usize, signature: &Signature<'_>) -> Value {
    let mut returns = json!({ "value": signature.returns });
    if signature.returns == "String" {
        returns["location"] = json!({
            "uri": STRING_URI,
            "range": {
                "start": { "line": 0, "character": 11 },
                "end": { "line": 0, "character": 17 }
            }
        });
    }
    json!({
        "position": position(text, at),
        "kind": 1,
        "label": [{ "value": ": " }, returns],
        "textEdits": [{
            "range": { "start": position(text, at), "end": position(text, at) },
            "newText": format!(": {}", signature.returns)
        }],
        "data": { "tooltip": format!("What `{}` returns.", signature.name) }
    })
}

/// `inlayHint/resolve`: the hint, with the tooltip its `data` carries. A real server would look
/// the tooltip up; the script keeps it in `data` so the round trip stays visible.
fn resolve(params: &Value) -> Value {
    let mut hint = params.clone();
    hint["tooltip"] = json!({ "kind": "markdown", "value": params["data"]["tooltip"] });
    hint
}

/// The identifier starting at byte `at`, empty when none does.
fn word(text: &str, at: usize) -> &str {
    let end = text[at..]
        .find(|c: char| !is_word(c))
        .map_or(text.len(), |i| at + i);
    &text[at..end]
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
