feat(iced): register and sync a CodeEditor with an LSP client

A host otherwise wires the change log, six request slots and the client's
local answers by hand, in an order that is easy to get wrong: sync before
dispatch, and restart the log only once the client has taken the document.
