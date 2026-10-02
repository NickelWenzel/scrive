feat(lsp): version-gated diagnostics

A publish computed for text the server has since been sent past would land on the wrong spans,
so versioned publishes apply only at the last sent version; unversioned publishes for files that
are not open yet are kept until the file opens instead of being lost.
