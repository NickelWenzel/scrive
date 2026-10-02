feat(lsp)!: forward inlay hint refreshes to every open editor

rust-analyzer's first non-empty hints and its post-edit updates arrive through
refreshes; servers without refresh support get one after `initialized`.
