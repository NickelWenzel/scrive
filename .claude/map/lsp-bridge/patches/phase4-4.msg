ci: gate headless crates against GUI dependencies and test scrive-lsp on wasip1

scrive-core's manifest promised a cargo-tree gate that CI never ran; it
now covers scrive-lsp too, and the LSP client's tests run on a 32-bit,
thread-free target like the core's.
