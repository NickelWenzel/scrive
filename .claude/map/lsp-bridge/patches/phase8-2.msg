feat(lsp): goto definition across open and unopened documents

A definition can live in the requester, in another open document, or in a file nobody opened,
and each needs different handling by the host. A target in an open document that moved since the
request points at text the server never saw, so it is dropped rather than mis-selected.
