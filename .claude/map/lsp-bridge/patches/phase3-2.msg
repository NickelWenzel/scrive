feat(iced): CodeEditor::select for host navigation

A host that learns where to jump (a definition, a search result it ran
itself) had no way to move the owned document's selection and reveal it.
`select` runs the core jump and the caret-move tail, so the popup and stale
requests close as they would for a click.
