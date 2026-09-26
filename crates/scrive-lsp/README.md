# scrive-lsp

A Language Server Protocol client for the scrive editor that does no I/O.

The host owns the transport (a child process's stdio, a WebSocket, a web worker…) and passes each
incoming JSON-RPC message to the client. The client returns the messages to send back and the
updates to apply to scrive-core documents. It keeps no clock, spawns no threads and builds for
wasm32.

- `message` — a tolerant JSON-RPC 2.0 envelope (`Message`), serde-ready.

Payload types come from `lsp-types`, re-exported as `scrive_lsp::lsp_types`.
