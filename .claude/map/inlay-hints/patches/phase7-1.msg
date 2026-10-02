feat(iced)!: sync_lsp sends inlay fetches and gestures and returns Applied

A hint's label jump is answered without a round trip, so the sync must hand the host its jump.
An inserted hint goes before its text lands, and the edit is synced in the same call.
