feat(lsp): document formatting with minimal, well-ordered edits

Formatters answer with CRLF text, whole-document replacements and edits in any order. Applied
verbatim, that moves every caret and squiggle in the file and can split a UTF-8 sequence, so the
reply is normalized to LF, line-diffed, trimmed on char boundaries and sorted before it lands.
