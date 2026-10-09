//! Configures a [`Client`] and connects it through one bridge.

#[cfg(not(target_family = "wasm"))]
pub mod error;

#[cfg(not(target_family = "wasm"))]
use std::time::Duration;

use lsp_types::{ClientInfo, InitializeParams, Uri, WorkspaceFolder};
use serde_json::Value;

#[cfg(not(target_family = "wasm"))]
pub use error::Error;

use super::{Client, Events, Id};
use crate::session::{self, Session};
use crate::{message, trace, transport, uri};

/// How long a server gets to exit after `shutdown` and `exit`, unless the builder says.
#[cfg(not(target_family = "wasm"))]
const GRACE: Duration = Duration::from_secs(2);
/// Unwritten bytes past which a server counts as unresponsive, unless the builder says.
#[cfg(not(target_family = "wasm"))]
const BACKLOG: usize = 256 * 1024 * 1024;

/// Configures a [`Client`] and connects it through one bridge.
#[must_use]
#[derive(Debug, Default)]
pub struct Builder {
    root: Option<Uri>,
    initialization_options: Option<Value>,
    configuration: Option<Value>,
    trace: trace::Mode,
    #[cfg(not(target_family = "wasm"))]
    grace: Option<Duration>,
    #[cfg(not(target_family = "wasm"))]
    backlog: Option<usize>,
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

    /// How long the server gets to exit after `shutdown` and `exit` before it is killed, and how
    /// long its output may stay open after it exits. Defaults to 2 seconds.
    #[cfg(not(target_family = "wasm"))]
    pub fn shutdown_grace(mut self, grace: Duration) -> Self {
        self.grace = Some(grace);
        self
    }

    /// Unwritten bytes past which the server counts as unresponsive. Defaults to 256 MiB. For
    /// tests.
    #[doc(hidden)]
    #[cfg(not(target_family = "wasm"))]
    pub fn backlog_limit(mut self, bytes: usize) -> Self {
        self.backlog = Some(bytes);
        self
    }

    /// Starts `command` as the language server, and talks to it over its stdin and stdout. The
    /// client sends `initialize` at once, with this process's id as `processId`.
    ///
    /// The command's stdin, stdout and stderr are replaced by pipes. On Windows its creation
    /// flags are replaced by `CREATE_NO_WINDOW`, so a console server opens no window; std cannot
    /// read flags the caller set, so they are lost. Also on Windows, `Command::new("foo")` finds
    /// only `foo.exe`: a server installed as a `.cmd` shim needs its full path.
    ///
    /// Each line the server writes to stderr arrives as an [`Update::Log`](crate::Update::Log)
    /// from [`Source::Stderr`](crate::log::Source::Stderr), and output on stdout that is not LSP
    /// as one from [`Source::Stdout`](crate::log::Source::Stdout). A server that exits, or whose
    /// stdout closes, stops the client with [`Reason::Exited`](super::Reason::Exited) once its
    /// stderr is read to the end or the grace period passes. [`Client::shutdown`], or dropping
    /// the client, sends `shutdown` and `exit` and kills the process if it is still alive after
    /// the [grace period](Self::shutdown_grace).
    ///
    /// A write to a server that died fails rather than killing this process, because Rust
    /// programs ignore `SIGPIPE`; a host that is not a Rust program must ignore it too. If this
    /// process dies, nothing kills the server: it sees its stdin close, and has `processId`.
    ///
    /// # Errors
    /// [`Error::Spawn`] when the process does not start, [`Error::Thread`] when a thread the
    /// bridge needs cannot be created.
    #[cfg(not(target_family = "wasm"))]
    pub fn stdio(self, mut command: std::process::Command) -> Result<(Client, Events), Error> {
        let grace = self.grace.unwrap_or(GRACE);
        let limit = self.backlog.unwrap_or(BACKLOG);
        let trace = self.trace;
        let started = transport::stdio::spawn(&mut command, grace, limit)?;
        let (session, initialize) = self.session(Some(std::process::id()));
        let (local, queued) = futures_channel::mpsc::unbounded();
        let id = Id::next();
        let events = Events::new(id, queued, transport::Inbound::Channel(started.events));
        let link = transport::Link::Stdio(started.writer);
        let control = transport::Control::Stdio(started.control);
        let mut client = Client::new(id, session, link, control, local, trace);
        client.send(vec![initialize], None);
        Ok((client, events))
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
        let mut client = Client::new(id, session, link, transport::Control::Memory, local, trace);
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
