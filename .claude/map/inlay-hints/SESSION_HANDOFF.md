# Session handoff — inlay-hints (initial, from /map on 2026-10-01)

## 1. Project

**scrive** is a code-editor widget for iced. Its LSP bridge (`scrive-lsp`, the `lsp` feature) is done
on `lsp_bridge` (0.4.0, untagged). This MAP adds **LSP inlay hints**:
- Hints render inline and push the rest of the line right, like `: i32` or `n:`.
- Each hint is anchored to the token it annotates and moves with edits.
- Hovering a hint shows a tooltip. Ctrl+click jumps through a linked label part. Double-click applies
  the hint's text edits.
- `CodeEditor::inlay_hints(bool)` and `set_inlay_hints(bool)` toggle hints; the examples bind Ctrl+I.

## 2. Where things stand

- **Branch** `lsp_bridge` at HEAD `8e72665`. The tree is clean except for the untracked `.claude/`.
- **Baseline** (measured on 2026-09-28): 901 tests pass with `--all-features`. Clippy (both feature
  sets), doc and the wasm all-features build are green. Re-run them before Phase 1.
- **No implementation has started.** The plan has been through:
  - 4 agent-critique rounds, approved;
  - a FOSS comparison against Zed, Helix and Lapce/Floem (`FOSS_NOTES.md` plus the plan's "FOSS
    comparison" section);
  - 2 expert rounds, approved;
  - 7 phase docs;
  - a cross-reference audit. `RESOLUTIONS.md` answers every open question the docs raised.
- **User decisions:** Ctrl+I toggles hints; box selection steps by character everywhere; labels are
  dimmed text on a pill.

## 3. Project management

- **Tracking is local only.** This file's status table is the source of truth.
- **The orchestrating session commits; phase agents never do.**
  - Agents write cumulative patches `patches/phase<N>-<k>.patch` with their `.msg` files.
  - The orchestrator runs `.claude/map/inlay-hints/patches/commit-phase.sh <N> <base> --all-features`,
    which verifies every commit in a throwaway worktree before advancing the branch.
  - Commits are Conventional Commits with no AI attribution.
- **Escalations:** design decisions a phase agent escalates go into `.claude/DECISIONS.md`.

## 4. The plan

`MAP_PLAN.md` is authoritative. Each phase also has a self-contained `MAP_PHASE_N.md`. Where a doc and
`RESOLUTIONS.md` disagree, `RESOLUTIONS.md` wins.

| # | Phase | Status |
|---|---|---|
| 1 | core: hint model, anchored inlay store, `set_inlays` | **done** (9a0dbb2, cd4ed02, fd9c94d; 935 tests) |
| 2 | core + iced: `Rows` view and `Edge` (no behaviour change) | **done** (91af091, b1051ab, 96df5f8; 939 tests) |
| 3 | core: hint-aware layout, `inlay_at`, `inlays()`, box selection | **done** (3359d35, d967433, 25f0e20; 962 tests) |
| 4 | iced: tab fix, painting | **done** (bbacb22..6442da1, 4 commits; 973 tests) |
| 5 | iced: gestures, `wake_after`, scheduler, toggle | **done** (6c2d120, d3a2415, a0bdca2; 1021 tests) |
| 6 | scrive-lsp: fetch, refresh, interactions, minimal `land` arms | **done** (b71e598..b2b7e3d, 4 commits; 1070 tests) |
| 7 | glue (`sync_lsp` → `Applied`), examples, docs | **done** (d3d47ba, f2ab3ce, 7fdfc80; 1081 tests, rust-analyzer ignored test passes) |

## 5. Next session

All 7 phases are implemented and committed (2026-10-01): 23 commits `9a0dbb2..7fdfc80` on
`lsp_bridge`, 1081 tests with all features, 1034 default, both clippy runs, doc and wasm green.
Remaining:
1. **Visual check by the user:** `cargo run -p scrive-iced --example lsp --features lsp` and
   `--example rust_analyzer` — pills and dimmed labels, a hint inside a selection (pill hidden), the
   tab fix in highlighted spans, hover tooltip, Ctrl+click jump, double-click insert, Ctrl+I toggle.
2. Push `lsp_bridge` and watch CI; then the 0.4.0 tag (shared with the LSP bridge).
