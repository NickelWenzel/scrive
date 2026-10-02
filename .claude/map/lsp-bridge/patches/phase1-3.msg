feat(core)!: per-commit change log that covers undo and redo

Incremental language-server sync needs every commit, undo and redo steps
included, with its pre-commit text and ops that replay in the rope's order.
The flat Vec<EditOp> log skipped undo/redo and replayed tied inserts backwards.
