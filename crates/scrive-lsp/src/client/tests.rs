use std::str::FromStr;

use futures::{FutureExt, StreamExt};
use scrive_core::intel::ticket::Counter;
use scrive_core::{Document, EditOp, HoverRequest};
use serde_json::{json, Value};

use super::*;

/// A client on the memory bridge, with the server end in the test's hands.
struct Wire {
    client: Client,
    events: Events,
    server: lsp_server::Connection,
}

impl Wire {
    fn new(builder: Builder) -> Self {
        let (near, server) = lsp_server::Connection::memory();
        let (client, events) = builder.memory(near);
        Self {
            client,
            events,
            server,
        }
    }

    /// What the client sent since the last call, as JSON.
    fn sent(&self) -> Vec<Value> {
        self.server
            .receiver
            .try_iter()
            .map(|message| serde_json::to_value(message).expect("lsp-server messages serialize"))
            .collect()
    }

    /// Sends `message` as the server.
    fn reply(&self, message: Value) {
        let message = serde_json::from_value(message).expect("fixtures are lsp-server messages");
        self.server
            .sender
            .send(message)
            .expect("the client end is open");
    }

    /// Folds every event the stream holds right now into the client.
    fn drain(&mut self) -> Vec<Update> {
        let mut updates = Vec::new();
        while let Some(Some(event)) = self.events.next().now_or_never() {
            updates.extend(self.client.receive(event));
        }
        updates
    }

    /// The stream has ended.
    fn ended(&mut self) -> bool {
        matches!(self.events.next().now_or_never(), Some(None))
    }

    /// Answers `initialize` with `capabilities` and drains the handshake.
    fn handshake(&mut self, capabilities: Value) -> Vec<Update> {
        self.reply(json!({"id": 1, "result": {"capabilities": capabilities}}));
        self.drain()
    }
}

/// A server with incremental sync, open/close notifications and hover.
fn capabilities() -> Value {
    json!({"textDocumentSync": {"openClose": true, "change": 2}, "hoverProvider": true})
}

fn uri(text: &str) -> Uri {
    Uri::from_str(text).expect("fixture URI parses")
}

fn document(text: &str) -> Document {
    let mut doc = Document::new(text).expect("fixture loads");
    doc.observe_changes(true);
    doc
}

/// The methods of `messages`, `response` for replies.
fn methods(messages: &[Value]) -> Vec<&str> {
    messages
        .iter()
        .map(|message| message["method"].as_str().unwrap_or("response"))
        .collect()
}

/// A compact description of `update`, for asserting sequences.
fn summary(update: &Update) -> String {
    match update {
        Update::Document(document) => match document.change() {
            update::Change::Diagnostics(set) => format!("diagnostics({})", set.len()),
            update::Change::Hover(card) => format!("hover({})", card.is_some()),
            update::Change::Definition(target) => format!("definition({})", target.is_some()),
            other => format!("{other:?}"),
        },
        Update::FileEdits(_) => "file edits".to_owned(),
        Update::Status(status) => format!("{status:?}"),
        Update::Log(entry) => format!("log({})", entry.text()),
        Update::Error(error) => format!("error({error})"),
        Update::Notification(notification) => format!("notification({})", notification.method()),
        Update::Trace(entry) => format!(
            "trace({:?}, {})",
            entry.direction(),
            entry.method().unwrap_or("-")
        ),
    }
}

fn summaries(updates: &[Update]) -> Vec<String> {
    updates.iter().map(summary).collect()
}

/// A hover request for `value` in `doc` (`let value = 1;`), under a fresh ticket.
fn hover_request(tickets: &mut Counter, doc: &Document) -> HoverRequest {
    HoverRequest::new(tickets.issue(doc.revision()), 5, 4..9)
}

/// A running client with `let value = 1;` open as `file:///a.rs`.
fn running() -> (Wire, Document) {
    let doc = document("let value = 1;");
    let mut wire = Wire::new(Builder::default());
    let _ = wire
        .client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    let _ = wire.handshake(capabilities());
    let _ = wire.sent();
    (wire, doc)
}

/// The whole conversation on the memory bridge: initialize, open, hover, diagnostics, shutdown.
#[test]
fn memory_runs_initialize_open_hover_diagnostics_and_shutdown() {
    let mut tickets = Counter::new();
    let doc = document("let value = 1;");
    let mut wire = Wire::new(Builder::default());
    let [initialize] = wire.sent().try_into().expect("initialize goes out alone");
    assert_eq!(
        initialize["method"], "initialize",
        "initialize goes out first"
    );
    assert_eq!(
        initialize["params"]["processId"],
        Value::Null,
        "no process id"
    );
    assert!(
        wire.client
            .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
            .expect("opens")
            .is_none(),
        "nothing was cached"
    );

    let updates = wire.handshake(capabilities());
    assert_eq!(
        summaries(&updates),
        ["Running"],
        "the handshake runs the client"
    );
    assert_eq!(
        methods(&wire.sent()),
        ["initialized", "textDocument/didOpen"],
        "initialized, then the deferred didOpen"
    );

    let request = hover_request(&mut tickets, &doc);
    assert!(
        wire.client.hover(&doc.snapshot(), &request).is_none(),
        "the hover is the server's to answer"
    );
    let [hover] = wire.sent().try_into().expect("the hover goes out");
    wire.reply(json!({"id": hover["id"], "result": {"contents": "a value"}}));
    assert_eq!(summaries(&wire.drain()), ["hover(true)"], "the hover lands");

    wire.reply(
        json!({"method": "textDocument/publishDiagnostics", "params": {
            "uri": "file:///a.rs", "version": 1, "diagnostics": [{
                "range": {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 9}},
                "message": "unused",
            }],
        }}),
    );
    assert_eq!(
        summaries(&wire.drain()),
        ["diagnostics(1)"],
        "the diagnostics land"
    );

    assert!(wire.client.shutdown().is_empty(), "nothing was in flight");
    assert_eq!(
        wire.sent(),
        [
            json!({"id": "scrive-lsp/shutdown", "method": "shutdown"}),
            json!({"method": "exit"})
        ],
        "shutdown, then exit"
    );
    assert_eq!(
        summaries(&wire.drain()),
        ["diagnostics(0)", "Stopped(Shutdown)"],
        "the diagnostics clear, then the client stops"
    );
    assert!(wire.ended(), "the stream ends");
}

/// `shutdown()` sends the exchange and ends the stream; a second call does nothing.
#[test]
fn shutdown_sends_shutdown_then_exit_and_ends_the_stream() {
    let (mut wire, _) = running();
    let _ = wire.client.shutdown();
    assert_eq!(
        methods(&wire.sent()),
        ["shutdown", "exit"],
        "shutdown, then exit"
    );
    assert!(
        wire.client.shutdown().is_empty(),
        "nothing is left to settle"
    );
    assert!(wire.sent().is_empty(), "the second call sends nothing");
    assert_eq!(
        summaries(&wire.drain()),
        ["diagnostics(0)", "Stopped(Shutdown)"],
        "one stop, not two"
    );
    assert!(wire.ended(), "the stream ends");
}

/// Before the handshake a server takes nothing but `exit`.
#[test]
fn shutdown_before_the_handshake_sends_only_exit() {
    let mut wire = Wire::new(Builder::default());
    let _ = wire.sent();
    assert!(wire.client.shutdown().is_empty(), "nothing was in flight");
    assert_eq!(methods(&wire.sent()), ["exit"], "only exit");
    assert_eq!(
        summaries(&wire.drain()),
        ["Stopped(Shutdown)"],
        "the client stops"
    );
    assert!(wire.ended(), "the stream ends");
}

/// A hover in flight at shutdown settles with no card, handed back for the editor.
#[test]
fn shutdown_settles_a_pending_hover_with_none() {
    let mut tickets = Counter::new();
    let (mut wire, doc) = running();
    let request = hover_request(&mut tickets, &doc);
    let _ = wire.client.hover(&doc.snapshot(), &request);
    let settled = wire.client.shutdown();
    let [document] = settled.as_slice() else {
        panic!("expected one settle, got {settled:?}")
    };
    assert_eq!(
        document.stamp(),
        update::Stamp::Ticket(request.ticket),
        "under the request's ticket"
    );
    assert!(
        matches!(document.change(), update::Change::Hover(None)),
        "with no card"
    );
}

/// A server that drops its end settles what was in flight, clears diagnostics and stops the
/// client.
#[test]
fn a_dropped_server_settles_pending_requests_and_stops_closed() {
    let mut tickets = Counter::new();
    let (mut wire, doc) = running();
    let request = hover_request(&mut tickets, &doc);
    let _ = wire.client.hover(&doc.snapshot(), &request);
    let Wire {
        mut client,
        mut events,
        server,
    } = wire;
    drop(server);
    let mut updates = Vec::new();
    while let Some(Some(event)) = events.next().now_or_never() {
        updates.extend(client.receive(event));
    }
    assert_eq!(
        summaries(&updates),
        ["hover(false)", "diagnostics(0)", "Stopped(Closed)"],
        "settle, clear, stop"
    );
    assert!(
        matches!(events.next().now_or_never(), Some(None)),
        "the stream ends"
    );
    assert_eq!(
        client.status(),
        &Status::Stopped(Reason::Closed),
        "the client is stopped"
    );
}

/// A failed `initialize` is reported, then stops the client; the document stays registered and
/// its requests decline.
#[test]
fn a_failed_initialize_reports_the_error_then_stops() {
    let mut tickets = Counter::new();
    let mut doc = document("let value = 1;");
    let mut wire = Wire::new(Builder::default());
    let _ = wire
        .client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    let _ = wire.sent();
    wire.reply(json!({"id": 1, "error": {"code": -32603, "message": "no"}}));
    let updates = wire.drain();
    let [Update::Error(Error::Server {
        doc_id: None,
        method,
        error,
    }), cleared, stopped] = updates.as_slice()
    else {
        panic!("expected an error, a clear and a stop, got {updates:?}")
    };
    assert_eq!(method, "initialize", "the failed request");
    assert_eq!(error.code(), -32603, "the server's code");
    assert_eq!(summary(cleared), "diagnostics(0)", "the diagnostics clear");
    assert_eq!(summary(stopped), "Stopped(Initialize)", "the client stops");
    assert!(wire.ended(), "the stream ends");

    let other = document("b");
    assert!(
        matches!(
            wire.client
                .open(&other.snapshot(), &uri("file:///a.rs"), "rust"),
            Err(Error::DuplicateUri { .. })
        ),
        "the document is still registered"
    );
    doc.edit(vec![EditOp::insert(14, " ")]).expect("edits");
    wire.client.sync(&doc.snapshot(), doc.drain_changes());
    let answer = wire
        .client
        .hover(&doc.snapshot(), &hover_request(&mut tickets, &doc))
        .expect("the hover declines locally");
    assert!(
        matches!(answer.change(), update::Change::Hover(None)),
        "with no card"
    );
    assert!(wire.sent().is_empty(), "nothing reaches the server");
}

/// With tracing on, both directions are recorded, and replies are named by their request.
#[test]
fn traces_record_both_directions_and_name_replies() {
    let mut wire = Wire::new(Builder::default().trace(trace::Mode::Messages));
    let first = wire.drain();
    assert_eq!(
        summaries(&first),
        ["trace(Outgoing, initialize)"],
        "the initialize trace comes first"
    );
    let updates = wire.handshake(capabilities());
    assert_eq!(
        summaries(&updates)[0],
        "trace(Incoming, initialize)",
        "the reply is named after its request"
    );
    assert!(
        summaries(&updates).contains(&"trace(Outgoing, initialized)".to_owned()),
        "initialized is traced: {:?}",
        summaries(&updates)
    );
    wire.reply(json!({"id": 7, "method": "workspace/configuration", "params": {"items": []}}));
    assert_eq!(
        summaries(&wire.drain()),
        [
            "trace(Incoming, workspace/configuration)",
            "trace(Outgoing, workspace/configuration)"
        ],
        "the answer is named after the request it answers"
    );
}

/// A message that fails is traced all the same, before its error.
#[test]
fn an_incoming_trace_comes_first_even_when_the_message_fails() {
    let mut wire = Wire::new(Builder::default().trace(trace::Mode::Messages));
    let _ = wire.handshake(capabilities());
    wire.reply(
        json!({"method": "textDocument/publishDiagnostics", "params": {
            "uri": "file:///a.rs", "diagnostics": 5,
        }}),
    );
    let updates = wire.drain();
    assert_eq!(
        summary(&updates[0]),
        "trace(Incoming, textDocument/publishDiagnostics)",
        "the trace comes first"
    );
    assert!(
        matches!(&updates[1..], [Update::Error(Error::Decode { method, .. })] if method == "textDocument/publishDiagnostics"),
        "then the decode error, got {updates:?}"
    );
}

/// A reply the client no longer waits for has no method to name.
#[test]
fn a_reply_nothing_waits_for_is_traced_without_a_method() {
    let mut wire = Wire::new(Builder::default().trace(trace::Mode::Messages));
    let _ = wire.handshake(capabilities());
    wire.reply(json!({"id": 99, "result": null}));
    assert_eq!(
        summaries(&wire.drain()),
        ["trace(Incoming, -)"],
        "only the trace"
    );
}

/// A server-specific notification.
enum Ping {}

impl lsp_types::notification::Notification for Ping {
    type Params = Value;
    const METHOD: &'static str = "custom/ping";
}

/// `notify` reaches only a running server.
#[test]
fn notify_sends_only_while_running() {
    let mut wire = Wire::new(Builder::default());
    let _ = wire.sent();
    wire.client.notify::<Ping>(json!({"n": 1}));
    assert!(wire.sent().is_empty(), "nothing before the handshake");
    let _ = wire.handshake(capabilities());
    let _ = wire.sent();
    wire.client.notify::<Ping>(json!({"n": 2}));
    assert_eq!(
        wire.sent(),
        [json!({"method": "custom/ping", "params": {"n": 2}})],
        "one custom notification"
    );
}

/// `configure` is told to a running server, and only stored before the handshake.
#[test]
fn configure_reaches_the_server_only_while_running() {
    let mut wire = Wire::new(Builder::default());
    let _ = wire.sent();
    wire.client.configure(json!({"x": 1}));
    assert!(wire.sent().is_empty(), "nothing before the handshake");
    let _ = wire.handshake(capabilities());
    assert_eq!(
        methods(&wire.sent()),
        ["initialized", "workspace/didChangeConfiguration"],
        "the handshake pushes the settings"
    );
    wire.client.configure(json!({"x": 2}));
    assert_eq!(
        wire.sent(),
        [json!({"method": "workspace/didChangeConfiguration", "params": {"settings": {"x": 2}}})],
        "a running server is told"
    );
}

/// A stop delivered twice changes nothing the second time.
#[test]
fn a_cloned_stopped_event_changes_nothing() {
    let (wire, _) = running();
    let Wire {
        mut client,
        mut events,
        server,
    } = wire;
    drop(server);
    let event = events
        .next()
        .now_or_never()
        .flatten()
        .expect("the stop is ready");
    let again = event.clone();
    assert_eq!(
        summaries(&client.receive(event)),
        ["diagnostics(0)", "Stopped(Closed)"],
        "the stop lands"
    );
    assert!(
        client.receive(again).is_empty(),
        "its clone changes nothing"
    );
    assert_eq!(
        client.status(),
        &Status::Stopped(Reason::Closed),
        "the status stays"
    );
}

/// `status()` agrees with the last status update the host saw.
#[test]
fn status_follows_the_updates() {
    let mut wire = Wire::new(Builder::default());
    assert_eq!(
        wire.client.status(),
        &Status::Starting,
        "every client starts"
    );
    let _ = wire.handshake(capabilities());
    assert_eq!(
        wire.client.status(),
        &Status::Running,
        "after the handshake"
    );
    let _ = wire.client.shutdown();
    assert_eq!(
        wire.client.status(),
        &Status::Running,
        "until the stop is received"
    );
    let _ = wire.drain();
    assert_eq!(
        wire.client.status(),
        &Status::Stopped(Reason::Shutdown),
        "after the stop"
    );
}

/// A dropped client says goodbye to the server, and its stream ends.
#[test]
fn dropping_the_client_says_goodbye_and_ends_the_stream() {
    let (wire, _) = running();
    let Wire {
        client,
        mut events,
        server,
    } = wire;
    drop(client);
    let sent: Vec<Value> = server
        .receiver
        .try_iter()
        .map(|message| serde_json::to_value(message).expect("lsp-server messages serialize"))
        .collect();
    assert_eq!(methods(&sent), ["shutdown", "exit"], "shutdown, then exit");
    assert!(
        matches!(events.next().now_or_never(), Some(None)),
        "the stream ends"
    );
}

/// `Event` can be an iced message on every target, and `Events` can be polled in place.
#[test]
fn events_and_event_have_the_bounds_hosts_need() {
    fn message<T: Send + Clone + core::fmt::Debug + 'static>() {}
    fn unpin<T: Unpin>() {}
    message::<Event>();
    unpin::<Events>();
    #[cfg(not(target_family = "wasm"))]
    {
        fn send<T: Send>() {}
        send::<Events>();
    }
}

/// A megabyte message prints as a short summary.
#[test]
fn event_debug_prints_a_summary_only() {
    let mut wire = Wire::new(Builder::default());
    let _ = wire.handshake(capabilities());
    wire.reply(json!({"method": "custom/blob", "params": {"blob": "x".repeat(1 << 20)}}));
    let event = wire
        .events
        .next()
        .now_or_never()
        .flatten()
        .expect("the message is ready");
    let printed = format!("{event:?}");
    assert!(
        printed.len() < 200,
        "a summary, got {} bytes",
        printed.len()
    );
    assert!(printed.contains("bytes"), "with the size: {printed}");
}

/// `Failed` compares by its error's kind, so hosts can compare statuses.
#[test]
fn status_and_reason_compare_failed_by_error_kind() {
    let failed = |kind, text: &str| {
        Status::Stopped(Reason::Failed(Arc::new(std::io::Error::new(kind, text))))
    };
    assert_eq!(
        failed(std::io::ErrorKind::NotFound, "a"),
        failed(std::io::ErrorKind::NotFound, "b"),
        "the same kind is equal"
    );
    assert_ne!(
        failed(std::io::ErrorKind::NotFound, "a"),
        failed(std::io::ErrorKind::Other, "a"),
        "another kind is not"
    );
}
