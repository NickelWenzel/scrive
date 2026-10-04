# Minimap: tree-sitter highlighting next to syntect

**Slug**: tree-sitter-highlighting
**Created**: 2026-10-04

## Checklist

- [x] Phase 1 — Capture styles in `TokenTheme`
- [x] Phase 2 — Split `highlight.rs` into modules (pure move)
- [x] Phase 3 — Crate-private `HighlightCache` facade and opaque `Grammar`
- [x] Phase 4 — `TreeSitterDef` and the whole-document tree-sitter highlighter
- [x] Phase 5 — Incremental tree-sitter cache wired into `Document`
- [x] Phase 6 — Budgeted, resumable tree-sitter parse
- [x] Phase 7 — `CodeEditor` accepts either grammar; tree-sitter example
- [x] Phase 8 — `syntect` and `tree-sitter` cargo features
- [x] Phase 9 — Benches and docs

> The orchestrator flips boxes as phases land and clear review. When
> driving via /dispatch-loop, edit this file alongside each per-commit
> task update.

## Goal

A scrive consumer picks the highlighting backend per document. The choices are a
syntect `.sublime-syntax` grammar (what exists today) or a tree-sitter grammar (a
grammar crate's `LANGUAGE` plus a `highlights.scm` query). Both backends produce
the same `HighlightSpan` rows, so the renderer, folding, inlay hints and the rest
of the editor don't change. A single `TokenTheme` colors both. Each backend sits
behind its own cargo feature: `syntect` (on by default) and `tree-sitter` (off
by default). A consumer who uses only tree-sitter can drop syntect from their
build entirely.

## Current state

- **Engine.** All highlighting lives in `crates/scrive-core/src/highlight.rs`
  (~1650 lines plus tests). It is syntect-only, and syntect types never cross
  the `pub` line (the module header and the Cargo.toml budget comment both say
  so).
  - Public types:
    - `SyntaxDef` (l.91), a single-grammar `SyntaxSet`.
    - `TokenTheme(Theme)` (l.139).
    - `Rgba`, `SpanStyle { fg, bold, italic }`, and `HighlightSpan { range: Range<u32> (line-relative bytes), style }`.
    - `Highlighter` (l.152), the whole-document oracle.
    - `HighlightCache` (l.448), re-exported at the root (lib.rs:80). Its public methods `new(.., n_lines)`, `on_commit`, `on_commit_patch`, `tokenize_until` and `first_dirty` are used only inside highlight.rs and `Document`.
    - The pool types `HighlightEngine`, `SegmentBoundary`, `SegmentStart` and `SegmentTokens`, plus `tokenize_segment` (l.1213–1330).
  - `HighlightCache` is a line-state convergence cache. It keeps:
    - a `DirtyRanges` list of rows to retokenize,
    - a `Checkpoints` SumTree with one state per 256 rows,
    - span and state storage only for the window (viewport ± 512 rows, at most 4096 rows).
  - Colors are resolved at tokenize time, so a theme swap retokenizes everything.
- **Document wiring** (`crates/scrive-core/src/document.rs`).
  - `Document` holds a concrete `highlight: Option<HighlightCache>` (l.48).
  - `rebase_views` (l.2611–2667) builds per-edit `(pre_start, old_lines, new_lines)` line splices from `committed.patch().edits()` and `committed.inverse_ops()`. They pair by index, because `transaction::apply` builds both in one loop (transaction.rs:213–222). It then debug-asserts them against `cache.line_count()` and calls `cache.on_commit_patch`. Edit, undo and redo all go through it.
  - `Document::edit` returns `Committed::from_patch` with an empty inverse (transaction.rs:84), so a `Committed` held outside the crate can't drive the splice.
  - Public methods: `set_syntax(SyntaxDef, TokenTheme)` (l.1403), `set_theme`, `tokenize_highlight(target)` (l.1882), `highlight_frontier`, `set_highlight_window`, `highlight_line_spans(row) -> Option<&[HighlightSpan]>`, `highlight_engine()` and `absorb_highlight`.
- **Buffer.** A custom rope with `u32` byte offsets and LF-only text (`Rope::chunk_at`, rope.rs:248). `Point { row, col }` uses byte columns (coords.rs:33), which is exactly tree-sitter's `Point` convention.
- **scrive-iced.**
  - `CodeEditor::language(SyntaxDef)` (`code_editor.rs:452`), `.theme(TokenTheme)` (l.464) and `load(source, Option<SyntaxDef>)` (l.633).
  - `uses_pool()` (l.1588) decides by buffer size alone (≥ 2 MiB, never on wasm). `seed_highlight` (l.1599) calls `tokenize_highlight` once for small documents and otherwise builds `HighlightPool`.
  - A per-frame `HighlightSweep` (l.1144) pumps while `highlight_frontier()` is `Some` or the pool is active (l.1551).
  - The renderer (`editor.rs` `draw_spans`, l.3723) reads only `fg`.
  - The default theme is `scrive_dark_theme()` (`scrive-iced/src/lib.rs:116`), built from `assets/scrive-dark.tmTheme`. Its selectors are all simple scope lists.
- **Features and CI.** scrive-core has no `[features]`, and syntect is unconditional. scrive-iced has only `lsp`, and scrive-lsp doesn't use `SyntaxDef` or `TokenTheme`. CI (`.github/workflows/ci.yml`) runs:
  - `cargo test`, clippy and docs, each both default and `--all-features`;
  - `cargo build --workspace --all-targets --all-features --target wasm32-unknown-unknown`;
  - core and lsp tests on `wasm32-wasip1`;
  - a `cargo tree` check that the headless crates pull in no GUI crate.
- **Crates.** tree-sitter appears nowhere in the repo. Latest crates:
  - `tree-sitter` 0.27.0, which supports wasm32-unknown-unknown through `DEP_TREE_SITTER_LANGUAGE_WASM_HEADERS` and re-exports `StreamingIterator`;
  - `tree-sitter-language` 0.1.8;
  - `tree-sitter-rust` 0.24.2. Its build.rs predates the current grammar template and doesn't add the wasm headers. Probe (2026-10-04, clang 18): with `CFLAGS_wasm32_unknown_unknown="-isystem <tree-sitter-language>/wasm/include"`, tree-sitter 0.27 plus tree-sitter-rust built as a wasm32-unknown-unknown cdylib, with **zero imports**. Under node it did a full parse, an incremental reparse and a highlights query (34,000 captures, no errors). Without the flag, `parser.c` fails on `stdlib.h`. The current CLI template (`crates/cli/src/templates/build.rs` on tree-sitter master) adds `DEP_TREE_SITTER_LANGUAGE_WASM_HEADERS` itself, so a grammar regenerated with it needs no flag.

## Target state

- **New types in scrive-core.**
  - `TreeSitterDef` (cfg `tree-sitter`), in `highlight/tree_sitter_def.rs`. Constructor: `TreeSitterDef::new(language: tree_sitter_language::LanguageFn, highlights: &str) -> Result<Self, TreeSitterError>`. It validates ABI compatibility and compiles the query, so an invalid pair can't be built (`smart-constructor-newtype`).
  - `TreeSitterError` is a `thiserror` enum: `IncompatibleVersion { version }`, `NotParseable`, and `Query { row, column, kind: QueryErrorKind, message }`. Here `QueryErrorKind` is scrive's own enum, mirrored from tree-sitter's (`structured-error`, and it keeps backend types out of the API).
  - `Grammar` is opaque: `pub struct Grammar(Inner)`, in `highlight/grammar.rs`. A private enum holds the feature-gated variants, and `From<SyntaxDef>` / `From<TreeSitterDef>` are the only ways in. Turning a feature on anywhere in the graph can't break a downstream `match` (feature additivity).
- **`TokenTheme`** (in `highlight/token_theme.rs`) is backend-neutral.
  - It holds ordered capture rules `name → SpanStyle`, resolved by longest dotted prefix: `function.method` falls back to `function`.
  - Under `syntect` it also holds the syntect `Theme`.
  - Constructors:
    - `TokenTheme::builder().capture(..).build()` works in every build and synthesizes a syntect `Theme` from a fixed vocabulary table.
    - `from_tm_theme` (cfg `syntect`) keeps the exact tmTheme and derives the capture rules from it.
- **`HighlightCache` is crate-private** (`pub(crate)`, no root re-export). It is a facade over private backends:
  - `highlight/line_state.rs`: the existing syntect cache, moved unchanged.
  - `highlight/parse_tree.rs`: new.
  - The facade takes `on_commit(&Buffer, &Committed)`, so `rebase_views` no longer knows the line-splice protocol.
- **Tree-sitter backend.** It owns a `Parser`, an optional `Tree`, a dirty-row list and window spans.
  - On commit: one `InputEdit` per edit, reparse pending, and the window rows are shifted and invalidated.
  - On `tokenize`:
    1. Advance the parse within a budget of progress-callback checks (each check is ≈100 parser ops). A cancelled parse resumes on the next call.
    2. When the parse completes, mark `changed_ranges` dirty.
    3. Query the dirty window rows with a rope `TextProvider`, within a row budget.
  - A theme swap re-queries the window without reparsing.
- **Document.** `set_syntax(grammar: impl Into<Grammar>, theme)` keeps its name, so existing `SyntaxDef` callers still compile. `highlight_engine()` returns `None` for a tree-sitter document.
- **scrive-iced.**
  - `.language(impl Into<Grammar>)`; `load(source, Option<Grammar>)` (breaking).
  - `uses_pool()` also requires an engine.
  - Features `syntect` (default) and `tree-sitter` forward to core.
  - `scrive_dark_theme()` works in every build.
  - New example `tree_sitter`, which also builds for wasm32-unknown-unknown.
- **Features and CI.** scrive-core `default = ["syntect"]`, `syntect = ["dep:syntect"]`, `tree-sitter = ["dep:tree-sitter", "dep:tree-sitter-language"]`; the workspace `scrive-core` dependency gets `default-features = false`. CI adds no-default-features and tree-sitter-only lint jobs, plus a no-syntect `cargo tree` check.

## Constraints

- **scrive-core imports no GUI crate.** Source: the `scrive-core/Cargo.toml` header and the `cargo tree` CI job.
- **Backend types stay out of the public API** (the scrive-core Cargo.toml budget comment). One exception is deliberate: `tree_sitter_language::LanguageFn` in `TreeSitterDef::new`. That crate is the stable, runtime-version-free handoff type that grammar crates export, and a consumer can't hand over a grammar any other way. `tree_sitter::{Query, QueryErrorKind, Tree, Node, Parser, LanguageError}` never appear in the API.
- **Dependencies are conservative** (`~/.claude/guides/RUST_STYLE.md` § Dependencies). New: `tree-sitter` and `tree-sitter-language`, both optional and justified in the Cargo.toml budget comment. Dev-only: `tree-sitter-rust`.
- **Module layout** (RUST_STYLE): no `mod.rs`; one primary type per module; `thiserror` errors; a doc comment on every public item (CI denies missing docs and rustdoc warnings); `#[non_exhaustive]` used sparingly, so `Grammar` is opaque instead.
- **Type invariants** (`~/.claude/guides/OPAQUE.md`):
  - N1: `TreeSitterDef::new` returns the validated type.
  - N2: no accessor leaks the `Query`, the `Language` or the `Grammar` internals.
  - N3: structured errors.
- **Edition and lints.** Workspace edition 2021 (unchanged). `cargo clippy --workspace --all-targets [--all-features] -D warnings`, extended in Phase 8 to the new feature sets.
- **wasm is a hard requirement** (user, 2026-10-04: no wasm32 support would be a showstopper). The `tree-sitter` feature, its tests' grammar and the `tree_sitter` example all build for `wasm32-unknown-unknown`, and the parse/query path runs there (Phase 4 adds a wasm smoke test). CI's exact all-features, all-targets wasm command is the check, not a narrower one. Budgets never use `std::time::Instant`, which panics there. `wasm32-wasip1` is not a target for the tree-sitter feature: tree-sitter's build.rs only shims libc for `wasm32-unknown*`, and the wasip1 job tests default features only.
- **Commits.** Conventional Commits, one commit per phase, `!` on API breaks (`commit-and-comment` skill). Comments say why, at the surrounding code's density.
- **Decisions.** Calls this plan doesn't settle go into `.claude/DECISIONS.md` as `D5`, `D6`, … (`~/.claude/guides/DECISIONS.md`). Phase 4 records D5 (capture precedence).
- **Performance.** The existing perf canaries and op-count tests in `highlight.rs` keep passing unmodified. The syntect path's behavior is unchanged.

## Out of scope

- **Injections and the locals query.** The user ruled them out for the first cut, and each needs a language registry.
- **Off-thread parsing for tree-sitter.** The `HighlightPool` and the segment API stay syntect-only. The budgeted parse (Phase 6) keeps big files from stalling a frame.
- **Folding, indentation, brackets or structural selection from the tree.**
- **Rendering bold, italic or backgrounds.** The renderer ignores them today.
- **LSP semantic tokens.**
- **Bundled grammars, or loading grammars from `.wasm` / shared libraries.**
- **The tree-sitter feature on `wasm32-wasip1`.** Building C for WASI needs a wasi-sdk sysroot. The browser target, `wasm32-unknown-unknown`, is the one that matters.
- **Reworking the syntect cache's model.** It is moved as-is.

## Phases

### Phase 1 — Capture styles in `TokenTheme`

**Goal**: A `TokenTheme` can be built from capture names in code, and it resolves any tree-sitter capture name to a `SpanStyle`. Syntect highlighting is unchanged.
**Files**: `crates/scrive-core/src/highlight.rs`, new `crates/scrive-core/src/highlight/token_theme.rs`, new `crates/scrive-core/src/highlight/vocabulary.rs`, `crates/scrive-core/src/lib.rs`.
**Steps**:
1. Move `TokenTheme` and `ThemeError` into `highlight/token_theme.rs`, re-exported from `highlight`. The root re-exports and the module-doc intra-doc links stay valid.
2. In `vocabulary.rs`, define a `const` table mapping each standard capture to its representative TextMate scopes:
   - Captures: attribute, comment, constant, constant.builtin, constructor, escape, function, function.builtin, function.macro, function.method, keyword, label, module, number, operator, property, punctuation, punctuation.bracket, punctuation.delimiter, string, string.escape, string.special, tag, type, type.builtin, variable, variable.builtin, variable.parameter.
   - Examples: `function` → `entity.name.function`, `support.function`; `type` → `entity.name.type`, `storage.type`, `support.type`.
3. `TokenTheme` gets private ordered capture rules plus a crate-private `resolve(&self, capture: &str) -> Option<SpanStyle>` that does a longest-dotted-prefix lookup. Captures named `none` or starting with `_` resolve to `None`.
4. `TokenTheme::builder()` with `.capture(name, SpanStyle)` and `.build()`. Under syntect, the build synthesizes a syntect `Theme`: one `ThemeItem` per rule, with selectors taken from the vocabulary. A rule outside the vocabulary applies to tree-sitter only, and the doc comment says so.
5. `from_tm_theme` keeps the parsed `Theme` untouched for syntect. For each vocabulary capture it derives a rule by resolving the capture's representative scopes with syntect's `Highlighter::style_for_stack`. Captures that resolve only to the theme's default foreground get no rule, so they fall through to plain text.
6. Unit tests:
   - prefix fallback;
   - `none` and `_private` names;
   - builder → syntect colors (the existing inline `GRAMMAR`, keyword colored);
   - an expected `Option<Rgba>` table for every vocabulary entry derived from scrive-dark's tmTheme content (copied into a test const). For example, `variable`, `property`, `module`, `tag`, `label` and plain `constant` are `None`.

**Exit criteria**: `cargo test -p scrive-core` and clippy pass. All existing highlight tests pass unmodified.

### Phase 2 — Split `highlight.rs` into modules (pure move)

**Goal**: The syntect cache lives in its own module, with zero signature or behavior changes. The diff is moves only.
**Files**: `crates/scrive-core/src/highlight.rs`, new `crates/scrive-core/src/highlight/line_state.rs`, new `crates/scrive-core/src/highlight/dirty_ranges.rs`, new `crates/scrive-core/src/highlight/splice.rs`, `crates/scrive-core/src/document.rs`.
**Steps**:
1. Move these into `highlight/line_state.rs` with their tests: `HighlightCache`, `Retention`, `Checkpoints`, `CkptItem`, `LineState`, `tokenize_line`, `StartState`, the segment and pool types, and `tokenize_segment`. Re-export the public items from `highlight` so every existing path (`scrive_core::HighlightCache`, `scrive_core::highlight::…`) still resolves.
2. Move `DirtyRanges` and its unit test to `highlight/dirty_ranges.rs` (`pub(crate)`). Phase 5 reuses it.
3. Move the per-edit splice builder out of `rebase_views` (document.rs l.2621–2662) into `highlight/splice.rs` as `pub(crate) fn line_splices(buffer: &Buffer, committed: &Committed) -> Vec<(u32, u32, u32)>`. The `debug_assert_eq!` against `cache.line_count()` stays at the call site.
4. Pure move: no renames, no signature changes, no logic edits.

**Exit criteria**: `cargo test --workspace` and `--all-features` pass with no test changed. `cargo bench --no-run -p scrive-core` builds. Clippy and docs are clean. `git diff --stat` is dominated by moves (`git diff -M` shows renamed blocks).

### Phase 3 — Crate-private `HighlightCache` facade and opaque `Grammar`

**Goal**: `Document` reaches highlighting through a crate-private facade that takes `(&Buffer, &Committed)`, and `set_syntax` accepts `impl Into<Grammar>`. This is a `refactor(core)!` commit, because `HighlightCache` leaves the public API.
**Files**: `crates/scrive-core/src/highlight.rs`, `crates/scrive-core/src/highlight/line_state.rs`, new `crates/scrive-core/src/highlight/grammar.rs`, `crates/scrive-core/src/document.rs`, `crates/scrive-core/src/lib.rs`.
**Steps**:
1. Rename the moved cache to `line_state::Cache`, keeping its internal API (`new(.., n_lines)`, `on_commit_patch`, `tokenize_until`, …), and point its tests at it.
2. Add `pub struct Grammar(Inner)` with a private `enum Inner { Syntect(SyntaxDef) }`, `From<SyntaxDef>`, and a doc comment naming the `From` impls as the way in. Re-export it at the root.
3. Add `pub(crate) struct HighlightCache { backend: Backend }`, with a private `enum Backend { Lines(line_state::Cache) }`. Its methods:
   - `new(Grammar, TokenTheme, &Buffer)`
   - `on_commit(&Buffer, &Committed)`: calls `splice::line_splices`, debug-asserts against its own `line_count()`, then delegates
   - `tokenize(&Buffer, target, max_lines) -> u32`
   - `pending`, `line_spans`, `set_window`, `window_aim`, `set_theme`, `line_count`
   - `engine() -> Option<HighlightEngine>`, `absorb(..) -> bool`
4. Drop `HighlightCache` from the lib.rs re-exports. Rewrite the module-doc intra-doc links (highlight.rs:14–27) that pointed at it so they point at `Document` methods, keeping `-D warnings` clean.
5. `Document::set_syntax(impl Into<Grammar>, TokenTheme)`. `rebase_views` calls `cache.on_commit(buffer, committed)`. `tokenize_highlight` calls `cache.tokenize(&self.buffer, ..)`.
6. Grep `crates/` (examples, benches, scrive-iced) for `HighlightCache` and update any use.

**Exit criteria**: `cargo test --workspace` and `--all-features` pass. Benches build. Clippy and docs are clean on both feature sets. `rebase_views` contains no line-splice code. The commit is marked `!`, and its body names the removed `HighlightCache` export.

### Phase 4 — `TreeSitterDef` and the whole-document tree-sitter highlighter

**Goal**: Behind the `tree-sitter` feature, a consumer can build a validated `TreeSitterDef`. A crate-private whole-document pass turns text into `Vec<Vec<HighlightSpan>>` with a defined capture precedence, and it is the oracle for Phase 5. `Grammar` doesn't accept a `TreeSitterDef` yet, so nothing can reach an unimplemented backend.
**Files**: `crates/scrive-core/Cargo.toml`, new `crates/scrive-core/src/highlight/tree_sitter_def.rs`, new `crates/scrive-core/src/highlight/capture_paint.rs`, `crates/scrive-core/src/highlight.rs`, `crates/scrive-core/src/lib.rs`, `.claude/DECISIONS.md`.
**Steps**:
1. Cargo setup:
   - `[features] tree-sitter = ["dep:tree-sitter", "dep:tree-sitter-language"]`, with `tree-sitter = { version = "0.27", optional = true }` and `tree-sitter-language = { version = "0.1", optional = true }`.
   - Dev-dependency `tree-sitter-rust = "0.24"`; its tests are gated `#[cfg(feature = "tree-sitter")]` only.
   - The `cc` header flag for wasm: a `.cargo/config.toml` entry can't hold it, because the header path lives in the cargo registry, so CI's wasm job resolves it: a step runs `cargo metadata --format-version 1` and gets `tree-sitter-language`'s `manifest_path`. It exports `CFLAGS_wasm32_unknown_unknown="-isystem $(dirname $manifest)/wasm/include"` to `$GITHUB_ENV`. Add a script `scripts/wasm-cflags.sh` that prints the line, so the same command works locally (`eval "$(scripts/wasm-cflags.sh)"`). Only grammars whose build.rs predates the current template need it. tree-sitter itself and scrive-core need nothing.
   - Extend the budget comment with why each crate is there and the `LanguageFn` exception.
2. `TreeSitterDef { language, query: Arc<Query> }` is `Clone`.
   - `new` probes `Parser::set_language`, mapping `LanguageError` to `IncompatibleVersion { version }` or `NotParseable`. It then runs `Query::new`, mapping the error to `Query { row, column, kind, message }`, with a scrive-owned `QueryErrorKind` that mirrors tree-sitter's variants.
   - Export `TreeSitterDef` at the root, plus `TreeSitterError` and `QueryErrorKind` via `highlight::`.
3. `capture_paint.rs`: `pub(crate) fn paint_rows(captures, rows: Range<u32>, row_starts, styles) -> Vec<Vec<HighlightSpan>>`. Precedence, recorded as **D5**:
   - a deeper (narrower) node overrides an enclosing one;
   - for two different nodes with the same range, the deeper node wins;
   - for the same node, the earlier pattern wins (tree-sitter-highlight semantics, which grammar crates' `HIGHLIGHTS_QUERY` assumes);
   - **deliberate deviation**: a capture whose name the theme doesn't style doesn't block a later pattern on the same node. With tree-sitter-rust and a theme lacking `constructor`, `Foo(..)` falls through to `@function` instead of going plain. D5 records why: a partial theme still colors as much as it can.

   Implementation: a boundary sweep over sorted capture intervals, not a per-byte array. Captures are clipped to `rows` (`set_byte_range` also yields matches whose nodes start far above the range, and other captures of an intersecting match). Multi-line captures are split at row boundaries into line-relative ranges. Adjacent spans with equal styles merge.
4. `highlight/tree_sitter_def.rs` (or a sibling `whole.rs`) gets the crate-private `fn highlight_whole(def, theme, text: &str) -> Vec<Vec<HighlightSpan>>`. It parses from scratch, runs a `QueryCursor` over `text.as_bytes()`, and emits a trailing empty row the way `Highlighter::highlight` does.
5. Tests (`tree-sitter-rust` with its `HIGHLIGHTS_QUERY`; small hand-written queries for precedence):
   - a keyword gets its color;
   - an escape inside a string overrides the string;
   - same node, earlier pattern wins;
   - an identical-range parent/child: the child wins;
   - an unstyled capture falls through (D5);
   - a block comment across rows splits per row;
   - `#eq?` / `#match?` filter;
   - a bad query gives `Query` with its position;
   - empty text.
6. With the flag from step 1, run the exact CI command, `cargo build --workspace --all-targets --all-features --target wasm32-unknown-unknown`, and update `ci.yml`'s wasm job to set the flag first.
7. Add a wasm smoke test that runs, not just builds: a CI step builds a tiny `cdylib` test crate (`crates/scrive-core/tests/wasm-smoke/`, not a workspace member, or an example with `crate-type = ["cdylib"]`). It exports `extern "C" fn run() -> u32`. In this phase `run` builds a `TreeSitterDef` from `tree_sitter_rust` (public API) and returns a non-zero code on success. Phase 5 switches it to `Document::set_syntax` + `tokenize_highlight` + `highlight_line_spans` and the span count, because `Grammar` only accepts a `TreeSitterDef` from Phase 5 on. A node script (`node` is on `ubuntu-latest`) instantiates it with an **empty import object**, asserting no libc imports leaked, and checks the count is non-zero. This mirrors the 2026-10-04 probe.
8. Document for consumers, in `TreeSitterDef`'s doc and the README wasm note: grammars generated with tree-sitter CLI ≥ 0.26's template build for wasm32 as-is. Older grammar crates need the `CFLAGS_wasm32_unknown_unknown` line.

**Exit criteria**: `cargo test -p scrive-core --features tree-sitter` passes, and the default `cargo test` is unaffected. The CI wasm command succeeds, and the node smoke test passes locally and in `ci.yml`. Clippy and docs are clean with `--all-features`. D5 is in `.claude/DECISIONS.md`.

### Phase 5 — Incremental tree-sitter cache wired into `Document`

**Goal**: `Document::set_syntax(tree_sitter_def, theme)` highlights incrementally. Edits, undo and redo reparse, and only changed rows are requeried. Results match `highlight_whole` after any edit sequence. In this phase the parse runs to completion on every call; Phase 6 adds the budget.
**Files**: new `crates/scrive-core/src/highlight/parse_tree.rs`, new `crates/scrive-core/src/highlight/rope_text.rs`, `crates/scrive-core/src/highlight.rs`, `crates/scrive-core/src/highlight/grammar.rs`, `crates/scrive-core/src/lib.rs`, `crates/scrive-core/src/document.rs` (tests).
**Steps**:
1. `Grammar`'s `Inner` gains `TreeSitter(TreeSitterDef)` (cfg), plus `From<TreeSitterDef>`.
2. `rope_text.rs`: a crate-private parse input callback and a `TextProvider` over a `Snapshot`, both built on `chunk_at` (rope.rs:248). Query predicates (`#eq?`, `#match?`) then read node text from the rope, with no whole-document `String`.
3. `parse_tree::Cache` holds:
   - `parser`, `def`, `styles: Vec<Option<SpanStyle>>` (by capture index)
   - `tree: Option<Tree>`, `reparse: bool`
   - `dirty: DirtyRanges`, `n_lines`, `aim` / `win`, `win_spans: Vec<Option<Vec<HighlightSpan>>>`

   It uses `padded_highlight_window` and `HIGHLIGHT_MAX_WINDOW_ROWS` like the line-state cache.
4. `on_commit(&Buffer, &Committed)`: for each `(edit, inverse)` in ascending order, apply an `InputEdit` in the tree's current coordinates (earlier edits already applied):
   - `start_byte = e.new.start`
   - `old_end_byte = e.new.start + e.old.len()`
   - `new_end_byte = e.new.end`
   - `start_position = buffer.offset_to_point(e.new.start)`; correct because the text before edit i is final
   - `old_end_position` = start position advanced over `inverse.text` (newline count, byte length after the last newline)
   - `new_end_position = buffer.offset_to_point(e.new.end)`

   Then set `reparse`, and shift `win_spans` and `dirty` through `splice::line_splices`: edited rows lose their spans, and unedited rows keep theirs, shifted. Put the coordinate convention in a doc comment.
5. `tokenize(&Buffer, target, max_lines)`:
   - If `reparse` is set, parse with the edited old tree. Mark `old.changed_ranges(&new)` rows dirty. A first parse has no old tree, so the whole window is dirty.
   - Then query the dirty rows that intersect the window, in contiguous runs: `QueryCursor::set_byte_range(run)` with the rope `TextProvider` → `capture_paint::paint_rows`, up to `max_lines` rows. Rows outside the window only lose their dirt, and nothing is stored for them.
6. `pending()` returns `Some` while a reparse is due or a dirty window row remains. `set_window` marks newly exposed window rows dirty. `set_theme` re-resolves `styles` and marks the window dirty with no reparse; old spans stay as a stale fallback, like `line_state`.
7. Facade: `Backend::Tree(parse_tree::Cache)` (cfg). `engine()` returns `None` and `absorb()` returns `false` for it.
8. Tests in `parse_tree.rs` (tree-sitter-rust; the cfg from Phase 4):
   - a fresh document equals the oracle;
   - an in-place edit reconverges;
   - inserting `/*` comments out the following rows, and deleting it reverts them;
   - randomized edits and window moves match the oracle (seeded, in the style of `randomized_edits_and_window_moves_match_the_oracle`);
   - a randomized multi-edit commit (multi-caret) matches the oracle;
   - a theme swap recolors with no reparse;
   - a keystroke requeries only rows near the edit (an op-count canary).
9. Document tests: undo and redo resync (mirror `undo_resyncs_brackets_and_highlight_to_the_reverted_text`), a multi-op transaction equals a fresh document, and `set_syntax` with a `TreeSitterDef` keeps the window aim.

10. Switch the wasm smoke test's `run` to the `Document` path: an edit, then the span count.

**Exit criteria**: The new tests pass under `--features tree-sitter`. The default suite is unchanged. Clippy and docs are clean on all features. The CI wasm command still builds, and the node smoke test passes with real spans.

### Phase 6 — Budgeted, resumable tree-sitter parse

**Goal**: The initial parse of a big file is spread across frames, and a keystroke reparse on a 10 MB document still finishes in one call.
**Files**: `crates/scrive-core/src/highlight/parse_tree.rs`, `crates/scrive-core/src/highlight.rs` (constant).
**Steps**:
1. Add `HIGHLIGHT_MAX_PARSE_CHECKS_PER_CALL`, a count of progress-callback invocations, each ≈100 parser ops (`OP_COUNT_PER_PARSER_CALLBACK_CHECK`). Start at a value that measures about 2 ms, and tune it in Phase 9. The budget doesn't use bytes, because `current_byte_offset` jumps on subtree reuse and stalls during balancing. It doesn't use wall-clock either, because `Instant` panics on wasm32-unknown-unknown.
2. `parse_with_options` takes a progress callback that counts calls and returns `ControlFlow::Break(())` at the budget. A `None` result means cancelled: the reparse stays pending, `pending()` stays `Some`, and the next `tokenize` resumes. The parser resumes by default when it's called again with the same old tree and input.
3. A commit while a parse is cancelled midway calls `parser.reset()`, so the next parse starts over against the newly edited old tree, or from scratch on a first parse.
4. While a first parse is pending, rows show no spans. During a later reparse, the shifted old spans stay visible for unedited rows.
5. Tests:
   - a generated multi-MB document takes several `tokenize` calls and then matches the oracle;
   - a commit in the middle of a pending first parse still converges to the oracle;
   - after a full parse, a one-byte edit in a 10 MB document reparses within one call;
   - `pending()` is `Some` until the parse and the window queries finish.

**Exit criteria**: The tests pass under `--features tree-sitter`. No test needs more calls than a bound the test asserts.

### Phase 7 — `CodeEditor` accepts either grammar; tree-sitter example

**Goal**: A scrive-iced host writes `.language(TreeSitterDef::new(tree_sitter_rust::LANGUAGE.into(), tree_sitter_rust::HIGHLIGHTS_QUERY)?)` and gets highlighting, including for documents of 2 MiB or more, which highlight sequentially through the frame pump instead of the pool.
**Files**: `crates/scrive-iced/Cargo.toml`, `crates/scrive-iced/src/code_editor.rs`, `crates/scrive-iced/src/lib.rs`, new `crates/scrive-iced/examples/tree_sitter.rs`, `crates/scrive-iced/examples/scratch.rs`, `crates/scrive-iced/examples/lsp/main.rs`, `crates/scrive-iced/examples/rust_analyzer.rs`.
**Steps**:
1. Add the scrive-iced feature `tree-sitter = ["scrive-core/tree-sitter"]` and the `tree-sitter-rust` dev-dependency. Add `[[example]] name = "tree_sitter"` with `required-features = ["tree-sitter"]`. It builds for wasm32-unknown-unknown like the other examples (CI's wasm job, with the Phase 4 flag).
2. `language(self, grammar: impl Into<Grammar>)`, and `load(&mut self, source, grammar: Option<Grammar>)` (breaking, `feat(iced)!`). Update every call site in the examples.
3. `uses_pool()` (code_editor.rs:1588) also requires `self.doc.highlight_engine().is_some()`. Otherwise a 2 MiB tree-sitter document goes down the pool path: `HighlightPool::new` returns `None`, nothing tokenizes, and the frames subscription spins forever. Check that `ViewportChanged` (l.927–934) and `HighlightSweep` (l.1144–1170) fall back to `tokenize_highlight` when `uses_pool()` is false.
4. `seed_highlight` calls `tokenize_highlight` once. With a budgeted parse that call may only start the parse, and `HighlightSweep` keeps pumping while `highlight_frontier()` is `Some`. Confirm that, and note it in the doc comment, which currently promises a highlighted first paint.
5. The `tree_sitter` example: the `minimal.rs` layout with `tree-sitter-rust` and the default theme.
6. Tests in `code_editor.rs` (the tree-sitter test cfg):
   - a tree-sitter language tokenizes at load without a viewport report;
   - a ≥ 2 MiB tree-sitter document doesn't start the pool and converges through repeated `HighlightSweep`;
   - `load(.., Some(def.into()))` swaps the backend.

**Exit criteria**: `cargo test --workspace --all-features` passes. `cargo run -p scrive-iced --example tree_sitter --features tree-sitter` builds. The CI wasm command passes.

### Phase 8 — `syntect` and `tree-sitter` cargo features

**Goal**: syntect becomes an optional default feature. `--no-default-features --features tree-sitter` builds a working editor with no syntect in the dependency tree, and `--no-default-features` alone builds an editor without highlighting.
**Files**: `Cargo.toml`, `crates/scrive-core/Cargo.toml`, `crates/scrive-lsp/Cargo.toml`, `crates/scrive-iced/Cargo.toml`, `crates/scrive-core/src/highlight.rs` and its submodules, `crates/scrive-core/src/document.rs`, `crates/scrive-core/src/lib.rs`, `crates/scrive-core/benches/support.rs`, `crates/scrive-iced/src/lib.rs`, `crates/scrive-iced/src/highlight_pool.rs`, `crates/scrive-iced/src/code_editor.rs`, `crates/scrive-iced/examples/*`, `.github/workflows/ci.yml`.
**Steps**:
1. Feature wiring:
   - scrive-core: `default = ["syntect"]`, `syntect = ["dep:syntect"]`.
   - The workspace `scrive-core` dependency gets `default-features = false`. Cargo ignores a member-level `default-features = false` when the workspace entry defaults to true.
   - scrive-lsp enables no highlight feature.
   - scrive-iced: `default = ["syntect"]`, `syntect = ["scrive-core/syntect"]`.
2. Gates in scrive-core:
   - `cfg(feature = "syntect")` on `SyntaxDef`, `SyntaxError`, `Highlighter`, `line_state` (with the pool types and `tokenize_segment`), `from_tm_theme` and the syntect half of `TokenTheme`, and `Inner::Syntect`.
   - `cfg(any(feature = "syntect", feature = "tree-sitter"))` on the shared code that would otherwise be dead and fail `-D warnings`: `dirty_ranges`, `splice`, `capture_paint` (tree-sitter only), `padded_highlight_window`, the window constants, and `HighlightCache` with the `Document` fields and methods that use it.
   - With neither feature, `Grammar`'s `Inner` is empty (uninhabited), so `set_syntax` can't be called. Keep `Document`'s highlight methods compiling and returning `None` / no-ops, so scrive-iced's code needs no cfg for them.
3. Gates elsewhere:
   - In scrive-iced, gate `HighlightPool` and the pool paths on `syntect`.
   - Gate scrive-iced lib tests that use `SyntaxDef` (code_editor.rs:2440 and others) on `syntect`.
   - Give the `perf` bench `required-features = ["syntect"]` (`benches/support.rs:5` uses `SyntaxDef`).
   - Give the examples that use `SyntaxDef` `required-features = ["syntect"]`.
4. Reword the regex comment in scrive-core's Cargo.toml so its "nearly free via syntect" rationale doesn't depend on syntect being present.
5. `scrive_dark_theme()`:
   - With `syntect`, it stays the tmTheme asset.
   - Without it, a const builder table in `scrive-iced/src/lib.rs` supplies the same colors.
   - A test under `syntect` asserts that the builder table equals the tmTheme-derived capture rules for every vocabulary entry, so the two can't drift.
6. CI (`ci.yml`):
   - In `lints`, add `cargo clippy -p scrive-core -p scrive-iced --all-targets --no-default-features -- -D warnings` and the same with `--features tree-sitter`.
   - Add `cargo test -p scrive-core --no-default-features --features tree-sitter` to `test`.
   - Extend the headless job: `cargo tree -p scrive-core --no-default-features --features tree-sitter -e normal` contains no `syntect`.

**Exit criteria**: These all pass locally:
- clippy `-D warnings` and the build for default, `--all-features`, `--no-default-features`, and `--no-default-features --features tree-sitter` (core and iced);
- tests for default, `--all-features`, and the tree-sitter-only core;
- the CI wasm command.

`ci.yml` contains the new steps.

### Phase 9 — Benches and docs

**Goal**: Tree-sitter performance and memory are measured and recorded, the parse budget is tuned, and the docs show how to pick a backend.
**Files**: `crates/scrive-core/benches/perf.rs`, `crates/scrive-core/benches/support.rs`, `crates/scrive-core/benches/LEDGER.md`, `crates/scrive-core/Cargo.toml`, `crates/scrive-core/src/highlight.rs`, `crates/scrive-core/src/lib.rs`, `README.md`, `crates/scrive-core/README.md`, `crates/scrive-iced/README.md`, `crates/scrive-iced/src/lib.rs`.
**Steps**:
1. Benches behind `tree-sitter`, either in a separate bench target with `required-features = ["tree-sitter"]` or as cfg'd groups in `perf`:
   - a keystroke followed by `tokenize_highlight(viewport.end)` on 1 MB of Rust;
   - a cold full parse of 1 MB and 10 MB (calls needed, time per call);
   - a window jump into a parsed document.

   Tune `HIGHLIGHT_MAX_PARSE_CHECKS_PER_CALL` so a call stays in the 0.5–5 ms band the line-state budget targets.
2. Record the timings, the chosen budget, and the tree memory for 1 MB and 10 MB in `LEDGER.md`. Measure memory via the allocator, or estimate it from the node count times the node size.
3. Docs:
   - the `highlight.rs` module doc covers both backends, the feature flags, the capture vocabulary and D5 precedence;
   - the crate docs and the three READMEs show a syntect and a tree-sitter `.language(..)` snippet plus the feature table;
   - the scrive-core Cargo `description` becomes "…an incremental syntect or tree-sitter highlight cache".

   Doc examples that use tree-sitter are cfg-gated, so the default `cargo test --doc` passes.

**Exit criteria**: `cargo bench --no-run -p scrive-core --all-features` builds. LEDGER.md has the tree-sitter rows, including memory. `cargo doc --workspace`, with default and all features, is clean with `-D warnings`.

## Critique log

Round 1 — 2026-10-04. The Plan agent confirmed these against the code and the 0.27 sources:
- the InputEdit convention;
- edit/inverse pairing (transaction.rs:213–222);
- resume after cancel;
- `changed_ranges`;
- the `StreamingIterator` re-export;
- workspace `default-features` semantics.

It flagged 4 major and 7 minor issues. All were addressed:

1. **Major — a byte-offset parse budget mismeasures work.** It jumps on reuse and stalls during balancing, and a resumed parse could livelock. → Budget on progress-callback counts. The budgeted parse is now its own Phase 6, with a one-call 10 MB keystroke test.
2. **Major — `uses_pool()` is size-only (code_editor.rs:1588).** A big tree-sitter document would never highlight, and the frames subscription would spin forever. → Phase 7 step 3 also requires an engine. The plan's wrong claim that `seed_highlight` loops is fixed.
3. **Major — `tree-sitter-rust` doesn't build on wasm32-unknown-unknown, and CI builds all targets there.** → Target-gated dev-dependency, gated tests and example, and the exact CI command as the check. Wasm grammar tests are out of scope.
4. **Major — a pub enum `Grammar` with feature-gated variants breaks feature additivity.** → An opaque `pub struct Grammar(Inner)` with `From` impls only.
5. **Minor — `HighlightCache` is public, and an external `Committed` has an empty inverse.** → It becomes `pub(crate)` with a `!` commit. Doc links are fixed. `new` takes `&Buffer`. The debug-assert stays in the cache.
6. **Minor — `Grammar::TreeSitter` would land before its backend.** → The variant moves to Phase 5.
7. **Minor — `set_byte_range` yields out-of-range captures, and predicates need rope text.** → Clipping in `paint_rows`, and a rope `TextProvider` (`rope_text.rs`).
8. **Minor — the precedence claim was imprecise.** → It is restated per node: deeper node wins on identical ranges, and the earlier pattern wins on the same node. Unstyled captures not blocking is a recorded deviation (D5).
9. **Minor — backend error types leaked.** → A scrive-owned `QueryErrorKind`, and an added `NotParseable` variant.
10. **Minor — dead shared code with neither feature on fails clippy, and the bench and iced tests use `SyntaxDef`.** → `any(..)` gates, `required-features` on the bench and examples, and gated tests.
11. **Minor — the Phase 1 "every entry resolves" test contradicted step 5.** → An expected `Option<Rgba>` table.

Also from the critique: Phase 2 is split into a pure move (2) and the facade with the API break (3), and Phase 9 records tree memory.

Post-checkpoint — 2026-10-04: the user ruled wasm32 support non-negotiable. The probe above disproved critique item 3's conclusion. tree-sitter's runtime builds and runs on wasm32-unknown-unknown with no imports, and only old grammar build.rs files lack the headers. → The target gates were removed. CI sets `CFLAGS_wasm32_unknown_unknown` from `cargo metadata`, Phase 4 adds a node-run wasm smoke test, and the consumer docs explain the flag for old grammars. Out of scope narrowed to `wasm32-wasip1`.
