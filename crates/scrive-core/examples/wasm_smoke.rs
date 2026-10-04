//! The tree-sitter backend's wasm smoke test: a `cdylib` that a JavaScript
//! host instantiates with no imports and calls.
//!
//! ```sh
//! eval "$(scripts/wasm-cflags.sh)"
//! cargo build -p scrive-core --example wasm_smoke --features tree-sitter --target wasm32-unknown-unknown
//! node scripts/wasm-smoke.mjs target/wasm32-unknown-unknown/debug/examples/wasm_smoke.wasm
//! ```

use scrive_core::{Document, EditOp, Rgba, SpanStyle, TokenTheme, TreeSitterDef};

/// Highlights a small Rust document with tree-sitter-rust through
/// [`Document`], edits it, highlights again, and returns the number of spans
/// on its rows: 0 means failure.
#[no_mangle]
pub extern "C" fn run() -> u32 {
    let Ok(def) = TreeSitterDef::new(tree_sitter_rust::LANGUAGE, tree_sitter_rust::HIGHLIGHTS_QUERY) else {
        return 0;
    };
    let red = SpanStyle { fg: Rgba { r: 0xff, g: 0, b: 0, a: 0xff }, bold: false, italic: false };
    let green = SpanStyle { fg: Rgba { r: 0, g: 0xff, b: 0, a: 0xff }, bold: false, italic: false };
    let theme = TokenTheme::builder().capture("keyword", red).capture("string", green).build();
    let Ok(mut doc) = Document::new("fn main() {\n    let s = \"hi\";\n}\n") else { return 0 };
    doc.set_syntax(def, theme);
    doc.tokenize_highlight(u32::MAX);
    if doc.edit(vec![EditOp::insert(0, "struct A;\n")]).is_err() {
        return 0;
    }
    while doc.highlight_frontier().is_some() {
        doc.tokenize_highlight(u32::MAX);
    }
    (0..doc.buffer().line_count()).filter_map(|row| doc.highlight_line_spans(row)).map(|s| s.len() as u32).sum()
}
