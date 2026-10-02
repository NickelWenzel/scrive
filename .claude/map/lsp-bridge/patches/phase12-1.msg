feat(lsp): didSave notifications

rust-analyzer runs its `cargo check` diagnostics only on didSave, so without it a fixed error
stays reported. The client now advertises didSave and sends it for the synced snapshot.
