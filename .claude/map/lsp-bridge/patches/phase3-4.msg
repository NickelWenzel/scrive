feat(iced)!: F2 rename field

Renaming needs a new name from the user, and a host had no place to ask
for one. An opt-in field, seeded with the symbol under the caret, records
a ticketed rename request on Enter. It shares the find bar's chords and
focus plumbing, so Escape closes whichever bar is open and at most one is.
`Event::Focused` names its input by `Field` now that there are three, and
`Action` gains `Rename`.
