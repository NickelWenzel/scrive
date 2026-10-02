# Phase 2 — core + iced: the `Rows` view and `Edge` (no behaviour change)

This is the implementation spec for Phase 2 of `MAP_PLAN.md` (Draft 7). It is self-contained: the
design decisions it implements are restated below. Every line number was read from source at HEAD
`8e72665` (branch `lsp_bridge`). Phase 1 lands first and moves `document.rs` lines (new field,
new methods), so **locate sites by function name and the quoted code**, not by line number alone.

Steps marked **[OQ-n]** follow the defaults this doc proposed; RESOLUTIONS.md R22 confirmed every
one of them (see "Resolved questions" at the end).

## 1. Prerequisites

- Phase 1 is merged. It adds `scrive_core::intel::inlay` (`Hint`, `Placed`, `Anchor`, `Side`, …)
  and a **dedicated inlay `DecorationStore` field on `Document`**, wired like `autoclose`
  (`Document::new`, `Views`, `rebase_views`). This doc calls that field `inlays` (`self.inlays`).
  If Phase 1 named it differently, use its name everywhere this doc writes `inlays` **[OQ-5]**.
  Phase 2 uses nothing else from Phase 1: no hint types appear in any Phase 2 signature.
- `git log --oneline -1` shows the Phase 1 tip; `git status` shows only `.claude/`.
- Baseline green after Phase 1: `cargo test --workspace` and `--all-features`, both clippy runs,
  the doc build and the wasm build (the Verification block below). Write down the test count; this
  phase adds exactly 4 tests and renames 1.
- Read in full before editing: `crates/scrive-core/src/row_layout.rs`, `crates/scrive-core/src/movement.rs`
  (lines 1-300 and the test helper `mv` at 507-518), `crates/scrive-core/src/document.rs`
  (`fold_map`/`ensure_fold_map` 330-353, `move_carets` 435-445, `add_caret_vertical` 606-623,
  `column_select` … `rebuild_column_box` 860-1046, `toggle_fold_opener` 1536-1580,
  `unfold_to_reveal` 2318-2360, `expand_folds_touched` 2374-2462, the tests at 2860-2880,
  3075-3092, 4427-4465), and in `crates/scrive-iced/src/editor.rs` every function named in
  Step 7.
- Conventions: `~/.claude/guides/RUST_STYLE.md`, `~/.claude/guides/OPAQUE.md`,
  `.claude/map/lsp-bridge/DISPATCH.md` (with the inlay-hints substitutions of MAP_PLAN Appendix A),
  the `/iced` and `/commit-and-comment` skills. Doc comments quoted below are content hints; trim
  them to `/commit-and-comment`. No comment may mention phases, plan sections or "previously".

## 2. Goal and exit criteria

**Goal.** Every geometry projection in core and in the widget goes through one view,
`row_layout::Rows`, which bundles the fold map, the buffer, the inlay store and the tab width, and
every projection site names the `row_layout::Edge` it wants. Visibility probes go through a
hint-free `FoldMap::renders`. The FoldMap geometry methods become crate-private. **Nothing renders,
moves or hit-tests differently**: Phase 2 has no hints in any layout, so every edge yields the same
cell, and the suite is the oracle.

**Exit criteria.**

1. `row_layout::Edge { Start, End, Caret }` and `row_layout::Rows<'a>` exist, with
   `Rows::{folds, layout, header, position, hit}` public, `Rows::{new, buffer, tab}` crate-private,
   and `Document::rows()` public.
2. `Rows::layout` memoises per row (`Rc<RowLayout>`); `HeaderLayout` shares its head through the
   same `Rc`.
3. `FoldMap::renders(buffer, offset, tab) -> bool` (crate-private) answers the three visibility
   probes; `DISPLAY_POSITION_PROBES` counts in `renders` and in `Rows::position`.
4. The 23 geometry call sites (table in §8.1) use `Rows`; the 3 visibility probes use `renders`.
5. `movement::move_selections(set, &Rows, motion, extend)`; `move_carets`, `add_caret_vertical`,
   `column_select` and `column_drag` read the cached fold map through `Rows` (no per-press
   `FoldMap::new`).
6. `FoldMap::display_position` is gone; `FoldMap::{row_layout, header_layout, hit_row}` are
   `pub(crate)`; `RowLayout::display_cell` takes an `Edge` **[OQ-3]**.
7. Grep gates (after the last commit):
   - `grep -rn 'display_position' crates --include='*.rs'` prints only `DISPLAY_POSITION_PROBES` lines;
   - `grep -n '\.row_layout(\|\.header_layout(\|\.hit_row(' crates/scrive-iced/src/editor.rs` prints nothing;
   - `grep -n 'FoldMap::new' crates/scrive-core/src/document.rs` outside `mod tests` prints only
     `ensure_fold_map`, `toggle_fold_opener`, `eject_hidden_carets`, `unfold_to_reveal` and
     `expand_folds_touched`.
8. The full Verification block is green after **each** commit. The only test changes are the
   call-site rewrites in §6 (identical expectations), one rename, and 4 new smoke tests.

## 3. Design decisions implemented

Restated from MAP_PLAN.md; these are binding.

**D5 — one view bundles everything a geometry projection needs.**
- `row_layout::Rows<'_>` holds `Ref<FoldMap>`, `&Buffer`, `&DecorationStore` (the inlay store)
  and `tab`. It owns `layout(row)`, `header(row)`, `position(offset, Edge)`, `hit(row, cell, Bias)`
  and (Phase 3, not here) `inlay_at(row, cell)`, plus `folds()` for FoldMap-only helpers
  (`skip_fold_*`, `line_end_folded`, the widget's row iteration).
- **Enforced:** `FoldMap::{row_layout, header_layout, hit_row}` become `pub(crate)` (this rides the
  unreleased 0.4.0), so `Rows::folds()` can hand out `&FoldMap` without letting anyone outside the
  crate build a layout that forgets hints. `move_selections` stays public; its `&Rows` comes from
  `Document::rows()`.
- **Per-view memo.** One frame builds the same row's layout in the text pass, the bracket pass,
  `max_line_px`, washes, squiggles and carets. `Rows` memoises built layouts per row in a
  `RefCell` map of `Rc<RowLayout<'a>>` for its lifetime and returns `Rc` clones, holding each
  `borrow_mut` only for the insert. Handing out a `Ref` into the cache would panic with
  `BorrowMutError` when a caller projects another row while holding it (`draw_wash_row`, a header
  tail). `HeaderLayout` holds `Rc<RowLayout>` instead of owning `head` (row_layout.rs:323), so
  header and head share one build. The memo can't go stale: `Rows` borrows the buffer and store and
  holds the fold `Ref`, and `RowLayout::new` copies folds out (row_layout.rs:194-209).
- The widget takes one `Rows` per pass and **passes it down** to the helpers that build their own
  `fold_map()` today: `draw_selection`, `hit_test`, `collapsed_chip_at`, `armed_boxes`,
  `max_line_px` (and, because they reach those, `max_scroll_x` and `hscrollbar`; see §3.2).
- `Document::rows()` is the public entry. Inside core a crate-private
  `Rows::new(Ref<FoldMap>, &Buffer, &DecorationStore, tab)` is built from **disjoint field
  borrows**, so `move_carets`, `add_caret_vertical` and the column paths compute through `Rows`
  while writing `self.selections` (a different field). `ensure_fold_map()` runs before the `Ref`
  is taken. The `movement.rs` test helper, which builds an owned `FoldMap`, wraps it in a `RefCell`
  to call `Rows::new`. Those four paths switch to the cached fold map instead of a per-press
  `FoldMap::new` (removes an O(folds) rebuild per press; the drift oracle
  `fold_map_cache_matches_a_fresh_build_across_changes` guarantees the cached map equals a fresh
  build).
- **Visibility probes stay on `FoldMap`.** `toggle_fold_opener`, `unfold_to_reveal` and
  `expand_folds_touched` (which probes a hypothetical `FoldSet` inside `rebase_views`, where the
  inlay store is borrowed mutably) only ask "does this offset render?", which hints never change.
  They use a hint-free `FoldMap::renders(buffer, offset, tab) -> bool`, the current
  `display_position` narrowed to its `is_some()`. The `DISPLAY_POSITION_PROBES` canary counts in
  both `renders` and `Rows::position`, so `typing_at_many_carets_over_folds_stays_linear`
  (document.rs:4427-4465) keeps seeing every probe on the commit path.
- A geometry projection can no longer forget hints (`grab-bag-signature` → `proof-bundle`).

**D6 — projections name the edge they want.** `row_layout::Edge { Start, End, Caret }` for an
offset `p` that (from Phase 3 on) may carry hints:
- `Start`: after every hint at `p`. Glyphs, bracket colours and boxes, the start of any range
  (washes, squiggles), popup anchors at a word start.
- `End`: before every hint at `p`. The end of any range.
- `Caret`: after the `Prefix` hints at `p`, before the `Suffix` hints: where the next typed
  character lands.
- An **empty** selection's caret uses `Caret`. A **non-empty** selection's caret renders at its
  wash edge: `End` when the head is the end, `Start` when reversed.
- An **empty range** (zero-width diagnostic, empty find match) uses `Caret` for both ends;
  `Start..End` would be inverted.
- On a multi-row range, an interior row's end is `Start` at the line end.

**D9 — every projection site picks its edge in this phase**, when it moves to `Rows` anyway; with
no hints every edge is the same cell.

| Site | Edge |
|---|---|
| empty-selection carets (`offset_xy`), autoscroll, signature anchor, box-drag corner, vertical motion | `Caret` |
| non-empty selection carets | the wash edge at the head (`End` forward, `Start` reversed) |
| selection / occurrence / find / scope washes, squiggles | `Start` … `End`; interior rows end at `Start` of the line end; empty ranges use `Caret` for both |
| glyph runs, bracket colours, matching-bracket box, collapsible box (`xo`/`xc`), chip pill / chip hover rects, completion and hover anchors | `Start` |
| row width, `max_line_px` | `width()` (no edge) |

**D7 (identity, the part this phase must hold):** with no hints every projection returns exactly
what it returns today. In this phase that is true by construction: the edge is accepted and not
yet consulted **[OQ-2]**.

### 3.1 Out of scope here (later phases; do not add)

- Hint spans in `RowLayout`, `Rows::inlay_at`, edges changing any cell, `width()` counting hints,
  the row clamp, hint-aware `hit`, box selection by character — Phase 3.
- Painting hints, the `expand_tabs` fix, `draw_spans` — Phase 4.
- Anything in `code_editor.rs`, scrive-lsp, examples — Phases 5-7.

### 3.2 Decisions this doc makes where the plan is silent

- **Decision: `FoldMap::display_position` is deleted**, not demoted. Its geometry body moves to
  `Rows::position` (using the memo); its `is_some()` becomes `renders`. The plan's intra-doc-link
  list ("demoted or renamed methods") treats it as renamed.
- **Decision: `FoldMap::header_layout` and `FoldMap::hit_row` stay (as `pub(crate)`) and stay
  live.** `renders` answers through `header_layout` (that is what keeps the plan's `tab` parameter
  meaningful and keeps `renders` bit-identical to `display_position(..).is_some()`), and
  `Rows::hit` delegates to `hit_row` (a click, box row or vertical landing is not a per-frame path,
  so it needs no memo; Phase 3 rewrites `Rows::hit` when it becomes hint-aware). `Rows::header`
  builds from the memo through a shared private constructor `HeaderLayout::new`. **[OQ-4]**
- **Decision: `Rows` gets two crate-private accessors not named in D5**, `buffer()` and `tab()`:
  `move_selections` takes only `&Rows` (D5), and its byte-motion helpers need the buffer and its
  hidden-offset fallback needs the tab width.
- **Decision: `movement::caret_one_display_row` takes `&Rows`.** D5 lists it among the
  FoldMap-only helpers, but it calls `vertical_by`, which is a geometry site.
- **Decision: `rebuild_column_box(&mut self, folds, tab, col)` becomes
  `column_box(rows: &Rows, col) -> SelectionSet`** (no `self`); `column_select` / `column_drag`
  assign `self.selections` from it while the view is alive (disjoint fields). `caret_corner`
  becomes `caret_corner(&self, rows: &Rows) -> CellCorner`. **[OQ-8]**
- **Decision: a private `cached_fold_map(&RefCell<FoldMapCache>) -> Ref<'_, FoldMap>` in
  document.rs** replaces the `Ref::map(self.fold_cache.borrow(), |c| &c.map)` expression, which
  `fold_map()` and the four selection paths would otherwise repeat.
- **Decision: widget helpers that took `fold_map: &FoldMap` take `rows: &Rows<'_>`** and bind
  `let fold_map = rows.folds();` at the top, so their bodies change only at geometry calls. The
  widget builds one `Rows` per pass: in `draw`, `layout`, `update` (once, at the top) and
  `mouse_interaction`. The three popup-anchor helpers (`popup_layout`, `hover_layout`,
  `draw_signature`) and `draw_fold_preview` project once each and are also reached from
  `update`; they pass a temporary `&self.doc.rows()`. **[OQ-7]**
- **Decision: no flat re-export of `Rows`/`Edge` from `lib.rs`** (module paths, RUST_STYLE).
  `lib.rs` only gains a "Where do I…?" bullet. The widget imports
  `scrive_core::row_layout::{Edge, Rows}`. **[OQ-10]**
- **Decision: the memo map is `HashMap<BufferRow, Rc<RowLayout<'a>>>`** (`BufferRow` is `Hash`;
  std `HashMap` already appears in core, `select_all_occurrences`).

## 4. Commit boundaries

Three commits, each green on its own (the Verification block). Patches go to
`.claude/map/inlay-hints/patches/phase2-<k>.patch` per DISPATCH override 2.

| # | Subject | Content |
|---|---|---|
| 1 | `refactor(core)!: project core geometry through row_layout::Rows` | Steps 1-5: `Edge`, `Rows`, memo, `HeaderLayout` `Rc` head, `renders`, `Document::rows`, `cached_fold_map`; the 6 core geometry sites and 3 probes; `move_selections(&Rows)`; the 4 selection paths on the cache; `lib.rs` bullet; core test rewrites (document.rs 2872/3084, movement.rs `mv`); 4 new smoke tests. `FoldMap::display_position` and the `pub` FoldMap methods still exist (the widget still calls them). Breaking: `move_selections`' signature. |
| 2 | `refactor(iced): draw and hit-test through one Rows per pass` | Step 7: the 17 widget sites, the helper signatures, the edge picks, the widget test rewrites. |
| 3 | `refactor(core)!: keep FoldMap geometry crate-private` | Step 6: delete `display_position`, demote `row_layout`/`header_layout`/`hit_row`, `display_cell(col, Edge)` and its 5 callers (incl. editor.rs 1688, 3100), intra-doc links, the row_layout.rs `display_position` test rewrite, prose in perf.rs / perf_gate.rs. |

Suggested bodies (1-3 lines, the why):
1. "A projection that bundles folds, buffer, inlay store and tab width can't forget hints once they render. Visibility probes keep a hint-free `FoldMap::renders`; box, add-caret and arrow paths read the cached fold map."
2. "One view per widget pass builds each row's layout once per frame, and every projection now names the edge it means."
3. "Only `Rows` can build a layout now, so no caller outside the crate projects without the hints; `display_cell` names its edge like `position` does."

If commit 1 can't be made green without part of commit 3 (it should: nothing in it removes a
public item the widget uses), merge them and say so in the report.

## 5. Step-by-step changes — scrive-core

### Step 1 — `row_layout.rs`: `Edge`, `Rows`, the memo, `HeaderLayout`'s shared head, `renders` (commit 1)

**1a. Imports** (row_layout.rs:16-21). Before:

```rust
use std::borrow::Cow;

use crate::buffer::Buffer;
use crate::coords::{Bias, Point};
use crate::display_map::{self, BufferRow, DisplayRow};
use crate::fold_map::{FoldMap, InlineFold};
```

After:

```rust
use std::borrow::Cow;
use std::cell::{Ref, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use crate::buffer::Buffer;
use crate::coords::{Bias, Point};
use crate::decorations::DecorationStore;
use crate::display_map::{self, BufferRow, DisplayRow};
use crate::fold_map::{FoldMap, InlineFold};
```

**1b. Canary comment** (row_layout.rs:23-27). Keep the static and its name; reword the comment to
say it counts `FoldMap::renders` and `Rows::position` probes, so a test can assert
`expand_folds_touched` probes O(edit points) per commit and never O(candidates · edits).

**1c. `Edge`** — insert after `CaretCell`'s `impl` (after row_layout.rs:63):

```rust
/// Which side of the inlay hints at one buffer offset a projection lands on.
/// Hints take cells but no bytes, so an offset with hints spans several cells;
/// each projection names the one it means. With no hints there, every edge is
/// the same cell.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Edge {
    /// After every hint at the offset: glyphs, brackets and their boxes, popup
    /// anchors at a word start, and the start of a range.
    Start,
    /// Before every hint at the offset: the end of a range.
    End,
    /// After the offset's prefix hints and before its suffix hints, where the
    /// next typed character lands: carets, autoscroll, vertical motion.
    Caret,
}
```

**1d. `HeaderLayout` holds an `Rc` head** (row_layout.rs:321-337). Before:

```rust
pub struct HeaderLayout<'a> {
    /// The header row's own horizontal projection.
    head: RowLayout<'a>,
```

After:

```rust
pub struct HeaderLayout<'a> {
    /// The header row's own horizontal projection, shared with the view that
    /// built it.
    head: Rc<RowLayout<'a>>,
```

Add a private constructor as the first item of `impl<'a> HeaderLayout<'a>` (before `head()` at
:333); `head()` keeps its signature (`&self.head` deref-coerces to `&RowLayout<'a>`):

```rust
    fn new(head: Rc<RowLayout<'a>>, last: BufferRow, buffer: &'a Buffer, tab: u32) -> Self {
        let tail_line = buffer.line(last.0);
        let tail_lead = tail_start_col(&tail_line);
        Self { head, last, tail_line, tail_lead, tab }
    }
```

**1e. `FoldMap::header_layout` uses it** (row_layout.rs:436-445). Before:

```rust
    pub fn header_layout<'a>(&self, buffer: &'a Buffer, row: BufferRow, tab: u32) -> Option<HeaderLayout<'a>> {
        let last = self.fold_at_header(row)?;
        let head = self.row_layout(buffer, row, tab);
        let tail_line = buffer.line(last.0);
        let tail_lead = tail_start_col(&tail_line);
        Some(HeaderLayout { head, last, tail_line, tail_lead, tab })
    }
```

After (visibility unchanged in commit 1; Step 6 demotes it):

```rust
    pub fn header_layout<'a>(&self, buffer: &'a Buffer, row: BufferRow, tab: u32) -> Option<HeaderLayout<'a>> {
        let last = self.fold_at_header(row)?;
        Some(HeaderLayout::new(Rc::new(self.row_layout(buffer, row, tab)), last, buffer, tab))
    }
```

**1f. `FoldMap::renders`** — add to the `impl FoldMap` block in row_layout.rs, right after
`display_position` (:469):

```rust
    /// Whether buffer `offset` renders anywhere: `false` only inside a
    /// collapsed block's gap or before the visible tail on its last row. The
    /// hint-free visibility probe; geometry goes through [`Rows::position`].
    #[must_use]
    pub(crate) fn renders(&self, buffer: &Buffer, offset: u32, tab: u32) -> bool {
        #[cfg(any(test, debug_assertions))]
        DISPLAY_POSITION_PROBES.with(|c| c.set(c.get() + 1));
        crate::perf::charge(1); // complexity gate: one display-map probe
        let p = buffer.offset_to_point(offset);
        let row = BufferRow(p.row);
        if !self.is_folded(row) {
            return true;
        }
        self.header_of_tail(row)
            .and_then(|hdr| self.header_layout(buffer, hdr, tab))
            .is_some_and(|layout| layout.tail_col_cell(p.col).is_some())
    }
```

This is `display_position(buffer, offset, tab).is_some()` exactly: an unfolded row always returned
`Some`; a folded row returned `Some` iff `header_of_tail` → `header_layout` → `tail_col_cell` all
did. It skips building a `RowLayout` for unfolded rows, which only lowers the perf meter.

**1g. `Rows`** — add after the `impl FoldMap` block (before `#[cfg(test)]` at :503):

```rust
/// A document's rows as they render: the fold projection, the buffer, the
/// inlay hints and the tab width in one view, so no geometry query can leave
/// one of them out. Get it from [`Document::rows`](crate::Document::rows) and
/// keep it for one pass (a frame, an event, an edit): it borrows the
/// document's fold cache.
///
/// Each row's layout is built once per view and shared, so the passes of one
/// frame (text, brackets, washes, squiggles, carets, scroll width) don't
/// rebuild it.
pub struct Rows<'a> {
    folds: Ref<'a, FoldMap>,
    buffer: &'a Buffer,
    _inlays: &'a DecorationStore, // [OQ-1]
    tab: u32,
    built: RefCell<HashMap<BufferRow, Rc<RowLayout<'a>>>>,
}

impl<'a> Rows<'a> {
    pub(crate) fn new(folds: Ref<'a, FoldMap>, buffer: &'a Buffer, inlays: &'a DecorationStore, tab: u32) -> Self {
        Self { folds, buffer, _inlays: inlays, tab, built: RefCell::new(HashMap::new()) }
    }

    /// The fold projection: buffer ↔ display rows, visible rows, fold lookups.
    #[must_use]
    pub fn folds(&self) -> &FoldMap {
        &self.folds
    }

    pub(crate) fn buffer(&self) -> &'a Buffer {
        self.buffer
    }

    pub(crate) fn tab(&self) -> u32 {
        self.tab
    }

    /// The horizontal projection of visible buffer `row`, built on first use
    /// and shared afterwards.
    #[must_use]
    pub fn layout(&self, row: BufferRow) -> Rc<RowLayout<'a>> {
        let cached = self.built.borrow().get(&row).cloned();
        if let Some(layout) = cached {
            return layout;
        }
        let layout = Rc::new(self.folds.row_layout(self.buffer, row, self.tab));
        self.built.borrow_mut().insert(row, Rc::clone(&layout));
        layout
    }

    /// The `head … tail` layout of `row`, iff it is a collapsed block fold's
    /// header. Its head is [`Self::layout`]'s.
    #[must_use]
    pub fn header(&self, row: BufferRow) -> Option<HeaderLayout<'a>> {
        let last = self.folds.fold_at_header(row)?;
        Some(HeaderLayout::new(self.layout(row), last, self.buffer, self.tab))
    }

    /// Where buffer `offset` renders, on the `edge` side of any hints there:
    /// its display row and cell. Follows a collapsed block's closing tail to
    /// the header row and clips a column hidden in an inline fold to its chip
    /// center. `None` iff the offset is hidden: inside a block fold's gap, or
    /// on the last row before the visible tail.
    #[must_use]
    pub fn position(&self, offset: u32, _edge: Edge) -> Option<DisplayPosition> { // [OQ-2]
        #[cfg(any(test, debug_assertions))]
        DISPLAY_POSITION_PROBES.with(|c| c.set(c.get() + 1));
        crate::perf::charge(1); // complexity gate: one display-map probe
        let p = self.buffer.offset_to_point(offset);
        let row = BufferRow(p.row);
        if !self.folds.is_folded(row) {
            return Some(DisplayPosition { row: self.folds.to_display_row(row), x: self.layout(row).caret_cell(p.col) });
        }
        // Hidden row: only a collapsed fold's closing tail is representable;
        // it rides the header's display line.
        let hdr = self.folds.header_of_tail(row)?;
        let cell = self.header(hdr)?.tail_col_cell(p.col)?;
        Some(DisplayPosition { row: self.folds.to_display_row(hdr), x: CaretCell::Cell(cell) })
    }

    /// The inverse of [`Self::position`] on visible `row`: a fractional
    /// display cell → the byte offset a click there lands on. A collapsed
    /// header's gap resolves to the header line's end, its tail to the last
    /// row's column.
    #[must_use]
    pub fn hit(&self, row: BufferRow, cell: f32, bias: Bias) -> u32 {
        self.folds.hit_row(self.buffer, row, cell, bias, self.tab)
    }
}
```

The `// [OQ-n]` markers are for the orchestrator; do not leave them in the code. The `layout`
borrow order matters (Pitfalls): the shared borrow ends with the `let cached` statement, before
`borrow_mut`.

**1h. `FoldMap::row_layout` doc** (row_layout.rs:429-430) says "never store it". Reword: it
builds a fresh layout, and `Rows::layout` memoises them per view.

### Step 2 — `document.rs`: `Document::rows`, `cached_fold_map`, the selection paths (commit 1)

**2a. Imports** (document.rs:12-32). Add `use crate::row_layout::{Edge, Rows};` to the crate group
(next to `use crate::movement::…`). `Edge` is used by `caret_corner` and the tests.

**2b. `cached_fold_map`** — a private free function next to `FoldMapCache` (after :127):

```rust
/// The cached fold map, borrowed from the cache cell alone so a caller can hold
/// it beside `&mut` borrows of other `Document` fields. Freshen the cache with
/// `ensure_fold_map` first.
fn cached_fold_map(cache: &RefCell<FoldMapCache>) -> Ref<'_, FoldMap> {
    Ref::map(cache.borrow(), |c| &c.map)
}
```

`fold_map` (document.rs:336-340). Before:

```rust
    pub fn fold_map(&self) -> Ref<'_, FoldMap> {
        self.ensure_fold_map();
        Ref::map(self.fold_cache.borrow(), |c| &c.map)
    }
```

After:

```rust
    pub fn fold_map(&self) -> Ref<'_, FoldMap> {
        self.ensure_fold_map();
        cached_fold_map(&self.fold_cache)
    }

    /// The document's rows as they render (see [`Rows`]): the view every
    /// on-screen projection goes through. It borrows the fold cache, so take
    /// one per pass and drop it before editing.
    #[must_use]
    pub fn rows(&self) -> Rows<'_> {
        Rows::new(self.fold_map(), &self.buffer, &self.inlays, self.tab_size())
    }
```

The `ensure_fold_map` doc (:341-346) mentions `move_carets`; extend it to "the selection paths".

**2c. `move_carets`** (document.rs:435-445). Before:

```rust
        self.ensure_fold_map();
        let tab = self.tab_size();
        let cache = self.fold_cache.borrow();
        movement::move_selections(&mut self.selections, &self.buffer, &cache.map, tab, motion, extend);
```

After (the comment above it stays; it already explains the disjoint borrow):

```rust
        self.ensure_fold_map();
        let tab = self.tab_size();
        let rows = Rows::new(cached_fold_map(&self.fold_cache), &self.buffer, &self.inlays, tab);
        movement::move_selections(&mut self.selections, &rows, motion, extend);
```

**2d. `add_caret_vertical`** (document.rs:606-623). Before:

```rust
        self.reset_transient();
        let folds = FoldMap::new(&self.folds, &self.brackets, &self.buffer);
        let tab = self.tab_size();
        let delta = if down { 1 } else { -1 };
        let heads: Vec<u32> = self.selections.all().iter().map(Selection::head).collect();
        let mut added = false;
        for head in heads {
            if let Some(off) = movement::caret_one_display_row(&self.buffer, &folds, tab, head, delta)
            {
                self.selections.add_caret(off);
                added = true;
            }
        }
        if added {
```

After:

```rust
        self.reset_transient();
        self.ensure_fold_map();
        let tab = self.tab_size();
        let delta = if down { 1 } else { -1 };
        let heads: Vec<u32> = self.selections.all().iter().map(Selection::head).collect();
        let mut added = false;
        {
            let rows = Rows::new(cached_fold_map(&self.fold_cache), &self.buffer, &self.inlays, tab);
            for head in heads {
                if let Some(off) = movement::caret_one_display_row(&rows, head, delta) {
                    self.selections.add_caret(off);
                    added = true;
                }
            }
        }
        if added {
```

The block is load-bearing: `Ref` has a destructor, so `rows` keeps `self.fold_cache` borrowed until
it drops, and `self.request_reveal` (`&mut self`) follows. The doc comment's mention of
"`movement::caret_one_display_row` rule → `hit_row`" (:600) becomes "→ `Rows::hit`".

**2e. `column_select`** (document.rs:860-875). Before:

```rust
        self.history.seal();
        self.expand_stack.clear();
        let folds = FoldMap::new(&self.folds, &self.brackets, &self.buffer);
        let tab = self.tab_size();
        let mut col = self.column.unwrap_or_else(|| {
            let corner = self.caret_corner(&folds, tab);
            ColumnSelection { anchor: corner, active: corner }
        });
        col.active = Self::step_corner(&folds, col.active, dir);
        self.column = Some(col);
        self.rebuild_column_box(&folds, tab, col);
    }
```

After:

```rust
        self.history.seal();
        self.expand_stack.clear();
        self.ensure_fold_map();
        let tab = self.tab_size();
        let rows = Rows::new(cached_fold_map(&self.fold_cache), &self.buffer, &self.inlays, tab);
        let mut col = self.column.unwrap_or_else(|| {
            let corner = self.caret_corner(&rows);
            ColumnSelection { anchor: corner, active: corner }
        });
        col.active = Self::step_corner(rows.folds(), col.active, dir);
        self.column = Some(col);
        self.selections = Self::column_box(&rows, col);
    }
```

The closure borrows `self` shared, which coexists with `rows`' shared field borrows; the two
assignments write `self.column` and `self.selections`, fields `rows` does not borrow.

**2f. `column_drag`** (document.rs:886-897). Before:

```rust
        self.history.seal();
        self.expand_stack.clear();
        let folds = FoldMap::new(&self.folds, &self.brackets, &self.buffer);
        let col = ColumnSelection {
            anchor: CellCorner { row: anchor.0, cell: anchor.1 },
            active: CellCorner { row: active.0, cell: active.1 },
        };
        self.column = Some(col);
        self.rebuild_column_box(&folds, self.tab_size(), col);
    }
```

After:

```rust
        self.history.seal();
        self.expand_stack.clear();
        self.ensure_fold_map();
        let tab = self.tab_size();
        let col = ColumnSelection {
            anchor: CellCorner { row: anchor.0, cell: anchor.1 },
            active: CellCorner { row: active.0, cell: active.1 },
        };
        self.column = Some(col);
        let rows = Rows::new(cached_fold_map(&self.fold_cache), &self.buffer, &self.inlays, tab);
        self.selections = Self::column_box(&rows, col);
    }
```

**2g. `caret_corner`** — call sites document.rs:907 (`Caret`) and :914. Before (:899-918):

```rust
    /// The primary caret's box corner: its rendered position — the one owner of
    /// display geometry, [`FoldMap::display_position`] — as a `(visible buffer
    /// row, display cell)`
    /// pair. …
    fn caret_corner(&self, folds: &FoldMap, tab: u32) -> CellCorner {
        let head = self.selections.newest().head();
        match folds.display_position(&self.buffer, head, tab) {
            Some(p) => CellCorner {
                row: folds.to_buffer_row(p.row).0,
                cell: crate::row_layout::virtual_cell(p.x.cells()),
            },
            None => {
                let p = self.buffer.offset_to_point(head);
                let layout = folds.row_layout(&self.buffer, BufferRow(p.row), tab);
                CellCorner { row: p.row, cell: layout.display_cell(p.col) }
            }
        }
    }
```

After (doc link → [`Rows::position`]; the rest of the doc unchanged):

```rust
    fn caret_corner(&self, rows: &Rows<'_>) -> CellCorner {
        let head = self.selections.newest().head();
        match rows.position(head, Edge::Caret) {
            Some(p) => CellCorner {
                row: rows.folds().to_buffer_row(p.row).0,
                cell: crate::row_layout::virtual_cell(p.x.cells()),
            },
            None => {
                let p = self.buffer.offset_to_point(head);
                let layout = rows.layout(BufferRow(p.row));
                CellCorner { row: p.row, cell: layout.display_cell(p.col) }
            }
        }
    }
```

Commit 3 changes the last line to `layout.display_cell(p.col, Edge::Caret)` (Step 6c).

**2h. `rebuild_column_box` → `column_box`** — call sites document.rs:1038, :1039. Before
(:1023-1046):

```rust
    /// Install the box `col` as the selection set: one selection per spanned
    /// *display* row (a collapsed fold's hidden rows get none), each corner cell
    /// resolved to its byte offset through the one click inverse
    /// ([`FoldMap::hit_row`]: tab snapping, chip resolution, header gap/tail) —
    /// so the box selects exactly what its rectangle crosses on screen, clamped
    /// to each row's content. The active row's selection is the newest
    /// (autoscroll target).
    fn rebuild_column_box(&mut self, folds: &FoldMap, tab: u32, col: ColumnSelection) {
        let da = folds.to_display_row(BufferRow(col.anchor.row)).index();
        …
            let anchor_off = folds.hit_row(&self.buffer, row, col.anchor.cell as f32, Bias::Left, tab);
            let head_off = folds.hit_row(&self.buffer, row, col.active.cell as f32, Bias::Left, tab);
        …
        self.selections = SelectionSet::from_ranges(&ranges, newest);
    }
```

After (doc: "The box `col` as a selection set: …", link → [`Rows::hit`]):

```rust
    fn column_box(rows: &Rows<'_>, col: ColumnSelection) -> SelectionSet {
        let folds = rows.folds();
        let da = folds.to_display_row(BufferRow(col.anchor.row)).index();
        let dv = folds.to_display_row(BufferRow(col.active.row)).index();
        let (d0, d1) = (da.min(dv), da.max(dv));
        let mut ranges = Vec::with_capacity((d1 - d0 + 1) as usize);
        let mut newest = 0;
        for d in d0..=d1 {
            let row = folds.to_buffer_row(DisplayRow(d));
            let anchor_off = rows.hit(row, col.anchor.cell as f32, Bias::Left);
            let head_off = rows.hit(row, col.active.cell as f32, Bias::Left);
            if d == dv {
                newest = ranges.len();
            }
            ranges.push((anchor_off, head_off));
        }
        SelectionSet::from_ranges(&ranges, newest)
    }
```

`ColumnSelection`'s doc (:145-150) links [`FoldMap::hit_row`]; change it to [`Rows::hit`].
`step_corner` (:1010) is unchanged (FoldMap-only).

**2i. Visibility probe 1: `toggle_fold_opener`** (document.rs:1573). Before:

```rust
                        if inside && fold_map.display_position(buffer, s.head(), tab).is_none() {
```

After:

```rust
                        if inside && !fold_map.renders(buffer, s.head(), tab) {
```

The comment at :1561-1563 ("`display_position` is `None` exactly for offsets in a fold's gap")
becomes "`renders` is false exactly for offsets in a fold's gap". The local
`FoldMap::new(folds, brackets, buffer)` on the destructured fields stays.

**2j. Visibility probe 2: `unfold_to_reveal`** (document.rs:2334). Before:

```rust
            let renders = fm.display_position(&self.buffer, offset, tab).is_some();
```

After:

```rust
            let renders = fm.renders(&self.buffer, offset, tab);
```

Doc (:2322) link → [`FoldMap::renders`] (private fn doc, not checked by rustdoc, but keep it
true); the comment at :2329-2333 says "`display_position` clips a chip-hidden column to the chip's
center, so an offset inside a collapsed INLINE fold still gets a position" → "`renders` is true for
a chip-hidden column (it gets the chip's center), so …".

**2k. Visibility probe 3: `expand_folds_touched`** (document.rs:2437, over the hypothetical `sub`
`FoldSet`). Before:

```rust
    let any_hidden = pts.iter().any(|&p| fold_map.display_position(buffer, p, tab).is_none());
```

After:

```rust
    let any_hidden = pts.iter().any(|&p| !fold_map.renders(buffer, p, tab));
```

The comment at :2451 ("`display_position` is `None` exactly in the gap") → "`renders` is false
exactly in the gap". Signatures of `expand_folds_touched` and `rebase_views` do not change.

**2l. Tests** (document.rs `mod tests`, commit 1; expectations identical).

`fold_toggle_seals_undo_and_clears_the_expand_ladder` (:2872-2876). Before:

```rust
        let fm = crate::fold_map::FoldMap::new(d.folds(), d.brackets(), d.buffer());
        assert!(
            fm.display_position(d.buffer(), head, d.tab_size()).is_some(),
            "…so the caret cannot be restored into the collapsed fold"
        );
```

After:

```rust
        assert!(
            d.rows().position(head, Edge::Caret).is_some(),
            "…so the caret cannot be restored into the collapsed fold"
        );
```

`find_navigation_expands_a_collapsed_fold_to_reveal_the_match` (:3084-3088): the same rewrite with
`m.end` and the message "the match head renders". `find_navigation_expands_a_collapsed_inline_fold`
(:2885) has only a comment naming `display_position`; reword it to `Rows::position`.

### Step 3 — `movement.rs`: `move_selections(&Rows)` and `vertical_by` (commit 1)

Call sites movement.rs:207 (`Caret`) and :224.

**3a. Imports** (movement.rs:18-22): add `use crate::row_layout::{Edge, Rows};`. `FoldMap`,
`display_map`, `Bias` stay used.

**3b. `move_selections`** (movement.rs:83-100). Before:

```rust
/// … `tab` is the document's tab-stop width — vertical
/// motion keeps a *visual* goal column, so it needs the display projection.
pub fn move_selections(set: &mut SelectionSet, buffer: &Buffer, folds: &FoldMap, tab: u32, motion: Motion, extend: bool) {
    set.map_each(|s| {
        let (target, goal) = motion_target(buffer, folds, tab, s.head(), s.goal, motion);
```

After (doc: "`rows` is the document's display projection; vertical motion keeps a *visual*
goal column, so it needs it."):

```rust
pub fn move_selections(set: &mut SelectionSet, rows: &Rows<'_>, motion: Motion, extend: bool) {
    set.map_each(|s| {
        let (target, goal) = motion_target(rows, s.head(), s.goal, motion);
```

**3c. `motion_target`** (movement.rs:105-120). Before:

```rust
fn motion_target(buffer: &Buffer, folds: &FoldMap, tab: u32, head: u32, goal: Option<u32>, motion: Motion) -> (u32, Option<u32>) {
    match motion {
        Motion::Left => (char_left(buffer, folds, head), None),
        …
        Motion::Up => vertical_by(buffer, folds, tab, head, goal, -1),
        Motion::Down => vertical_by(buffer, folds, tab, head, goal, 1),
        Motion::PageUp(rows) => vertical_by(buffer, folds, tab, head, goal, -(rows as i32)),
        Motion::PageDown(rows) => vertical_by(buffer, folds, tab, head, goal, rows as i32),
```

After (the byte motions keep `(buffer, folds)`; the `PageUp(rows)` / `PageDown(rows)` pattern
bindings would shadow the new `rows` parameter, so rename them to `n`):

```rust
fn motion_target(rows: &Rows<'_>, head: u32, goal: Option<u32>, motion: Motion) -> (u32, Option<u32>) {
    let (buffer, folds) = (rows.buffer(), rows.folds());
    match motion {
        Motion::Left => (char_left(buffer, folds, head), None),
        …
        Motion::Up => vertical_by(rows, head, goal, -1),
        Motion::Down => vertical_by(rows, head, goal, 1),
        Motion::PageUp(n) => vertical_by(rows, head, goal, -(n as i32)),
        Motion::PageDown(n) => vertical_by(rows, head, goal, n as i32),
```

The `WordLeft`/`WordRight`/`LineStart`/`LineEnd` arms are unchanged (they use `buffer`, `folds`).

**3d. `vertical_by`** (movement.rs:205-226). Before:

```rust
fn vertical_by(buffer: &Buffer, folds: &FoldMap, tab: u32, offset: u32, goal: Option<u32>, delta: i32) -> (u32, Option<u32>) {
    // …
    let (row, cell) = match folds.display_position(buffer, offset, tab) {
        Some(p) => (p.row, p.x.cells()),
        None => {
            let p = buffer.offset_to_point(offset);
            (folds.to_display_row(BufferRow(p.row)), display_map::expand(&buffer.line(p.row), p.col, tab) as f32)
        }
    };
    …
    let new_row = folds.to_buffer_row(DisplayRow(target as u32));
    let off = folds.hit_row(buffer, new_row, goal_cell as f32, Bias::Left, tab);
    (off, Some(goal_cell))
}
```

After:

```rust
fn vertical_by(rows: &Rows<'_>, offset: u32, goal: Option<u32>, delta: i32) -> (u32, Option<u32>) {
    let (buffer, folds) = (rows.buffer(), rows.folds());
    // …
    let (row, cell) = match rows.position(offset, Edge::Caret) {
        Some(p) => (p.row, p.x.cells()),
        None => {
            let p = buffer.offset_to_point(offset);
            (folds.to_display_row(BufferRow(p.row)), display_map::expand(&buffer.line(p.row), p.col, rows.tab()) as f32)
        }
    };
    …
    let new_row = folds.to_buffer_row(DisplayRow(target as u32));
    let off = rows.hit(new_row, goal_cell as f32, Bias::Left);
    (off, Some(goal_cell))
}
```

The hidden-offset fallback keeps the raw `display_map::expand` (not `rows.layout`), so it stays
bit-identical. Its doc's [`FoldMap::hit_row`] (:197) → [`Rows::hit`].

**3e. `caret_one_display_row`** (movement.rs:233-245). Before:

```rust
pub(crate) fn caret_one_display_row(
    buffer: &Buffer,
    folds: &FoldMap,
    tab: u32,
    offset: u32,
    delta: i32,
) -> Option<u32> {
    let (off, _) = vertical_by(buffer, folds, tab, offset, None, delta);
    // …
    let row_of = |o: u32| folds.to_display_row(BufferRow(buffer.offset_to_point(o).row));
```

After:

```rust
pub(crate) fn caret_one_display_row(rows: &Rows<'_>, offset: u32, delta: i32) -> Option<u32> {
    let (off, _) = vertical_by(rows, offset, None, delta);
    // …
    let (buffer, folds) = (rows.buffer(), rows.folds());
    let row_of = |o: u32| folds.to_display_row(BufferRow(buffer.offset_to_point(o).row));
```

**3f. Test helper `mv`** (movement.rs:507-518). Before:

```rust
    fn mv(set: &mut SelectionSet, b: &Buffer, motion: Motion, extend: bool) {
        use crate::fold_map::{FoldMap, FoldSet};
        move_selections(
            set,
            b,
            &FoldMap::new(&FoldSet::new(), &crate::bracket::Brackets::default(), b),
            display_map::default_tab_size(),
            motion,
            extend,
        );
    }
```

After:

```rust
    fn mv(set: &mut SelectionSet, b: &Buffer, motion: Motion, extend: bool) {
        use crate::fold_map::{FoldMap, FoldSet};
        let folds = std::cell::RefCell::new(FoldMap::new(&FoldSet::new(), &crate::bracket::Brackets::default(), b));
        let inlays = crate::decorations::DecorationStore::new();
        let rows = Rows::new(folds.borrow(), b, &inlays, display_map::default_tab_size());
        move_selections(set, &rows, motion, extend);
    }
```

### Step 4 — `lib.rs` (commit 1)

`pub use movement::{move_selections, …}` (:94) stays. Add one bullet to the "Where do I…?" list
(:18-27):

```rust
//! - place something on screen (row layouts, offset ↔ cell) → [`row_layout::Rows`],
//!   from [`Document::rows`]
```

No re-export of `Rows`/`Edge` at the crate root **[OQ-10]**.

### Step 5 — new smoke tests, `row_layout.rs` `mod tests` (commit 1)

One per `Rows` method (plan: "a smoke test per `Rows` method"). `use super::*;` already brings
`Rc`, `Edge`, `Rows`. Use the module's `doc_with_folds`.

```rust
    /// One build per row per view, and a collapsed header's head is that build.
    #[test]
    fn rows_build_each_row_once_and_the_header_shares_it() {
        let text = "a {\nhidden\n} tail\nafter\n";
        let doc = doc_with_folds(text, &[text.find('{').unwrap() as u32]);
        let rows = doc.rows();
        let head = rows.layout(BufferRow(0));
        assert!(Rc::ptr_eq(&head, &rows.layout(BufferRow(0))), "a second query reuses the first build");
        let hl = rows.header(BufferRow(0)).expect("row 0 is a collapsed header");
        assert!(std::ptr::eq(hl.head(), &*head), "the header's head is the memoised layout");
        assert!(rows.header(BufferRow(3)).is_none(), "a plain row has no header layout");
    }

    /// The view's fold projection is the document's cached one.
    #[test]
    fn rows_folds_is_the_documents_fold_map() {
        let doc = doc_with_folds("a {\nb\n}\nc\n", &[2]);
        assert_eq!(*doc.rows().folds(), *doc.fold_map());
    }

    /// Without hints every edge is the same cell, and `renders` agrees with
    /// `position` on every offset.
    #[test]
    fn rows_position_is_one_cell_on_every_edge_and_renders_agrees() {
        let text = "f([a, b]) {\ninner\n} tail\nafter\n";
        let inline_open = text.find('[').unwrap() as u32;
        let block_open = text.find('{').unwrap() as u32;
        let doc = doc_with_folds(text, &[inline_open, block_open]);
        let rows = doc.rows();
        for offset in 0..=doc.buffer().len() {
            let caret = rows.position(offset, Edge::Caret);
            assert_eq!(rows.position(offset, Edge::Start), caret, "offset {offset}: Start");
            assert_eq!(rows.position(offset, Edge::End), caret, "offset {offset}: End");
            assert_eq!(rows.folds().renders(doc.buffer(), offset, doc.tab_size()), caret.is_some(), "offset {offset}: renders");
        }
    }

    /// `hit` lands back on every landable offset `position` projected, across an
    /// inline chip, a collapsed header and its tail.
    #[test]
    fn rows_hit_inverts_position_on_landable_offsets() {
        let text = "f([a, b]) {\ninner\n}\nafter\n";
        let inline_open = text.find('[').unwrap() as u32;
        let close = text.find(']').unwrap() as u32;
        let block_open = text.find('{').unwrap() as u32;
        let doc = doc_with_folds(text, &[inline_open, block_open]);
        let rows = doc.rows();
        let tail = doc.buffer().point_to_offset(Point::new(2, 0));
        let after = doc.buffer().point_to_offset(Point::new(3, 2));
        for offset in [0, inline_open + 1, close, block_open, tail, after] {
            let p = rows.position(offset, Edge::Caret).expect("landable offset");
            let row = rows.folds().to_buffer_row(p.row);
            assert_eq!(rows.hit(row, p.x.cells(), Bias::Left), offset, "offset {offset} round-trips");
        }
    }
```

The offsets in the last test are the ones `hit_test_inverts_offset_screen_x_across_fold_geometry`
(editor.rs:4552-4575) already round-trips through pixels. All four fixtures are ASCII, so every
offset in the sweep is a char boundary.

### Step 6 — narrowing the FoldMap API (commit 3)

**6a. Delete `FoldMap::display_position`** (row_layout.rs:447-469) entirely.

**6b. Demote** `FoldMap::row_layout` (:432), `FoldMap::header_layout` (:439) and
`FoldMap::hit_row` (:476) from `pub fn` to `pub(crate) fn`. All three keep a non-test caller
(`Rows::layout`/`header_layout`/`hit_row`; `renders`/`hit_row`; `Rows::hit`), so no dead code.
`hit_row`'s doc "Inverse of [`Self::display_position`]" → "Inverse of [`Rows::position`]".

**6c. `RowLayout::display_cell` names its edge [OQ-3]** (row_layout.rs:241-247). Before:

```rust
    pub fn display_cell(&self, col: u32) -> u32 {
        self.cell_of(display_map::expand(&self.line, col, self.tab))
    }
```

After (doc gains: "on the `edge` side of any hints at `col`"):

```rust
    pub fn display_cell(&self, col: u32, _edge: Edge) -> u32 { // [OQ-2]
        self.cell_of(display_map::expand(&self.line, col, self.tab))
    }
```

Its callers:

| Site | Before | After |
|---|---|---|
| row_layout.rs:258, `caret_cell` | `self.display_cell(col)` | `self.display_cell(col, Edge::Caret)` |
| row_layout.rs:299, `width` | `self.display_cell(self.line.len() as u32)` | `self.display_cell(self.line.len() as u32, Edge::Start)` |
| document.rs:915, `caret_corner` fallback | `layout.display_cell(p.col)` | `layout.display_cell(p.col, Edge::Caret)` |
| editor.rs:1688, bracket colours | `row_layout.display_cell(col)` | `row_layout.display_cell(col, Edge::Start)` |
| editor.rs:3100, `draw_row_inline`'s `seg` (glyph runs) | `row_layout.display_cell(start_col)` | `row_layout.display_cell(start_col, Edge::Start)` |
| row_layout.rs:536 (test) | `rl.display_cell(7)` | `rl.display_cell(7, Edge::Caret)` |
| row_layout.rs:563 (test) | `rl.display_cell(col)` | `rl.display_cell(col, Edge::Start)` |
| row_layout.rs:676, 677 (test) | `rl.display_cell(0)`, `rl.display_cell(1)` | `…(0, Edge::Start)`, `…(1, Edge::Start)` |

`caret_cell(col)` keeps its signature (D7: it is the `Caret` edge).

**6d. Intra-doc links** (the `-D warnings` doc build fails on a public doc linking a removed or
crate-private item):

| Where | Before | After |
|---|---|---|
| row_layout.rs:68, `DisplayPosition` (pub) | `(see [`FoldMap::display_position`])` | `(see [`Rows::position`])` |
| row_layout.rs:177, `RowLayout` (pub) | `Built per use by [`FoldMap::row_layout`]; … Like [`FoldMap`], it is cheap to rebuild and never stored.` | `Built by [`Rows::layout`], which shares it for the view's lifetime; …` and drop "never stored" (the memo stores it); keep the "no derived state that could drift" sentence |
| fold_map.rs:849, `entry_edge_if_hidden` (pub) | `[`Self::display_position`]` | `[`Rows::position`](crate::row_layout::Rows::position)` |
| editor.rs:798, `offset_screen_x` (private) | `([`FoldMap::display_position`])` | `([`Rows::position`])` (done in commit 2) |
| editor.rs:3068, `hit_test` (private) | `([`FoldMap::hit_row`]: …)` | `([`Rows::hit`]: …)` (done in commit 2) |

**6e. The row_layout.rs `display_position` test** (:615-638). Rename
`display_position_follows_tail_and_hides_gap` → `position_follows_tail_and_hides_gap`, section
comment "── display_position: …" → "── Rows::position: …". Before (the body):

```rust
        let fm = fold_map(&doc);
        let buffer = doc.buffer();
        let hidden = buffer.point_to_offset(Point::new(1, 2));
        assert_eq!(fm.display_position(buffer, hidden, 4), None);
        let tail = buffer.point_to_offset(Point::new(2, 0));
        let p = fm.display_position(buffer, tail, 4).expect("tail is visible");
        assert_eq!(p.row, DisplayRow(0));
        let hl = fm.header_layout(buffer, BufferRow(0), 4).unwrap();
        assert_eq!(p.x, CaretCell::Cell(hl.tail_cell()));
        let after = buffer.point_to_offset(Point::new(3, 0));
        let p = fm.display_position(buffer, after, 4).expect("visible");
```

After (comments kept; `doc.tab_size()` is 4):

```rust
        let rows = doc.rows();
        let buffer = doc.buffer();
        let hidden = buffer.point_to_offset(Point::new(1, 2));
        assert_eq!(rows.position(hidden, Edge::Caret), None);
        let tail = buffer.point_to_offset(Point::new(2, 0));
        let p = rows.position(tail, Edge::Caret).expect("tail is visible");
        assert_eq!(p.row, DisplayRow(0));
        let hl = rows.header(BufferRow(0)).unwrap();
        assert_eq!(p.x, CaretCell::Cell(hl.tail_cell()));
        let after = buffer.point_to_offset(Point::new(3, 0));
        let p = rows.position(after, Edge::Caret).expect("visible");
```

The other row_layout.rs tests keep calling `fm.row_layout` / `fm.header_layout` / `fm.hit_row`
(crate-private still reaches them) and are unchanged except 6c.

**6f. Prose.** perf.rs:6 lists `FoldMap::display_position` among the metered primitives → 
`FoldMap::renders`, `Rows::position`. perf_gate.rs:10 lists `display_position` → `Rows::position`.

## 6. Step-by-step changes — scrive-iced (Step 7, `editor.rs`, commit 2)

**7a. Import** (editor.rs:36-40): add `use scrive_core::row_layout::{Edge, Rows};` below the
`scrive_core::{…}` import. `FoldMap`, `RowLayout`, `Ref` stay used (`Editor::fold_map`,
`draw_row_inline`).

**7b. One `Rows` per pass.**

| Pass | Where | Add |
|---|---|---|
| `draw` | replace `let fold_map = self.fold_map();` (:1402) | `let rows = self.doc.rows();` then `let fold_map = rows.folds();` |
| `layout` | after `self.ensure_metrics(state);` (:1262) | `let rows = self.doc.rows();` |
| `update` | after `let (advance, line_h) = …;` (:2003) | `let rows = self.doc.rows();` |
| `mouse_interaction` | after `let geo = self.geo(state, bounds);` (:2629) | `let rows = self.doc.rows();` |

`self.doc` is `&'a Document`, so `rows` does not borrow `self`; `&mut self` calls in `layout` and
`update` still compile. In `draw`, `fold_map` becomes `&FoldMap` (was `Ref<FoldMap>`); every
remaining use (`display_window`, `visible_rows`, `is_folded`, `to_display_row`, …) is unchanged.

**7c. The `layout` local `rows` (:1306) shadows the view** inside the autoscroll block. Rename that
`f64` local to `target_rows` (its two uses: the `let` at :1306 and
`ScrollAnchor::from_rows(rows, line_h)` at :1320). No other pass has a local named `rows` in scope
of a `Rows` use (`draw_signature` :2841 and `draw_fold_preview` :3269 use a temporary
`self.doc.rows()` instead, see 7f).

### 7d. Helper signatures

Each `fold_map: &FoldMap` parameter becomes `rows: &Rows<'_>`; where the body still needs row
logic, add `let fold_map = rows.folds();` as its first line. Helpers that built their own
`self.fold_map()` and now take `rows` drop that line.

| Helper (line) | Before | After |
|---|---|---|
| `offset_screen_x` (:802) | `(&self, fold_map: &FoldMap, geo: &Geo, offset: u32)` | `(&self, rows: &Rows<'_>, geo: &Geo, offset: u32, edge: Edge)` |
| `popup_anchor` (:815) | `(&self, fold_map: &FoldMap, geo: &Geo, offset: u32)` | `(&self, rows: &Rows<'_>, geo: &Geo, offset: u32, edge: Edge)` |
| `any_caret_on_screen` (:840) | `(&self, buffer, fold_map: &FoldMap, top_rows, viewport_rows)` | `(&self, buffer, rows: &Rows<'_>, top_rows, viewport_rows)` |
| `collapsible_box_rect` (:876) | `(&self, fold_map: &FoldMap, geo, open, close, header, last)` | `(&self, rows: &Rows<'_>, geo, open, close, header, last)` |
| `armed_boxes` (:921) | `(&self, geo: &Geo, pos: Point)` + own `fold_map()` | `(&self, rows: &Rows<'_>, geo: &Geo, pos: Point)` |
| `max_line_px` (:960) | `(&self, advance, line_h, bounds, scroll_rows)` + own `fold_map()` | `(&self, rows: &Rows<'_>, advance, line_h, bounds, scroll_rows)` |
| `max_scroll_x` (:985) | `(&self, bounds, advance, line_h, scroll_rows)` | `(&self, rows: &Rows<'_>, bounds, advance, line_h, scroll_rows)` |
| `hscrollbar` (:1037) | `(&self, bounds, advance, line_h, scroll_x, scroll_rows)` | `(&self, rows: &Rows<'_>, bounds, advance, line_h, scroll_x, scroll_rows)` |
| `draw_selection` (:2937) | `(&self, renderer, geo, start, end, color)` + own `fold_map()` | `(&self, renderer, rows: &Rows<'_>, geo, start, end, color)` |
| `draw_wash_row` (:2976) | `(…, fold_map: &FoldMap, geo, row, a, b, start, end, color)` | `(…, rows: &Rows<'_>, geo, row, a, b, start, end, color)` |
| `draw_fold_tail_wash` (:3012) | `(…, fold_map: &FoldMap, geo, tail_row, a, b, color)` | `(…, rows: &Rows<'_>, geo, tail_row, a, b, color)` |
| `hit_test` (:3071) | `(&self, geo: &Geo, pos: Point)` + own `fold_map()` | `(&self, rows: &Rows<'_>, geo: &Geo, pos: Point)` |
| `collapsed_chip_rect` (:3149) | `(&self, fold_map: &FoldMap, geo, opener)` | `(&self, rows: &Rows<'_>, geo, opener)` |
| `chip_pill_rect` (:3198) | `(&self, fold_map: &FoldMap, geo, opener)` | `(&self, rows: &Rows<'_>, geo, opener)` |
| `collapsed_chip_at` (:3224) | `(&self, geo: &Geo, pos: Point)` + own `fold_map()` | `(&self, rows: &Rows<'_>, geo: &Geo, pos: Point)` |

Unchanged (no geometry): `Editor::fold_map`, `hit_cell`, `last_visible_row`, `scrollbar`,
`max_scroll_rows`, `code_area_width`. `popup_layout`,
`hover_layout`, `draw_signature`, `draw_fold_preview` keep their signatures (7f).
`hscrollbar` reaches 7 inputs counting `self`, at clippy's `too_many_arguments` limit, not over it.

### 7e. The 17 geometry call sites

**Site 1 — editor.rs:804, `offset_screen_x`.** Before:

```rust
    fn offset_screen_x(&self, fold_map: &FoldMap, geo: &Geo, offset: u32) -> f32 {
        let cells = fold_map
            .display_position(self.doc.buffer(), offset, TAB)
            .map_or(0.0, |p| p.x.cells());
        geo.cell_x(cells)
    }
```

After (doc link → [`Rows::position`]; "the `edge` side of any hints"):

```rust
    fn offset_screen_x(&self, rows: &Rows<'_>, geo: &Geo, offset: u32, edge: Edge) -> f32 {
        let cells = rows.position(offset, edge).map_or(0.0, |p| p.x.cells());
        geo.cell_x(cells)
    }
```

**Site 2 — editor.rs:816, `popup_anchor`.** Before:

```rust
    fn popup_anchor(&self, fold_map: &FoldMap, geo: &Geo, offset: u32) -> (f32, f32, f32) {
        let top = match fold_map.display_position(self.doc.buffer(), offset, TAB) {
            Some(p) => geo.row_y(p.row),
            // Hidden offset (callers don't pass one): clip to its fold's header row.
            None => {
                let row = self.doc.buffer().offset_to_point(offset).row;
                geo.row_y(fold_map.to_display_row(BufferRow(row)))
            }
        };
        (self.offset_screen_x(fold_map, geo, offset), top, top + geo.line_h())
    }
```

After:

```rust
    fn popup_anchor(&self, rows: &Rows<'_>, geo: &Geo, offset: u32, edge: Edge) -> (f32, f32, f32) {
        let top = match rows.position(offset, edge) {
            Some(p) => geo.row_y(p.row),
            // Hidden offset (callers don't pass one): clip to its fold's header row.
            None => {
                let row = self.doc.buffer().offset_to_point(offset).row;
                geo.row_y(rows.folds().to_display_row(BufferRow(row)))
            }
        };
        (self.offset_screen_x(rows, geo, offset, edge), top, top + geo.line_h())
    }
```

**Site 3 — editor.rs:856, `any_caret_on_screen`** (autoscroll → `Caret`). Add
`let fold_map = rows.folds();` first. Before:

```rust
            fold_map
                .display_position(buffer, s.head(), TAB)
                .is_some_and(|p| band.contains(&f64::from(p.row.index())))
```

After:

```rust
            rows.position(s.head(), Edge::Caret).is_some_and(|p| band.contains(&f64::from(p.row.index())))
```

**Site 4 — editor.rs:898, `collapsible_box_rect` block width.** Add `let fold_map = rows.folds();`
first. Before: `hi = hi.max(fold_map.row_layout(buffer, BufferRow(r), TAB).width());`
After: `hi = hi.max(rows.layout(BufferRow(r)).width());`

**Sites 5, 6 — editor.rs:967, 969, `max_line_px`.** Before:

```rust
        let buffer = self.doc.buffer();
        let fold_map = self.fold_map();
        let window = fold_map
            .display_window(scroll_rows, scroll_rows + f64::from(bounds.height) / f64::from(line_h));
        fold_map
            .visible_rows(window)
            .map(|vr| match fold_map.header_layout(buffer, vr.buffer_row, TAB) {
                Some(hl) => hl.width(),
                None => fold_map.row_layout(buffer, vr.buffer_row, TAB).width(),
            })
```

After (`buffer` is no longer needed; drop it):

```rust
        let fold_map = rows.folds();
        let window = fold_map
            .display_window(scroll_rows, scroll_rows + f64::from(bounds.height) / f64::from(line_h));
        fold_map
            .visible_rows(window)
            .map(|vr| match rows.header(vr.buffer_row) {
                Some(hl) => hl.width(),
                None => rows.layout(vr.buffer_row).width(),
            })
```

**Site 7 — editor.rs:1286, `layout` autoscroll** (`Caret`). Delete `let fold_map = self.fold_map();`
(:1279). Before: `if let Some(p) = fold_map.display_position(buffer, head, TAB) {`
After: `if let Some(p) = rows.position(head, Edge::Caret) {`
And :1317 `self.any_caret_on_screen(buffer, &fold_map, cur, viewport_rows)` →
`self.any_caret_on_screen(buffer, &rows, cur, viewport_rows)`.

**Site 8 — editor.rs:1423, `draw`'s `offset_xy` closure.** Before:

```rust
        let offset_xy = |off: u32| -> Option<(f32, f32)> {
            let p = fold_map.display_position(buffer, off, TAB)?;
            window.contains(&p.row.index()).then(|| (geo.cell_x(p.x.cells()), geo.row_y(p.row)))
        };
```

After:

```rust
        let offset_xy = |off: u32, edge: Edge| -> Option<(f32, f32)> {
            let p = rows.position(off, edge)?;
            window.contains(&p.row.index()).then(|| (geo.cell_x(p.x.cells()), geo.row_y(p.row)))
        };
```

**Site 9 — editor.rs:1605, text pass.** Before:
`let row_layout = fold_map.row_layout(buffer, BufferRow(row), TAB);`
After: `let row_layout = rows.layout(BufferRow(row));`
The next line `self.draw_row_inline(…, &row_layout, …)` compiles unchanged: `&Rc<RowLayout>`
deref-coerces to the `&RowLayout<'_>` parameter.

**Site 10 — editor.rs:1626, collapsed header.** Before:

```rust
                let hl = fold_map
                    .header_layout(buffer, BufferRow(row), TAB)
                    .expect("is_fold_header rows have a header layout");
```

After:

```rust
                let hl = rows.header(BufferRow(row)).expect("is_fold_header rows have a header layout");
```

**Site 11 — editor.rs:1679, bracket colours.** Before:
`let row_layout = fold_map.row_layout(buffer, BufferRow(row), TAB);`
After: `let row_layout = rows.layout(BufferRow(row));` (the edge on :1688's `display_cell` comes in
commit 3, 6c).

**Site 12 — editor.rs:2993, `draw_wash_row`.** Shown in full under 7g.

**Site 13 — editor.rs:3019, `draw_fold_tail_wash`.** Add `let fold_map = rows.folds();` first.
Before: `let Some(hl) = fold_map.header_layout(buffer, hdr, TAB) else { return };`
After: `let Some(hl) = rows.header(hdr) else { return };`
(Tail cells come from `tail_col_cell`; no edge.)

**Site 14 — editor.rs:3074, `hit_test`.** Before:

```rust
    fn hit_test(&self, geo: &Geo, pos: Point) -> u32 {
        let fold_map = self.fold_map();
        let row = fold_map.to_buffer_row(fold_map.display_row_at(geo.rows_from_top(pos.y)));
        fold_map.hit_row(self.doc.buffer(), row, geo.x_cell(pos.x), scrive_core::Bias::Left, TAB)
    }
```

After (doc link → [`Rows::hit`]):

```rust
    fn hit_test(&self, rows: &Rows<'_>, geo: &Geo, pos: Point) -> u32 {
        let fold_map = rows.folds();
        let row = fold_map.to_buffer_row(fold_map.display_row_at(geo.rows_from_top(pos.y)));
        rows.hit(row, geo.x_cell(pos.x), scrive_core::Bias::Left)
    }
```

**Site 15 — editor.rs:3168, `collapsed_chip_rect` block.** Add `let fold_map = rows.folds();`
first. Before: `let hl = fold_map.header_layout(buffer, BufferRow(header), TAB)?;`
After: `let hl = rows.header(BufferRow(header))?;`

**Sites 16, 17 — editor.rs:3212, 3215, `chip_pill_rect`.** Add `let fold_map = rows.folds();`
first. Before:

```rust
            geo.cell_x(fold_map.header_layout(buffer, BufferRow(header), TAB)?.gap_center())
        } else {
            let row_start = buffer.point_to_offset(BufPoint { row: header, col: 0 });
            let layout = fold_map.row_layout(buffer, BufferRow(header), TAB);
```

After:

```rust
            geo.cell_x(rows.header(BufferRow(header))?.gap_center())
        } else {
            let row_start = buffer.point_to_offset(BufPoint { row: header, col: 0 });
            let layout = rows.layout(BufferRow(header));
```

### 7f. Edge picks at the helpers' callers (D9)

**Popup anchors** (each passes a temporary view; remove the function's
`let fold_map = self.fold_map();` line):

| Caller | Before | After |
|---|---|---|
| `popup_layout` :2718 (completion, word start) | `self.popup_anchor(&fold_map, geo, list.anchor)` | `self.popup_anchor(&self.doc.rows(), geo, list.anchor, Edge::Start)` |
| `hover_layout` :2749 (hover, word start) | `self.popup_anchor(&fold_map, geo, info.range.start)` | `self.popup_anchor(&self.doc.rows(), geo, info.range.start, Edge::Start)` |
| `draw_signature` :2832 (signature, caret) | `self.popup_anchor(&fold_map, geo, head)` | `self.popup_anchor(&self.doc.rows(), geo, head, Edge::Caret)` |

**`draw_fold_preview`** :3298: `self.collapsed_chip_rect(&fold_map, geo, opener)` →
`self.collapsed_chip_rect(&self.doc.rows(), geo, opener)`. Its `let fold_map = self.fold_map();`
(:3259) stays (row logic) and its local `rows: Vec<u32>` is untouched.

**`collapsible_box_rect`** :909-910 (collapsible box → `Start`):

```rust
            let xo = self.offset_screen_x(rows, geo, open, Edge::Start);
            let xc = self.offset_screen_x(rows, geo, close, Edge::Start);
```

**`collapsed_chip_rect`** :3186-3187 (chip hover rect → `Start`), the same two lines with `opener`
/ `close`.

**Bracket box** (draw :1708, matching-bracket box → `Start`):
`let Some((x, y)) = offset_xy(off) else { continue };` →
`let Some((x, y)) = offset_xy(off, Edge::Start) else { continue };`

**Carets** (draw :1842-1846). Before:

```rust
            for sel in &sels[visible_selection_span(sels, vis_start, vis_end)] {
                if let Some((x, y)) = offset_xy(sel.head()) {
```

After (D6: an empty selection's caret is `Caret`; a non-empty one sits at its wash edge):

```rust
            for sel in &sels[visible_selection_span(sels, vis_start, vis_end)] {
                let edge = if sel.is_empty() {
                    Edge::Caret
                } else if sel.head() == sel.end() {
                    Edge::End
                } else {
                    Edge::Start
                };
                if let Some((x, y)) = offset_xy(sel.head(), edge) {
```

**Squiggles** (draw :1739-1756). Before:

```rust
            for &(start, end, sp_row, ep_row, _sev, color) in &diags {
                …
                let x0 = self.offset_screen_x(&fold_map, &geo, row_start);
                let x1 = self.offset_screen_x(&fold_map, &geo, row_end).max(x0 + advance);
```

After (empty diagnostic → `Caret` both ends; an interior row ends at `Start` of its line end):

```rust
            for &(start, end, sp_row, ep_row, _sev, color) in &diags {
                …
                let (from, to) = if start == end { (Edge::Caret, Edge::Caret) } else { (Edge::Start, Edge::End) };
                let x0 = self.offset_screen_x(&rows, &geo, row_start, from);
                let x1 = self.offset_screen_x(&rows, &geo, row_end, if row == ep_row { to } else { Edge::Start }).max(x0 + advance);
```

`row_start` on a non-first row is the line's column 0; `from` is `Start` there because the range is
non-empty (an empty range has one row).

### 7g. Washes

**`draw_selection`** (:2937-2969): new `rows` parameter; replace `let fold_map = self.fold_map();`
with `let fold_map = rows.folds();`; the three inner calls pass `rows` instead of `&fold_map`:
`self.draw_wash_row(renderer, rows, geo, …)` (:2944, :2962) and
`self.draw_fold_tail_wash(renderer, rows, geo, …)` (:2966). Its four callers in `draw`
(:1511, :1520, :1530, :1535) gain `&rows` after `renderer`.

**`draw_wash_row`** (:2976-3004; site 12 and three edge picks). Before:

```rust
    fn draw_wash_row(&self, renderer: &mut iced::Renderer, fold_map: &FoldMap, geo: &Geo, row: u32, a: BufPoint, b: BufPoint, start: u32, end: u32, color: Color) {
        draw_budget::bump_rows(1);
        if fold_map.is_folded(BufferRow(row)) {
            self.draw_fold_tail_wash(renderer, fold_map, geo, row, a, b, color);
            return;
        }
        …
        let x0 = if row == a.row { self.offset_screen_x(fold_map, geo, start) } else { geo.cell_x(0.0) };
        let x1 = if row == b.row {
            self.offset_screen_x(fold_map, geo, end)
        } else if let Some(hl) = fold_map.header_layout(buffer, BufferRow(row), TAB) {
            …
            geo.cell_x(hl.tail_cell() as f32)
        } else {
            let line_end = buffer.point_to_offset(scrive_core::Point::new(row, buffer.line_len(row)));
            self.offset_screen_x(fold_map, geo, line_end) + advance * 0.5
        };
```

After:

```rust
    fn draw_wash_row(&self, renderer: &mut iced::Renderer, rows: &Rows<'_>, geo: &Geo, row: u32, a: BufPoint, b: BufPoint, start: u32, end: u32, color: Color) {
        draw_budget::bump_rows(1);
        let fold_map = rows.folds();
        if fold_map.is_folded(BufferRow(row)) {
            self.draw_fold_tail_wash(renderer, rows, geo, row, a, b, color);
            return;
        }
        …
        let (from, to) = if start == end { (Edge::Caret, Edge::Caret) } else { (Edge::Start, Edge::End) };
        let x0 = if row == a.row { self.offset_screen_x(rows, geo, start, from) } else { geo.cell_x(0.0) };
        let x1 = if row == b.row {
            self.offset_screen_x(rows, geo, end, to)
        } else if let Some(hl) = rows.header(BufferRow(row)) {
            …
            geo.cell_x(hl.tail_cell() as f32)
        } else {
            let line_end = buffer.point_to_offset(scrive_core::Point::new(row, buffer.line_len(row)));
            self.offset_screen_x(rows, geo, line_end, Edge::Start) + advance * 0.5
        };
```

The `#[allow(clippy::too_many_arguments)]` with its justification stays on both wash helpers.

### 7h. Remaining caller updates

| Call (line) | After |
|---|---|
| `armed_boxes` body :922, :934 | drop `let fold_map = self.fold_map();`, add `let fold_map = rows.folds();`; `self.collapsible_box_rect(rows, geo, open, close, header, last)` |
| `max_scroll_x` body :986 | `self.max_line_px(rows, advance, line_h, bounds, scroll_rows)` |
| `hscrollbar` body :1038 | `self.max_scroll_x(rows, bounds, advance, line_h, scroll_rows)` |
| `layout` :1343 | `self.max_scroll_x(&rows, vp, advance, line_h, state.scroll.rows(line_h))` |
| `draw` :1789 | `self.armed_boxes(&rows, &geo, pos)` |
| `draw` :1804 | `self.collapsible_box_rect(&rows, &geo, open, close, header, last_row)` |
| `draw` :1827 | `self.chip_pill_rect(&rows, &geo, opener)` |
| `draw` :1958 | `self.hscrollbar(&rows, bounds, advance, line_h, scroll_x, state.scroll.rows(line_h))` |
| `update` :2071, :2226 | `self.hscrollbar(&rows, bounds, …)` |
| `update` :2103, :2587; closure :2192 | `self.collapsed_chip_at(&rows, &geo, pos)` / `(&rows, &geo, p)` |
| `update` :2114 | `self.armed_boxes(&rows, &geo, pos)` |
| `update` :2124, :2240, :2258, :2597 | `self.hit_test(&rows, &geo, pos)` |
| `update` :2351 | `self.max_scroll_x(&rows, bounds, advance, line_h, state.scroll.rows(line_h))` |
| `mouse_interaction` :2633 | `self.armed_boxes(&rows, &geo, p)` |
| `mouse_interaction` :2642 | `self.collapsed_chip_at(&rows, &geo, p)` |
| `mouse_interaction` :2669 | `.hscrollbar(&rows, bounds, advance, line_h, state.scroll_x, state.scroll.rows(line_h))` |
| `collapsed_chip_at` body :3225, :3238 | drop `self.fold_map()`, add `let fold_map = rows.folds();`; `self.collapsed_chip_rect(rows, geo, o)` |
| `collapsed_chip_rect` body :3158, :3162, :3185 | unchanged (`fold_map` now from `rows.folds()`) |

Compile errors are the checklist: after the signature changes, `cargo build -p scrive-iced` lists
every caller.

### 7i. Widget tests (`mod tests`, :3929; expectations identical)

`collapsed_chip_at_windows_to_the_hovered_row` (:4398-4418) and
`block_fold_hover_target_excludes_text_after_the_closer` (:4420-4452): replace
`let fm = ed.fold_map();` with `let rows = doc.rows();`;
`ed.collapsed_chip_rect(&fm, &geo, opener)` → `ed.collapsed_chip_rect(&rows, &geo, opener)`;
every `ed.collapsed_chip_at(&geo, …)` → `ed.collapsed_chip_at(&rows, &geo, …)`.

`popup_anchors_are_display_space_below_a_fold` (:4521-4548): `let fm = ed.fold_map();` →
`let rows = doc.rows();`; `ed.popup_anchor(&fm, &geo, head)` →
`ed.popup_anchor(&rows, &geo, head, Edge::Caret)`. `popup_layout` / `hover_layout` calls are
unchanged.

`hit_test_inverts_offset_screen_x_across_fold_geometry` (:4552-4575). Before:

```rust
        let fm = ed.fold_map();
        …
            let p = fm.display_position(buffer, off, TAB).expect("landable offset");
            let pos = Point::new(ed.offset_screen_x(&fm, &geo, off), geo.row_y(p.row) + 5.0);
            assert_eq!(ed.hit_test(&geo, pos), off, "offset {off} round-trips");
```

After:

```rust
        let rows = doc.rows();
        …
            let p = rows.position(off, Edge::Caret).expect("landable offset");
            let pos = Point::new(ed.offset_screen_x(&rows, &geo, off, Edge::Caret), geo.row_y(p.row) + 5.0);
            assert_eq!(ed.hit_test(&rows, &geo, pos), off, "offset {off} round-trips");
```

`max_line_px_shrinks_when_the_widest_lines_fold` (:4612-4629). Before:

```rust
        let unfolded = Editor::new(&doc, |_: Action| ()).max_line_px(10.0, 20.0, vp, 0.0);
        …
        let folded = ed.max_line_px(10.0, 20.0, vp, 0.0);
        let fm = ed.fold_map();
        let hl = fm.header_layout(doc.buffer(), BufferRow(0), TAB).unwrap();
```

After (the temporaries in the first line drop before `toggle_fold_opener` borrows `doc` mutably):

```rust
        let unfolded = Editor::new(&doc, |_: Action| ()).max_line_px(&doc.rows(), 10.0, 20.0, vp, 0.0);
        …
        let rows = doc.rows();
        let folded = ed.max_line_px(&rows, 10.0, 20.0, vp, 0.0);
        let hl = rows.header(BufferRow(0)).unwrap();
```

`max_line_px_is_viewport_scoped` (:4632-4647): `let rows = doc.rows();` after `let ed = …`; both
`ed.max_line_px(…)` calls gain `&rows` first.

`deep_scroll_row_positions_stay_exact` (:4584) keeps `ed.fold_map()` (row logic only).

## 7. Files changed

| File | Commit | Change |
|---|---|---|
| crates/scrive-core/src/row_layout.rs | 1, 3 | `Edge`, `Rows` (+ memo), `HeaderLayout::new` and `Rc` head, `renders`, canary comment, 4 smoke tests (1); `display_position` removed, three methods `pub(crate)`, `display_cell(col, Edge)`, doc links, test rename/rewrite (3) |
| crates/scrive-core/src/document.rs | 1, 3 | `rows()`, `cached_fold_map`, `move_carets` / `add_caret_vertical` / `column_select` / `column_drag` / `caret_corner` / `column_box`, three probes on `renders`, doc/comments, 2 tests (1); `display_cell` edge in `caret_corner` (3) |
| crates/scrive-core/src/movement.rs | 1 | `move_selections`, `motion_target`, `vertical_by`, `caret_one_display_row` on `&Rows`; `mv` helper |
| crates/scrive-core/src/lib.rs | 1 | "Where do I…?" bullet |
| crates/scrive-core/src/fold_map.rs | 3 | intra-doc link at :849 |
| crates/scrive-core/src/perf.rs, perf_gate.rs | 3 | prose naming `display_position` (comment-only; R22 OQ-9) |
| crates/scrive-iced/src/editor.rs | 2, 3 | one `Rows` per pass, 15 helper signatures, 17 sites, edge picks, 6 test rewrites (2); `display_cell` edge at :1688, :3100 (3) |

## 8. Spot-check tables

### 8.1 The 23 geometry call sites

| # | Site (HEAD line) | Function | Before | After | Edge |
|---|---|---|---|---|---|
| 1 | movement.rs:207 | `vertical_by` | `folds.display_position(buffer, offset, tab)` | `rows.position(offset, Edge::Caret)` | `Caret` (vertical motion) |
| 2 | movement.rs:224 | `vertical_by` | `folds.hit_row(buffer, new_row, goal, Left, tab)` | `rows.hit(new_row, goal, Left)` | — |
| 3 | document.rs:907 | `caret_corner` | `folds.display_position(&self.buffer, head, tab)` | `rows.position(head, Edge::Caret)` | `Caret` (box corner) |
| 4 | document.rs:914 | `caret_corner` | `folds.row_layout(&self.buffer, BufferRow(p.row), tab)` | `rows.layout(BufferRow(p.row))` (+ `display_cell(.., Caret)` in commit 3) | `Caret` |
| 5 | document.rs:1038 | `rebuild_column_box` → `column_box` | `folds.hit_row(…anchor…)` | `rows.hit(…)` | — |
| 6 | document.rs:1039 | same | `folds.hit_row(…active…)` | `rows.hit(…)` | — |
| 7 | editor.rs:804 | `offset_screen_x` | `display_position` | `rows.position(offset, edge)` | caller's (7f) |
| 8 | editor.rs:816 | `popup_anchor` | `display_position` | `rows.position(offset, edge)` | caller's: `Start` completion/hover, `Caret` signature |
| 9 | editor.rs:856 | `any_caret_on_screen` | `display_position` | `rows.position(head, Edge::Caret)` | `Caret` (autoscroll) |
| 10 | editor.rs:898 | `collapsible_box_rect` | `row_layout(..).width()` | `rows.layout(..).width()` | `width()` |
| 11 | editor.rs:967 | `max_line_px` | `header_layout` | `rows.header` | `width()` |
| 12 | editor.rs:969 | `max_line_px` | `row_layout(..).width()` | `rows.layout(..).width()` | `width()` |
| 13 | editor.rs:1286 | `layout` autoscroll | `display_position` | `rows.position(head, Edge::Caret)` | `Caret` |
| 14 | editor.rs:1423 | `draw` `offset_xy` | `display_position` | `rows.position(off, edge)` | caller's: `Start` bracket box, caret rule |
| 15 | editor.rs:1605 | `draw` text pass | `row_layout` | `rows.layout` | glyphs `Start` (inside `draw_row_inline`, :3100) |
| 16 | editor.rs:1626 | `draw` header | `header_layout` | `rows.header` | — (tail cells) |
| 17 | editor.rs:1679 | `draw` bracket pass | `row_layout` | `rows.layout` | `Start` (:1688) |
| 18 | editor.rs:2993 | `draw_wash_row` | `header_layout` | `rows.header` | — (gap to `tail_cell`) |
| 19 | editor.rs:3019 | `draw_fold_tail_wash` | `header_layout` | `rows.header` | — (tail cells) |
| 20 | editor.rs:3074 | `hit_test` | `hit_row` | `rows.hit` | — |
| 21 | editor.rs:3168 | `collapsed_chip_rect` | `header_layout` | `rows.header` | — |
| 22 | editor.rs:3212 | `chip_pill_rect` | `header_layout` | `rows.header` | — (gap center) |
| 23 | editor.rs:3215 | `chip_pill_rect` | `row_layout` | `rows.layout` | — (chip center) |

### 8.2 Edge picks that are not themselves FoldMap call sites

| Line | What | Edge |
|---|---|---|
| editor.rs:909, 910 | collapsible box `xo`, `xc` | `Start` |
| editor.rs:1688 (commit 3) | bracket colour glyph | `Start` |
| editor.rs:1708 | matching-bracket box | `Start` |
| editor.rs:1755 | squiggle row start | `Start`; `Caret` if the diagnostic is empty |
| editor.rs:1756 | squiggle row end | `End` on the last row, `Start` on an interior row; `Caret` if empty |
| editor.rs:1843 | carets | `Caret` empty; `End` if head is the end; `Start` if reversed |
| editor.rs:2718 | completion anchor (word start) | `Start` |
| editor.rs:2749 | hover anchor (word start) | `Start` |
| editor.rs:2832 | signature anchor (caret) | `Caret` |
| editor.rs:2990 | wash start on the first row | `Start`; `Caret` if empty |
| editor.rs:2992 | wash end on the last row | `End`; `Caret` if empty |
| editor.rs:3001 | wash end of an interior row (line end) | `Start` |
| editor.rs:3100 (commit 3) | glyph runs | `Start` |
| editor.rs:3186, 3187 | inline chip hover rect | `Start` |
| row_layout.rs:258 (commit 3) | `caret_cell` | `Caret` |
| row_layout.rs:299 (commit 3) | `width` | `Start` |

### 8.3 Visibility probes

| Site | Before | After |
|---|---|---|
| document.rs:1573 `toggle_fold_opener` | `fold_map.display_position(buffer, s.head(), tab).is_none()` | `!fold_map.renders(buffer, s.head(), tab)` |
| document.rs:2334 `unfold_to_reveal` | `fm.display_position(&self.buffer, offset, tab).is_some()` | `fm.renders(&self.buffer, offset, tab)` |
| document.rs:2437 `expand_folds_touched` | `fold_map.display_position(buffer, p, tab).is_none()` | `!fold_map.renders(buffer, p, tab)` |
| canary | incremented in `display_position` | incremented in `renders` and `Rows::position` |

### 8.4 Borrow shapes in core

| Path | View built from | Writes while the view lives | Notes |
|---|---|---|---|
| `move_carets` | `cached_fold_map(&self.fold_cache)`, `&self.buffer`, `&self.inlays` | `&mut self.selections` (passed in) | disjoint fields |
| `add_caret_vertical` | same | `self.selections.add_caret` | inner block ends before `request_reveal(&mut self)` |
| `column_select` | same | `self.column =`, `self.selections =` | `caret_corner(&self, …)` in the closure is a shared borrow |
| `column_drag` | same | `self.selections =` | |
| `toggle_fold_opener`, `eject_hidden_carets`, `unfold_to_reveal`, `expand_folds_touched` | unchanged (`FoldMap::new` on fields / a sub-`FoldSet`) | — | no `Rows` |

### 8.5 Public API delta (scrive-core, rides 0.4.0)

| Item | Before | After |
|---|---|---|
| `row_layout::Edge`, `row_layout::Rows` | — | new, public |
| `Document::rows` | — | new, public |
| `movement::move_selections` | `(set, &Buffer, &FoldMap, tab, motion, extend)` | `(set, &Rows, motion, extend)` |
| `FoldMap::display_position` | `pub` | removed |
| `FoldMap::{row_layout, header_layout, hit_row}` | `pub` | `pub(crate)` |
| `RowLayout::display_cell` | `(col)` | `(col, Edge)` |

## 9. Verification

After each commit:

```
cargo build --workspace --all-targets
cargo test --workspace
cargo test --workspace --all-features
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --all-features
cargo build --workspace --all-targets --all-features --target wasm32-unknown-unknown
```

Targeted, after commit 1 (they must pass unchanged):

```
cargo test -p scrive-core typing_at_many_carets_over_folds_stays_linear
cargo test -p scrive-core fold_map_cache_matches
cargo test -p scrive-core keystroke_and_arrow_do_not_rebuild_the_fold_map_at_scale
cargo test -p scrive-core perf_gate
cargo test -p scrive-core column_
cargo test -p scrive-core add_caret_vertical
cargo test -p scrive-core rows_
```

After commit 2: `cargo test -p scrive-iced` (the draw-budget test, the hit/anchor/max-scroll tests).
After commit 3, the grep gates of §2 item 7. The test count is the Phase 1 baseline + 4.

Don't run `cargo run --example …` or `cargo fmt`.

## 10. What NOT to change

- No hint logic: no hint spans in `RowLayout`, no `inlay_at`, no consumer of `Edge` beyond passing
  it; every edge must keep producing the same cell. `caret_cell`, `hit`, `width`, `chips`,
  `glyph_hidden`, `HeaderLayout`'s cell math are untouched.
- No change to the fold cache (`FoldMapCache`, `ensure_fold_map`'s key, `apply_patch`), to
  `FoldMap::new`, or to `step_corner`, `hit_cell`, `eject_hidden_carets` / `entry_edge_if_hidden`.
- The probe sites keep building their `FoldMap` from the destructured fields or the `sub` set; do
  not route them through `Rows` (the inlay store is mutably borrowed inside `rebase_views`).
- No signature change to `expand_folds_touched`, `rebase_views`, `unfold_to_reveal`,
  `toggle_fold_opener`.
- No change to `draw_spans`, `expand_tabs`, `draw_row_inline`'s logic (only its `display_cell`
  call), `Geo`, or any scrive-lsp / `code_editor` code.
- No new dependencies; no flat re-export of `Rows`/`Edge`; no `#[allow(dead_code)]`.
- Phase 1's inlay store, `intel::inlay`, and its tests: untouched.
- Test expectations: none change. Only the call shapes listed in Steps 2l, 3f, 6e, 7i and the 4
  new tests.

## 11. Pitfalls

- **`RefCell` panics in core.** A `Rows` built in core holds a `Ref` into `fold_cache`. Never call
  `self.fold_map()`, `self.rows()` or `ensure_fold_map` while such a view is alive if the cache
  could be stale: a stale key takes `borrow_mut` and panics. Every path in Step 2 freshens first and
  edits no text while the view lives. In the widget this cannot happen (`Editor` holds
  `&Document`; nothing edits during a pass).
- **`Ref` has a destructor**, so the borrow lasts to the end of scope, not the last use. That is
  why `add_caret_vertical` needs its inner block before `self.request_reveal(…)`.
- **The memo.** Never return a `Ref` into `built`; clone the `Rc` out and let the shared borrow end
  before `borrow_mut` (the `let cached = …` statement in `Rows::layout`). A caller holding a returned
  layout while asking for another row (`draw_wash_row` → header, a header's head) is normal.
- **Disjoint borrows only work through field paths.** `Rows::new(cached_fold_map(&self.fold_cache),
  &self.buffer, &self.inlays, tab)` borrows three fields; a helper taking `&self` to build the view
  would borrow all of `self` and block the `self.selections` writes.
- **`Rc` deref coercion.** `&row_layout` (an `&Rc<RowLayout>`) coerces to `&RowLayout<'_>` at a
  call argument and in `HeaderLayout::head`'s return; a `let x: &RowLayout = …` binding needs
  `&*row_layout`.
- **Shadowing.** `motion_target`'s `Motion::PageUp(rows)` pattern would shadow the `rows`
  parameter: rename the pattern binding (3c). `layout`'s `let rows = match mode` would shadow the
  view: rename it (7c). In `draw_signature` and `draw_fold_preview` use the temporary
  `&self.doc.rows()`, not a binding named `rows`.
- **The editor's local `row_layout` variables** (:1605, :1679, `draw_row_inline`'s parameter) are
  values; they don't collide with the `scrive_core::row_layout` module path, but import
  `Edge`/`Rows` by name (7a) and don't write `row_layout::Edge` inside those functions.
- **Rustdoc.** A public item's doc must not link a `pub(crate)` or removed item
  (`rustdoc::private_intra_doc_links`, fatal under `-D warnings`). Public docs link
  `Rows::position` / `Rows::layout` / `Rows::hit`, never `FoldMap::renders`/`row_layout`/`hit_row`.
- **The perf meter.** `renders` charges `perf::charge(1)` exactly like `display_position` did and
  skips the `RowLayout` build on unfolded rows; `Rows::position` charges the same and reuses memoised
  layouts. Both lower the meter, never raise it; run the `perf_gate` tests anyway. The memo keeps
  every layout a view builds until it drops: a 10k-caret Up/Down holds ≤ 10k layouts for one call.
  That is the D5 design; don't add eviction.
- **clippy `too_many_arguments`** (limit 7 inputs incl. `self`): `hscrollbar`, `draw_selection` and
  `collapsible_box_rect` land exactly at 7. Don't add a parameter beyond those in 7d.
- **Never run `cargo fmt`**; the tree is not rustfmt-clean.

## 12. Resolved questions

All confirmed as proposed (R22):

- **OQ-1, OQ-2:** the `_inlays` field and the `_edge` parameters (in `Rows::position` and
  `RowLayout::display_cell`) stay until Phase 3 consumes and renames them.
- **OQ-3:** `RowLayout::display_cell` takes an `Edge` in this phase (commit 3).
- **OQ-4:** `FoldMap::header_layout` and `FoldMap::hit_row` stay `pub(crate)` and in use
  (`renders`, `Rows::hit`); `display_position` is deleted. Phase 3 moves `Rows::hit` onto the
  memo when it becomes hint-aware.
- **OQ-5:** the Phase 1 field is `inlays` (MAP_PHASE_1 Step 6b).
- **OQ-6:** the plan-wording corrections stand (`caret_one_display_row` takes `&Rows`;
  crate-private `Rows::{buffer, tab}`; the row_layout.rs `display_position` test moves).
- **OQ-7:** the extra helpers (`max_scroll_x`, `hscrollbar`, the nine `&FoldMap` helpers, the
  temporary `&self.doc.rows()` for popup anchors and the fold preview) are in scope.
- **OQ-8:** `column_box(&Rows, col) -> SelectionSet` and `caret_corner(&self, &Rows)`.
- **OQ-9:** perf.rs and perf_gate.rs prose is updated (MAP_PLAN's Files touched now lists them).
- **OQ-10:** no top-level re-export of `Rows` or `Edge`.

Still open: none.
