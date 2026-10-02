feat(iced): paint inlay hints

The layout already reserves cells for hints; this draws each label there in
a dimmed text colour on a pill like the fold chip's, so it never reads as
buffer text. Padding stays editor background, as the LSP spec asks, and the
pill drops only where a selection strictly contains the hint and washes it.
