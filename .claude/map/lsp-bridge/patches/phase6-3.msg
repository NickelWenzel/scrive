feat(lsp): re-issue completions the server dropped as content-modified

Servers answer ContentModified when an edit raced the request, although the text they have by then
matches the editor's; asking once more gets a list instead of an empty popup. Server-side
cancellations are treated like the client's own, so neither closes the popup.
