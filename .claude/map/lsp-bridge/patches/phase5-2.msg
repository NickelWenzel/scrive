feat(lsp): client handshake, document registry and lifecycle

A host needs one state machine per server that runs initialize and shutdown, keeps documents
registered under normalized URIs across the handshake, and answers the server's own requests so
it never blocks waiting on the client.
