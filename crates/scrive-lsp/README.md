# scrive-lsp

A Language Server Protocol client for the scrive editor that does no I/O.

The host owns the transport (a child process's stdio, a WebSocket, a web worker…) and passes each
incoming JSON-RPC message to the client. The client returns the messages to send back and the
updates to apply to scrive-core documents. It keeps no clock, spawns no threads and builds for
wasm32.

- `client` — `Client`, one connection to one language server.
- `update` — what the client hands back for documents (`Update`).
- `uri` — normalized document URIs (`uri::Key`).
- `message` — a tolerant JSON-RPC 2.0 envelope (`Message`), serde-ready.
- `encoding` — the negotiated position encoding and conversions between LSP positions and scrive
  byte offsets.

Payload types come from `lsp-types`, re-exported as `scrive_lsp::lsp_types`.

A host builds one `Client` per server with `Client::builder()` and sends the `initialize` request
that `build()` returns. It registers each document with `open`, and after every round of edits
calls `sync` with the document's snapshot and drained change log; the client sends ranged edits
when the log chains from what the server has, and the whole text otherwise. Every message from the
server goes to `receive`, which answers server requests itself and returns an `Output`: messages
for the transport, plus updates such as version-gated diagnostics (`Update::Document`) and
notifications it does not consume (`Update::Notification`). `close` and `shutdown` end a
document's and the connection's lifetime.
