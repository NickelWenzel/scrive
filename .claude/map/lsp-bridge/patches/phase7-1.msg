feat(lsp): signature help that follows the call while typing

Typing a call's arguments asks for signature help on every keystroke; superseding each request
would cancel the server's work before it answers. Keeping one request per call in flight, and
asking again only once its reply shows the caret moved, keeps the box current at one request per
reply.
