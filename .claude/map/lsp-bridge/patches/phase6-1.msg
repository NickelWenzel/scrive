feat(lsp): completion requests with supersession and cancellation

The editor records completion requests that a language server has to answer. Each document keeps
one request per kind in flight, so a new one cancels the old; replies the editor no longer awaits
are dropped silently, and a request the client cannot send, or whose reply fails or does not
decode, still settles the editor's slot with an empty list.
