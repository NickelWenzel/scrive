refactor(core)!: keep FoldMap geometry crate-private

Only `Rows` builds layouts for callers outside the crate, so none can project
without the inlay hints; `display_cell` names its edge like `position` does.
