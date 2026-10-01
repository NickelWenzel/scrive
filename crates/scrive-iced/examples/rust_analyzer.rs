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
//! The update loop is the one `examples/lsp` uses, cut down to one editor. What that example
//! fakes with an in-process server is real here: a child process, a reader thread that decodes
//! the LSP base protocol from its stdout, and a writer thread that encodes onto its stdin. The
//! server's log goes to this process's stderr.

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

/// The LSP base protocol: a `Content-Length` header, a blank line, then that many bytes of JSON.
#[cfg(not(target_arch = "wasm32"))]
mod frame {
    use core::fmt;

    use scrive_iced::lsp;

    const SEPARATOR: &[u8] = b"\r\n\r\n";

    /// Why the bytes read so far are not a frame.
    #[derive(Debug)]
    pub enum Error {
        /// A header line that is not `Name: value`, or a `Content-Length` that is not a number.
        Header(String),
        /// A header block with no `Content-Length`.
        MissingLength,
        /// A body that is not a JSON-RPC message.
        Body(serde_json::Error),
    }

    impl fmt::Display for Error {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Error::Header(line) => write!(f, "malformed header line {line:?}"),
                Error::MissingLength => f.write_str("header without Content-Length"),
                Error::Body(error) => write!(f, "body is not a JSON-RPC message: {error}"),
            }
        }
    }

    /// `message` framed for the wire.
    pub fn encode(message: &lsp::Message) -> Vec<u8> {
        let body = serde_json::to_vec(message).expect("envelopes serialize");
        let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        frame.extend(body);
        frame
    }

    /// Takes the first complete frame off the front of `buffer`. `Ok(None)` means more bytes are
    /// needed, and leaves `buffer` as it was.
    pub fn decode(buffer: &mut Vec<u8>) -> Result<Option<lsp::Message>, Error> {
        let Some(end) = buffer
            .windows(SEPARATOR.len())
            .position(|window| window == SEPARATOR)
        else {
            return Ok(None);
        };
        let header = String::from_utf8_lossy(&buffer[..end]);
        let mut length = None;
        for line in header.split("\r\n") {
            let (name, value) = line
                .split_once(':')
                .ok_or_else(|| Error::Header(line.to_owned()))?;
            // LSP §baseProtocol: `Content-Type` is the only other header, and it is optional.
            if name.trim().eq_ignore_ascii_case("content-length") {
                let value = value.trim().parse::<usize>();
                length = Some(value.map_err(|_| Error::Header(line.to_owned()))?);
            }
        }
        let length = length.ok_or(Error::MissingLength)?;
        let start = end + SEPARATOR.len();
        if buffer.len() < start + length {
            return Ok(None);
        }
        let frame: Vec<u8> = buffer.drain(..start + length).collect();
        serde_json::from_slice(&frame[start..])
            .map(Some)
            .map_err(Error::Body)
    }

    #[cfg(test)]
    mod tests {
        use serde_json::json;

        use super::*;

        fn message(text: &str) -> lsp::Message {
            serde_json::from_value(json!({
                "jsonrpc": "2.0",
                "method": "window/showMessage",
                "params": { "type": 3, "message": text },
            }))
            .expect("the fixture is an envelope")
        }

        /// A decoded frame is the message that was encoded, and the buffer is left empty. The
        /// body holds non-ASCII text, so the length counts bytes.
        #[test]
        fn a_frame_round_trips() {
            let sent = message("héllo, wörld");
            let mut buffer = encode(&sent);
            let received = decode(&mut buffer).expect("the frame decodes");
            assert_eq!(received, Some(sent), "the message survives the wire");
            assert!(buffer.is_empty(), "the frame is consumed");
        }

        /// A frame that arrives one byte at a time decodes only once its last byte is in.
        #[test]
        fn a_frame_split_across_reads_waits_for_its_last_byte() {
            let sent = message("split");
            let wire = encode(&sent);
            let mut buffer = Vec::new();
            for (index, byte) in wire.iter().enumerate() {
                buffer.push(*byte);
                let decoded = decode(&mut buffer).expect("a partial frame is not an error");
                if index + 1 < wire.len() {
                    assert_eq!(decoded, None, "byte {index} does not complete the frame");
                } else {
                    assert_eq!(decoded, Some(sent.clone()), "the last byte completes it");
                }
            }
        }

        /// Two frames and the start of a third in one read decode in order, and the partial one
        /// stays buffered.
        #[test]
        fn frames_in_one_read_decode_in_order() {
            let (first, second, third) = (message("one"), message("two"), message("three"));
            let mut buffer = encode(&first);
            buffer.extend(encode(&second));
            let third_wire = encode(&third);
            buffer.extend(&third_wire[..10]);
            assert_eq!(
                decode(&mut buffer).ok().flatten(),
                Some(first),
                "first frame"
            );
            assert_eq!(
                decode(&mut buffer).ok().flatten(),
                Some(second),
                "second frame"
            );
            assert_eq!(
                decode(&mut buffer).ok().flatten(),
                None,
                "third frame is partial"
            );
            assert_eq!(buffer, third_wire[..10], "the partial frame stays buffered");
        }

        /// A header block without `Content-Length` is an error, even with a valid body after it.
        #[test]
        fn a_header_without_content_length_is_rejected() {
            let mut buffer = b"Content-Type: application/vscode-jsonrpc\r\n\r\n{}".to_vec();
            assert!(
                matches!(decode(&mut buffer), Err(Error::MissingLength)),
                "the length is required",
            );
        }

        /// Header lines that are not `Name: value`, or a length that is not a number, are errors.
        #[test]
        fn a_garbage_header_is_rejected() {
            for garbage in [&b"hello\r\n\r\n{}"[..], b"Content-Length: many\r\n\r\n{}"] {
                let mut buffer = garbage.to_vec();
                assert!(
                    matches!(decode(&mut buffer), Err(Error::Header(_))),
                    "{:?} is rejected",
                    String::from_utf8_lossy(garbage),
                );
            }
        }
    }
}

/// rust-analyzer as a child process: a reader thread decodes its stdout, and a writer thread owns
/// its stdin.
#[cfg(not(target_arch = "wasm32"))]
mod transport {
    use std::io::{self, Read, Write};
    use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
    use std::sync::mpsc;
    use std::thread;

    use scrive_iced::lsp;

    use crate::frame;

    /// The command that starts the server, looked up on `PATH`.
    pub const SERVER: &str = "rust-analyzer";

    /// Hands messages to the writer thread. Cloning it is cheap; the server's stdin closes when
    /// the last clone drops.
    #[derive(Debug, Clone)]
    pub struct Sender(mpsc::Sender<lsp::Message>);

    /// What the reader thread delivers.
    #[derive(Debug)]
    pub enum Incoming {
        /// One message from the server.
        Message(lsp::Message),
        /// The server is gone; nothing follows. The text says why, for a person.
        Closed(String),
    }

    impl Sender {
        /// Queue `messages` for the server, in order. After the server exits they are dropped:
        /// the reader has already reported the exit.
        pub fn send(&self, messages: Vec<lsp::Message>) {
            for message in messages {
                if self.0.send(message).is_err() {
                    return;
                }
            }
        }
    }

    /// Start the server with piped stdin and stdout and an inherited stderr. `deliver` runs on the
    /// reader thread for every message, then once with [`Incoming::Closed`].
    pub fn spawn(deliver: impl FnMut(Incoming) + Send + 'static) -> io::Result<Sender> {
        let mut child = Command::new(SERVER)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let (sender, outgoing) = mpsc::channel();
        thread::spawn(move || write(stdin, &outgoing));
        thread::spawn(move || read(child, stdout, deliver));
        Ok(Sender(sender))
    }

    /// Why `spawn` failed, for a person.
    pub fn describe(error: &io::Error) -> String {
        match error.kind() {
            io::ErrorKind::NotFound => format!(
                "{SERVER} is not on PATH; install it with `rustup component add rust-analyzer`"
            ),
            _ => format!("could not start {SERVER}: {error}"),
        }
    }

    fn write(mut stdin: ChildStdin, outgoing: &mpsc::Receiver<lsp::Message>) {
        for message in outgoing {
            let written = stdin.write_all(&frame::encode(&message));
            if written.and_then(|()| stdin.flush()).is_err() {
                return;
            }
        }
    }

    fn read(mut child: Child, mut stdout: ChildStdout, mut deliver: impl FnMut(Incoming)) {
        let mut buffer = Vec::new();
        let mut chunk = [0; 8192];
        let failure = loop {
            match frame::decode(&mut buffer) {
                Ok(Some(message)) => {
                    deliver(Incoming::Message(message));
                    continue;
                }
                Ok(None) => {}
                Err(error) => break Some(format!("{SERVER} sent a bad frame: {error}")),
            }
            match stdout.read(&mut chunk) {
                Ok(0) => break None,
                Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => break Some(format!("reading from {SERVER} failed: {error}")),
            }
        };
        if failure.is_some() {
            // It may be blocked writing to a pipe nobody reads any more.
            let _ = child.kill();
        }
        let exit = match child.wait() {
            Ok(status) => format!("{SERVER} exited ({status})"),
            Err(error) => format!("{SERVER} is gone: {error}"),
        };
        deliver(Incoming::Closed(match failure {
            Some(failure) => format!("{failure}; {exit}"),
            None => exit,
        }));
    }
}

/// The application: one editor, one client, and the link to the server process.
#[cfg(not(target_arch = "wasm32"))]
mod app {
    use std::io;
    use std::path::{Path, PathBuf};

    use iced::futures::channel::mpsc;
    use iced::futures::{SinkExt, Stream, StreamExt};
    use iced::keyboard::{self, Key};
    use iced::time::Instant;
    use iced::widget::{column, container, text};
    use iced::{Element, Fill, Subscription, Task, Theme};

    use scrive_core::SyntaxDef;
    use scrive_iced::{lsp, CodeEditor, Event};

    use crate::transport;

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
        root: PathBuf,
        file: PathBuf,
        text: String,
    }

    struct App {
        editor: CodeEditor,
        client: lsp::Client,
        link: Link,
        /// Where Ctrl+S writes the document.
        file: PathBuf,
        /// The one-line status bar.
        status: String,
    }

    #[derive(Debug, Clone)]
    enum Message {
        /// A message from the editor.
        Editor(Event),
        /// Ctrl+S: write the document to its file and tell the server.
        Save,
        /// A JSON-RPC message from the server.
        Lsp(lsp::Message),
        /// The server process is running, and this reaches its stdin.
        Connected(transport::Sender),
        /// The server could not start, or has exited; the text says why.
        Disconnected(String),
    }

    /// Where outgoing messages go.
    enum Link {
        /// Waiting for the process: messages queue, the `initialize` request first.
        Connecting(Vec<lsp::Message>),
        Connected(transport::Sender),
        /// The server is gone, and messages are dropped.
        Closed,
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
                })?
                .to_owned();
            Ok(Self { root, file, text })
        }

        /// Writes the scratch crate into `root`, replacing an earlier one, and opens its
        /// `src/main.rs`.
        fn scratch(root: &Path) -> io::Result<Self> {
            std::fs::create_dir_all(root.join("src"))?;
            std::fs::write(root.join("Cargo.toml"), SCRATCH_MANIFEST)?;
            std::fs::write(root.join("src/main.rs"), SCRATCH_MAIN)?;
            Self::open(&root.join("src/main.rs"))
        }

        fn file_uri(&self) -> lsp::lsp_types::Uri {
            file_uri(&self.file)
        }

        fn root_uri(&self) -> lsp::lsp_types::Uri {
            file_uri(&self.root)
        }

        fn text(&self) -> &str {
            &self.text
        }
    }

    impl App {
        fn new(workspace: &Workspace) -> Self {
            let (mut client, initialize) = lsp::Client::builder()
                .root(workspace.root_uri())
                .process_id(std::process::id())
                .build();
            let mut editor = CodeEditor::new(workspace.text())
                .language(rust())
                .rename(true);
            let mut queued = vec![initialize];
            // Before the handshake this only records the text; the didOpen follows `initialized`.
            queued.extend(
                editor
                    .open_lsp(&mut client, &workspace.file_uri(), "rust")
                    .expect("the client has no other document"),
            );
            Self {
                editor,
                client,
                link: Link::Connecting(queued),
                file: workspace.file.clone(),
                status: format!("starting {}…", transport::SERVER),
            }
        }

        /// `now` is the instant iced stamps on the message (`iced::application::timed`).
        fn update(&mut self, message: Message, now: Instant) -> Task<Message> {
            let Self {
                editor,
                client,
                link,
                file,
                status,
            } = self;
            match message {
                Message::Editor(event) => {
                    let task = editor.update(event, now).map(Message::Editor);
                    let synced = editor.sync_lsp(client);
                    link.send(synced.messages);
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
                            link.send(saved.messages);
                            if let Some(line) = jumped(saved.jump) {
                                *status = line;
                            }
                            let name = file.file_name().unwrap_or(file.as_os_str()).display();
                            *status = match link {
                                Link::Connected(_) => {
                                    format!("saved {name} — cargo check running…")
                                }
                                Link::Connecting(_) | Link::Closed => format!("saved {name}"),
                            };
                        }
                        Err(error) => {
                            *status = format!("could not save {}: {error}", file.display());
                        }
                    }
                    Task::none()
                }
                Message::Connected(sender) => {
                    if let Link::Connecting(queued) = std::mem::replace(link, Link::Closed) {
                        sender.send(queued);
                    }
                    *link = Link::Connected(sender);
                    *status = format!("connected to {}", transport::SERVER);
                    Task::none()
                }
                Message::Lsp(message) => {
                    let output = match client.receive(message) {
                        Ok(output) => output,
                        Err(error) => {
                            *status = format!("error: {error}");
                            return Task::none();
                        }
                    };
                    let mut outgoing = output.messages;
                    for update in output.updates {
                        match update {
                            lsp::Update::Document(document) => {
                                let applied = editor.apply_lsp(client, document);
                                outgoing.extend(applied.messages);
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
                            lsp::Update::Notification(notification) => {
                                if let Some(line) = headline(&notification) {
                                    *status = line;
                                }
                            }
                        }
                    }
                    link.send(outgoing);
                    Task::none()
                }
                Message::Disconnected(reason) => {
                    *link = Link::Closed;
                    *status = reason;
                    Task::none()
                }
            }
        }

        fn view(&self) -> Element<'_, Message> {
            let status = text(self.status.as_str())
                .size(12)
                .font(scrive_iced::DEFAULT_FONT);
            column![
                container(self.editor.view().map(Message::Editor))
                    .width(Fill)
                    .height(Fill),
                container(status).padding([2, 8]),
            ]
            .into()
        }

        fn subscription(&self) -> Subscription<Message> {
            Subscription::batch([
                self.editor.subscription().map(Message::Editor),
                keyboard::listen().filter_map(save_chord),
                Subscription::run(connect),
            ])
        }
    }

    impl Link {
        fn send(&mut self, messages: Vec<lsp::Message>) {
            match self {
                Link::Connecting(queued) => queued.extend(messages),
                Link::Connected(sender) => sender.send(messages),
                Link::Closed => {}
            }
        }
    }

    pub fn run(workspace: Workspace) -> iced::Result {
        let title = format!("scrive — rust-analyzer — {}", workspace.file.display());
        // `timed` hands `update` each message's instant, which the editor's debounces run on.
        iced::application::timed(
            move || App::new(&workspace),
            App::update,
            App::subscription,
            App::view,
        )
        .title(move |_: &App| title.clone())
        .theme(theme)
        .fonts(scrive_iced::required_fonts().iter().copied())
        .run()
    }

    /// The server's messages: `Connected` first, then every message it sends, then one
    /// `Disconnected`.
    fn connect() -> impl Stream<Item = Message> {
        iced::stream::channel(100, async |mut output: mpsc::Sender<Message>| {
            let (incoming, mut received) = mpsc::unbounded();
            let spawned = transport::spawn(move |message| {
                // The receiver drops only when the app has stopped listening.
                let _ = incoming.unbounded_send(message);
            });
            let first = match spawned {
                Ok(sender) => Message::Connected(sender),
                Err(error) => Message::Disconnected(transport::describe(&error)),
            };
            if output.send(first).await.is_err() {
                return;
            }
            while let Some(message) = received.next().await {
                let message = match message {
                    transport::Incoming::Message(message) => Message::Lsp(message),
                    transport::Incoming::Closed(reason) => Message::Disconnected(reason),
                };
                if output.send(message).await.is_err() {
                    return;
                }
            }
        })
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

    /// The status line a server notification earns: `window/showMessage` text, and the title of
    /// a `$/progress` report.
    fn headline(notification: &lsp::message::Notification) -> Option<String> {
        let params = notification.params.as_ref()?;
        let line = match notification.method.as_str() {
            "window/showMessage" => params.get("message")?.as_str()?.to_owned(),
            "$/progress" => {
                let value = params.get("value")?;
                let title = value.get("title").and_then(|title| title.as_str());
                let message = value.get("message").and_then(|message| message.as_str());
                match (title, message) {
                    (Some(title), Some(message)) => format!("{title}: {message}"),
                    (Some(line), None) | (None, Some(line)) => line.to_owned(),
                    (None, None) => return None,
                }
            }
            _ => return None,
        };
        Some(line)
    }

    /// A `file:` URI for the absolute `path`, percent-encoding every byte outside RFC 3986's
    /// unreserved set, `/` and `:`.
    fn file_uri(path: &Path) -> lsp::lsp_types::Uri {
        let mut path = path.to_string_lossy().into_owned();
        if cfg!(windows) {
            path = path.replace('\\', "/");
        }
        if !path.starts_with('/') {
            // A drive-letter path: `file:///C:/…`.
            path.insert(0, '/');
        }
        let mut uri = String::from("file://");
        for byte in path.bytes() {
            if byte.is_ascii_alphanumeric() || b"-._~/:".contains(&byte) {
                uri.push(char::from(byte));
            } else {
                uri.push_str(&format!("%{byte:02X}"));
            }
        }
        uri.parse().expect("an encoded file path is a valid URI")
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

        use iced::keyboard::key::{Code, Physical};
        use iced::keyboard::{Location, Modifiers};
        use scrive_core::{Diagnostic, EditOp};
        use serde_json::Value;

        use super::*;

        /// Spaces and non-ASCII are percent-encoded, and the result is already in the form the
        /// client normalizes to, so the server sees the URI the host built.
        #[cfg(unix)]
        #[test]
        fn file_uris_percent_encode_and_are_normalized() {
            let uri = file_uri(Path::new("/tmp/a dir/é.rs"));
            assert_eq!(
                uri.as_str(),
                "file:///tmp/a%20dir/%C3%A9.rs",
                "the path is encoded"
            );
            assert_eq!(
                lsp::uri::normalize(&uri).as_str(),
                uri.as_str(),
                "normalizing changes nothing",
            );
        }

        /// The status bar shows a showMessage's text, and a progress report's title and message.
        #[test]
        fn headlines_come_from_show_message_and_progress() {
            let notification = |method: &str, params: Value| lsp::message::Notification {
                method: method.to_owned(),
                params: Some(params),
            };
            let shown = notification(
                "window/showMessage",
                serde_json::json!({ "type": 3, "message": "hello" }),
            );
            assert_eq!(
                headline(&shown).as_deref(),
                Some("hello"),
                "showMessage text"
            );
            let progress = notification(
                "$/progress",
                serde_json::json!({ "token": 1, "value": {
                    "kind": "begin", "title": "Indexing", "message": "1/3"
                }}),
            );
            assert_eq!(
                headline(&progress).as_deref(),
                Some("Indexing: 1/3"),
                "progress title and message",
            );
            let logged = notification(
                "window/logMessage",
                serde_json::json!({ "type": 3, "message": "noise" }),
            );
            assert_eq!(headline(&logged), None, "log messages stay off the bar");
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

        /// The next message from the server, folded into `client`, with its answers sent back.
        /// `None` once `deadline` passes.
        fn pump(
            client: &mut lsp::Client,
            incoming: &mpsc::Receiver<transport::Incoming>,
            sender: &transport::Sender,
            deadline: Instant,
        ) -> Option<lsp::Output> {
            let timeout = deadline.saturating_duration_since(Instant::now());
            let message = match incoming.recv_timeout(timeout) {
                Ok(transport::Incoming::Message(message)) => message,
                Ok(transport::Incoming::Closed(reason)) => panic!("the server left: {reason}"),
                Err(_) => return None,
            };
            let output = client.receive(message).expect("server messages decode");
            sender.send(output.messages.clone());
            Some(output)
        }

        /// The diagnostic sets in `output`, with their stamps, logged as they arrive.
        fn published(
            output: &lsp::Output,
            editor: &CodeEditor,
            started: Instant,
        ) -> Vec<(lsp::update::Stamp, Vec<Diagnostic>)> {
            let mut sets = Vec::new();
            for update in &output.updates {
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
            let (deliver, incoming) = mpsc::channel();
            let sender = transport::spawn(move |message| {
                let _ = deliver.send(message);
            })
            .unwrap_or_else(|error| panic!("{}", transport::describe(&error)));

            let (mut client, initialize) = lsp::Client::builder()
                .root(workspace.root_uri())
                .process_id(std::process::id())
                .build();
            let mut editor = CodeEditor::new(workspace.text());
            let mut outgoing = vec![initialize];
            outgoing.extend(
                editor
                    .open_lsp(&mut client, &workspace.file_uri(), "rust")
                    .expect("the only document"),
            );
            sender.send(outgoing);

            let started = Instant::now();
            let deadline = started + Duration::from_secs(120);
            let mut did_open = false;
            let mut diagnostics = None;
            while diagnostics.is_none() {
                let Some(output) = pump(&mut client, &incoming, &sender, deadline) else {
                    break;
                };
                did_open |= output.messages.iter().any(|message| {
                    matches!(message, lsp::Message::Notification(n) if n.method == "textDocument/didOpen")
                });
                diagnostics = published(&output, &editor, started)
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
            editor
                .try_edit(vec![EditOp::insert(u32::try_from(at).expect("small"), ".to_string()")])
                .expect("the fix applies");
            let doc = editor.document();
            std::fs::write(&workspace.file, doc.serialize(doc.buffer().eol_flavor()))
                .expect("the fixed file is written");
            let fixed = lsp::update::Stamp::Revision(doc.revision());
            let saved = editor.save_lsp(&mut client);
            assert!(
                saved.messages.iter().any(|message| {
                    matches!(message, lsp::Message::Notification(n) if n.method == "textDocument/didSave")
                }),
                "rust-analyzer asks for saves, so the didSave goes out",
            );
            eprintln!("{:?}: saved", started.elapsed());
            sender.send(saved.messages);

            let deadline = Instant::now() + Duration::from_secs(120);
            let mut cleared = false;
            while !cleared {
                let Some(output) = pump(&mut client, &incoming, &sender, deadline) else {
                    break;
                };
                cleared = published(&output, &editor, started)
                    .iter()
                    .any(|(stamp, list)| *stamp == fixed && !mismatched(list));
            }
            assert!(cleared, "a publish for the fixed text drops the type error");
            let quiet = Instant::now() + Duration::from_secs(2);
            while let Some(output) = pump(&mut client, &incoming, &sender, quiet) {
                for (_, list) in published(&output, &editor, started) {
                    assert!(!mismatched(&list), "the type error stays cleared: {list:?}");
                }
            }

            sender.send(client.shutdown().messages);
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let timeout = deadline.saturating_duration_since(Instant::now());
                match incoming.recv_timeout(timeout) {
                    Ok(transport::Incoming::Message(message)) => {
                        let output = client.receive(message).expect("server messages decode");
                        sender.send(output.messages);
                    }
                    Ok(transport::Incoming::Closed(reason)) => {
                        eprintln!("{reason}");
                        break;
                    }
                    Err(_) => panic!("the server did not exit after shutdown"),
                }
            }
            let _ = std::fs::remove_dir_all(&root);
        }
    }
}
