feat(core): identify the enclosing call for signature requests

A signature request carried only the caret, so a client could not tell a
keystroke inside the same call from a move into another one. The request now
names the innermost open `(`, found on the bracket stack, which lets a client
keep one request in flight while the arguments are typed.
