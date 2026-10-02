feat(core): anchor inlay hints to the tokens they annotate

Hints ride edits like diagnostics, but drop with their token, so a deleted
argument never leaves a stale label behind.
