feat(examples): LSP example driven by a scripted server

A host needs one working reference for the whole loop: sync after every
editor update, route document updates to the tab that owns them, and hand a
cross-file jump to the target tab. An in-process server keeps it runnable on
wasm and testable headlessly.
