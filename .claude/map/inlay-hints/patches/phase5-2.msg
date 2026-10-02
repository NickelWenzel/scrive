feat(iced)!: hover, Ctrl+click and double-click on inlay hints

A hint's text is not in the buffer, so the gestures on its cells mean
something of their own: a tooltip for the part, a jump through its link, or
inserting the hint. A set the text has moved past takes no gesture, because
the host's tooltips, locations and edits describe text that is gone.
