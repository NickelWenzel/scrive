//! The tree-sitter backend's wasm smoke test: a `cdylib` that a JavaScript
//! host instantiates with no imports and calls.
//!
//! ```sh
//! eval "$(scripts/wasm-cflags.sh)"
//! cargo build -p scrive-core --example wasm_smoke --features tree-sitter --target wasm32-unknown-unknown
//! node scripts/wasm-smoke.mjs target/wasm32-unknown-unknown/debug/examples/wasm_smoke.wasm
//! ```

use scrive_core::TreeSitterDef;

/// Builds a [`TreeSitterDef`] from tree-sitter-rust, which loads the grammar
/// and compiles its highlights query. Returns 1 on success, 0 on failure.
#[no_mangle]
pub extern "C" fn run() -> u32 {
    u32::from(TreeSitterDef::new(tree_sitter_rust::LANGUAGE, tree_sitter_rust::HIGHLIGHTS_QUERY).is_ok())
}
