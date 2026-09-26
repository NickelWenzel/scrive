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
        for (index, (title, path, source)) in [
            ("main.rs", MAIN_URI, MAIN_RS),
            ("util.rs", UTIL_URI, UTIL_RS),
        ]
        .into_iter()
        .enumerate()
        {
            let mut editor = CodeEditor::new(source).language(rust()).rename(true);
            // Before the handshake completes, this only records the text. The didOpen goes
            // out with `initialized`.
            outgoing.extend(
                editor
                    .open_lsp(&mut client, &uri(path), "rust")
                    .expect("the demo's URIs are distinct"),
            );
            tabs.push(Tab {
                id: tab::Id(index),
                title,
                editor,
            });
        }
        let mut transport = Transport::default();
        let task = transport.send(outgoing);
        (
            Self {
                client,
                tabs,
                active: tab::Id(0),
                transport,
            },
            task,
        )
    }

    /// `now` is the instant iced stamps on the message (`iced::application::timed`).
    fn update(&mut self, message: Message, now: Instant) -> Task<Message> {
        let Self {
            client,
            tabs,
            active,
            transport,
        } = self;
        match message {
            Message::Editor(id, event) => {
                let Some(tab) = tabs.iter_mut().find(|tab| tab.id == id) else {
                    return Task::none();
                };
                let task = tab.editor.update(event, now).map(Message::Editor.with(id));
                let outgoing = tab.editor.sync_lsp(client);
                Task::batch([task, transport.send(outgoing)])
            }
            Message::Lsp(message) => {
                let output = match client.receive(message) {
                    Ok(output) => output,
                    Err(error) => {
                        transport.note(format!("error: {error}"));
                        // The replies queued behind this one still need a delivery.
                        return transport.send(Vec::new());
                    }
                };
                let mut outgoing = output.messages;
                for update in output.updates {
                    match update {
                        lsp::Update::Document(document) => {
                            let Some(tab) = tabs
                                .iter_mut()
                                .find(|tab| tab.editor.document().doc_id() == document.doc_id())
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
                                    let Some(target) = tabs.iter_mut().find(|tab| {
                                        tab.editor.document().doc_id() == open.doc_id()
                                    }) else {
                                        continue;
                                    };
                                    match target.editor.jump(client, open) {
                                        Ok(messages) => {
                                            outgoing.extend(messages);
                                            *active = target.id;
                                        }
                                        Err(refusal) => {
                                            transport.note(format!("jump refused: {refusal}"))
                                        }
                                    }
                                }
                                // A host with files would read this one, open a tab, `open_lsp`
                                // it, then `select(unopened.span(&text))`. Every demo file is
                                // open, and wasm has no disk.
                                Some(lsp::update::Jump::Unopened(unopened)) => {
                                    transport.note(format!(
                                        "definition in unopened {}",
                                        unopened.uri().as_str()
                                    ));
                                }
                                None => {}
                            }
                        }
                        // A host with files writes `edits.apply(&disk_text)` back to disk.
                        lsp::Update::FileEdits(edits) => {
                            transport.note(format!("edits for unopened {}", edits.uri().as_str()));
                        }
                        lsp::Update::Notification(notification) => {
                            transport.note(format!("{notification:?}"))
                        }
                    }
                }
                transport.send(outgoing)
            }
            Message::Deliver => match transport.next() {
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
        let body = row![
            container(editor).width(FillPortion(3)).height(Fill),
            traffic(&self.transport.traffic)
        ]
        .spacing(8);
        column![tabs, body].spacing(8).padding(8).into()
    }

    /// Only the active tab listens: global chords (Ctrl+F) must reach one editor.
    fn subscription(&self) -> Subscription<Message> {
        match self.tabs.iter().find(|tab| tab.id == self.active) {
            // `Subscription::map` takes only non-capturing closures, so the id rides along
            // through `with`.
            Some(tab) => tab
                .editor
                .subscription()
                .with(tab.id)
                .map(|(id, event)| Message::Editor(id, event)),
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
        if self.server.is_idle() {
            Task::none()
        } else {
            Task::done(Message::Deliver)
        }
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
    let style = if tab.id == active {
        button::primary
    } else {
        button::secondary
    };
    button(text(tab.title))
        .style(style)
        .on_press(Message::Select(tab.id))
        .into()
}

/// The traffic panel: newest at the bottom, pinned there as lines arrive.
fn traffic(lines: &VecDeque<String>) -> Element<'_, Message> {
    let lines =
        column(lines.iter().map(|line| {
            Element::from(text(line.as_str()).size(12).font(scrive_iced::DEFAULT_FONT))
        }));
    container(scrollable(lines).anchor_bottom().width(Fill).height(Fill))
        .width(FillPortion(2))
        .height(Fill)
        .into()
}

fn uri(text: &str) -> lsp::lsp_types::Uri {
    text.parse().expect("the demo's URIs parse")
}

fn rust() -> SyntaxDef {
    SyntaxDef::from_sublime_syntax(include_str!("../assets/rust.sublime-syntax"))
        .expect("bundled Rust grammar parses")
}

fn theme(_app: &App) -> Theme {
    Theme::Dark
}

fn main() -> iced::Result {
    // `timed` hands `update` each message's instant, which the editor's debounces run on.
    let app = iced::application::timed(App::new, App::update, App::subscription, App::view)
        .title("scrive — lsp")
        .theme(theme);
    app.fonts(scrive_iced::required_fonts().iter().copied())
        .run()
}

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
        &app.tabs
            .iter()
            .find(|tab| tab.id == id)
            .expect("the demo has this tab")
            .editor
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
        assert_eq!(
            diagnostics(&app, MAIN),
            2,
            "main.rs shows both trailing-whitespace warnings"
        );
        assert_eq!(
            diagnostics(&app, UTIL),
            1,
            "util.rs shows its trailing-whitespace warning"
        );
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
        assert_eq!(
            editor(&app, UTIL).selection(),
            name..name + 5,
            "greet's name is selected"
        );
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
            assert!(
                text.contains("welcome(") && !text.contains("greet("),
                "{uri} is renamed"
            );
            assert_eq!(
                app.transport.server.text(uri),
                Some(text.as_str()),
                "the server saw {uri}'s didChange"
            );
        }
    }

    /// Shift+Alt+F strips trailing whitespace: the whole-document reply is diffed down to the
    /// padded lines, synced back, and the server's next publish clears the warnings.
    #[test]
    fn format_strips_trailing_whitespace() {
        let mut app = booted();
        press(&mut app, MAIN, Event::Editor(Action::Format));
        let text = text_of(&app, MAIN);
        assert!(
            text.lines().all(|line| line == line.trim_end()),
            "no line of main.rs ends in whitespace"
        );
        assert_eq!(
            app.transport.server.text(MAIN_URI),
            Some(text.as_str()),
            "the server saw the formatted text"
        );
        assert_eq!(
            diagnostics(&app, MAIN),
            0,
            "the warnings went with the whitespace"
        );
    }

    /// The panel shows both directions: the client's initialize request, and the server's
    /// diagnostics notifications.
    #[test]
    fn the_traffic_panel_logs_both_directions() {
        let app = booted();
        let traffic = &app.transport.traffic;
        assert!(
            traffic.iter().any(|line| line.starts_with("→ initialize")),
            "the initialize request is logged"
        );
        assert!(
            traffic
                .iter()
                .any(|line| line == "← textDocument/publishDiagnostics"),
            "a server notification is logged",
        );
    }
}
