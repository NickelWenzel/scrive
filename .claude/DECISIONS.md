# Decisions

Design calls made during implementation that MAP_PLAN.md didn't settle. Entries are append-only (see
`~/.claude/guides/DECISIONS.md`).

## D1 — Undecodable intel results settle the awaited slot

**Date:** 2026-09-26 · **Context:** lsp-bridge Phase 6, base 4503f39 · **Status:** decided

### Issue
Per D21, a payload that doesn't decode returns `Err(Error::Decode)`. An `Err` carries no update, so a
completion reply that fails to decode never settles the editor's awaited slot.

For signature help this is worse. While a signature request is awaited, `drive_signature` re-queries on
every editor event, so every one of those replies would fail again the same way.

### Options

| | A — keep `Err` | **B — settle with the empty answer** |
|---|---|---|
| Slot | stays until the next edit (completion) or forever re-queried (signature) | settles at once |
| Diagnosability | host sees the decode error | host still sees the raw message it routed in |
| Consistency | differs from server-error handling | same as the audit's rule for server errors |

### Decision
B. Intel requests (completion, signature, hover) always settle with their empty answer. The empty
answers are `Completions([])`, `Signature(None)` and `Hover(None)`, stamped with the pending
ticket. Only these still return `Err`:
- user commands (definition, rename, format), because a user asked for them;
- payloads that are not replies to a request.

### Followups
Phase 7 applies the same rule to signature and hover.

## D2 — Kind-3 completion requests require a list

**Date:** 2026-09-26 · **Context:** lsp-bridge Phase 6 · **Status:** decided

### Issue
`Session::new` starts marked incomplete. Suppose an in-flight request is dropped as stale, and a
later `Continuing` request still passes the D14 checks. That request goes out as
`TriggerForIncompleteCompletions`, even though no list was ever received.

### Decision
A request continues a session only if the session has received a list, or if its own request is
still in flight. Otherwise it starts a fresh session and is sent as `Invoked`, or as
`TriggerCharacter` if a trigger char matched.

### Why
LSP defines kind 3 as a re-trigger of an incomplete list the server already sent. Sending it
without a list misstates the context to a strict server.

## D3 — Glue is lenient on unregistered editors; close closes the popups

**Date:** 2026-09-26 · **Context:** lsp-bridge Phase 9, base 824fa24 · **Status:** decided

### Issue
Two problems with the draft `sync_lsp` and `close_lsp`:
- `sync_lsp` debug-asserted that the editor was registered, so a host that syncs every tab the same
  way panicked in debug builds.
- `close_lsp` left the completion popup, the signature box and the hover card open. Nothing can ever
  land in them after close, and a signature box that stays open keeps recording requests that are
  never sent.

### Decision
- `sync_lsp`, `apply_lsp` and `jump` on an editor that isn't registered do nothing (they return empty).
- A debug-assert remains only for an editor registered with a *different* client. That is the misuse
  D5's one-client-per-editor rule forbids.
- `close_lsp` closes all three popups, alongside the cleanup D19 already requires.

## D4 — `didSave` is in scope

**Date:** 2026-09-27 · **Context:** minimap stale-diagnostics-after-edit, base ae46e1d · **Status:** decided

### Issue
MAP_PLAN listed save notifications as out of scope. In the `rust_analyzer` example a fixed type
error kept its squiggle. The error comes from rust-analyzer's flycheck (`cargo check` over the file
on disk, `source: "rustc"`), which re-runs only on `textDocument/didSave`. A probe against
rust-analyzer 02dede3ce5 showed sync and diagnostics gating were exact; the server advertises
`"save": {}` and was simply never told about a save.

### Decision
- The client advertises `synchronization.didSave` (no `dynamicRegistration`) and reads the server's
  `save` option into an owned enum, `capabilities::Save { Never, Notify, WithText }`:
  - a missing `save`, `save: false`, and a bare sync kind → `Never`;
  - `save: true`, and `SaveOptions` without `includeText: true` → `Notify`;
  - `includeText: true` → `WithText`, carrying the synced (LF-normalized) text.
- `Client::save(&self, &Snapshot)` sends `didSave` only while running, for a document the server
  has open, when the server asks for saves, and when the snapshot's revision is the synced one, so
  the saved text provably is the text the server has. It changes no state, and a save before the
  handshake is not replayed: the server reads the disk when it starts.
- `CodeEditor::save_lsp` syncs, then saves, in one batch. Like the rest of the glue (D3) it does
  nothing on an unregistered editor. Writing the file stays the host's job.
- `willSave` and `willSaveWaitUntil` stay out of scope.

### Why
Without `didSave`, rust-analyzer's check diagnostics never refresh. The revision gate mirrors the
requests' gates and turns "sync first" from a documented contract into a checked one.

## D6 — `TokenTheme::builder()` takes a default foreground

**Date:** 2026-10-04 · **Context:** tree-sitter-highlighting Phase 1 · **Status:** decided

### Issue
The plan's builder has only `.capture(..)` and `.build()`. Under syntect, every unstyled run is painted
with the theme's default foreground, and a synthesized `Theme` with no `settings.foreground` falls back
to syntect's `Color::BLACK`. A builder-made theme would render plain text black on a dark editor.

### Options

| | A — no foreground | B — fixed fallback color | **C — `Builder::foreground(Rgba)`** |
|---|---|---|---|
| Syntect plain text | black | arbitrary | host's color |
| API | as planned | as planned | one extra method |
| Phase 8 `scrive_dark_theme()` table | can't match the tmTheme | can't match | can (`#DFE1E6`) |

### Decision
C. `Builder::foreground(Rgba)` sets `ThemeSettings::foreground`. Unset, syntect's default applies.
Tree-sitter ignores it: unstyled text gets no span and the renderer's own default.

### Followups
Phase 8's no-syntect `scrive_dark_theme()` table should call `.foreground(#DFE1E6)` so both builds agree.
Review (2026-10-04): the foreground is now also a backend-neutral `TokenTheme::foreground() -> Option<Rgba>`,
set by the builder and by `from_tm_theme` from `settings.foreground`. The renderer may use it for plain
text; it doesn't yet.

## D7 — `TokenTheme` stays a tuple struct with the syntect `Theme` at `.0`

**Date:** 2026-10-04 · **Context:** tree-sitter-highlighting Phase 1 · **Status:** decided

### Issue
Existing tests in `highlight.rs` read `eng.theme.0` as the syntect `Theme` (e.g. `str_state`), and
they must stay unmodified. Moving `TokenTheme` to `highlight/token_theme.rs` with named fields would
break them.

### Decision
`pub struct TokenTheme(pub(super) Theme, Vec<Rule>)`. Field `.0` is visible to `highlight` and its
descendants only; the capture rules are private to `token_theme`.

### Followups
Phase 8 gates the syntect half. Either keep `.0` as the syntect field (cfg'd, with the rules moved to
a named second field) or convert to named fields once the tests that read `.0` are cfg-gated anyway.
Also note: `escape` and `string.escape` share the scope `constant.character.escape`. In a
builder-synthesized syntect theme, the one added first wins (syntect keeps the first of equal-score
single selectors). Tree-sitter resolves them independently.
Superseded by D8 (named fields and a `syntect()` accessor). The escape tie is resolved there too.

## D8 — `TokenTheme` uses named fields behind a `syntect()` accessor (supersedes D7)

**Date:** 2026-10-04 · **Context:** tree-sitter-highlighting Phase 1 review, amending c992b25 · **Status:** decided

### Issue
D7 kept the syntect `Theme` at tuple field `.0` so tests stayed untouched. Production code read `.0`
as well (highlight.rs, four call sites), and positional fields don't survive cfg gating: once Phase 8
gates the syntect half, the indices shift or vanish.

### Decision
`TokenTheme { theme: Theme, rules: Vec<Rule>, foreground: Option<Rgba> }`, all private. Every
syntect read, in production and in tests, goes through `pub(super) fn syntect(&self) -> &Theme`,
which Phase 8 cfg-gates along with the `theme` field. Tests change only in field access.

The review also settled the `escape` / `string.escape` tie from D7: `Builder::build` emits theme
items ordered by capture depth (more dotted first), because syntect keeps the first of equally
specific selectors. The deeper capture wins regardless of call order.

## D9 — The oracle stays in `highlight.rs`; line-state internals and test fixtures widen to `pub(super)`

**Date:** 2026-10-04 · **Context:** tree-sitter-highlighting Phase 2 · **Status:** decided

### Issue
Phase 2 moves `LineState` and `tokenize_line` into `highlight/line_state.rs` but doesn't list
`Highlighter`, the `HIGHLIGHT_*` constants or `padded_highlight_window`. `Highlighter::highlight`
builds a `LineState` by its fields and calls `tokenize_line`. The cache tests that move with
`HighlightCache` use the fixtures `GRAMMAR`, `THEME` and `highlighter()`, which the `Highlighter`
tests that stay behind also use.

### Options

| | A — move `Highlighter` and the fixtures too | **B — keep them, widen to `pub(super)`** | C — duplicate the fixtures |
|---|---|---|---|
| Matches the plan's move list | no | yes | yes |
| Test bodies unchanged | yes | yes | no (new copies) |
| Visibility change | none | `LineState` + fields, `tokenize_line`, three fixtures | `LineState` + fields, `tokenize_line` |

### Decision
B. `Highlighter`, the constants and `padded_highlight_window` stay in `highlight.rs` (Phases 5 and 6
expect the constants and the window formula there). `LineState`, its two fields and `tokenize_line`
are `pub(super)`. In `highlight::tests`, `GRAMMAR`, `THEME` and `highlighter()` are `pub(super)`, and
`line_state::tests` imports them from `crate::highlight::tests`. `DirtyRanges` and its methods are
`pub(crate)`, per the plan.

### Followups
Phase 8 gates `Highlighter` with syntect; the shared fixtures go under the same cfg. Phase 4/5 tests
can reuse `GRAMMAR`/`THEME` only for syntect comparisons; tree-sitter fixtures live with their backend.

## D10 — `Grammar`'s inner enum is `pub(super)`; test-only `line_state::Cache` methods are `cfg(test)`

**Date:** 2026-10-04 · **Context:** tree-sitter-highlighting Phase 3 · **Status:** decided

### Issue
The plan calls `enum Inner` private, but the facade that turns a `Grammar` into a cache backend
lives in `highlight.rs`, a parent of `highlight/grammar.rs`, so it can't see a module-private enum.
Separately, once `HighlightCache` left the public API, `line_state::Cache::on_commit(start, old, new)`
and `first_dirty` had no non-test caller and failed `-D warnings` as dead code.

### Options

| | A — accessor on `Grammar` | **B — `pub(super)` field and enum** | C — facade in `grammar.rs` |
|---|---|---|---|
| Visible outside `highlight` | no | no | no |
| Extra API | `pub(super) fn into_inner` | none | none |
| Facade location | `highlight.rs` (plan) | `highlight.rs` (plan) | wrong owner |

### Decision
B. `pub struct Grammar(pub(super) Inner)` with `pub(super) enum Inner`: only `highlight` and its
children can name or match it; the root re-export exposes nothing. The two test-only cache methods
are `#[cfg(test)]` rather than deleted, so the cache tests stay unchanged.

### Followups
Phase 5 adds `Inner::TreeSitter` under cfg; `HighlightCache::new`'s match gains the arm. Phase 8's
cfg gating applies to the `Syntect` variant and its arm the same way.

## D5 — Tree-sitter capture precedence

**Date:** 2026-10-04 · **Context:** tree-sitter-highlighting Phase 4 · **Status:** decided

Numbered D5 because the plan reserved that number for it; it was written after D10, when Phase 4
implemented the painter.

### Issue
A highlights query captures one byte under several nodes and patterns. `capture_paint::paint_rows`
needs one rule that picks the style, and the incremental backend (Phase 5) must reproduce it exactly.

### Options

| | A — tree-sitter-highlight exactly | **B — tree-sitter-highlight, unstyled captures don't claim** | C — last pattern wins (nvim style) |
|---|---|---|---|
| Matches what grammar crates' `HIGHLIGHTS_QUERY` assume | yes | yes, for a full theme | no: those queries order general patterns last |
| Partial theme (no `constructor`), `Foo(..)` | plain | `@function` color | n/a |

### Decision
B:
1. A deeper node overrides every node enclosing it.
2. For two distinct nodes with the same range, the deeper one wins. tree-sitter's capture order
   doesn't encode depth (it sorts by start byte, then pattern), so `collect` ranks each node among
   the captured nodes sharing its exact range (its count of ancestors among them, via
   `Node::child_with_descendant`). Ranks are computed only for such ties; every other pair is
   ordered by range alone, since non-empty tree nodes nest or are disjoint.
3. On one node, the earliest pattern wins.
4. Deviation from tree-sitter-highlight: a capture whose name the theme doesn't style (`resolve` is
   `None`, including `@none` and `_private`) doesn't claim its node. A later pattern on the node
   colors it, or else the enclosing node's color shows through. A partial theme still colors as
   much as it can.

Zero-width captures are dropped. The painter sorts by (start, end descending, rank, node, pattern),
keeps each node's first styled capture, and sweeps the boundaries with a stack of open nodes
(innermost on top), so it needs no per-byte array.

### Followups
`@none` can't force plain text under rule 4. If a grammar relies on that, `resolve` needs a
"styled as plain" result distinct from "unstyled".

## D11 — The wasm smoke test is a `cdylib` example of scrive-core

**Date:** 2026-10-04 · **Context:** tree-sitter-highlighting Phase 4 · **Status:** decided

### Issue
Phase 4 step 7 allows a non-member crate under `crates/scrive-core/tests/wasm-smoke/` or an
example with `crate-type = ["cdylib"]`.

### Options

| | A — non-member crate | **B — `[[example]] wasm_smoke`, cdylib, `required-features = ["tree-sitter"]`** |
|---|---|---|
| Needs its own `[workspace]`, lockfile and target dir | yes | no |
| Covered by CI's all-targets wasm build, clippy and `cargo test --all-features` | no | yes |
| Built by default-feature commands | no | no (required feature) |

### Decision
B. `crates/scrive-core/examples/wasm_smoke.rs` exports `run() -> u32`, and `scripts/wasm-smoke.mjs`
instantiates it with an empty import object (failing on any import) and checks `run() != 0`. CI's
all-targets wasm build already writes it to `target/wasm32-unknown-unknown/debug/examples/`, so the
smoke step only runs node. Phase 5 changes `run` to the `Document` path.

## D12 — The tree-sitter-rust dev-dependency skips WASI

**Date:** 2026-10-04 · **Context:** tree-sitter-highlighting Phase 4 · **Status:** decided

### Issue
Dev-dependencies build for every test target, so the CI step
`cargo test -p scrive-core --lib --target wasm32-wasip1` would compile tree-sitter-rust's C for
WASI, which needs a wasi-sdk sysroot. That job runs default features only.

### Decision
`tree-sitter-rust` sits under `[target.'cfg(not(target_os = "wasi"))'.dev-dependencies]`. Its only
users are the `tree-sitter`-gated tests and the `wasm_smoke` example, neither of which builds on
WASI (the feature is out of scope there).

## D13 — The tree-sitter oracle tests check against the cache's own tree

**Date:** 2026-10-04 · **Context:** tree-sitter-highlighting Phase 5 · **Status:** decided

### Issue
The plan's oracle is `highlight_whole`, a fresh parse. With tree-sitter 0.27 and tree-sitter-rust
0.24, an incremental reparse of error-laden text can recover differently from a fresh parse of
the same text. One seeded run hit it after a single delete, and it reproduced from a freshly
parsed old tree with a correct `InputEdit`, so the difference is tree-sitter's, not the cache's.
Random edits made of syntax fragments produce such text all the time. The Phase 5 review reproduced
it independently: 39 seeds × 400 steps, 4 diverged, and in every divergent case both trees had errors.

User-visible consequence: while the code has syntax errors, its highlighting can differ from what a
reload of the same text shows, and the difference can persist across edits. It converges once the
text parses cleanly again.

### Options

| | A — fresh oracle only | B — valid edits only | **C — own-tree oracle, plus fresh on clean text** |
|---|---|---|---|
| Random fragment edits (comments, strings opened mid-row) | flaky | not covered | covered |
| Catches a wrong `InputEdit` | yes | yes | yes (point check) |
| Catches a missed invalidation | yes | yes | yes |

### Decision
C. `assert_matches_oracle` (parse_tree tests) compares retained rows with `highlight_tree`, a
whole-document paint of the cache's current tree. When that tree has no error node it also
compares with `highlight_whole`. Two more checks pin the `InputEdit` convention, since a reparse
re-lexes the tokens beside an edit and hides a wrong point there:
- After every commit, before the reparse, each node the edits didn't touch (`!has_changes()`)
  sits at the points its bytes give in the post-commit buffer.
- After a reparse, every node does.

`randomized_valid_multi_edit_commits_match_a_fresh_parse` keeps the text valid and asserts the fresh
oracle on every step. Mutating any of the six `InputEdit` fields, or the edit/inverse pairing, fails
the parse_tree tests.

### Followups
Phase 6's "matches the oracle" tests should use the same helper. The Document-level tests compare
against a fresh `Document`, so they use valid text only.

## D14 — The rope text helpers read `Buffer`'s rope, not a `Snapshot`

**Date:** 2026-10-04 · **Context:** tree-sitter-highlighting Phase 5 · **Status:** decided

### Issue
Phase 5 step 2 builds the parse input and `TextProvider` "over a `Snapshot`". The cache only ever
gets `&Buffer` (`tokenize(&Buffer, ..)`), and neither type exposes its rope.

### Decision
`rope_text` works on `&Rope`. `Buffer` gains `pub(crate) fn rope(&self) -> &Rope` (under
`cfg(feature = "tree-sitter")`). A `Snapshot` per call would cost an `Arc` clone for no gain: the
parse and the queries borrow the buffer for the call only. If an off-thread consumer ever needs
it, `Snapshot` can get the same accessor.

## D15 — The tree-sitter cache tracks dirt inside the window only and ignores `target`

**Date:** 2026-10-04 · **Context:** tree-sitter-highlighting Phase 5 · **Status:** decided

### Issue
The line-state cache has to walk dirt top-down outside the window, because its states chain from
row 0, and it stops its walk at `target`. The tree-sitter cache has no per-row state. A row
outside the window has no spans to repaint, so its dirt carries no information.

### Decision
`parse_tree::Cache` clips its `DirtyRanges` to the window after every commit, window move and
reparse. A window row is queried when it has no spans or is dirty. `tokenize` ignores `target`:
its work is bounded by the window (at most `HIGHLIGHT_MAX_WINDOW_ROWS`) and by `max_lines`, and the
facade's doc says so. `pending()` returns the first such row, or the window top while a reparse is
due.

## D16 — `HIGHLIGHT_MAX_PARSE_CHECKS_PER_CALL` is public, feature-gated, and starts at 100

**Date:** 2026-10-04 · **Context:** tree-sitter-highlighting Phase 6 · **Status:** decided

### Issue
Phase 6 adds the parse budget constant without saying whether it joins the public `HIGHLIGHT_*`
constants, or what value it starts at beyond "about 2 ms".

### Decision
It is `pub` in `highlight` and re-exported at the root like `HIGHLIGHT_MAX_LINES_PER_CALL`, under
`cfg(feature = "tree-sitter")`, because it only means something for that backend. It names no
tree-sitter type. A host reads it to reason about frame cost; it isn't configurable. The cache holds
a copy in a `parse_budget` field so tests can shrink it.

Value: 100. Measured 2026-10-04 with tree-sitter 0.27 + tree-sitter-rust 0.24, by counting
progress-callback calls during an uncancelled parse of generated Rust (1, 4 and 10 MB):
- release: 18.5–20.8 µs per check, so 100 checks ≈ 2 ms;
- debug (C at -O0): 51–55 µs per check.

At this value a first parse of 10 MB takes about 1,270 calls. Phase 9 tunes it.

See D18: a commit no longer cancels a pending parse.

## D17 — A keystroke reparse fits one call for nested code, not for a flat file

**Date:** 2026-10-04 · **Context:** tree-sitter-highlighting Phase 6 · **Status:** decided

### Issue
Phase 6's test wants a one-byte edit in a 10 MB document to reparse within one call. The same
measurement as D16 showed that the reparse cost depends on the shape of the file:
- A flat file of top-level items (fn/struct/blank, repeated) reparses in time linear in its item
  count: 709 checks at 1 MB and 7,080 at 10 MB (238 ms in release). tree-sitter steps over every
  reused top-level sibling.
- The same rows wrapped in `mod` blocks of 1,400 rows reparse in 31–114 checks at 1 or 10 MB.

### Decision
The test generates nested text (`nested_sample`), which is how real code is shaped, and asserts the
reparse finishes in one call at the default budget. The 31–114 range runs past the budget of 100,
so the one-call assertion also depends on the edit landing at a cheap spot. The test's spot (an
identifier mid-document) reparses in 14–15 checks at any size, so the test now uses 2 MB
(`a_keystroke_in_a_nested_multi_megabyte_document_reparses_in_one_call`). A huge flat file (generated tables, for one)
still converges, over about checks/100 calls (≈ 70 for 10 MB). Meanwhile its unedited rows show
their shifted old spans and its edited rows show plain text. No scrive-side fix exists short of a
bigger budget.

### Followups
Phase 9's benches should cover both shapes, and decide whether the budget should grow when a
reparse (as opposed to a first parse) is pending. D18 keeps such a multi-call reparse from being
restarted by every keystroke.

## D18 — A commit queues behind a pending parse instead of resetting it (supersedes Phase 6 step 3)

**Date:** 2026-10-04 · **Context:** tree-sitter-highlighting Phase 6 review · **Status:** decided

### Issue
Phase 6 step 3 resets the parser when a commit lands mid-parse. A parse longer than the gap
between keystrokes then never finishes: a first parse of 1 MB is about 110 calls (≈ 2 s), and a
flat 10 MB reparse about 70 (D17). Someone typing faster than that never sees the edited rows
highlighted.

### Options

| | A — reset (plan) | **B — snapshot and queue** |
|---|---|---|
| Typing during a long parse | starves: restarts every keystroke | parse finishes on its start text, then one catch-up reparse |
| Extra state | none | an O(1) `Rope` clone and a `Vec<InputEdit>` |
| After the parse | `changed_ranges` | whole window dirty when edits were queued |

### Decision
B. A parse holds `PendingParse { text: Rope, queued: Vec<InputEdit> }` from its first call until it
finishes.
- **Input:** the parse reads from `text`, an O(1) clone of the buffer's rope at parse start (D14),
  so a resumed call sees the same text. parser.c re-seeks its position through the input callback
  on resume.
- **A commit while it's pending:** the commit's `InputEdit`s are computed at commit time and
  queued. Neither `self.tree` nor the parser is touched. The parser retained the root it started
  from, and `self.tree` stays that tree, unedited. The window shift and dirt splicing run as
  usual.
- **Done, nothing queued:** `changed_ranges(old, new)` dirt, as before.
- **Done, edits queued:** apply them in order to the new tree (they are sequential in tree
  coordinates from the start text) and store it. Keep `reparse` set and dirty the whole window,
  because `changed_ranges` against the old tree would be in rows the queued commits have moved.
  The next call reparses incrementally against the buffer. Stale spans stay visible meanwhile.

Tests cover typing on every call (each parse still finishes and the highlight converges), several
queued commits including multi-edit ones, and commits queued during both a first parse and a
reparse. Dropping the queued edits, or the whole-window dirt, fails them.

## D19 — With no highlight feature, `Grammar` and the cache backend are empty enums; `Document` keeps every highlight field ungated

**Date:** 2026-10-04 · **Context:** tree-sitter-highlighting Phase 8 · **Status:** decided

### Issue
Phase 8 step 2 gates `HighlightCache` and the `Document` fields and methods that use it on
`any(syntect, tree-sitter)`, and also asks that `Document`'s highlight methods keep compiling as
no-ops with neither feature. The two conflict: `Document` touches `highlight` in about a dozen
places (the field, its init, `Views`, `rebase_views`, three destructures, six methods), each of
which would need a cfg.

### Options

| | A — gate the cache and the `Document` sites (plan) | **B — empty `Inner` and `Backend`, nothing gated in `Document`** |
|---|---|---|
| cfgs in `document.rs` | ~12, scattered through bodies | 2 (`highlight_engine`, `absorb_highlight`, syntect-only types) |
| No-feature behavior | methods return `None` / do nothing | same: no `Grammar` value exists, so `highlight` stays `None` |
| Cost in the facade | none | its matches are on the place `self.backend` with `ref`/`ref mut` arms (an empty match on `&Backend` doesn't compile), and one `expect(unused_variables)` on the impl for the no-feature build |

### Decision
B. `grammar::Inner` and `highlight::Backend` gate each variant; with neither feature both are
uninhabited, `HighlightCache::new` is `match grammar.0 {}`, and every facade method compiles to
an empty match. `splice`, the window constants and `padded_highlight_window` stay ungated (pub or
used by the facade). `dirty_ranges` is `any(..)`-gated and its syntect-only methods carry
`cfg(feature = "syntect")` like `insert_range` carries `tree-sitter`.
`HIGHLIGHT_CHECKPOINT_STRIDE` is syntect-only, being a line-state concept.

scrive-iced does the same thing at the widget seam: the pool-driving code moved to
`code_editor/pool.rs` (as `lsp.rs` holds the lsp glue), whose four hooks (`pool_viewport`,
`pool_sweep`, `pool_seed`, `pool_active`) return `false` when the synchronous path should run.
Without syntect a second, trivial `impl` returns `false` from each. Only the `hl_pool` field,
its init and the `HighlightPool` import carry a cfg in `code_editor.rs`.

## D20 — `TokenTheme::resolve` is public, and `TokenTheme::vocabulary()` lists the captures

**Date:** 2026-10-04 · **Context:** tree-sitter-highlighting Phase 8 · **Status:** decided

### Issue
Step 5's drift test lives in scrive-iced and compares the builder table with the tmTheme-derived
capture rules "for every vocabulary entry". Both the rules (`resolve`, `pub(crate)`) and the
vocabulary (`pub(super)`) were invisible outside scrive-core.

### Decision
`TokenTheme::resolve(&self, &str) -> Option<SpanStyle>` becomes `pub` (it names no backend type,
and a host can use it to compare themes or show a capture's color). `TokenTheme::vocabulary()`
returns the standard capture names in table order, so the test can't miss a capture added later.
This also removes `resolve`'s `expect(dead_code)`, and keeps `vocabulary.rs` live in every build
(only its `scopes` lookup is syntect-gated). The test asserts equal `resolve` for each vocabulary
capture (full `SpanStyle`, so comment's italic counts) and equal `foreground()`.

## D21 — Tests that need syntect are gated, not rewritten

**Date:** 2026-10-04 · **Context:** tree-sitter-highlighting Phase 8 · **Status:** decided

### Issue
Several tests use a `SyntaxDef` only as a convenient grammar while checking something
backend-neutral (`undo_resyncs_brackets_and_highlight_to_the_reverted_text`,
`bracket_lexing_skips_in_string_brackets`). Others mix both backends
(`a_large_tree_sitter_document_converges_through_the_sweep_without_a_pool` asserts there is no pool).

### Decision
Each such test is `cfg(feature = "syntect")`, or `all(syntect, tree-sitter)` when it uses both.
The default and all-features runs, which CI always does, keep the full coverage; the tree-sitter-only
run covers the tree-sitter backend and everything that needs no grammar. One assertion was dropped
rather than gated: `undo_and_redo_resync_tree_sitter_highlight` checked `highlight_engine().is_none()`,
which `absorb_highlight_takes_no_segment_for_tree_sitter` already asserts.

## D22 — The parse budget stays at 100 checks, the same for a reparse as for a first parse

**Date:** 2026-10-04 · **Context:** tree-sitter-highlighting Phase 9 · **Status:** decided

### Issue
Phase 9 tunes `HIGHLIGHT_MAX_PARSE_CHECKS_PER_CALL` into the 0.5–5 ms band, and D17 asked whether
the budget should grow while a reparse, rather than a first parse, is pending.

### Options

| | **A — 100 for both** | B — 200 for both (extrapolated) | C — 100 first parse, larger reparse |
|---|---|---|---|
| First-parse call, median / max (1 and 10 MB nested) | 2.2–2.6 / 4.7 ms | ~4.5–5 / ~8–9 ms | 2.2–2.6 / 4.7 ms |
| Flat reparse call (100 checks cost 3.0–3.4 ms there) | 3.0–4.8 ms | ~6–10 ms | over 5 ms |
| 10 MB first parse | 1,059 calls | ~530 calls | 1,059 calls |

### Decision
A. Measured with the `tree_sitter` bench (benches/LEDGER.md, i7-9750H, release): every call sits
inside the band, the finishing call's window query included. A reparse check costs more than a
first-parse check, so a larger reparse budget would leave the band on exactly the flat files that
need more calls. The constant's doc now cites the measured 2.2–2.6 ms instead of D16's ~2 ms; the
value and every test bound that depends on it are unchanged.

### Followups
The flat-file keystroke spike isn't the budget's: the call that finishes a reparse runs
`changed_ranges`, which walks every top-level sibling (13.6 ms at 1 MB, ~145 ms at 10 MB, unbudgeted).
Nested code doesn't pay it. Fixing it means invalidating rows without `changed_ranges`, for instance
from the edited rows plus a bounded walk of the window's nodes; not planned.

Tree memory, for the record: 36× the text on the bench corpus, 28× on scrive's own sources
(allocator-counted, ~94 bytes per C allocation). A tree-sitter document holds it while open.

## D23 — Syntect drops spans in the theme's plain style

**Date:** 2026-10-04 · **Context:** follow-up after the tree-sitter visual check, base 5f519b7 · **Status:** decided

### Issue
Under syntect, every unstyled run got a span in the theme's default foreground. Scrive Dark's is #DFE1E6, so in the iced light palette plain identifiers were light gray on white and hard to read. Tree-sitter didn't have the problem after 5f519b7, because it spans only styled captures and the renderer draws the gaps in the palette's text color.

### Decision
`line_state::tokenize_line` drops a span whose foreground and font style equal the syntect theme's default (`Highlighter::get_default`). Both backends now span only styled text, and the palette owns plain-text color. `Document::highlight_line_spans`, `Builder::foreground` and the renderer's `colored_runs` docs say so.

### Why
It's one rule for both backends, and plain text follows light and dark mode with no theme switching. Colored tokens still use the dark theme's colors on a light background. Picking a light theme for light mode stays a separate, later choice.

### Followups
D6's reason for a builder foreground (plain text would render black under syntect) no longer applies to drawing, because plain-style text has no span. The setting still decides which runs count as plain.
