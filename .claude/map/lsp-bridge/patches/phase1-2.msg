feat(core): Document::doc_id and a public select_and_reveal jump verb

A host jumping to a location (goto-definition) needs the same fold-aware
select-and-center that find and diagnostic navigation use, and a language
client needs the document's identity without reaching through the buffer.
