//! `rust_analyzer` — one editor on a real rust-analyzer, over stdio.
//!
//! ```text
//! cargo run -p scrive-iced --features lsp --example rust_analyzer
//! cargo run -p scrive-iced --features lsp --example rust_analyzer -- path/to/file.rs
//! ```
//!
//! Native only, and rust-analyzer must be on `PATH` (`rustup component add rust-analyzer`).
//! With a path, the workspace root is the nearest directory above it holding a `Cargo.toml`.
//! Without one, a scratch crate is written to the system temp directory and its `src/main.rs`
//! opened: it has a type error, a function to hover and F12 to, and a call to retype for
//! signature help. Ctrl+S (Cmd+S on macOS) writes the file and tells the server, which re-runs
//! `cargo check`: the type error's squiggle updates only then.
//!
//! Inlay hints are on: double-click a type hint to insert it, Ctrl+click a part to jump, hover
//! one for its tooltip. Ctrl+I (Cmd+I on macOS) turns them off and on.
//!
//! The update loop is the one `examples/lsp` uses, cut down to one editor. The server runs as a
//! child process started by `Builder::stdio`, which owns its pipes, threads and shutdown, and
//! starts it again if it crashes. The status bar shows where the server stands, and the line
//! under it the server's latest log line. Closing the window shuts the server down first.

#[cfg(not(target_arch = "wasm32"))]
fn main() -> iced::Result {
    let workspace = match app::Workspace::from_args() {
        Ok(workspace) => workspace,
        Err(error) => {
            eprintln!("rust_analyzer: {error}");
            std::process::exit(1);
        }
    };
    app::run(workspace)
}

#[cfg(target_arch = "wasm32")]
fn main() {
    eprintln!("the rust_analyzer example spawns a process, so it needs a native target");
}

/// The application: one editor and its client.
#[cfg(not(target_arch = "wasm32"))]
mod app {
    use std::io;
    use std::path::{Path, PathBuf};

    use iced::keyboard::{self, Key};
    use iced::time::Instant;
    use iced::widget::{button, column, container, row, text};
    use iced::{Element, Fill, Subscription, Task, Theme};
    use serde_json::json;

    use scrive_core::SyntaxDef;
    use scrive_iced::{lsp, CodeEditor, Event};

    /// The command that starts the server, looked up on `PATH`.
    const SERVER: &str = "rust-analyzer";

    /// Where the scratch crate goes when no file is given, under the system temp directory.
    const SCRATCH: &str = "scrive-rust-analyzer-example";

    const SCRATCH_MANIFEST: &str =
        "[package]\nname = \"scratch\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n";

    const SCRATCH_MAIN: &str = r#"/// Adds two numbers. Hover `add`, or press F12 on a call to it.
fn add(left: i32, right: i32) -> i32 {
    left + right
}

fn main() {
    let sum = add(1, 2);
    // Retype the `(` of this call for signature help.
    let doubled = add(sum, sum);
    // rust-analyzer flags this: an `i32` is not a `String`.
    let label: String = doubled;
    println!("{label}");
}
"#;

    /// The file to edit and the crate it belongs to.
    #[derive(Debug, Clone)]
    pub struct Workspace {
        root_uri: lsp::lsp_types::Uri,
        file: PathBuf,
        file_uri: lsp::lsp_types::Uri,
        text: String,
    }

    struct App {
        editor: CodeEditor,
        client: lsp::Client,
        /// Where Ctrl+S writes the document.
        file: PathBuf,
        /// The one-line status bar.
        status: String,
        /// The server's latest stderr or `window/logMessage` line.
        log: String,
        /// Set by the close request: the window closes once the server has shut down.
        closing: bool,
        /// Whether rust-analyzer runs `cargo check` on save; the button flips it.
        check_on_save: bool,
        /// Whether inlay hints show; Ctrl+I flips it.
        hints: bool,
    }

    #[derive(Debug, Clone)]
    enum Message {
        /// A message from the editor.
        Editor(Event),
        /// Ctrl+S: write the document to its file and tell the server.
        Save,
        /// Ctrl+I: turn inlay hints off or on.
        ToggleHints,
        /// The button: turn `cargo check` on save off or on, reconfiguring the server.
        ToggleCheck,
        /// An event from the client's stream.
        Lsp(lsp::client::Event),
        /// The window's close button: shut the server down first.
        CloseRequested,
    }

    impl Workspace {
        /// The file named by the first argument, or the scratch crate when there is none.
        pub fn from_args() -> io::Result<Self> {
            match std::env::args_os().nth(1) {
                Some(file) => Self::open(Path::new(&file)),
                None => Self::scratch(&std::env::temp_dir().join(SCRATCH)),
            }
        }

        /// `file`, in the crate of the nearest directory above it that holds a `Cargo.toml`.
        fn open(file: &Path) -> io::Result<Self> {
            let file = std::path::absolute(file)?;
            let text = std::fs::read_to_string(&file).map_err(|error| {
                io::Error::new(error.kind(), format!("{}: {error}", file.display()))
            })?;
            let root = file
                .ancestors()
                .skip(1)
                .find(|dir| dir.join("Cargo.toml").is_file())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("no Cargo.toml above {}", file.display()),
                    )
                })?;
            let uri = |path: &Path| {
                lsp::uri::from_path(path).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("{} is not UTF-8", path.display()),
                    )
                })
            };
            Ok(Self {
                root_uri: uri(root)?,
                file_uri: uri(&file)?,
                file,
                text,
            })
        }

        /// Writes the scratch crate into `root`, replacing an earlier one, and opens its
        /// `src/main.rs`.
        fn scratch(root: &Path) -> io::Result<Self> {
            std::fs::create_dir_all(root.join("src"))?;
            std::fs::write(root.join("Cargo.toml"), SCRATCH_MANIFEST)?;
            std::fs::write(root.join("src/main.rs"), SCRATCH_MAIN)?;
            Self::open(&root.join("src/main.rs"))
        }

        fn file_uri(&self) -> &lsp::lsp_types::Uri {
            &self.file_uri
        }

        fn root_uri(&self) -> &lsp::lsp_types::Uri {
            &self.root_uri
        }

        fn text(&self) -> &str {
            &self.text
        }
    }

    impl App {
        /// The app on a freshly started server, and its client's event stream, before anything
        /// runs it. Tests drive the stream themselves.
        fn boot(
            workspace: &Workspace,
        ) -> Result<(Self, lsp::client::Events), lsp::client::builder::Error> {
            let (mut client, events) = lsp::Client::builder()
                .root(workspace.root_uri().clone())
                .configuration(json!({ "rust-analyzer": { "checkOnSave": true } }))
                .stdio(std::process::Command::new(SERVER))?;
            let mut editor = CodeEditor::new(workspace.text())
                .language(rust())
                .rename(true)
                .inlay_hints(true);
            // Before the handshake this only records the text; the didOpen follows `initialized`.
            editor
                .open_lsp(&mut client, workspace.file_uri(), "rust")
                .expect("the client has no other document");
            let app = Self {
                editor,
                client,
                file: workspace.file.clone(),
                status: format!("starting {SERVER}…"),
                log: String::new(),
                closing: false,
                check_on_save: true,
                hints: true,
            };
            Ok((app, events))
        }

        /// `now` is the instant iced stamps on the message (`iced::application::timed`).
        fn update(&mut self, message: Message, now: Instant) -> Task<Message> {
            let Self {
                editor,
                client,
                file,
                status,
                log,
                closing,
                check_on_save,
                hints,
            } = self;
            match message {
                Message::Editor(event) => {
                    let task = editor.update(event, now).map(Message::Editor);
                    let synced = editor.sync_lsp(client);
                    if let Some(line) = jumped(synced.jump) {
                        *status = line;
                    }
                    task
                }
                Message::Save => {
                    let doc = editor.document();
                    // The buffer holds LF; a CRLF file is written back as CRLF.
                    match std::fs::write(&*file, doc.serialize(doc.buffer().eol_flavor())) {
                        Ok(()) => {
                            let saved = editor.save_lsp(client);
                            if let Some(line) = jumped(saved.jump) {
                                *status = line;
                            }
                            let name = file.file_name().unwrap_or(file.as_os_str()).display();
                            let checking =
                                *check_on_save && client.status() == &lsp::client::Status::Running;
                            *status = if checking {
                                format!("saved {name} — cargo check running…")
                            } else {
                                format!("saved {name}")
                            };
                        }
                        Err(error) => {
                            *status = format!("could not save {}: {error}", file.display());
                        }
                    }
                    Task::none()
                }
                Message::ToggleHints => {
                    *hints = !*hints;
                    editor.set_inlay_hints(*hints);
                    *status = format!("inlay hints {}", if *hints { "on" } else { "off" });
                    Task::none()
                }
                Message::ToggleCheck => {
                    *check_on_save = !*check_on_save;
                    client.configure(json!({ "rust-analyzer": { "checkOnSave": *check_on_save } }));
                    *status = format!(
                        "check on save {}",
                        if *check_on_save { "on" } else { "off" }
                    );
                    Task::none()
                }
                Message::Lsp(event) => {
                    for update in client.receive(event) {
                        if let Some(line) = headline(&update) {
                            *status = line;
                            continue;
                        }
                        match update {
                            lsp::Update::Document(document) => {
                                let applied = editor.apply_lsp(client, document);
                                if let Some(refusal) = applied.refused {
                                    *status = format!("refused: {refusal}");
                                }
                                if let Some(line) = jumped(applied.jump) {
                                    *status = line;
                                }
                            }
                            lsp::Update::FileEdits(edits) => {
                                *status =
                                    format!("rename skipped {}, which is not open", edits.uri());
                            }
                            lsp::Update::Status(lsp::client::Status::Stopped(
                                lsp::client::Reason::Shutdown,
                            )) if *closing => return iced::exit(),
                            // A dead server's progress never ends, so every status replaces
                            // the progress headline.
                            lsp::Update::Status(other) => *status = described(&other),
                            lsp::Update::Error(error) => *status = format!("error: {error}"),
                            lsp::Update::Log(entry) => {
                                if let Some(line) = entry.text().lines().next() {
                                    *log = line.to_owned();
                                }
                            }
                            lsp::Update::Notification(_) | lsp::Update::Trace(_) => {}
                        }
                    }
                    Task::none()
                }
                Message::CloseRequested => {
                    let shut = lsp::client::Status::Stopped(lsp::client::Reason::Shutdown);
                    if client.status() == &shut {
                        return iced::exit();
                    }
                    *closing = true;
                    for document in client.shutdown() {
                        let _ = editor.apply_lsp(client, document);
                    }
                    Task::none()
                }
            }
        }

        fn view(&self) -> Element<'_, Message> {
            let status = text(self.status.as_str())
                .size(12)
                .font(scrive_iced::DEFAULT_FONT);
            let check = if self.check_on_save {
                "check on save: on"
            } else {
                "check on save: off"
            };
            let check = button(text(check).size(12)).on_press(Message::ToggleCheck);
            let log = text(self.log.as_str())
                .size(12)
                .font(scrive_iced::DEFAULT_FONT);
            column![
                container(self.editor.view().map(Message::Editor))
                    .width(Fill)
                    .height(Fill),
                container(row![check, status].spacing(8)).padding([2, 8]),
                container(log).padding([0, 8]),
            ]
            .into()
        }

        fn subscription(&self) -> Subscription<Message> {
            Subscription::batch([
                self.editor.subscription().map(Message::Editor),
                keyboard::listen().filter_map(save_chord),
                keyboard::listen().filter_map(toggle_chord),
                iced::window::close_requests().map(|_| Message::CloseRequested),
            ])
        }
    }

    /// Starts the server, then opens the window on it. A server that does not start is reported
    /// before any window opens.
    pub fn run(workspace: Workspace) -> iced::Result {
        let title = format!("scrive — rust-analyzer — {}", workspace.file.display());
        let booted = match App::boot(&workspace) {
            Ok(booted) => booted,
            Err(error) => {
                eprintln!("rust_analyzer: {}", describe(&error));
                std::process::exit(1);
            }
        };
        // iced's boot is `Fn`, so the app moves out of a cell the one time it runs.
        let booted = std::cell::Cell::new(Some(booted));
        // `timed` hands `update` each message's instant, which the editor's debounces run on.
        iced::application::timed(
            move || {
                let (app, events) = booted.take().expect("iced boots the app once");
                (app, Task::run(events, Message::Lsp))
            },
            App::update,
            App::subscription,
            App::view,
        )
        .title(move |_: &App| title.clone())
        .theme(theme)
        .exit_on_close_request(false)
        .fonts(scrive_iced::required_fonts().iter().copied())
        .run()
    }

    /// Why the server did not start, for a person.
    fn describe(error: &lsp::client::builder::Error) -> String {
        match error {
            lsp::client::builder::Error::Spawn(spawn) if spawn.kind() == io::ErrorKind::NotFound => {
                format!(
                    "{SERVER} is not on PATH; install it with `rustup component add rust-analyzer`"
                )
            }
            lsp::client::builder::Error::Spawn(_) | lsp::client::builder::Error::Thread(_) => {
                error.to_string()
            }
        }
    }

    /// Ctrl+S, or Cmd+S on macOS, without Shift or Alt and not repeated. The editor ignores it,
    /// so it reaches `keyboard::listen`.
    fn save_chord(event: keyboard::Event) -> Option<Message> {
        match event {
            keyboard::Event::KeyPressed {
                key: Key::Character(c),
                modifiers,
                repeat: false,
                ..
            } if c == "s" && modifiers.command() && !modifiers.shift() && !modifiers.alt() => {
                Some(Message::Save)
            }
            _ => None,
        }
    }

    /// Ctrl+I, or Cmd+I on macOS, without Shift or Alt and not repeated. The editor binds no
    /// Ctrl+I, so the key reaches `keyboard::listen`.
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

    /// The status line a jump earns. A host with several editors hands `Jump::Open` to the one
    /// that owns `open.doc_id()`; here every open document is local.
    fn jumped(jump: Option<lsp::update::Jump>) -> Option<String> {
        match jump? {
            lsp::update::Jump::Open(_) => Some("definition in another open document".to_owned()),
            lsp::update::Jump::Unopened(unopened) => {
                Some(format!("definition in {}, which is not open", unopened.uri()))
            }
        }
    }

    /// The status line for where the server stands.
    fn described(status: &lsp::client::Status) -> String {
        match status {
            lsp::client::Status::Starting => format!("starting {SERVER}…"),
            lsp::client::Status::Running => format!("{SERVER} ready"),
            lsp::client::Status::Restarting { attempt } => {
                format!("{SERVER} stopped; restarting (attempt {attempt})…")
            }
            lsp::client::Status::Stopped(reason) => {
                format!("{SERVER} stopped: {}", stopped(reason))
            }
        }
    }

    /// Why the server stopped, for a person.
    fn stopped(reason: &lsp::client::Reason) -> String {
        match reason {
            lsp::client::Reason::Shutdown => "shut down".to_owned(),
            lsp::client::Reason::Closed => "it closed the connection".to_owned(),
            lsp::client::Reason::Initialize => "it failed to initialize".to_owned(),
            lsp::client::Reason::Failed(error) => format!("the connection failed: {error}"),
            lsp::client::Reason::Exited {
                signal: Some(signal),
                ..
            } => format!("it was killed by signal {signal}"),
            lsp::client::Reason::Exited {
                code: Some(code), ..
            } => format!("it exited with code {code}"),
            lsp::client::Reason::Exited {
                code: None,
                signal: None,
            } => "it exited".to_owned(),
            lsp::client::Reason::Unresponsive => "it stopped reading its input".to_owned(),
            lsp::client::Reason::GaveUp => "it kept crashing, so it was given up on".to_owned(),
            lsp::client::Reason::Timeout => "it did not answer initialize in time".to_owned(),
        }
    }

    /// The status line an update earns: a message the server asked to show, and the title of a
    /// `$/progress` report.
    fn headline(update: &lsp::Update) -> Option<String> {
        match update {
            lsp::Update::Log(entry) if entry.is_shown() => Some(entry.text().to_owned()),
            lsp::Update::Notification(notification) if notification.method() == "$/progress" => {
                let value = notification.params()?.get("value")?;
                let title = value.get("title").and_then(|title| title.as_str());
                let message = value.get("message").and_then(|message| message.as_str());
                match (title, message) {
                    (Some(title), Some(message)) => Some(format!("{title}: {message}")),
                    (Some(line), None) | (None, Some(line)) => Some(line.to_owned()),
                    (None, None) => None,
                }
            }
            lsp::Update::Document(_)
            | lsp::Update::FileEdits(_)
            | lsp::Update::Status(_)
            | lsp::Update::Log(_)
            | lsp::Update::Error(_)
            | lsp::Update::Notification(_)
            | lsp::Update::Trace(_) => None,
        }
    }

    fn rust() -> SyntaxDef {
        SyntaxDef::from_sublime_syntax(include_str!("assets/rust.sublime-syntax"))
            .expect("bundled Rust grammar parses")
    }

    fn theme(_app: &App) -> Theme {
        Theme::Dark
    }

    #[cfg(test)]
    mod tests {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        use iced::futures::{FutureExt, StreamExt};
        use iced::keyboard::key::{Code, Physical};
        use iced::keyboard::{Location, Modifiers};
        use scrive_core::{Diagnostic, EditOp};
        use scrive_iced::Action;
        use serde_json::Value;

        use super::*;

        /// What a fresh client makes of `message` from its server.
        fn updates(message: Value) -> Vec<lsp::Update> {
            let (near, far) = lsp::lsp_server::Connection::memory();
            let (mut client, mut events) = lsp::Client::builder().memory(near);
            let message = serde_json::from_value(message).expect("the fixture is a message");
            far.sender.send(message).expect("the client end is open");
            let mut updates = Vec::new();
            while let Some(Some(event)) = events.next().now_or_never() {
                updates.extend(client.receive(event));
            }
            updates
        }

        /// The status lines `message` earns.
        fn headlines(method: &str, params: Value) -> Vec<String> {
            updates(serde_json::json!({ "jsonrpc": "2.0", "method": method, "params": params }))
                .iter()
                .filter_map(headline)
                .collect()
        }

        /// The status bar shows a showMessage's text, and a progress report's title and message.
        #[test]
        fn headlines_come_from_show_message_and_progress() {
            assert_eq!(
                headlines(
                    "window/showMessage",
                    serde_json::json!({ "type": 3, "message": "hello" })
                ),
                ["hello"],
                "showMessage text"
            );
            assert_eq!(
                headlines(
                    "$/progress",
                    serde_json::json!({ "token": 1, "value": {
                        "kind": "begin", "title": "Indexing", "message": "1/3"
                    }}),
                ),
                ["Indexing: 1/3"],
                "progress title and message",
            );
            assert!(
                headlines(
                    "window/logMessage",
                    serde_json::json!({ "type": 3, "message": "noise" })
                )
                .is_empty(),
                "log messages stay off the bar"
            );
        }

        /// Every stop reason reads as a sentence on the status bar.
        #[test]
        fn stopped_reasons_read_as_sentences() {
            let stop = |reason| described(&lsp::client::Status::Stopped(reason));
            assert_eq!(
                stop(lsp::client::Reason::Exited {
                    code: Some(101),
                    signal: None
                }),
                "rust-analyzer stopped: it exited with code 101",
                "an exit code"
            );
            assert_eq!(
                stop(lsp::client::Reason::Exited {
                    code: None,
                    signal: Some(11)
                }),
                "rust-analyzer stopped: it was killed by signal 11",
                "a signal"
            );
            assert_eq!(
                stop(lsp::client::Reason::GaveUp),
                "rust-analyzer stopped: it kept crashing, so it was given up on",
                "the policy gave up"
            );
            assert_eq!(
                described(&lsp::client::Status::Restarting { attempt: 2 }),
                "rust-analyzer stopped; restarting (attempt 2)…",
                "a restart"
            );
        }

        /// Only a plain Ctrl+S (Cmd+S on macOS) saves.
        #[test]
        fn save_chord_matches_only_plain_ctrl_s() {
            let press = |modifiers: Modifiers, repeat: bool| keyboard::Event::KeyPressed {
                key: Key::Character("s".into()),
                modified_key: Key::Character("s".into()),
                physical_key: Physical::Code(Code::KeyS),
                location: Location::Standard,
                modifiers,
                text: None,
                repeat,
            };
            assert!(
                matches!(save_chord(press(Modifiers::COMMAND, false)), Some(Message::Save)),
                "Ctrl+S saves",
            );
            for (modifiers, repeat, chord) in [
                (Modifiers::COMMAND | Modifiers::SHIFT, false, "Ctrl+Shift+S"),
                (Modifiers::COMMAND | Modifiers::ALT, false, "Ctrl+Alt+S"),
                (Modifiers::COMMAND, true, "a repeated Ctrl+S"),
                (Modifiers::empty(), false, "a plain s"),
            ] {
                assert!(
                    save_chord(press(modifiers, repeat)).is_none(),
                    "{chord} does not save",
                );
            }
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

        /// Starts rust-analyzer on `workspace` and boots the app on it, tracing, with its events
        /// handed over by a thread so a test can wait with a deadline.
        fn started(workspace: &Workspace) -> (App, mpsc::Receiver<lsp::client::Event>) {
            let (mut app, events) =
                App::boot(workspace).unwrap_or_else(|error| panic!("{}", describe(&error)));
            app.client.set_trace(lsp::trace::Mode::Messages);
            (app, forward(events))
        }

        /// Runs `events` on a thread and hands each event over, so a test can wait with a
        /// deadline.
        fn forward(events: lsp::client::Events) -> mpsc::Receiver<lsp::client::Event> {
            let (sender, receiver) = mpsc::channel();
            std::thread::spawn(move || {
                iced::futures::executor::block_on(events.for_each(|event| {
                    let _ = sender.send(event);
                    std::future::ready(())
                }));
            });
            receiver
        }

        /// The next event folded into the app's client, or `None` once `deadline` passes.
        fn pump(
            app: &mut App,
            events: &mpsc::Receiver<lsp::client::Event>,
            deadline: Instant,
        ) -> Option<Vec<lsp::Update>> {
            let timeout = deadline.saturating_duration_since(Instant::now());
            let event = events.recv_timeout(timeout).ok()?;
            Some(app.client.receive(event))
        }

        /// Whether `updates` trace `method` going out.
        fn went_out(updates: &[lsp::Update], method: &str) -> bool {
            updates.iter().any(|update| {
                matches!(update, lsp::Update::Trace(entry)
                    if entry.direction() == lsp::trace::Direction::Outgoing
                        && entry.method() == Some(method))
            })
        }

        /// The diagnostic sets in `updates`, with their stamps, logged as they arrive.
        fn published(
            updates: &[lsp::Update],
            editor: &CodeEditor,
            started: Instant,
        ) -> Vec<(lsp::update::Stamp, Vec<Diagnostic>)> {
            let mut sets = Vec::new();
            for update in updates {
                let lsp::Update::Document(document) = update else {
                    continue;
                };
                assert_eq!(
                    document.doc_id(),
                    editor.document().doc_id(),
                    "the update is for the opened file",
                );
                if let lsp::update::Change::Diagnostics(list) = document.change() {
                    eprintln!(
                        "{:?}: publishDiagnostics at {:?} with {} entries: {:?}",
                        started.elapsed(),
                        document.stamp(),
                        list.len(),
                        list.iter().map(|d| d.message.as_str()).collect::<Vec<_>>(),
                    );
                    sets.push((document.stamp(), list.clone()));
                }
            }
            sets
        }

        /// Whether `list` holds the scratch crate's type error.
        fn mismatched(list: &[Diagnostic]) -> bool {
            list.iter().any(|d| d.message.contains("mismatched"))
        }

        /// Against a real rust-analyzer: the handshake completes, the scratch file's didOpen
        /// goes out, and a `publishDiagnostics` for it comes back through `Client::receive` as
        /// the type error. Once the error is fixed, written to disk and saved, a publish for the
        /// fixed text clears it, and it stays cleared. Then an orderly shutdown ends the process.
        #[test]
        #[ignore = "needs rust-analyzer on PATH and a Rust toolchain; run with --ignored"]
        fn rust_analyzer_reports_the_scratch_crates_type_error_and_clears_it_on_save() {
            let root = std::env::temp_dir().join(format!("{SCRATCH}-test-{}", std::process::id()));
            let workspace = Workspace::scratch(&root).expect("the scratch crate is written");
            let (mut app, events) = started(&workspace);

            let started = Instant::now();
            let deadline = started + Duration::from_secs(120);
            let mut did_open = false;
            let mut diagnostics = None;
            while diagnostics.is_none() {
                let Some(updates) = pump(&mut app, &events, deadline) else {
                    break;
                };
                did_open |= went_out(&updates, "textDocument/didOpen");
                diagnostics = published(&updates, &app.editor, started)
                    .into_iter()
                    .map(|(_, list)| list)
                    .find(|list| !list.is_empty());
            }
            assert!(did_open, "the handshake completed and the didOpen went out");
            let diagnostics = diagnostics.expect("diagnostics arrived within the timeout");
            let label = SCRATCH_MAIN
                .find("label: String")
                .expect("the scratch has the error");
            assert!(
                diagnostics
                    .iter()
                    .any(|d| d.span.start as usize >= label && d.message.contains("mismatched")),
                "the type error is reported: {diagnostics:?}",
            );

            let fix = "let label: String = doubled;";
            let at = SCRATCH_MAIN.find(fix).expect("the scratch has the error") + fix.len() - 1;
            app.editor
                .try_edit(vec![EditOp::insert(u32::try_from(at).expect("small"), ".to_string()")])
                .expect("the fix applies");
            let doc = app.editor.document();
            std::fs::write(&workspace.file, doc.serialize(doc.buffer().eol_flavor()))
                .expect("the fixed file is written");
            let fixed = lsp::update::Stamp::Revision(doc.revision());
            let _ = app.editor.save_lsp(&mut app.client);
            eprintln!("{:?}: saved", started.elapsed());

            let deadline = Instant::now() + Duration::from_secs(120);
            let mut did_save = false;
            let mut cleared = false;
            while !cleared {
                let Some(updates) = pump(&mut app, &events, deadline) else {
                    break;
                };
                did_save |= went_out(&updates, "textDocument/didSave");
                cleared = published(&updates, &app.editor, started)
                    .iter()
                    .any(|(stamp, list)| *stamp == fixed && !mismatched(list));
            }
            assert!(did_save, "rust-analyzer asks for saves, so the didSave goes out");
            assert!(cleared, "a publish for the fixed text drops the type error");
            let quiet = Instant::now() + Duration::from_secs(2);
            while let Some(updates) = pump(&mut app, &events, quiet) {
                for (_, list) in published(&updates, &app.editor, started) {
                    assert!(!mismatched(&list), "the type error stays cleared: {list:?}");
                }
            }

            shut_down(&mut app, &events);
            let _ = std::fs::remove_dir_all(&root);
        }

        /// Shut the server down, as closing the window does, and wait for the client to stop.
        fn shut_down(app: &mut App, events: &mpsc::Receiver<lsp::client::Event>) {
            for document in app.client.shutdown() {
                let _ = app.editor.apply_lsp(&mut app.client, document);
            }
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let updates = pump(app, events, deadline).expect("the client stops after shutdown");
                let stopped = updates.iter().any(|update| {
                    matches!(
                        update,
                        lsp::Update::Status(lsp::client::Status::Stopped(
                            lsp::client::Reason::Shutdown
                        ))
                    )
                });
                if stopped {
                    break;
                }
            }
        }

        /// The offsets of the hints `editor` shows.
        fn hint_offsets(editor: &CodeEditor) -> Vec<u32> {
            let document = editor.document();
            document
                .inlays_in(0..document.buffer().len())
                .map(|hint| hint.offset())
                .collect()
        }

        /// Drive the app's editor against the server until `done` holds, and say whether it did
        /// before `deadline`. Each round fires a scheduled hint fetch at once, as the widget
        /// would once its delay passed, then takes one event and lands its documents.
        fn settle_until(
            app: &mut App,
            events: &mpsc::Receiver<lsp::client::Event>,
            (started, deadline): (Instant, Instant),
            done: impl Fn(&CodeEditor) -> bool,
        ) -> bool {
            while !done(&app.editor) {
                if let Some(wake) = app.editor.pending_wake() {
                    let event = Event::Editor(Action::Wake(wake.generation));
                    let _ = app.editor.update(event, Instant::now());
                    let _ = app.editor.sync_lsp(&mut app.client);
                }
                let Some(updates) = pump(app, events, deadline) else {
                    return false;
                };
                for update in updates {
                    let lsp::Update::Document(document) = update else {
                        continue;
                    };
                    if let lsp::update::Change::Inlays(Some(hints)) = document.change() {
                        eprintln!(
                            "{:?}: {} hints at {:?}",
                            started.elapsed(),
                            hints.len(),
                            document.stamp(),
                        );
                    }
                    let _ = app.editor.apply_lsp(&mut app.client, document);
                }
            }
            true
        }

        /// Against a real rust-analyzer: the scratch file's `let sum = add(1, 2);` gets its
        /// `: i32` hint after load; an edit above moves the hint before any refetch; a refetch
        /// at the new revision keeps it there; and a double-click inserts `: i32` once, with no
        /// hint left beside it, before or after the next refetch.
        #[test]
        #[ignore = "needs rust-analyzer on PATH and a Rust toolchain; run with --ignored"]
        fn rust_analyzer_hints_arrive_move_refresh_and_insert_once() {
            let root =
                std::env::temp_dir().join(format!("{SCRATCH}-hints-test-{}", std::process::id()));
            let workspace = Workspace::scratch(&root).expect("the scratch crate is written");
            let (mut app, events) = started(&workspace);
            let started = Instant::now();
            let within = || (started, Instant::now() + Duration::from_secs(120));
            let current = |editor: &CodeEditor| {
                let document = editor.document();
                document.inlays_revision() == Some(document.revision())
            };

            let sum_end = SCRATCH_MAIN.find("let sum").expect("the scratch has `sum`");
            let sum_end = u32::try_from(sum_end + "let sum".len()).expect("small");
            let arrived = settle_until(&mut app, &events, within(), |e| {
                hint_offsets(e).contains(&sum_end)
            });
            assert!(arrived, "the `sum` hint arrived within the timeout");

            let moved = "// moved\n";
            let main_at = SCRATCH_MAIN.find("fn main").expect("the scratch has main");
            app.editor
                .try_edit(vec![EditOp::insert(u32::try_from(main_at).expect("small"), moved)])
                .expect("the edit applies");
            let typed = sum_end + 9;
            assert!(
                hint_offsets(&app.editor).contains(&typed),
                "the hint rides the edit before any refetch: {:?}",
                hint_offsets(&app.editor),
            );
            let _ = app.editor.sync_lsp(&mut app.client);
            let refetched = settle_until(&mut app, &events, within(), |e| {
                current(e) && hint_offsets(e).contains(&typed)
            });
            assert!(refetched, "a refetch at the new revision keeps the hint after `sum`");

            let key = app
                .editor
                .document()
                .inlays_in(typed..typed)
                .find(|hint| hint.offset() == typed)
                .expect("the `sum` hint shows")
                .key();
            let insert = Event::Editor(Action::InlayInsert { key, offset: typed });
            let _ = app.editor.update(insert, Instant::now());
            let _ = app.editor.sync_lsp(&mut app.client);
            let text = app.editor.document().text().into_owned();
            assert_eq!(
                text.matches("let sum: i32 = add(1, 2);").count(),
                1,
                "the type is inserted once: {text}",
            );
            let typed_end = typed + u32::try_from(": i32".len()).expect("small");
            let beside = |editor: &CodeEditor| {
                let offsets = hint_offsets(editor);
                offsets.contains(&typed) || offsets.contains(&typed_end)
            };
            assert!(!beside(&app.editor), "no hint is left beside the inserted type");

            let settled = settle_until(&mut app, &events, within(), current);
            assert!(settled, "the hints are refetched after the insert");
            assert!(
                !beside(&app.editor),
                "the refetch brings no hint beside the inserted type: {:?}",
                hint_offsets(&app.editor),
            );
            eprintln!("{:?}: done", started.elapsed());

            shut_down(&mut app, &events);
            let _ = std::fs::remove_dir_all(&root);
        }
    }
}
