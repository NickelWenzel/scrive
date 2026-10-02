feat(lsp): chain-checked incremental document sync

Sending ranged edits is only correct when the drained change log leads exactly from what the
server holds to the new snapshot; checking that chain lets undo runs and multi-commit drains go
out incrementally and falls back to the whole text everywhere else.
