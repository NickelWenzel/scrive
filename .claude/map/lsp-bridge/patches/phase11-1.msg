feat(examples): native LSP example against rust-analyzer

The scripted lsp example leaves the transport out. This one spawns a real
server over stdio, so hosts see the framing, the connect-and-queue order,
and the update routing working against rust-analyzer end to end.
