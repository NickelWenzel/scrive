feat(iced): apply completion extra edits and follow-up requests on accept

Accepting an item applied only its main text, placed snippet stops by the
pre-edit offset (wrong once an import above moves it), and retriggered only
synchronous providers. The accept is one batch with the extra edits, reads
its base from the patch, and asks async hosts for the follow-ups too.
