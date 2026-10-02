docs: document the lsp feature and scrive-lsp

The bridge is a third crate and an opt-in feature, and neither README said
how a host wires it up or what it leaves to the host: the transport, files
that are not open, and several servers per editor.
