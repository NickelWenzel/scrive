//! A stdio client against a scripted worker: the test plays the bridge, feeding transport
//! events through `receive` and reading what the client tells the worker and writes.

use std::str::FromStr;
use std::time::Duration;

use futures::{FutureExt, StreamExt};
use scrive_core::intel::ticket::Counter;
use scrive_core::{Document, EditOp, HoverRequest};
use serde_json::{json, Value};

use super::*;
use crate::client::{event, Builder, Event, Id};
use crate::transport::stdio::tap;
use crate::{trace, update};

/// A stdio-flavoured client whose worker and connections are the test's.
struct Scripted {
    client: Client,
    inbox: tap::Inbox,
    queue: tap::Queue,
    /// What the client queued on its `Events`.
    local: futures_channel::mpsc::UnboundedReceiver<Event>,
}

impl Scripted {
    /// A client that sent `initialize` on connection 0, with nothing else queued.
    fn new(policy: restart::Policy) -> Self {
        Self::with(policy, None)
    }

    fn with(policy: restart::Policy, initialize_timeout: Option<Duration>) -> Self {
        let (session, initialize) = Builder::default().session(Some(1));
        let (link, queue) = tap::Queue::link(Generation::FIRST);
        let (control, inbox) = tap::Inbox::control();
        let (local, queued) = futures_channel::mpsc::unbounded();
        let mut client = Client::new(
            Id::next(),
            session,
            link,
            transport::Control::Stdio(control),
            local,
            trace::Mode::Off,
            State::new(policy, initialize_timeout),
        );
        client.send(vec![initialize], None);
        let _ = queue.items();
        Self {
            client,
            inbox,
            queue,
            local: queued,
        }
    }

    /// A running client with `let value = 1;` open as `file:///a.rs`.
    fn running(policy: restart::Policy) -> (Self, Document) {
        Self::running_with(policy, None)
    }

    fn running_with(
        policy: restart::Policy,
        initialize_timeout: Option<Duration>,
    ) -> (Self, Document) {
        let mut scripted = Self::with(policy, initialize_timeout);
        let doc = document("let value = 1;");
        let _ = scripted
            .client
            .open(&doc.snapshot(), &uri(), "rust")
            .expect("opens");
        let updates = scripted.message(
            0,
            json!({"id": 1, "result": {"capabilities": capabilities()}}),
        );
        assert!(
            summaries(&updates).ends_with(&["Running".to_owned()]),
            "the handshake runs the client: {updates:?}"
        );
        let _ = scripted.queue.items();
        let _ = scripted.inbox.lifecycles();
        (scripted, doc)
    }

    fn deliver(&mut self, event: transport::Event) -> Vec<Update> {
        let id = self.client.id;
        self.client
            .receive(Event::new(id, event::Payload::Transport(event)))
    }

    /// `message` from the server, on connection `generation`.
    fn message(&mut self, generation: u64, message: Value) -> Vec<Update> {
        self.deliver(transport::Event::Message {
            generation: nth(generation),
            body: message.to_string().into_bytes().into(),
        })
    }

    fn lost(&mut self, generation: u64, reason: Reason) -> Vec<Update> {
        self.deliver(transport::Event::Lost {
            generation: nth(generation),
            reason,
        })
    }

    /// Connection `generation` comes up, with its queue in the test's hands.
    fn reconnected(&mut self, generation: u64) -> (Vec<Update>, tap::Queue) {
        let (link, queue) = tap::Queue::link(nth(generation));
        let updates = self.deliver(transport::Event::Reconnected {
            generation: nth(generation),
            link,
        });
        (updates, queue)
    }

    /// What the client queued on its `Events` since the last call, folded in.
    fn queued(&mut self) -> Vec<Update> {
        let mut updates = Vec::new();
        while let Some(Some(event)) = self.local.next().now_or_never() {
            updates.extend(self.client.receive(event));
        }
        updates
    }

    fn attempting(&mut self, generation: u64) -> Vec<Update> {
        self.deliver(transport::Event::Attempting {
            generation: nth(generation),
            failure: Arc::new(io::Error::from(io::ErrorKind::NotFound)),
        })
    }
}

/// Generation `n`.
fn nth(n: u64) -> Generation {
    (0..n).fold(Generation::FIRST, |generation, _| generation.next())
}

fn uri() -> lsp_types::Uri {
    lsp_types::Uri::from_str("file:///a.rs").expect("fixture URI parses")
}

fn document(text: &str) -> Document {
    let mut doc = Document::new(text).expect("fixture loads");
    doc.observe_changes(true);
    doc
}

fn capabilities() -> Value {
    json!({"textDocumentSync": {"openClose": true, "change": 2}, "hoverProvider": true})
}

fn hover(doc: &Document) -> HoverRequest {
    HoverRequest::new(Counter::new().issue(doc.revision()), 5, 4..9)
}

/// The methods of what was queued, `close` for the close and `reply` for answers.
fn methods(items: &[Option<Value>]) -> Vec<&str> {
    items
        .iter()
        .map(|item| match item {
            Some(body) => body["method"].as_str().unwrap_or("reply"),
            None => "close",
        })
        .collect()
}

/// A compact description of `update`, for asserting sequences.
fn summary(update: &Update) -> String {
    match update {
        Update::Document(document) => match document.change() {
            update::Change::Diagnostics(set) => format!("diagnostics({})", set.len()),
            update::Change::Hover(card) => format!("hover({})", card.is_some()),
            other => format!("{other:?}"),
        },
        Update::Status(status) => format!("{status:?}"),
        Update::Log(entry) => format!("log({})", entry.text()),
        Update::Error(error) => format!("error({error})"),
        other => format!("{other:?}"),
    }
}

fn summaries(updates: &[Update]) -> Vec<String> {
    updates.iter().map(summary).collect()
}

fn exited(code: i32) -> Reason {
    Reason::Exited {
        code: Some(code),
        signal: None,
    }
}

/// Answers the `initialize` queued on `queue` with the test capabilities.
fn handshake(scripted: &mut Scripted, generation: u64, queue: &tap::Queue) -> Vec<Update> {
    let items = queue.items();
    let [Some(initialize)] = items.as_slice() else {
        panic!("expected initialize alone, got {items:?}")
    };
    assert_eq!(initialize["method"], "initialize", "a fresh initialize");
    let id = initialize["id"].clone();
    scripted.message(
        generation,
        json!({"id": id, "result": {"capabilities": capabilities()}}),
    )
}

/// The first handshake tells the worker, so it can disarm its deadline and reset its backoff.
#[test]
fn the_handshake_is_reported_to_the_worker() {
    let mut scripted = Scripted::new(restart::Policy::default());
    let updates = scripted.message(0, json!({"id": 1, "result": {"capabilities": {}}}));
    assert_eq!(summaries(&updates), ["Running"], "running");
    assert_eq!(
        scripted.inbox.lifecycles(),
        [Lifecycle::Handshaken(nth(0))],
        "the worker hears of it"
    );
}

/// A loss after the handshake settles what was pending, clears diagnostics, and restarts.
#[test]
fn a_loss_settles_pending_requests_and_restarts() {
    let (mut scripted, doc) = Scripted::running(restart::Policy::default());
    let request = hover(&doc);
    assert!(
        scripted.client.hover(&doc.snapshot(), &request).is_none(),
        "the hover goes out"
    );
    let updates = scripted.lost(1, exited(101));
    assert_eq!(
        summaries(&updates),
        [
            "hover(false)",
            "diagnostics(0)",
            "Restarting { attempt: 1 }"
        ],
        "settle, clear, restart"
    );
    assert_eq!(
        scripted.inbox.lifecycles(),
        [Lifecycle::Reconnect(nth(1))],
        "the worker reconnects"
    );
    assert_eq!(
        scripted.client.status(),
        &Status::Restarting { attempt: 1 },
        "restarting"
    );
}

/// An unresponsive server is restarted like a crashed one.
#[test]
fn an_unresponsive_server_is_restarted_too() {
    let (mut scripted, _) = Scripted::running(restart::Policy::default());
    let updates = scripted.lost(1, Reason::Unresponsive);
    assert_eq!(
        summaries(&updates).last().map(String::as_str),
        Some("Restarting { attempt: 1 }"),
        "restarting: {updates:?}"
    );
    assert_eq!(
        scripted.inbox.lifecycles(),
        [Lifecycle::Reconnect(nth(1))],
        "the worker reconnects"
    );
}

/// The new connection gets a fresh `initialize`, and its reply runs the client again, with
/// every document reopened.
#[test]
fn a_reconnected_server_is_reinitialized_and_runs() {
    let (mut scripted, _) = Scripted::running(restart::Policy::default());
    let _ = scripted.lost(1, exited(101));
    let _ = scripted.inbox.lifecycles();
    let (updates, queue) = scripted.reconnected(1);
    assert!(updates.is_empty(), "nothing to report yet: {updates:?}");
    assert_eq!(
        scripted.client.status(),
        &Status::Restarting { attempt: 1 },
        "still restarting"
    );
    let updates = handshake(&mut scripted, 1, &queue);
    assert_eq!(summaries(&updates), ["Running"], "running again");
    assert_eq!(
        methods(&queue.items()),
        ["initialized", "textDocument/didOpen"],
        "the document reopens"
    );
    assert_eq!(
        scripted.inbox.lifecycles(),
        [Lifecycle::Handshaken(nth(1))],
        "the worker hears of the handshake"
    );
}

/// A `Reconnected` delivered twice doesn't start the session over again.
#[test]
fn a_duplicated_reconnected_does_not_reinitialize() {
    let (mut scripted, _) = Scripted::running(restart::Policy::default());
    let _ = scripted.lost(1, exited(101));
    let (link, queue) = tap::Queue::link(nth(1));
    let reconnected = transport::Event::Reconnected {
        generation: nth(1),
        link,
    };
    let _ = scripted.deliver(reconnected.clone());
    assert_eq!(methods(&queue.items()), ["initialize"], "one initialize");
    assert!(
        scripted.deliver(reconnected).is_empty(),
        "the clone changes nothing"
    );
    assert!(queue.items().is_empty(), "no second initialize");
}

/// What the dead connection still delivers is dropped once the new one is held.
#[test]
fn traffic_from_a_dead_connection_is_dropped() {
    let (mut scripted, _) = Scripted::running(restart::Policy::default());
    let _ = scripted.lost(1, exited(101));
    let (_, queue) = scripted.reconnected(1);
    let late = json!({"method": "textDocument/publishDiagnostics", "params": {
        "uri": "file:///a.rs", "diagnostics": [{
            "range": {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 9}},
            "message": "stale",
        }],
    }});
    assert!(scripted.message(0, late).is_empty(), "filtered");
    let error = transport::Event::Error {
        generation: nth(0),
        error: Error::Oversized { length: 1 << 30 },
    };
    assert!(scripted.deliver(error).is_empty(), "its errors too");
    let _ = queue.items();
}

/// The dead server's stderr is what the host needs to see, so it is never dropped.
#[test]
fn stderr_of_a_dead_connection_is_still_delivered() {
    let (mut scripted, _) = Scripted::running(restart::Policy::default());
    let _ = scripted.lost(1, exited(101));
    let _ = scripted.reconnected(1);
    let entry = crate::log::Entry::stderr("late panic text".to_owned());
    let updates = scripted.deliver(transport::Event::Log(Arc::from([entry])));
    assert_eq!(summaries(&updates), ["log(late panic text)"], "delivered");
}

/// A loss delivered twice is decided once.
#[test]
fn a_duplicated_lost_is_ignored() {
    let (mut scripted, _) = Scripted::running(restart::Policy::default());
    let _ = scripted.lost(1, exited(101));
    let (_, queue) = scripted.reconnected(1);
    let _ = handshake(&mut scripted, 1, &queue);
    let _ = scripted.inbox.lifecycles();
    assert!(scripted.lost(1, exited(101)).is_empty(), "already decided");
    assert!(scripted.inbox.lifecycles().is_empty(), "nothing sent");
    assert_eq!(scripted.client.status(), &Status::Running, "still running");
}

/// A server that never completed `initialize` is not restarted, and a reconnect that raced
/// the stop is dropped.
#[test]
fn a_loss_before_the_first_handshake_stops_without_restarting() {
    let mut scripted = Scripted::new(restart::Policy::default());
    let updates = scripted.lost(1, exited(1));
    assert_eq!(
        summaries(&updates),
        ["Stopped(Exited { code: Some(1), signal: None })"],
        "stopped with the loss's reason"
    );
    assert_eq!(
        scripted.inbox.lifecycles(),
        [Lifecycle::Stop(nth(1))],
        "the worker is told to stop"
    );
    let (updates, queue) = scripted.reconnected(1);
    assert!(updates.is_empty(), "the race changes nothing");
    assert!(queue.items().is_empty(), "nothing is sent on it");
    assert_eq!(
        scripted.client.status(),
        &Status::Stopped(exited(1)),
        "still stopped"
    );
}

/// Once more losses than the policy allows fall in its window, the client gives up.
#[test]
fn the_policy_gives_up_when_its_window_fills() {
    let policy = restart::Policy::UpTo {
        count: 1,
        within: std::time::Duration::from_secs(180),
    };
    let (mut scripted, _) = Scripted::running(policy);
    let _ = scripted.lost(1, exited(101));
    let (_, queue) = scripted.reconnected(1);
    let _ = handshake(&mut scripted, 1, &queue);
    let _ = scripted.inbox.lifecycles();
    let updates = scripted.lost(2, exited(101));
    assert_eq!(
        summaries(&updates).last().map(String::as_str),
        Some("Stopped(GaveUp)"),
        "gave up: {updates:?}"
    );
    assert_eq!(
        scripted.inbox.lifecycles(),
        [Lifecycle::Stop(nth(2))],
        "the worker is told to stop"
    );
}

/// Under `Never`, a loss stops the client with its own reason.
#[test]
fn a_never_policy_stops_with_the_loss_reason() {
    let (mut scripted, _) = Scripted::running(restart::Policy::Never);
    let updates = scripted.lost(1, exited(9));
    assert_eq!(
        summaries(&updates).last().map(String::as_str),
        Some("Stopped(Exited { code: Some(9), signal: None })"),
        "stopped: {updates:?}"
    );
    assert_eq!(
        scripted.inbox.lifecycles(),
        [Lifecycle::Stop(nth(1))],
        "the worker is told to stop"
    );
}

/// A failed respawn is reported and counted; once the window is full, the client gives up.
#[test]
fn a_failed_respawn_counts_against_the_policy() {
    let policy = restart::Policy::UpTo {
        count: 2,
        within: std::time::Duration::from_secs(180),
    };
    let (mut scripted, _) = Scripted::running(policy);
    let _ = scripted.lost(1, exited(101));
    let _ = scripted.inbox.lifecycles();
    let updates = scripted.attempting(1);
    assert_eq!(
        summaries(&updates),
        [
            "error(could not start the server again: entity not found)",
            "Restarting { attempt: 2 }"
        ],
        "the failure, then the next attempt"
    );
    assert!(
        scripted.inbox.lifecycles().is_empty(),
        "the worker retries by itself"
    );
    let updates = scripted.attempting(1);
    assert_eq!(
        summaries(&updates).last().map(String::as_str),
        Some("Stopped(GaveUp)"),
        "the window is full: {updates:?}"
    );
    assert_eq!(
        scripted.inbox.lifecycles(),
        [Lifecycle::Stop(nth(1))],
        "the worker is told to stop"
    );
    assert!(
        scripted.attempting(1).is_empty(),
        "a stopped client ignores attempts"
    );
}

/// After `shutdown()`, losses and reconnects change nothing, server requests still get their
/// `null` answer during the grace period, and the worker's stop ends it.
#[test]
fn shutdown_is_absorbing() {
    let (mut scripted, _) = Scripted::running(restart::Policy::default());
    let _ = scripted.client.shutdown();
    assert_eq!(
        scripted.inbox.lifecycles(),
        [Lifecycle::Shutdown {
            generation: nth(0),
            handshake: Handshake::Done,
        }],
        "the worker runs the handshake"
    );
    let _ = scripted.message(
        0,
        json!({"id": 7, "method": "workspace/configuration", "params": {"items": [{}]}}),
    );
    assert_eq!(
        scripted.queue.items(),
        [Some(json!({"jsonrpc": "2.0", "id": 7, "result": null}))],
        "the request is answered with null"
    );
    assert!(
        scripted.lost(1, exited(1)).is_empty(),
        "a loss changes nothing"
    );
    let (updates, queue) = scripted.reconnected(1);
    assert!(updates.is_empty(), "a reconnect changes nothing");
    assert!(queue.items().is_empty(), "nothing goes to it");
    assert!(scripted.inbox.lifecycles().is_empty(), "nothing is decided");
    assert_eq!(scripted.client.status(), &Status::Running, "until the stop");
    let updates = scripted.deliver(transport::Event::Stopped(Reason::Shutdown));
    assert_eq!(
        summaries(&updates),
        ["diagnostics(0)", "Stopped(Shutdown)"],
        "the stop"
    );

    let (mut scripted, _) = Scripted::running(restart::Policy::default());
    let _ = scripted.lost(1, exited(101));
    let _ = scripted.inbox.lifecycles();
    assert!(scripted.client.shutdown().is_empty(), "already settled");
    assert_eq!(
        scripted.inbox.lifecycles(),
        [Lifecycle::Shutdown {
            generation: nth(1),
            handshake: Handshake::Pending,
        }],
        "no handshake to finish"
    );
    let (updates, queue) = scripted.reconnected(1);
    assert!(updates.is_empty(), "the raced reconnect changes nothing");
    assert!(queue.items().is_empty(), "nothing goes to it");
    let updates = scripted.deliver(transport::Event::Stopped(Reason::Shutdown));
    assert_eq!(
        summaries(&updates),
        ["Stopped(Shutdown)"],
        "the stop, with nothing left to clear"
    );
}

/// A failed `initialize` reports the error, closes the live connection and stops, revivable.
#[test]
fn a_failed_initialize_closes_the_connection_and_stops() {
    let mut scripted = Scripted::new(restart::Policy::default());
    let doc = document("let value = 1;");
    let _ = scripted.client.open(&doc.snapshot(), &uri(), "rust");
    let updates = scripted.message(
        0,
        json!({"id": 1, "error": {"code": -32603, "message": "no"}}),
    );
    assert_eq!(
        summaries(&updates),
        [
            "error(server failed `initialize`: no (-32603))",
            "diagnostics(0)",
            "Stopped(Initialize)"
        ],
        "the error, the clear, the stop"
    );
    assert_eq!(
        scripted.inbox.lifecycles(),
        [Lifecycle::Stop(nth(0))],
        "the worker closes the connection"
    );
}

/// Edits made while reconnecting reach the new server as the reopened text, at a newer
/// version, and nothing goes anywhere meanwhile.
#[test]
fn edits_while_reconnecting_reopen_with_the_latest_text() {
    let (mut scripted, mut doc) = Scripted::running(restart::Policy::default());
    let _ = scripted.lost(1, exited(101));
    doc.edit(vec![EditOp::insert(14, " // edited")])
        .expect("edits");
    scripted.client.sync(&doc.snapshot(), doc.drain_changes());
    let answer = scripted
        .client
        .hover(&doc.snapshot(), &hover(&doc))
        .expect("the hover declines locally");
    assert!(
        matches!(answer.change(), update::Change::Hover(None)),
        "with no card"
    );
    assert!(
        scripted.queue.items().is_empty(),
        "nothing reached the old connection"
    );
    let (_, queue) = scripted.reconnected(1);
    let _ = handshake(&mut scripted, 1, &queue);
    let items = queue.items();
    let [Some(_), Some(open)] = items.as_slice() else {
        panic!("expected initialized and didOpen, got {items:?}")
    };
    assert_eq!(
        open["params"]["textDocument"]["text"], "let value = 1; // edited",
        "the latest text"
    );
    assert_eq!(
        open["params"]["textDocument"]["version"], 2,
        "the version counts on"
    );
}

/// A client that gave up comes back on `restart()`, which starts the count over.
#[test]
fn restart_revives_a_client_that_gave_up() {
    let (mut scripted, _) = Scripted::running(restart::Policy::Never);
    let _ = scripted.lost(1, exited(101));
    assert_eq!(
        scripted.inbox.lifecycles(),
        [Lifecycle::Stop(nth(1))],
        "stopped at 1, moving on to 2"
    );
    let settled = scripted.client.restart().expect("stdio restarts");
    assert!(settled.is_empty(), "the loss already settled everything");
    assert_eq!(
        scripted.inbox.lifecycles(),
        [Lifecycle::Restart(nth(3))],
        "a new connection at once"
    );
    assert_eq!(
        summaries(&scripted.queued()),
        ["Restarting { attempt: 1 }"],
        "the status follows through the stream"
    );
    let (_, queue) = scripted.reconnected(3);
    let updates = handshake(&mut scripted, 3, &queue);
    assert_eq!(summaries(&updates), ["Running"], "running again");
}

/// `restart()` on a running server settles what was pending, and what the old server still
/// answers is dropped.
#[test]
fn restart_drops_the_old_connections_replies() {
    let (mut scripted, doc) = Scripted::running(restart::Policy::default());
    let request = hover(&doc);
    let _ = scripted.client.hover(&doc.snapshot(), &request);
    let items = scripted.queue.items();
    let [Some(sent)] = items.as_slice() else {
        panic!("expected the hover, got {items:?}")
    };
    let settled = scripted.client.restart().expect("stdio restarts");
    assert!(
        matches!(settled.as_slice(), [first, ..] if matches!(first.change(), update::Change::Hover(None))),
        "the hover settles empty: {settled:?}"
    );
    assert_eq!(
        scripted.inbox.lifecycles(),
        [Lifecycle::Restart(nth(1))],
        "the worker restarts"
    );
    assert_eq!(
        summaries(&scripted.queued()),
        ["Restarting { attempt: 1 }"],
        "restarting"
    );
    let reply = json!({"id": sent["id"], "result": {"contents": "late"}});
    assert!(
        scripted.message(0, reply).is_empty(),
        "the old reply is dropped"
    );
}

/// After `shutdown()` there is nothing to restart.
#[test]
fn restart_after_shutdown_does_nothing() {
    let (mut scripted, _) = Scripted::running(restart::Policy::default());
    let _ = scripted.client.shutdown();
    let _ = scripted.inbox.lifecycles();
    assert!(
        scripted.client.restart().expect("not an error").is_empty(),
        "no documents"
    );
    assert!(scripted.queued().is_empty(), "no status");
    assert!(
        scripted.inbox.lifecycles().is_empty(),
        "nothing for the worker"
    );
}

/// A server that never answers `initialize` stops the client the first time; after a
/// handshake, the timeout is a loss like any other and restarts.
#[test]
fn an_initialize_timeout_stops_first_and_restarts_later() {
    let five = Duration::from_secs(5);
    let mut scripted = Scripted::with(restart::Policy::default(), Some(five));
    let updates = scripted.lost(1, Reason::Timeout);
    assert_eq!(
        summaries(&updates),
        [
            "error(the server did not answer `initialize` within 5s)",
            "Stopped(Timeout)"
        ],
        "the error, then the stop"
    );
    assert_eq!(
        scripted.inbox.lifecycles(),
        [Lifecycle::Stop(nth(1))],
        "the worker is told to stop"
    );

    let (mut scripted, _) = Scripted::running_with(restart::Policy::default(), Some(five));
    let _ = scripted.lost(1, exited(101));
    let (_, queue) = scripted.reconnected(1);
    let _ = queue.items();
    let _ = scripted.inbox.lifecycles();
    let updates = scripted.lost(2, Reason::Timeout);
    assert_eq!(
        summaries(&updates),
        [
            "error(the server did not answer `initialize` within 5s)",
            "diagnostics(0)",
            "Restarting { attempt: 2 }"
        ],
        "the error, the clear, the restart"
    );
    assert_eq!(
        scripted.inbox.lifecycles(),
        [Lifecycle::Reconnect(nth(2))],
        "the worker reconnects"
    );
}
