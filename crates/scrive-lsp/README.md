# scrive-lsp

A Language Server Protocol client for the scrive editor. A `Client` owns its connection to one
language server, keeps the server's copy of each open document in sync with scrive-core
snapshots, and turns the server's replies into updates for the editors.

The builder's last call picks the transport:

| Call | Transport | Targets |
|---|---|---|
| `stdio(Command)` | spawns the server and talks over its stdin and stdout; stderr becomes log entries | native |
| `connect(address)` | TCP: dials a server that listens (`lsp_server::Connection::listen` on the other end) | native |
| `listen(SocketAddr)` | TCP: waits for the server to dial in (`lsp_server::Connection::connect`) | native |
| `websocket(&str)` | `ws://` and `wss://`, one JSON-RPC message per text frame; needs the `websocket` feature | native and browser |
| `memory(Connection)` | the client end of `lsp_server::Connection::memory()`, for in-process servers and tests | every target |

```rust
let (mut client, events) = scrive_lsp::Client::builder()
    .root(scrive_lsp::uri::from_path(&workspace).expect("an absolute path"))
    .stdio(std::process::Command::new("rust-analyzer"))?;
// Run `events` (a futures `Stream`) and pass each item to `client.receive(event)`.
```

`memory` can't fail. The other four return `client::builder::Error`, whose variants depend on the
target and on the `websocket` feature: `Spawn`, `Thread` and `Bind` are native, `Url` comes with
`websocket`, and `Tls` with `websocket` natively. Cargo unifies features across a build, so any
crate in the tree that turns on `websocket` adds variants to your match. The enum is
`#[non_exhaustive]`, so the compiler enforces it: match the variants you handle and end with a
wildcard arm (or use `if let`).

The modules a host uses:

- `client`: `Client`, its `Builder`, `Events` and `Event`, `Status` and `Reason`, and `Error`.
- `update`: what `receive` hands back (`Update`), including document-bound changes.
- `log` and `trace`: the server's log lines, and an opt-in record of the traffic.
- `restart`: the restart `Policy`.
- `uri`: `uri::from_path`, and `uri::Key` for comparing URIs the way the client does.

`lsp_types` and `lsp_server` are re-exported, and so is `rustls` natively with `websocket`, so a
host and this crate agree on one version of each. The crate needs Rust 1.85 or newer
(lsp-server is edition 2024).

## Features

- `websocket` (off): the `Builder::websocket` terminal. Natively it pulls tungstenite, rustls
  with the ring provider, webpki-roots and mio: 24 crates an iced host doesn't already have. In
  the browser it uses web-sys's `WebSocket`, whose crates iced's web build already pulls in.
- In scrive-iced, `lsp` re-exports this crate as `scrive_iced::lsp`, and `lsp-websocket` turns
  on `lsp` plus this crate's `websocket`.

## What it covers

- **Diagnostics**, gated on the document version they were computed for. A publish for a file
  that is not open yet is cached and lands when it opens.
- **Saves**: `didSave` once the host has written the synced text to disk, with the text when the
  server asks for it. rust-analyzer re-runs `cargo check` on it.
- **Completion**, reusing a complete list while the user keeps typing the same word, and
  re-asking when the server marks the list incomplete.
- **Signature help** and **hover**.
- **Goto definition**, from a `Location`, a list of them, or a `LocationLink` list, in the same
  document, another open one, or a file that is not open.
- **Rename**, all or nothing across every file it touches: a stale edit for any open document
  rejects the whole rename.
- **Formatting**, diffed down to the lines that changed, so carets and decorations elsewhere stay
  put.
- **Inlay hints** for a byte span, refetched when the server asks; tooltips resolved on demand; a
  label part's location as a jump; a hint's text edits as an edit batch.
- **Server requests** (`workspace/configuration`, `client/registerCapability`,
  `workspace/applyEdit`, …) answered by the client itself with safe defaults.
- **Settings and custom notifications**: `Client::configure` replaces the settings and pushes
  them to the server, `Client::notify::<N>` sends a notification the client doesn't own, and any
  server notification the client doesn't consume arrives as `Update::Notification`.

## Events, status and logs

`Events` is a `futures` `Stream` of `client::Event`s. The host runs it (in iced,
`Task::run(events, Message::Lsp)`) and passes each event to `Client::receive`, which returns the
`Update`s it causes, in order. A client whose events nobody runs never sees a reply.

- `Client::status()` and `Update::Status` report `Starting`, `Running`, `Restarting { attempt }`
  and `Stopped(reason)`. `receive` returns a status change as its last update.
- Runtime problems (a message that isn't JSON-RPC, a payload that doesn't decode, a server error
  reply, a failed restart attempt) arrive as `Update::Error`.
- Log lines arrive as `Update::Log`: `window/logMessage` and `window/showMessage`, the server's
  stderr lines and the non-LSP text on its stdout (stdio), and the text on a socket that isn't an
  LSP message, plus the socket's own notes such as a WebSocket close code. `log::Entry::source()`
  says which (`Server`, `Stderr`, `Stdout`, `Socket`), `level()` how severe it is, and
  `is_shown()` marks `window/showMessage`.
- `Builder::trace` or `Client::set_trace` adds an `Update::Trace` for every message in both
  directions.
- **Progress**: rust-analyzer sends thousands of `$/progress` notifications while it indexes, and
  each is an `Update::Notification` from its own `receive`. The client doesn't coalesce them. A
  host that shows progress should clear it on every `Update::Status`, since a lost or stopped
  server never ends the progress it started.

## The server's lifecycle

- **A lost server** (a crash, a closed connection, a server that stopped reading) settles every
  request in flight with its empty answer at once and clears what the server published. The
  client then brings it back under `Builder::restart`: stdio spawns it again, `connect` and
  `websocket` dial again. The default `restart::Policy` gives up when a fifth loss falls within
  3 minutes; the waits before attempts start at 1 s and grow ×1.3 up to 10 s. A failed attempt
  counts as a loss, so a deleted binary or a dead host ends in `Stopped(GaveUp)` instead of
  retrying forever. The new server gets a fresh `initialize`, then every open document, and the
  editors stay attached throughout. A server that never completed its first `initialize` is not
  brought back. Memory and `listen` clients have nothing to restart: their server's end closing
  stops them with `Reason::Closed`.
- **`Client::restart()`** starts the server again at once, whether it is running or stopped,
  for example after `Stopped(GaveUp)` or a first connection that never came up. It settles the
  requests in flight and returns their documents, and the policy's count starts over. After
  `shutdown()` it does nothing. Memory and `listen` clients return `Err(Error::Unrestartable)`.
- **The `initialize` deadline** is off by default, since servers index before they answer.
  `Builder::initialize_timeout` sets one; a first server that misses it stops with
  `Reason::Timeout`, and later ones count as a loss.
- **`Client::shutdown()`** settles every request in flight and returns their documents for the
  editors. Over stdio, TCP and WebSocket the client then sends `shutdown`, waits up to
  `Builder::shutdown_grace` (2 s by default) for the reply, sends `exit`, and waits up to the
  grace again before it kills the process or closes the connection. A server that never answered
  `initialize` gets only `exit`. The memory transport sends `shutdown` and `exit` without
  waiting: the in-process server's end decides when it stops. `Status::Stopped(Reason::Shutdown)`
  follows through `Events` on every transport.
- **Dropping the client** sends the same goodbye. Over stdio, TCP and WebSocket the bridge's
  worker thread runs the sequence; if this process exits first, the server sees its stdin or
  socket close.
- **When `Events` ends**: after `Stopped(Shutdown)`, after a memory or `listen` connection
  closes or fails, or when the client is dropped. Every other stop (`GaveUp`, `Initialize`,
  `Exited` before the handshake, `Timeout`, a first connection that never came up) keeps the
  stream running, so `restart()` can bring the server back.

## Transports

- **stdio** replaces the `Command`'s stdin, stdout and stderr with pipes, and on Windows its
  creation flags with `CREATE_NO_WINDOW`. On Windows, `Command::new("foo")` finds only `foo.exe`;
  a server installed as a `.cmd` shim needs its full path. Output on stdout that isn't an LSP
  frame is skipped and logged. `processId` is this process's id. A server that stops reading
  while 256 MiB wait to be written to it is killed and stops with `Reason::Unresponsive`.
- **TCP** sets `TCP_NODELAY` and writes each message in one write. `connect` resolves and dials
  on a worker thread, so it returns at once. The first dials retry a server that is still
  starting until `Builder::connect_timeout` (10 s by default) runs out; then the client reports
  `Stopped(Failed)`, and `Client::restart()` dials again. `listen` binds before it returns
  (`Client::listening_on()` gives the port, also for port 0), accepts exactly one connection, and
  stops with `Reason::Closed` when that connection ends. Whoever connects first receives the text
  of every open document, so bind `127.0.0.1`. No TCP keepalive is set: a peer that vanishes
  without closing the connection goes unnoticed while nothing is sent. A server built on
  lsp-server writes each message in two writes without `TCP_NODELAY`, so its replies wait for
  delayed ACKs. On Windows, the reader thread of a lost connection stays parked until the peer
  closes its end, so restarting a server that is still alive but stuck leaves one idle thread
  per restart until that server goes away. The client never acts on anything it reads after the
  loss.
- **WebSocket** sends one bare JSON-RPC message per text frame, as vscode-ws-jsonrpc frames it.
  Natively it runs tungstenite on one I/O thread per connection; `wss://` uses rustls with the
  ring provider and the webpki roots, and `Builder::tls` takes a custom `rustls::ClientConfig`,
  for example one that trusts a self-signed `wss://localhost`. In the browser it uses the page's
  `WebSocket` and the browser's TLS, and has no hung-server guard. The first dial retries for
  `Builder::connect_timeout` as TCP's does, and a lost socket is dialed again under the restart
  policy. A message over 64 MiB arrives as `Error::Oversized` and drops the connection.
- **memory** takes the client end of `lsp_server::Connection::memory()`; the host drives the
  server end. Natively a watcher thread wakes `Events` when a message arrives. In a browser the
  client polls for replies every 10 ms, and the server end must not block: step it with
  `try_recv` from the host's update loop. Other wasm targets, such as wasm32-wasip1, have no way
  to wake `Events`, so memory doesn't work there at run time.
- `processId` is `null` on every transport but stdio.

## With scrive-iced

Enable `scrive-iced`'s `lsp` feature (and `lsp-websocket` for WebSocket): it re-exports this
crate as `scrive_iced::lsp` and gives `CodeEditor` six methods.

- `open_lsp` registers the editor's document with a client.
- `sync_lsp`, called after every `update`, mirrors edits and sends the requests the editor
  recorded, including the inlay hint fetches and the gestures on hints.
- `apply_lsp` applies one `Update::Document` to the editor it is for.
- `save_lsp`, called after writing the document to disk, syncs and sends `didSave`.
- `jump` selects a definition that another editor's `apply_lsp` returned.
- `close_lsp` unregisters the document.

The client sends what these produce. `sync_lsp`, `save_lsp` and `apply_lsp` return
`update::Applied`: a jump into another document for the host to route, and why the update was
refused, if it was. `jump` returns the same inside a `Result`. `crates/scrive-iced/examples/lsp`
wires two tabs to one client against a scripted in-process server, and
`examples/rust_analyzer` runs rust-analyzer over stdio:

```bash
cargo run -p scrive-iced --features lsp --example lsp
cd crates/scrive-iced && trunk serve --release --example lsp --features lsp
cargo run -p scrive-iced --features lsp --example rust_analyzer
```

## Without scrive-iced

A host builds one `Client` per server and runs its `Events`. It registers each document with
`open`, turns on the document's change log (`Document::observe_changes(true)`), and after every
round of edits calls `Client::sync(&Snapshot, Changes)` with the document's snapshot and drained
log. The client sends ranged edits when the log chains from what the server has, and the whole
text otherwise.

Requests take the request types from `scrive_core::intel` (`CompletionRequest`,
`SignatureRequest`, `HoverRequest`, `DefinitionRequest`, `RenameRequest`, `FormatRequest`).
`Client::inlays(&Snapshot, &inlay::Request)` fetches inlay hints and
`Client::interact(&Snapshot, &inlay::Interaction)` answers a gesture on one, with the request types
from `scrive_core::intel::inlay`. A request the client answers without the server (a decline, a
reused completion list) returns its `update::Document` at once. Every event from `Events` goes to
`receive`:

- `Update::Document` is bound for one open document. `update::Document::into_parts()` yields its
  document id, its stamp (a request ticket or a document revision) and the change. Checking the
  stamp against the document before applying the change is then the host's job.
- `Update::FileEdits` is a rename's edits for a file that is not open: `FileEdits::apply(&text)`
  returns the edited file.
- A definition in a file that is not open arrives as `update::jump::Unopened`:
  `Unopened::span(&text)` finds it in the file's text once the host has read it.
- `Change::InlayRefresh` asks the host to fetch the document's inlay hints again.
- `Update::Status`, `Update::Log`, `Update::Error`, `Update::Notification` and `Update::Trace`
  are described above.

`close` ends a document's lifetime and `shutdown` the server's.

## What it does not do

It does not cover references, code actions, semantic tokens, label-part commands,
`prepareRename`, range and on-type formatting, or file operations, and an editor talks to one
server. It sends no custom requests (only `notify` for custom notifications), has no per-request
timeouts, and has no WebSocket server side. Killing a server kills only that process, not its
children. If this process crashes, a stdio server notices only when its stdin closes.
