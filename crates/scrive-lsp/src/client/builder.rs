//! Configures a [`Client`](super::Client) and connects it through one bridge.

use lsp_types::{ClientInfo, InitializeParams, Uri, WorkspaceFolder};
use serde_json::Value;

use super::{Client, Events, Id};
use crate::session::{self, Session};
use crate::{message, trace, transport, uri};

/// Configures a [`Client`] and connects it through one bridge.
#[must_use]
#[derive(Debug, Default)]
pub struct Builder {
    root: Option<Uri>,
    initialization_options: Option<Value>,
    configuration: Option<Value>,
    trace: trace::Mode,
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

    /// The settings: pushed with `workspace/didChangeConfiguration` after `initialized`, read by
    /// the server's `workspace/configuration` requests, and replaced by
    /// [`Client::configure`].
    pub fn configuration(mut self, configuration: Value) -> Self {
        self.configuration = Some(configuration);
        self
    }

    /// Whether the client records traffic as [`Update::Trace`](crate::Update::Trace) from the
    /// start; [`Client::set_trace`] changes it later. Off by default.
    pub fn trace(mut self, mode: trace::Mode) -> Self {
        self.trace = mode;
        self
    }

    /// A client on the client end of an in-process pair from
    /// [`lsp_server::Connection::memory`], and its event stream. The host drives the server end.
    /// The client sends `initialize` at once; `processId` is `null`.
    ///
    /// Outgoing messages are serialized, then parsed once more into lsp-server's type on the
    /// calling thread. Incoming ones are polled from the channel directly: natively, a watcher
    /// thread (spawned on the first poll that finds nothing) wakes the stream; in a browser it
    /// polls every 10 ms, so there the server end must be non-blocking, driven by the host
    /// through `try_recv`, since every blocking lsp-server call spins or panics. Other wasm
    /// targets have no waker: unsupported at runtime. The server dropping its end stops the
    /// client with [`Reason::Closed`](super::Reason::Closed); nothing restarts it.
    pub fn memory(self, connection: lsp_server::Connection) -> (Client, Events) {
        let trace = self.trace;
        let (session, initialize) = self.session(None);
        let lsp_server::Connection { sender, receiver } = connection;
        let (local, queued) = futures_channel::mpsc::unbounded();
        let id = Id::next();
        let inbound = transport::Inbound::Memory(transport::memory::Inbox::new(receiver));
        let events = Events::new(id, queued, inbound);
        let link = transport::Link::Memory(transport::memory::Link::new(sender));
        let mut client = Client::new(id, session, link, local, trace);
        client.send(vec![initialize], None);
        (client, events)
    }

    /// The session this builder describes, and its `initialize` request.
    pub(crate) fn session(self, process_id: Option<u32>) -> (Session, message::Message) {
        let root = self.root.as_ref().map(uri::Key::new);
        let initialize = initialize_params(root.as_ref(), process_id, self.initialization_options);
        Session::new(initialize, self.configuration)
    }
}

// `rootUri` and `rootPath` are deprecated in favour of `workspaceFolders`, but servers still read
// them (pyright reads `rootPath`), so all three are sent.
#[allow(deprecated)]
fn initialize_params(
    root: Option<&uri::Key>,
    process_id: Option<u32>,
    initialization_options: Option<Value>,
) -> InitializeParams {
    InitializeParams {
        process_id,
        root_uri: root.map(|root| root.uri().clone()),
        root_path: root.and_then(uri::Key::file_path),
        initialization_options,
        capabilities: session::capabilities::client(),
        workspace_folders: root.map(|root| vec![folder(root)]),
        client_info: Some(ClientInfo {
            name: "scrive-lsp".to_owned(),
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        }),
        ..InitializeParams::default()
    }
}

/// The workspace folder for `root`, named after its last path segment.
fn folder(root: &uri::Key) -> WorkspaceFolder {
    WorkspaceFolder {
        uri: root.uri().clone(),
        name: root.name(),
    }
}
