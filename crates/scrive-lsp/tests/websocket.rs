//! The WebSocket bridge against `tungstenite::accept` on loopback.
#![cfg(not(target_family = "wasm"))]

use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use futures::StreamExt;
use scrive_core::intel::ticket::Counter;
use scrive_core::{Diagnostic, Document, HoverInfo, HoverRequest};
use scrive_lsp::client::{self, builder, Client, Reason, Status};
use scrive_lsp::{log, restart, update, Update};
use serde_json::{json, Value};
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tungstenite::{Message, WebSocket};

const GRACE: Duration = Duration::from_millis(100);
/// How long any one wait may take: generous for Windows, which refuses a dial to a closed port
/// only after about 2 s, and for Defender.
const DEADLINE: Duration = Duration::from_secs(20);
/// The largest message the client reads.
const LIMIT: usize = 64 << 20;

/// A client with its events pumped onto a std channel so waits can time out.
struct Harness {
    client: Client,
    events: mpsc::Receiver<client::Event>,
}

/// What a test server saw of one connection.
#[derive(Debug, Default)]
struct Transcript {
    /// Each message's method, `reply` for a response, `close` for a close frame and `eof` at
    /// the end.
    log: Vec<String>,
    /// `initialize`'s `processId`.
    process_id: Option<Value>,
    /// The id of the `shutdown` request.
    shutdown: Option<Value>,
    /// Every data frame's text, and whether it came as a text frame.
    frames: Vec<(bool, String)>,
}

/// How the test server sends `publishDiagnostics`.
#[derive(Clone, Copy)]
enum Publish {
    Text,
    Binary,
}

/// A server's end of a connection.
struct Peer {
    socket: WebSocket<TcpStream>,
}

/// One frame a peer read.
enum Frame {
    Data { text: bool, body: String },
    Close,
}

impl Harness {
    fn new((client, events): (Client, client::Events)) -> Self {
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
    /// `DEADLINE`.
    fn until(&mut self, done: impl Fn(&Update) -> bool) -> Vec<Update> {
        let deadline = Instant::now() + DEADLINE;
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

    /// Every update until `Events` ends. Panics after `DEADLINE`.
    // "Read to the end", the counterpart of `until`, not a conversion.
    #[allow(clippy::wrong_self_convention)]
    fn to_end(&mut self) -> Vec<Update> {
        let deadline = Instant::now() + DEADLINE;
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

    /// Opens `text` as `websocket.rs`.
    fn open(&mut self, text: &str) -> Document {
        let mut doc = Document::new(text).expect("the fixture loads");
        doc.observe_changes(true);
        let uri = scrive_lsp::uri::from_path(&std::env::temp_dir().join("websocket.rs"))
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

    /// Shuts the client down and reads its events to their end, which is the shutdown stop.
    fn shut_down(&mut self) -> Vec<Update> {
        assert!(self.client.shutdown().is_empty(), "nothing was in flight");
        let end = self.to_end();
        assert_eq!(
            end.last().and_then(stopped),
            Some(&Reason::Shutdown),
            "the stop is last: {end:#?}"
        );
        end
    }
}

impl Peer {
    /// The next connection `listener` accepts, after its WebSocket handshake.
    fn accept(listener: &TcpListener) -> Self {
        let (stream, _) = listener.accept().expect("the client dials in");
        stream
            .set_read_timeout(Some(DEADLINE))
            .expect("the stream takes a timeout");
        let config = WebSocketConfig::default()
            .max_message_size(Some(LIMIT))
            .max_frame_size(Some(LIMIT));
        let socket =
            tungstenite::accept_with_config(stream, Some(config)).expect("the client upgrades");
        Self { socket }
    }

    /// The next data or close frame, or `None` once the connection is over.
    fn read(&mut self) -> Option<Frame> {
        loop {
            match self.socket.read() {
                Ok(Message::Text(text)) => {
                    return Some(Frame::Data {
                        text: true,
                        body: text.as_str().to_owned(),
                    })
                }
                Ok(Message::Binary(bytes)) => {
                    return Some(Frame::Data {
                        text: false,
                        body: String::from_utf8_lossy(&bytes).into_owned(),
                    })
                }
                Ok(Message::Close(_)) => return Some(Frame::Close),
                Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => {}
                Err(_) => return None,
            }
        }
    }

    /// The next JSON-RPC message, skipping nothing: panics on a close.
    fn message(&mut self) -> Value {
        match self.read() {
            Some(Frame::Data { body, .. }) => {
                serde_json::from_str(&body).expect("the client sends JSON")
            }
            Some(Frame::Close) | None => panic!("the client sends a message"),
        }
    }

    fn send(&mut self, message: &Value) {
        self.socket
            .send(Message::text(message.to_string()))
            .expect("the client reads");
    }

    /// Reads `initialize` and answers it with hover and full sync.
    fn initialize(&mut self) {
        let request = self.message();
        assert_eq!(request["method"], "initialize", "the first message");
        self.send(&reply(&request["id"], initialized()));
    }

    /// Reads until `method` arrives.
    fn skip_to(&mut self, method: &str) {
        while self.message()["method"] != method {}
    }

    /// Drops the TCP connection without a close frame.
    fn drop_connection(self) {
        let _ = self.socket.get_ref().shutdown(Shutdown::Both);
    }

    /// Serves a whole conversation: answers `initialize`, hovers with the client's `processId`,
    /// and every other request with `null`; publishes one `fake` diagnostic per opened document
    /// the way `publish` says. Returns once the connection is over.
    fn serve(mut self, publish: Publish) -> Transcript {
        let mut transcript = Transcript::default();
        loop {
            let (text, body) = match self.read() {
                Some(Frame::Data { text, body }) => (text, body),
                Some(Frame::Close) => {
                    transcript.log.push("close".to_owned());
                    continue;
                }
                None => break,
            };
            transcript.frames.push((text, body.clone()));
            let message: Value = serde_json::from_str(&body).expect("the client sends JSON");
            let Some(method) = message["method"].as_str() else {
                transcript.log.push("reply".to_owned());
                continue;
            };
            transcript.log.push(method.to_owned());
            if message.get("id").is_some() {
                let result = match method {
                    "initialize" => {
                        transcript.process_id = Some(message["params"]["processId"].clone());
                        initialized()
                    }
                    "textDocument/hover" => {
                        hover(transcript.process_id.as_ref().unwrap_or(&Value::Null))
                    }
                    "shutdown" => {
                        transcript.shutdown = Some(message["id"].clone());
                        Value::Null
                    }
                    _ => Value::Null,
                };
                self.send(&reply(&message["id"], result));
            } else if method == "textDocument/didOpen" {
                let document = &message["params"]["textDocument"];
                let diagnostics = publish_fake(&document["uri"], &document["version"]);
                match publish {
                    Publish::Text => self.send(&diagnostics),
                    Publish::Binary => self
                        .socket
                        .send(Message::binary(diagnostics.to_string().into_bytes()))
                        .expect("the client reads"),
                }
            }
        }
        transcript.log.push("eof".to_owned());
        transcript
    }
}

fn reply(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// The `initialize` result: hover and full sync.
fn initialized() -> Value {
    json!({ "capabilities": { "hoverProvider": true, "textDocumentSync": 1 } })
}

/// A hover answer naming `process_id`.
fn hover(process_id: &Value) -> Value {
    json!({ "contents": { "kind": "markdown", "value": format!("pid {process_id}") } })
}

/// One `fake` diagnostic over `value` for `uri`, at `version`.
fn publish_fake(uri: &Value, version: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": {
            "uri": uri,
            "version": version,
            "diagnostics": [{
                "range": {
                    "start": { "line": 0, "character": 4 },
                    "end": { "line": 0, "character": 9 },
                },
                "severity": 1,
                "message": "fake",
            }],
        },
    })
}

fn builder() -> client::Builder {
    Client::builder()
        .shutdown_grace(GRACE)
        .backoff(Duration::from_millis(50))
}

/// A listener on a free loopback port, and the `ws://` URL that reaches it.
fn listener() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port is free");
    let address = listener.local_addr().expect("the listener is bound");
    (listener, format!("ws://{address}/"))
}

/// A loopback address nothing listens on: bound once for a free port, then released.
fn vacant() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("a loopback port is free")
}

/// Serves one accepted connection on a thread of its own.
fn serve_one(listener: TcpListener, publish: Publish) -> thread::JoinHandle<Transcript> {
    thread::spawn(move || Peer::accept(&listener).serve(publish))
}

fn running(update: &Update) -> bool {
    matches!(update, Update::Status(Status::Running))
}

fn restarting(update: &Update) -> bool {
    matches!(update, Update::Status(Status::Restarting { .. }))
}

/// The reason of a stop update.
fn stopped(update: &Update) -> Option<&Reason> {
    let Update::Status(Status::Stopped(reason)) = update else {
        return None;
    };
    Some(reason)
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

fn published_fake(update: &Update) -> bool {
    let Update::Document(document) = update else {
        return false;
    };
    let update::Change::Diagnostics(list) = document.change() else {
        return false;
    };
    list.iter()
        .any(|diagnostic: &Diagnostic| diagnostic.message == "fake")
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

/// The log entries among `updates`, from the socket.
fn socket_logs(updates: &[Update]) -> Vec<&log::Entry> {
    updates
        .iter()
        .filter_map(|update| match update {
            Update::Log(entry) if entry.source() == log::Source::Socket => Some(entry),
            _ => None,
        })
        .collect()
}

/// A connected client runs the whole conversation: handshake with a `null` `processId`,
/// document sync, diagnostics, a hover, and the shutdown handshake with the reserved id, `exit`
/// and the close frame.
#[test]
fn websocket_runs_the_common_conversation() {
    let (listener, url) = listener();
    let mut harness = Harness::new(builder().websocket(&url).expect("the worker starts"));
    let server = serve_one(listener, Publish::Text);
    let updates = harness.until(running);
    assert_eq!(
        statuses(&updates),
        [Status::Running],
        "starting, then running: {updates:#?}"
    );
    let doc = harness.open("let value = 1;");
    harness.until(published_fake);
    let request = harness.hover(&doc);
    harness.until(|update| {
        hovered(update, &request)
            .flatten()
            .is_some_and(|card| card.markdown.contains("pid null"))
    });
    harness.shut_down();
    let transcript = server.join().expect("the server ran");
    assert_eq!(
        transcript.process_id,
        Some(Value::Null),
        "no process id over a socket"
    );
    assert_eq!(
        transcript.shutdown,
        Some(json!("scrive-lsp/shutdown")),
        "the reserved id"
    );
    assert!(
        transcript.log.ends_with(&[
            "shutdown".to_owned(),
            "exit".to_owned(),
            "close".to_owned(),
            "eof".to_owned()
        ]),
        "shutdown, exit, then the close frame: {:?}",
        transcript.log
    );
}

/// Every message goes out alone in a text frame, as bare JSON.
#[test]
fn every_message_is_one_text_frame() {
    let (listener, url) = listener();
    let mut harness = Harness::new(builder().websocket(&url).expect("the worker starts"));
    let server = serve_one(listener, Publish::Text);
    let _doc = harness.open("let value = 1;");
    harness.until(published_fake);
    harness.shut_down();
    let frames = server.join().expect("the server ran").frames;
    assert!(frames.len() >= 5, "a whole conversation: {frames:?}");
    for (text, body) in &frames {
        assert!(*text, "a text frame: {body}");
        assert!(!body.contains("Content-Length"), "no header: {body}");
        let message: Value = serde_json::from_str(body).expect("one JSON value per frame");
        assert!(message.is_object(), "one message per frame: {body}");
    }
}

/// A connection the server drops after the handshake is dialed again: a fresh `initialize`,
/// and the document reopens.
#[test]
fn a_dropped_connection_reconnects_and_reinitializes() {
    let (listener, url) = listener();
    let mut harness = Harness::new(builder().websocket(&url).expect("the worker starts"));
    let _doc = harness.open("let value = 1;");
    let server = thread::spawn(move || {
        let mut first = Peer::accept(&listener);
        first.initialize();
        first.skip_to("initialized");
        first.drop_connection();
        Peer::accept(&listener).serve(Publish::Text)
    });
    let first = harness.until(running);
    assert_eq!(statuses(&first), [Status::Running], "the first handshake");
    let again = harness.until(running);
    assert_eq!(
        statuses(&again),
        [Status::Restarting { attempt: 1 }, Status::Running],
        "restarting, never running in between: {again:#?}"
    );
    harness.until(published_fake);
    harness.shut_down();
    let log = server.join().expect("the second server ran").log;
    assert_eq!(
        log.get(..3),
        Some(
            &[
                "initialize".to_owned(),
                "initialized".to_owned(),
                "textDocument/didOpen".to_owned()
            ][..]
        ),
        "a fresh handshake, then the document reopens: {log:?}"
    );
}

/// JSON in a binary frame is read as text.
#[test]
fn a_binary_json_frame_is_read_like_text() {
    let (listener, url) = listener();
    let mut harness = Harness::new(builder().websocket(&url).expect("the worker starts"));
    let server = serve_one(listener, Publish::Binary);
    let _doc = harness.open("let value = 1;");
    harness.until(published_fake);
    harness.shut_down();
    server.join().expect("the server ran");
}

/// A binary frame that isn't UTF-8 is logged and dropped, and the connection goes on.
#[test]
fn a_binary_frame_that_is_not_utf8_is_logged_and_dropped() {
    let (listener, url) = listener();
    let mut harness = Harness::new(builder().websocket(&url).expect("the worker starts"));
    let server = thread::spawn(move || {
        let mut peer = Peer::accept(&listener);
        peer.socket
            .send(Message::binary(vec![0xff, 0xfe]))
            .expect("the client reads");
        peer.serve(Publish::Text)
    });
    let doc = harness.open("let value = 1;");
    let mut updates = harness.until(running);
    let request = harness.hover(&doc);
    updates.extend(harness.until(|update| hovered(update, &request).flatten().is_some()));
    let logs = socket_logs(&updates);
    assert_eq!(logs.len(), 1, "one log entry: {updates:#?}");
    assert_eq!(
        logs[0].level(),
        scrive_lsp::lsp_types::MessageType::WARNING,
        "a warning"
    );
    assert!(
        logs[0].text().contains("binary") && logs[0].text().contains("2 bytes"),
        "it names the frame: {}",
        logs[0].text()
    );
    harness.shut_down();
    server.join().expect("the server ran");
}

/// The server's close code and reason are logged before the client restarts.
#[test]
fn a_close_frame_is_logged_before_the_status_change() {
    let (listener, url) = listener();
    let mut harness = Harness::new(builder().websocket(&url).expect("the worker starts"));
    let server = thread::spawn(move || {
        let mut first = Peer::accept(&listener);
        first.initialize();
        first.skip_to("initialized");
        first
            .socket
            .close(Some(CloseFrame {
                code: CloseCode::Away,
                reason: "going away".into(),
            }))
            .expect("the close frame goes out");
        while first.read().is_some() {}
        drop(first);
        Peer::accept(&listener).serve(Publish::Text)
    });
    harness.until(running);
    let updates = harness.until(restarting);
    let logs = socket_logs(&updates);
    assert_eq!(logs.len(), 1, "one log entry: {updates:#?}");
    assert_eq!(
        logs[0].level(),
        scrive_lsp::lsp_types::MessageType::WARNING,
        "1001 is a warning"
    );
    assert!(
        logs[0].text().contains("1001") && logs[0].text().contains("going away"),
        "it names the code and reason: {}",
        logs[0].text()
    );
    let log = updates
        .iter()
        .position(|update| matches!(update, Update::Log(_)))
        .expect("the log is there");
    let restart = updates
        .iter()
        .position(restarting)
        .expect("the restart is there");
    assert!(log < restart, "the log comes first: {updates:#?}");
    harness.until(running);
    harness.shut_down();
    server.join().expect("the second server ran");
}

/// A ping from the server is answered while the client is idle.
#[test]
fn a_server_ping_is_answered() {
    let (listener, url) = listener();
    let mut harness = Harness::new(builder().websocket(&url).expect("the worker starts"));
    let (ponged, pong) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut peer = Peer::accept(&listener);
        peer.initialize();
        peer.skip_to("initialized");
        peer.socket
            .send(Message::Ping("p".into()))
            .expect("the client reads");
        loop {
            match peer.socket.read() {
                Ok(Message::Pong(payload)) => {
                    ponged.send(payload.to_vec()).expect("the test waits");
                    break;
                }
                Ok(_) => {}
                Err(error) => panic!("no pong: {error}"),
            }
        }
        peer.serve(Publish::Text)
    });
    harness.until(running);
    let payload = pong.recv_timeout(DEADLINE).expect("the pong came");
    assert_eq!(payload, b"p", "the ping's payload");
    harness.shut_down();
    server.join().expect("the server ran");
}

/// A message over the limit is an error, and the connection that carried it is lost and
/// dialed again.
#[test]
fn an_oversize_frame_is_an_error_and_the_connection_reconnects() {
    let declared: u64 = 65 << 20;
    let (listener, url) = listener();
    let mut harness = Harness::new(builder().websocket(&url).expect("the worker starts"));
    let server = thread::spawn(move || {
        let mut first = Peer::accept(&listener);
        first.initialize();
        first.skip_to("initialized");
        let mut header = vec![0x81, 127];
        header.extend(declared.to_be_bytes());
        std::io::Write::write_all(first.socket.get_mut(), &header).expect("the client reads");
        while first.read().is_some() {}
        Peer::accept(&listener).serve(Publish::Text)
    });
    harness.until(running);
    let updates = harness.until(running);
    let error = updates
        .iter()
        .position(|update| {
            matches!(update, Update::Error(scrive_lsp::Error::Oversized { length }) if *length == declared)
        })
        .unwrap_or_else(|| panic!("the oversize error: {updates:#?}"));
    let restart = updates
        .iter()
        .position(restarting)
        .expect("the restart is there");
    assert!(error < restart, "the error comes first: {updates:#?}");
    assert_eq!(
        statuses(&updates),
        [Status::Restarting { attempt: 1 }, Status::Running],
        "dialed again: {updates:#?}"
    );
    harness.shut_down();
    server.join().expect("the second server ran");
}

/// A 16 MiB message reaches a server that reads nothing until `receive` has returned, so no
/// `receive` can have waited on the socket.
#[test]
fn a_large_message_goes_out_while_the_server_reads_slowly() {
    let size = 16 << 20;
    let (listener, url) = listener();
    let mut harness = Harness::new(builder().websocket(&url).expect("the worker starts"));
    let (released, release) = mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let mut peer = Peer::accept(&listener);
        peer.initialize();
        // A client that wrote the 16 MiB `didOpen` from `receive` would block on the full
        // socket here, never release the server, and fail on the deadline instead.
        release.recv().expect("the test releases the server");
        loop {
            let message = peer.message();
            if message["method"] == "textDocument/didOpen" {
                let length = message["params"]["textDocument"]["text"]
                    .as_str()
                    .map_or(0, str::len);
                peer.send(&json!({
                    "jsonrpc": "2.0",
                    "method": "scrive-test/length",
                    "params": length,
                }));
                break;
            }
        }
        peer.serve(Publish::Text)
    });
    let text = format!("let value = 1;{}", "x".repeat(size - 14));
    let _doc = harness.open(&text);
    let deadline = Instant::now() + DEADLINE;
    let length = loop {
        let timeout = deadline.saturating_duration_since(Instant::now());
        let event = harness
            .events
            .recv_timeout(timeout)
            .expect("the server answers");
        let updates = harness.client.receive(event);
        // The server may already be reading; later releases have no receiver to wake.
        let _ = released.send(());
        let found = updates.iter().find_map(|update| match update {
            Update::Notification(notification) if notification.method() == "scrive-test/length" => {
                notification.params().and_then(Value::as_u64)
            }
            _ => None,
        });
        if let Some(length) = found {
            break length;
        }
    };
    assert_eq!(length, size as u64, "the server got the whole text");
    harness.shut_down();
    server.join().expect("the server ran");
}

/// A server that stops reading is lost as unresponsive once the unwritten messages pass the
/// limit.
#[test]
fn a_server_that_stops_reading_trips_the_hung_server_guard() {
    enum Blob {}
    impl scrive_lsp::lsp_types::notification::Notification for Blob {
        type Params = String;
        const METHOD: &'static str = "scrive-test/blob";
    }
    let (listener, url) = listener();
    let mut harness = Harness::new(
        builder()
            .restart(restart::Policy::Never)
            .backlog_limit(64 * 1024)
            .websocket(&url)
            .expect("the worker starts"),
    );
    let (release, released) = mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let mut peer = Peer::accept(&listener);
        peer.initialize();
        let _ = released.recv();
        drop(peer);
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
    drop(release);
    server.join().expect("the server ran");
}

/// A server that never listens stops the client once the connect timeout runs out; the client
/// stays revivable, and a restart dials again.
#[test]
fn a_refused_initial_dial_stops_after_connect_timeout() {
    let address = vacant();
    let mut harness = Harness::new(
        builder()
            .connect_timeout(Duration::from_millis(300))
            .websocket(&format!("ws://{address}/"))
            .expect("the worker starts"),
    );
    let end = harness.until(|update| stopped(update).is_some());
    assert!(
        matches!(end.last().and_then(stopped), Some(Reason::Failed(_))),
        "the dial failed: {end:#?}"
    );
    assert!(
        matches!(harness.events.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the stream runs on"
    );
    let listener = TcpListener::bind(address).expect("the port is still free");
    let server = serve_one(listener, Publish::Text);
    let settled = harness.client.restart().expect("a dialing client restarts");
    assert!(settled.is_empty(), "nothing was in flight");
    let updates = harness.until(running);
    assert_eq!(
        statuses(&updates),
        [Status::Restarting { attempt: 1 }, Status::Running],
        "restarting, then running: {updates:#?}"
    );
    harness.shut_down();
    let log = server.join().expect("the server ran").log;
    assert_eq!(
        log.first().map(String::as_str),
        Some("initialize"),
        "{log:?}"
    );
}

/// `wss://` against a server that speaks no TLS fails each dial and stops after the connect
/// timeout.
#[test]
fn a_wss_dial_to_a_plain_server_fails_the_tls_handshake() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port is free");
    let address = listener.local_addr().expect("the listener is bound");
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let _ = stream.set_read_timeout(Some(DEADLINE));
            let _ = tungstenite::accept(stream);
        }
    });
    let mut harness = Harness::new(
        builder()
            .connect_timeout(Duration::from_millis(300))
            .websocket(&format!("wss://{address}/"))
            .expect("the worker starts"),
    );
    let end = harness.until(|update| stopped(update).is_some());
    assert!(
        matches!(end.last().and_then(stopped), Some(Reason::Failed(_))),
        "the handshake failed: {end:#?}"
    );
}

/// A caller's TLS configuration is taken as it is.
#[test]
fn a_custom_tls_config_is_accepted() {
    let config = scrive_lsp::rustls::ClientConfig::builder_with_provider(Arc::new(
        scrive_lsp::rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("ring supports the default versions")
    .with_root_certificates(scrive_lsp::rustls::RootCertStore::empty())
    .with_no_client_auth();
    let started = Client::builder()
        .tls(Arc::new(config))
        .websocket("wss://localhost:1");
    assert!(started.is_ok(), "the client starts: {:?}", started.err());
}

/// A URL with a space in its host doesn't parse.
#[test]
fn an_unparsable_url_is_rejected() {
    let error = Client::builder().websocket("ws://exa mple").err();
    assert!(
        matches!(
            error,
            Some(builder::Error::Url {
                reason: builder::error::Url::Unparsable,
                ..
            })
        ),
        "{error:?}"
    );
}

/// Only `ws` and `wss` are WebSocket schemes.
#[test]
fn a_non_websocket_scheme_is_rejected() {
    let error = Client::builder().websocket("http://localhost").err();
    assert!(
        matches!(
            error,
            Some(builder::Error::Url {
                reason: builder::error::Url::Scheme,
                ..
            })
        ),
        "{error:?}"
    );
}
