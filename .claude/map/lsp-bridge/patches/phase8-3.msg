feat(lsp): rename through all-or-nothing workspace edits

A rename spans documents the editor holds and files it doesn't, and applying part of one leaves
the code broken. The whole edit is refused when any touched document is not the text the server
edited, or when it asks for file operations the client does not perform.
