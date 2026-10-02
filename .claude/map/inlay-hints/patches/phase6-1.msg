refactor(lsp): resolve definition targets from a snapshot and recorded revisions

Label-part jumps resolve targets from a stored inlay set, not from a pending
request, so the lookup takes the snapshot and revisions it needs directly.
