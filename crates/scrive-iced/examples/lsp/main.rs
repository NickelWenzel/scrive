//! `lsp` — two editors on one language-server client, against a scripted server.
//!
//! ```text
//! cargo run -p scrive-iced --features lsp --example lsp
//! ```
//!
//! The server (`server.rs`) runs in-process on the memory bridge, so there's no process to
//! start. It answers `initialize`, completion, signature help and hover with canned JSON, and
//! computes diagnostics (trailing whitespace), goto definition, rename, formatting and inlay
//! hints from the text the client sent it. The right-hand panel shows the client's traces. Try:
//!
//! - F12 on `greet` in main.rs: the util.rs tab opens with the definition selected;
//! - F2 on `greet`, type a new name, Enter: both files change;
//! - Shift+Alt+F: the trailing whitespace goes, and so do its warnings;
//! - typing, `(`, and hovering `greet`: the canned completion, signature and hover;
//! - Ctrl+click `name` in the parameter hint: the util.rs tab opens with the parameter selected;
//! - Ctrl+click `String` in the type hint: the panel notes the jump into an unopened file;
//! - double-click the type hint: `: String` is inserted, and the hint goes;
//! - hovering a hint: its tooltip, resolved by the server;
//! - Ctrl+I (Cmd+I on macOS): hints off and on.
//!
//! The wiring is the one a real host uses: run the client's event stream, sync after every editor
//! update, route each `Update::Document` to the tab that owns it, and hand a cross-file jump to
//! the target tab.

// On Windows, a release build is a GUI app with no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod server;

use std::collections::VecDeque;

use iced::keyboard::{self, Key};
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

/// The application: two tabs on one language-server client, the in-process server, and the
/// traffic log.
struct App {
    client: lsp::Client,
    tabs: Vec<Tab>,
    active: tab::Id,
    server: server::Scripted,
    traffic: Traffic,
    /// Whether inlay hints show; Ctrl+I flips it for every tab.
    hints: bool,
}

#[derive(Debug, Clone)]
enum Message {
    /// A message from one tab's editor.
    Editor(tab::Id, Event),
    /// An event from the client's stream.
    Lsp(lsp::client::Event),
    /// A tab button was pressed.
    Select(tab::Id),
    /// Ctrl+I: turn inlay hints off or on.
    ToggleHints,
}

/// The traffic panel's lines: the client's traces, and notes on what the app did.
#[derive(Default)]
struct Traffic {
    lines: VecDeque<String>,
}

impl App {
    /// The app and its client's event stream, before anything runs it. Tests drain the stream
    /// themselves.
    fn boot() -> (Self, lsp::client::Events) {
        let (near, far) = lsp::lsp_server::Connection::memory();
        let (mut client, events) = lsp::Client::builder()
            .root(uri(ROOT))
            .trace(lsp::trace::Mode::Messages)
            .memory(near);
        let mut tabs = Vec::new();
        for (index, (title, path, source)) in [
            ("main.rs", MAIN_URI, MAIN_RS),
            ("util.rs", UTIL_URI, UTIL_RS),
        ]
        .into_iter()
        .enumerate()
        {
            let mut editor = CodeEditor::new(source)
                .language(rust())
                .rename(true)
                .inlay_hints(true);
            // Before the handshake completes, this only records the text. The didOpen goes
            // out with `initialized`.
            editor
                .open_lsp(&mut client, &uri(path), "rust")
                .expect("the demo's URIs are distinct");
            tabs.push(Tab {
                id: tab::Id(index),
                title,
                editor,
            });
        }
        let mut server = server::Scripted::new(far);
        server.step();
        let app = Self {
            client,
            tabs,
            active: tab::Id(0),
            server,
            traffic: Traffic::default(),
            hints: true,
        };
        (app, events)
    }

    fn new() -> (Self, Task<Message>) {
        let (app, events) = Self::boot();
        (app, Task::run(events, Message::Lsp))
    }

    /// `now` is the instant iced stamps on the message (`iced::application::timed`). The
    /// server answers whatever the update sent before it returns.
    fn update(&mut self, message: Message, now: Instant) -> Task<Message> {
        let Self {
            client,
            tabs,
            active,
            server,
            traffic,
            hints,
        } = self;
        let task = match message {
            Message::Editor(id, event) => {
                let Some(tab) = tabs.iter_mut().find(|tab| tab.id == id) else {
                    return Task::none();
                };
                let task = tab.editor.update(event, now).map(Message::Editor.with(id));
                let synced = tab.editor.sync_lsp(client);
                follow(tabs, active, client, traffic, synced.jump);
                task
            }
            Message::Lsp(event) => {
                for update in client.receive(event) {
                    match update {
                        lsp::Update::Document(document) => {
                            let Some(tab) = tabs
                                .iter_mut()
                                .find(|tab| tab.editor.document().doc_id() == document.doc_id())
                            else {
                                continue;
                            };
                            let applied = tab.editor.apply_lsp(client, document);
                            if let Some(refusal) = applied.refused {
                                traffic.note(format!("refused: {refusal}"));
                            }
                            follow(tabs, active, client, traffic, applied.jump);
                        }
                        // A host with files writes `edits.apply(&disk_text)` back to disk.
                        lsp::Update::FileEdits(edits) => {
                            traffic.note(format!("edits for unopened {}", edits.uri().as_str()));
                        }
                        lsp::Update::Notification(notification) => {
                            traffic.note(format!("{notification:?}"));
                        }
                        lsp::Update::Trace(entry) => traffic.trace(&entry),
                        lsp::Update::Log(entry) => traffic.note(format!("log: {}", entry.text())),
                        lsp::Update::Error(error) => traffic.note(format!("error: {error}")),
                        lsp::Update::Status(status) => traffic.note(format!("status: {status:?}")),
                    }
                }
                Task::none()
            }
            Message::Select(id) => {
                *active = id;
                Task::none()
            }
            // Turning hints on schedules a fetch per tab, which the shown tab's widget wakes.
            Message::ToggleHints => {
                *hints = !*hints;
                for tab in tabs.iter_mut() {
                    tab.editor.set_inlay_hints(*hints);
                }
                traffic.note(format!("inlay hints {}", if *hints { "on" } else { "off" }));
                Task::none()
            }
        };
        server.step();
        task
    }

    fn view(&self) -> Element<'_, Message> {
        let tabs = row(self.tabs.iter().map(|tab| tab_button(tab, self.active))).spacing(4);
        let editor = match self.tabs.iter().find(|tab| tab.id == self.active) {
            Some(tab) => tab.editor.view().map(Message::Editor.with(tab.id)),
            None => text("no tab").into(),
        };
        let body = row![
            container(editor).width(FillPortion(3)).height(Fill),
            traffic(&self.traffic.lines)
        ]
        .spacing(8);
        column![tabs, body].spacing(8).padding(8).into()
    }

    /// Only the active tab listens, so global chords (Ctrl+F) reach one editor; Ctrl+I reaches
    /// the app.
    fn subscription(&self) -> Subscription<Message> {
        let editor = match self.tabs.iter().find(|tab| tab.id == self.active) {
            // `Subscription::map` takes only non-capturing closures, so the id rides along
            // through `with`.
            Some(tab) => tab
                .editor
                .subscription()
                .with(tab.id)
                .map(|(id, event)| Message::Editor(id, event)),
            None => Subscription::none(),
        };
        Subscription::batch([editor, keyboard::listen().filter_map(toggle_chord)])
    }
}

impl Traffic {
    /// A line of commentary: refusals, errors, and what a disk-backed host would do.
    fn note(&mut self, note: String) {
        self.push(format!("· {note}"));
    }

    /// A traced message as one line: `→ method #id`, `← method`, `← response #id`.
    fn trace(&mut self, entry: &lsp::trace::Entry) {
        let arrow = match entry.direction() {
            lsp::trace::Direction::Outgoing => "→",
            lsp::trace::Direction::Incoming => "←",
        };
        let wire: Value = serde_json::from_slice(entry.json()).unwrap_or(Value::Null);
        let what = match (wire.get("method").and_then(Value::as_str), wire.get("id")) {
            (Some(method), Some(id)) => format!("{method} #{id}"),
            (Some(method), None) => method.to_owned(),
            (None, Some(id)) => format!("response #{id}"),
            (None, None) => "?".to_owned(),
        };
        self.push(format!("{arrow} {what}"));
    }

    fn push(&mut self, line: String) {
        if self.lines.len() == TRAFFIC_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }
}

/// Route `jump` to the tab whose document holds it, and so on for any jump that tab's sync
/// returns. A host with files would read an unopened one, open a
/// tab, `open_lsp` it, then `select(unopened.span(&text))`; every demo file is open, and wasm
/// has no disk, so it is only noted.
fn follow(
    tabs: &mut [Tab],
    active: &mut tab::Id,
    client: &mut lsp::Client,
    traffic: &mut Traffic,
    mut jump: Option<lsp::update::Jump>,
) {
    while let Some(next) = jump.take() {
        match next {
            lsp::update::Jump::Open(open) => {
                let Some(target) = tabs
                    .iter_mut()
                    .find(|tab| tab.editor.document().doc_id() == open.doc_id())
                else {
                    break;
                };
                match target.editor.jump(client, open) {
                    Ok(applied) => {
                        jump = applied.jump;
                        *active = target.id;
                    }
                    Err(refusal) => traffic.note(format!("jump refused: {refusal}")),
                }
            }
            lsp::update::Jump::Unopened(unopened) => {
                traffic.note(format!("definition in unopened {}", unopened.uri().as_str()));
            }
        }
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

/// Ctrl+I, or Cmd+I on macOS, without Shift or Alt and not repeated. The editor binds no Ctrl+I,
/// so the key reaches `keyboard::listen`.
fn toggle_chord(event: keyboard::Event) -> Option<Message> {
    match event {
        keyboard::Event::KeyPressed {
            key: Key::Character(c),
            modifiers,
            repeat: false,
            ..
        } if c == "i" && modifiers.command() && !modifiers.shift() && !modifiers.alt() => {
            Some(Message::ToggleHints)
        }
        _ => None,
    }
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
    use iced::keyboard::key::{Code, Physical};
    use iced::keyboard::{Location, Modifiers};
    use scrive_core::intel::inlay;
    use scrive_iced::Action;

    use super::*;

    const MAIN: tab::Id = tab::Id(0);
    const UTIL: tab::Id = tab::Id(1);

    use iced::futures::{FutureExt, StreamExt};

    /// Step the server and drain the client's stream, as the runtime would, until both are idle.
    fn settle(app: &mut App, events: &mut lsp::client::Events) {
        let now = Instant::now();
        loop {
            let stepped = app.server.step();
            let mut drained = false;
            while let Some(Some(event)) = events.next().now_or_never() {
                drained = true;
                let _ = app.update(Message::Lsp(event), now);
            }
            if !stepped && !drained {
                break;
            }
        }
    }

    /// The app after its handshake: both documents open, their first diagnostics landed.
    fn booted() -> (App, lsp::client::Events) {
        let (mut app, mut events) = App::boot();
        settle(&mut app, &mut events);
        (app, events)
    }

    /// Feed `event` to tab `id`, then let the conversation finish.
    fn press(app: &mut App, events: &mut lsp::client::Events, id: tab::Id, event: Event) {
        let _ = app.update(Message::Editor(id, event), Instant::now());
        settle(app, events);
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
        let (app, _events) = booted();
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
        let (mut app, mut events) = booted();
        let call = MAIN_RS.find("greet").expect("main.rs calls greet") as u32;
        press(&mut app, &mut events, MAIN, Event::Editor(Action::PlaceCaret(call + 1)));
        press(&mut app, &mut events, MAIN, Event::Editor(Action::GotoDefinition));
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
        let (mut app, mut events) = booted();
        let call = MAIN_RS.find("greet").expect("main.rs calls greet") as u32;
        press(&mut app, &mut events, MAIN, Event::Editor(Action::PlaceCaret(call + 1)));
        press(&mut app, &mut events, MAIN, Event::Editor(Action::Rename));
        press(&mut app, &mut events, MAIN, Event::RenameText("welcome".into()));
        press(&mut app, &mut events, MAIN, Event::SubmitRename);
        for (id, uri) in [(MAIN, MAIN_URI), (UTIL, UTIL_URI)] {
            let text = text_of(&app, id);
            assert!(
                text.contains("welcome(") && !text.contains("greet("),
                "{uri} is renamed"
            );
            assert_eq!(
                app.server.text(uri),
                Some(text.as_str()),
                "the server saw {uri}'s didChange"
            );
        }
    }

    /// Shift+Alt+F strips trailing whitespace: the whole-document reply is diffed down to the
    /// padded lines, synced back, and the server's next publish clears the warnings.
    #[test]
    fn format_strips_trailing_whitespace() {
        let (mut app, mut events) = booted();
        press(&mut app, &mut events, MAIN, Event::Editor(Action::Format));
        let text = text_of(&app, MAIN);
        assert!(
            text.lines().all(|line| line == line.trim_end()),
            "no line of main.rs ends in whitespace"
        );
        assert_eq!(
            app.server.text(MAIN_URI),
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
        let (app, _events) = booted();
        let traffic = &app.traffic.lines;
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

    /// Fire tab `id`'s scheduled hint fetch, as its widget does once the delay has passed, and
    /// let the conversation finish.
    fn fetch(app: &mut App, events: &mut lsp::client::Events, id: tab::Id) {
        let generation = editor(app, id)
            .pending_wake()
            .expect("a fetch is scheduled")
            .generation;
        press(app, events, id, Event::Editor(Action::Wake(generation)));
    }

    /// The hints tab `id` shows, as `(render offset, key)`, in offset order.
    fn hints(app: &App, id: tab::Id) -> Vec<(u32, inlay::Key)> {
        let document = editor(app, id).document();
        document
            .inlays_in(0..document.buffer().len())
            .map(|hint| (hint.offset(), hint.key()))
            .collect()
    }

    /// Where `needle` starts in `text`, as an editor offset.
    fn at(text: &str, needle: &str) -> u32 {
        let found = text
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} is in the demo"));
        u32::try_from(found).expect("the demo is small")
    }

    /// Where main.rs's type hint renders: after `message`.
    fn type_offset() -> u32 {
        at(MAIN_RS, "let message") + "let message".len() as u32
    }

    /// Where main.rs's parameter hint renders: before `"scrive"`.
    fn parameter_offset() -> u32 {
        at(MAIN_RS, "\"scrive\"")
    }

    /// main.rs after boot with its hints fetched: `(type hint key, parameter hint key)`.
    fn hinted() -> (App, lsp::client::Events, inlay::Key, inlay::Key) {
        let (mut app, mut events) = booted();
        fetch(&mut app, &mut events, MAIN);
        let shown = hints(&app, MAIN);
        let key_at = |offset: u32| {
            shown
                .iter()
                .find(|(at, _)| *at == offset)
                .unwrap_or_else(|| panic!("a hint renders at {offset}: {shown:?}"))
                .1
        };
        let (ty, parameter) = (key_at(type_offset()), key_at(parameter_offset()));
        (app, events, ty, parameter)
    }

    /// The fetch for main.rs brings exactly its two hints: the type after `message` and the
    /// parameter before `"scrive"`.
    #[test]
    fn the_hint_script_lands_in_main_rs() {
        let (app, _events, _, _) = hinted();
        let offsets: Vec<u32> = hints(&app, MAIN).iter().map(|(at, _)| *at).collect();
        assert_eq!(
            offsets,
            [type_offset(), parameter_offset()],
            "the type hint and the parameter hint show, in offset order",
        );
        assert!(
            app.traffic
                .lines
                .iter()
                .any(|line| line.starts_with("→ textDocument/inlayHint")),
            "the fetch went to the server",
        );
    }

    /// Ctrl+click on `name` in the parameter hint switches to util.rs and selects the
    /// parameter there: an `Open` label jump routed through the editor arm.
    #[test]
    fn ctrl_click_on_a_parameter_hint_opens_util_rs_at_the_parameter() {
        let (mut app, mut events, _, parameter) = hinted();
        press(
            &mut app,
            &mut events,
            MAIN,
            Event::Editor(Action::InlayJump {
                key: parameter,
                part: 0,
            }),
        );
        assert_eq!(app.active, UTIL, "util.rs is active");
        let name = at(UTIL_RS, "(name") + 1;
        assert_eq!(
            editor(&app, UTIL).selection(),
            name..name + 4,
            "greet's parameter is selected",
        );
    }

    /// Ctrl+click on `String` in the type hint jumps into a file no tab holds: `sync_lsp`
    /// returns the jump, with no request to the server, and the panel notes it.
    #[test]
    fn ctrl_click_on_a_type_hint_jumps_into_an_unopened_file_through_sync_lsp() {
        let (mut app, mut events, ty, _) = hinted();
        press(
            &mut app,
            &mut events,
            MAIN,
            Event::Editor(Action::InlayJump { key: ty, part: 1 }),
        );
        let traffic = &app.traffic.lines;
        assert!(
            traffic
                .iter()
                .any(|line| line == "· definition in unopened file:///demo/std/string.rs"),
            "the jump into the standard library is noted",
        );
        assert!(
            !traffic
                .iter()
                .any(|line| line.starts_with("→ textDocument/definition")),
            "no definition request goes out",
        );
        assert_eq!(app.active, MAIN, "main.rs stays active");
    }

    /// Double-clicking the type hint inserts `: String` once and removes the hint; the server
    /// sees the edit in the same press, and the refetch brings back only the parameter hint.
    #[test]
    fn double_click_inserts_the_type_once_and_leaves_no_duplicate_hint() {
        let (mut app, mut events, ty, _) = hinted();
        let offset = type_offset();
        press(
            &mut app,
            &mut events,
            MAIN,
            Event::Editor(Action::InlayInsert { key: ty, offset }),
        );
        let text = text_of(&app, MAIN);
        let mut expected = MAIN_RS.to_owned();
        expected.insert_str(offset as usize, ": String");
        assert_eq!(text, expected, "the type is inserted at the hint");
        assert_eq!(text.matches(": String").count(), 1, "the type is written once");
        let shown = hints(&app, MAIN);
        assert!(
            shown.iter().all(|(at, key)| *key != ty && *at != offset),
            "the inserted hint is gone: {shown:?}",
        );
        assert_eq!(
            app.server.text(MAIN_URI),
            Some(text.as_str()),
            "the insert's didChange went out with the press",
        );
        fetch(&mut app, &mut events, MAIN);
        let offsets: Vec<u32> = hints(&app, MAIN).iter().map(|(at, _)| *at).collect();
        assert_eq!(
            offsets,
            [at(&text, "\"scrive\"")],
            "only the parameter hint comes back",
        );
    }

    /// Hovering the type hint asks the server to resolve its tooltip, and the answer lands.
    #[test]
    fn hovering_a_hint_resolves_its_tooltip() {
        let (mut app, mut events, ty, _) = hinted();
        press(
            &mut app,
            &mut events,
            MAIN,
            Event::Editor(Action::InlayHover { key: ty, part: 1 }),
        );
        let traffic: Vec<&String> = app.traffic.lines.iter().collect();
        let resolve = traffic
            .iter()
            .position(|line| line.starts_with("→ inlayHint/resolve"))
            .expect("the tooltip is resolved by the server");
        assert!(
            !traffic[resolve..]
                .iter()
                .any(|line| line.starts_with("· refused")),
            "the resolved tooltip lands",
        );
    }

    /// Ctrl+I clears every tab's hints, and a second Ctrl+I brings them back after a fetch.
    #[test]
    fn the_toggle_key_clears_and_restores_the_hints() {
        let (mut app, mut events, _, _) = hinted();
        let now = Instant::now();
        let _ = app.update(Message::ToggleHints, now);
        assert!(!app.hints, "hints are off");
        assert!(hints(&app, MAIN).is_empty(), "main.rs shows no hints");
        let _ = app.update(Message::ToggleHints, now);
        fetch(&mut app, &mut events, MAIN);
        assert!(app.hints, "hints are on");
        assert_eq!(hints(&app, MAIN).len(), 2, "both hints are back");
    }

    /// Only a plain Ctrl+I (Cmd+I on macOS) toggles the hints.
    #[test]
    fn toggle_chord_matches_only_plain_ctrl_i() {
        let press = |modifiers: Modifiers, repeat: bool| keyboard::Event::KeyPressed {
            key: Key::Character("i".into()),
            modified_key: Key::Character("i".into()),
            physical_key: Physical::Code(Code::KeyI),
            location: Location::Standard,
            modifiers,
            text: None,
            repeat,
        };
        assert!(
            matches!(
                toggle_chord(press(Modifiers::COMMAND, false)),
                Some(Message::ToggleHints)
            ),
            "Ctrl+I toggles",
        );
        for (modifiers, repeat, chord) in [
            (Modifiers::COMMAND | Modifiers::SHIFT, false, "Ctrl+Shift+I"),
            (Modifiers::COMMAND | Modifiers::ALT, false, "Ctrl+Alt+I"),
            (Modifiers::COMMAND, true, "a repeated Ctrl+I"),
            (Modifiers::empty(), false, "a plain i"),
        ] {
            assert!(
                toggle_chord(press(modifiers, repeat)).is_none(),
                "{chord} does not toggle",
            );
        }
    }
}
