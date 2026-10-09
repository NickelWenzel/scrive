//! The TCP bridge against lsp-server and raw sockets on loopback.
#![cfg(not(target_family = "wasm"))]

use std::io::{BufReader, BufWriter, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use futures::StreamExt;
use scrive_core::intel::ticket::Counter;
use scrive_core::{Diagnostic, Document, HoverInfo, HoverRequest};
use scrive_lsp::client::{self, Client, Reason, Status};
use scrive_lsp::lsp_server::{Connection, Message, Notification, RequestId, Response};
use scrive_lsp::{update, Update};
use serde_json::{json, Value};

const GRACE: Duration = Duration::from_millis(100);
/// How long any one wait may take: generous for Windows, which refuses a dial to a closed port
/// only after about 2 s, and for Defender.
const DEADLINE: Duration = Duration::from_secs(20);

/// A client with its events pumped onto a std channel so waits can time out.
struct Harness {
    client: Client,
    events: mpsc::Receiver<client::Event>,
}

/// What a test server saw of one connection.
#[derive(Debug, Default)]
struct Transcript {
    /// Each message's method, `reply` for a response, and `eof` when the client closed.
    log: Vec<String>,
    /// `initialize`'s `processId`.
    process_id: Option<Value>,
    /// The id of the `shutdown` request.
    shutdown: Option<RequestId>,
}

/// A server's end of a connection, framed by lsp-server.
struct Peer {
    reader: BufReader<TcpStream>,
    writer: BufWriter<TcpStream>,
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

    /// Opens `let value = 1;` as `tcp.rs`.
    fn open(&mut self) -> Document {
        let mut doc = Document::new("let value = 1;").expect("the fixture loads");
        doc.observe_changes(true);
        let uri = scrive_lsp::uri::from_path(&std::env::temp_dir().join("tcp.rs"))
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
            end.iter().filter_map(stopped).collect::<Vec<_>>(),
            [&Reason::Shutdown],
            "one stop: {end:#?}"
        );
        assert_eq!(
            end.last().and_then(stopped),
            Some(&Reason::Shutdown),
            "the stop is last: {end:#?}"
        );
        end
    }
}

impl Peer {
    /// The next connection `listener` accepts.
    fn accept(listener: &TcpListener) -> Self {
        let (stream, _) = listener.accept().expect("the client dials in");
        Self::new(stream)
    }

    fn new(stream: TcpStream) -> Self {
        stream
            .set_read_timeout(Some(DEADLINE))
            .expect("the stream takes a timeout");
        Self {
            reader: BufReader::new(stream.try_clone().expect("the stream clones")),
            writer: BufWriter::new(stream),
        }
    }

    /// The next message, or `None` at EOF.
    fn read(&mut self) -> Option<Message> {
        Message::read(&mut self.reader).expect("the client frames its messages")
    }

    /// Writes `message` in one flush, so the test server doesn't pay Nagle's delay either.
    fn write(&mut self, message: impl Into<Message>) {
        message
            .into()
            .write(&mut self.writer)
            .and_then(|()| self.writer.flush())
            .expect("the client reads");
    }

    /// Reads `initialize` and answers it with hover and full sync, and returns its params.
    fn initialize(&mut self) -> Value {
        let Some(Message::Request(request)) = self.read() else {
            panic!("the client opens with a request");
        };
        assert_eq!(request.method, "initialize", "the first message");
        self.write(Response::new_ok(request.id, initialized()));
        request.params
    }

    /// Serves a whole conversation: answers `initialize`, hovers with the client's
    /// `processId`, and every other request with `null`; publishes one `fake` diagnostic per
    /// opened document. Returns at EOF.
    fn serve(mut self) -> Transcript {
        let mut transcript = Transcript::default();
        while let Some(message) = self.read() {
            match message {
                Message::Request(request) => {
                    transcript.log.push(request.method.clone());
                    let result = match request.method.as_str() {
                        "initialize" => {
                            transcript.process_id = Some(request.params["processId"].clone());
                            initialized()
                        }
                        "textDocument/hover" => {
                            hover(transcript.process_id.as_ref().unwrap_or(&Value::Null))
                        }
                        "shutdown" => {
                            transcript.shutdown = Some(request.id.clone());
                            Value::Null
                        }
                        _ => Value::Null,
                    };
                    self.write(Response::new_ok(request.id, result));
                }
                Message::Notification(notification) => {
                    transcript.log.push(notification.method.clone());
                    if notification.method == "textDocument/didOpen" {
                        let document = &notification.params["textDocument"];
                        self.write(publish(&document["uri"], &document["version"]));
                    }
                }
                Message::Response(_) => transcript.log.push("reply".to_owned()),
            }
        }
        transcript.log.push("eof".to_owned());
        transcript
    }
}

/// Hover and full sync.
fn capabilities() -> Value {
    json!({ "hoverProvider": true, "textDocumentSync": 1 })
}

/// The `initialize` result with [`capabilities`].
fn initialized() -> Value {
    json!({ "capabilities": capabilities() })
}

/// A hover answer naming `process_id`.
fn hover(process_id: &Value) -> Value {
    json!({ "contents": { "kind": "markdown", "value": format!("pid {process_id}") } })
}

/// One `fake` diagnostic over `value` for `uri`, at `version`.
fn publish(uri: &Value, version: &Value) -> Notification {
    let params = json!({
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
    });
    Notification::new("textDocument/publishDiagnostics".to_owned(), params)
}

fn builder() -> client::Builder {
    Client::builder().shutdown_grace(GRACE)
}

/// A loopback address nothing listens on: bound once for a free port, then released.
fn vacant() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("a loopback port is free")
}

/// Port 0 on loopback, for the OS to pick a port.
fn loopback() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 0))
}

/// A listener on a free loopback port, and its address.
fn listener() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port is free");
    let address = listener.local_addr().expect("the listener is bound");
    (listener, address)
}

/// Serves one accepted connection on a thread of its own.
fn serve_one(listener: TcpListener) -> thread::JoinHandle<Transcript> {
    thread::spawn(move || Peer::accept(&listener).serve())
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

/// A dialed server runs the whole conversation: handshake with a `null` `processId`, document
/// sync, diagnostics, a hover, and the shutdown handshake with the reserved id, `exit` and the
/// close.
#[test]
fn connect_carries_a_whole_conversation_to_a_test_listener() {
    let (listener, address) = listener();
    let mut harness = Harness::new(builder().connect(address).expect("the worker starts"));
    let server = serve_one(listener);
    harness.until(running);
    let doc = harness.open();
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
        Some(RequestId::from("scrive-lsp/shutdown".to_owned())),
        "the reserved id"
    );
    assert!(
        transcript
            .log
            .ends_with(&["shutdown".to_owned(), "exit".to_owned(), "eof".to_owned()]),
        "shutdown, exit, then the close: {:?}",
        transcript.log
    );
}

/// What the client sends before the connection is up waits for it, in order.
#[test]
fn connect_queues_messages_until_the_dial_succeeds() {
    let (listener, address) = listener();
    let mut harness = Harness::new(builder().connect(address).expect("the worker starts"));
    let _doc = harness.open();
    let server = serve_one(listener);
    harness.until(published_fake);
    harness.shut_down();
    let log = server.join().expect("the server ran").log;
    assert_eq!(
        log.get(..3),
        Some(
            &[
                "initialize".to_owned(),
                "initialized".to_owned(),
                "textDocument/didOpen".to_owned()
            ][..]
        ),
        "initialize first, the open after initialized: {log:?}"
    );
}

/// A server that never listens stops the client once the connect timeout runs out; the client
/// stays revivable, and a restart dials again.
#[test]
fn connect_gives_up_after_its_timeout() {
    let address = vacant();
    let timeout = Duration::from_millis(500);
    let dialed = Instant::now();
    let mut harness = Harness::new(
        builder()
            .connect_timeout(timeout)
            .connect(address)
            .expect("the worker starts"),
    );
    let end = harness.until(|update| stopped(update).is_some());
    let elapsed = dialed.elapsed();
    let Some(Reason::Failed(error)) = end.last().and_then(stopped) else {
        panic!("the dial failed: {end:#?}");
    };
    assert!(
        matches!(
            error.kind(),
            std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::TimedOut
        ),
        "refused, or timed out where refusals are slow: {error}"
    );
    assert!(
        elapsed >= timeout,
        "it tried for the whole timeout: {elapsed:?}"
    );
    assert!(
        matches!(harness.events.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the stream runs on"
    );

    let listener = TcpListener::bind(address).expect("the port is still free");
    let server = serve_one(listener);
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

/// A server that binds its port after the client dialed is reached by the retries.
#[test]
fn connect_retries_until_a_late_server_binds() {
    let address = vacant();
    let mut harness = Harness::new(builder().connect(address).expect("the worker starts"));
    let server = thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        let listener = TcpListener::bind(address).expect("the port is still free");
        Peer::accept(&listener).serve()
    });
    let updates = harness.until(running);
    assert_eq!(
        statuses(&updates),
        [Status::Running],
        "starting, then running, nothing between: {updates:#?}"
    );
    harness.shut_down();
    let log = server.join().expect("the server ran").log;
    assert_eq!(
        log.first().map(String::as_str),
        Some("initialize"),
        "{log:?}"
    );
}

/// A connection the server closes after the handshake is dialed again, and the documents
/// reopen on the new one.
#[test]
fn connect_redials_after_a_loss() {
    let (listener, address) = listener();
    let mut harness = Harness::new(
        builder()
            .backoff(Duration::from_millis(50))
            .connect(address)
            .expect("the worker starts"),
    );
    let _doc = harness.open();
    let server = thread::spawn(move || {
        let mut first = Peer::accept(&listener);
        let _ = first.initialize();
        first
            .writer
            .get_ref()
            .shutdown(Shutdown::Both)
            .expect("the first connection shuts down");
        drop(first);
        Peer::accept(&listener).serve()
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

/// A peer that neither reads nor closes doesn't hold the shutdown up past its two grace
/// periods.
#[test]
fn shutdown_completes_against_a_peer_that_never_answers() {
    let (listener, address) = listener();
    let mut harness = Harness::new(builder().connect(address).expect("the worker starts"));
    let (accepted, held) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("the client dials in");
        accepted.send(()).expect("the test waits");
        let _ = released.recv();
        drop(stream);
    });
    held.recv_timeout(DEADLINE).expect("the client dialed");
    let asked = Instant::now();
    harness.shut_down();
    let elapsed = asked.elapsed();
    assert!(
        elapsed < 2 * GRACE + Duration::from_secs(1),
        "the worker didn't wait on the silent peer: {elapsed:?}"
    );
    drop(release);
    server.join().expect("the server ran");
}

/// Shutting a socket down both ways wakes a thread blocked reading it, though the peer stays
/// open and silent. The bridge releases a dead connection's reader this way.
#[test]
fn shutdown_both_wakes_a_reader_blocked_on_a_silent_peer() {
    let (listener, address) = listener();
    let stream = TcpStream::connect(address).expect("the listener accepts");
    let (_peer, _) = listener.accept().expect("the dial arrives");
    let mut reading = stream.try_clone().expect("the stream clones");
    let (done, woken) = mpsc::channel();
    thread::spawn(move || {
        let mut byte = [0];
        let _ = done.send(reading.read(&mut byte).map_err(|error| error.kind()));
    });
    thread::sleep(Duration::from_millis(200));
    stream
        .shutdown(Shutdown::Both)
        .expect("the stream shuts down");
    let result = woken.recv_timeout(Duration::from_secs(2));
    assert!(
        matches!(result, Ok(Ok(0) | Err(_))),
        "the reader returned: {result:?}"
    );
}

/// An lsp-server that dials in runs the whole conversation, and its loop ends after `exit`.
#[test]
fn listen_carries_a_whole_conversation_from_a_connecting_lsp_server() {
    let mut harness = Harness::new(builder().listen(loopback()).expect("the port binds"));
    let address = harness.client.listening_on().expect("a listen client");
    let server = thread::spawn(move || {
        let (connection, _threads) = Connection::connect(address).expect("the client listens");
        let params = connection
            .initialize(capabilities())
            .expect("the client initializes");
        let mut log = Vec::new();
        for message in &connection.receiver {
            match message {
                Message::Request(request) => {
                    log.push(request.method.clone());
                    // A request between `shutdown` and `exit` fails this, which ends the
                    // conversation as well.
                    if connection.handle_shutdown(&request).unwrap_or(true) {
                        log.push("exit".to_owned());
                        break;
                    }
                    let result = match request.method.as_str() {
                        "textDocument/hover" => hover(&params["processId"]),
                        _ => Value::Null,
                    };
                    connection
                        .sender
                        .send(Response::new_ok(request.id, result).into())
                        .expect("lsp-server's writer runs");
                }
                Message::Notification(notification) => {
                    log.push(notification.method.clone());
                    if notification.method == "textDocument/didOpen" {
                        let document = &notification.params["textDocument"];
                        connection
                            .sender
                            .send(publish(&document["uri"], &document["version"]).into())
                            .expect("lsp-server's writer runs");
                    }
                }
                Message::Response(_) => log.push("reply".to_owned()),
            }
        }
        (params, log)
    });
    harness.until(running);
    let doc = harness.open();
    harness.until(published_fake);
    let request = harness.hover(&doc);
    harness.until(|update| {
        hovered(update, &request)
            .flatten()
            .is_some_and(|card| card.markdown.contains("pid null"))
    });
    harness.shut_down();
    let (params, log) = server.join().expect("the server ran");
    assert_eq!(
        params["processId"],
        Value::Null,
        "no process id over a socket"
    );
    assert!(
        log.ends_with(&["shutdown".to_owned(), "exit".to_owned()]),
        "the loop ended after shutdown and exit: {log:?}"
    );
}

/// A server that dialed in and disconnects stops the client for good: the stream ends, and
/// there is nothing to restart.
#[test]
fn listen_stops_closed_when_its_server_disconnects() {
    let mut harness = Harness::new(builder().listen(loopback()).expect("the port binds"));
    let address = harness.client.listening_on().expect("a listen client");
    let mut peer = Peer::new(TcpStream::connect(address).expect("the client listens"));
    let _ = peer.initialize();
    harness.until(running);
    peer.writer
        .get_ref()
        .shutdown(Shutdown::Both)
        .expect("the connection shuts down");
    drop(peer);
    let end = harness.to_end();
    assert_eq!(
        end.last().and_then(stopped),
        Some(&Reason::Closed),
        "the stop is last and says closed: {end:#?}"
    );
    assert!(
        matches!(
            harness.client.restart(),
            Err(scrive_lsp::Error::Unrestartable)
        ),
        "a listen client can't restart"
    );
    assert_eq!(
        harness.client.listening_on(),
        Some(address),
        "the bound address stays"
    );
}

/// A server that dials in and never answers `initialize` stops the listen client at the deadline,
/// and the timeout error comes right before the stop, as on the other bridges.
#[test]
fn listen_reports_an_initialize_timeout_before_it_stops() {
    let timeout = Duration::from_millis(300);
    let mut harness = Harness::new(
        builder()
            .initialize_timeout(timeout)
            .listen(loopback())
            .expect("the port binds"),
    );
    let address = harness.client.listening_on().expect("a listen client");
    let mut peer = Peer::new(TcpStream::connect(address).expect("the client listens"));
    assert!(
        matches!(peer.read(), Some(Message::Request(request)) if request.method == "initialize"),
        "the client opens with initialize"
    );
    let end = harness.to_end();
    let [.., error, status] = end.as_slice() else {
        panic!("an error and a stop: {end:#?}");
    };
    assert!(
        matches!(error, Update::Error(client::Error::Timeout { after }) if *after == timeout),
        "the timeout error comes right before the stop: {end:#?}"
    );
    assert_eq!(
        stopped(status),
        Some(&Reason::Timeout),
        "the stop is last and says timeout: {end:#?}"
    );
    assert_eq!(
        statuses(&end),
        [Status::Stopped(Reason::Timeout)],
        "one stop: {end:#?}"
    );
    drop(peer);
}

/// A listener no server dialed closes on shutdown and on drop, and its port is free again.
#[test]
fn listen_releases_its_port_when_shut_down_before_any_dial() {
    let address = vacant();
    let mut harness = Harness::new(builder().listen(address).expect("the port binds"));
    let asked = Instant::now();
    harness.shut_down();
    let elapsed = asked.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "the stop came at once: {elapsed:?}"
    );
    drop(TcpListener::bind(address).expect("the port is free after the stop"));

    let dropped = builder().listen(address).expect("the port binds again");
    drop(dropped);
    let deadline = Instant::now() + Duration::from_secs(1);
    while let Err(error) = TcpListener::bind(address) {
        assert!(
            Instant::now() < deadline,
            "the port is free within a poll of the drop: {error}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}
