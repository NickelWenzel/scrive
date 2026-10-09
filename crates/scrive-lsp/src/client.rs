//! The language-server client: one connection to one server, built by a [`Builder`] terminal,
//! with incoming traffic as the [`Events`] stream.

pub mod builder;
pub mod error;
mod event;
mod events;
mod status;
#[cfg(test)]
mod tests;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use lsp_types::Uri;
use scrive_core::{
    document, intel, CompletionRequest, DefinitionRequest, DocId, FormatRequest, HoverRequest,
    RenameRequest, SignatureRequest, Snapshot,
};
use serde_json::Value;

pub use builder::Builder;
pub use error::Error;
pub use event::Event;
pub use events::Events;
pub use status::{Reason, Status};

use crate::session::{self, Session};
use crate::{message, trace, transport, update, Update};

static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);

/// The notifications the client sends itself, which `notify` must not duplicate.
const OWNED: [&str; 8] = {
    use lsp_types::notification::{
        Cancel, DidChangeConfiguration, DidChangeTextDocument, DidCloseTextDocument,
        DidOpenTextDocument, DidSaveTextDocument, Exit, Initialized, Notification,
    };
    [
        DidOpenTextDocument::METHOD,
        DidChangeTextDocument::METHOD,
        DidCloseTextDocument::METHOD,
        DidSaveTextDocument::METHOD,
        Initialized::METHOD,
        Exit::METHOD,
        Cancel::METHOD,
        DidChangeConfiguration::METHOD,
    ]
};

/// One connection to one language server, over the bridge its [`Builder`] chose: a child
/// process with `Builder::stdio`, or an in-process server with [`Builder::memory`].
///
/// The client sends by itself; the host runs the [`Events`] stream the builder returned and
/// passes each event to [`receive`](Self::receive), which folds it in and returns the updates.
/// Without that, no reply ever arrives. Documents are registered with [`open`](Self::open) and
/// kept current with [`sync`](Self::sync) after every round of edits; [`save`](Self::save)
/// reports that the synced text was written to disk. [`shutdown`](Self::shutdown), or dropping
/// the client, ends the connection.
#[derive(Debug)]
pub struct Client {
    id: Id,
    session: Session,
    connection: Connection,
    /// The bridge's worker, told when the client is done with the server.
    control: transport::Control,
    /// Feeds the client's own events into `Events`: outgoing traces and its stops.
    local: futures_channel::mpsc::UnboundedSender<Event>,
    trace: trace::Mode,
    status: Status,
}

/// A process-unique client identity, so an editor can tell which client it is registered with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Id(u64);

/// What the client holds of its transport, which decides where messages go.
#[derive(Debug)]
enum Connection {
    /// The session's messages go to `link`.
    Live { link: transport::Link },
    /// `shutdown()` ran or the connection stopped: absorbing. After a worker bridge's
    /// `shutdown()` the link stays, so server requests during the grace period still get their
    /// `null` answers.
    Shut(Option<transport::Link>),
}

/// The server request a response answers: its id and method.
struct Answering {
    id: message::Id,
    method: String,
}

/// Dropping the client ends the connection as [`Client::shutdown`] would, without tracing.
impl Drop for Client {
    fn drop(&mut self) {
        let Connection::Live { link } = &self.connection else {
            return;
        };
        match &self.control {
            transport::Control::Memory => {
                for message in self.goodbye() {
                    link.send(serialize(&message));
                }
            }
            #[cfg(not(target_family = "wasm"))]
            transport::Control::Stdio(control) => {
                control.send(transport::Lifecycle::Shutdown {
                    handshake: self.handshake(),
                });
            }
        }
    }
}

impl Client {
    /// A builder for a new client.
    pub fn builder() -> Builder {
        Builder::default()
    }

    /// This client's process-unique identity.
    #[must_use]
    pub fn id(&self) -> Id {
        self.id
    }

    /// Where the connection stands, as of the last status update [`receive`](Self::receive)
    /// returned. Starts at [`Status::Starting`].
    #[must_use]
    pub fn status(&self) -> &Status {
        &self.status
    }

    /// Registers the document of `snapshot` under `uri`, and tells the server once it is
    /// initialized. Opening a document that is already registered re-registers it, closing it
    /// first.
    ///
    /// Diagnostics the server published for `uri` while it was not open come back as the
    /// document update, stamped with the snapshot's revision.
    ///
    /// # Errors
    /// [`Error::DuplicateUri`] when another document is registered under the same normalized URI.
    pub fn open(
        &mut self,
        snapshot: &Snapshot,
        uri: &Uri,
        language: impl Into<String>,
    ) -> Result<Option<update::Document>, Error> {
        let output = self.session.open(snapshot, uri, language)?;
        Ok(self.answered(output))
    }

    /// Forgets a document, cancelling its requests in flight, and sends `didClose` if the server
    /// was told about it. Its version high-water mark stays, so a reopen continues the count.
    pub fn close(&mut self, doc_id: DocId) {
        let output = self.session.close(doc_id);
        self.sent(output);
    }

    /// Brings the server up to `snapshot`. `changes` is the document's drained change log: when
    /// it leads exactly from what the server has to `snapshot`, and the server syncs
    /// incrementally, the edits go out as ranges; otherwise the whole text goes out. Either way
    /// the snapshot becomes the one server positions are converted against.
    ///
    /// A snapshot no newer than the synced one and a document that is not registered send
    /// nothing. While the server is not running, the snapshot is stored and nothing is sent.
    pub fn sync(&mut self, snapshot: &Snapshot, changes: document::Changes) {
        let output = self.session.sync(snapshot, changes);
        self.sent(output);
    }

    /// Tells the server that the document of `snapshot` was written to disk, with the text when
    /// the server asks for it. The host writes the file and calls [`sync`](Self::sync) first:
    /// `snapshot` must be the synced one, so the saved text is the text the server has.
    ///
    /// Nothing is sent for a snapshot other than the synced one, a document that is not
    /// registered or not yet open on the server, a server that asks for no saves, or while the
    /// server is not running.
    pub fn save(&mut self, snapshot: &Snapshot) {
        let output = self.session.save(snapshot);
        self.sent(output);
    }

    /// Answers the editor's completion request: locally from the list of the request it
    /// continues, by adopting it into that request while it is in flight, or with a new
    /// `textDocument/completion` that supersedes the document's previous one.
    ///
    /// When the server cannot be asked (it is not running, has no completion provider, or did not
    /// register the trigger the text before the caret ends with) the request is declined with an
    /// empty [`update::Change::Completions`] under its ticket, so the editor stops waiting. A
    /// request for a document that is not registered gets nothing, and so does one from a
    /// revision other than `snapshot`'s or the last synced one while the server runs. Otherwise
    /// a request at or past the synced revision is declined.
    #[must_use]
    pub fn complete(
        &mut self,
        snapshot: &Snapshot,
        request: &CompletionRequest,
    ) -> Option<update::Document> {
        let output = self.session.complete(snapshot, request);
        self.answered(output)
    }

    /// Answers the editor's signature request with a `textDocument/signatureHelp` at its caret.
    /// While the caret stays in one call, a request in flight for that call is adopted instead of
    /// superseded.
    ///
    /// When the server is not running or has no signature provider, the request is declined with
    /// [`update::Change::Signature`]`(None)` under its ticket. Stale requests get nothing, as for
    /// [`complete`](Self::complete).
    #[must_use]
    pub fn signature_help(
        &mut self,
        snapshot: &Snapshot,
        request: &SignatureRequest,
    ) -> Option<update::Document> {
        let output = self.session.signature_help(snapshot, request);
        self.answered(output)
    }

    /// Answers the editor's hover request with a `textDocument/hover` at its offset, superseding
    /// the document's previous hover request.
    ///
    /// When the server is not running or has no hover provider, the request is declined with
    /// [`update::Change::Hover`]`(None)` under its ticket. Stale requests get nothing, as for
    /// [`complete`](Self::complete).
    #[must_use]
    pub fn hover(
        &mut self,
        snapshot: &Snapshot,
        request: &HoverRequest,
    ) -> Option<update::Document> {
        let output = self.session.hover(snapshot, request);
        self.answered(output)
    }

    /// Asks the server where the symbol at the request's offset is defined. The answer is an
    /// [`update::Change::Definition`] under the request's ticket.
    ///
    /// When the server is not running or has no definition provider, the request is declined
    /// with [`update::Change::Definition`]`(None)` under its ticket. Stale requests get nothing,
    /// as for [`complete`](Self::complete).
    #[must_use]
    pub fn definition(
        &mut self,
        snapshot: &Snapshot,
        request: &DefinitionRequest,
    ) -> Option<update::Document> {
        let output = self.session.definition(snapshot, request);
        self.answered(output)
    }

    /// Asks the server to rename the symbol at the request's offset to its new name. The answer
    /// arrives through [`receive`](Self::receive) as edits: an [`update::Change::Edits`] stamped
    /// with the synced revision for each open document, and an [`Update::FileEdits`] for each
    /// file that is not open. Either the whole rename arrives, or an [`Update::Error`] reports
    /// why none of it can: [`Error::StaleEdit`] or [`Error::Unsupported`].
    ///
    /// When the server is not running or has no rename provider, or the request is stale,
    /// nothing is sent.
    #[must_use]
    pub fn rename(
        &mut self,
        snapshot: &Snapshot,
        request: &RenameRequest,
    ) -> Option<update::Document> {
        let output = self.session.rename(snapshot, request);
        self.answered(output)
    }

    /// Asks the server to format the whole document, indenting with spaces. The answer is an
    /// [`update::Change::Edits`] under the request's ticket; a result that changes nothing
    /// answers nothing.
    ///
    /// When the server is not running or has no formatting provider, or the request is stale,
    /// nothing is sent.
    #[must_use]
    pub fn format(
        &mut self,
        snapshot: &Snapshot,
        request: &FormatRequest,
    ) -> Option<update::Document> {
        let output = self.session.format(snapshot, request);
        self.answered(output)
    }

    /// Asks the server for the inlay hints over the request's byte span, clamped to the
    /// document. The answer is an [`update::Change::Inlays`] under the request's ticket,
    /// replacing the hints the editor shows.
    ///
    /// When the server is not running or has no inlay hint provider, the request is declined
    /// with an empty [`update::Change::Inlays`] under its ticket. Stale requests get nothing, as
    /// for [`complete`](Self::complete).
    #[must_use]
    pub fn inlays(
        &mut self,
        snapshot: &Snapshot,
        request: &intel::inlay::Request,
    ) -> Option<update::Document> {
        let output = self.session.inlays(snapshot, request);
        self.answered(output)
    }

    /// Answers a gesture on an inlay hint from the document's last hint answer: a tooltip with
    /// [`update::Change::InlayTooltip`], a jump with [`update::Change::Definition`], an insert
    /// with [`update::Change::Edits`], each under the gesture's ticket. A gesture that cannot be
    /// served is declined with its empty answer; one for a document that is not registered gets
    /// nothing.
    #[must_use]
    pub fn interact(
        &mut self,
        snapshot: &Snapshot,
        interaction: &intel::inlay::Interaction,
    ) -> Option<update::Document> {
        let output = self.session.interact(snapshot, interaction);
        self.answered(output)
    }

    /// Replaces the settings the server reads through `workspace/configuration`. A running
    /// server is told with `workspace/didChangeConfiguration`; before the handshake, the
    /// handshake pushes them.
    pub fn configure(&mut self, configuration: Value) {
        let output = self.session.configure(configuration);
        self.sent(output);
    }

    /// Sends a notification the client does not send itself, such as a server-specific one.
    /// Dropped unless the server is running. Document sync, `initialized`, `exit`,
    /// `$/cancelRequest` and `workspace/didChangeConfiguration` belong to the client (use
    /// [`configure`](Self::configure) for the last).
    pub fn notify<N: lsp_types::notification::Notification>(&mut self, params: N::Params) {
        debug_assert!(
            !OWNED.contains(&N::METHOD),
            "`{}` belongs to the client",
            N::METHOD
        );
        if self.session.running() {
            let notification = message::Notification::new::<N>(params);
            self.send(vec![message::Message::Notification(notification)], None);
        }
    }

    /// Whether to record traffic as [`Update::Trace`] from now on.
    pub fn set_trace(&mut self, mode: trace::Mode) {
        self.trace = mode;
    }

    /// Ends the connection: every request in flight settles with its empty answer, returned for
    /// the editors to apply; from then on nothing is synced and every request declines. The
    /// server is sent `shutdown` and `exit` (only `exit` before the handshake completed), and
    /// [`Status::Stopped`]`(`[`Reason::Shutdown`]`)` follows through [`Events`]. A server process
    /// gets the grace period for its reply to `shutdown`, then for exiting after `exit`, and is
    /// killed after that. A second call does nothing.
    #[must_use]
    pub fn shutdown(&mut self) -> Vec<update::Document> {
        let goodbye = self.goodbye();
        #[cfg(not(target_family = "wasm"))]
        let handshake = self.handshake();
        let settled = self.session.shutdown();
        debug_assert!(
            settled.messages.is_empty(),
            "the session sends no shutdown of its own"
        );
        if let Connection::Live { .. } = self.connection {
            match &self.control {
                transport::Control::Memory => {
                    self.send(goodbye, None);
                    self.stop(Reason::Shutdown);
                }
                #[cfg(not(target_family = "wasm"))]
                transport::Control::Stdio(control) => {
                    control.send(transport::Lifecycle::Shutdown { handshake });
                    let connection =
                        std::mem::replace(&mut self.connection, Connection::Shut(None));
                    if let Connection::Live { link } = connection {
                        self.connection = Connection::Shut(Some(link));
                    }
                }
            }
        }
        documents(settled.updates)
    }

    /// Folds one event from this client's [`Events`] in, and returns what the host must act on,
    /// in order: an incoming message's [`Update::Trace`] first, a [`Update::Status`] change last.
    /// Server requests are answered, and messages the event causes are sent. After the client
    /// stopped, events change nothing.
    #[must_use]
    pub fn receive(&mut self, event: Event) -> Vec<Update> {
        let (client, payload) = event.into_parts();
        debug_assert_eq!(
            client, self.id,
            "an Event goes to the Client whose Events yielded it"
        );
        if let Status::Stopped(_) = self.status {
            return Vec::new();
        }
        match payload {
            event::Payload::Transport(transport::Event::Message(body)) => self.received(body),
            #[cfg(not(target_family = "wasm"))]
            event::Payload::Transport(transport::Event::Log(entries)) => {
                entries.iter().cloned().map(Update::Log).collect()
            }
            #[cfg(not(target_family = "wasm"))]
            event::Payload::Transport(transport::Event::Error(error)) => vec![Update::Error(error)],
            event::Payload::Sent(entry) => vec![Update::Trace(entry)],
            event::Payload::Transport(transport::Event::Stopped(reason))
            | event::Payload::Stopped(reason) => self.stopped(reason),
        }
    }

    fn new(
        id: Id,
        session: Session,
        link: transport::Link,
        control: transport::Control,
        local: futures_channel::mpsc::UnboundedSender<Event>,
        trace: trace::Mode,
    ) -> Self {
        Self {
            id,
            session,
            connection: Connection::Live { link },
            control,
            local,
            trace,
            status: Status::Starting,
        }
    }

    /// Serializes each message once, traces it, and queues it on the connection. `answering`
    /// names the server request a response answers, for its trace.
    fn send(&mut self, messages: Vec<message::Message>, answering: Option<&Answering>) {
        for message in messages {
            let body = serialize(&message);
            if self.trace == trace::Mode::Messages {
                let method = match &message {
                    message::Message::Request(request) => Some(request.method.as_str()),
                    message::Message::Notification(notification) => {
                        Some(notification.method.as_str())
                    }
                    message::Message::Response(response) => answering
                        .filter(|answering| response.id.as_ref() == Some(&answering.id))
                        .map(|answering| answering.method.as_str()),
                };
                let entry =
                    trace::Entry::new(trace::Direction::Outgoing, method, Arc::clone(&body));
                // `Events` was dropped: nobody reads traces any more.
                let _ = self
                    .local
                    .unbounded_send(Event::new(self.id, event::Payload::Sent(entry)));
            }
            match &self.connection {
                Connection::Live { link } | Connection::Shut(Some(link)) => link.send(body),
                Connection::Shut(None) => {}
            }
        }
    }

    /// Sends `output`'s messages, and returns its one local answer.
    fn answered(&mut self, output: session::Output) -> Option<update::Document> {
        let session::Output { messages, updates } = output;
        self.send(messages, None);
        debug_assert!(updates.len() <= 1, "a request answers at most once locally");
        let update = updates.into_iter().next()?;
        let Update::Document(document) = update else {
            debug_assert!(false, "local answers are document updates, got {update:?}");
            return None;
        };
        Some(document)
    }

    /// Sends `output`'s messages; it has no updates.
    fn sent(&mut self, output: session::Output) {
        debug_assert!(
            output.updates.is_empty(),
            "sync, close, save and configure update nothing"
        );
        self.send(output.messages, None);
    }

    /// Ends the connection from this side: drops the link and queues `Stopped(reason)`.
    fn stop(&mut self, reason: Reason) {
        self.connection = Connection::Shut(None);
        // `Events` was dropped: nobody is left to tell.
        let _ = self
            .local
            .unbounded_send(Event::new(self.id, event::Payload::Stopped(reason)));
    }

    fn received(&mut self, body: Arc<[u8]>) -> Vec<Update> {
        let decoded = serde_json::from_slice::<message::Message>(&body);
        let mut updates = Vec::new();
        if self.trace == trace::Mode::Messages {
            let method = match &decoded {
                Ok(message::Message::Request(request)) => Some(request.method.as_str()),
                Ok(message::Message::Notification(notification)) => {
                    Some(notification.method.as_str())
                }
                Ok(message::Message::Response(response)) => {
                    response.id.as_ref().and_then(|id| self.session.method(id))
                }
                Err(_) => None,
            };
            let entry = trace::Entry::new(trace::Direction::Incoming, method, body);
            updates.push(Update::Trace(entry));
        }
        let message = match decoded {
            Ok(message) => message,
            Err(source) => {
                updates.push(Update::Error(Error::Envelope {
                    source: Arc::new(source),
                }));
                return updates;
            }
        };
        let answering = match &message {
            message::Message::Request(request) => Some(Answering {
                id: request.id.clone(),
                method: request.method.clone(),
            }),
            message::Message::Response(_) | message::Message::Notification(_) => None,
        };
        let initializing = self.session.initializing();
        match self.session.receive(message) {
            Ok(output) => {
                self.send(output.messages, answering.as_ref());
                updates.extend(output.updates);
            }
            Err(error) => updates.push(Update::Error(error)),
        }
        if initializing && self.session.running() {
            self.status = Status::Running;
            updates.push(Update::Status(Status::Running));
        } else if initializing && !self.session.initializing() {
            // No capabilities, so nothing can be synced: the connection is over.
            #[cfg(not(target_family = "wasm"))]
            self.control.send(transport::Lifecycle::Shutdown {
                handshake: transport::Handshake::Pending,
            });
            self.stop(Reason::Initialize);
        }
        updates
    }

    /// The connection is over: settles and clears through `disconnected`, once, then reports it.
    fn stopped(&mut self, reason: Reason) -> Vec<Update> {
        self.connection = Connection::Shut(None);
        let session::Output {
            messages,
            mut updates,
        } = self.session.disconnected();
        debug_assert!(messages.is_empty(), "a disconnected session sends nothing");
        self.status = Status::Stopped(reason.clone());
        updates.push(Update::Status(Status::Stopped(reason)));
        updates
    }

    /// `shutdown` then `exit`, without waiting for the reply; before the handshake only `exit`,
    /// since a server treats anything before `initialize` as a protocol error.
    fn goodbye(&self) -> Vec<message::Message> {
        let exit = message::Message::Notification(message::Notification::new::<
            lsp_types::notification::Exit,
        >(()));
        if self.session.initializing() {
            return vec![exit];
        }
        let shutdown = message::Request::new::<lsp_types::request::Shutdown>(
            message::Id::String(transport::SHUTDOWN_ID.to_owned()),
            (),
        );
        vec![message::Message::Request(shutdown), exit]
    }

    /// Whether the server answered `initialize`, for the worker's shutdown sequence.
    #[cfg(not(target_family = "wasm"))]
    fn handshake(&self) -> transport::Handshake {
        if self.session.running() {
            transport::Handshake::Done
        } else {
            transport::Handshake::Pending
        }
    }
}

impl Id {
    pub(crate) fn next() -> Self {
        Self(NEXT_CLIENT.fetch_add(1, Ordering::Relaxed))
    }
}

/// `message` as the JSON text that goes on the wire.
pub(crate) fn serialize(message: &message::Message) -> Arc<[u8]> {
    serde_json::to_vec(message)
        .expect("envelopes serialize to JSON")
        .into()
}

/// The document updates among `updates`, which the session's settles are made of.
fn documents(updates: Vec<Update>) -> Vec<update::Document> {
    updates
        .into_iter()
        .filter_map(|update| {
            let Update::Document(document) = update else {
                debug_assert!(false, "settles are document updates, got {update:?}");
                return None;
            };
            Some(document)
        })
        .collect()
}
