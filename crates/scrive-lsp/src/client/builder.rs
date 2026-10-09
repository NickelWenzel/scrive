//! Configures a [`Client`] and connects it through one bridge.

#[cfg(any(
    not(target_family = "wasm"),
    all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
))]
pub mod error;

#[cfg(not(target_family = "wasm"))]
use std::net::{SocketAddr, ToSocketAddrs};
#[cfg(all(feature = "websocket", not(target_family = "wasm")))]
use std::sync::Arc;
#[cfg(any(
    not(target_family = "wasm"),
    all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
))]
use std::time::Duration;

use lsp_types::{ClientInfo, InitializeParams, Uri, WorkspaceFolder};
use serde_json::Value;

#[cfg(any(
    not(target_family = "wasm"),
    all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
))]
pub use error::Error;

use super::{Client, Events, Id};
#[cfg(any(
    not(target_family = "wasm"),
    all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
))]
use crate::restart;
use crate::session::{self, Session};
use crate::{message, trace, transport, uri};

/// How long a server gets to exit after `shutdown` and `exit`, unless the builder says.
#[cfg(any(
    not(target_family = "wasm"),
    all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
))]
const GRACE: Duration = Duration::from_secs(2);
/// Unwritten bytes past which a server counts as unresponsive, unless the builder says.
#[cfg(not(target_family = "wasm"))]
const BACKLOG: usize = 256 * 1024 * 1024;
/// The first wait before a lost server is started again, unless the builder says.
#[cfg(any(
    not(target_family = "wasm"),
    all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
))]
const BACKOFF: Duration = Duration::from_secs(1);
/// How long the first dials of `connect` retry, unless the builder says.
#[cfg(any(
    not(target_family = "wasm"),
    all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
))]
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Configures a [`Client`] and connects it through one bridge.
#[must_use]
#[derive(Debug, Default)]
pub struct Builder {
    root: Option<Uri>,
    initialization_options: Option<Value>,
    configuration: Option<Value>,
    trace: trace::Mode,
    #[cfg(any(
        not(target_family = "wasm"),
        all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
    ))]
    grace: Option<Duration>,
    #[cfg(not(target_family = "wasm"))]
    backlog: Option<usize>,
    #[cfg(any(
        not(target_family = "wasm"),
        all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
    ))]
    restart: restart::Policy,
    #[cfg(any(
        not(target_family = "wasm"),
        all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
    ))]
    backoff: Option<Duration>,
    #[cfg(any(
        not(target_family = "wasm"),
        all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
    ))]
    initialize_timeout: Option<Duration>,
    #[cfg(any(
        not(target_family = "wasm"),
        all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
    ))]
    connect_timeout: Option<Duration>,
    #[cfg(all(feature = "websocket", not(target_family = "wasm")))]
    tls: Option<Arc<rustls::ClientConfig>>,
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

    /// How long the server gets to answer `shutdown`, and then to exit after `exit`, before it is
    /// killed; also how long its output may stay open after it exits. Defaults to 2 seconds.
    #[cfg(any(
        not(target_family = "wasm"),
        all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
    ))]
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

    /// When a lost server is started again; [`restart::Policy::default`] unless set.
    ///
    /// A server whose own child process still holds its output leaves one reader thread behind
    /// per restart, until that child exits.
    #[cfg(any(
        not(target_family = "wasm"),
        all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
    ))]
    pub fn restart(mut self, policy: restart::Policy) -> Self {
        self.restart = policy;
        self
    }

    /// How long the server may take to answer `initialize`; unset by default, because servers
    /// index before they answer. Running out counts as losing the server: before its first
    /// successful start the client stops with [`Reason::Timeout`](super::Reason::Timeout),
    /// later the [restart policy](Self::restart) decides. A reply that arrives just before the
    /// deadline can still lose the race against it.
    #[cfg(any(
        not(target_family = "wasm"),
        all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
    ))]
    pub fn initialize_timeout(mut self, timeout: Duration) -> Self {
        self.initialize_timeout = Some(timeout);
        self
    }

    /// The first wait before a lost server is started again, growing ×1.3 per attempt up to
    /// 10 s. Defaults to 1 second. For tests.
    #[doc(hidden)]
    #[cfg(any(
        not(target_family = "wasm"),
        all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
    ))]
    pub fn backoff(mut self, first: Duration) -> Self {
        self.backoff = Some(first);
        self
    }

    /// How long the first dials of `connect` and `websocket` retry a server that refuses or
    /// can't be reached, counted from the call; then the client stops with
    /// [`Reason::Failed`](super::Reason::Failed), and [`Client::restart`] dials again. Defaults
    /// to 10 seconds. A lost connection is dialed again under the [restart policy](Self::restart)
    /// instead.
    #[cfg(any(
        not(target_family = "wasm"),
        all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
    ))]
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = Some(timeout);
        self
    }

    /// The TLS configuration [`websocket`](Self::websocket) dials `wss://` with, for example one
    /// that trusts a self-signed certificate. Ignored for `ws://`. Without it, `wss://` trusts the
    /// webpki roots.
    #[cfg(all(feature = "websocket", not(target_family = "wasm")))]
    pub fn tls(mut self, config: Arc<rustls::ClientConfig>) -> Self {
        self.tls = Some(config);
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
    /// as one from [`Source::Stdout`](crate::log::Source::Stdout). A server that exits, whose
    /// stdout closes, or that stops reading is killed if need be, and lost once its output is
    /// read to the end or the grace period passes. The [restart policy](Self::restart) then
    /// decides whether it starts again, as [`Status::Restarting`](super::Status::Restarting);
    /// a server that never completed `initialize` is not restarted.
    /// [`Client::shutdown`], or dropping the client, runs the LSP shutdown handshake and kills
    /// the process if it is still alive after the [grace period](Self::shutdown_grace).
    ///
    /// A write to a server that died fails rather than killing this process, because Rust
    /// programs ignore `SIGPIPE`; a host that is not a Rust program must ignore it too. If this
    /// process dies, nothing kills the server: it sees its stdin close, and has `processId`.
    ///
    /// # Errors
    /// [`Error::Spawn`] when the process does not start, [`Error::Thread`] when a thread the
    /// bridge needs cannot be created.
    #[cfg(not(target_family = "wasm"))]
    pub fn stdio(self, command: std::process::Command) -> Result<(Client, Events), Error> {
        let started = transport::stdio::spawn(command, self.settings())?;
        Ok(self.start(started, Some(std::process::id())))
    }

    /// Connects to a language server listening on `address`: the client side of
    /// `lsp_server::Connection::listen`. The client sends `initialize` at once, queued until the
    /// connection is up, with `processId` `null`, since the server may run on another machine.
    ///
    /// Resolving and dialing happen on a worker thread, so this returns at once. Each dial tries
    /// every address `address` resolves to, for at most 5 seconds each; a server that is still
    /// starting up is dialed again until [`connect_timeout`](Self::connect_timeout). Text on the
    /// socket between messages arrives as an [`Update::Log`](crate::Update::Log) from
    /// [`Source::Socket`](crate::log::Source::Socket). A connection the server closes, or a
    /// server that stops reading, is lost, and the [restart policy](Self::restart) decides
    /// whether it is dialed again, as [`Status::Restarting`](super::Status::Restarting); a
    /// server that never completed `initialize` is not. [`Client::shutdown`], or dropping the
    /// client, runs the LSP shutdown handshake and closes the connection after the
    /// [grace period](Self::shutdown_grace).
    ///
    /// The connection sets no TCP keepalive, so a server whose machine vanishes without closing
    /// it goes unnoticed while nothing is sent; [`Client::restart`] dials again.
    ///
    /// # Errors
    /// [`Error::Thread`] when the worker thread cannot be created.
    #[cfg(not(target_family = "wasm"))]
    pub fn connect(
        self,
        address: impl ToSocketAddrs + Send + 'static,
    ) -> Result<(Client, Events), Error> {
        let budget = self.connect_timeout.unwrap_or(CONNECT_TIMEOUT);
        let started = transport::tcp::connect(address, budget, self.settings())?;
        Ok(self.start(started, None))
    }

    /// Connects to a language server behind a WebSocket at `url`, `ws://` or `wss://`, with one
    /// JSON-RPC message per text frame, as vscode-ws-jsonrpc frames it. The client sends
    /// `initialize` at once, queued until the connection is up, with `processId` `null`.
    ///
    /// Dialing and the handshakes happen on a worker thread, or in a browser on the page's event
    /// loop, so this returns at once; they retry until
    /// [`connect_timeout`](Self::connect_timeout) as `connect` does, and a lost connection is
    /// dialed again under the [restart policy](Self::restart). Natively `wss://` uses rustls with
    /// the ring provider and the webpki roots, or the `tls` configuration; a browser uses its own
    /// TLS, and there is no hung-server guard: the browser buffers what the server doesn't read. A binary frame is read as text when it is UTF-8; any other arrives as an
    /// [`Update::Log`](crate::Update::Log) from [`Source::Socket`](crate::log::Source::Socket)
    /// and is dropped, as does the server's close code. A message over 64 MiB arrives as
    /// [`Error::Oversized`](super::Error::Oversized) and loses the connection. No headers beyond
    /// the handshake's are sent, and no pings; the server's pings are answered.
    ///
    /// [`Client::shutdown`], or dropping the client, sends `shutdown` and `exit`, then the close
    /// frame, and lets go of the connection after the [grace period](Self::shutdown_grace).
    ///
    /// # Errors
    /// [`Error::Url`] for a URL that doesn't parse, has no host, or isn't `ws`/`wss`. Natively
    /// also `Error::Tls` when the default TLS configuration can't be built, and `Error::Thread`
    /// when the worker thread or the first connection's event poll can't be created.
    #[cfg(all(
        feature = "websocket",
        any(
            not(target_family = "wasm"),
            all(target_arch = "wasm32", target_os = "unknown")
        )
    ))]
    pub fn websocket(self, url: &str) -> Result<(Client, Events), Error> {
        #[cfg(not(target_family = "wasm"))]
        let endpoint = transport::websocket::Endpoint::new(url, self.tls.clone())?;
        #[cfg(all(feature = "websocket", target_arch = "wasm32", target_os = "unknown"))]
        let endpoint = transport::websocket::Endpoint::new(url)?;
        let budget = self.connect_timeout.unwrap_or(CONNECT_TIMEOUT);
        let started = transport::websocket::start(endpoint, budget, self.settings())?;
        Ok(self.start(started, None))
    }

    /// Waits for a language server to connect to `address`: the client side of
    /// `lsp_server::Connection::connect`. The client sends `initialize` at once, queued until the
    /// server connects, with `processId` `null`.
    ///
    /// The listener is bound before this returns, so [`Client::listening_on`] can tell the server
    /// where to dial, also when `address` has port 0. It accepts exactly one connection and then
    /// closes. When that connection ends, the client stops with
    /// [`Reason::Closed`](super::Reason::Closed) and its [`Events`] ends; it can't be restarted.
    /// Dropping the client or [`Client::shutdown`] before the server connected closes the
    /// listener. Otherwise it behaves like [`connect`](Self::connect), keepalive caveat
    /// included: no TCP keepalive is set.
    ///
    /// Whoever connects first receives the text of every open document. Bind a loopback address
    /// such as `127.0.0.1`, not `0.0.0.0`.
    ///
    /// # Errors
    /// [`Error::Bind`] when `address` can't be bound, [`Error::Thread`] when the worker thread
    /// cannot be created.
    #[cfg(not(target_family = "wasm"))]
    pub fn listen(self, address: SocketAddr) -> Result<(Client, Events), Error> {
        let (started, bound) = transport::tcp::listen(address, self.settings())?;
        let (mut client, events) = self.start(started, None);
        client.listening_on = Some(bound);
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
        #[cfg(any(
            not(target_family = "wasm"),
            all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
        ))]
        let recovery = super::recovery::State::new(self.restart, None);
        let (session, initialize) = self.session(None);
        let lsp_server::Connection { sender, receiver } = connection;
        let (local, queued) = futures_channel::mpsc::unbounded();
        let id = Id::next();
        let inbound = transport::Inbound::Memory(transport::memory::Inbox::new(receiver));
        let events = Events::new(id, queued, inbound);
        let link = transport::Link::Memory(transport::memory::Link::new(sender));
        let mut client = Client::new(
            id,
            session,
            link,
            transport::Control::Memory,
            local,
            trace,
            #[cfg(any(
                not(target_family = "wasm"),
                all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
            ))]
            recovery,
        );
        client.send(vec![initialize], None);
        (client, events)
    }

    /// What a worker bridge needs of this builder.
    #[cfg(any(
        not(target_family = "wasm"),
        all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
    ))]
    fn settings(&self) -> transport::Settings {
        transport::Settings {
            grace: self.grace.unwrap_or(GRACE),
            backoff: self.backoff.unwrap_or(BACKOFF),
            #[cfg(not(target_family = "wasm"))]
            limit: self.backlog.unwrap_or(BACKLOG),
            initialize_timeout: self.initialize_timeout,
        }
    }

    /// The client on a started worker bridge, with `initialize` sent.
    #[cfg(any(
        not(target_family = "wasm"),
        all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
    ))]
    fn start(self, started: transport::Started, process_id: Option<u32>) -> (Client, Events) {
        let trace = self.trace;
        let recovery = super::recovery::State::new(self.restart, self.initialize_timeout);
        let (session, initialize) = self.session(process_id);
        let (local, queued) = futures_channel::mpsc::unbounded();
        let id = Id::next();
        let transport::Started {
            link,
            control,
            inbound,
        } = started;
        let events = Events::new(id, queued, inbound);
        let mut client = Client::new(id, session, link, control, local, trace, recovery);
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
