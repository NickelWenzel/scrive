feat(lsp): reuse and continue completion sessions while typing

Asking the server on every keystroke cancels requests faster than it can answer them. While the
user only extends the word at the caret, a complete list is refiltered locally and a request in
flight is adopted by the newer keystroke instead of being cancelled and sent again.
