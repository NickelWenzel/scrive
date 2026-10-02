feat(lsp): negotiated LSP position encodings

LSP counts a position's character in utf-8, utf-16 or utf-32 units while
scrive counts bytes, so every position crossing the bridge needs converting,
against rope snapshots and against plain disk text alike.
