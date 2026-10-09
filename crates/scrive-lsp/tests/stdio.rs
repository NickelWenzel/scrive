//! The stdio bridge against fake language servers: this binary re-executes itself as a fake
//! server when `SCRIVE_LSP_FAKE_SERVER` names a mode.

#[cfg(target_family = "wasm")]
fn main() {}

#[cfg(not(target_family = "wasm"))]
fn main() {
    match std::env::var(native::FAKE) {
        Ok(mode) => native::fake::serve(&mode),
        Err(_) => native::run(),
    }
}

#[cfg(not(target_family = "wasm"))]
mod native {
    use std::process::Command;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use futures::StreamExt;
    use scrive_core::intel::ticket::Counter;
    use scrive_core::{Diagnostic, Document, HoverInfo, HoverRequest};
    use scrive_lsp::client::{self, Client, Reason, Status};
    use scrive_lsp::{log, update, Update};

    /// The variable that turns this binary into a fake server, naming its mode.
    pub const FAKE: &str = "SCRIVE_LSP_FAKE_SERVER";
    const GRACE: Duration = Duration::from_millis(100);
    /// Room for the first spawn under Windows Defender.
    const PATIENCE: Duration = Duration::from_secs(30);
    /// How long one test may run before the runner gives up on it.
    const DEADLINE: Duration = Duration::from_secs(120);

    /// A client on a fake server, with its events pumped onto a std channel so waits can time
    /// out.
    struct Harness {
        client: Client,
        events: mpsc::Receiver<client::Event>,
    }

    /// A notification no server knows, big enough to fill a pipe quickly.
    enum Blob {}

    impl scrive_lsp::lsp_types::notification::Notification for Blob {
        type Params = String;
        const METHOD: &'static str = "scrive-test/blob";
    }

    impl Harness {
        fn start(mode: &str, configure: impl FnOnce(client::Builder) -> client::Builder) -> Self {
            let mut command =
                Command::new(std::env::current_exe().expect("the test binary has a path"));
            command.env(FAKE, mode);
            let (client, events) = configure(Client::builder().shutdown_grace(GRACE))
                .stdio(command)
                .expect("the fake server starts");
            let (sender, receiver) = mpsc::channel();
            thread::spawn(move || {
                futures::executor::block_on(events.for_each(|event| {
                    let _ = sender.send(event);
                    std::future::ready(())
                }));
            });
            Self {
                client,
                events: receiver,
            }
        }

        /// Every update up to the event whose updates include one `done` matches. Panics after
        /// `PATIENCE`.
        fn until(&mut self, done: impl Fn(&Update) -> bool) -> Vec<Update> {
            let deadline = Instant::now() + PATIENCE;
            let mut updates = Vec::new();
            loop {
                let timeout = deadline.saturating_duration_since(Instant::now());
                let Ok(event) = self.events.recv_timeout(timeout) else {
                    panic!("the awaited update did not come; got {updates:#?}");
                };
                let received = self.client.receive(event);
                let found = received.iter().any(&done);
                updates.extend(received);
                if found {
                    return updates;
                }
            }
        }

        /// Every update until `Events` ends. Panics after `PATIENCE`.
        // "Read to the end", the counterpart of `until`, not a conversion.
        #[allow(clippy::wrong_self_convention)]
        fn to_end(&mut self) -> Vec<Update> {
            let deadline = Instant::now() + PATIENCE;
            let mut updates = Vec::new();
            loop {
                let timeout = deadline.saturating_duration_since(Instant::now());
                match self.events.recv_timeout(timeout) {
                    Ok(event) => updates.extend(self.client.receive(event)),
                    Err(mpsc::RecvTimeoutError::Disconnected) => return updates,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        panic!("the stream did not end; got {updates:#?}")
                    }
                }
            }
        }

        /// Opens `let value = 1;` on the running server.
        fn open(&mut self) -> Document {
            let mut doc = Document::new("let value = 1;").expect("the fixture loads");
            doc.observe_changes(true);
            let uri = scrive_lsp::uri::from_path(&std::env::temp_dir().join("fake.rs"))
                .expect("the temp directory is absolute and UTF-8");
            let answer = self
                .client
                .open(&doc.snapshot(), &uri, "rust")
                .expect("nothing else is open");
            assert!(answer.is_none(), "opening answers nothing locally");
            doc
        }

        /// Sends a hover over `value` to the server, and returns it.
        fn hover(&mut self, doc: &Document) -> HoverRequest {
            let request = HoverRequest::new(Counter::new().issue(doc.revision()), 5, 4..9);
            let answer = self.client.hover(&doc.snapshot(), &request);
            assert!(answer.is_none(), "the hover goes to the server");
            request
        }
    }

    fn tests() -> Vec<(&'static str, fn())> {
        let tests: Vec<(&'static str, fn())> = vec![
            (
                "a_conversation_runs_over_stdio",
                a_conversation_runs_over_stdio,
            ),
            (
                "a_crash_stops_the_client_and_settles_its_requests",
                a_crash_stops_the_client_and_settles_its_requests,
            ),
            (
                "a_server_that_ignores_exit_is_killed_after_the_grace_period",
                a_server_that_ignores_exit_is_killed_after_the_grace_period,
            ),
            (
                "a_live_server_whose_stdout_closes_is_killed_and_reaped",
                a_live_server_whose_stdout_closes_is_killed_and_reaped,
            ),
            (
                "the_hung_server_guard_stops_a_server_that_stops_reading",
                the_hung_server_guard_stops_a_server_that_stops_reading,
            ),
            (
                "dropping_the_client_shuts_the_server_down",
                dropping_the_client_shuts_the_server_down,
            ),
        ];
        #[cfg(windows)]
        let tests = [
            tests,
            vec![(
                "a_console_grandchild_opens_no_window",
                a_console_grandchild_opens_no_window as fn(),
            )],
        ]
        .concat();
        tests
    }

    /// Runs every test on its own thread, one after another, each within `DEADLINE`.
    pub fn run() {
        let tests = tests();
        println!("\nrunning {} tests", tests.len());
        let mut failed = 0;
        for &(name, test) in &tests {
            let (done, result) = mpsc::channel();
            thread::Builder::new()
                .name(name.to_owned())
                .spawn(move || {
                    test();
                    let _ = done.send(());
                })
                .expect("the test thread starts");
            let verdict = match result.recv_timeout(DEADLINE) {
                Ok(()) => "ok",
                Err(mpsc::RecvTimeoutError::Disconnected) => "FAILED",
                Err(mpsc::RecvTimeoutError::Timeout) => "TIMED OUT",
            };
            if verdict != "ok" {
                failed += 1;
            }
            println!("test {name} ... {verdict}");
        }
        let outcome = if failed == 0 { "ok" } else { "FAILED" };
        let passed = tests.len() - failed;
        println!("\ntest result: {outcome}. {passed} passed; {failed} failed\n");
        if failed > 0 {
            std::process::exit(1);
        }
    }

    fn running(update: &Update) -> bool {
        matches!(update, Update::Status(Status::Running))
    }

    /// The reason of a stop update.
    fn stopped(update: &Update) -> Option<&Reason> {
        let Update::Status(Status::Stopped(reason)) = update else {
            return None;
        };
        Some(reason)
    }

    fn diagnostics(update: &Update) -> Option<&[Diagnostic]> {
        let Update::Document(document) = update else {
            return None;
        };
        let update::Change::Diagnostics(list) = document.change() else {
            return None;
        };
        Some(list)
    }

    /// The hover answer to `request` in `update`.
    fn hovered<'a>(update: &'a Update, request: &HoverRequest) -> Option<Option<&'a HoverInfo>> {
        let Update::Document(document) = update else {
            return None;
        };
        let update::Change::Hover(card) = document.change() else {
            return None;
        };
        (document.stamp() == update::Stamp::Ticket(request.ticket)).then_some(card.as_ref())
    }

    /// Whether `update` logs `text` from `source`.
    fn logs(update: &Update, source: log::Source, text: &str) -> bool {
        matches!(update, Update::Log(entry) if entry.source() == source && entry.text() == text)
    }

    fn published_fake(update: &Update) -> bool {
        diagnostics(update)
            .is_some_and(|list| list.iter().any(|diagnostic| diagnostic.message == "fake"))
    }

    /// A server process runs the whole conversation: handshake with this process's id, document
    /// sync, diagnostics, a hover, its stdout banner and stderr line as logs, and an orderly
    /// shutdown that ends the stream.
    fn a_conversation_runs_over_stdio() {
        let mut harness = Harness::start("conversation", |builder| builder);
        let mut updates = harness.until(running);
        let doc = harness.open();
        updates.extend(harness.until(published_fake));
        let request = harness.hover(&doc);
        let pid = std::process::id().to_string();
        updates.extend(harness.until(|update| {
            hovered(update, &request)
                .flatten()
                .is_some_and(|card| card.markdown.contains(&pid))
        }));
        assert!(
            harness.client.shutdown().is_empty(),
            "nothing was in flight"
        );
        let end = harness.to_end();
        let stops: Vec<&Reason> = end.iter().filter_map(stopped).collect();
        assert_eq!(stops, [&Reason::Shutdown], "one stop: {end:#?}");
        assert!(
            end.last().and_then(stopped) == Some(&Reason::Shutdown),
            "the stop is last: {end:#?}"
        );
        updates.extend(end);
        assert!(
            updates
                .iter()
                .any(|update| logs(update, log::Source::Stdout, "fake server banner")),
            "the stdout banner is a log line: {updates:#?}"
        );
        assert!(
            updates
                .iter()
                .any(|update| logs(update, log::Source::Stderr, "fake server ready")),
            "the stderr line is a log line: {updates:#?}"
        );
    }

    /// A server that exits mid-request settles the request empty, clears its diagnostics, and
    /// stops the client with its exit code after its last stderr line.
    fn a_crash_stops_the_client_and_settles_its_requests() {
        let mut harness = Harness::start("crash-on-hover", |builder| builder);
        harness.until(running);
        let doc = harness.open();
        harness.until(published_fake);
        let request = harness.hover(&doc);
        let end = harness.to_end();
        assert!(
            end.iter()
                .any(|update| matches!(hovered(update, &request), Some(None))),
            "the hover settles empty: {end:#?}"
        );
        assert!(
            end.iter()
                .any(|update| diagnostics(update).is_some_and(<[Diagnostic]>::is_empty)),
            "the diagnostics are cleared: {end:#?}"
        );
        let crashing = end
            .iter()
            .position(|update| logs(update, log::Source::Stderr, "crashing"));
        assert!(
            crashing.is_some_and(|at| at + 1 < end.len()),
            "the last stderr line comes before the stop: {end:#?}"
        );
        assert_eq!(
            end.last().and_then(stopped),
            Some(&Reason::Exited {
                code: Some(3),
                signal: None
            }),
            "the stop is last and carries the exit code: {end:#?}"
        );
    }

    /// A server that stays alive after `exit` is killed once the grace period passes, and the
    /// stop still reads as the shutdown.
    fn a_server_that_ignores_exit_is_killed_after_the_grace_period() {
        let mut harness = Harness::start("ignore-exit", |builder| builder);
        harness.until(running);
        let asked = Instant::now();
        assert!(
            harness.client.shutdown().is_empty(),
            "nothing was in flight"
        );
        let end = harness.to_end();
        let elapsed = asked.elapsed();
        assert_eq!(
            end.last().and_then(stopped),
            Some(&Reason::Shutdown),
            "the stop is the shutdown: {end:#?}"
        );
        assert!(
            elapsed >= GRACE,
            "the server had its grace period: {elapsed:?}"
        );
        assert!(elapsed < PATIENCE, "then it was killed: {elapsed:?}");
    }

    /// A server that closes its stdout but keeps running is killed and reaped, and the stop
    /// shows the kill.
    fn a_live_server_whose_stdout_closes_is_killed_and_reaped() {
        let mut harness = Harness::start("close-stdout", |builder| builder);
        let end = harness.to_end();
        #[cfg(unix)]
        let killed = Reason::Exited {
            code: None,
            signal: Some(9),
        };
        #[cfg(windows)]
        let killed = Reason::Exited {
            code: Some(1),
            signal: None,
        };
        assert_eq!(
            end.last().and_then(stopped),
            Some(&killed),
            "the stop shows the kill: {end:#?}"
        );
    }

    /// A server that stops reading its stdin is killed once the unwritten messages pass the
    /// limit.
    fn the_hung_server_guard_stops_a_server_that_stops_reading() {
        let mut harness = Harness::start("deaf", |builder| builder.backlog_limit(16 * 1024));
        harness.until(running);
        for _ in 0..4096 {
            harness.client.notify::<Blob>("x".repeat(4096));
        }
        let end = harness.to_end();
        assert_eq!(
            end.last().and_then(stopped),
            Some(&Reason::Unresponsive),
            "the guard stopped it: {end:#?}"
        );
    }

    /// Dropping a client neither panics nor blocks, and ends its event stream.
    fn dropping_the_client_shuts_the_server_down() {
        let mut harness = Harness::start("conversation", |builder| builder);
        harness.until(running);
        let Harness { client, events } = harness;
        drop(client);
        let deadline = Instant::now() + 2 * GRACE + PATIENCE;
        loop {
            let timeout = deadline.saturating_duration_since(Instant::now());
            match events.recv_timeout(timeout) {
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
                Err(mpsc::RecvTimeoutError::Timeout) => panic!("the stream did not end"),
            }
        }
    }

    /// A console program the server starts opens no console window either.
    #[cfg(windows)]
    fn a_console_grandchild_opens_no_window() {
        let mut harness = Harness::start("console-parent", |builder| builder);
        let updates = harness.until(|update| {
            matches!(update, Update::Log(entry)
                if entry.source() == log::Source::Stderr
                    && entry.text().starts_with("console-window:"))
        });
        assert!(
            updates.iter().any(|update| logs(
                update,
                log::Source::Stderr,
                "console-window: absent"
            )),
            "the grandchild has no console window: {updates:#?}"
        );
        let _ = harness.client.shutdown();
        let end = harness.to_end();
        assert_eq!(
            end.last().and_then(stopped),
            Some(&Reason::Shutdown),
            "the pipes still reach EOF: {end:#?}"
        );
    }

    /// The fake servers, one per mode.
    pub mod fake {
        use std::io::{self, Write};
        use std::thread;

        use scrive_lsp::lsp_server::{Connection, Message, Notification, Response};
        use serde_json::{json, Value};

        /// How a conversation goes.
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Script {
            /// Answers everything.
            Answer,
            /// Exits with code 3 on a hover.
            CrashOnHover,
            /// Runs a console program before serving.
            #[cfg(windows)]
            ConsoleParent,
        }

        /// Serves as the fake server `mode` names, and exits.
        pub fn serve(mode: &str) {
            match mode {
                "conversation" => conversation(Script::Answer),
                "crash-on-hover" => conversation(Script::CrashOnHover),
                "ignore-exit" => ignore_exit(),
                "close-stdout" => close_stdout(),
                "deaf" => deaf(),
                #[cfg(windows)]
                "console-parent" => conversation(Script::ConsoleParent),
                #[cfg(windows)]
                "console-report" => console_report(),
                other => panic!("no fake server mode {other:?}"),
            }
        }

        /// An lsp-server server with hover and full sync that publishes one `fake` diagnostic
        /// per opened document and answers hovers with the client's `processId`.
        fn conversation(script: Script) {
            // lsp-server's writer holds stdout's lock for its whole life, so the banner goes
            // out before the connection exists.
            let mut stdout = io::stdout();
            stdout
                .write_all(b"fake server banner\n")
                .and_then(|()| stdout.flush())
                .expect("stdout is open");
            let (connection, io_threads) = Connection::stdio();
            eprintln!("fake server ready");
            let params = connection
                .initialize(json!({ "hoverProvider": true, "textDocumentSync": 1 }))
                .expect("the client initializes");
            let pid = params["processId"].clone();
            #[cfg(windows)]
            if script == Script::ConsoleParent {
                let report = std::process::Command::new(
                    std::env::current_exe().expect("the test binary has a path"),
                )
                .env(super::FAKE, "console-report")
                .status()
                .expect("the console program runs");
                assert!(report.success(), "the console program reports");
            }
            for message in &connection.receiver {
                match message {
                    Message::Request(request) => {
                        // A message other than `exit` after `shutdown` is an error there.
                        let shut = connection
                            .handle_shutdown(&request)
                            .unwrap_or_else(|_| std::process::exit(1));
                        if shut {
                            break;
                        }
                        let result = if request.method == "textDocument/hover" {
                            if script == Script::CrashOnHover {
                                eprintln!("crashing");
                                std::process::exit(3);
                            }
                            json!({ "contents": { "kind": "markdown", "value": format!("pid {pid}") } })
                        } else {
                            Value::Null
                        };
                        send(&connection, Response::new_ok(request.id, result).into());
                    }
                    Message::Notification(notification)
                        if notification.method == "textDocument/didOpen" =>
                    {
                        let document = &notification.params["textDocument"];
                        let params = json!({
                            "uri": document["uri"],
                            "version": document["version"],
                            "diagnostics": [{
                                "range": {
                                    "start": { "line": 0, "character": 4 },
                                    "end": { "line": 0, "character": 9 },
                                },
                                "severity": 1,
                                "message": "fake",
                            }],
                        });
                        let publish = "textDocument/publishDiagnostics".to_owned();
                        send(&connection, Notification::new(publish, params).into());
                    }
                    Message::Notification(_) | Message::Response(_) => {}
                }
            }
            drop(connection);
            let _ = io_threads.join();
        }

        /// Answers every request with `null`, `shutdown` included, and stays alive after
        /// `exit`.
        fn ignore_exit() {
            let (connection, _io_threads) = Connection::stdio();
            connection
                .initialize(json!({}))
                .expect("the client initializes");
            for message in &connection.receiver {
                if let Message::Request(request) = message {
                    send(
                        &connection,
                        Response::new_ok(request.id, Value::Null).into(),
                    );
                }
            }
            park()
        }

        /// Closes stdout and stays alive.
        fn close_stdout() {
            #[cfg(unix)]
            {
                use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
                // SAFETY: fd 1 is this process's stdout, and nothing in this mode uses it again.
                drop(unsafe { OwnedFd::from_raw_fd(io::stdout().as_raw_fd()) });
            }
            #[cfg(windows)]
            {
                use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
                // SAFETY: this is the process's stdout handle, and nothing in this mode uses it
                // again.
                drop(unsafe { OwnedHandle::from_raw_handle(io::stdout().as_raw_handle()) });
            }
            eprintln!("stdout closed");
            park()
        }

        /// Answers `initialize`, then never reads stdin again.
        fn deaf() {
            let message = Message::read(&mut io::stdin().lock())
                .expect("stdin reads")
                .expect("the client sends initialize");
            let Message::Request(request) = message else {
                panic!("the client opens with a request, got {message:?}");
            };
            let mut stdout = io::stdout().lock();
            Message::from(Response::new_ok(request.id, json!({ "capabilities": {} })))
                .write(&mut stdout)
                .and_then(|()| stdout.flush())
                .expect("stdout is open");
            park()
        }

        /// Reports on stderr whether this process has a console window.
        #[cfg(windows)]
        fn console_report() {
            // SAFETY: `GetConsoleWindow` takes nothing and only reads this process's console.
            let window = unsafe { windows_sys::Win32::System::Console::GetConsoleWindow() };
            let presence = if window.is_null() {
                "absent"
            } else {
                "present"
            };
            eprintln!("console-window: {presence}");
        }

        fn send(connection: &Connection, message: Message) {
            connection
                .sender
                .send(message)
                .expect("lsp-server's writer runs");
        }

        /// Stays alive until killed.
        fn park() -> ! {
            loop {
                thread::park();
            }
        }
    }
}
