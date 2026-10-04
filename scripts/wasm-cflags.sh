#!/usr/bin/env bash
# Prints the CFLAGS line a wasm32-unknown-unknown build needs for tree-sitter
# grammar crates whose build.rs predates the tree-sitter CLI 0.26 template
# (tree-sitter-rust 0.24, a scrive-core dev-dependency): it points their C
# compiler at the libc headers tree-sitter-language ships. The headers live in
# the cargo registry, so the path is resolved per machine. CFLAGS can't carry
# a path with spaces, so neither can the cargo home.
#
#   eval "$(scripts/wasm-cflags.sh)"
set -euo pipefail

manifest=$(cargo metadata --format-version 1 \
  | grep -o '"manifest_path":"[^"]*/tree-sitter-language-[^"/]*/Cargo.toml"' \
  | head -n 1 \
  | sed 's/^"manifest_path":"//; s/"$//' || true)

if [ -z "$manifest" ]; then
  echo "tree-sitter-language is not in the dependency graph" >&2
  exit 1
fi

echo "export CFLAGS_wasm32_unknown_unknown=\"-isystem $(dirname "$manifest")/wasm/include\""
