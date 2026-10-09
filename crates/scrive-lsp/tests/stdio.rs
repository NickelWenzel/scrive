//! The stdio bridge against fake language servers: this binary re-executes itself as a fake
//! server when `SCRIVE_LSP_FAKE_SERVER` names a mode.

#[cfg(target_family = "wasm")]
fn main() {}

#[cfg(not(target_family = "wasm"))]
fn main() {
    if std::env::var_os(native::GRANDCHILD).is_some() {
        return native::fake::grandchild();
    }
    match std::env::var(native::FAKE) {
        Ok(mode) => native::fake::serve(&mode),
        Err(_) => native::run(),
    }
}

#[cfg(not(target_family = "wasm"))]
mod native {
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use futures::StreamExt;
    use scrive_core::intel::ticket::Counter;
    use scrive_core::{Diagnostic, Document, HoverInfo, HoverRequest};
    use scrive_lsp::client::{self, Client, Reason, Status};
    use scrive_lsp::{log, restart, trace, update, Update};

    /// The variable that turns this binary into a fake server, naming its mode.
    pub const FAKE: &str = "SCRIVE_LSP_FAKE_SERVER";
    /// The prefix of the knobs the `conversation` mode reads.
    pub const KNOB: &str = "SCRIVE_LSP_FAKE_";
    /// The variable that turns this binary into a crashed server's grandchild.
    pub const GRANDCHILD: &str = "SCRIVE_LSP_GRANDCHILD";
    /// What the crashed server's grandchild publishes about, besides the open document.
    pub const UNOPENED: &str = "grandchild.rs";
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
        log: Log,
    }

    /// The fake server's log file: one line per `spawn <n>`, `recv <method>` and `exit <code>`.
    struct Log(PathBuf);

    impl Log {
        /// The lines so far.
        fn lines(&self) -> Vec<String> {
            std::fs::read_to_string(&self.0)
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }

        /// The lines once `done` holds for them. Panics after `PATIENCE`.
        fn until(&self, done: impl Fn(&[String]) -> bool) -> Vec<String> {
            let deadline = Instant::now() + PATIENCE;
            loop {
                let lines = self.lines();
                if done(&lines) {
                    return lines;
                }
                assert!(
                    Instant::now() < deadline,
                    "the log never got there: {lines:#?}"
                );
                thread::sleep(Duration::from_millis(10));
            }
        }
    }

    /// A notification no server knows, big enough to fill a pipe quickly.
    enum Blob {}

    impl scrive_lsp::lsp_types::notification::Notification for Blob {
        type Params = String;
        const METHOD: &'static str = "scrive-test/blob";
    }

    impl Harness {
        /// A client on the fake server `mode`, with the `conversation` knobs in `env` (named
        /// without their prefix). The server logs to a file of this test's own.
        fn start(
            mode: &str,
            env: &[(&str, &str)],
            configure: impl FnOnce(client::Builder) -> client::Builder,
        ) -> Self {
            let name = thread::current().name().unwrap_or("test").to_owned();
            let log = std::env::temp_dir().join(format!(
                "scrive-lsp-stdio-{name}-{}.log",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&log);
            let mut command =
                Command::new(std::env::current_exe().expect("the test binary has a path"));
            command.env(FAKE, mode).env(format!("{KNOB}LOG"), &log);
            for (knob, value) in env {
                command.env(format!("{KNOB}{knob}"), value);
            }
            let builder = Client::builder()
                .shutdown_grace(GRACE)
                .backoff(Duration::from_millis(10));
            let (client, events) = configure(builder)
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
                log: Log(log),
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

        /// Opens `let value = 1;` as `fake.rs`.
        fn open(&mut self) -> Document {
            self.open_as("fake.rs")
        }

        /// Opens `let value = 1;` as `name` in the temp directory, where it caches nothing.
        fn open_as(&mut self, name: &str) -> Document {
            let mut doc = Document::new("let value = 1;").expect("the fixture loads");
            doc.observe_changes(true);
            let uri = temp_uri(name);
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

    /// `name` in the temp directory, as a URI.
    pub fn temp_uri(name: &str) -> scrive_lsp::lsp_types::Uri {
        scrive_lsp::uri::from_path(&std::env::temp_dir().join(name))
            .expect("the temp directory is absolute and UTF-8")
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
            (
                "shutdown_finishes_inside_the_grace_period",
                shutdown_finishes_inside_the_grace_period,
            ),
            (
                "a_server_that_ignores_shutdown_is_killed_after_two_grace_periods",
                a_server_that_ignores_shutdown_is_killed_after_two_grace_periods,
            ),
            (
                "a_server_that_hangs_on_exit_is_killed_after_the_grace",
                a_server_that_hangs_on_exit_is_killed_after_the_grace,
            ),
            (
                "shutdown_while_initializing_skips_the_shutdown_request",
                shutdown_while_initializing_skips_the_shutdown_request,
            ),
            (
                "dropping_the_client_runs_the_shutdown_sequence",
                dropping_the_client_runs_the_shutdown_sequence,
            ),
            (
                "a_crash_restarts_and_reopens_the_documents",
                a_crash_restarts_and_reopens_the_documents,
            ),
            (
                "restarts_follow_the_policy_then_give_up",
                restarts_follow_the_policy_then_give_up,
            ),
            (
                "crash_text_arrives_before_the_status_change",
                crash_text_arrives_before_the_status_change,
            ),
            (
                "nothing_from_a_dead_server_is_applied_after_its_loss",
                nothing_from_a_dead_server_is_applied_after_its_loss,
            ),
            (
                "a_grandchild_holding_stdout_does_not_delay_the_loss_past_the_grace",
                a_grandchild_holding_stdout_does_not_delay_the_loss_past_the_grace,
            ),
            (
                "the_new_process_receives_nothing_meant_for_the_old",
                the_new_process_receives_nothing_meant_for_the_old,
            ),
            (
                "a_crash_right_after_initialize_is_restarted_not_stopped",
                a_crash_right_after_initialize_is_restarted_not_stopped,
            ),
            (
                "a_crash_before_initialize_stops_without_restarting",
                a_crash_before_initialize_stops_without_restarting,
            ),
            (
                "shutdown_during_a_backoff_stops_at_once",
                shutdown_during_a_backoff_stops_at_once,
            ),
            (
                "a_reconnect_racing_shutdown_never_shows_running",
                a_reconnect_racing_shutdown_never_shows_running,
            ),
            (
                "restart_after_giving_up_brings_the_server_back",
                restart_after_giving_up_brings_the_server_back,
            ),
            (
                "restart_kills_a_healthy_server_and_reopens",
                restart_kills_a_healthy_server_and_reopens,
            ),
            (
                "an_initialize_timeout_on_the_first_start_stops",
                an_initialize_timeout_on_the_first_start_stops,
            ),
            (
                "an_initialize_timeout_after_a_restart_counts_against_the_policy",
                an_initialize_timeout_after_a_restart_counts_against_the_policy,
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

    fn restarting(update: &Update) -> bool {
        matches!(update, Update::Status(Status::Restarting { .. }))
    }

    /// The statuses among `updates`, in order.
    fn statuses(updates: &[Update]) -> Vec<Status> {
        updates
            .iter()
            .filter_map(|update| {
                let Update::Status(status) = update else {
                    return None;
                };
                Some(status.clone())
            })
            .collect()
    }

    /// The log lines of spawn `n` of the fake server.
    fn spawn_lines(lines: &[String], n: usize) -> Vec<String> {
        lines
            .iter()
            .skip_while(|line| **line != format!("spawn {n}"))
            .skip(1)
            .take_while(|line| !line.starts_with("spawn "))
            .cloned()
            .collect()
    }

    fn spawns(lines: &[String]) -> usize {
        lines.iter().filter(|line| line.starts_with("spawn ")).count()
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
        let mut harness = Harness::start("conversation", &[], |builder| builder);
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

    /// A server that exits mid-request settles the request empty, clears its diagnostics, and,
    /// with no restarts, stops the client with its exit code after its last stderr line.
    fn a_crash_stops_the_client_and_settles_its_requests() {
        let mut harness = Harness::start("crash-on-hover", &[], |builder| {
            builder.restart(restart::Policy::Never)
        });
        harness.until(running);
        let doc = harness.open();
        harness.until(published_fake);
        let request = harness.hover(&doc);
        let end = harness.until(|update| stopped(update).is_some());
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
        let mut harness = Harness::start("ignore-exit", &[], |builder| builder);
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

    /// A server that closes its stdout but keeps running is killed and reaped; it never
    /// completed `initialize`, so the client stops at once. The server would park forever, so
    /// only the kill can produce the stop.
    fn a_live_server_whose_stdout_closes_is_killed_and_reaped() {
        let mut harness = Harness::start("close-stdout", &[], |builder| {
            builder.restart(restart::Policy::Never)
        });
        let end = harness.until(|update| stopped(update).is_some());
        assert!(
            matches!(end.last().and_then(stopped), Some(Reason::Exited { .. })),
            "the stop shows the exit: {end:#?}"
        );
    }

    /// A server that stops reading its stdin is killed once the unwritten messages pass the
    /// limit.
    fn the_hung_server_guard_stops_a_server_that_stops_reading() {
        let mut harness = Harness::start("deaf", &[], |builder| {
            builder
                .backlog_limit(16 * 1024)
                .restart(restart::Policy::Never)
        });
        harness.until(running);
        for _ in 0..4096 {
            harness.client.notify::<Blob>("x".repeat(4096));
        }
        let end = harness.until(|update| stopped(update).is_some());
        assert_eq!(
            end.last().and_then(stopped),
            Some(&Reason::Unresponsive),
            "the guard stopped it: {end:#?}"
        );
    }

    /// Dropping a client neither panics nor blocks, and its event stream ends because the
    /// client is gone. `dropping_the_client_runs_the_shutdown_sequence` proves the sequence.
    fn dropping_the_client_shuts_the_server_down() {
        let mut harness = Harness::start("conversation", &[], |builder| builder);
        harness.until(running);
        let Harness { client, events, .. } = harness;
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

    /// Whether `lines` holds each of `expected`, in that order, with anything in between.
    fn in_order(lines: &[String], expected: &[&str]) -> bool {
        let mut lines = lines.iter();
        expected
            .iter()
            .all(|expected| lines.any(|line| line == expected))
    }

    fn exited(line: &str) -> bool {
        line.starts_with("exit ")
    }

    /// A well-behaved server answers `shutdown`, gets `exit` and leaves well inside the grace
    /// period. The reply goes to the worker: no update and no trace carries the reserved id.
    fn shutdown_finishes_inside_the_grace_period() {
        let grace = Duration::from_secs(2);
        let mut harness = Harness::start("conversation", &[], |builder| {
            builder
                .shutdown_grace(grace)
                .trace(trace::Mode::Messages)
        });
        let mut updates = harness.until(running);
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
        assert!(elapsed < grace, "no deadline ran out: {elapsed:?}");
        let lines = harness.log.lines();
        assert!(
            in_order(&lines, &["recv shutdown", "recv exit"]),
            "shutdown, then exit: {lines:#?}"
        );
        assert!(
            lines.last().is_some_and(|line| line == "exit 0" || line == "exit 1"),
            "the server exited by itself: {lines:#?}"
        );
        updates.extend(end);
        assert!(
            !updates.iter().any(|update| matches!(update, Update::Trace(entry)
                if entry.direction() == trace::Direction::Incoming
                    && String::from_utf8_lossy(entry.json()).contains("scrive-lsp/shutdown"))),
            "the reply is not traced: {updates:#?}"
        );
    }

    /// A server that never answers `shutdown` gets `exit` after one grace period, and is killed
    /// after the second.
    fn a_server_that_ignores_shutdown_is_killed_after_two_grace_periods() {
        let grace = Duration::from_millis(200);
        let mut harness = Harness::start("conversation", &[("IGNORE_SHUTDOWN", "1")], |builder| {
            builder.shutdown_grace(grace)
        });
        harness.until(running);
        let asked = Instant::now();
        let _ = harness.client.shutdown();
        let end = harness.to_end();
        let elapsed = asked.elapsed();
        assert_eq!(
            end.last().and_then(stopped),
            Some(&Reason::Shutdown),
            "the stop is the shutdown: {end:#?}"
        );
        assert!(elapsed >= 2 * grace, "both grace periods ran: {elapsed:?}");
        assert!(
            elapsed < 2 * grace + Duration::from_secs(2),
            "then it was killed: {elapsed:?}"
        );
        let lines = harness.log.lines();
        assert!(
            lines.iter().any(|line| line == "recv shutdown"),
            "the request arrived: {lines:#?}"
        );
        assert!(
            !lines.iter().any(|line| exited(line)),
            "the server never exited: {lines:#?}"
        );
    }

    /// A server that answers `shutdown` but stays after `exit` is killed after one grace period.
    fn a_server_that_hangs_on_exit_is_killed_after_the_grace() {
        let grace = Duration::from_millis(200);
        let mut harness = Harness::start("conversation", &[("HANG_ON_EXIT", "1")], |builder| {
            builder.shutdown_grace(grace)
        });
        harness.until(running);
        let asked = Instant::now();
        let _ = harness.client.shutdown();
        let end = harness.to_end();
        let elapsed = asked.elapsed();
        assert_eq!(
            end.last().and_then(stopped),
            Some(&Reason::Shutdown),
            "the stop is the shutdown: {end:#?}"
        );
        assert!(elapsed >= grace, "the exit grace ran: {elapsed:?}");
        let lines = harness.log.lines();
        assert!(
            in_order(&lines, &["recv shutdown", "recv exit"]),
            "shutdown, then exit: {lines:#?}"
        );
    }

    /// A server that hasn't answered `initialize` gets `exit` without `shutdown`.
    fn shutdown_while_initializing_skips_the_shutdown_request() {
        let mut harness = Harness::start("conversation", &[("SILENT_FROM", "0")], |builder| {
            builder.shutdown_grace(Duration::from_millis(200))
        });
        harness.log.until(|lines| lines.iter().any(|line| line == "recv initialize"));
        let _ = harness.client.shutdown();
        let end = harness.to_end();
        assert_eq!(
            end.last().and_then(stopped),
            Some(&Reason::Shutdown),
            "the stop is the shutdown: {end:#?}"
        );
        let lines = harness.log.lines();
        assert!(
            !lines.iter().any(|line| line == "recv shutdown"),
            "no shutdown request: {lines:#?}"
        );
        assert!(
            lines.iter().any(|line| line == "recv exit"),
            "only exit: {lines:#?}"
        );
    }

    /// Dropping a running client runs the whole sequence on the worker, which the server's log
    /// shows; nobody is left to receive a stop.
    fn dropping_the_client_runs_the_shutdown_sequence() {
        let mut harness = Harness::start("conversation", &[], |builder| builder);
        harness.until(running);
        let Harness {
            client,
            events,
            log,
        } = harness;
        drop(client);
        drop(events);
        let lines = log.until(|lines| lines.iter().any(|line| exited(line)));
        assert!(
            in_order(&lines, &["recv shutdown", "recv exit"]),
            "shutdown, then exit, then the exit: {lines:#?}"
        );
        assert!(
            lines.last().is_some_and(|line| exited(line)),
            "the server exited last: {lines:#?}"
        );
    }

    fn restarting_once() -> Vec<Status> {
        vec![
            Status::Running,
            Status::Restarting { attempt: 1 },
            Status::Running,
        ]
    }

    /// A server that crashes after the handshake is restarted: the same client reopens its
    /// document on the new process and answers from it.
    fn a_crash_restarts_and_reopens_the_documents() {
        let mut harness = Harness::start(
            "conversation",
            &[("CRASH_AFTER", "3"), ("CRASHES", "1")],
            |builder| builder,
        );
        let id = harness.client.id();
        let mut updates = harness.until(running);
        let doc = harness.open();
        updates.extend(harness.until(running));
        assert_eq!(statuses(&updates), restarting_once(), "{updates:#?}");
        // The answer comes after the server read everything sent before the hover.
        let request = harness.hover(&doc);
        harness.until(|update| hovered(update, &request).flatten().is_some());
        let lines = harness.log.lines();
        let respawned = spawn_lines(&lines, 1);
        assert_eq!(
            respawned.get(..3),
            Some(
                &[
                    "recv initialize".to_owned(),
                    "recv initialized".to_owned(),
                    "recv textDocument/didOpen let value = 1;".to_owned(),
                ][..]
            ),
            "the new process is initialized and gets the document: {lines:#?}"
        );
        assert_eq!(harness.client.id(), id, "the same client");
    }

    /// Each crash is restarted until the policy's window is full; then the client stops, and
    /// its stream runs on until a shutdown ends it.
    fn restarts_follow_the_policy_then_give_up() {
        let policy = restart::Policy::UpTo {
            count: 2,
            within: Duration::from_secs(60),
        };
        let mut harness = Harness::start("conversation", &[("CRASH_AFTER", "3")], |builder| {
            builder.restart(policy)
        });
        let mut updates = harness.until(running);
        let _doc = harness.open();
        updates.extend(harness.until(|update| stopped(update).is_some()));
        assert_eq!(
            statuses(&updates),
            [
                Status::Running,
                Status::Restarting { attempt: 1 },
                Status::Running,
                Status::Restarting { attempt: 1 },
                Status::Running,
                Status::Stopped(Reason::GaveUp),
            ],
            "{updates:#?}"
        );
        assert_eq!(spawns(&harness.log.lines()), 3, "three processes");
        let _ = harness.client.shutdown();
        let end = harness.to_end();
        assert_eq!(
            statuses(&end),
            [Status::Stopped(Reason::Shutdown)],
            "the stream outlived the stop: {end:#?}"
        );
    }

    /// The dead server's last stderr line reaches the host before the restart does.
    fn crash_text_arrives_before_the_status_change() {
        let mut harness = Harness::start(
            "conversation",
            &[("STDERR", "2"), ("CRASH_AFTER", "3"), ("CRASHES", "1")],
            |builder| builder,
        );
        harness.until(running);
        let _doc = harness.open();
        let updates = harness.until(restarting);
        let panicked = updates.iter().position(|update| {
            logs(update, log::Source::Stderr, "thread 'main' panicked at fake")
        });
        assert!(
            panicked.is_some_and(|at| at + 1 < updates.len()),
            "the panic text comes first: {updates:#?}"
        );
    }

    /// A grandchild of the crashed server that writes to the old stdout after the restart
    /// changes nothing: its diagnostics are neither applied nor cached.
    fn nothing_from_a_dead_server_is_applied_after_its_loss() {
        let mut harness = Harness::start(
            "conversation",
            &[
                ("GRANDCHILD", "publish"),
                ("CRASH_AFTER", "3"),
                ("CRASHES", "1"),
            ],
            |builder| builder,
        );
        harness.until(running);
        let doc = harness.open();
        let mut updates = harness.until(restarting);
        let lost_at = updates.len();
        updates.extend(harness.until(running));
        harness
            .log
            .until(|lines| lines.iter().any(|line| line == "published"));
        let request = harness.hover(&doc);
        updates.extend(harness.until(|update| hovered(update, &request).flatten().is_some()));
        assert!(
            !updates[lost_at..].iter().any(|update| diagnostics(update)
                .is_some_and(|list| list.iter().any(|d| d.message == "grandchild"))),
            "the grandchild's diagnostics are dropped: {updates:#?}"
        );
        let _ = harness.open_as(super::native::UNOPENED);
    }

    /// A grandchild holding the dead server's stdout doesn't hold up the loss beyond the
    /// grace period.
    fn a_grandchild_holding_stdout_does_not_delay_the_loss_past_the_grace() {
        let grace = Duration::from_millis(300);
        let mut harness = Harness::start(
            "conversation",
            &[
                ("GRANDCHILD", "publish"),
                ("CRASH_AFTER", "3"),
                ("CRASHES", "1"),
            ],
            |builder| builder.shutdown_grace(grace),
        );
        harness.until(running);
        let crash = Instant::now();
        let _doc = harness.open();
        harness.until(restarting);
        let elapsed = crash.elapsed();
        assert!(
            elapsed < grace + Duration::from_secs(2),
            "lost in time: {elapsed:?}"
        );
    }

    /// What the client does while the server is gone never reaches the new process: it starts
    /// with `initialize` and gets the edited text in its `didOpen`.
    fn the_new_process_receives_nothing_meant_for_the_old() {
        let mut harness = Harness::start(
            "conversation",
            &[("CRASH_AFTER", "3"), ("CRASHES", "1")],
            |builder| builder.backoff(Duration::from_millis(300)),
        );
        harness.until(running);
        let mut doc = harness.open();
        harness.until(restarting);
        doc.edit(vec![scrive_core::EditOp::insert(14, " // edited")])
            .expect("edits");
        harness.client.sync(&doc.snapshot(), doc.drain_changes());
        let request = HoverRequest::new(Counter::new().issue(doc.revision()), 5, 4..9);
        let answer = harness
            .client
            .hover(&doc.snapshot(), &request)
            .expect("the hover declines locally");
        assert!(
            matches!(answer.change(), update::Change::Hover(None)),
            "with no card"
        );
        harness.until(running);
        let request = harness.hover(&doc);
        harness.until(|update| hovered(update, &request).flatten().is_some());
        let lines = harness.log.lines();
        let respawned = spawn_lines(&lines, 1);
        assert_eq!(
            respawned.first().map(String::as_str),
            Some("recv initialize"),
            "initialize first: {lines:#?}"
        );
        let opened = respawned
            .iter()
            .position(|line| line.starts_with("recv textDocument/didOpen"))
            .expect("the document reopens");
        assert_eq!(
            respawned[opened], "recv textDocument/didOpen let value = 1; // edited",
            "with the edited text"
        );
        assert!(
            !respawned[..opened]
                .iter()
                .any(|line| line.starts_with("recv textDocument/didChange")),
            "no change before the open: {lines:#?}"
        );
    }

    /// The `initialize` reply that came just before the crash counts: the client had a
    /// handshake, so the crash restarts it.
    fn a_crash_right_after_initialize_is_restarted_not_stopped() {
        let mut harness = Harness::start(
            "conversation",
            &[("CRASH_AFTER", "1"), ("CRASHES", "1")],
            |builder| builder,
        );
        let mut updates = harness.until(running);
        updates.extend(harness.until(restarting));
        updates.extend(harness.until(running));
        assert_eq!(statuses(&updates), restarting_once(), "{updates:#?}");
    }

    /// A server that dies before its first handshake stops the client at once, which stays
    /// revivable: its stream runs on.
    fn a_crash_before_initialize_stops_without_restarting() {
        let mut harness = Harness::start("conversation", &[("CRASH_AFTER", "0")], |builder| {
            builder
        });
        let updates = harness.until(|update| stopped(update).is_some());
        assert!(
            matches!(
                statuses(&updates).as_slice(),
                [Status::Stopped(Reason::Exited { .. })]
            ),
            "stopped with the exit: {updates:#?}"
        );
        assert_eq!(spawns(&harness.log.lines()), 1, "one process");
        let _ = harness.client.shutdown();
        let end = harness.to_end();
        assert_eq!(
            statuses(&end),
            [Status::Stopped(Reason::Shutdown)],
            "the stream outlived the stop: {end:#?}"
        );
    }

    /// A shutdown during the backoff ends at once, and no process is started.
    fn shutdown_during_a_backoff_stops_at_once() {
        let mut harness = Harness::start("conversation", &[("CRASH_AFTER", "3")], |builder| {
            builder.backoff(Duration::from_secs(10))
        });
        harness.until(running);
        let _doc = harness.open();
        harness.until(restarting);
        let asked = Instant::now();
        let _ = harness.client.shutdown();
        let end = harness.to_end();
        let elapsed = asked.elapsed();
        assert_eq!(
            statuses(&end),
            [Status::Stopped(Reason::Shutdown)],
            "{end:#?}"
        );
        assert!(elapsed < Duration::from_secs(1), "at once: {elapsed:?}");
        assert_eq!(spawns(&harness.log.lines()), 1, "no respawn");
    }

    /// A respawn that races the shutdown is shut down too, and never shows as running.
    fn a_reconnect_racing_shutdown_never_shows_running() {
        let mut harness = Harness::start(
            "conversation",
            &[("CRASH_AFTER", "3"), ("CRASHES", "1")],
            |builder| builder.backoff(Duration::ZERO),
        );
        harness.until(running);
        let _doc = harness.open();
        harness.until(restarting);
        let _ = harness.client.shutdown();
        let end = harness.to_end();
        assert_eq!(
            statuses(&end),
            [Status::Stopped(Reason::Shutdown)],
            "only the shutdown: {end:#?}"
        );
    }

    /// After the policy gave up, `restart()` brings the server back for the same client.
    fn restart_after_giving_up_brings_the_server_back() {
        let policy = restart::Policy::UpTo {
            count: 2,
            within: Duration::from_secs(60),
        };
        let mut harness = Harness::start(
            "conversation",
            &[("CRASH_AFTER", "3"), ("CRASHES", "3")],
            |builder| builder.restart(policy),
        );
        let id = harness.client.id();
        harness.until(running);
        let doc = harness.open();
        let updates = harness.until(|update| stopped(update).is_some());
        assert_eq!(
            statuses(&updates).last(),
            Some(&Status::Stopped(Reason::GaveUp)),
            "{updates:#?}"
        );
        let settled = harness.client.restart().expect("stdio restarts");
        assert!(settled.is_empty(), "nothing was pending");
        let updates = harness.until(running);
        assert_eq!(
            statuses(&updates),
            [Status::Restarting { attempt: 1 }, Status::Running],
            "{updates:#?}"
        );
        let request = harness.hover(&doc);
        harness.until(|update| hovered(update, &request).flatten().is_some());
        assert_eq!(harness.client.id(), id, "the same client");
    }

    /// `restart()` replaces a healthy server: it is killed and reaped before the next starts,
    /// and the document reopens on the new one.
    fn restart_kills_a_healthy_server_and_reopens() {
        let mut harness = Harness::start("conversation", &[], |builder| builder);
        harness.until(running);
        let doc = harness.open();
        harness.until(published_fake);
        let _ = harness.client.restart().expect("stdio restarts");
        let updates = harness.until(running);
        assert_eq!(
            statuses(&updates),
            [Status::Restarting { attempt: 1 }, Status::Running],
            "{updates:#?}"
        );
        let request = harness.hover(&doc);
        harness.until(|update| hovered(update, &request).flatten().is_some());
        let lines = harness.log.lines();
        assert_eq!(spawns(&lines), 2, "two processes: {lines:#?}");
        assert!(
            spawn_lines(&lines, 1)
                .iter()
                .any(|line| line.starts_with("recv textDocument/didOpen")),
            "the document reopens: {lines:#?}"
        );
    }

    /// A server that never answers `initialize` stops the client once the deadline passes,
    /// after reporting it.
    fn an_initialize_timeout_on_the_first_start_stops() {
        let timeout = Duration::from_millis(300);
        let mut harness = Harness::start("conversation", &[("SILENT_FROM", "0")], |builder| {
            builder.initialize_timeout(timeout)
        });
        let updates = harness.until(|update| stopped(update).is_some());
        let timed_out = updates.iter().position(|update| {
            matches!(update, Update::Error(client::Error::Timeout { after }) if *after == timeout)
        });
        assert!(
            timed_out.is_some_and(|at| at + 1 < updates.len()),
            "the error comes first: {updates:#?}"
        );
        assert_eq!(
            statuses(&updates),
            [Status::Stopped(Reason::Timeout)],
            "{updates:#?}"
        );
        assert_eq!(spawns(&harness.log.lines()), 1, "one process");
    }

    /// After a restart, a server that never answers `initialize` is a loss the policy counts.
    fn an_initialize_timeout_after_a_restart_counts_against_the_policy() {
        let timeout = Duration::from_secs(2);
        let policy = restart::Policy::UpTo {
            count: 1,
            within: Duration::from_secs(60),
        };
        let mut harness = Harness::start(
            "conversation",
            &[("CRASH_AFTER", "3"), ("CRASHES", "1"), ("SILENT_FROM", "1")],
            |builder| builder.restart(policy).initialize_timeout(timeout),
        );
        let mut updates = harness.until(running);
        let _doc = harness.open();
        updates.extend(harness.until(|update| stopped(update).is_some()));
        assert_eq!(
            statuses(&updates),
            [
                Status::Running,
                Status::Restarting { attempt: 1 },
                Status::Stopped(Reason::GaveUp),
            ],
            "{updates:#?}"
        );
        assert!(
            updates
                .iter()
                .any(|update| matches!(update, Update::Error(client::Error::Timeout { .. }))),
            "the timeout is reported: {updates:#?}"
        );
    }

    /// A console program the server starts opens no console window either.
    #[cfg(windows)]
    fn a_console_grandchild_opens_no_window() {
        let mut harness = Harness::start("console-parent", &[], |builder| builder);
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
        use std::fs::OpenOptions;
        use std::io::{self, Write};
        use std::path::PathBuf;
        use std::thread;

        use scrive_lsp::lsp_server::{Connection, Message, Notification, Response};
        use serde_json::{json, Value};

        use super::KNOB;

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

        /// The `conversation` knobs, read from `SCRIVE_LSP_FAKE_*`.
        struct Knobs {
            log: Log,
            /// On `shutdown`, sleep until killed.
            ignore_shutdown: bool,
            /// On `exit` or stdin EOF, sleep until killed.
            hang_on_exit: bool,
            /// Spawns from this one on never answer `initialize`.
            silent_from: Option<usize>,
            /// Exit with code 101 after handling this many messages.
            crash_after: Option<usize>,
            /// Only spawns before this one crash.
            crashes: Option<usize>,
            /// Lines to write to stderr at start, and a panic line before a crash.
            stderr: Option<usize>,
            /// Before a crash, leave a grandchild that holds stdout and publishes on it.
            grandchild: bool,
        }

        /// The log file the harness reads, if it set one.
        struct Log(Option<PathBuf>);

        impl Knobs {
            fn read() -> Self {
                let knob = |name: &str| std::env::var(format!("{KNOB}{name}")).ok();
                let number = |name: &str| {
                    knob(name).map(|value| value.parse().expect("the knob is a number"))
                };
                Self {
                    log: Log(knob("LOG").map(PathBuf::from)),
                    ignore_shutdown: knob("IGNORE_SHUTDOWN").is_some(),
                    hang_on_exit: knob("HANG_ON_EXIT").is_some(),
                    silent_from: number("SILENT_FROM"),
                    crash_after: number("CRASH_AFTER"),
                    crashes: number("CRASHES"),
                    stderr: number("STDERR"),
                    grandchild: knob("GRANDCHILD").as_deref() == Some("publish"),
                }
            }

            /// Whether spawn `spawn` crashes once it handled `handled` messages.
            fn crashes(&self, spawn: usize, handled: usize) -> bool {
                self.crash_after == Some(handled) && self.crashes.is_none_or(|k| spawn < k)
            }

            /// Crashes as configured: panic text, a grandchild, exit 101.
            fn crash(&self) -> ! {
                if self.stderr.is_some() {
                    eprintln!("thread 'main' panicked at fake");
                }
                if self.grandchild {
                    // Outliving this process, which exits at once, is the grandchild's job.
                    #[allow(clippy::zombie_processes)]
                    std::process::Command::new(
                        std::env::current_exe().expect("the test binary has a path"),
                    )
                    .env(super::GRANDCHILD, "1")
                    .stdin(std::process::Stdio::null())
                    .spawn()
                    .expect("the grandchild starts");
                }
                self.log.exit(101)
            }
        }

        impl Log {
            /// Appends `line` in one write, so lines from several processes never interleave.
            fn line(&self, line: &str) {
                let Some(path) = &self.0 else {
                    return;
                };
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .and_then(|mut file| file.write_all(format!("{line}\n").as_bytes()))
                    .expect("the log is writable");
            }

            /// How many spawns logged before this one. Spawns never overlap: the client kills
            /// and reaps a server before it starts the next.
            fn spawns(&self) -> usize {
                let Some(path) = &self.0 else {
                    return 0;
                };
                std::fs::read_to_string(path)
                    .unwrap_or_default()
                    .lines()
                    .filter(|line| line.starts_with("spawn "))
                    .count()
            }

            /// Logs the exit, then exits.
            fn exit(&self, code: i32) -> ! {
                self.line(&format!("exit {code}"));
                std::process::exit(code)
            }
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

        /// A server with hover and full sync that publishes one `fake` diagnostic per opened
        /// document and answers hovers with the client's `processId`. It reads and writes on
        /// this one thread, so every reply is written before it exits.
        fn conversation(script: Script) {
            let knobs = Knobs::read();
            let spawn = knobs.log.spawns();
            knobs.log.line(&format!("spawn {spawn}"));
            let silent = knobs.silent_from.is_some_and(|from| spawn >= from);
            let mut stdout = io::stdout().lock();
            stdout
                .write_all(b"fake server banner\n")
                .and_then(|()| stdout.flush())
                .expect("stdout is open");
            eprintln!("fake server ready");
            for line in 0..knobs.stderr.unwrap_or(0) {
                eprintln!("fake stderr line {line}");
            }
            if knobs.crashes(spawn, 0) {
                knobs.crash();
            }
            let mut stdin = io::stdin().lock();
            let mut pid = Value::Null;
            let mut shut = false;
            let mut handled = 0;
            while let Ok(Some(message)) = Message::read(&mut stdin) {
                knobs.log.line(&format!("recv {}", received(&message)));
                handled += 1;
                let crash = knobs.crashes(spawn, handled);
                match message {
                    Message::Request(request) => {
                        let result = match request.method.as_str() {
                            "initialize" if silent => continue,
                            "initialize" => {
                                pid = request.params["processId"].clone();
                                #[cfg(windows)]
                                if script == Script::ConsoleParent {
                                    console_parent();
                                }
                                json!({ "capabilities": {
                                    "hoverProvider": true,
                                    "textDocumentSync": 1,
                                } })
                            }
                            "shutdown" if knobs.ignore_shutdown => park(),
                            "shutdown" => {
                                shut = true;
                                Value::Null
                            }
                            "textDocument/hover" if script == Script::CrashOnHover => {
                                eprintln!("crashing");
                                knobs.log.exit(3)
                            }
                            "textDocument/hover" => {
                                let value = format!("pid {pid}");
                                json!({ "contents": { "kind": "markdown", "value": value } })
                            }
                            _ => Value::Null,
                        };
                        write(&mut stdout, Response::new_ok(request.id, result).into());
                    }
                    Message::Notification(notification) if notification.method == "exit" => {
                        if knobs.hang_on_exit {
                            park()
                        }
                        knobs.log.exit(if shut { 0 } else { 1 })
                    }
                    Message::Notification(notification)
                        if notification.method == "textDocument/didOpen" =>
                    {
                        let document = &notification.params["textDocument"];
                        write(&mut stdout, publish(&document["uri"], &document["version"], "fake"));
                    }
                    Message::Notification(_) | Message::Response(_) => {}
                }
                if crash {
                    knobs.crash();
                }
            }
            if knobs.hang_on_exit {
                park()
            }
            knobs.log.exit(1)
        }

        /// A crashed server's grandchild: once the server was started again, it writes
        /// diagnostics to the dead server's stdout, which it inherited, then lingers a moment.
        pub fn grandchild() {
            let log = Knobs::read().log;
            let respawned = (0..1000).any(|_| {
                if log.spawns() > 1 {
                    return true;
                }
                thread::sleep(std::time::Duration::from_millis(10));
                false
            });
            if !respawned {
                return;
            }
            let mut stdout = io::stdout().lock();
            for name in ["fake.rs", super::UNOPENED] {
                let uri = super::temp_uri(name);
                write(&mut stdout, publish(&json!(uri.as_str()), &Value::Null, "grandchild"));
            }
            log.line("published");
            thread::sleep(std::time::Duration::from_secs(1));
        }

        /// One `fake` diagnostic over `value` for `uri`, at `version`.
        fn publish(uri: &Value, version: &Value, message: &str) -> Message {
            let params = json!({
                "uri": uri,
                "version": version,
                "diagnostics": [{
                    "range": {
                        "start": { "line": 0, "character": 4 },
                        "end": { "line": 0, "character": 9 },
                    },
                    "severity": 1,
                    "message": message,
                }],
            });
            Notification::new("textDocument/publishDiagnostics".to_owned(), params).into()
        }

        /// Runs a console program and waits for it.
        #[cfg(windows)]
        fn console_parent() {
            let report = std::process::Command::new(
                std::env::current_exe().expect("the test binary has a path"),
            )
            .env(super::FAKE, "console-report")
            .status()
            .expect("the console program runs");
            assert!(report.success(), "the console program reports");
        }

        /// How the log names a message: its method, or `reply <id>`.
        fn received(message: &Message) -> String {
            match message {
                Message::Request(request) => request.method.clone(),
                Message::Notification(notification)
                    if notification.method == "textDocument/didOpen" =>
                {
                    let text = &notification.params["textDocument"]["text"];
                    format!("{} {}", notification.method, text.as_str().unwrap_or(""))
                }
                Message::Notification(notification) => notification.method.clone(),
                Message::Response(response) => format!("reply {}", response.id),
            }
        }

        fn write(stdout: &mut impl Write, message: Message) {
            message
                .write(stdout)
                .and_then(|()| stdout.flush())
                .expect("stdout is open");
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
