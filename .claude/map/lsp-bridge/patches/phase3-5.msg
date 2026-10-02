fix(iced): reset widget state when the rendered document changes

iced keeps widget state by tree position, so a host that swapped documents
in one place (a tab switch) handed the new text the old one's scroll, drags
and hover. The widget now starts its view state over when the document id
changes, keeping focus and measured metrics, and reveals the new caret.
