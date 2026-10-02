feat(lsp)!: answer inlay hint tooltips, label jumps and inserts

Tooltips resolve on demand and fall back to the hover at a label part's
location; links jump through the definition path, and text edits insert the
hint.
