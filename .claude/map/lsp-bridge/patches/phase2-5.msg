feat(iced)!: Ctrl+Space opens completion at the caret

There was no manual invoke: Space always typed, so a host could not ask for
completions where no trigger char had fired. Adding the Action variant is a
breaking change to the exhaustive enum.
