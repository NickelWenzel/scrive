feat(iced)!: goto-definition and format-document requests

A host backed by a language server had no way to learn that the user asked
for a definition or a reformat. F12 and Shift+Alt+F now record ticketed
requests; a definition in the same document lands through `set_definition`
and is dropped once the caret or the text has moved. Shift+Alt+F is matched
on the physical key because the logical character differs by layout.
`Action` gains two variants.
