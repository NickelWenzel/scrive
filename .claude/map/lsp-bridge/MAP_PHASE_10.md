# Phase 10 — the `lsp` example, the READMEs, and the 0.4.0 bump

## Prerequisites

- Phases 1–9 are merged, and `cargo test --workspace --all-features` is green.
- Read in full:
  - `crates/scrive-iced/examples/minimal.rs` and `scratch.rs` (conventions: module doc with the
    run command, `iced::application::timed`, `required_fonts`, headless tests that call `update`
    with a fixed `Instant`);
  - `crates/scrive-iced/src/code_editor/lsp.rs` (Phase 9);
  - `crates/scrive-lsp/src/{lib.rs, update.rs, client.rs}`;
  - `README.md`, `crates/scrive-iced/README.md` (byte-identical today) and
    `crates/scrive-lsp/README.md` (Phase 4's);
  - `Cargo.toml` and `crates/scrive-iced/{Cargo.toml, index.html}`.
- The iced skill governs every line of the example:
  - a boot function returning `(State, Task)`;
  - id-tagged envelopes with `.with(id)`;
  - function helpers, never `Widget::new`;
  - view fragments as functions;
  - no aliased imports;
  - module-path names.

## Goal and exit criteria

`crates/scrive-iced/examples/lsp/` (`main.rs`, `server.rs`) is a two-tab editor (demo files
`main.rs` and `util.rs`) on one `lsp::Client`,
wired exactly like MAP_PLAN.md "Target state":
- The "server" is `server.rs`, a scripted in-process language server. It answers initialize,
  completion, signature help and hover with canned JSON. It computes diagnostics (trailing
  whitespace), goto definition (`fn <word>(`), rename (whole-word, every open file) and
  formatting (strip trailing whitespace, sent as one whole-document edit).
- A traffic panel shows the JSON-RPC traffic both ways.
- The READMEs document the feature, and the workspace is 0.4.0.

**Exit.**
- `cargo test -p scrive-iced --features lsp --example lsp` passes these headless tests, all in
  `examples/lsp/main.rs`:
  - `diagnostics_land_in_both_documents`
  - `f12_switches_tabs_and_selects_the_definition`
  - `rename_changes_both_documents_and_the_server_sees_both_did_changes`
  - `format_strips_trailing_whitespace`
  - `the_traffic_panel_logs_both_directions`
- `cargo build --workspace --all-targets --all-features --target wasm32-unknown-unknown` builds
  the example.
- Both doc builds are clean.
- `cmp README.md crates/scrive-iced/README.md` succeeds.
- No `0.3.0` remains in any manifest.

## Design decisions implemented

- **D1.** The example imports the bridge through the facade: `use scrive_iced::lsp;`. The bump
  touches only the root manifest, because Phase 4 moved the internal crates to
  `[workspace.dependencies]`.
- **D4 / D19.** The host loop:
  - `Message::Editor(id, event)` runs `editor.update`, then `sync_lsp`.
  - `Message::Lsp(message)` runs `client.receive`, then routes:
    - `Update::Document` goes to the tab whose `doc_id()` matches, through `apply_lsp`;
    - `Jump::Open` goes to `target.editor.jump`, which switches the active tab;
    - `Jump::Unopened`, `FileEdits` and `Notification` are logged.
- **D5.** One client for both tabs, and one `open_lsp` per tab at boot, before the handshake.
  Opens are deferred until `initialized`.
- **D17.** Each editor is built with `.rename(true)`. F12, F2, Shift+Alt+F and Ctrl+Space come
  from Phases 2/3.
- **D18.** Formatting arrives as `{0:0 → line_count:0}`, so the demo exercises D7's
  end-of-document clamp and the line diff. Rename arrives as `documentChanges` with versions.
- **Risk 8.** Only the active tab's editor is subscribed, so global chords (Ctrl+F) reach one
  editor.
- **Constraints.** No I/O, no threads, no `std::time`. The example builds for wasm32.

Decisions this doc makes where the plan is open:

- **Decision: the transport is a `Transport` struct holding the scripted server and the traffic
  log.**
  - `send` hands messages to the server and returns `Task::done(Message::Deliver)` while replies
    are queued.
  - `Message::Deliver` pops **one** reply and routes it through the `Message::Lsp` arm.
  - This pumps the same way under the iced runtime and in tests. Tests call `update(Deliver)`
    until the server is idle and never run a `Task`.
- **Decision: the server speaks raw `serde_json::Value`.** It converts `lsp::Message` to and from
  JSON with serde. That keeps it independent of the envelope's Rust constructors and makes it read
  like the far end of a pipe. serde_json is Phase 9's dev-dependency.
- **Decision: the initialize result negotiates `positionEncoding: "utf-8"`.** The server's
  columns are then byte offsets. The client advertises utf-8 first (D7), so this is a legal
  choice.
- **Decision: hover is canned but gated on the word `greet`,** and returns `null` elsewhere.
  One canned card over every word would read as a bug.
- **Decision: `Jump::Unopened` and `FileEdits` are logged, not acted on.** Both demo files are
  open, and there is no disk on wasm. The README points hosts at `unopened.span(&text)` and
  `edits.apply(&disk_text)`.
- **Decision: the client field is named `client`**, not `lsp`, so `lsp::Update` always means
  the module.
- **Decision: the server type is `server::Scripted`.** That avoids the stutter of
  `server::Server`.
- **Decision: the iced theme is `Theme::Dark`,** which suits the bundled Scrive Dark syntax
  theme.

## Step-by-step changes

### 1. `crates/scrive-iced/Cargo.toml`

After the existing `[[example]]` entries:

```toml
[[example]]
name = "lsp"
path = "examples/lsp/main.rs"
# The example drives the `CodeEditor` language-server glue, which only exists
# with the feature.
required-features = ["lsp"]
# Its headless tests run under `cargo test --features lsp` (CI: `--all-features`).
test = true
```

Don't change the description, keywords or dependencies (serde_json is already a dev-dependency
from Phase 9).

### 2. `crates/scrive-iced/examples/lsp/main.rs` (new)

```rust
//! `lsp` — two editors on one language-server client, against a scripted server.
//!
//! ```text
//! cargo run -p scrive-iced --features lsp --example lsp
//! ```
//!
//! The server (`server.rs`) runs in-process, so there's no transport to set up. It answers
//! `initialize`, completion, signature help and hover with canned JSON, and computes
//! diagnostics (trailing whitespace), goto definition, rename and formatting from the text the
//! client sent it. The right-hand panel shows the traffic. Try:
//!
//! - F12 on `greet` in main.rs: the util.rs tab opens with the definition selected;
//! - F2 on `greet`, type a new name, Enter: both files change;
//! - Shift+Alt+F: the trailing whitespace goes, and so do its warnings;
//! - typing, `(`, and hovering `greet`: the canned completion, signature and hover.
//!
//! The wiring is the one a real host uses: sync after every editor update, route each
//! `Update::Document` to the tab that owns it, and hand a cross-file jump to the target tab.

// On Windows, a release build is a GUI app with no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod server;

use std::collections::VecDeque;

use iced::time::Instant;
use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Element, Fill, FillPortion, Function, Subscription, Task, Theme};
use serde_json::Value;

use scrive_core::SyntaxDef;
use scrive_iced::{lsp, CodeEditor, Event};

/// The workspace root the client reports to the server.
const ROOT: &str = "file:///demo/";
const MAIN_URI: &str = "file:///demo/main.rs";
const UTIL_URI: &str = "file:///demo/util.rs";

/// The demo files. Some lines end in spaces on purpose: the server flags them, and formatting
/// removes them.
const MAIN_RS: &str = "mod util;\n\nfn main() {   \n    let message = util::greet(\"scrive\");  \n    println!(\"{message}\");\n}\n";
const UTIL_RS: &str = "/// Builds a greeting for `name`.\npub fn greet(name: &str) -> String {  \n    format!(\"hello, {name}\")\n}\n\npub fn farewell(name: &str) -> String {\n    format!(\"goodbye, {name}\")\n}\n";

/// How many lines the traffic panel keeps.
const TRAFFIC_LINES: usize = 200;

/// Tab identity.
mod tab {
    /// Which tab an editor message belongs to: the key of the `Message::Editor` envelope.
    /// `Hash` because the active tab's subscription is keyed by it (`Subscription::with`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Id(pub usize);
}

/// One open file.
struct Tab {
    id: tab::Id,
    title: &'static str,
    editor: CodeEditor,
}

/// The application: two tabs on one language-server client, and the wire to the server.
struct App {
    client: lsp::Client,
    tabs: Vec<Tab>,
    active: tab::Id,
    transport: Transport,
}

#[derive(Debug, Clone)]
enum Message {
    /// A message from one tab's editor.
    Editor(tab::Id, Event),
    /// A JSON-RPC message from the server.
    Lsp(lsp::Message),
    /// Take the server's next queued reply off the wire.
    Deliver,
    /// A tab button was pressed.
    Select(tab::Id),
}

/// The app-owned transport: here, a scripted server in the same process, plus a log of
/// everything that crossed it. A real host would hold a child process's pipes or a socket.
#[derive(Default)]
struct Transport {
    server: server::Scripted,
    traffic: VecDeque<String>,
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let (mut client, initialize) = lsp::Client::builder().root(uri(ROOT)).build();
        let mut outgoing = vec![initialize];
        let mut tabs = Vec::new();
        for (index, (title, path, source)) in
            [("main.rs", MAIN_URI, MAIN_RS), ("util.rs", UTIL_URI, UTIL_RS)].into_iter().enumerate()
        {
            let mut editor = CodeEditor::new(source).language(rust()).rename(true);
            // Before the handshake completes, this only records the text. The didOpen goes
            // out with `initialized`.
            outgoing.extend(editor.open_lsp(&mut client, &uri(path), "rust").expect("the demo's URIs are distinct"));
            tabs.push(Tab { id: tab::Id(index), title, editor });
        }
        let mut transport = Transport::default();
        let task = transport.send(outgoing);
        (Self { client, tabs, active: tab::Id(0), transport }, task)
    }

    /// `now` is the instant iced stamps on the message (`iced::application::timed`).
    fn update(&mut self, message: Message, now: Instant) -> Task<Message> {
        // Disjoint borrows: a tab's editor and the client are both `&mut` in the Lsp arm.
        let Self { client, tabs, active, transport } = self;
        match message {
            Message::Editor(id, event) => {
                let Some(tab) = tabs.iter_mut().find(|tab| tab.id == id) else { return Task::none() };
                let task = tab.editor.update(event, now).map(Message::Editor.with(id));
                let outgoing = tab.editor.sync_lsp(client);
                Task::batch([task, transport.send(outgoing)])
            }
            Message::Lsp(message) => {
                let output = match client.receive(message) {
                    Ok(output) => output,
                    Err(error) => {
                        transport.note(format!("error: {error}"));
                        return transport.send(Vec::new());
                    }
                };
                let mut outgoing = output.messages;
                for update in output.updates {
                    match update {
                        lsp::Update::Document(document) => {
                            let Some(tab) = tabs.iter_mut().find(|tab| tab.editor.document().doc_id() == document.doc_id())
                            else {
                                continue;
                            };
                            let applied = tab.editor.apply_lsp(client, document);
                            outgoing.extend(applied.messages);
                            if let Some(refusal) = applied.refused {
                                transport.note(format!("refused: {refusal}"));
                            }
                            match applied.jump {
                                Some(lsp::update::Jump::Open(open)) => {
                                    let Some(target) = tabs.iter_mut().find(|tab| tab.editor.document().doc_id() == open.doc_id())
                                    else {
                                        continue;
                                    };
                                    match target.editor.jump(client, open) {
                                        Ok(messages) => {
                                            outgoing.extend(messages);
                                            *active = target.id;
                                        }
                                        Err(refusal) => transport.note(format!("jump refused: {refusal}")),
                                    }
                                }
                                // A host with files would read this one, open a tab, `open_lsp`
                                // it, then `select(unopened.span(&text))`. Every demo file is
                                // open, and wasm has no disk.
                                Some(lsp::update::Jump::Unopened(unopened)) => {
                                    transport.note(format!("definition in unopened {}", unopened.uri().as_str()));
                                }
                                None => {}
                            }
                        }
                        // A host with files writes `edits.apply(&disk_text)` back to disk.
                        lsp::Update::FileEdits(edits) => {
                            transport.note(format!("edits for unopened {}", edits.uri().as_str()));
                        }
                        lsp::Update::Notification(notification) => transport.note(format!("{notification:?}")),
                    }
                }
                transport.send(outgoing)
            }
            Message::Deliver => match transport.next() {
                // Route it exactly as a message from a real transport would arrive.
                Some(message) => self.update(Message::Lsp(message), now),
                None => Task::none(),
            },
            Message::Select(id) => {
                *active = id;
                Task::none()
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let tabs = row(self.tabs.iter().map(|tab| tab_button(tab, self.active))).spacing(4);
        let editor = match self.tabs.iter().find(|tab| tab.id == self.active) {
            Some(tab) => tab.editor.view().map(Message::Editor.with(tab.id)),
            None => text("no tab").into(),
        };
        let body = row![container(editor).width(FillPortion(3)).height(Fill), traffic(&self.transport.traffic)].spacing(8);
        column![tabs, body].spacing(8).padding(8).into()
    }

    /// Only the active tab listens: global chords (Ctrl+F) must reach one editor.
    fn subscription(&self) -> Subscription<Message> {
        match self.tabs.iter().find(|tab| tab.id == self.active) {
            // `Subscription::map` takes only non-capturing closures, so the id rides along
            // through `with`.
            Some(tab) => tab.editor.subscription().with(tab.id).map(|(id, event)| Message::Editor(id, event)),
            None => Subscription::none(),
        }
    }
}

impl Transport {
    /// Hand `outgoing` to the server, logging each message. If replies are queued, schedule the
    /// next delivery.
    fn send(&mut self, outgoing: Vec<lsp::Message>) -> Task<Message> {
        for message in &outgoing {
            self.log("→", message);
            self.server.receive(message);
        }
        if self.server.is_idle() { Task::none() } else { Task::done(Message::Deliver) }
    }

    /// The server's next reply, logged.
    fn next(&mut self) -> Option<lsp::Message> {
        let message = self.server.next()?;
        self.log("←", &message);
        Some(message)
    }

    /// A line of commentary: refusals, errors, and what a disk-backed host would do.
    fn note(&mut self, note: String) {
        self.push(format!("· {note}"));
    }

    fn log(&mut self, arrow: &str, message: &lsp::Message) {
        let wire = serde_json::to_value(message).expect("envelopes serialize");
        let what = match (wire.get("method").and_then(Value::as_str), wire.get("id")) {
            (Some(method), Some(id)) => format!("{method} #{id}"),
            (Some(method), None) => method.to_owned(),
            (None, Some(id)) => format!("response #{id}"),
            (None, None) => "?".to_owned(),
        };
        self.push(format!("{arrow} {what}"));
    }

    fn push(&mut self, line: String) {
        if self.traffic.len() == TRAFFIC_LINES {
            self.traffic.pop_front();
        }
        self.traffic.push_back(line);
    }
}

fn tab_button(tab: &Tab, active: tab::Id) -> Element<'_, Message> {
    let style = if tab.id == active { button::primary } else { button::secondary };
    button(text(tab.title)).style(style).on_press(Message::Select(tab.id)).into()
}

/// The traffic panel: newest at the bottom, pinned there as lines arrive.
fn traffic(lines: &VecDeque<String>) -> Element<'_, Message> {
    let lines = column(lines.iter().map(|line| Element::from(text(line.as_str()).size(12).font(scrive_iced::DEFAULT_FONT))));
    container(scrollable(lines).anchor_bottom().width(Fill).height(Fill)).width(FillPortion(2)).height(Fill).into()
}

fn uri(text: &str) -> lsp::lsp_types::Uri {
    text.parse().expect("the demo's URIs parse")
}

fn rust() -> SyntaxDef {
    SyntaxDef::from_sublime_syntax(include_str!("../assets/rust.sublime-syntax")).expect("bundled Rust grammar parses")
}

fn theme(_app: &App) -> Theme {
    Theme::Dark
}

fn main() -> iced::Result {
    // `timed` hands `update` each message's instant, which the editor's debounces run on.
    let app = iced::application::timed(App::new, App::update, App::subscription, App::view)
        .title("scrive — lsp")
        .theme(theme);
    app.fonts(scrive_iced::required_fonts().iter().copied()).run()
}
```

Notes:
- `mod server;` resolves to `examples/lsp/server.rs`, because `main.rs` is the crate root of that
  directory. No `#[path]`.
- `include_str!("../assets/rust.sublime-syntax")` points at the shared `examples/assets/` grammar.
- If `.theme(theme)` doesn't accept a free function in the pinned iced, pass
  `|_: &App| Theme::Dark` instead. scratch.rs passes a free `fn theme(_state: &App) -> Theme`, so
  it should.
- The editors keep the default widget id. Only the active tab renders, and Phase 3's `diff` reset
  rebuilds widget state when the rendered `DocId` changes, which is exactly a tab switch.

### 3. `crates/scrive-iced/examples/lsp/server.rs` (new)

```rust
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
        let Some(method) = wire.get("method").and_then(Value::as_str) else { return };
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
        self.documents.get(uri).map(|document| document.text.as_str())
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
        let uri = params["textDocument"]["uri"].as_str().unwrap_or_default().to_owned();
        match method {
            "textDocument/didOpen" => {
                let document = Document {
                    version: params["textDocument"]["version"].as_i64().unwrap_or_default(),
                    text: params["textDocument"]["text"].as_str().unwrap_or_default().to_owned(),
                };
                self.documents.insert(uri.clone(), document);
                self.publish(&uri);
            }
            "textDocument/didChange" => {
                let Some(document) = self.documents.get_mut(&uri) else { return };
                document.version = params["textDocument"]["version"].as_i64().unwrap_or(document.version);
                // Changes apply in order, each against the text the previous one left.
                for change in params["contentChanges"].as_array().into_iter().flatten() {
                    let text = change["text"].as_str().unwrap_or_default();
                    match change.get("range") {
                        Some(range) => {
                            let range = offset(&document.text, &range["start"])..offset(&document.text, &range["end"]);
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
        let Some(document) = self.documents.get(uri) else { return };
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
        let document = self.documents.get(params["textDocument"]["uri"].as_str()?)?;
        let text = document.text.as_str();
        let at = offset(text, &params["position"]);
        let start = text[..at].char_indices().rev().take_while(|&(_, c)| is_word(c)).last().map_or(at, |(i, _)| i);
        let end = text[at..].char_indices().find(|&(_, c)| !is_word(c)).map_or(text.len(), |(i, _)| at + i);
        (start < end).then(|| &text[start..end])
    }

    /// The name in the first `fn <word>(` of any document.
    fn definition(&self, params: &Value) -> Value {
        let Some(word) = self.word_at(params) else { return Value::Null };
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
        let (Some(word), Some(new_name)) = (self.word_at(params), params["newName"].as_str()) else {
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
        let Some(document) = params["textDocument"]["uri"].as_str().and_then(|uri| self.documents.get(uri)) else {
            return Value::Null;
        };
        let lines: Vec<&str> = document.text.split('\n').collect();
        let formatted = lines.iter().map(|line| line.trim_end()).collect::<Vec<_>>().join("\n");
        if formatted == document.text {
            return json!([]);
        }
        // The end line is one past the last line; the client clamps it to the end of the
        // document (D7).
        json!([{
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": lines.len(), "character": 0 } },
            "newText": formatted
        }])
    }

    fn push(&mut self, envelope: Value) {
        self.outbox.push_back(serde_json::from_value(envelope).expect("the script writes valid envelopes"));
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
    text.match_indices(word).map(|(at, _)| at).filter(move |&at| {
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
```

`unwrap_or_default` and `unwrap_or` are fine here: they aren't `unwrap()`, and this is example
code. Every `expect` names its invariant.

### 4. Headless tests (bottom of `examples/lsp/main.rs`)

The tests never run a `Task`. `settle` plays the iced runtime's part: it feeds `Message::Deliver`
until the server has nothing queued. Because `Transport::send` queues and `Deliver` pops one reply
at a time, the order is the same as under iced.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use scrive_iced::Action;

    const MAIN: tab::Id = tab::Id(0);
    const UTIL: tab::Id = tab::Id(1);

    /// Deliver every queued server message, as the runtime would by running the Deliver tasks.
    fn settle(app: &mut App) {
        let now = Instant::now();
        while !app.transport.server.is_idle() {
            let _ = app.update(Message::Deliver, now);
        }
    }

    /// The app after its handshake: both documents open, their first diagnostics landed.
    fn booted() -> App {
        let (mut app, _boot) = App::new();
        settle(&mut app);
        app
    }

    /// Feed `event` to tab `id`, then let the conversation finish.
    fn press(app: &mut App, id: tab::Id, event: Event) {
        let _ = app.update(Message::Editor(id, event), Instant::now());
        settle(app);
    }

    fn editor(app: &App, id: tab::Id) -> &CodeEditor {
        &app.tabs.iter().find(|tab| tab.id == id).expect("the demo has this tab").editor
    }

    fn text_of(app: &App, id: tab::Id) -> String {
        editor(app, id).document().text().into_owned()
    }

    fn diagnostics(app: &App, id: tab::Id) -> usize {
        let document = editor(app, id).document();
        document.diagnostics_in(0..document.buffer().len()).count()
    }

    /// The server's first publish reaches both editors: main.rs has two padded lines, util.rs one.
    #[test]
    fn diagnostics_land_in_both_documents() {
        let app = booted();
        assert_eq!(diagnostics(&app, MAIN), 2, "main.rs shows both trailing-whitespace warnings");
        assert_eq!(diagnostics(&app, UTIL), 1, "util.rs shows its trailing-whitespace warning");
    }

    /// F12 on a call in main.rs lands in util.rs: that tab becomes active with the function's
    /// name selected. This is the cross-document path, `apply_lsp` → `Jump::Open` → `jump`.
    #[test]
    fn f12_switches_tabs_and_selects_the_definition() {
        let mut app = booted();
        let call = MAIN_RS.find("greet").expect("main.rs calls greet") as u32;
        press(&mut app, MAIN, Event::Editor(Action::PlaceCaret(call + 1)));
        press(&mut app, MAIN, Event::Editor(Action::GotoDefinition));
        assert_eq!(app.active, UTIL, "the definition's tab is active");
        let name = (UTIL_RS.find("fn greet").expect("util.rs defines greet") + "fn ".len()) as u32;
        assert_eq!(editor(&app, UTIL).selection(), name..name + 5, "greet's name is selected");
    }

    /// F2 renames across files: both editors change, and each `didChange` reached the server,
    /// whose copy of each file matches the editor's.
    #[test]
    fn rename_changes_both_documents_and_the_server_sees_both_did_changes() {
        let mut app = booted();
        let call = MAIN_RS.find("greet").expect("main.rs calls greet") as u32;
        press(&mut app, MAIN, Event::Editor(Action::PlaceCaret(call + 1)));
        press(&mut app, MAIN, Event::Editor(Action::Rename));
        press(&mut app, MAIN, Event::RenameText("welcome".into()));
        press(&mut app, MAIN, Event::SubmitRename);
        for (id, uri) in [(MAIN, MAIN_URI), (UTIL, UTIL_URI)] {
            let text = text_of(&app, id);
            assert!(text.contains("welcome(") && !text.contains("greet("), "{uri} is renamed");
            assert_eq!(app.transport.server.text(uri), Some(text.as_str()), "the server saw {uri}'s didChange");
        }
    }

    /// Shift+Alt+F strips trailing whitespace: the whole-document reply is diffed down to the
    /// padded lines, synced back, and the server's next publish clears the warnings.
    #[test]
    fn format_strips_trailing_whitespace() {
        let mut app = booted();
        press(&mut app, MAIN, Event::Editor(Action::Format));
        let text = text_of(&app, MAIN);
        assert!(text.lines().all(|line| line == line.trim_end()), "no line of main.rs ends in whitespace");
        assert_eq!(app.transport.server.text(MAIN_URI), Some(text.as_str()), "the server saw the formatted text");
        assert_eq!(diagnostics(&app, MAIN), 0, "the warnings went with the whitespace");
    }

    /// The panel shows both directions: the client's initialize request, and the server's
    /// diagnostics notifications.
    #[test]
    fn the_traffic_panel_logs_both_directions() {
        let app = booted();
        let traffic = &app.transport.traffic;
        assert!(traffic.iter().any(|line| line.starts_with("→ initialize")), "the initialize request is logged");
        assert!(
            traffic.iter().any(|line| line == "← textDocument/publishDiagnostics"),
            "a server notification is logged",
        );
    }
}
```

`PlaceCaret(call + 1)` puts the caret inside `greet`, so the word under it is `greet` whether the
server looks left or right. Run the tests with
`cargo test -p scrive-iced --features lsp --example lsp`.

### 5. READMEs

`README.md` and `crates/scrive-iced/README.md` are **byte-identical**. Edit `README.md`, then
`cp README.md crates/scrive-iced/README.md` and check with `cmp`. The edits:

**a. The crate list.** Replace "Two crates, with the dependency pointing one way:" with "Three
crates, with every dependency pointing one way:". Append a third bullet after the `scrive-iced`
bullet:

```markdown
- **`scrive-lsp`** — a Language Server Protocol bridge with no I/O: a state
  machine that turns editor snapshots and change logs into JSON-RPC messages,
  and server messages into per-document updates. Depends on `scrive-core`
  only; `scrive-iced` pulls it in behind its `lsp` feature.
```

**b. Features.** After the "Language intelligence" bullet:

```markdown
- **Language servers** (the `lsp` feature) — one client per server, many
  documents per client: incremental sync (including undo), diagnostics,
  completion, signature help, hover, goto definition, rename across files, and
  formatting. Late replies are dropped, never applied to text that moved.
```

**c. A new section** between "Quick start" and "Examples":

~~~markdown
## Language servers

Enable the `lsp` feature and `scrive_iced::lsp` re-exports the bridge. The app
owns the transport (a child process, a socket, a web worker) and carries
`lsp::Message`s both ways; the bridge owns everything between them:

```rust
// Boot: one client per server, one open_lsp per editor.
let (mut client, initialize) = lsp::Client::builder().root(root).build();
let mut outgoing = vec![initialize];
outgoing.extend(editor.open_lsp(&mut client, &file, "rust")?);

// After every editor update: sync, and send what it returns.
let task = editor.update(event, now).map(Message::Editor);
let outgoing = editor.sync_lsp(&mut client);

// For every message from the server.
let output = client.receive(message)?;
let mut outgoing = output.messages;
for update in output.updates {
    if let lsp::Update::Document(document) = update {
        // Route to the editor whose document().doc_id() == document.doc_id().
        let applied = editor.apply_lsp(&mut client, document);
        outgoing.extend(applied.messages);
        // applied.jump: a definition in another tab — call that editor's jump().
    }
}
```

`scrive-lsp` does no I/O and builds for wasm32. `examples/lsp` runs two tabs
against a scripted in-process server and shows the traffic.
~~~

**d. Examples.** Add a line to the code block:

```bash
cargo run -p scrive-iced --features lsp --example lsp   # two tabs on a scripted language server
```

Add after the `scratch` sentence: "`lsp` shows the traffic panel next to the editor. Press F12 on
`greet`, F2 to rename it across both files, or Shift+Alt+F to format."

**e. Web.** Change the trunk block to:

```bash
cd crates/scrive-iced
trunk serve --release --example minimal   # or: --example scratch
trunk serve --release --example lsp --features lsp
```

**`crates/scrive-lsp/README.md`.** Keep Phase 4's opening. Make sure it ends up with these
sections, rewriting any Phase 4 placeholder:
- **What it covers.** Diagnostics (versioned, cached for unopened files), completion (with
  reuse and incomplete lists), signature help, hover, goto definition (`LocationLink` too), rename
  (all-or-nothing across files), formatting (diffed to the changed lines), the lifecycle
  (initialize → shutdown → exit), and server requests answered with safe defaults.
- **With scrive-iced.** Enable `scrive-iced`'s `lsp` feature. Name the five methods (`open_lsp`,
  `sync_lsp`, `apply_lsp`, `jump`, `close_lsp`) and point at `examples/lsp`.
- **Without scrive-iced.** `Client::sync(&Snapshot, Changes)`, the request types from
  `scrive_core::intel`, `update::Document::into_parts()`, `FileEdits::apply` and
  `jump::Unopened::span`.
- **What it does not do.** No transport, I/O, clock or threads. Not covered: references, code
  actions, semantic tokens and inlay hints; `prepareRename`, range and on-type formatting; file
  operations; several servers per editor.

Keep it short and factual. No version history.

### 6. Version bump to 0.4.0

In the root `Cargo.toml`:

```diff
 [workspace.package]
-version = "0.3.0"
+version = "0.4.0"
```

In `[workspace.dependencies]` (Phase 4), set every internal crate's `version = "0.4.0"`:
scrive-core and scrive-lsp. `Cargo.lock` picks up the new member versions on the next
`cargo build`. Never run `cargo update`. Afterwards:

```
grep -rn '0\.3\.0' Cargo.toml crates/*/Cargo.toml   # no output
```

The bump is a semver break: `Action` gained variants (Phase 3), and `set_*` take tickets
(Phase 2).

## Files changed

| File | Change |
|---|---|
| crates/scrive-iced/Cargo.toml | `[[example]] lsp` (path, required-features, test) |
| crates/scrive-iced/examples/lsp/main.rs | new: App, tabs, transport, view, subscription, tests |
| crates/scrive-iced/examples/lsp/server.rs | new: `server::Scripted` with canned and computed answers |
| Cargo.toml | workspace version and internal pins → 0.4.0 |
| README.md, crates/scrive-iced/README.md | crate list, feature bullet, "Language servers" section, example and trunk lines (byte-identical) |
| crates/scrive-lsp/README.md | final usage sections |
| Cargo.lock | member versions (from the build) |

## Verification

```
cargo test -p scrive-iced --features lsp --example lsp
cargo test --workspace
cargo test --workspace --all-features
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --all-features
cargo build --workspace --all-targets --all-features --target wasm32-unknown-unknown
for c in scrive-core scrive-lsp; do out=$(cargo tree -p $c -e normal --prefix none) || exit 1; if grep -qE '^(iced|winit|wgpu)' <<<"$out"; then echo "$c LEAK"; exit 1; fi; done
cmp README.md crates/scrive-iced/README.md
grep -rn '0\.3\.0' Cargo.toml crates/*/Cargo.toml   # expect no output
rustfmt --edition 2021 crates/scrive-iced/examples/lsp/main.rs crates/scrive-iced/examples/lsp/server.rs
```

Don't run `cargo run --example lsp` (it opens a window and hangs the agent) or `trunk serve`. The
manual checks for the orchestrator are:
- `cargo run -p scrive-iced --features lsp --example lsp`;
- `cd crates/scrive-iced && trunk serve --release --example lsp --features lsp`.

## Spot-check tables

### Scripted exchanges

Versions are illustrative: D10 starts the count at the first `didOpen`.

| User action | Client → server | Server → client | Result |
|---|---|---|---|
| boot | `initialize` (`rootUri: file:///demo/`) | canned `INITIALIZE` result | — |
| (initialize lands) | `initialized`, `didOpen` main.rs, `didOpen` util.rs | `publishDiagnostics` main.rs (2), util.rs (1) | squiggles on the padded lines |
| type `g` in main.rs | `didChange` (incremental, byte columns), `completion` | canned list; `publishDiagnostics` | popup with `greet` |
| accept `greet` | `didChange`, then `signatureHelp` (`signature_after`) | canned signature | box: `greet(name: &str) -> String`, `name: &str` active |
| hover `greet` | `hover` | canned `HOVER` | card: code line, prose |
| hover `message` | `hover` | `null` | nothing |
| caret in `greet`, F12 | `definition` | `{uri: util.rs, range: 1:7–1:12}` | util.rs tab active, `greet` selected |
| caret in `greet`, F2, `welcome`, Enter | `rename` (`newName: welcome`) | `documentChanges: [main.rs vN: 1 edit, util.rs vM: 1 edit]` | both files renamed; `didChange` main.rs, `didChange` util.rs; two `publishDiagnostics` |
| Shift+Alt+F in main.rs | `formatting` (`tabSize`, `insertSpaces: true`) | `[{0:0–7:0, stripped text}]` | two line hunks, each trimmed to its spaces; `didChange`; `publishDiagnostics` (0) |
| Shift+Alt+F again | `formatting` | `[]` | nothing |

### Routing in `update`'s Lsp arm

| Update | Route | Traffic note |
|---|---|---|
| `Document` for a tab's `DocId` | `tab.editor.apply_lsp` | `refused: …` if refused |
| `Document` for no tab | skipped | — |
| `applied.jump = Open(o)` | `target.editor.jump(client, o)`, `active = target.id` | `jump refused: …` on `Err` |
| `applied.jump = Unopened(u)` | — | `definition in unopened …` |
| `FileEdits(e)` | — | `edits for unopened …` |
| `Notification(n)` | — | the notification |
| `receive` → `Err(e)` | — | `error: …` |

## What NOT to change

- No library code in scrive-core, scrive-lsp or scrive-iced `src/`. If the example needs an API
  that doesn't exist, stop and report it.
- No edits to `minimal.rs`, `scratch.rs`, `record_showcase.rs`, `shared/` or `index.html`.
- Don't make `lsp` a default feature, and add no dependencies.
- CI stays as Phase 9 left it: `--all-features` already covers the example.
- The READMEs keep their existing sections and wording outside the listed edits. No changelog
  file (none exists).

## Pitfalls

- **`Subscription::map` rejects capturing closures at compile time.** A `const` check panics
  with "not non-capturing", so `.map(Message::Editor.with(id))` fails on a subscription. Use
  `.with(id).map(|(id, event)| Message::Editor(id, event))`. `tab::Id` must be
  `Hash + Clone + Send + Sync`. `Task::map` and `Element::map` do take capturing closures, so
  `.with(id)` is right there (iced's `Function` trait: `use iced::Function;`).
- **Borrows in `update`.**
  - `let Self { client, tabs, active, transport } = self;` gives disjoint `&mut` borrows, so
    `tab.editor.apply_lsp(client, …)` compiles while `tab` borrows `tabs`.
  - The second `tabs.iter_mut()` for the jump target is fine, because `tab` isn't used after
    `apply_lsp`.
  - Don't call a `&mut self` method while any binding is live. The `Deliver` arm's
    `self.update(…)` compiles only because no binding is used after `transport.next()` (checked
    with rustc).
- **`#[must_use]`.** `sync_lsp`, `apply_lsp`'s `Applied` and `jump`'s messages all go into
  `outgoing`. Never `let _ =` them in the app. In tests, `let _ = app.update(..)` drops a `Task`
  on purpose, because the tests pump `Deliver` themselves.
- **Dead code in the example.** `Scripted::text` is test-only, so it's `#[cfg(test)]`. clippy
  `--all-targets` builds the example both as a binary and as a test harness (`test = true`), so a
  helper used only by tests fails `-D warnings` in the binary build. Don't add a `new()` without a
  caller: `Scripted` and `Transport` derive `Default`.
- **wasm32.** `--all-targets --all-features` builds the example and its tests for
  `wasm32-unknown-unknown`. So:
  - no `std::fs`, `std::thread`, `std::env` or `std::time`;
  - `Instant` comes from `iced::time`;
  - the traffic panel uses `scrive_iced::DEFAULT_FONT`, not `Font::MONOSPACE`, which has no
    system font in a browser;
  - fonts come from `required_fonts()`.
- **Paths.** `mod server;` needs no `#[path]`. The grammar is `../assets/…` from `examples/lsp/`.
- **Raw-string JSON.** The canned payloads use `r#"…"#`. None may contain `"#`. The hover's
  fence and backticks are fine.
- **Clippy on the `Message` enum.** If `large_enum_variant` fires (`lsp::Message` against
  `Event`), box the larger payload in the variant, not the whole enum, and say so in your report.
- **READMEs drift.** Edit one and copy it. Don't hand-edit both.
- **Never run `cargo fmt`.** Only `rustfmt --edition 2021` on the two files this phase creates.
