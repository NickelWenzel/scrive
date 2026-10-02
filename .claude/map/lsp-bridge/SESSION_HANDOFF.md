# Session handoff — lsp-bridge (initial, from /map on 2026-09-26)

## 1. Project

**scrive** is a code-editor widget for iced. It has two crates:

- `scrive-core`: the headless core — rope, transactions, undo, decorations, intel seams.
- `scrive-iced`: the `CodeEditor` widget.

This MAP adds **`scrive-lsp`**, a Language Server Protocol client written as a pure state machine. It
does no I/O and has no GUI dependencies. It sits behind an `lsp` feature on scrive-iced.

An iced app routes `Message::Lsp(lsp::Message)` (a JSON-RPC envelope) into the client. The editors
then pick up:

- diagnostics, completion, signature help and hover;
- goto-definition (F12), rename (F2, through a built-in field) and formatting (Shift+Alt+F);
- Ctrl+Space manual completion.

One client serves several documents. The app owns the transport. A two-tab example driven by a
scripted server shows all of it.

## 2. Where things stand

- **Branch** is `lsp_bridge` at HEAD `6cf4f2c` ("feat: enable wasm32 builds"). The tree is clean
  except for the untracked `.claude/`, which is not git-ignored in this repo.
- **Size:** about 35k lines of Rust across 2 crates, with 565 `#[test]`s.
- **Baseline:**
  - `cargo test --workspace` passes in about 10 s.
  - **`cargo clippy --workspace --all-targets -- -D warnings` fails at HEAD** on one new lint from
    clippy 0.1.98: `manual_slice_fill` at `crates/scrive-core/src/highlight.rs:1097`. Replacing it with
    `self.ret.win_states.fill(None);` makes the whole workspace clean (verified in a scratch copy).
    **Phase 1 Step 0 fixes it.**
- **No implementation work has started.** The plan has been through:
  - 6 critique rounds (the last one approved it);
  - a FOSS comparison (Helix, CodeMirror `@codemirror/lsp-client`);
  - 2 expert passes;
  - a consolidating rewrite.

## 3. Project management

- **Tracking is local only**, by choice: there are no GitHub milestones, issues or board. This file's
  status table is the source of truth.
- **The orchestrating session commits.** Phase agents never do. Commits go on `lsp_bridge`, as
  Conventional Commits (`fix(core)`, `feat(core)`, `feat(iced)`, `feat(lsp)`, `ci`, `docs`), with
  **no AI attribution**.
- `gh` resolves to the user's fork `NickelWenzel/scrive` (the `upstream` remote). `origin` is
  `robbym/scrive`.
- Design decisions a phase agent escalates go into `.claude/DECISIONS.md` in the D-entry format from
  `~/.claude/guides/DECISIONS.md`.

## 4. The plan

`MAP_PLAN.md` is authoritative. Each phase has a self-contained `MAP_PHASE_N.md`.

| # | Phase | Status |
|---|---|---|
| 1 | scrive-core: clippy baseline fix, per-commit change log, snapshot primitives, `select_and_reveal` | **done** (baddf21, 0801c3d, debed7d, 2d0835f) |
| 2 | Async seam: tickets, abandonment, sync parity, completion-item additions, Ctrl+Space | **done** (7ef466d..75c86d5, 5 commits) |
| 3 | Editor commands (F12/F2/Shift+Alt+F, rename field), call identity, widget `diff` reset, safe hover markdown | **done** (2c2c984..8891c67, 6 commits) |
| 4 | scrive-lsp crate: JSON-RPC envelope, position `Encoding`, CI headless gate | **done** (25efcf8..9a0243d, 4 commits) |
| 5 | Client core: multi-document sync (chain-checked incremental), diagnostics, lifecycle, URIs, server requests | **done** (f8ca140..4503f39, 4 commits) |
| 6 | Completion: pending machinery, sessions (reuse/continuation), snippet lowering | **done** (f31a186..482c5d8, 3 commits) |
| 7 | Signature help (call continuation) and hover | **done** (0ed83b5, d15f056) |
| 8 | Goto definition, rename, formatting (edit hygiene, capped line diff, WorkspaceEdit) | **done** (6a35e68..824fa24, 3 commits) |
| 9 | scrive-iced `lsp` feature + `CodeEditor` glue + CI `--all-features` | **done** (0a65ed3..b68ac86, 4 commits) |
| 10 | Example (`examples/lsp/`), READMEs, bump to 0.4.0 | **done** (80de6c2, 760b520, c454741) |

Phases run **strictly sequentially**.


## Status after the implementation session (2026-09-26)

**All 10 phases are implemented and committed** on `lsp_bridge`: 38 commits on top of `6cf4f2c`,
ending at `c454741 chore(release): 0.4.0`.

Final verification on the tip:

| Check | Result |
|---|---|
| `cargo test --workspace` | 860 passed |
| `cargo test --workspace --all-features` | 886 passed; the example's 5 tests pass |
| clippy `-D warnings` (default and all features) | clean |
| rustdoc `-D warnings` | clean |
| wasm32 build (all features, including the example) | clean |
| headless gate | clean |

Every commit was checked on its own (clippy + tests) in a throwaway worktree before the branch moved.

**Follow-up (2026-09-27):** a native-only `rust_analyzer` example drives a real rust-analyzer over
stdio: `8109af7` and `ae46e1d`. An `#[ignore]`d test checks it end to end with
`cargo test -p scrive-iced --features lsp --example rust_analyzer -- --ignored` (diagnostics arrive in
about 4 s). scrive-lsp doesn't advertise `window.workDoneProgress`, so rust-analyzer sends no
`$/progress`. Advertising it is a small follow-up if the status bar should show indexing progress.

Remaining for a human:
- **Visual check of the example.** It hasn't been run in a GUI. Launch it with
  `cargo run -p scrive-iced --features lsp --example lsp`, or on the web with
  `cd crates/scrive-iced && trunk serve --release --example lsp --features lsp`. Check:
  - the layout: tabs, a 3/5 editor and a 2/5 traffic panel;
  - squiggles at load;
  - F12 on `greet` switching tabs;
  - F2 renaming across both tabs;
  - Shift+Alt+F stripping trailing spaces;
  - completion showing `greet`, the snippet expanding, and the signature box opening;
  - the hover card;
  - Ctrl+F reaching only the active tab.
- **CI.** The wasip1 tests have never run locally, because wasmtime isn't installed, and the new CI
  steps haven't run on GitHub. Pushing `lsp_bridge` runs them.
- **Push.** The branch is local and hasn't been pushed. Push when ready. Don't tag it until the visual
  check passes.
- **Records.** Design calls made during implementation are D1-D3 in `.claude/DECISIONS.md`. The
  commit tooling is `patches/commit-phase.sh`, and the dispatch rules are in `DISPATCH.md`.

## 5. What to build next (historical — Phase 1 is done): Phase 1

Read `MAP_PHASE_1.md`. In short:

1. **Step 0:** fix the clippy baseline in `highlight.rs`, as its own commit.
2. `Snapshot::{offset_to_point, point_to_offset, chunks, clip_offset}` and `Document::doc_id()`.
3. `Document::select_and_reveal(range)`. `step_find` and `next_diagnostic` share its private helper.
4. The per-commit change log:
   - a `ChangeLog` field owned by one `record(before, &[EditOp])` method;
   - entries `document::Change { before: Snapshot, ops }`, with ops reversed;
   - undo and redo get logged too;
   - a 1024-entry cap that marks the log broken;
   - `drain_changes()` returns the opaque `document::Changes { doc_id, from, entries }`.
5. Update `CodeEditor::drain_changes` and its test.

The exit test is a per-entry replay property: replaying each entry's ops onto `before.text()` gives
the next text. It must hold across tied inserts and single- and multi-step undo/redo.

Dispatch with MAP_PLAN.md **Appendix A** + `MAP_PHASE_1.md`, after working through **"TODO before
dispatch"**.

## 6. Known bugs and tech debt (found during planning)

These are fixed by the plan:

- **Undo and redo are never logged.** `drain_changes` misses them (document.rs:1088-1151). *(Phase 1)*
- **Tied edits replay in the wrong order.** Ops with the same start come out of the log in the wrong
  order (`Reverse(start)` stable sort vs the rope's reverse application). *(Phase 1)*
- **Late async replies still land.** Results for requests the user abandoned are accepted if the
  revision is unchanged. So are results arriving for a different document at the same revision.
  *(Phase 2)*
- **Escape doesn't stick in async mode.** A later reply reopens the popup, and there is no local
  refilter. *(Phase 2)*
- **Signature help drops out.** Typing right after `(` in async mode loses it. *(Phase 2)*
- **Retrigger does nothing in async mode.** It only calls sync providers. *(Phase 2)*
- **Ctrl+Space types a space.** There is no manual invoke. *(Phase 2)*
- **Accepting an unparseable snippet inserts the raw `${…}` body.** *(mitigated by Phase 6 lowering)*
- **The hover markdown parser has no escapes.** It toggles bold and code on any `**` or backtick.
  *(Phase 3)*
- **Widget state leaks between documents.** It has no `diff` override, so swapping documents keeps
  stale scroll and reveal state. *(Phase 3)*
- **CI never builds features.** It has no `--all-features` run. *(Phase 9)*
- **The claimed `cargo tree` GUI gate isn't there.** `scrive-core/Cargo.toml` claims it, but CI has
  none. *(Phase 4)*

These are *not* fixed and are out of scope:

- The stale scrive-core README.
- The "iced 0.14" crate descriptions.
- Global find chords reach every subscribed editor.

## 7. Key files to read first

1. `.claude/map/lsp-bridge/MAP_PLAN.md`: the plan, design decisions D1-D21, constraints and risks.
2. `.claude/map/lsp-bridge/MAP_PHASE_1.md`: the next phase.
3. `~/.claude/guides/RUST_STYLE.md` and `~/.claude/guides/OPAQUE.md`: the style rules.
4. `~/.claude/skills/iced/SKILL.md`: iced conventions, which govern Phases 3, 9 and 10.
5. `crates/scrive-core/src/document.rs`: the change log, undo/redo and the jump verbs.
6. `crates/scrive-core/src/buffer.rs`: `Snapshot`, `Revision` and `DocId`.
7. `crates/scrive-iced/src/code_editor.rs`: `CodeEditor`, the async seam, the find bar and the accept
   path.
8. `crates/scrive-iced/src/editor.rs`: widget `State`, key handling, hover arming and markdown runs.
9. `crates/scrive-core/src/intel/*.rs`: the completion, signature, hover and snippet seams.
10. `.github/workflows/ci.yml`: the CI jobs `test`, `wasm` and `lints`.

## 8. Running commands

```
cargo test --workspace                                                # ~10 s
cargo clippy --workspace --all-targets -- -D warnings                 # fails at HEAD until Phase 1 Step 0
cargo clippy --workspace --all-targets --all-features -- -D warnings  # meaningful from Phase 9
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --all-features
cargo build --workspace --all-targets --all-features --target wasm32-unknown-unknown
cargo run -p scrive-iced --example minimal                           # GUI — human only
cargo run -p scrive-iced --example lsp --features lsp                # from Phase 10 — human only
cd crates/scrive-iced && trunk serve --release --example lsp --features lsp   # web, from Phase 10
```

- **Never run `cargo fmt`.** The code isn't rustfmt-clean. Use `rustfmt --edition 2021 <new file>`
  only on files a phase creates.
- **wasmtime is not installed locally**, so the wasip1 test job runs in CI only.

## 9. End-of-session checklist

1. Run the `verify` and `lint` lists from `goon.yaml`.
2. Review each phase diff against its doc's exit criteria and "What NOT to change". Commit per phase,
   plus the separate Step 0 fix commit.
3. Update the status table above and note any drift in cited line numbers.
4. Log escalated decisions in `.claude/DECISIONS.md`.
5. Write the next handoff with `/handoff`.
6. Push `lsp_bridge` only if the user asks.

## 10. Style guide, key points

- **Language and layout:** edition 2021; no `mod.rs`; no aliased imports except the
  `pub use scrive_lsp as lsp` facade.
- **Naming and types:**
  - no composite names on new scrive-lsp types (use `client::Builder`, not `ClientBuilder`);
  - types that carry invariants get private fields and accessors;
  - no raw `bool` flag parameters (builder toggles are fine);
  - `#[must_use]` on anything that returns messages.
- **Correctness:** no `unwrap()` in library code; errors use `thiserror`.
- **scrive-lsp is headless:** no `std::time`, threads, I/O or `std::process::id()` (wasm32).
- **Comments** explain *why* (about 25% density). Every `#[allow]` is justified. No plan narration in
  code.
- **Tests:** colocated, with sentence-style names, a `///` invariant doc, and string assert messages.
