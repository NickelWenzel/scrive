feat(iced): CodeEditor::try_edit reports rejected edit batches

`edit` drops a rejected batch without saying so, which leaves a caller
applying server edits unable to tell an overlap from success.
