fix(iced): safe hover markdown and diagnostics kept in async hover cards

The hover parser toggled bold and code on any `**` or backtick with no way
to escape them, so a message like `expected *mut T` restyled the card. The
grammar is now documented on `HoverInfo::markdown`, and `escape_markdown` is
the one way to put text in it verbatim. The card is built in one place, so
an async answer, `None` included, no longer drops the diagnostics under the
pointer.
