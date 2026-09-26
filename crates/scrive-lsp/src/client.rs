//! The LSP client: a pure state machine from JSON-RPC messages and document snapshots to messages
//! to send and updates to apply.

mod capabilities;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use lsp_types::{
    ApplyWorkspaceEditResponse, ClientInfo, ConfigurationParams, DidChangeConfigurationParams,
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    InitializeParams, InitializeResult, InitializedParams, TextDocumentContentChangeEvent,
    TextDocumentIdentifier, TextDocumentItem, TextDocumentSyncKind, Uri,
    VersionedTextDocumentIdentifier, WorkspaceFolder,
};
use scrive_core::{document, DocId, Revision, Snapshot};
use serde_json::Value;

use crate::message::{self, Message};
use crate::update::Update;
use crate::{uri, Encoding};

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

    /// Forgets a document, and sends `didClose` if the server was told about it. Its version
    /// high-water mark stays, so a reopen continues the count.
    pub fn close(&mut self, doc_id: DocId) -> Output {
        let Some(index) = self.tracked.iter().position(|t| t.doc_id == doc_id) else {
            return Output::default();
        };
        let tracked = self.tracked.remove(index);
        let mut output = Output::default();
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
    /// [`Error::Decode`] for a payload that does not decode, and [`Error::Server`] when
    /// `initialize` fails. A failed `initialize` leaves the client exited, with its registered
    /// documents dropped.
    pub fn receive(&mut self, message: Message) -> Result<Output, Error> {
        match message {
            Message::Request(request) => Ok(self.answer(request)),
            Message::Notification(notification) => Ok(Output {
                messages: Vec::new(),
                updates: vec![Update::Notification(notification)],
            }),
            Message::Response(response) => self.responded(response),
        }
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
            | State::Exited => Ok(Output::default()),
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
