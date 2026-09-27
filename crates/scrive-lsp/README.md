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
- **The lifecycle**: `initialize`, `initialized`, `shutdown`, `exit`. Opens made before the
  handshake are sent once it completes.
- **Server requests** (`workspace/configuration`, `client/registerCapability`,
  `workspace/applyEdit`, …) answered by the client itself with safe defaults.

## With scrive-iced

Enable `scrive-iced`'s `lsp` feature: it re-exports this crate as `scrive_iced::lsp` and gives
`CodeEditor` six methods.

- `open_lsp` registers the editor's document with a client.
- `sync_lsp`, called after every `update`, mirrors edits and sends the requests the editor
  recorded.
- `apply_lsp` applies one `Update::Document` to the editor it is for.
- `save_lsp`, called after writing the document to disk, syncs and sends `didSave`.
- `jump` selects a definition that another editor's `apply_lsp` returned.
- `close_lsp` unregisters the document.

Each returns the messages to send. `crates/scrive-iced/examples/lsp` wires two tabs to one client
against a scripted server:

```bash
cargo run -p scrive-iced --features lsp --example lsp
cd crates/scrive-iced && trunk serve --release --example lsp --features lsp
```

## Without scrive-iced

A host builds one `Client` per server with `Client::builder()` and sends the `initialize` request
that `build()` returns. It registers each document with `open`, turns on the document's change log
(`Document::observe_changes(true)`), and after every round of edits calls
`Client::sync(&Snapshot, Changes)` with the document's snapshot and drained log. The client sends
ranged edits when the log chains from what the server has, and the whole text otherwise.

Requests take the request types from `scrive_core::intel` (`CompletionRequest`,
`SignatureRequest`, `HoverRequest`, `DefinitionRequest`, `RenameRequest`, `FormatRequest`). Every
message from the server goes to `receive`, which returns an `Output`: messages for the transport,
plus updates.

- `Update::Document` is bound for one open document. `update::Document::into_parts()` yields its
  document id, its stamp (a request ticket or a document revision) and the change. Checking the
  stamp against the document before applying the change is then the host's job.
- `Update::FileEdits` is a rename's edits for a file that is not open: `FileEdits::apply(&text)`
  returns the edited file.
- A definition in a file that is not open arrives as `update::jump::Unopened`:
  `Unopened::span(&text)` finds it in the file's text once the host has read it.
- `Update::Notification` is a server notification the client does not consume.

`close` and `shutdown` end a document's and the connection's lifetime.

## What it does not do

It has no transport and does no I/O, and it keeps no clock and spawns no threads. It does not
cover references, code actions, semantic tokens, inlay hints, `prepareRename`, range and on-type
formatting, or file operations, and an editor talks to one server.
