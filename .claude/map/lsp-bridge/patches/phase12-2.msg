feat(iced): CodeEditor::save_lsp

A save must reach the server after the edits it covers, or the server checks text it was never
sent. One call that syncs, then saves, keeps that order for every host.
