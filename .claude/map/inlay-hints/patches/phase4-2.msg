refactor(iced): measure caret, wash, squiggle and bracket-box extents in pure helpers

Each overlay computed its screen span inline in draw, so its edge choice
could only be checked by eye. As helpers they can be tested headlessly
against a document with hints.
