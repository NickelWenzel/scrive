feat(lsp): scrive-lsp crate with a tolerant JSON-RPC envelope

Real servers omit `jsonrpc`, send float ids, answer unreadable requests
with `"id": null` and sometimes send both `result` and `error`; the envelope
reads all of those while writing strict JSON-RPC 2.0 back.
