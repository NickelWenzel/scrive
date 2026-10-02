feat(lsp): hover with markdown lowered to the editor's subset

The hover card renders only bold, inline code and escapes, while servers send full markdown or
plain text full of `*` and backticks. Lowering fences, links and headings, and escaping everything
literal, keeps a server's card from switching the rest of a line to bold or code.
