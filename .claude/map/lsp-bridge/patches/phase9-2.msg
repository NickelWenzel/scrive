feat(iced): lsp feature re-exporting scrive-lsp

A host that drives a language server names one crate. The feature is off by
default, so hosts without a server compile none of it, and CI now lints,
tests, documents and builds wasm with every feature on.
