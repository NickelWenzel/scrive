// Runs scrive-core's `wasm_smoke` example under node. Instantiating with an
// empty import object fails if any libc (or other) import leaked into the
// module, and `run` returns non-zero on success.
//
//   node scripts/wasm-smoke.mjs <path to wasm_smoke.wasm>
import { readFileSync } from "node:fs";

const path = process.argv[2];
if (!path) {
  console.error("usage: node scripts/wasm-smoke.mjs <wasm_smoke.wasm>");
  process.exit(2);
}

const module = new WebAssembly.Module(readFileSync(path));
const imports = WebAssembly.Module.imports(module);
if (imports.length > 0) {
  console.error("unexpected wasm imports:", imports);
  process.exit(1);
}
const { exports } = new WebAssembly.Instance(module, {});
const count = exports.run();
if (count === 0) {
  console.error("wasm smoke test failed: run() returned 0");
  process.exit(1);
}
console.log(`wasm smoke test passed: run() = ${count}`);
