feat(iced): drive a CodeEditor from an LSP client

Server answers arrive late and for any open document, so each one must be
checked against the editor it lands in before it applies. `apply_lsp` does
that check and reports refusals instead of dropping them, and `jump` hands a
definition in another document to that document's editor.
