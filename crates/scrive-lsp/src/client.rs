//! The LSP client: a pure state machine from JSON-RPC messages and document snapshots to messages
//! to send and updates to apply.

mod capabilities;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use lsp_types::error_codes::REQUEST_CANCELLED;
use lsp_types::{
    ApplyWorkspaceEditResponse, ClientInfo, CompletionContext, CompletionParams,
    CompletionTriggerKind, ConfigurationParams, DidChangeConfigurationParams,
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    InitializeParams, InitializeResult, InitializedParams, PublishDiagnosticsParams,
    TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, TextDocumentSyncKind, Uri, VersionedTextDocumentIdentifier,
    WorkspaceFolder,
};
use scrive_core::{
    document, Bias, CompletionRequest, CompletionTrigger, DocId, Revision, Snapshot, Ticket,
};
use serde_json::Value;

use crate::message::{self, Message};
use crate::update::{self, Update};
use crate::{completion, diagnostics, uri, Encoding};

/// JSON-RPC's "method not found": a server request this client does not implement.
const METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC's "invalid params": a server request whose params do not decode.
const INVALID_PARAMS: i64 = -32602;

static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);

/// One connection to one language server.
///
/// The host builds it with [`Client::builder`], sends the `initialize` request that
/// [`Builder::build`] returns, then passes every server message to [`receive`](Self::receive).
/// Documents are registered with [`open`](Self::open) and kept current with
/// [`sync`](Self::sync) after every round of edits. Every entry point returns what to send
/// and what to apply; the client itself never does I/O.
#[derive(Debug)]
pub struct Client {
    id: Id,
    state: State,
    encoding: Encoding,
    root: Option<uri::Key>,
    configuration: Option<Value>,
    /// In registration order, so the deferred `didOpen`s go out in a deterministic order.
    tracked: Vec<Tracked>,
    /// Version high-water marks. They survive `close`, so a reopened document continues its
    /// count and a late publish for the old incarnation never matches the new one.
    versions: HashMap<uri::Key, i32>,
    /// The latest unversioned diagnostics for URIs that are not open, applied at `open`.
    cached: HashMap<uri::Key, Vec<lsp_types::Diagnostic>>,
    /// Requests in flight, at most one per document and [`Kind`].
    pending: Vec<Pending>,
    next_request: i64,
}

/// A process-unique client identity, so an editor can tell which client it is registered with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Id(u64);

/// Configures and builds a [`Client`].
#[must_use]
#[derive(Debug, Default)]
pub struct Builder {
    root: Option<Uri>,
    initialization_options: Option<Value>,
    configuration: Option<Value>,
    process_id: Option<u32>,
}

/// What an entry point hands back: messages for the transport and updates for the documents.
#[must_use]
#[derive(Clone, Debug, Default)]
pub struct Output {
    /// Messages to send to the server, in order.
    pub messages: Vec<Message>,
    /// Changes and notifications for the host, in order.
    pub updates: Vec<Update>,
}

/// Why an entry point had nothing to send.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A notification or response payload did not decode as the method's type.
    #[error("`{method}` payload does not decode: {source}")]
    Decode {
        /// The method whose payload failed.
        method: String,
        /// The decoding error.
        source: serde_json::Error,
    },
    /// The server answered a request with an error.
    #[error("server failed `{method}`: {error}")]
    Server {
        /// The document the request was for; `None` for `initialize`.
        doc_id: Option<DocId>,
        /// The request's method.
        method: String,
        /// The server's error object.
        error: message::Error,
    },
    /// Another document is already registered under this URI.
    #[error("`{uri}` is already open as another document")]
    DuplicateUri {
        /// The normalized URI.
        uri: uri::Key,
    },
}

/// The connection lifecycle.
#[derive(Debug)]
enum State {
    /// `initialize` is in flight.
    Initializing { request: message::Id },
    /// Initialized; the server's capabilities are known.
    Running(capabilities::Server),
    /// `shutdown` is in flight.
    ShuttingDown { request: message::Id },
    /// Nothing more goes out except answers to server requests.
    Exited,
}

/// One registered document.
#[derive(Debug)]
struct Tracked {
    doc_id: DocId,
    key: uri::Key,
    language: String,
    /// What the server has, or will get with the deferred `didOpen`.
    synced: Snapshot,
    /// The version last sent; `None` while the server has not been told about the document.
    version: Option<i32>,
}

/// One request in flight.
#[derive(Debug)]
struct Pending {
    id: message::Id,
    doc_id: DocId,
    /// The editor ticket the reply answers.
    ticket: Ticket,
    query: Query,
}

/// The kind of a pending request; with the document, it keys the pending table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Completion,
}

/// What a pending request asked, with what its reply needs.
#[derive(Debug)]
enum Query {
    Completion(completion::Query),
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

    /// The negotiated position encoding; utf-16 until `initialize` is answered.
    #[must_use]
    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    /// Registers the document of `snapshot` under `uri`, and tells the server once it is
    /// initialized. Opening a document that is already registered re-registers it, closing it
    /// first. After [`shutdown`](Self::shutdown) nothing is registered.
    ///
    /// Diagnostics the server published for `uri` while it was not open arrive in the output's
    /// updates, stamped with the snapshot's revision.
    ///
    /// # Errors
    /// [`Error::DuplicateUri`] when another document is registered under the same normalized URI.
    pub fn open(
        &mut self,
        snapshot: &Snapshot,
        uri: &Uri,
        language: impl Into<String>,
    ) -> Result<Output, Error> {
        if matches!(self.state, State::ShuttingDown { .. } | State::Exited) {
            return Ok(Output::default());
        }
        let key = uri::normalize(uri);
        let doc_id = snapshot.doc_id();
        if self
            .tracked
            .iter()
            .any(|t| t.key == key && t.doc_id != doc_id)
        {
            return Err(Error::DuplicateUri { uri: key });
        }
        let mut output = self.close(doc_id);
        if let Some(cached) = self.cached.remove(&key) {
            output.updates.push(Update::Document(update::Document::new(
                doc_id,
                update::Stamp::Revision(snapshot.revision()),
                update::Change::Diagnostics(diagnostics::convert(self.encoding, snapshot, &cached)),
            )));
        }
        self.tracked.push(Tracked {
            doc_id,
            key,
            language: language.into(),
            synced: snapshot.clone(),
            version: None,
        });
        if self.opens_and_closes() {
            let index = self.tracked.len() - 1;
            output.messages.push(self.did_open(index));
        }
        Ok(output)
    }

    /// Forgets a document, cancelling its requests in flight, and sends `didClose` if the server
    /// was told about it. Its version high-water mark stays, so a reopen continues the count.
    pub fn close(&mut self, doc_id: DocId) -> Output {
        let Some(index) = self.tracked.iter().position(|t| t.doc_id == doc_id) else {
            return Output::default();
        };
        let tracked = self.tracked.remove(index);
        let mut output = Output::default();
        self.pending.retain(|pending| {
            let keep = pending.doc_id != doc_id;
            if !keep {
                output.messages.push(cancel(pending.id.clone()));
            }
            keep
        });
        if tracked.version.is_some() && self.opens_and_closes() {
            output
                .messages
                .push(Message::Notification(message::Notification::new::<
                    lsp_types::notification::DidCloseTextDocument,
                >(
                    DidCloseTextDocumentParams {
                        text_document: TextDocumentIdentifier {
                            uri: tracked.key.uri().clone(),
                        },
                    },
                )));
        }
        output
    }

    /// Answers the editor's completion request with a `textDocument/completion` request, which
    /// supersedes the document's previous one.
    ///
    /// When the server cannot be asked (it is not running, has no completion provider, or did not
    /// register the trigger the text before the caret ends with) the request is declined with an
    /// empty [`update::Change::Completions`] under its ticket, so the editor stops waiting. A
    /// request from a revision other than `snapshot`'s or the last synced one, or for a document
    /// that is not registered, gets nothing: the editor has moved on.
    pub fn complete(&mut self, snapshot: &Snapshot, request: &CompletionRequest) -> Output {
        let doc_id = snapshot.doc_id();
        let ticket = request.ticket();
        let Some(tracked) = self.tracked.iter().find(|t| t.doc_id == doc_id) else {
            return Output::default();
        };
        if ticket.revision() != snapshot.revision()
            || snapshot.revision() != tracked.synced.revision()
        {
            return Output::default();
        }
        let decline = || Output::answer(doc_id, ticket, update::Change::Completions(Vec::new()));
        let State::Running(server) = &self.state else {
            return decline();
        };
        let Some(triggers) = &server.completion else {
            return decline();
        };
        let word = request.word();
        let context = match request.trigger() {
            CompletionTrigger::TriggerChar(_) => {
                match matched_trigger(snapshot, word.end, triggers) {
                    Some(trigger) => CompletionContext {
                        trigger_kind: CompletionTriggerKind::TRIGGER_CHARACTER,
                        trigger_character: Some(trigger),
                    },
                    None => return decline(),
                }
            }
            CompletionTrigger::Typed(_) | CompletionTrigger::Manual => CompletionContext {
                trigger_kind: CompletionTriggerKind::INVOKED,
                trigger_character: None,
            },
        };
        self.send(
            doc_id,
            ticket,
            Query::Completion(completion::Query { word, context }),
        )
    }

    /// Brings the server up to `snapshot`. `changes` is the document's drained change log: when
    /// it leads exactly from what the server has to `snapshot`, and the server syncs
    /// incrementally, the edits go out as ranges; otherwise the whole text goes out. Servers that
    /// take no changes are sent nothing. Either way the snapshot becomes the one server
    /// positions are converted against.
    ///
    /// A snapshot no newer than the synced one, a document that is not registered, and any call
    /// after [`shutdown`](Self::shutdown) send nothing.
    pub fn sync(&mut self, snapshot: &Snapshot, changes: document::Changes) -> Output {
        let Self {
            state,
            encoding,
            tracked,
            versions,
            ..
        } = self;
        let Some(tracked) = tracked.iter_mut().find(|t| t.doc_id == snapshot.doc_id()) else {
            return Output::default();
        };
        if snapshot.revision() <= tracked.synced.revision() {
            return Output::default();
        }
        let server = match state {
            State::Running(server) => server,
            State::Initializing { .. } => {
                tracked.synced = snapshot.clone();
                return Output::default();
            }
            State::ShuttingDown { .. } | State::Exited => return Output::default(),
        };
        let content_changes = if !server.open_close || tracked.version.is_none() {
            None
        } else if server.change == TextDocumentSyncKind::INCREMENTAL {
            Some(
                incremental(*encoding, &tracked.synced, snapshot, &changes)
                    .unwrap_or_else(|| full(snapshot)),
            )
        } else if server.change == TextDocumentSyncKind::FULL {
            Some(full(snapshot))
        } else {
            None
        };
        tracked.synced = snapshot.clone();
        let Some(content_changes) = content_changes else {
            return Output::default();
        };
        let version = next_version(versions, &tracked.key);
        tracked.version = Some(version);
        Output {
            messages: vec![Message::Notification(message::Notification::new::<
                lsp_types::notification::DidChangeTextDocument,
            >(
                DidChangeTextDocumentParams {
                    text_document: VersionedTextDocumentIdentifier {
                        uri: tracked.key.uri().clone(),
                        version,
                    },
                    content_changes,
                },
            ))],
            updates: Vec::new(),
        }
    }

    /// Starts an orderly shutdown: sends `shutdown`, and `exit` once its response arrives. Before
    /// initialization completes there is nothing to shut down, and the client exits silently.
    /// Afterwards every client call declines, and server requests are answered with `null`.
    pub fn shutdown(&mut self) -> Output {
        match self.state {
            State::Initializing { .. } => {
                self.state = State::Exited;
                Output::default()
            }
            State::Running(_) => {
                // Their replies could no longer be delivered, and `shutdown` goes out alone.
                self.pending.clear();
                let request = self.next_request();
                self.state = State::ShuttingDown {
                    request: request.clone(),
                };
                Output {
                    messages: vec![Message::Request(message::Request::new::<
                        lsp_types::request::Shutdown,
                    >(request, ()))],
                    updates: Vec::new(),
                }
            }
            State::ShuttingDown { .. } | State::Exited => Output::default(),
        }
    }

    /// Folds one message from the server into the client. Server requests are always answered.
    ///
    /// # Errors
    /// [`Error::Decode`] for a payload that does not decode (a `publishDiagnostics`, or the
    /// `initialize` result), and [`Error::Server`] when `initialize` fails. A failed or
    /// undecodable `initialize` leaves the client exited, with its registered documents dropped.
    pub fn receive(&mut self, message: Message) -> Result<Output, Error> {
        match message {
            Message::Request(request) => Ok(self.answer(request)),
            Message::Notification(notification) => self.notified(notification),
            Message::Response(response) => self.responded(response),
        }
    }

    /// `publishDiagnostics` for an open document applies only if its version, when present, is
    /// the last one sent. A missing version is read as the last one sent, since servers that
    /// ignore `versionSupport` would otherwise never land a diagnostic.
    fn notified(&mut self, notification: message::Notification) -> Result<Output, Error> {
        if notification.method != "textDocument/publishDiagnostics" {
            return Ok(Output {
                messages: Vec::new(),
                updates: vec![Update::Notification(notification)],
            });
        }
        let params: PublishDiagnosticsParams = decode(&notification.method, notification.params)?;
        let key = uri::normalize(&params.uri);
        let Some(tracked) = self.tracked.iter().find(|t| t.key == key) else {
            // A versioned publish cannot apply to a closed URI, but it does supersede the
            // unversioned set cached for it.
            if params.version.is_some() || params.diagnostics.is_empty() {
                self.cached.remove(&key);
            } else {
                self.cached.insert(key, params.diagnostics);
            }
            return Ok(Output::default());
        };
        if params.version.is_some() && params.version != tracked.version {
            return Ok(Output::default());
        }
        Ok(Output {
            messages: Vec::new(),
            updates: vec![Update::Document(update::Document::new(
                tracked.doc_id,
                update::Stamp::Revision(tracked.synced.revision()),
                update::Change::Diagnostics(diagnostics::convert(
                    self.encoding,
                    &tracked.synced,
                    &params.diagnostics,
                )),
            ))],
        })
    }

    fn responded(&mut self, response: message::Response) -> Result<Output, Error> {
        let Some(id) = response.id else {
            return Ok(Output::default());
        };
        match &self.state {
            State::Initializing { request } if *request == id => self.initialized(response.result),
            // Any answer completes the shutdown handshake, an error one included.
            State::ShuttingDown { request } if *request == id => {
                self.state = State::Exited;
                Ok(Output {
                    messages: vec![Message::Notification(message::Notification::new::<
                        lsp_types::notification::Exit,
                    >(()))],
                    updates: Vec::new(),
                })
            }
            State::Initializing { .. }
            | State::Running(_)
            | State::ShuttingDown { .. }
            | State::Exited => Ok(self.settled(&id, response.result)),
        }
    }

    /// Routes the reply to a pending request. Replies to requests the client no longer waits
    /// for, and cancellations, are silent. A failed or undecodable reply settles the editor's
    /// slot with the request's empty answer.
    fn settled(&mut self, id: &message::Id, result: Result<Value, message::Error>) -> Output {
        let Some(index) = self.pending.iter().position(|p| p.id == *id) else {
            return Output::default();
        };
        let entry = self.pending.swap_remove(index);
        let synced = self
            .tracked
            .iter()
            .find(|t| t.doc_id == entry.doc_id)
            .map(|t| t.synced.revision());
        if synced != Some(entry.ticket.revision()) {
            return Output::default();
        }
        match result {
            Err(error) if error.code == REQUEST_CANCELLED => Output::default(),
            Err(_) => entry.failed(),
            Ok(value) => self.resolved(&entry, value),
        }
    }

    fn resolved(&mut self, entry: &Pending, value: Value) -> Output {
        match &entry.query {
            Query::Completion(query) => self.completed(entry, query, value),
        }
    }

    fn initialized(&mut self, result: Result<Value, message::Error>) -> Result<Output, Error> {
        const METHOD: &str = "initialize";
        let result = result
            .map_err(|error| Error::Server {
                doc_id: None,
                method: METHOD.to_owned(),
                error,
            })
            .and_then(|value| decode::<InitializeResult>(METHOD, Some(value)));
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                // Without known capabilities nothing can be synced, so the connection is over.
                self.state = State::Exited;
                self.tracked.clear();
                return Err(error);
            }
        };
        self.encoding = Encoding::negotiate(result.capabilities.position_encoding.as_ref());
        self.state = State::Running(capabilities::Server::new(&result.capabilities));
        let mut output = Output::default();
        output
            .messages
            .push(Message::Notification(message::Notification::new::<
                lsp_types::notification::Initialized,
            >(InitializedParams {})));
        if let Some(settings) = &self.configuration {
            output
                .messages
                .push(Message::Notification(message::Notification::new::<
                    lsp_types::notification::DidChangeConfiguration,
                >(
                    DidChangeConfigurationParams {
                        settings: settings.clone(),
                    },
                )));
        }
        if self.opens_and_closes() {
            for index in 0..self.tracked.len() {
                output.messages.push(self.did_open(index));
            }
        }
        Ok(output)
    }

    /// Whether the server is running and wants `didOpen`/`didClose`.
    fn opens_and_closes(&self) -> bool {
        matches!(&self.state, State::Running(server) if server.open_close)
    }

    /// `didOpen` for `tracked[index]` at the next version, with its synced text.
    fn did_open(&mut self, index: usize) -> Message {
        let Self {
            tracked, versions, ..
        } = self;
        let tracked = &mut tracked[index];
        let version = next_version(versions, &tracked.key);
        tracked.version = Some(version);
        Message::Notification(message::Notification::new::<
            lsp_types::notification::DidOpenTextDocument,
        >(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: tracked.key.uri().clone(),
                language_id: tracked.language.clone(),
                version,
                text: tracked.synced.text().into_owned(),
            },
        }))
    }

    fn next_request(&mut self) -> message::Id {
        let id = self.next_request;
        self.next_request += 1;
        message::Id::Number(id)
    }

    /// Sends `query` for `doc_id` at its synced snapshot, cancelling the request of the same
    /// kind it supersedes.
    fn send(&mut self, doc_id: DocId, ticket: Ticket, query: Query) -> Output {
        let mut output = Output::default();
        if let Some(index) = self
            .pending
            .iter()
            .position(|p| p.doc_id == doc_id && p.query.kind() == query.kind())
        {
            output
                .messages
                .push(cancel(self.pending.swap_remove(index).id));
        }
        let Some(index) = self.tracked.iter().position(|t| t.doc_id == doc_id) else {
            return output;
        };
        let id = self.next_request();
        let tracked = &self.tracked[index];
        output.messages.push(Message::Request(query.request(
            id.clone(),
            tracked.key.uri(),
            self.encoding,
            &tracked.synced,
        )));
        self.pending.push(Pending {
            id,
            doc_id,
            ticket,
            query,
        });
        output
    }

    fn completed(&self, entry: &Pending, query: &completion::Query, value: Value) -> Output {
        let Some(reply) = completion::Reply::decode(value) else {
            return entry.failed();
        };
        let Some(tracked) = self.tracked.iter().find(|t| t.doc_id == entry.doc_id) else {
            return Output::default();
        };
        // `settled` checked that the ticket's revision is the synced one, so the synced snapshot
        // is the text the request was made against.
        let candidates =
            completion::convert(self.encoding, &tracked.synced, query.word.clone(), reply);
        let word = tracked.synced.slice(query.word.clone());
        Output::answer(
            entry.doc_id,
            entry.ticket,
            update::Change::Completions(completion::answer(&candidates, &word)),
        )
    }

    fn answer(&self, request: message::Request) -> Output {
        let response = match self.state {
            // After `shutdown()` the client promises nothing; `null` keeps the server unblocked.
            State::ShuttingDown { .. } | State::Exited => message::Response::ok(request.id, ()),
            State::Initializing { .. } | State::Running(_) => self.respond(request),
        };
        Output {
            messages: vec![Message::Response(response)],
            updates: Vec::new(),
        }
    }

    fn respond(&self, request: message::Request) -> message::Response {
        let message::Request { id, method, params } = request;
        match method.as_str() {
            "workspace/configuration" => {
                match serde_json::from_value::<ConfigurationParams>(params.unwrap_or(Value::Null)) {
                    Ok(params) => message::Response::ok(
                        id,
                        params
                            .items
                            .iter()
                            .map(|item| self.section(item.section.as_deref()))
                            .collect::<Vec<_>>(),
                    ),
                    Err(error) => message::Response::error(
                        Some(id),
                        message::Error {
                            code: INVALID_PARAMS,
                            message: error.to_string(),
                            data: None,
                        },
                    ),
                }
            }
            "workspace/workspaceFolders" => {
                message::Response::ok(id, self.root.as_ref().map(|root| vec![folder(root)]))
            }
            "client/registerCapability"
            | "client/unregisterCapability"
            | "window/workDoneProgress/create"
            | "window/showMessageRequest" => message::Response::ok(id, ()),
            "workspace/applyEdit" => message::Response::ok(
                id,
                ApplyWorkspaceEditResponse {
                    applied: false,
                    failure_reason: Some(
                        "scrive-lsp does not apply server-initiated edits".to_owned(),
                    ),
                    failed_change: None,
                },
            ),
            // `workspace/semanticTokens/refresh`, `workspace/inlayHint/refresh`, …: nothing is
            // cached that a refresh would invalidate.
            method if method.starts_with("workspace/") && method.ends_with("/refresh") => {
                message::Response::ok(id, ())
            }
            method => message::Response::error(
                Some(id),
                message::Error {
                    code: METHOD_NOT_FOUND,
                    message: format!("`{method}` is not supported"),
                    data: None,
                },
            ),
        }
    }

    /// The dotted `section` of the configuration. An empty or absent section is the whole value,
    /// and a path that does not exist is `null`.
    fn section(&self, section: Option<&str>) -> Value {
        let Some(configuration) = &self.configuration else {
            return Value::Null;
        };
        match section.filter(|s| !s.is_empty()) {
            None => configuration.clone(),
            Some(path) => path
                .split('.')
                .try_fold(configuration, |value, key| value.get(key))
                .cloned()
                .unwrap_or(Value::Null),
        }
    }
}

impl Builder {
    /// The workspace root, sent as `rootUri`, `rootPath` and the one workspace folder.
    pub fn root(mut self, root: Uri) -> Self {
        self.root = Some(root);
        self
    }

    /// Sent verbatim as `initializationOptions`.
    pub fn initialization_options(mut self, options: Value) -> Self {
        self.initialization_options = Some(options);
        self
    }

    /// The settings object: pushed with `workspace/didChangeConfiguration` after `initialized`,
    /// and read by the server's `workspace/configuration` requests.
    pub fn configuration(mut self, configuration: Value) -> Self {
        self.configuration = Some(configuration);
        self
    }

    /// The host's process id, sent as `processId`. The client cannot read it itself, since wasm
    /// has no process.
    pub fn process_id(mut self, process_id: u32) -> Self {
        self.process_id = Some(process_id);
        self
    }

    /// The client, and the `initialize` request to send before anything else. The request's id
    /// is `1`.
    #[must_use]
    pub fn build(self) -> (Client, Message) {
        let root = self.root.as_ref().map(uri::normalize);
        let request = message::Id::Number(1);
        let initialize = message::Request::new::<lsp_types::request::Initialize>(
            request.clone(),
            self.initialize_params(root.as_ref()),
        );
        let client = Client {
            id: Id(NEXT_CLIENT.fetch_add(1, Ordering::Relaxed)),
            state: State::Initializing { request },
            encoding: Encoding::default(),
            root,
            configuration: self.configuration,
            tracked: Vec::new(),
            versions: HashMap::new(),
            cached: HashMap::new(),
            pending: Vec::new(),
            next_request: 2,
        };
        (client, Message::Request(initialize))
    }

    // `rootUri` and `rootPath` are deprecated in favour of `workspaceFolders`, but servers still
    // read them (pyright reads `rootPath`), so all three are sent.
    #[allow(deprecated)]
    fn initialize_params(&self, root: Option<&uri::Key>) -> InitializeParams {
        InitializeParams {
            process_id: self.process_id,
            root_uri: root.map(|root| root.uri().clone()),
            root_path: root.and_then(uri::Key::file_path),
            initialization_options: self.initialization_options.clone(),
            capabilities: capabilities::client(),
            workspace_folders: root.map(|root| vec![folder(root)]),
            client_info: Some(ClientInfo {
                name: "scrive-lsp".to_owned(),
                version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            }),
            ..InitializeParams::default()
        }
    }
}

impl Output {
    /// A ticket-stamped change for one document, with nothing to send.
    fn answer(doc_id: DocId, ticket: Ticket, change: update::Change) -> Self {
        Self {
            messages: Vec::new(),
            updates: vec![Update::Document(update::Document::new(
                doc_id,
                update::Stamp::Ticket(ticket),
                change,
            ))],
        }
    }
}

impl Pending {
    /// What settles the editor's slot after the server failed the request.
    fn failed(&self) -> Output {
        match self.query {
            Query::Completion(_) => Output::answer(
                self.doc_id,
                self.ticket,
                update::Change::Completions(Vec::new()),
            ),
        }
    }
}

impl Query {
    fn kind(&self) -> Kind {
        match self {
            Query::Completion(_) => Kind::Completion,
        }
    }

    /// The request message for this query at `snapshot`.
    fn request(
        &self,
        id: message::Id,
        uri: &Uri,
        encoding: Encoding,
        snapshot: &Snapshot,
    ) -> message::Request {
        match self {
            Query::Completion(query) => message::Request::new::<lsp_types::request::Completion>(
                id,
                CompletionParams {
                    text_document_position: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position: encoding.position(snapshot, query.word.end),
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                    context: Some(query.context.clone()),
                },
            ),
        }
    }
}

/// `$/cancelRequest` for `id`, built by hand because lsp-types' `CancelParams` holds an `i32` id,
/// narrower than [`message::Id`].
fn cancel(id: message::Id) -> Message {
    Message::Notification(message::Notification {
        method: "$/cancelRequest".to_owned(),
        params: Some(serde_json::json!({ "id": id })),
    })
}

/// The longest registered trigger that the text before `caret` ends with. Triggers may be
/// longer than one character (`::`).
fn matched_trigger(snapshot: &Snapshot, caret: u32, triggers: &[String]) -> Option<String> {
    let reach = triggers.iter().map(|t| t.len() as u32).max()?;
    let start = snapshot.clip_offset(caret.saturating_sub(reach), Bias::Left);
    let before = snapshot.slice(start..caret);
    triggers
        .iter()
        .filter(|t| !t.is_empty() && before.ends_with(t.as_str()))
        .max_by_key(|t| t.len())
        .cloned()
}

/// The next version for `key`, advancing its high-water mark.
// `Key` hashes and compares by its URI text, which fluent-uri's internal `Cell` never changes.
#[allow(clippy::mutable_key_type)]
fn next_version(versions: &mut HashMap<uri::Key, i32>, key: &uri::Key) -> i32 {
    let version = versions.entry(key.clone()).or_insert(0);
    *version += 1;
    *version
}

/// The whole document as one content change.
fn full(snapshot: &Snapshot) -> Vec<TextDocumentContentChangeEvent> {
    vec![TextDocumentContentChangeEvent {
        range: None,
        range_length: None,
        text: snapshot.text().into_owned(),
    }]
}

/// The logged edits as ranged content changes, or `None` when `changes` does not lead exactly
/// from `synced` to `snapshot`: another document's log, a broken log, a gap, or an end short of
/// `snapshot`.
fn incremental(
    encoding: Encoding,
    synced: &Snapshot,
    snapshot: &Snapshot,
    changes: &document::Changes,
) -> Option<Vec<TextDocumentContentChangeEvent>> {
    if changes.doc_id() != snapshot.doc_id() || changes.from() != Some(synced.revision()) {
        return None;
    }
    let mut cursor = synced.revision();
    let mut events = Vec::new();
    for change in changes.iter() {
        let before = change.before();
        if before.revision() != cursor {
            return None;
        }
        // The server applies content changes in order (LSP §textDocument_didChange). A
        // commit's ops are descending, so every op lies before the ones already applied and
        // its range is the same in `before` as in the text the server holds by then.
        events.extend(
            change
                .ops()
                .iter()
                .map(|op| TextDocumentContentChangeEvent {
                    range: Some(encoding.range(before, op.range.clone())),
                    range_length: None,
                    text: op.text.clone(),
                }),
        );
        cursor = Revision(cursor.0 + 1);
    }
    (cursor == snapshot.revision()).then_some(events)
}

/// The workspace folder for `root`, named after its last path segment.
fn folder(root: &uri::Key) -> WorkspaceFolder {
    WorkspaceFolder {
        uri: root.uri().clone(),
        name: root.name(),
    }
}

/// Decodes a payload, naming the method on failure. Absent params decode from `null`.
fn decode<T: serde::de::DeserializeOwned>(method: &str, params: Option<Value>) -> Result<T, Error> {
    serde_json::from_value(params.unwrap_or(Value::Null)).map_err(|source| Error::Decode {
        method: method.to_owned(),
        source,
    })
}
