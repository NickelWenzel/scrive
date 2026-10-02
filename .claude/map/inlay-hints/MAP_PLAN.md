# Inlay hints — virtual text from the LSP bridge

> **Draft 7.** Agent critique (4 rounds), FOSS comparison and expert critique (2 rounds) all applied. Next: phase docs (step 6). The critique log at the end records each round.

## Context

scrive's `CodeEditor` gets diagnostics, completion, signature help, hover, definition, rename and
formatting from a language server through `scrive-lsp` (the `lsp` feature, branch `lsp_bridge`).
This plan adds **LSP inlay hints**: short labels such as `: i32` or `name:` that the server places at
buffer positions. They render inside the line and push the following text right.

Interview decisions (2026-09-28):

| Question | Answer |
|---|---|
| Rendering | **Inline; text shifts.** Hints occupy cells and push the rest of the line right. Caret, clicks, selection and find step over them. |
| Interactions | **All four:** toggle on/off; hover tooltip (through `inlayHint/resolve`); clickable label parts (a part with a location jumps through the existing definition jump); double-click applies the hint's `textEdits`. |
| While typing | **Hints move with the edits.** They stay anchored, like diagnostics, until a fresh set arrives. |
| Prior art | Zed's `InlayMap`, Helix's inline annotations, Lapce/Floem phantom text. |
| Conventions | As for the LSP bridge: `/iced`, `/commit-and-comment`, RUST_STYLE, OPAQUE, local tracking, orchestrator commits. |

## Current state — verified ground truth

Read from source at HEAD `8e72665` (branch `lsp_bridge`) by three exploration passes, plus a live
probe against rust-analyzer `02dede3ce5`. "TODO before dispatch" re-checks the line numbers.

### scrive-core: there is no display-map stack

- **Rows.** `FoldMap` hides block-folded rows (fold_map.rs:496-508). The document caches it under
  `(buffer revision, FoldSet::generation)` and shifts it in place on edits (document.rs:108-119,
  336-353, 1155-1173).
- **Columns.** `RowLayout` (row_layout.rs:183-191) is built per row, per use, and never stored
  (:175-182). Per row it expands tabs with the free functions `display_map::expand` / `collapse`
  (display_map.rs:218, 334), then applies a fixed cell shift per collapsed inline fold (`shift_at`,
  row_layout.rs:228-234). Tab stops are measured on the raw buffer line; content after a fold keeps
  its buffer-space tab widths. `TabMap`, `DisplayPoint`, `DisplayChunk(s)` and `DisplayEdit` are
  exported but unused outside display_map.rs's tests. Soft wrap is rejected on purpose
  (display_map.rs:10-11, 25-26).
- **The one forward projection** is `FoldMap::display_position(buffer, offset, tab)`
  (row_layout.rs:453-469), "THE owner of where does buffer `offset` render". It takes **no bias**.
  `RowLayout::display_cell(col)` (:245) and `caret_cell(col)` (:252-260) take none either. The
  inverse is `FoldMap::hit_row(buffer, row, cell, bias, tab)` (:476-486) → `RowLayout::hit`
  (:276-292) or `HeaderLayout::hit` (:415-425).
- **Chips are the nearest precedent.** An inline fold hides `open+1..close` behind
  `INLINE_CHIP_CELLS = 3` cells (:36). Its shift formula allows net-positive width. But a chip has two
  caret stops at two offsets (`open+1`, `close`); a hint has **one offset with two visual sides**.
  Modelling a hint as a zero-width inline fold breaks in nine ways (the core pass's §2.7): `FoldMap::new`
  refuses empty interiors (fold_map.rs:538); the width is a global constant; the inline tree is keyed
  by unique openers; an edit overlapping an inline fold rebuilds the whole `FoldMap`
  (fold_map.rs:569-577, 624-652), which would fail `keystroke_and_arrow_do_not_rebuild_the_fold_map_at_scale`
  (document.rs:4649-4705).
- **Call sites.** `FoldMap::{row_layout, header_layout, display_position, hit_row}` have 26 non-test
  call sites outside row_layout.rs: editor.rs:804, 816, 856, 898, 967, 969, 1286, 1423, 1605, 1626,
  1679, 2993, 3019, 3074, 3168, 3212, 3215; document.rs:907, 914, 1038, 1039, 1573, 2334, 2437;
  movement.rs:207, 224. Every one passes `(buffer, …, tab)` next to the fold map. They split in two:
  - **Geometry (23 call sites):** editor.rs ×17, `caret_corner` / `rebuild_column_box` (document.rs:907-914,
    1038-1039) and `vertical_by` (movement.rs:207, 224).
  - **Visibility probes:** `toggle_fold_opener` (1568-1576, on a destructured `self`),
    `unfold_to_reveal` (2328-2334) and `expand_folds_touched` (2431-2437, over a hypothetical `sub`
    `FoldSet` inside `rebase_views`) only ask "does this offset render?". The
    `DISPLAY_POSITION_PROBES` canary (row_layout.rs:28-32) counts these.
- **Borrow shapes.** `move_carets` freshens the fold cache, then borrows `fold_cache` and `selections`
  separately (document.rs:434-444). `add_caret_vertical`, `column_select` and `column_drag` build
  their own `FoldMap::new` per press and then mutate `self` (608, 866, 890; `rebuild_column_box(&mut
  self, …)` at 1030). Tests call `display_position` / `header_layout` directly (document.rs:2872,
  3084; editor.rs:4571, 4626; movement.rs:515).
- **`HeaderLayout` derives from the head row's `RowLayout`**: `head_cells` and `tail_cell`
  (row_layout.rs:343-358) are what will shift the gap and tail past header hints.
- **Consumers that are byte-only** and need no change: find, word motion, Ctrl+D, bracket matching,
  verbs. Left/Right step bytes and would cross a hint in one keypress. Up/Down keep a display-cell
  goal (`vertical_by`, movement.rs:191-226); box selection works in display cells
  (`caret_corner`, `rebuild_column_box`, document.rs:877-918, 1030-1046).

### scrive-core: decorations are the anchor mechanism

- `DecorationStore` (decorations.rs:434-444) is a delta-gap interval `SumTree` in `(start, id)`
  order. Its single-edit mover is windowed (`apply_single_edit`, :888-919); multi-edit patches take
  the naive path (:856-867); both share `remap_ranges` (:392-399) and are pinned together by
  `windowed_apply_patch_equals_naive_under_random_single_edits` (:1395-1462).
- `Stickiness` (:88-99) maps to `(start bias, end bias)`: `AlwaysGrows (L,R)`, `NeverGrows (R,L)`,
  `GrowsOnlyBefore (L,L)`, `GrowsOnlyAfter (R,R)`. `DecorationKind` (:118-145) is `#[non_exhaustive]`
  with `Diagnostic`, `FindMatch`, `SnippetStop`, `AutoClosePair`; every in-crate match has a `_` arm.
  `empty_policy` (:152-157) is `Drop` only for `FindMatch`. **A collapsed range with `Drop` must never
  be stored**: the windowed and naive movers diverge on it (:883-887, 1436-1441).
- `rebase_views` calls every store's mover once per transaction, on edits, undo steps and redo steps
  (document.rs:1123-1136, 1238-1249, 1280-1290, 2555). The auto-close pairs have a **dedicated store**
  (document.rs:54-66) "so every arm/clear/validate is O(pairs) instead of scanning the bulk store".
  `Views` (:2464-2479) says adding one "is a field here plus one line in `rebase_views`".
- Gaps: no batch insert with a kind per item (`add_sorted_batch` clones one kind, :746-763); no
  revision-gated replace except `set_diagnostics` (:936-965); `take_decoration` is O(n) (:492).
- Diagnostics are revision-gated; a stale set is dropped, never forwarded (decorations.rs:232-234).

### scrive-core: performance gates

- The meter `perf::charge` (perf.rs:31) and the scale matrix (perf_gate.rs:89-100: only Constant ≤
  small·1.25+256 and Linear ≤ small·2.6+256; there is no O(n log n) budget). Canaries: `FOLD_BUILDS`, `DECORATION_SORTS`, `DECORATION_VISITS`,
  `DISPLAY_POSITION_PROBES` (≤ 4 per caret per commit, document.rs:4428-4465), `NODE_ALLOCS`.
- The widget's draw budget: at most `1024 * (viewport rows + 1)` rows visited per frame
  (editor.rs:1766-1774, 2677-2709): "no per-frame work proportional to anything but the viewport".

### scrive-iced: painting and interaction

- Geometry is rebuilt inside `draw` every pass (`layout` only ensures metrics, resolves autoscroll and
  clamps scroll, editor.rs:1254-1345). `Geo::cell_x`/`x_cell` (geo.rs:100-110) are exact inverses.
- **Every place that assumes "cell = byte column after tabs and folds"** (the widget pass's list):
  `offset_screen_x` (editor.rs:795-807); the `offset_xy` closure in `draw` (1418-1425; carets 1843,
  bracket box 1708); `popup_anchor` (815-825); horizontal autoscroll (1286, 1321-1332); `draw_spans`
  (3332-3358, positions spans with `display_map::expand`, bypassing `RowLayout`); the unhighlighted
  plain row (1613-1615); `draw_row_inline` (3083-3142); the collapsed header (1619-1662); bracket
  colouring (1665-1696); the bracket box width (1709) and `Geo::inline_halo` (geo.rs:186-193);
  squiggles (1713-1760); `draw_wash_row` / `draw_fold_tail_wash` (2976-3037); `collapsed_chip_rect` /
  `chip_pill_rect` (3149-3220); `max_line_px` (950-974) and everything derived from it; the fold
  preview (3249-3328); `hit_test` (3066-3075); `hit_cell` (3058-3064); `collapsed_chip_at`
  (3224-3239); the hover `still_in` test (2263-2266).
- **A tab bug.** `expand_tabs` (3462-3481) counts tab stops from cell 0 of each run, while
  `draw_spans` and `draw_row_inline`'s `seg` place each run at its true start cell (3346-3351,
  3100-3101). A tab inside a span that doesn't start on a tab stop paints too wide, so glyphs after
  it drift right of the caret. Found by reading, not reproduced.
- **Press order** (2024-2166): unfocus, fold-preview clear, completion rows, scrollbars, gutter
  toggle, collapsed chip (plain click unfolds, returns before `mouse::Click::new`), Ctrl collapse
  (compiled out by `SHOW_CTRL_COLLAPSE_AFFORDANCE = false`, 86-92), then `hit_test` plus click-count
  granularity (2147-2164). No Ctrl+click action is live; goto-definition is F12 only.
- **Hover arming** (2248-2287, 2570-2606): a move re-arms a 300 ms idle timer on iced's `now`; the
  redraw that crosses it checks the collapsed chip first (fold preview), else publishes
  `HoverQuery(offset)`. `hover_queried` and `HoverDismiss` retire an unanswered query. The host arms
  at code_editor.rs:857-882; `hover_pending` (code_editor.rs:1141-1147) keeps a move inside the
  awaited word from cancelling it.
- **Viewport.** The widget publishes `ViewportChanged(rows)` (buffer rows, ±8 rows margin) whenever
  the window changes (editor.rs:2011-2021); `CodeEditor` stores it (code_editor.rs:759-780) and
  closes hover on scroll.
- **Clock.** `CodeEditor::update` stamps `now_ms` from iced's instant (752-753); hosts run
  `iced::application::timed`. The find "debounce" is a rate limit checked on the next edit
  (find.rs:754-763): **no timer wakes the editor**. Timer precedents: `window::frames()` while the
  highlight sweep is active (code_editor.rs:1325-1332), and `shell.request_redraw_at` in the widget.
- **The LSP glue.** `sync_lsp` routes the client's local answers through `route`, which
  `debug_assert!`s that `applied.jump.is_none()` and drops the `Applied` (code_editor/lsp.rs:185-201).
  `land` matches the exhaustive `update::Change` (lsp.rs:213-294) and reads the ticket verdict before
  applying (227). `try_edit` runs `after_edit` before returning (code_editor.rs:546-553).
- **Toggles** are builders (`find(bool)`, `rename(bool)`, code_editor.rs:431-445). `Event` is
  hidden from hosts; a runtime toggle is a `pub fn`.
- **Async seam.** `Awaiting { completion, signature, hover: (Ticket, u32, Range), definition }`
  (293-302), `accepts` (1792-1800: `awaited == Some(ticket) && ticket.revision() == doc.revision()`),
  `abandon` (1804-1823), `take_*`/`set_*` pairs (614-714), `hover_card` (1957-1972).

### scrive-lsp

- The pending machinery (client.rs): `Kind` (184-193), `Query` (195-207, with `kind`, `method`,
  `caret`, `request` at 1405-1525), `send` (933-982, supersedes same doc and kind with
  `$/cancelRequest`), `settled` (748-788), `reissue` once per ticket on ContentModified (790-822),
  `resolved` (824-833), `Pending::failed` (1361-1383), `target` (1074-1103: `Local`, `Open`,
  `Unopened`).
- **Server requests** go through `answer` (1184-1194), which always returns `updates: Vec::new()`,
  and `respond` (1196-1250), which answers every `workspace/*/refresh` with `null` (1236-1240).
- Capabilities (client/capabilities.rs): no `inlayHint` advertised; `Server` has no inlay field; the
  `OneOf` pattern at 89-100 is the template.
- `target(&self, entry: &Pending, …)` needs a pending entry for its `request_snapshot` and
  `revision_of` (1076-1103); `send` records other documents' `revisions` only for
  `Definition | Rename` (953-960). Requests decline while the state is not `Running` (450-452), and
  hosts call `open_lsp` before `initialize` finishes (the `rust_analyzer` example starts in
  `Link::Connecting`); `initialized` is at 835.
- `update::Change` (update.rs:39-56) is exhaustive and has no hint variant. The README says inlay hints are not
  covered (scrive-lsp/README.md:86-88).
- `workspace.rs` decodes locations per entry so one bad URI doesn't fail a whole result (:1-5, 82-83);
  `edits::hygiene` (56-80) turns `TextEdit`s into well-ordered `EditOp`s.

### rust-analyzer, measured (probe at `02dede3ce5`, 129-line scratch file)

- `inlayHintProvider` is `{"resolveProvider": true}` **only if** the client lists some lazily
  resolvable property; which fields are deferred follows the advertised property list exactly.
- Default kinds: 81 hints (51 type, 29 parameter, 1 closing brace), 0.63 per line. **No tooltips at
  all with default settings**, resolved or not. Without lazy `label.location`/`textEdits`, 61 of 101
  label parts carry a location inline (25 into unopened sysroot files) and 30 type hints carry
  `textEdits`; the payload is 37% larger than in resolve mode (28 KB vs 21 KB).
- Padding is always sent as explicit booleans: type hints `false/false`, parameter `false/true`,
  chaining and `= usize` `true/false`, closing brace `true/false`.
- **Hints arrive unsorted**; several can share one position (8 at one offset with all kinds on).
- **`workspace/inlayHint/refresh`** (only with `refreshSupport`) arrives twice at load (+0.7-1.1 s),
  1-2 ms after **every** `didChange`, and after a workspace reload. It never arrives after indexing
  or `cargo check`. The first non-empty answer is only reachable because the post-refresh request
  fails with ContentModified at the end of indexing and a **single re-issue** (what `Client::reissue`
  already does) succeeds (~+4.1 s).
- A request in flight across a `didChange` always answers ContentModified; the server never returns
  old-text positions. A **stale resolve** silently returns the hint without its deferred fields.
- **Ranged requests drop hints whose syntax node starts before the range** (chaining hints of a
  chain that starts above it). Warm whole-document requests take 14-20 ms; ranged ones 2.5-7 ms.
- No `initializationOptions` are needed; `workspace/configuration` answered with `null` keeps the
  defaults.

### Repo

- Version 0.4.0 is **unreleased**: the tags are `v0.2.0` and `v0.3.0`, and `lsp_bridge` is not
  pushed. Breaking API changes in this plan ride the same 0.4.0.
- Baseline after 8e72665: 901 tests pass with `--all-features`; clippy (both feature sets), doc and
  the wasm all-features build are green.

## Target state

```
LSP server ─JSON-RPC─▶ host transport ─▶ Message::Lsp ─▶ Client::receive ─▶ Update::Document ─▶ CodeEditor::apply_lsp
                                                                                       │
    Change::Inlays / InlayTooltip / InlayRefresh / Definition / Edits ◀────────────────┘
                                                                                       ▼
scrive-core Document ── inlay store (DecorationStore, anchored, moved by rebase_views)
                     └─ Rows view (FoldMap + Buffer + inlays + tab) ─▶ RowLayout with hint spans
                                                                                       ▼
scrive-iced Editor ── paints labels, projects with Edge, hit-tests hints ─▶ Action::Inlay*
CodeEditor ── schedules fetches (debounced on its clock), records interactions, shows tooltips
```

When this plan is done:
- With hints enabled on a `CodeEditor` registered with a `Client` whose server has an inlay-hint
  provider, hints for the visible rows (padded) appear inline and move with edits.
- The caret, clicks, selection, find, squiggles and brackets behave as if the hint were not buffer
  text; Left/Right cross a hint in one press; Up/Down keep their visual column.
- Hovering a hint shows its tooltip (resolved on demand), or the hover at the label part's location.
- Ctrl+click on a label part with a location jumps there (open doc, other open doc, or unopened file).
- Double-click on a hint with text edits inserts them; the hint disappears.
- `CodeEditor::inlay_hints(bool)` / `set_inlay_hints(bool)` toggle them; disabled means the layout is
  exactly the no-hint layout.
- The scripted `lsp` example shows hints; the `rust_analyzer` example shows real ones and toggles
  them with a key.

## Key design decisions

### Model and anchoring (scrive-core)

**D1 — The core hint model is LSP-free and display-sized.** New module `intel::inlay` (`Hint`,
`Part`, `Padding`, `Kind`, `Key`, `Outcome`), with `Request` and `Interaction` in their own
submodules `intel/inlay/request.rs` and `intel/inlay/interaction.rs` because scrive-lsp imports them
on their own (RUST_STYLE: one semantic type per module).

```rust
pub struct Hint {            // private fields; immutable once installed
    kind: Kind,              // Type | Parameter | Other
    placement: Placement,    // Suffix | Prefix | Auto: which neighbour it annotates (D3)
    label: Vec<Part>,        // non-empty; each part's text sanitized (control chars → ' ')
    padding: Padding,        // { left: bool, right: bool }
    key: Key,                // opaque to core; minted by the host (the client)
    insert: Insert,          // Available | Unavailable: the host can apply edits for it
}
pub struct Part { text: String, link: Link }  // Link::Jumps | Link::None
pub struct Key(u64);
```

- Built by `Hint::new(kind, label, key) -> Result<Hint, inlay::Error>` plus builder setters
  `.padding(Padding)`, `.insert(Insert)`, `.placement(Placement)`; no raw `bool` parameters (OPAQUE
  N4, DISPATCH). Getters (RESOLUTIONS.md R1): `kind()`, `parts()`, `padded()`, `insertable() ->
  bool`, `key()`, `width()`; `Part::{text(), link()}`.
- The hint carries **no offset**: an offset goes stale once the hint rides edits. Installing takes
  `Vec<inlay::Placed>`, where `Placed { offset: u32, hint: Hint }` (private fields, `Placed::new`)
  pairs a fetch-time offset with its payload; the store owns the position afterwards. `Placed` is
  what crosses the seam (`Change::Inlays`, `set_inlays`).
- `placement` (input `Placement { Suffix, Prefix, Auto }`; the store keeps a resolved
  `Side { Suffix, Prefix }` in `Anchor`): `Hint::new` defaults `Type` → `Suffix`, `Parameter` →
  `Prefix`, `Other` → `Auto`, and `.placement` overrides it (R5). The client sets `Other` by
  padding, Zed's rule (`hint_position_and_bias`, lsp_command.rs:3897-3933 at zed `1399a80`), on the
  **raw** server flags before collapsing them (D16): right-only → `Prefix`, left-only → `Suffix`,
  symmetric (`false/false`, `true/true`) → `Auto`. `set_inlays`
  resolves `Auto` against the buffer (RESOLUTIONS.md R27, measured on all 137 hints of
  rust-analyzer `02dede3ce5` with every kind on): `Suffix` when a word char precedes `p`, or the char
  at `p` is whitespace, end of line/buffer, or one of `) ] } , ; .`; otherwise `Prefix`. So
  adjustments before `&`, `*`, `(`, `"`, `[` or `|` (`&*` at `‸&s`, `<closure-to-fn-pointer>` at
  `‸|| f`) annotate the expression after, and `<'0>` at `foo‸(`, discriminants at `A‸,` and drops
  at `}‸` annotate what is before. rust-analyzer labels elided lifetimes `'0`/`'1`; `'0 ` before
  `str` is right-padded and so `Prefix` without `Auto`. The one known miss is the range-exclusive
  `<` at `0..‸10` (off by default; no visual error).
  `Auto` never reaches the store: `Side` has no such variant.
- Width in cells = `padding.left + Σ part chars + padding.right`, one cell per scalar value, the same
  rule as the rest of the grid (display_map.rs:6-7).
- A label with no visible text is rejected by `Hint::new`, so an installed hint always has width.
- Tooltips, locations and edits stay with the host; core stores only what layout and gestures need.

**D2 — Hints live in a dedicated `DecorationStore`**, wired like the auto-close store: a
`Document` field, `Document::new`, `Views`, one line in `rebase_views`, and the `undo`/`redo`
destructuring. They ride every edit, undo and redo through the one mover. A new variant
`DecorationKind::InlayHint(inlay::Anchor)` carries the payload. `Anchor` is opaque with a
crate-private constructor (it holds `Arc<Hint>` and whether the range is anchored), so a host can't
put a collapsed `Drop` range into the public bulk store through `decorations_mut()`
(document.rs:1398). `empty_policy` gets an explicit `InlayHint` arm, not the `_` arm. Diagnostics and
find never see hints, and a hint publish never re-sorts them.

**D3 — A hint is anchored to the token it annotates.** A zero-width range would stack stale
parameter hints at one point when an argument list is deleted (Helix's behaviour); Zed and Lapce hide
a hint whose anchor text is deleted. The anchor is the **word** next to `p` (the core word
classifier), or one char when the neighbour is not a word char, so backspacing a typo at the end of
`count` doesn't drop its hint and jitter the line.

| Side | Annotates | Stored range | Stickiness | Renders at | Typing at the hint's offset |
|---|---|---|---|---|---|
| `Suffix` | the token ending at `p` | `[word_start, p)` | `GrowsOnlyAfter` | `min(range end, end of the row holding range start)` | text lands before the hint (`let xy: i32`) |
| `Prefix` | the token starting at `p` | `[p, word_end)` | `GrowsOnlyBefore` | `max(range start, min(range end, first non-blank of the row holding range end))` | text lands after the hint (`foo(n: yx)`) |

- **Row clamp.** Enter at the end of a line grows a `Suffix` range across the newline and the
  auto-indent; without the clamp an end-of-line chaining or closing-brace hint (both on by default in
  rust-analyzer) would jump to the next line, right of the caret. The clamp keeps it on its line.
  `RowLayout` filters the row's touching query by the clamped render row. `Prefix` mirrors it for
  Enter typed before an argument.
- Deleting the whole anchor token empties the range, and `EmptyPolicy::Drop` removes the hint
  (`empty_policy` returns `Drop` for an anchored `Anchor`). Undo does **not** restore a dropped hint;
  the refetch the undo triggers does.
- **Replacing the whole anchor drops the hint too.** Patch mapping keeps interior offsets
  (patch.rs:9-12, 201-214), so after a paste over a selection, a line-replacing formatter edit or a
  host replacement, a surviving anchor would sit at an arbitrary byte inside the new text, and
  retyping a selected word would drop a `Suffix` hint (its `(R,R)` range collapses) but keep a
  `Prefix` one. So `remap_ranges` drops an anchored `InlayHint` range when one edit's old range
  covers the whole anchor and inserts text. Such a range touches the edit, so it is in the windowed
  mover's middle band (decorations.rs:883-899) and the oracle equivalence holds. Backspacing inside
  the word doesn't cover the whole anchor and keeps the hint.
  - The coverage test reads the range in **old** coordinates, before `remap_ranges` overwrites
    `r.range` (decorations.rs:392-395).
  - On the naive (multi-edit) path it finds the covering edit with one `partition_point` on
    `old.end` per endpoint, as `map_many` does (patch.rs:177), not O(n·e) comparisons (a 10k-caret
    edit over a few hundred hints).
  - `Document::edit` doesn't trim common prefixes or suffixes, so verbs that replace whole regions
    drop the hints there until the refetch (~300 ms): `move_line` (Alt+↑/↓) replaces both lines in
    one op (document.rs:939-961), and case changes do the same. That beats carrying hints to the wrong
    line. Tested: Alt+↓ drops, then the refetch restores.
- **No mid-word render guard** (removed at the phase-doc audit, RESOLUTIONS.md R11): it would hide
  correct hints after typing at a shared offset (`||X -> fn()<…>f`). Two touching multi-cursor edits
  that split one anchor can still misplace a hint until the refetch; accepted.
- **One owner for the render offset:** `Anchor::render_offset(range, row_start, row_end, line)`
  (R4; the store holds the range) computes it from the row's byte bounds and line text (start in the row → `Suffix` renders here; end in the
  row → `Prefix` renders here), with no `offset_to_point` per hit. The `RowLayout` row filter,
  `remove_inlay`, `inlay_at` and the `InlayInsert` offset all use it.
- **Fallback:** when there is no anchor on the same line (a `Suffix` hint at column 0; a `Prefix`
  hint at line end or EOF), the hint is a zero-width `Keep` range (not anchored) with
  `GrowsOnlyAfter` (`Suffix`) or `NeverGrows` (`Prefix`). No stored range is ever
  collapsed-and-`Drop`, so the windowed/naive equivalence holds (checked in critique round 1: an
  anchored range is non-empty at install and only collapses when an edit touches it, which is always
  in the windowed mover's middle band, decorations.rs:883-899).
- The oracle test grows a hint case; a new mover table test pins every row above, plus "Enter at
  EOL keeps the hint on its line" and "backspace inside the anchor word keeps the hint".

**D4 — Replace, don't merge.** `Document::set_inlays(revision, Vec<inlay::Placed>) -> inlay::Outcome`
replaces the whole store when `revision == doc.revision()`, else returns `Stale` and changes nothing
(the `set_diagnostics` shape). `clear_inlays()` empties it. `inlays_revision() -> Option<Revision>`
is the revision the set was installed at (for D14). `remove_inlay(key, offset)` removes one hint by
key at its current render offset (O(log n + hits at the offset)); D19 calls it at the current
revision, so the offset is exact.
- **Mixed sides at one fetch offset are normalised to `Suffix`.** The LSP spec says hints at the same
  position "are shown in the order they appear in the response", and the visually right order is
  `L ‹suffix›‹prefix› R`, which no single caret split can keep. So `set_inlays` groups by fetch
  offset; if a group has both sides, every hint in it is re-anchored as `Suffix` (typed text then
  lands before all of them). Zed does the same (`normalize_hint_biases`, editor/src/inlays/
  inlay_hints.rs:1128-1180; `test_colocated_mixed_kind_hints_share_bias` with rust-analyzer's
  `|| -> fn()<fn-item-to-fn-pointer>f`).
- Installing keeps server order: ids are minted in server order **before** grouping and
  normalisation, and `Anchor` stores the server index. The store itself orders by `(start, id)`
  (decorations.rs:985-991), which is not render order (a `Suffix` range starts at its word start),
  so `RowLayout` sorts the row's hits by `(render column, Prefix before Suffix, server index)`.
- **Answers are clipped to the request span.** The spec doesn't forbid hints outside the range; the
  client drops entries outside the span (± one line) before conversion (D16), so the store stays
  bounded by the window (a few hundred hints whatever the file size), which the per-row query
  costs rely on.
- **Anchors never leave the inlay store.** No public API returns an `InlayHint(Anchor)`:
  `inlays_in` returns a dedicated `inlay::Shown { key, offset, … }` view, so a host can't copy a
  collapsed `Drop` range into the bulk store. `add_decoration`, `add_sorted_batch` and
  `splice_sorted_batch` `debug_assert!` against the `InlayHint` kind (no early return: it is
  unreachable from outside, and returning would need a fake `DecorationId`).

### Projection (scrive-core)

**D5 — One view bundles everything a geometry projection needs.** `row_layout::Rows<'_>` holds
`Ref<FoldMap>`, `&Buffer`, `&DecorationStore` (the inlay store) and `tab`, and owns `layout(row)`,
`header(row)`, `position(offset, Edge)`, `hit(row, cell, Bias)` and `inlay_at(row, cell)`, plus
`folds()` for the FoldMap-only helpers (`skip_fold_*`, `line_end_folded`, `caret_one_display_row`)
and the widget's row iteration.
- **Enforced:** `FoldMap::{row_layout, header_layout, hit_row}` become `pub(crate)` (rides 0.4.0), so
  `Rows::folds()` can hand out `&FoldMap` without letting anyone build a layout that forgets hints.
  `move_selections` stays public; its `&Rows` comes from `Document::rows()`.
- **Per-frame memo:** one frame builds the same row's layout in the text pass, the bracket pass,
  `max_line_px`, washes, squiggles and carets (editor.rs:967-969, 1605, 1679, 1755-1756, 2990-3001,
  1843). `Rows` memoises built layouts per row in a `RefCell` map of `Rc<RowLayout<'a>>`
  for its lifetime and returns `Rc` clones, holding each `borrow_mut` only for the insert; handing
  out a `Ref` into the cache would panic with `BorrowMutError` when a caller projects another row
  while holding it (`draw_wash_row`, a header tail). `HeaderLayout` holds `Rc<RowLayout>` instead of
  owning `head` (row_layout.rs:323), so header and head share one build. The memo can't go stale:
  `Rows` borrows the buffer and store and holds the fold `Ref`, and `RowLayout::new` copies folds
  out (row_layout.rs:194-209). The widget takes one `Rows` per draw and **passes it down** to the
  helpers that build their own `fold_map()` today: `draw_selection` (2939), `hit_test` (3072),
  `collapsed_chip_at` (3225), `armed_boxes` (922), `max_line_px` (962). `Hint` stores its width at construction; the row query uses a
  borrowing visitor, not the owned `decorations_in` with an `Arc` clone per hit
  (decorations.rs:570-575).
- `Document::rows()` is the public entry for the widget. Inside core, a crate-private
  `Rows::new(Ref<FoldMap>, &Buffer, &DecorationStore, tab)` is built from **disjoint field borrows**,
  so `move_carets`, `add_caret_vertical` and the column paths compute their new ranges through
  `Rows` and assign `self.selections` after the view drops (`ensure_fold_map()` runs before the
  `Ref` is taken; `column_select` gets ranges back from `caret_corner(&self)` instead of
  `rebuild_column_box(&mut self)` writing them). The movement.rs:515 test, which builds an owned
  `FoldMap`, wraps it in a `RefCell` to call `Rows::new`. Those three paths switch to the cached
  fold map instead of a per-press `FoldMap::new` (removes an O(folds) rebuild per press).
- **Visibility probes stay on `FoldMap`.** `toggle_fold_opener`, `unfold_to_reveal` and
  `expand_folds_touched` (which probes a hypothetical `FoldSet` inside `rebase_views`, where the
  inlay store is borrowed mutably) ask "does this offset render?", which hints never change. They use
  a hint-free `FoldMap::renders(buffer, offset, tab) -> bool` (the current `display_position`
  narrowed and made crate-visible). The `DISPLAY_POSITION_PROBES` canary counts in both `renders`
  and `Rows::position`, so the test at document.rs:4428-4465 keeps seeing every probe on the commit
  path.
- So: the 23 geometry call sites move to `Rows` (editor.rs ×17; document.rs:907, 914, 1038, 1039 in
  `caret_corner` / `rebuild_column_box`; movement.rs:207, 224 in `vertical_by`); `move_selections` takes `&Rows` instead of `(buffer, folds, tab)`. A geometry
  projection can no longer forget hints (`grab-bag-signature` → `proof-bundle`); visibility probes
  never needed them.

**D6 — Projections name the edge they want.** `row_layout::Edge { Start, End, Caret }` for offset `p`:

- `Start`: after every hint at `p`. Glyphs, bracket colours and boxes, the start of any range (washes,
  squiggles), popup anchors at a word start.
- `End`: before every hint at `p`. The end of any range.
- `Caret`: after the `Prefix` hints at `p`, before the `Suffix` hints — exactly where the next typed
  character lands under D3. One canonical position per offset (Zed's rule); no caret affinity state.
- At install every offset holds one side only (D4's normalisation), so hints render in server order
  and `Caret` sits before or after the whole group. Mixed groups can still form **after edits**
  (two hints moved onto one offset); then the render order is Prefix hints, then Suffix hints, each
  in server order, which is what the mover produces for text typed between them, with the caret
  between the groups. The next fetch repairs it.
- An **empty** selection's caret uses `Caret`. A non-empty selection's caret renders at its wash edge
  (`End` when the head is the end, `Start` when reversed), so it never floats beyond an excluded
  boundary hint (Zed's `SelectionLayout`).
- An **empty range** (a zero-width diagnostic, an empty find match) uses one edge, `Caret`, for both
  ends; `Start..End` would be inverted.
- Consequence: hints strictly inside a range are washed and underlined; boundary hints are not. On a
  multi-row range the interior row's end is `Start` at the line end (after end-of-line hints).

**D7 — Layout math.** `RowLayout` gains a sorted per-row hint list `{ col, raw_cell, width, side }`
from a windowed store query (`O(log n + hits)`), next to the inline-fold descent.
- `display_cell(col, edge)` adds the widths of hints before `col`, plus those at `col` selected by
  the edge. `caret_cell(col)` uses `Edge::Caret`.
- `hit(cell, bias)` maps any cell on a hint (label or padding) to the hint's offset; the caret then
  renders on the canonical side. `inlay_at(cell)` returns an `inlay::At` (R3):
  `Label { key, part, offset, link, insert, cells }` under a label part, `Padding { key, offset }`
  over padding. Padding is inert: it is editor background (the spec), so it is not a hover or link
  target and doesn't fall through to the word either (Zed, inlay_hints.rs:686-729). A click on
  padding still maps to the hint's offset through `hit`.
- `RowLayout::inlays()` yields each laid-out hint as `&row_layout::Inlay { key, offset, cell,
  width, padding, hint }` for painting (R2).
- `width()` includes hints at the line end, so `max_line_px` can reach them.
- `is_plain()` is false on a row with hints.
- Tabs after a hint keep their buffer-space width (the fixed-shift model), as they do after a chip.
- **Folds:** a hint whose render offset is inside a collapsed inline fold's hidden interior
  (`open < o ≤ close`) is not laid out. Hints on block-folded rows, including the collapsed header's
  tail, are not shown. Hints on the header row itself are; `HeaderLayout::head_cells` / `tail_cell`
  derive from the hinted head `RowLayout`, so the gap and tail shift past them (tested).
- **Box selection** steps Left/Right by character within the line's content and re-projects with
  `Caret`, so it doesn't stall for `width` presses on a hint; past the line end it keeps stepping
  by cell (`c.cell + 1`, document.rs:1019), so a box still reaches past short lines.
- **Identity:** with no hints, every projection returns exactly what it does today. A test sweeps a
  fixture document through every edge and compares with the pre-refactor projection.

### Rendering and gestures (scrive-iced)

**D8 — Hints are drawn in the code font, at the code size, on the cell grid.** A smaller font would
break the whole-cell grid (matcha's 0.75 scale does not transfer). The label is one `draw_line` per
part in a dimmed text colour on a pill spanning the label cells; padding cells stay editor background
(the LSP spec's rule). Rows with hints take the inline path; spans are split at hint columns.
The tab bug is fixed first, in its own commit, because the split changes the same code. The fix
takes each run's tab phase from `display_map::expand(line, start_col)`, the raw cell before hints and
chips, not from the display cell, so tabs after a hint keep their buffer-space width (D7). On selected
rows the pill is suppressed the way chips are (editor.rs:3127-3138), so it doesn't island inside the
wash. The test is **strict interior** (`a < p < b`, D6), not the chips' span test (editor.rs:3134),
which would suppress unwashed boundary hints.

**D9 — Every projection site picks an edge** (the table in "Files touched"). Empty-selection carets
and autoscroll use `Caret`; washes and squiggles `Start..End` (one edge for empty ranges, `Start` at
the line end of interior rows); glyphs, brackets, collapsible boxes, chip pills and word-start popups
`Start`; the signature box `Caret`; `max_line_px` uses `width()`. The sites pick their edge in
Phase 2, when they move to `Rows` anyway; with no hints every edge is the same cell.

**D10 — Gestures.** Hint hover, like word hover, arms only while the widget is focused
(editor.rs:2573); clicks work either way.
- **Hover:** the idle timer checks the collapsed chip, then `inlay_at`, then the word. A hint hit
  publishes `Action::InlayHover { key, part }` (R7) instead of `HoverQuery`. The tooltip card has
  a keyed identity (`HoverTarget::Inlay { key, part }` beside today's range): `still_in` holds while
  the pointer stays on the same hint part or on the card, and the card anchors **on** the part's
  cells, not at `popup_anchor`'s `Start` edge. `HoverDismiss` retires both.
- **Ctrl+click** on a link part publishes `Action::InlayJump { key, part }` and captures the event.
  While Ctrl is held over a link part it is underlined and the cursor is a pointer, unless a
  non-empty drag selection is pending (Zed's `!has_pending_nonempty_selection()` guard,
  inlay_hints.rs:792-810), so a Ctrl-drag neither underlines nor captures. The link test runs
  before the (compiled-out) Ctrl collapse affordance (editor.rs:2111), should that ever be enabled.
- **Double-click** on an insertable hint publishes `Action::InlayInsert { key, offset }` instead of
  selecting a word. The press on a hint still goes through `mouse::Click::new`, so the second press
  counts as a double; a single click places the caret at the hint's offset (the plain path).
- Hint text is never a word for double-click, find or Ctrl+D (it is not in the buffer).

### The request seam (CodeEditor)

**D11 — The editor owns scheduling.** `CodeEditor` keeps `inlays: Inlays { enabled, wait:
Option<Wait>, window: Option<Range<u32>> }` (the window in buffer rows).
- **Window:** the visible rows padded by one viewport height above and two below (minimum 50 rows;
  Helix's shape, commands/lsp.rs:1362-1372), so chains that start above the visible top still get
  their hints (the probe's range finding) and scrolling down, the common direction, stays inside the
  window longer. Sent as a byte span.
- **Triggers** (all ignored while disabled, including `InlayRefresh`): enabling, `open_lsp` and
  `set_inlay_hints(true)` → wait 0; an edit → wait 300 ms (trailing; restarts); `InlayRefresh` → the
  same as an edit; `ViewportChanged` whose rows leave the requested window's inner half → wait 75 ms,
  **trailing** (restarts on each event) with a 300 ms max-wait, so a scrollbar drag doesn't
  supersede-and-cancel every request before it answers; `load` → clear the store (D13), then wait 0.
- **Delays, not deadlines.** `apply_lsp` takes no `now` (lsp.rs:121-129) and `now_ms` is stamped only
  in `update` (code_editor.rs:751-753). An absolute `due = now_ms + 300` set from `InlayRefresh` in an
  editor idle for minutes is already past, and rust-analyzer's refresh after every `didChange`
  would make every visible editor fetch on every keystroke. So `CodeEditor` keeps a **pending wait**
  `{ generation, delay, cap }`; each trigger bumps the generation.
- **Timer:** no frame subscription. A general widget API, documented and public:
  `Editor::wake_after(Option<Wake>)` with `Wake { generation, delay, cap: Option<Duration> }`, and
  `Action::Wake(u64)`. Only `ViewportChanged` sets a cap; edits and refreshes don't, so an edit
  during a drag doesn't fire early. On `RedrawRequested(now)` the widget **first** restamps when the
  generation is new, `at = now + delay`, capped at `first_seen + cap`, from its own clock (the
  `hover_rearm` pattern, editor.rs:2570-2579), **then** checks `now ≥ at`; it calls
  `shell.request_redraw_at(at)` and on the redraw that crosses it publishes
  `Action::Wake(generation)` once. `first_seen` survives generation changes and resets only when
  the widget publishes a `Wake` or receives `wake_after(None)`, so a drag that bumps the generation
  per event still hits the max-wait. The wake fields are reset by `diff` when the document changes
  (editor.rs:1217-1231), not preserved like `focus`. It arms outside the hover code's `is_focused()` gate
  (editor.rs:2573). `update` records `inlay::Request { ticket, span }` when the generation matches
  the pending one. The host's `sync_lsp` after `update` sends it on the same message. `Wake` is not
  inlay-specific; find could use it for a real debounce later (today's is a rate limit,
  find.rs:754-763).
- **Limits, documented:** a widget that isn't in the view tree (a background tab in the scripted
  example, examples/lsp/main.rs:237-247) never wakes, so its fetch waits until it is shown; the
  first draw re-arms it. Likewise a minimised or occluded window may get no `RedrawRequested`
  (winit), so the fetch waits until the window is visible.
- Tests: an `InlayRefresh` applied to an editor idle for 10 s fetches 300 ms later, not on the next
  frame; a 1-second scroll drag produces about 3 fetches, not 0 and not one per event.
- Zed uses 700/50 ms, Helix 250 ms idle. 300 ms keeps hints close to the text without firing on every
  keystroke.

**D12 — Slots and answers follow D11 of the LSP plan.** New `Awaiting` fields: `inlays: Option<Ticket>`,
`inlay_tooltip: Option<(Ticket, Key, u32)>` (R6), `inlay_insert: Option<(Ticket, Key, u32)>`.
Label jumps reuse `awaiting.definition`, so `Local`, `Open` and `Unopened` targets and `Applied.jump`
work unchanged. `abandon` gains the new kinds. Abandon table additions: disabling hints and
`close_lsp` clear all three; `HoverDismiss` and `ViewportChanged` clear `inlay_tooltip`; an edit
clears `inlay_tooltip` and `inlay_insert` (an edit moved the revision anyway).
- `take_inlay_request() -> Option<inlay::Request>`, `take_inlay_interaction() -> Option<inlay::Interaction>`
  (one slot; a newer gesture supersedes).
- `set_inlays(ticket, Option<Vec<inlay::Placed>>)`: `Some` replaces (`Some(vec![])` clears); `None` settles the
  slot and keeps what is shown (a failed fetch must not blank the hints).
- `set_inlay_tooltip(ticket, Option<String>)` shows the markdown as a card anchored at the hint part.
- The three `Inlay*` actions (`InlayHover`, `InlayJump`, `InlayInsert`) and `Wake` are handled in
  `update` **before** the catch-all `apply` (code_editor.rs:913-916), as `GotoDefinition` and
  `TriggerCompletion` already are. `apply` always ends in `after_edit(CaretOrClose)`
  (code_editor.rs:1524, 1537-1558), which closes the completion popup, clears `self.hover` and
  abandons `Hover` and `Definition`: a falling-through `Wake` would close completion 300 ms
  after every typing pause, and `InlayHover` / `InlayJump` would lose their slots. In `apply`'s
  exhaustive no-op list they are unreachable.
- Phase 5 reads every new `Awaiting` field in non-test code, so none is write-only under
  `-D warnings` (DISPATCH forbids `#[allow(dead_code)]`): `accepts` (code_editor.rs:1792-1800) gets
  an arm per new `Awaited` variant, and the edit and abandon paths construct them.

**D13 — Toggle.** `CodeEditor::inlay_hints(bool)` (builder, default off, like `rename`) and
`set_inlay_hints(&mut self, bool)` (runtime). Off clears the store, abandons the slots, closes a
showing inlay tooltip card, and clears the pending wait. There is no built-in key; the examples bind one. `CodeEditor::load` replaces the whole buffer,
and the mover would keep hints at their old relative offsets (patch.rs:206-211), so `load` calls
`clear_inlays` too.

**D14 — Interactions only on a current set.** Hints move with edits (the interview's choice), but
the host's copy of their tooltips, locations and edits is only valid at the revision it was fetched
for. The editor records `Interaction`s only while `doc.inlays_revision() == Some(doc.revision())`.
On a moved set, gestures on hint cells do nothing: no hover (not the neighbouring word's), no jump
(Ctrl+click is consumed), and double-click places the caret at the hint's offset without selecting.

### The client (scrive-lsp)

**D15 — Capabilities.** Advertise `textDocument.inlayHint { dynamicRegistration: false,
resolveSupport: { properties: ["tooltip", "label.tooltip"] } }` and
`workspace.inlayHint.refreshSupport: true`. Read `inlay_hint_provider` into
`capabilities::Server.inlay: Option<Resolve>` (`OneOf::Left(true)` or options; `resolve_provider`).
Only tooltips are lazy: locations and text edits come inline (37% more bytes, no round trips, no
stale-resolve trap), and the resolve request serves the tooltip requirement.

**D16 — Fetch.** `Client::inlays(&Snapshot, &inlay::Request) -> Output`: the usual tracked /
revision / capability gates; a decline answers `Change::Inlays(Some(vec![]))` under the ticket; an answer is
  `Change::Inlays(Some(Vec<inlay::Placed>))`.
- `Kind::Inlays` is an intel kind (not a command): a failure answers `Inlays(None)`; ContentModified
  re-issues once (the existing `reissue`), which is what gets rust-analyzer's first hints.
- A `null` result (allowed by the spec) decodes to `Change::Inlays(Some(vec![]))` and clears, not
  `None`; Lapce keeps stale hints on `null`.
- Decoding is **per entry**: the result decodes as `Vec<serde_json::Value>`, and each entry decodes
  alone, so one malformed hint (a bad location URI) doesn't lose the set (the workspace.rs rule).
- Entries outside the request span (± one line) are dropped before conversion (D4).
- Conversion against the request snapshot: positions through `Encoding::offset`; hints on a line at
  or past `line_count` are dropped; kinds `1 → Type`, `2 → Parameter`, else `Other`; a part is a
  `Link::Jumps` iff it has a location; `Insert::Available` iff `text_edits` is non-empty; `Other`
  hints get their placement from the padding rule (D1) on the raw flags, passed with `.placement`
  (R5). A padding flag is cleared when the label's first
  (last) part already starts (ends) with whitespace, so D1's width doesn't double-space (Zed,
  editor/src/inlays.rs:60-75). Keys are stable across refetches: a hint with the same position,
  kind and label as one in the previous set keeps its key (matched as an ordered multiset, so
  repeated identical hints at one offset keep their keys in order; only when both sets are at the
  same revision), so an open tooltip card
  (`HoverTarget::Inlay { key, part }`) survives a refresh. New keys come from a client
  counter.
- The client keeps, per document, the last set: its revision, the request snapshot, every open
  document's synced revision at send time, and the raw hints by key. `send` records `revisions` for
  `Kind::Inlays` as it does for `Definition | Rename` (client.rs:953-960). The set is replaced on
  each answer and dropped on `close`.
- **Not running yet.** Hosts open documents before `initialize` finishes, so the first request
  declines. `initialized` emits one `Change::InlayRefresh` per tracked document when the server has
  an inlay provider (D17's fan-out), which re-arms the editors. A server without refresh support
  therefore still shows hints at startup.

**D17 — Refresh reaches the editors.** `workspace/inlayHint/refresh` gets a dedicated `respond` arm
(still answered `null`) and `answer` returns one `Update::Document { stamp: Revision(synced),
change: Change::InlayRefresh }` per open document. The editor treats it as an edit trigger (D11)
and never refuses it as stale. The generic refresh arm keeps answering the other refreshes.

**D18 — Interactions.** `Client::interact(&Snapshot, &inlay::Interaction) -> Output`, gated on
`ticket.revision() == synced == set revision` and a known key; otherwise it declines with the empty
answer for the action. A declined insert answers `Change::Edits(vec![])`, and D19 removes the hint
only when the ops are non-empty.
- **Foreign targets in `send`.** `send` and `Query::request` always target the requester's URI and
  snapshot (client.rs:963-970, 1441-1449), and `hovered` converts against `entry.request_snapshot`
  (1049-1055). New `Query` variants carry what they need: `Resolve` carries the raw hint JSON;
  `LocationHover` carries the target URI and the raw LSP position, converts no range for a foreign
  URI, and applies `revision_of`'s stale check for an open target (as `target` does at 1095). Both
  get arms in `reissue`, `kind`, `method` and `caret`.
- **Tooltip** → `Change::InlayTooltip(Option<String>)`:
  1. a known tooltip (the part's if a part is hovered, else the hint's) → answer at once, lowered with
     the hover card's markdown subset;
  2. else, if the server resolves and the hint has `data` and is unresolved → `inlayHint/resolve`
     (`Kind::Tooltip`); the reply only **adds tooltips** to the stored hint, then step 1 or 3 runs.
     The fetched label owns the parts: a resolve may restructure the label (Zed's own test turns a
     string label into five parts, hover_popover.rs:2650-2700), so part tooltips are taken from the
     resolved hint only when its part count and texts match, else only its hint-level tooltip is
     used. A resolve never changes links or text edits after install;
  3. else, if the hovered part has a location → `textDocument/hover` at that location (any URI; the
     spec says a part's location drives its hover), converted with the location's own text or
     positions;
  4. else `None`.
  `Kind::Tooltip` is an intel kind: failures answer `None`.
- **Jump** → `Change::Definition(target)`, no round trip. `target` is refactored to take
  `(requester, &Snapshot, &[(uri::Key, Revision)])` instead of `&Pending` (client.rs:1076-1103); the
  definition path passes its pending entry's fields, the jump passes the stored set's. So `Local`,
  `Open` (with the stale check against the other document's recorded revision) and `Unopened` all
  work.
- **Insert** → `Change::Edits(hygiene(text_edits))` stamped with the ticket, no round trip.

### Glue

**D19 — `sync_lsp` pulls the new requests and surfaces jumps; `land` gets three arms.**
- `sync_lsp` returns `update::Applied` (messages and jump; `refused` stays `None`) instead of the
  bare messages (breaking, rides 0.4.0); a second type would be `Applied` minus one field.
  `update::Change` stays exhaustive: `land` is in another crate, and the compile error on a missing
  arm is what keeps every new `Change` handled (round 1's B1). `route` keeps the `Applied.jump` of a local answer instead of
  asserting it is `None` (lsp.rs:192-199): today's local answers never jump, an inlay label jump
  does, and 25 of 61 linked parts in the probe point into unopened files. Hosts handle the jump as
  they do any `Applied.jump`.
- `land` arms: `Inlays` (ticket gate → `set_inlays`), `InlayTooltip` (ticket gate →
  `set_inlay_tooltip`), `InlayRefresh` (schedule; never `Stale`).
- **Insert removes its hint first.** In the `Edits` arm, when the ticket equals
  `awaiting.inlay_insert` and the ops are non-empty, `land` takes the slot and calls `remove_inlay(key, offset)` **before**
  `try_edit`. Afterwards is too late: `try_edit` runs `after_edit`, which clears the slot
  (code_editor.rs:546-553), and D3 would keep the hint after the inserted text (`let x: i32: i32`).
  The arm's revision check (lsp.rs:286) runs first, so the hint's offset is exact. If `try_edit`
  fails (`Overlap`), the hint stays removed until the refetch the next edit triggers; accepted, the
  edits come from one hint's `textEdits`, which `hygiene` already orders.
- A local answer that lands an edit leaves a `didChange` unsent, and requests its `after_edit`
  records (a signature re-query, code_editor.rs:1665-1685) unsent too. `sync_lsp` re-runs the whole
  sync-and-pull loop until the revision stops moving. It ends after at most one editing pass:
  interactions come only from gestures and the first pass takes the single slot, and the inlay
  request a landed edit schedules is not due for 300 ms, so the next pass sees a stable revision.
- **Jump merging.** `apply_lsp` sets `applied.messages = self.sync_lsp(client)` (lsp.rs:126-127),
  so a landed answer and the sync can each yield a jump: the landed answer's jump wins, else the
  sync's. `save_lsp` and `jump` also return `sync_lsp`'s output and change with it.
- `close_lsp` clears hints and abandons the slots.

## Constraints

- RUST_STYLE: no `mod.rs`; one semantic type per module; module paths over composite names for new
  types (`inlay::Hint`, `inlay::Request`, `row_layout::Edge`, `row_layout::Rows`); no aliased imports;
  typed errors; `expect` over `unwrap` in library code; tests in-file with sentence names.
- OPAQUE: `Hint`, `Part`, `Key`, `Placed`, `Anchor`, `Request` and `Interaction` have private fields and
  constructors; `Hint::new` rejects an empty label (`smart-constructor-newtype`); no raw `bool`
  parameters (`Placement`, `Side`, `Insert`, `Link` enums); `Rows` is a `proof-bundle`.
- scrive-core stays headless: no iced, no lsp-types, no I/O, no clocks.
- scrive-lsp stays pure: no `std::time`, threads or I/O; every returned message goes through
  `#[must_use] Output`.
- Performance: a single edit moves hints in `O(window + log n)`; no store sort per keystroke; per-row
  queries are windowed; hints never enter the `FoldMap` or its cache key; the draw budget holds.
- `#![deny(missing_docs)]`, `#![forbid(unsafe_code)]`; comments per `/commit-and-comment` (DISPATCH.md
  override 1); every `#[allow]` justified.
- No new dependencies (lsp-types 0.97 has every inlay type).
- The code is not rustfmt-clean; never run `cargo fmt`.

## Phases

Each phase builds, passes clippy (`-D warnings`, both feature sets once `lsp` code changes) and tests
on its own, and ends in 1-4 commits built by `commit-phase.sh`.

### Phase 1 — core: the hint model and its anchored store
`intel::inlay` (`Hint`, `Part`, `Padding`, `Kind`, `Placement`, `Side`, `Insert`, `Link`, `Key`, `Anchor`,
`Placed`, `Outcome`, `Error`), `intel::inlay::request::Request`, `intel::inlay::interaction::Interaction`;
`DecorationKind::InlayHint(Anchor)` with an explicit `empty_policy` arm; the dedicated store and its
wiring (D2); word anchoring, `Placement::Auto` resolution and the row clamp's data (D3, D1);
same-offset normalisation (D4); `set_inlays` /
`clear_inlays` / `remove_inlay` / `inlays_revision` / `inlays_in` (D4); the borrowing row query
`DecorationStore::visit_in` with the `filter_visit` lifetime (D5, R8). Tests: the D3 table (incl.
Enter at EOL, backspace inside the anchor word), undo/redo (a dropped hint stays dropped), the oracle
case, sanitising, sort order, `Auto` resolution (Zed's lifetime cases), the mixed-offset `||`
closure case renders `|| -> fn()<fn-item-to-fn-pointer>f` and typing gives `||X -> fn()<…>f`;
paste over a selection, a line-replacing edit and retyping a selected word drop the hints anchored
there; `add_decoration` rejects the `InlayHint` kind; ids follow server order; perf-gate cells (typing next to N hints: Constant; installing N:
Linear, since there is no O(n log n) budget and `set_sorted` charges `len`; `DECORATION_SORTS` on the
inlay store stays 0 per keystroke).

### Phase 2 — core + iced: the `Rows` view and `Edge` (no behaviour change)
Add `row_layout::Rows`, `Document::rows()`, the crate-private `Rows::new` from disjoint borrows,
`Rows::folds()`, `row_layout::Edge`, and `FoldMap::renders` (D5). Move the 23 geometry call sites onto
`Rows`, each picking its D9 edge now (with no hints every edge gives the same cell); move the three
visibility probes and the canary onto `renders`; `move_selections` takes `&Rows`; `move_carets`,
`add_caret_vertical`, `column_select` and `column_drag` use the cached fold map. Tests that called
`display_position` / `header_layout` directly (document.rs:2872, 3084; editor.rs:4571, 4626;
movement.rs:515) switch to `Rows` with the same expectations; nothing else in the suite changes.
Intra-doc links to the demoted or renamed methods move to the `Rows` methods, or the
`-D warnings` doc build fails: row_layout.rs:68 (`DisplayPosition` → `FoldMap::display_position`),
row_layout.rs:177 (`RowLayout` → `FoldMap::row_layout`), fold_map.rs:849 (`entry_edge_if_hidden` →
`Self::display_position`), and the private ones at editor.rs:798, 3068. Draw-path helpers take one
`&Rows` passed down from `draw`.
New: a smoke test per `Rows` method.

### Phase 3 — core: hint-aware layout
Hint spans in `RowLayout` (D6, D7) with the row clamp; edges take effect (`Rows::position`
honours its edge, `Rows::hit` runs on the memo); `hit`; `inlay_at` returning `inlay::At` (R3);
`RowLayout::inlays()` for painting (R2); `width`; fold hiding; the header dependency; vertical motion through `Caret`; box selection stepping
by character. Core-only: the signatures already exist from Phase 2. Tests: the identity sweep, each
edge at shared offsets (both groups), empty ranges, hit on label and padding, EOL width, hints inside
and at the edges of inline folds, block-folded rows, header gap and tail shift past a header hint,
Up/Down goal columns across hints, a hint between two word characters is laid out (no render
guard, R11), `render_offset` agreeing across the row
filter / `inlay_at` / `remove_inlay`, the per-frame memo builds each row once, box selection across a hint, `DISPLAY_POSITION_PROBES` unchanged.

### Phase 4 — iced: painting
Commit 1: the `expand_tabs` fix with a regression test. Then: rows with hints on the inline path,
labels and pills with selected-row suppression (D8), `max_line_px` from `width()`. Headless tests
through `Document::set_inlays`: caret x, wash extents (incl. interior-row ends), squiggle extents
(incl. zero-width), bracket box, max scroll with an EOL hint, a click on a hint places the caret at
its offset.

### Phase 5 — iced: gestures, the seam and the toggle
`Action::{InlayHover, InlayJump, InlayInsert, Wake}` and `Editor::wake_after` (D10, D11); the keyed tooltip card; the
`CodeEditor` toggle and `load` clearing (D13), the scheduler with `request_redraw_at` (D11), slots,
`take_*` / `set_*` (D12), `pending_wake()` (R10), the D14 gate. Tests: pump/rest_on hover tests over a hint, Ctrl+click,
double-click vs single click, stale-set gestures do nothing, a `Wake` keeps the
completion popup open, a refresh to an editor idle for 10 s fetches 300 ms later, a scroll drag
fetches at the max-wait and not per event, a folded viewport pads in display rows (R20), keys stay stable across a refetch (the tooltip card stays
open), debounce timing on a fake clock (one
wake per trigger), viewport re-request, abandon table rows.

### Phase 6 — scrive-lsp: fetch, refresh, interactions (+ the minimal `land` arms)
Capabilities (D15); `Kind::Inlays` and `Kind::Tooltip`; `revisions` recorded for `Inlays`; the
`target` refactor; `Client::inlays` and `Client::interact` (D16, D18); `null` results and padding collapse; clipping to the request span; stable keys; `Resolve` /
`LocationHover` query variants; refresh updates and the
post-`initialized` refresh (D16, D17); the per-document set. Because `update::Change` is exhaustive
and `land` matches it, this phase also adds the three `land` arms in scrive-iced (ticket gate →
`set_inlays` / `set_inlay_tooltip`; `InlayRefresh` → schedule), so `--all-features` builds. Tests:
capability JSON, conversion (UTF-16 positions, out-of-range lines, sort, ties, sanitising via core,
the `Other` side heuristic), per-entry decode, ContentModified re-issue, supersede, refresh fan-out,
refresh after `initialized` for a server without refresh support, resolve then tooltip, a resolve that restructures the label keeps the fetched parts, stale
resolve guard, location hover for an unopened URI, jump targets for local / other open (incl. stale)
/ unopened, insert edits through hygiene.

### Phase 7 — glue, examples, docs
`sync_lsp` pulls requests and interactions, returns `update::Applied`, and loops sync-and-pull
until the revision stops moving; `apply_lsp`, `save_lsp` and `jump` adopt it (landed jump wins); `route` keeps jumps; the insert path removes its hint before `try_edit` and a declined
(empty) insert settles its slot without `try_edit` (R16); `open_lsp` schedules the wait-0 fetch
and `close_lsp` also clears the pending wait (D19, R19). Hosts and examples handle the jump from `sync_lsp`'s `Applied`. The scripted `lsp` example gains a hint script (type,
parameter, a linked part into another file, an insertable hint, a tooltip through resolve) and a
toggle; the `rust_analyzer` example enables hints with a toggle key; README and crate docs; headless
example tests, including an `Unopened` label jump through `sync_lsp` and "insert leaves no
duplicate hint".

## Execution order

Strictly 1 → 7. Phase 6 depends on Phase 1's types and on Phase 5's `set_inlays` /
`set_inlay_tooltip` / scheduler (its `land` arms call them), so it can't move before Phase 5.

## Testing strategy

- **Regression:** the full suite after every commit (Phase 2 only rewrites the tests that called the
  moved methods, with identical expectations).
- **Identity:** no hints ⇒ every `Rows` projection equals the pre-hint projection (Phase 3).
- **Mover:** the D3 table; the windowed/naive oracle with hint ranges; undo/redo round trips.
- **Scale:** perf-gate cells and canaries (Phase 1, 3).
- **Widget:** headless projection and gesture tests with the existing `pump`/`rest_on` helpers.
- **Client:** scripted JSON-RPC exchanges, as in client/tests.rs.
- **Real server:** an `#[ignore]` rust-analyzer test in the example: hints arrive after load, move
  with an edit, refresh after the edit, an insert applies `: i32` without a duplicate.

## Risks

- **Two-sided offsets.** The Edge split touches every projection; a site on the wrong edge paints a
  wash or caret one hint off. Mitigation: the Phase 4 test per site, the identity sweep.
- **Refactor breadth.** Phase 2 moves 23 geometry call sites and 3 probes, and reshapes four
  selection paths around disjoint borrows. Mitigation: no behaviour change allowed, the suite is the
  oracle.
- **Anchor heuristics.** D3 misplaces a hint for some edits (typing a new argument before an existing
  one). The next fetch repairs it within ~300 ms plus the round trip.
- **Refresh fan-out.** rust-analyzer sends `workspace/inlayHint/refresh` after every `didChange`, so
  every open editor re-fetches after each typing pause, not only the one being typed in. Accepted:
  warm whole-document requests take 14-20 ms and ranged ones 2.5-7 ms.
- **Server variety.** Servers other than rust-analyzer may defer locations or edits despite the
  advertised list; the client then treats the part as not linkable / the hint as not insertable. The
  `Other` side heuristic may misplace a prefix hint without right padding.
- **Scroll-back gap.** Each windowed answer replaces the whole store, so rows outside the new window
  lose their hints; scrolling back shows bare rows until the 75 ms re-request answers. Helix behaves
  the same; Zed avoids it with a 50-row chunk cache. Accepted; the asymmetric window softens it.
- **Location hover on unopened files** depends on the server accepting hovers for documents it was
  never sent; on error the tooltip is simply empty.

## Out of scope

- Label-part `command`s (need `workspace/executeCommand`; rust-analyzer never sends them).
- A refresh when a server's work-done progress ends (Zed's `mark_refresh_pending_on_work_end`, for
  servers that answer `[]` while indexing and never send a refresh). scrive-lsp acknowledges
  `window/workDoneProgress/create` but doesn't track `$/progress`; rust-analyzer is covered by the
  ContentModified re-issue and its refreshes.
- Jumping to the definition of the symbol at a part's location (the spec's wording) instead of the
  location itself. Zed and Lapce jump to the location; for rust-analyzer it is the definition.
- A per-chunk hint cache across scrolling (Zed), and merging same-revision answers outside the
  requested window (the expert's optional fix for the scroll-back gap: `take_matching_in` plus a
  per-kind splice). Revisit if the gap shows in practice.
- Hints on the collapsed header's tail and in the fold preview.
- Per-kind styling, a label length cap (rust-analyzer truncates at 25 by default), hold-modifier
  toggling, multiple servers per editor.
- Mapping a stale set forward through the change log (diagnostics don't either).
- Wide-glyph cell widths (already one cell per scalar everywhere).

## Files touched

| Phase | Files |
|---|---|
| 1 | scrive-core: `intel.rs`, new `intel/inlay.rs`, `intel/inlay/request.rs`, `intel/inlay/interaction.rs`, `decorations.rs`, `sum_tree.rs` (R8), `document.rs`, `lib.rs`, `perf_gate.rs` |
| 2 | scrive-core: `row_layout.rs`, `fold_map.rs`, `document.rs`, `movement.rs`, `lib.rs`, `perf.rs` and `perf_gate.rs` (prose, R22); scrive-iced: `editor.rs` |
| 3 | scrive-core: `row_layout.rs`, `intel/inlay.rs` (`Anchor::{side, index}`, `At`), `document.rs`, `movement.rs` |
| 4 | scrive-iced: `editor.rs`, `geo.rs` |
| 5 | scrive-iced: `editor.rs`, `code_editor.rs`, `lib.rs` (export `Wake`) |
| 6 | scrive-lsp: `client.rs`, `client/capabilities.rs`, `client/tests.rs`, `update.rs`, `hover.rs` (`contents` → `pub(crate)`), new `inlay.rs`, `lib.rs`; scrive-iced: `code_editor/lsp.rs` (the three `land` arms) |
| 7 | scrive-iced: `code_editor/lsp.rs`, `examples/lsp/*`, `examples/rust_analyzer.rs`; READMEs |

Projection sites and their edge (D9):

| Site | Edge |
|---|---|
| empty-selection carets (`offset_xy`), autoscroll, signature anchor, box-drag corner, vertical motion | `Caret` |
| non-empty selection carets | the wash edge at the head (`End` forward, `Start` reversed) |
| selection / occurrence / find / scope washes, squiggles | `Start` … `End`; interior rows end at `Start` of the line end; empty ranges use `Caret` for both |
| glyph runs, bracket colours, matching-bracket box, collapsible box (`xo`/`xc`, editor.rs:909-910), chip pill rects, completion and hover anchors | `Start` |
| inlay tooltip card | the hovered part's own cells |
| row width, `max_line_px` | `width()` |


## Verification

```
cargo test --workspace
cargo test --workspace --all-features
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --all-features
cargo build --workspace --all-targets --all-features --target wasm32-unknown-unknown
```

## FOSS comparison

Step 4, against zed `1399a80`, helix `ba40e54`, lapce `f66ffaa`, floem `1351ffb` (2026-10-01), plus
the earlier notes in `FOSS_NOTES.md`.

**Adopted:** same-offset normalisation to `Suffix` in server order (D4, D6; fixes a Draft 4 bug where
mixed sides rendered prefix-first); Zed's padding-and-word rule for `Other` sides (D1); padding not
a hover target (D7); fetched label owns the parts, resolve adds tooltips only (D18); `null` clears
(D16); padding collapsed when the label already has the space (D16); no link underline during a
Ctrl-drag (D10); triggers ignored while disabled, asymmetric window (D11); scroll-back gap in Risks;
work-done refresh, definition-of-location jumps and chunk caching listed as out of scope.

**Where we differ on purpose:**
- **Anchors (D3).** Zed anchors to the neighbouring character on the *wrong* side for meaning
  (deleting `x` keeps `: i32`; deleting an argument leaves `foo(a: )`); Helix stacks hints; Lapce
  drops on the next char. Ours drops a hint with its token and matches Zed for typing.
- **Row clamp (D3)** exists in none of them.
- **Inline locations and edits (D15).** Zed resolves `label.location` lazily, so Ctrl+click works only
  after a hover, and resolves without a revision gate (the stale-resolve trap).
- **Tooltip on arrival (D18).** Zed shows a resolved tooltip only on the next mouse move; it has no
  location-hover fallback, which the spec asks for.
- **Part hit-testing (D7).** Zed finds the hovered part from an unbiased position, so Parameter-hint
  parts are probably unreachable; ours is row-local through `inlay_at`.
- **Applying `textEdits` (D10, D19)**: none of the three does.
- **Padding as background (D8)** follows the spec; Zed styles padding with the label.

**Confirmed:** D14's gate (Zed drops its hint cache on every edit); the 300/75 ms debounce (Zed 700/50,
Helix 250); `$/cancelRequest` on supersede (Zed cancels on drop); the non-empty selection head snap
(Zed's `SelectionLayout`). Rendering: all three use the code font; Zed's hint background is off by
default. We keep the pill with `range_selected` suppression; any future visible-whitespace pass
must skip hint cells.

## Critique resolution log

### Round 1 (Plan agent, against 8e72665) — 2 blockers, 6 majors, 13 minors; all addressed

Confirmed sound: D3's stickiness rows and the `Caret` edge agree for typing at `p`; the windowed/naive
equivalence holds; undo/redo go through `rebase_views`; ~40 cited lines accurate (client.rs drifted
1-5 lines).

| # | Issue | Resolution |
|---|---|---|
| B1 | Phase 6 adds variants to the exhaustive `Change`, which `land` matches, so scrive-iced breaks until Phase 7 | The three `land` arms move into Phase 6; execution order notes Phase 6 needs Phase 5 |
| B2 | Label jumps answered locally hit `route`'s `debug_assert!(jump.is_none())` and are dropped; `target` needs a `Pending` | `sync_lsp` returns `Synced { messages, jump }`, `route` keeps jumps (D19); `target` takes `(requester, &Snapshot, revisions)`; `send` records revisions for `Inlays` (D16, D18) |
| M1 | `try_edit` → `after_edit` clears `inlay_insert` before D19 checks it; duplicate `: i32` | Remove the hint before `try_edit`, at the current revision with an exact offset; re-sync after a landed local edit (D19, D4) |
| M2 | `rows(&self)` conflicts with `&mut self.selections`; visibility probes can't use a hinted view; tests call the moved methods | Split: `FoldMap::renders` for probes + canary; crate-private `Rows::new` from disjoint borrows; `Rows::folds()`; 19 geometry sites; tests rewritten with identical expectations (D5, Phase 2) |
| M3 | Phase 3 changes signatures used by editor.rs but lists only core files | `Edge` lands in Phase 2; sites pick edges there; Phase 3 stays core-only |
| M4 | Enter at EOL drags a `GrowsOnlyAfter` hint to the next line, right of the caret | Row clamp on the render position, mirrored for `Prefix` (D3) |
| M5 | First fetch declines while initializing and nothing re-arms it | `initialized` fans out `InlayRefresh` when the server has a provider (D16) |
| M6 | Interior-row ends, empty ranges, collapsible box and pills had no edge; D6/D9 caret contradiction | D6 and the D9 table cover them; empty selections use `Caret` |
| m1 | Composite `InlaysOutcome`; one module holding `Request`/`Interaction`; core `Action` clashes; raw `insertable: bool`; stored offset goes stale | `inlay::Outcome`; `inlay/{request,interaction}.rs`; core `Action` dropped; `Insert`/`Link`/`Side` enums + builder; offsets passed beside hints (D1, D4) |
| m2 | Public `InlayHint { anchored: bool }` lets hosts store a collapsed `Drop` range | Opaque `inlay::Anchor`; explicit `empty_policy` arm (D2) |
| m3 | No O(n log n) perf budget | Install gated as Linear (Phase 1) |
| m4 | D14 needs the set's revision | `inlays_revision()` (D4) |
| m5 | One-char anchors drop hints on typo backspace | Word anchors; undo doesn't restore a dropped hint (D3) |
| m6 | `Other` always suffix | `Side` heuristic from padding (D1) |
| m7 | Box selection stalls on a hint | Step by character, re-project with `Caret` (D7) |
| m8 | `window::frames()` redraws for 300 ms per keystroke; background tabs; focus | `request_redraw_at` + `InlayDue` once; limits documented (D11, D10) |
| m9 | Stale-set gestures fall through to word hover; Ctrl+click unspecified; Ctrl collapse order | Hint cells inert on a stale set; link test first (D14, D10) |
| m10 | Pills island on selected rows; tooltip card identity and anchor | `range_selected` suppression (D8); keyed `HoverTarget::Inlay`, anchored on the part (D10) |
| m11 | `load` keeps hints at stale offsets | `load` clears the store (D13) |
| m12 | Refresh fans out to every open editor after each `didChange` | Accepted, in Risks |
| m13 | Header gap/tail depend on the hinted head layout | Stated in D7, tested in Phase 3 |

### Round 2 (same agent, Draft 2) — 0 blockers, 0 majors, 10 minors; all addressed

Confirmed sound: the row clamp is row-local and windowed and keeps Prefix-then-Suffix order; the
Phase 2 borrows work from disjoint fields; Phase 6 builds; `Synced` and remove-before-`try_edit`
are sound; one `request_redraw_at` wake per trigger works.

| # | Issue | Resolution |
|---|---|---|
| 1 | `Vec<Hint>` at the seam after `Hint` lost its offset | `inlay::Placed { offset, hint }` everywhere (D1, D4, D12, D16) |
| 2 | "Private `Action::InlayDue`" impossible on a public exhaustive enum | Public `#[doc(hidden)]` variant; `due` passed as an `Instant`; armed outside the focus gate (D11) |
| 3 | `Prefix` clamp lands at column 0, before the indent | Clamp to the row's first non-blank, capped at range end (D3) |
| 4 | `Synced.jump` vs `apply_lsp`'s landed jump; `save_lsp`/`jump` | Landed jump wins; both signatures change (D19, Phase 7) |
| 5 | Requests recorded by a landed edit wait | Sync-and-pull loop until the revision is stable (D19) |
| 6 | `apply` abandons `Definition` before the jump slot is used | Inlay gesture arms run before `apply` (D12) |
| 7 | Box selection by character loses virtual cells past EOL | By character in content, by cell past EOL (D7) |
| 8 | Write-only `Awaiting` fields fail `-D warnings` in Phase 5 | Phase 5 reads every new field (D12) |
| 9 | Canary should also count `Rows::position` | Counts in both (D5) |
| 10 | Failed insert loses the hint | Accepted until the refetch; stated (D19) |
| — | "19 geometry sites" is 23 call sites; owned-FoldMap test | Counts fixed; the test wraps it in a `RefCell` (D5) |
| — | `Other` heuristic unmeasured for rust-analyzer's prefix kinds | Marked expected; TODO probes them (D1) |

### Round 3 (same agent, Draft 3) — 0 blockers, 0 majors, 1 minor + 2 wording fixes; all addressed

Confirmed: `Placed` at the seam, the `InlayDue` hand-off, the `Prefix` clamp, the loop and jump
merge, and every round-2 fix.

| # | Issue | Resolution |
|---|---|---|
| 1 | `InlayDue` / `InlayHover` falling into `apply` close completion and kill the hover slot | All four `Inlay*` actions bypass `apply`; Phase 5 test (D12) |
| 2 | "Pulls never edit" is false (an Insert pull edits) | Termination argued from the single gesture slot and the 300 ms delay (D19) |
| 3 | Test-only reads don't silence dead-code | Named the non-test readers (D12) |
| — | `Placed` missing from the OPAQUE line | Added (Constraints) |

### Round 4 (same agent, Draft 4) — "the plan looks great." Step 3 closed.

### Expert critique, round 1 (Draft 5) — 0 blockers, 3 majors, 13 minors; all addressed

Confirmed sound: the store is bounded by the window, so per-row costs stay O(line + log F + log n +
hits) and the draw budget holds; `display_cell` is monotone per edge with End ≤ Caret ≤ Start;
`hit(position(p, e)) == p`; the row filter is complete; one document per URI (client.rs:247-253).

| # | Issue | Resolution |
|---|---|---|
| M1 | LSP-triggered schedules use a stale `now_ms`; refresh after every `didChange` makes every visible editor fetch per keystroke | Delays, not deadlines: the widget stamps `at` from its own clock via `Editor::wake_after` / `Action::Wake(gen)` (D11) |
| M2 | Region-replacing patches keep anchors at arbitrary interior bytes; Suffix/Prefix asymmetric on retype | Drop when one edit covers the whole anchor and inserts; mid-word render guard (D3) |
| M3 | "Can't forget hints" not enforced: FoldMap geometry stays `pub` | `row_layout`/`header_layout`/`hit_row` → `pub(crate)` (D5) |
| m1 | Out-of-span answers unbound the store | Clip to the request span (D4, D16) |
| m2 | Store order ≠ render order; render offset has many owners | `Anchor::render_offset`; server index stored; row sort spelled out (D3, D4) |
| m3 | Repeated per-frame `RowLayout` builds | Per-`Rows` memo, width stored in `Hint`, borrowing visitor (D5) |
| m4 | Anchors could leak into the bulk store | `inlay::Shown` view; bulk inserts reject the kind (D4) |
| m5 | Tab fix must use the raw cell as phase | Stated (D8) |
| m6 | Resolve / location hover don't fit `send` | `Resolve` / `LocationHover` query variants with stale check (D18) |
| m7 | Declined insert still removes the hint | Remove only for non-empty ops (D18, D19) |
| m8 | Hidden variant in a public exhaustive enum; `Side::Auto` stored; `Synced` ≈ `Applied`; `Change` exhaustive | General `Wake`; `sync_lsp` returns `Applied`; `Change` stays exhaustive (reversed in round 2); input `Placement` (with `Auto`) split from the stored `Side` (D1) |
| m9 | Viewport throttle causes a cancel storm while dragging | Trailing 75 ms with a 300 ms max-wait (D11) |
| m10 | Keys churn on refetch, closing an open card | Stable keys for unchanged hints (D16) |
| m11 | Optional same-revision merge | Out of scope, noted for revisit |
| m12 | Pill suppression must be strict interior; toggle-off closes the card; location-hover stale check | D8, D13, D18 |
| m13 | Closing-brace hints for blocks starting above a ranged request | Probe in TODO before dispatch |

### Expert critique, round 2 (Draft 6) — 0 blockers, 0 majors; gaps closed → "the plan looks great"

Confirmed: the drop rule sits in the middle band (multi-edit patches take the naive path only); no
`Wake` is lost or doubled thanks to the generation check; the memo can't go stale; nothing outside
core needs the demoted `FoldMap` methods (examples and benches grepped).

| # | Gap | Resolution |
|---|---|---|
| 1-4 | Coverage test must use old coordinates; O(n·e) on the naive path; region-replacing verbs (`move_line`) now drop hints; touching multi-cursor edits | Stated in D3 with `partition_point`, the Alt+↓ test, and the render guard named as backstop |
| 5-7 | `first_seen` undefined (cap never fires during a drag); check order; minimised windows | `cap: Option`, only viewport sets it; `first_seen` survives generations; restamp then check; `diff` resets; limit documented; drag test (D11) |
| 8-9 | `Ref` into the memo panics on nested projection; memo bypassed by helpers | `Rc<RowLayout>` cache, `HeaderLayout` shares it; one `&Rows` passed down (D5, Phase 2) |
| 10 | Intra-doc links to demoted methods fail the doc gate | Listed in Phase 2 |
| — | `#[non_exhaustive]` on `Change` loses the missing-arm check | Reverted: stays exhaustive (D19) |
| — | Early return in bulk-store reject needs a fake id | `debug_assert` only (D4) |
| — | Stable-key matching undefined | Ordered multiset, same revision only (D16) |

## Phase-doc resolutions

The open questions the seven phase docs raised are answered in `RESOLUTIONS.md` (binding;
includes the user's choices: Ctrl+I toggle, box selection by character, pill + dimmed text).

## TODO before dispatch

- [ ] Re-read RUST_STYLE.md, OPAQUE.md, the `/iced` and `/commit-and-comment` skills, and
      `.claude/map/lsp-bridge/DISPATCH.md`.
- [ ] `git log --oneline -1` shows `8e72665` or a descendant; `git status` shows only `.claude/`.
- [ ] Baseline green: tests (both feature sets), clippy (both), doc, wasm.
- [ ] Re-grep the cited lines (Current state) and update the phase docs if they drifted.
- [ ] Confirm `v0.4.0` is still untagged.
- [x] Probe rust-analyzer: ranged requests return closing-brace hints whenever the range covers the
      `}` (2026-10-01); no padding change (R28).
- [x] Probe rust-analyzer with all hint kinds on (2026-10-01): D1's `Auto` rule revised (R27).

## Appendix A — Standard agent dispatch preamble

Use `.claude/map/inlay-hints/DISPATCH.md` (the lsp-bridge preamble with these substitutions applied: the map directory is
`.claude/map/inlay-hints/`; patches go to `.claude/map/inlay-hints/patches/`; the `--all-features`
clippy run applies to every phase; RESOLUTIONS.md is in the reading order). Append the phase doc path and the phase's base commit and commit
boundaries.

## Appendix B — Resume guidance (`/goon inlay-hints`)

1. Read `SESSION_HANDOFF.md` and `goon.yaml`, then run the `quick` checklist.
2. Skim this plan's "Current state", "Key design decisions" and "Risks".
3. Take the next phase from the handoff's status table, work through "TODO before dispatch", and
   dispatch it with Appendix A.
4. After the agent reports: review the diff against the phase doc; run `verify` and `lint`; build the
   commits with `patches/commit-phase.sh`; update the handoff.
5. Run the phases strictly in order.
