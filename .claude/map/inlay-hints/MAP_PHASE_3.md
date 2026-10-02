# Phase 3: hint-aware layout (scrive-core)

This doc specifies Phase 3 of the inlay-hints plan (`MAP_PLAN.md`, Draft 7) and is meant to be
enough on its own. It restates the parts of D3, D4, D5, D6 and D7 that this phase implements.
Line numbers are HEAD `8e72665` numbering. Phases 1 and 2 move code in every file this phase
touches, so **locate each site by the function or item named**, never by number alone.

Read `.claude/map/lsp-bridge/DISPATCH.md` first, with the substitutions from the plan's
Appendix A: the map directory is `.claude/map/inlay-hints/`, patches go to
`.claude/map/inlay-hints/patches/`, and the `--all-features` clippy run applies to every phase.

## Prerequisites

### What Phases 1 and 2 must have left behind

This phase builds on APIs that do not exist at `8e72665`. The names below are the ones
MAP_PHASE_1 and MAP_PHASE_2 specify (after the RESOLUTIONS.md audit). Confirm each one before
writing code; if a piece is missing, stop and report.

Phase 1 (`crates/scrive-core/src/intel/inlay.rs`, `decorations.rs`, `document.rs`):

| Item | Used here for | Check |
|---|---|---|
| `inlay::Side { Suffix, Prefix }` | edge selection, sort | `grep -n "pub enum Side" crates/scrive-core/src/intel/inlay.rs` |
| `inlay::Anchor` (private fields `hint: Arc<Hint>`, `side`, `anchored`, `index`) with crate-private `hint() -> &Hint`; this phase adds `side()` and `index()` (Step 1) | sort, `inlay_at` | `grep -n "impl Anchor" -A 40 crates/scrive-core/src/intel/inlay.rs` |
| `Anchor::render_offset(&self, range: Range<u32>, row_start: u32, row_end: u32, line: &str) -> Option<u32>` (R4) | the row filter | `grep -n "fn render_offset" crates/scrive-core/src/intel/inlay.rs` |
| `Hint::{kind(), parts() -> &[Part], padded() -> Padding, insertable() -> bool, key() -> Key, width() -> u32}`; `Part::{text(), link()}`; `Padding { pub left, pub right }` (R1) | widths, `inlay_at`, `inlays()` | same file |
| `inlay::Key: Copy + Eq + Hash + Debug`, `inlay::Link`, `inlay::Insert` | `inlay::At` | same file |
| `DecorationKind::InlayHint(inlay::Anchor)` | the row filter | `grep -n "InlayHint" crates/scrive-core/src/decorations.rs` |
| `DecorationStore::visit_in<'s>(&'s self, range, FnMut(Range<u32>, &'s DecorationKind))` and the `'s`-tied `SumTree::filter_visit` (R8) | the row filter | `grep -n "fn visit_in" crates/scrive-core/src/decorations.rs` |
| `Document::set_inlays(revision, Vec<inlay::Placed>) -> inlay::Outcome`, `inlays_in`, `remove_inlay(key, offset) -> bool`, `inlays_revision` | tests | `grep -n "fn set_inlays\|fn inlays_in\|fn remove_inlay" crates/scrive-core/src/document.rs` |
| `Hint::new(kind, parts, key)`, `.padding(..)`, `.insert(..)`, `.placement(..)`, `Part::new(text, link)`, `Placed::new(offset, hint)`, `Key::new(u64)` | tests | same files |
| the dedicated inlay `DecorationStore` field `Document::inlays` | `Rows` | `grep -n "inlays" crates/scrive-core/src/document.rs` |

Phase 2 (`row_layout.rs`, `fold_map.rs`, `document.rs`, `movement.rs`):

| Item | Check |
|---|---|
| `row_layout::Edge { Start, End, Caret }` | `grep -n "pub enum Edge" crates/scrive-core/src/row_layout.rs` |
| `row_layout::Rows<'a>` holding `Ref<FoldMap>`, `&Buffer`, `_inlays: &DecorationStore` (unread until this phase, R22) and `tab`, with `layout(row) -> Rc<RowLayout>` (built by `self.folds.row_layout(..)`), `header(row)`, `position(offset, _edge)` (calls `caret_cell`, edge unused), `hit(row, cell, Bias)` (delegates to `FoldMap::hit_row`), `folds()`, crate-private `buffer()` / `tab()`, and the `RefCell` memo | `grep -n "impl<'a> Rows" -A 80 crates/scrive-core/src/row_layout.rs` |
| no `Rows::inlay_at` and no edge-taking `CaretCell` method: this phase adds both (`inlay_at`, `edge_cell`) | same |
| `Document::rows()` and crate-private `Rows::new(Ref<FoldMap>, &Buffer, &DecorationStore, tab)` | `grep -n "pub fn rows\|fn new(" crates/scrive-core/src/{document,row_layout}.rs` |
| `RowLayout::display_cell(col, _edge: Edge)` (edge unused), `caret_cell(col)` | `grep -n "pub fn display_cell\|pub fn caret_cell\|-> CaretCell" crates/scrive-core/src/row_layout.rs` |
| `FoldMap::{row_layout, header_layout, hit_row}` `pub(crate)`; `FoldMap::renders` answers through `header_layout` | `grep -n "pub(crate) fn row_layout\|pub(crate) fn header_layout\|pub(crate) fn hit_row\|fn renders" crates/scrive-core/src/row_layout.rs` |
| `HeaderLayout` holding `Rc<RowLayout>` for its head | `grep -n "head:" crates/scrive-core/src/row_layout.rs` |
| `FoldMap::renders(buffer, offset, tab) -> bool` counting `DISPLAY_POSITION_PROBES`, and `Rows::position` counting it too | `grep -n "fn renders\|DISPLAY_POSITION_PROBES" crates/scrive-core/src/*.rs` |
| `move_selections(set, &Rows, motion, extend)`; `vertical_by` calling `rows.position(offset, Edge::Caret)` and `rows.hit(..)` | `grep -n "fn vertical_by\|fn move_selections" -A 6 crates/scrive-core/src/movement.rs` |
| `caret_corner(&self, &Rows)` using `Edge::Caret`; `column_box(&Rows, col) -> SelectionSet`; `step_corner(&FoldMap, ..)` unchanged; `column_select` / `column_drag` / `add_caret_vertical` on the cached fold map through `Rows` | `grep -n "fn caret_corner\|fn step_corner\|fn column_box" -A 12 crates/scrive-core/src/document.rs` |

### Baseline

- `git log --oneline -1` is the Phase 2 tip; `git status` shows only `.claude/`.
- Green: `cargo test --workspace --all-features`, both clippy runs, the doc build.
- Read in full before editing: `row_layout.rs`, `movement.rs`, the `Rows`/inlay parts of
  `document.rs`, `intel/inlay.rs`, and `decorations.rs` (`visit_in`, `count_visit`, the
  canaries).

## Goal and exit criteria

**Goal.** `RowLayout` lays out the inlay hints of its row. Every projection that Phase 2 routed
through `Rows` and an `Edge` now produces hint-aware cells: carets, hits, widths, chips, the
collapsed header's gap and tail, vertical motion and box selection. With an empty inlay store
every projection returns exactly what it returned at the Phase 2 tip.

This phase is core-only. No scrive-iced or scrive-lsp file changes; the widget already passes
an edge at every site (Phase 2), and painting is Phase 4.

**Exit.**

1. The identity sweep passes against the golden table recorded at the Phase 3 base.
2. All tests in "Tests to add" pass.
3. `typing_at_many_carets_over_folds_stays_linear` and
   `keystroke_and_arrow_do_not_rebuild_the_fold_map_at_scale` pass unchanged.
4. The whole workspace is green under the verification commands, both feature sets.

## Design decisions implemented

Restated from the plan. The plan's wording wins over this summary if they ever disagree.

**D3, the anchor and where a hint renders.** A hint is stored as a range over the token it
annotates, in the dedicated inlay store:

| Side | Annotates | Stored range | Renders at |
|---|---|---|---|
| `Suffix` | the token ending at `p` | `[word_start, p)` | `min(range end, end of the row holding range start)` |
| `Prefix` | the token starting at `p` | `[p, word_end)` | `max(range start, min(range end, first non-blank of the row holding range end))` |

- **Row clamp.** The `min`/`max` above keep a hint on its own line when Enter grows its range
  across a newline. `RowLayout` filters the row's touching query by the clamped render row:
  a `Suffix` hint belongs to the row holding its range start, a `Prefix` hint to the row
  holding its range end.
- **One owner.** `Anchor::render_offset` (Phase 1) computes the render offset from the row's
  byte bounds and line text, with no `offset_to_point` per hit. The `RowLayout` row filter,
  `remove_inlay`, `inlay_at` and the future `InlayInsert` offset all use it.
- **No render guard** (R11). A hint renders wherever `render_offset` puts it, even between two
  word characters (`||X -> fn()<…>f`).

**D4, order.** The store orders by `(start, id)`, which is not render order (a `Suffix` range
starts at its word start). `RowLayout` sorts the row's hints by
`(render column, Prefix before Suffix, server index)`.

**D5, the view.** `Rows` memoises built layouts per row for its lifetime, so the extra store
query costs one windowed descent per row per `Rows`. The row query uses Phase 1's borrowing
`visit_in` (R8), not the owned `decorations_in` (decorations.rs:549-581), which clones every
hit's kind.

**D6, edges.** For offset `p`, `Edge::Start` is after every hint at `p`, `Edge::End` before
every hint at `p`, and `Edge::Caret` after the `Prefix` hints at `p` and before the `Suffix`
hints, exactly where the next typed character lands under D3. At install every offset holds one
side only. Mixed groups form only after edits; then the render order is the `Prefix` hints, then
the `Suffix` hints, each in server order, with the caret between the groups. An empty range uses
`Caret` for both ends, because `Start..End` would be inverted.

**D7, layout math.**

- `RowLayout` gains a sorted per-row hint list `{ col, raw_cell, width, side }` from a windowed
  store query (`O(log n + hits)`).
- `display_cell(col, edge)` adds the widths of the hints before `col`, plus the hints at `col`
  that the edge selects. `caret_cell(col)` uses `Edge::Caret`.
- `hit(cell, bias)` maps any cell on a hint, label or padding, to the hint's offset.
- `inlay_at(cell)` returns an `inlay::At` (R3): `Label { key, part, offset, link, insert, cells }`
  for a label part, `Padding { key, offset }` for a padding cell, `None` elsewhere. `Padding` is
  inert: no hover, no link, and no fall-through to the word beneath (D7); clicks still use `hit`.
- `RowLayout::inlays()` yields each laid-out hint as `&row_layout::Inlay { key, offset, cell,
  width, padding, hint: &Hint }` in render order (R2), `cell` being its first display cell,
  left padding included. Phase 4 paints from this only.
- `width()` includes the hints at the line end.
- `is_plain()` is false on a row with hints.
- Tabs after a hint keep their buffer-space width (the fixed-shift model), as they do after a chip.
- **Folds.** A hint whose render offset `o` is inside a collapsed inline fold's hidden interior
  (`open < o <= close`) is not laid out. Hints on block-folded rows, including the collapsed
  header's tail row, are not shown. Hints on the header row are, and `HeaderLayout::head_cells`
  and `tail_cell` derive from the hinted head layout, so the gap and tail shift past them.
- **Box selection** steps Left/Right by character within the line's content and re-projects with
  `Caret`; past the line end it keeps stepping by cell (`c.cell + 1`, document.rs:1019).
- **Identity.** With no hints every projection returns exactly what it returns today.

### Decisions this doc makes where the plan is silent

- **Decision: the identity sweep is a golden table** recorded at the Phase 3 base. After Phase 2
  there is no pre-refactor code left to compare against, so commit 1 records the hint-free
  projections of a fixture as text, and later commits must keep it byte-identical.
- **Decision: composition order is tabs, then chips, then hints.** A column's cell is
  `cell_of(expand(line, col, tab)) + hint widths`. `shift_at` keeps comparing raw (pre-collapse,
  pre-hint) cells, so the chip math is untouched. This is what "tabs after a hint keep their
  buffer-space width" means.
- **Decision: `RowLayout::new` skips the store query on a block-folded row**
  (`fold_map.is_folded(row)`). The widget never lays such a row out, but the `caret_corner`
  fallback and tests can, and the plan says these hints are not shown.
- **Decision: "by character" in box selection means the next landable caret stop on the row**:
  the next char boundary, snapped out of a collapsed inline fold's gap to its right edge (left
  edge going left). This is `movement::char_right`/`char_left` semantics restricted to one row.
- **Decision: `RowLayout` hint spans borrow their `Anchor` from the store** (`&'a inlay::Anchor`),
  so `inlay_at` can read the parts without cloning an `Arc` per hit and without a second query.
  Phase 1's `visit_in` yields `&'s` items for exactly this (R8).
- **Decision: `Rows::hit` and `Rows::position` become hint-aware here.** Phase 2 left
  `Rows::position` ignoring its edge and `Rows::hit` delegating to `FoldMap::hit_row`, which
  builds a hint-free layout. This phase routes both through the memoised, hinted layouts (Steps
  3 and 4). `FoldMap::row_layout` and `FoldMap::header_layout` stay as the hint-free builders
  that `FoldMap::renders` needs (the visibility probes never see hints, D5); `FoldMap::hit_row`
  loses its last non-test caller and is deleted.

Settled by RESOLUTIONS.md: `inlay_at` returns `inlay::At` (R3); box selection steps by
character everywhere within content, by cell past the line end, with or without hints (R12);
this phase adds the painting accessor `RowLayout::inlays()` (R2).

## Step-by-step changes

### Step 1. The anchor's accessors and the hit result (intel/inlay.rs)

The borrowing row query is Phase 1's `DecorationStore::visit_in` (R8); use it as is.

Two crate-private accessors on `Anchor`, beside `hint()` (the row filter sorts by them):

```rust
    pub(crate) fn side(&self) -> Side {
        self.side
    }

    pub(crate) fn index(&self) -> u32 {
        self.index
    }
```

The hit result (R3), after `Shown`. Like `Chip` and `TailGlyph` it is a plain output view: public
fields, no invariant a caller could break.

```rust
/// What a display cell on an inlay hint holds.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum At {
    /// A label part.
    Label {
        /// The hint's key.
        key: Key,
        /// Index of the part in the hint's label.
        part: u32,
        /// The buffer offset the hint renders at.
        offset: u32,
        /// Whether the part leads somewhere.
        link: Link,
        /// Whether the host can insert the hint as text.
        insert: Insert,
        /// The part's display cells on the row.
        cells: Range<u32>,
    },
    /// A padding cell: editor background, never a hover or link target, and
    /// not the word beneath either.
    Padding {
        /// The hint's key.
        key: Key,
        /// The buffer offset the hint renders at.
        offset: u32,
    },
}
```

`Range` is already imported there (Phase 1, Step 4).

### Step 2. Hint spans in `RowLayout` (row_layout.rs)

New imports (no aliases):

```rust
use crate::decorations::DecorationKind;
use crate::intel::inlay;
```

(`DecorationStore` is already imported since Phase 2.)

The painting view (R2), public, next to `Chip`:

```rust
/// One inlay hint as it lays out on a row: what [`RowLayout::inlays`] yields.
#[derive(Clone, Copy, Debug)]
pub struct Inlay<'a> {
    /// The hint's key.
    pub key: inlay::Key,
    /// The buffer offset the hint renders at.
    pub offset: u32,
    /// The hint's first display cell, its left padding included.
    pub cell: u32,
    /// Cells the hint takes, padding included.
    pub width: u32,
    /// The blank cells around its label.
    pub padding: inlay::Padding,
    /// The hint itself, for its label parts.
    pub hint: &'a inlay::Hint,
}
```

A private span type next to `InlineSpan`:

```rust
/// One inlay hint laid out on a row.
#[derive(Copy, Clone, Debug)]
struct HintSpan<'a> {
    /// Byte column of the hint's render offset.
    col: u32,
    /// Tab-expanded, pre-collapse cell of `col`.
    raw_cell: u32,
    anchor: &'a inlay::Anchor,
    /// What `inlays()` hands out; `cell` is placed once the row's chips are known.
    inlay: Inlay<'a>,
}

impl HintSpan<'_> {
    fn width(&self) -> u32 {
        self.inlay.width
    }

    fn side(&self) -> inlay::Side {
        self.anchor.side()
    }
}
```

`RowLayout` before (row_layout.rs:183-191) and after:

```rust
// before
pub struct RowLayout<'a> {
    line: Cow<'a, str>,
    row_start: u32,
    tab: u32,
    spans: Vec<InlineSpan>,
}
// after
pub struct RowLayout<'a> {
    line: Cow<'a, str>,
    row_start: u32,
    tab: u32,
    /// This row's root inline folds, sorted by opening cell.
    spans: Vec<InlineSpan>,
    /// This row's laid-out inlay hints in render order.
    hints: Vec<HintSpan<'a>>,
}
```

Update the struct's doc comment: it now also borrows the row's hints from the inlay store, and it
still holds nothing derived that could outlive its borrows. Add one line to the module doc saying
the layout includes inlay hints.

`RowLayout::new` gains the store as an `Option`. `None` builds the hint-free layout that
`FoldMap::row_layout` keeps producing for `FoldMap::header_layout` → `FoldMap::renders` (the
visibility probes, which run inside `rebase_views` while the inlay store is borrowed mutably, D5)
and for the in-file tests that call `fm.row_layout(..)`. `Rows::layout` passes `Some`:

```rust
fn new(fold_map: &FoldMap, buffer: &'a Buffer, inlays: Option<&'a DecorationStore>, row: BufferRow, tab: u32) -> Self {
    let line = buffer.line(row.0);
    let row_start = buffer.point_to_offset(Point::new(row.0, 0));
    let mut spans: Vec<InlineSpan> = /* unchanged */;
    spans.sort_by_key(|s| s.open_cell);
    let hints = match inlays {
        Some(store) if !fold_map.is_folded(row) => hints_on_row(store, &line, row_start, &spans, tab),
        _ => Vec::new(),
    };
    let mut layout = Self { line, row_start, tab, spans, hints };
    // `cell_of` needs the spans, so each hint's first cell is placed once they are in.
    let mut prior = 0;
    for i in 0..layout.hints.len() {
        layout.hints[i].inlay.cell = layout.cell_of(layout.hints[i].raw_cell) + prior;
        prior += layout.hints[i].width();
    }
    layout
}
```

`FoldMap::row_layout` passes `None`. In `Rows` (Phase 2), rename the `_inlays` field to `inlays`
and build through the store:

```rust
// Rows::new
Self { folds, buffer, inlays, tab, built: RefCell::new(HashMap::new()) }
// Rows::layout, in place of `self.folds.row_layout(self.buffer, row, self.tab)`
let layout = Rc::new(RowLayout::new(&self.folds, self.buffer, Some(self.inlays), row, self.tab));
```

The row filter, a private free function below the `impl` blocks:

```rust
/// The hints that render on this row, in `(column, Prefix before Suffix, server index)` order.
fn hints_on_row<'a>(
    inlays: &'a DecorationStore,
    line: &str,
    row_start: u32,
    spans: &[InlineSpan],
    tab: u32,
) -> Vec<HintSpan<'a>> {
    let row_end = row_start + line.len() as u32;
    let mut out = Vec::new();
    inlays.visit_in(row_start..row_end, |range, kind| {
        let DecorationKind::InlayHint(anchor) = kind else {
            debug_assert!(false, "the inlay store holds only inlay hints");
            return;
        };
        let Some(offset) = anchor.render_offset(range, row_start, row_end, line) else { return };
        if spans.iter().any(|s| s.fold.open < offset && offset <= s.fold.close) {
            return;
        }
        let col = offset - row_start;
        let hint = anchor.hint();
        let inlay = Inlay { key: hint.key(), offset, cell: 0, width: hint.width(), padding: hint.padded(), hint };
        out.push(HintSpan { col, raw_cell: display_map::expand(line, col, tab), anchor, inlay });
    });
    out.sort_by_key(|h| (h.col, side_rank(h.side()), h.anchor.index()));
    out
}

fn side_rank(side: inlay::Side) -> u8 {
    match side {
        inlay::Side::Prefix => 0,
        inlay::Side::Suffix => 1,
    }
}
```

The painting accessor (R2), in `impl RowLayout`:

```rust
/// This row's laid-out inlay hints, in render order.
pub fn inlays(&self) -> impl Iterator<Item = &Inlay<'a>> + '_ {
    self.hints.iter().map(|h| &h.inlay)
}
```

Notes:

- `render_offset` takes the stored range as an argument (R4): the anchor does not hold its range;
  the store does.
- `render_offset` returns an offset inside `[row_start, row_end]` or `None`. A `debug_assert!`
  on that, plus `line.is_char_boundary(col as usize)`, is cheap and catches a Phase 1 slip.
- There is no mid-word render guard (R11).
- The fold test is `open < o <= close` with `InlineFold` offsets; it hides a hint at `open + 1`
  and at the closing bracket, and keeps one at `open` (rendered before the `[`).
- `display_map::expand` slices the line; render offsets are char boundaries because every edit
  and anchor is.

### Step 3. Edges and the forward projection (row_layout.rs)

Add to `Edge` (Phase 2's enum, same file):

```rust
impl Edge {
    /// Whether a hint of `side` at the projected offset lies left of this edge.
    fn passes(self, side: inlay::Side) -> bool {
        match (self, side) {
            (Self::Start, inlay::Side::Prefix | inlay::Side::Suffix) => true,
            (Self::End, inlay::Side::Prefix | inlay::Side::Suffix) => false,
            (Self::Caret, inlay::Side::Prefix) => true,
            (Self::Caret, inlay::Side::Suffix) => false,
        }
    }
}
```

No `_` arms: both enums are ours (DISPATCH self-review).

In `impl RowLayout`:

```rust
/// Total width of the hints left of byte column `col` under `edge`.
fn hint_cells(&self, col: u32, edge: Edge) -> u32 {
    self.hints
        .iter()
        .take_while(|h| h.col <= col)
        .filter(|h| h.col < col || edge.passes(h.side()))
        .map(HintSpan::width)
        .sum()
}
```

`display_cell` (row_layout.rs:245 at HEAD; Phase 2 added the `edge` parameter):

```rust
// before (Phase 2)
pub fn display_cell(&self, col: u32, _edge: Edge) -> u32 {
    self.cell_of(display_map::expand(&self.line, col, self.tab))
}
// after
pub fn display_cell(&self, col: u32, edge: Edge) -> u32 {
    self.cell_of(display_map::expand(&self.line, col, self.tab)) + self.hint_cells(col, edge)
}
```

Doc comment: byte column to display cell, with tabs, collapsed inline folds and inlay hints;
total and monotone for each edge, and `End <= Caret <= Start` at every column. Keep the note that
a column hidden inside a chip maps into the chip's span.

The chip's left cell appears in three places (`caret_cell`, `edge_cell`, `chips`). Give it one
owner:

```rust
/// Display cell of the chip's first cell for inline span `s`.
fn chip_cell(&self, s: &InlineSpan) -> u32 {
    self.cell_of(s.open_cell + 1) + self.hint_cells(s.fold.left_edge() - self.row_start, Edge::End)
}
```

`hint_cells(open + 1, End)` counts the hints at columns `<= open`. No hint renders at
`open + 1` (Step 2 hides it), so this is exact.

`caret_cell` (row_layout.rs:252-260) and a new edge-taking sibling, `edge_cell` (Phase 2 added
none; `Rows::position` needs one):

```rust
// before (HEAD)
pub fn caret_cell(&self, col: u32) -> CaretCell {
    let off = self.row_start + col;
    match self.spans.iter().find(|s| s.fold.hides_caret_at(off)) {
        Some(s) => CaretCell::ChipCenter(
            self.cell_of(s.open_cell + 1) as f32 + INLINE_CHIP_CELLS as f32 / 2.0,
        ),
        None => CaretCell::Cell(self.display_cell(col)),
    }
}
// after
pub fn edge_cell(&self, col: u32, edge: Edge) -> CaretCell {
    let off = self.row_start + col;
    match self.spans.iter().find(|s| s.fold.hides_caret_at(off)) {
        Some(s) => CaretCell::ChipCenter(self.chip_cell(s) as f32 + INLINE_CHIP_CELLS as f32 / 2.0),
        None => CaretCell::Cell(self.display_cell(col, edge)),
    }
}

pub fn caret_cell(&self, col: u32) -> CaretCell {
    self.edge_cell(col, Edge::Caret)
}
```

`edge_cell` is `pub`, like `caret_cell`. `Rows::position` (Phase 2) now honours its edge:
rename `_edge` to `edge` and, on an unfolded row, call
`self.layout(row).edge_cell(p.col, edge)` in place of `caret_cell(p.col)`. The folded-tail
branch keeps `tail_col_cell` (hints on block-folded rows are not shown). Rename
`display_cell`'s `_edge` to `edge` too (below).

`chips()` (row_layout.rs:303-313): `let cell = self.chip_cell(s);` in place of
`self.cell_of(s.open_cell + 1)`.

`width()` (row_layout.rs:298-300):

```rust
pub fn width(&self) -> u32 {
    self.display_cell(self.line.len() as u32, Edge::Start)
}
```

Doc comment: the rendered width in cells, including hints at the line end.

`is_plain()` (row_layout.rs:214-216):

```rust
pub fn is_plain(&self) -> bool {
    self.spans.is_empty() && self.hints.is_empty()
}
```

Doc comment: whether the row has no collapsed inline folds and no inlay hints.

`glyph_hidden` and `row_start` do not change.

### Step 4. The inverse projection (row_layout.rs)

Split today's `hit` (row_layout.rs:276-292) into the hint pass and the unchanged chip pass:

```rust
pub fn hit(&self, cell: f32, bias: Bias) -> u32 {
    let dc = cell.round().max(0.0) as u32;
    let mut passed = 0;
    let mut i = 0;
    while i < self.hints.len() {
        let col = self.hints[i].col;
        let group: u32 = self.hints[i..].iter().take_while(|h| h.col == col).map(HintSpan::width).sum();
        let left = self.cell_of(self.hints[i].raw_cell) + passed;
        if dc < left {
            break;
        }
        if dc <= left + group {
            return col;
        }
        passed += group;
        i += self.hints[i..].iter().take_while(|h| h.col == col).count();
    }
    self.hit_unhinted(dc - passed, bias)
}

/// [`Self::hit`] on a row with its hints removed: `dc` is a whole display cell
/// in hint-free display space.
fn hit_unhinted(&self, dc: u32, bias: Bias) -> u32 {
    let mut extra = 0i32;
    for s in &self.spans {
        let d_open = self.cell_of(s.open_cell);
        let d_chip_end = d_open + 1 + INLINE_CHIP_CELLS;
        if dc >= d_chip_end {
            extra += (s.close_cell as i32 - s.open_cell as i32 - 1) - INLINE_CHIP_CELLS as i32;
        } else if dc > d_open {
            return s.fold.left_edge() - self.row_start;
        }
    }
    let raw_cell = (dc as i32 + extra).max(0) as u32;
    display_map::collapse(&self.line, raw_cell, self.tab, bias)
}
```

Write the group walk however reads best (a `chunk_by` over `self.hints` keyed on `col` is the
cleanest; `slice::chunk_by` is stable since Rust 1.77). What matters:

- A group is all the hints at one column. `left` is `display_cell(col, End)`, the boundary
  before the group; `left + group` is `display_cell(col, Start)`. Every boundary in
  `[left, left + group]` lands on `col`, so a click on a label or its padding, rounded either way,
  lands on the hint's offset.
- `passed` sums the groups wholly left of `dc`. Subtracting it puts `dc` in hint-free display
  space, where the chip pass runs exactly as before. No hint sits inside a chip, so a hinted cell
  on a chip still lands on the chip.
- Two groups are never adjacent: distinct columns have at least one visible character between
  them, and every visible character is at least one cell.
- `hit(display_cell(col, e), _) == col` for every landable column and edge (the expert
  critique's round-trip property); a test pins it.

Keep the doc comment's rounding-policy sentence and add: a cell on a hint, label or padding,
resolves to the hint's offset.

`Rows::hit` (Phase 2 delegates to `FoldMap::hit_row`, which builds a hint-free layout) moves
onto the memo, with `hit_row`'s body:

```rust
pub fn hit(&self, row: BufferRow, cell: f32, bias: Bias) -> u32 {
    if let Some(header) = self.header(row) {
        match header.hit(cell, bias) {
            HeaderHit::Tail(col) => return self.buffer.point_to_offset(Point::new(header.last_row().0, col)),
            HeaderHit::Gap => return self.buffer.point_to_offset(Point::new(row.0, self.buffer.line_len(row.0))),
            HeaderHit::Head => {}
        }
    }
    self.buffer.point_to_offset(Point::new(row.0, self.layout(row).hit(cell, bias)))
}
```

Delete `FoldMap::hit_row` (row_layout.rs:476 at HEAD): it has no other non-test caller, and a
`pub(crate)` method used only by tests fails `-D warnings`. Its in-file tests call `rows.hit`
instead, with the same expectations (they have no hints). Fix the intra-doc links that named it
(`ColumnSelection`'s and `vertical_by`'s docs already link `Rows::hit` since Phase 2).

`HeaderLayout` needs no code change. `head_cells()` is `self.head.width()` and `tail_cell()`
and `gap_center()` derive from it, so they shift past header hints, including end-of-line ones.
`HeaderLayout::hit` sends a cell on a header hint to `HeaderHit::Head`, then `RowLayout::hit`.
Update `head_cells`' doc comment to say inlay hints on the header widen it.

### Step 5. `inlay_at` (row_layout.rs)

It returns Step 1's `inlay::At` (R3):

```rust
/// What the inlay hint under fractional display cell `cell` holds there: a
/// label part, or its padding. `None` off every hint.
#[must_use]
pub fn inlay_at(&self, cell: f32) -> Option<inlay::At> {
    if cell < 0.0 {
        return None;
    }
    let k = cell.floor() as u32;
    for h in &self.hints {
        let Inlay { key, offset, cell: first, width, padding, hint } = h.inlay;
        if k < first {
            return None;
        }
        if k >= first + width {
            continue;
        }
        let mut at = first + u32::from(padding.left);
        for (part, p) in hint.parts().iter().enumerate() {
            let n = p.text().chars().count() as u32;
            if (at..at + n).contains(&k) {
                let insert = if hint.insertable() { inlay::Insert::Available } else { inlay::Insert::Unavailable };
                return Some(inlay::At::Label { key, part: part as u32, offset, link: p.link(), insert, cells: at..at + n });
            }
            at += n;
        }
        return Some(inlay::At::Padding { key, offset });
    }
    None
}
```

`first` is the hint's own first cell (`Inlay::cell`, placed in `RowLayout::new` from the widths
of every hint before it in render order). `inlay_at` floors (it asks "which cell is the pointer
over"); `hit` rounds (it asks "which caret boundary is nearest"). A cell inside a hint but on no
label part is padding (an empty part takes no cell).

`Rows::inlay_at`:

```rust
/// What the inlay hint under `cell` on visible `row` holds there; `None` over text, or a
/// collapsed header's gap and tail.
#[must_use]
pub fn inlay_at(&self, row: BufferRow, cell: f32) -> Option<inlay::At> {
    if let Some(header) = self.header(row) {
        if header.hit(cell, Bias::Left) != HeaderHit::Head {
            return None;
        }
    }
    self.layout(row).inlay_at(cell)
}
```

No crate-root re-export (Phase 2 adds none for `Rows`/`Edge`, R22): callers write
`scrive_core::intel::inlay::At` and `scrive_core::row_layout::Inlay`.

### Step 6. Vertical motion (movement.rs)

No code change expected. After Phase 2, `vertical_by` (movement.rs:204-226 at HEAD) reads the
caret's cell with `rows.position(offset, Edge::Caret)` and lands with `rows.hit(..)`, so the goal
column is the caret's visual column and a goal on a hint lands on the hint's offset. Verify both
calls; if Phase 2 used any other edge for the read, change it to `Edge::Caret`. Update the doc
comment's list of things the goal survives: add "inlay hints". Add the tests listed below.

### Step 7. Box selection by character (row_layout.rs, document.rs) (R12)

Two crate-private methods on `RowLayout`, beside `hit`:

```rust
/// The box corner cell one step right of `cell`: the next caret stop while inside the
/// line's content, one cell at a time past it.
pub(crate) fn step_right(&self, cell: u32) -> u32 {
    let width = self.width();
    if cell >= width {
        return cell + 1;
    }
    let col = self.hit(cell as f32, Bias::Left);
    let here = self.stop_cell(col);
    if here > cell {
        return here;
    }
    self.next_stop(col).map_or(width, |next| self.stop_cell(next))
}

/// The box corner cell one step left of `cell`, mirroring [`Self::step_right`].
pub(crate) fn step_left(&self, cell: u32) -> u32 {
    if cell > self.width() {
        return cell - 1;
    }
    let col = self.hit(cell as f32, Bias::Left);
    let here = self.stop_cell(col);
    if here < cell {
        return here;
    }
    self.prev_stop(col).map_or(0, |prev| self.stop_cell(prev))
}

fn stop_cell(&self, col: u32) -> u32 {
    virtual_cell(self.caret_cell(col).cells())
}

/// The next landable column after `col` on this row, hopping a collapsed inline gap.
fn next_stop(&self, col: u32) -> Option<u32> {
    let ch = self.line[col as usize..].chars().next()?;
    let next = self.row_start + col + ch.len_utf8() as u32;
    let landed = self.spans.iter().find(|s| s.fold.hides_caret_at(next)).map_or(next, |s| s.fold.right_edge());
    Some(landed - self.row_start)
}

/// The previous landable column before `col` on this row, hopping a collapsed inline gap.
fn prev_stop(&self, col: u32) -> Option<u32> {
    let ch = self.line[..col as usize].chars().next_back()?;
    let prev = self.row_start + col - ch.len_utf8() as u32;
    let landed = self.spans.iter().find(|s| s.fold.hides_caret_at(prev)).map_or(prev, |s| s.fold.left_edge());
    Some(landed - self.row_start)
}
```

Why these terminate and progress:

- `step_right` with `cell < width`: either `here > cell`, or the next stop's caret cell, which is
  at least the next column's `End` cell, which is past `cell` (`hit` with `Bias::Left` returned
  `col`, so `cell` is at most `col`'s `Start` cell, or inside a tab that starts at `col`). With no
  next stop, `col` is the line end and `cell` sits on end-of-line hints, so `width > cell`.
- `step_left` with `0 < cell <= width`: either `here < cell` or the previous stop, whose caret
  cell is before `col`'s `End` cell. `cell == 0` returns 0 through `prev_stop(0) == None`.
- `stop_cell` never sees a hidden column (the stops are snapped), so `caret_cell` is a whole cell.

`step_corner` (document.rs:1010-1021 at HEAD) takes the view, not the bare fold map:

```rust
// before
fn step_corner(folds: &FoldMap, c: CellCorner, dir: ColumnDir) -> CellCorner {
    ...
        ColumnDir::Left => CellCorner { row: c.row, cell: c.cell.saturating_sub(1) },
        ColumnDir::Right => CellCorner { row: c.row, cell: c.cell + 1 },
// after
fn step_corner(rows: &Rows, c: CellCorner, dir: ColumnDir) -> CellCorner {
    let folds = rows.folds();
    ...
        ColumnDir::Left => CellCorner { row: c.row, cell: rows.layout(BufferRow(c.row)).step_left(c.cell) },
        ColumnDir::Right => CellCorner { row: c.row, cell: rows.layout(BufferRow(c.row)).step_right(c.cell) },
```

`column_select` already holds a `Rows` after Phase 2; pass it. On a collapsed header row the head
layout's width ends at the header text, so stepping is by character over the head and by cell
across the gap and tail, as today. Rewrite the doc comment of `step_corner` (left/right step by
caret stop within the content, by cell past it) and of `ColumnDir::{Left, Right}` in movement.rs
("one cell" becomes "one character, or one cell past the line end").

`rebuild_column_box`, `caret_corner` and `column_drag` do not change.

## Files changed

| File | Change |
|---|---|
| crates/scrive-core/src/row_layout.rs | `Inlay` (pub), `HintSpan`, `hints_on_row`, `side_rank`; `RowLayout.hints`; `RowLayout::new` takes `Option<&DecorationStore>`, `FoldMap::row_layout` passes `None`; `Rows`: `_inlays` → `inlays`, `layout` builds with the store, `position` honours its edge, `hit` on the memo; `FoldMap::hit_row` deleted; `Edge::passes`; `hint_cells`, `chip_cell`; hint-aware `display_cell`, `edge_cell` (new), `caret_cell`, `chips`, `width`, `is_plain`, `hit` (+ `hit_unhinted`); `inlay_at`, `inlays`, `Rows::inlay_at`; `step_right`, `step_left` and their stop helpers; doc comments; tests |
| crates/scrive-core/src/intel/inlay.rs | `Anchor::{side, index}` (crate-private); `At` |
| crates/scrive-core/src/document.rs | `step_corner` takes `&Rows` and steps by caret stop; tests |
| crates/scrive-core/src/movement.rs | doc comments (`vertical_by`, `ColumnDir`); tests |

MAP_PLAN's Files touched lists these four. The borrowing visitor is Phase 1's (R8), and there is
no crate-root re-export (R22).

## Commit boundaries

Base: the Phase 2 tip. Each boundary builds, passes clippy (both feature sets) and the workspace
tests. Patches: `.claude/map/inlay-hints/patches/phase3-<k>.patch` with `.msg` files, per DISPATCH
override 2.

1. **`test(core): pin the hint-free projections`**. Only the identity sweep with its golden text,
   recorded against the base code. No library change. Body: the hint layout changes every
   projection's arithmetic; this records what an empty inlay store must keep producing.
2. **`feat(core): lay out inlay hints in RowLayout`**. Steps 1-6 and their tests. Body: hints
   push the following text right, so carets, hits, widths and the collapsed header must count
   their cells; edges pick a side of the hints at an offset.
3. **`feat(core): step box selection by caret stop`**. Step 7 and the two box tests. Body: a
   corner stepping by cell stalls for a hint's width on one offset; stepping by caret stop crosses
   a hint, a tab or a chip in one press, and past the line end it still steps by cell.

If commit 2 is too large to review, split it after Step 4 (projection and hit with their tests)
and land `inlay_at` with Step 5's tests as its own `feat(core): find the hint label part under a
cell`. Both halves build alone.

## Tests to add

Shared fixtures. Write a small helper in each test module that installs hints through the public
path, `doc.set_inlays(doc.revision(), placed)`, asserting the outcome is the applied variant.
Build hints with Phase 1's constructors (`Hint::new(kind, parts, key)`, `.padding(..)`,
`Part::new(text, link)`, `Placed::new(offset, hint)`).

- **MAIN**: `"let ab = f(cd);\nlet abcdefghijklmnopqrstuvwxyz0123\n"`, row 1 starts at 16.
  - H1: `Kind::Type` at 6, parts `[": ", "i32" (linked)]`, no padding. Width 5, `Suffix`, key 1.
  - H2: `Kind::Parameter` at 11, label `"n:"`, padding right. Width 3, `Prefix`, key 2.
  - H3: `Kind::Other` at 15 (end of row 0), label `"end"`, padding left only. Its placement
    stays `Auto` (R5: padding never sets it in core) and resolves to `Suffix` at the line end.
    Width 4, key 3.
  - Server order H1, H2, H3. Renders `let ab: i32 = f(n: cd); end`.
- **MIXED**: `"g(x) (a)"` with S = `Kind::Type` `"ss"` at 4 (anchor `)`) and P =
  `Kind::Parameter` `"ppp"` at 5 (anchor `(`), server order S, P. Then delete `4..5` (the
  space): both render at 4 in `"g(x)(a)"`. Different fetch offsets, so install does not normalise
  them.
- **CHIP**: `"x[abcd]y"`, fold opener 1 collapsed. A = `Type` `"aa"` at 1; B = `Parameter`
  `"bb"` at 7; C = `Parameter` `"cc"` at 2; D = `Type` `"dd"` at 6. All width 2.
- **HEADER**: `"fn f(a) {\n    body\n}\n"`, fold opener 8 collapsed. P = `Parameter` `"x:"`
  padding right at 5 (width 3); a `Type` `": T"` at 18 (end of `body`, hidden row 1); an `Other`
  `"fn f"` padding left at 20 (after `}`, tail row 2).

### row_layout.rs (`mod tests`)

- **`hint_free_projections_match_the_pre_hint_golden`** (commit 1). Fixture:
  `"\tlet x = [1, 2, 3]; é\nfn f() {\n\tbody\n}\ttail\nz\n"`, inline fold on the `[` of row 0,
  block fold on the `{` of row 1, empty inlay store. Through `doc.rows()`:
  - every char-boundary offset `0..=len` × every `Edge` → `rows.position(o, e)`;
  - every visible row, cells `0.0, 0.5, ..= width + 2` → `rows.hit(row, cell, Bias::Left)` and
    `Bias::Right`;
  - every visible row's `layout(row).width()` and `is_plain()`; the header's `head_cells`,
    `tail_cell`, `width`.
  Render each result as one `format!("{:?}", ..)` line into a `String` and compare with a
  `const GOLDEN: &str` raw string. Record `GOLDEN` by running the test at the Phase 3 base and
  pasting its output (print it on mismatch so this is one copy). Also assert that the three
  edges agree at every offset. Later commits never edit `GOLDEN`.
- **`hints_sort_by_column_then_prefix_then_server_index`**. MIXED after the delete: the layout
  orders P before S although S came first from the server; MAIN orders H1, H2, H3.
- **`each_edge_places_the_hints_at_an_offset`**. MAIN: the per-column table in the spot checks,
  for all three edges and all columns `0..=15`; `End <= Caret <= Start` at every column; each
  edge is monotone.
- **`a_shared_offset_puts_the_caret_between_the_prefix_and_suffix_groups`**. MIXED: `End` 4,
  `Caret` 7, `Start` 9. Then type `X` at offset 4: `X` renders at
  `display_cell(4, Start) == 7`, the old caret cell, with P left of it and S right of it (P still
  at col 4, S now at col 5). A second case: two `Type` hints at one fetch offset (widths 1 and 2)
  render in server order and `Caret == End` there.
- **`an_empty_range_uses_the_caret_edge_for_both_ends`**. MIXED at offset 4: `Caret..Caret` is
  `7..7`; `Start..End` would be `9..4`, inverted.
- **`a_hit_on_a_label_or_its_padding_lands_on_the_hint_offset`**. MAIN: the hit table in the
  spot checks, including the padding cells 18 and 23 and fractional cells.
- **`hit_round_trips_every_edge`**. MAIN, MIXED and CHIP: for every landable column and edge,
  `hit(display_cell(col, e) as f32, Bias::Left) == col`.
- **`inlay_at_names_the_part_and_reports_padding`**. MAIN: the `inlay_at` table in the spot
  checks, including part 1 of H1 (`link: Jumps`), the padding cells (`At::Padding`) and a
  negative cell.
- **`inlays_yield_each_hint_with_its_first_cell`**. MAIN: `layout(BufferRow(0)).inlays()` yields
  H1, H2, H3 with `(key, offset, cell, width)` = `(1, 6, 6, 5)`, `(2, 11, 16, 3)`,
  `(3, 15, 23, 4)`; H2's `padding` is right only, H3's left only; `hint.parts()` are the
  installed parts. CHIP: only A (cell 1) and B (cell 8). Row 1 of MAIN yields nothing.
- **`width_includes_end_of_line_hints`**. MAIN: row 0 `width() == 27`; `is_plain()` false on
  row 0, true on row 1.
- **`tabs_after_a_hint_keep_their_buffer_space_width`**. `"a\tb"`, `Type` `"tt"` at 1: the tab
  spans cells `3..6` and `b` is at cell 6, not 8.
- **`hints_inside_a_collapsed_inline_fold_are_hidden_and_edges_shift`**. CHIP: C and D are not
  laid out (no cell is on them; `inlay_at` finds only A and B); the CHIP tables in the spot
  checks (cells, chip cell 4, centre 5.5, `caret_cell(3) == ChipCenter(5.5)`, hits). Then unfold:
  all four are laid out.
- **`hints_on_block_folded_rows_are_not_shown`**. HEADER: `rows.layout(BufferRow(1))` and
  `(2)` are plain; the tail `}` at offset 19 projects to `(DisplayRow(0), Cell(16))` for every
  edge; `rows.inlay_at(BufferRow(0), 17.0)` is `None`.
- **`a_header_hint_shifts_the_gap_and_the_tail`**. HEADER: `head_cells` 12 (9 without hints),
  `tail_cell` 16, `gap_center` 14.0, `width` 17; `rows.hit(row 0, 12.6)` is the header line end
  (9), `rows.hit(row 0, 16.0)` is 19, `rows.hit(row 0, 6.0)` is 5.
- **`a_hint_between_two_word_characters_is_laid_out`** (R11: no render guard). Phase 1's
  mixed-offset fixture `"… = ||f; }"` with the `" -> fn()"` type hint and the
  `"<fn-item-to-fn-pointer>"` other hint at `f`; type `X` at that offset. Both hints are laid
  out at the column after `X` (between `X` and `f`), in server order, and `display_cell` of `f`'s
  column with `Start` is past both.
- **`rows_build_each_hinted_row_once`**. MAIN: read `DECORATION_VISITS`, call
  `rows.layout(BufferRow(0))`; record the delta `v` (`v > 0`). Then call `layout` again,
  `position(6, Start)`, `position(15, Caret)`, `hit(BufferRow(0), 8.0, Bias::Left)`,
  `inlay_at(BufferRow(0), 7.0)` and `header(BufferRow(0))`: the delta is still `v`.

### movement.rs (`mod tests`, using `crate::document::Document` as `inline_hidden_binary_search_picks_the_right_fold` does)

- **`vertical_motion_keeps_the_visual_column_across_hints`**. MAIN, plain `move_carets`:
  - caret at `(1, 12)` (offset 28), Up → offset 7, whose caret cell is 12; Down → 28;
  - caret at `(1, 8)` (24), Up → offset 6 (cell 8 is on H1) with the caret drawn at cell 6;
    Down → 24, the goal survived;
  - caret at `(0, 7)` (cell 12), Down → `(1, 12)`, not `(1, 7)`;
  - caret at `(1, 18)` (34), Up → 11 (cell 18 is on H2's padding); Down → 34.
- **`add_caret_vertical_lands_on_the_visual_column_across_hints`**. MAIN: caret at 28,
  `add_caret_vertical(false)` adds a caret at 7.

### document.rs (`mod tests`)

- **`render_offset_agrees_across_layout_inlay_at_and_remove_inlay`**. `"let ab\nf(cd)\n"`,
  `Type` at 6 (end of `ab`) and `Parameter` at the `c` of `cd`. Insert `"\n    "` at 6 (Enter at
  end of line), then `"\n  "` just before `cd`. For every hint in `inlays_in(0..len)`: its
  `Shown` offset `o` is on the row the layout puts it on; `rows.inlay_at(row, cell)` at its first
  label cell (`display_cell(col, End) + left padding`) returns its key and offset `o`; and
  `remove_inlay(key, o)` removes exactly that hint. Also assert the `Type` hint still renders on
  row 0 (the row clamp).
- **`hints_leave_display_position_probes_unchanged`**. Factor the scenario of
  `typing_at_many_carets_over_folds_stays_linear` into a helper that returns the probe count for
  the typed commit, with a flag for installing one `Type` hint per block (after `pid` in
  `(pid: u8)`, through `set_inlays` after the folds and before typing). Assert both counts are
  equal and `<= 4 * carets`. Leave the original test as is.
- **`column_select_crosses_a_hint_in_one_step`** (commit 3). MAIN, caret at `(1, 5)` (21),
  `column_select(Up)`, `Right`, `Right`: the active corner is at cell 12 after two presses, so
  row 0 selects `5..7` and row 1 `21..28`. One more `Left` returns row 0 to `5..6`.
- **`column_select_keeps_stepping_by_cell_past_the_line_end`** (commit 3). MAIN, caret at
  `(1, 23)` (39), `column_select(Up)`: active cell 23 on row 0. `Right` → 27 (past H3, row 1
  `39..43`), `Right` → 28 (`39..44`), `Left` → 27, `Left` → 23.

Existing tests stay unchanged; in particular every `column_select_*`, `column_box_*` and
`vertical_*` test must pass as they are.

## Verification

```
cargo test -p scrive-core
cargo test -p scrive-core -- row_layout:: hint inlay column_select vertical render_offset probes
cargo test --workspace
cargo test --workspace --all-features
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --all-features
cargo build --workspace --all-targets --all-features --target wasm32-unknown-unknown
for c in scrive-core scrive-lsp; do out=$(cargo tree -p $c -e normal --prefix none) || exit 1; if grep -qE '^(iced|winit|wgpu)' <<<"$out"; then echo "$c LEAK"; exit 1; fi; done
git diff --stat <BASE> -- crates/scrive-iced crates/scrive-lsp   # expect no output
```

Never run `cargo fmt`. `rustfmt` applies only to files this phase creates, and it creates none.

## Spot-check tables

### MAIN row 0: `let ab = f(cd);` with H1 (col 6, `Suffix`, 5), H2 (col 11, `Prefix`, 3), H3 (col 15, `Suffix`, 4)

Rendered cells: `let ab` 0-5, H1 6-10 (`: ` 6-7, `i32` 8-10), ` ` 11, `=` 12, ` ` 13, `f` 14,
`(` 15, H2 16-18 (`n:` 16-17, padding 18), `c` 19, `d` 20, `)` 21, `;` 22, H3 23-26 (padding 23,
`end` 24-26). Width 27.

| col | End | Caret | Start |
|---|---|---|---|
| 0..=5 | col | col | col |
| 6 | 6 | 6 | 11 |
| 7 | 12 | 12 | 12 |
| 8 | 13 | 13 | 13 |
| 9 | 14 | 14 | 14 |
| 10 | 15 | 15 | 15 |
| 11 | 16 | 19 | 19 |
| 12 | 20 | 20 | 20 |
| 13 | 21 | 21 | 21 |
| 14 | 22 | 22 | 22 |
| 15 | 23 | 23 | 27 |

`caret_cell(6) == Cell(6)`, `caret_cell(11) == Cell(19)`, `caret_cell(15) == Cell(23)`.

`hit(cell, Bias::Left)`:

| cell | rounded | col | why |
|---|---|---|---|
| 5.0 | 5 | 5 | text |
| 6.0 | 6 | 6 | H1's left boundary (`End` of 6) |
| 8.4 | 8 | 6 | on H1's label |
| 10.6 | 11 | 6 | H1's right boundary (`Start` of 6) |
| 11.6 | 12 | 7 | `12 - 5` |
| 15.0 | 15 | 10 | `15 - 5` |
| 16.0 | 16 | 11 | H2's left boundary |
| 18.0 | 18 | 11 | H2's padding |
| 18.5 | 19 | 11 | H2's right boundary |
| 20.0 | 20 | 12 | `20 - 8` |
| 23.0 | 23 | 15 | H3's padding |
| 26.4 | 26 | 15 | on H3's label |
| 40.0 | 40 | 15 | past the end clamps |

`inlay_at(cell)` (floored):

| cell | result |
|---|---|
| -1.0 | `None` |
| 5.9 | `None` (`b`) |
| 6.0, 7.9 | `Label` key 1, part 0, offset 6, `Link::None`, `Unavailable`, cells `6..8` |
| 8.0, 10.5 | `Label` key 1, part 1, offset 6, `Link::Jumps`, `Unavailable`, cells `8..11` |
| 11.0 | `None` (the space after H1) |
| 16.2, 17.0 | `Label` key 2, part 0, offset 11, `Link::None`, `Unavailable`, cells `16..18` |
| 18.0 | `Padding` key 2, offset 11 |
| 23.5 | `Padding` key 3, offset 15 |
| 24.0, 26.9 | `Label` key 3, part 0, offset 15, `Link::None`, `Unavailable`, cells `24..27` |
| 27.0 | `None` |

Box steps on row 0 (`step_right` / `step_left`):

| from | Right | Left | how |
|---|---|---|---|
| 5 | 6 | 4 | plain |
| 6 | 12 | 5 | Right: `here` 6, next stop col 7 at 12 |
| 9 | 12 | 6 | on H1: hit 6; Left: `here` 6 < 9 |
| 12 | 13 | 6 | Left: `here` 12, previous stop col 6 at its caret cell 6 |
| 15 | 19 | 14 | Right: next stop col 11, caret after H2 |
| 16 | 19 | 15 | on H2: Right `here` 19 > 16; Left: `here` 19, previous stop col 10 at 15 |
| 22 | 23 | 21 | |
| 23 | 27 | 22 | Right: no next stop, jump to `width` |
| 27 | 28 | 23 | Right: past the end, by cell; Left: `here` 23 < 27 |
| 28 | 29 | 27 | by cell |

### MIXED after the delete: `g(x)(a)`, P (`Prefix`, 3) and S (`Suffix`, 2) at col 4

Rendered: `g(x)` 0-3, P 4-6, S 7-8, `(` 9, `a` 10, `)` 11. Width 12.

| col | End | Caret | Start |
|---|---|---|---|
| 3 | 3 | 3 | 3 |
| 4 | 4 | 7 | 9 |
| 5 | 10 | 10 | 10 |
| 7 (len) | 12 | 12 | 12 |

Hits: cells 4 through 9 → 4; 10 → 5. After typing `X` at 4: `g(x)` P `X` S `(a)`, `X` at
cell 7.

### CHIP: `x[abcd]y`, `[` at 1 collapsed (shift 1), A at col 1 (`Suffix`), B at col 7 (`Prefix`), C and D hidden

Rendered: `x` 0, A 1-2, `[` 3, chip 4-6, `]` 7, B 8-9, `y` 10. Width 11.

| col | End | Caret | Start | note |
|---|---|---|---|---|
| 0 | 0 | 0 | 0 | |
| 1 | 1 | 1 | 3 | A before `[` |
| 2 | 4 | 4 | 4 | chip's left edge; C hidden |
| 3 | `caret_cell` → `ChipCenter(5.5)` | | | hidden column |
| 6 | 7 | 7 | 7 | `]`; D hidden |
| 7 | 8 | 10 | 10 | B |
| 8 | 11 | 11 | 11 | |

`chips()` → `cell 4, center 5.5, open_col 1, close_col 6`. Hits: 1-3 → 1; 4, 5, 6 → 2 (on the
chip); 7 → 6; 8-10 → 7; 11 → 8.

### HEADER: `fn f(a) {` collapsed, P at col 5 (`Prefix`, 3)

| quantity | no hints | with P |
|---|---|---|
| `head_cells` | 9 | 12 |
| `gap_center` | 11.0 | 14.0 |
| `tail_cell` | 13 | 16 |
| `width` | 14 | 17 |
| `position(19 = '}', any edge)` | `(row 0, Cell(13))` | `(row 0, Cell(16))` |

`rows.hit(row 0, ..)`: 6.0 → 5; 12.6 → 9 (gap → header line end); 16.0 → 19 (tail);
`inlay_at(row 0, 5.0)` → `Label` P part 0, offset 5, cells `5..7`; `inlay_at(row 0, 7.0)` →
`Padding` P, offset 5.

### Vertical motion on MAIN

| caret | motion | lands | goal |
|---|---|---|---|
| `(1, 12)` = 28 | Up | 7 (drawn at cell 12) | 12 |
| then | Down | 28 | 12 |
| `(1, 8)` = 24 | Up | 6 (drawn at cell 6) | 8 |
| then | Down | 24 | 8 |
| `(0, 7)` = 7 | Down | `(1, 12)` = 28 | 12 |
| `(1, 18)` = 34 | Up | 11 (drawn at cell 19) | 18 |

## What NOT to change

- No scrive-iced or scrive-lsp file. The widget's sites picked their edges in Phase 2; painting,
  pills and `max_line_px` are Phase 4.
- No edge choice at any call site. If a site looks wrong, report it.
- Do not touch the inlay store's mover, `remap_ranges`, `empty_policy`, `set_inlays`, the side
  normalisation or `render_offset` (Phase 1). Read them; don't fix them here.
- Hints never enter `FoldMap`, its cache key, or `FoldMap::renders`. The visibility probes stay
  hint-free.
- Do not make `FoldMap::{row_layout, header_layout}` public again (`hit_row` is deleted, Step 4),
  and do not add a public way to build a `RowLayout` without the inlay store.
- Keep the chip formulas (`shift_at`, `cell_of`, `INLINE_CHIP_CELLS`, the chip loop in
  `hit_unhinted`) and the `HeaderLayout` formulas as they are.
- `Motion::Left`/`Right`, word motion, find and Ctrl+D stay byte-based; they cross a hint in one
  press with no change.
- No new dependency, no `#[allow(dead_code)]`, never `cargo fmt`.

## Pitfalls

- **Order of composition.** Add hint widths after `cell_of`, never before: `shift_at` compares
  against raw closing-bracket cells, and a hinted cell would cross the chip threshold early.
- **`hit` subtracts before the chip pass.** Running `collapse` or the chip loop on a hinted cell
  lands one hint-width too far right. The golden sweep will not catch this (it has no hints);
  `hit_round_trips_every_edge` on CHIP will.
- **Rounding.** `hit` rounds and `inlay_at` floors; don't share the conversion. `f32::round`
  rounds half away from zero, so 10.5 is 11, which is still H1's group in MAIN.
- **Sort key.** Side rank comes before the server index. Sorting by `(col, index)` breaks MIXED.
- **The fold interval is half-open on the other side.** Hidden is `open < o <= close`, not
  `hides_caret_at` (`open + 1 < o < close`) and not `hides_glyph_at`.
- **`is_folded` excludes the header.** The header row keeps its hints; the tail row (`last`)
  counts as folded and loses them, which is what the plan wants.
- **Lifetimes.** `HintSpan<'a>` borrows from the store, so `RowLayout<'a>` ties the buffer and the
  store to one `'a`. `Rows<'a>` must hand both in with that lifetime. If Phase 2 gave them
  separate lifetimes, unify them on `Rows` rather than cloning anchors.
- **The memo.** `RowLayout::new` must not call back into `Rows`; the memo's `RefCell` is borrowed
  mutably while inserting. Build first, then insert.
- **`let ... else` on `DecorationKind`.** `DecorationKind` is `#[non_exhaustive]`, which binds
  only other crates. Inside the crate the `else` branch is the "not a hint" case, guarded by
  `debug_assert!(false, ..)`. Don't write a `match` with a `_` arm.
- **Clippy.** `as u32` from `usize` on line lengths already appears in this file; follow it.
  `chunk_by` needs Rust 1.77; check `rust-version` in `Cargo.toml` before using it.
- **Doc links.** `Edge`, `Rows` and `Inlay` are in this module; link them with
  `[`Edge`]`-style paths. Don't link `CodeEditor` or anything in scrive-iced from core docs.
- **Perf gates.** The extra query per row is `O(log n + hits)` and charged per hit. If a draw or
  movement perf cell moves, the memo is probably being bypassed: some path builds a
  `RowLayout` outside `Rows`.
- **Golden first.** Record `GOLDEN` before any library change, on the unmodified base. A golden
  recorded after Step 3 proves nothing.

## Resolved questions

1. **`render_offset`'s signature:** `render_offset(&self, range, row_start, row_end, line)` (R4),
   and there is no mid-word render guard anywhere (R11).
2. **The borrowing visitor and the file list:** Phase 1 adds `visit_in` and the `filter_visit`
   lifetime (R8); this phase only uses it.
3. **`inlay_at`'s return type:** `inlay::At { Label { key, part, offset, link, insert, cells },
   Padding { key, offset } }` (R3), defined in intel/inlay.rs (Step 1). `Padding` is inert.
4. **Box stepping without hints:** stepping by character within content applies everywhere,
   hints or not; by cell past the line end (R12, the user's call).
5. **The painting accessor:** this phase adds `RowLayout::inlays()` yielding
   `&row_layout::Inlay { key, offset, cell, width, padding, hint }` (R2).
6. **Phase 2's naming:** Phase 2 adds no `edge_cell` and no `Rows::inlay_at`; this phase adds
   both, renames the `_inlays` field and the `_edge` parameters (R22), and makes `Rows::position`
   and `Rows::hit` hint-aware.

Still open:

- **The hint-free builder (audit).** Making `Rows::hit` and `Rows::position` hint-aware is
  required (D5, D7), and `FoldMap::renders` must stay hint-free (it runs inside `rebase_views`).
  This doc's mechanism, `RowLayout::new(.., Option<&DecorationStore>, ..)` with
  `FoldMap::row_layout` passing `None`, and deleting the then test-only `FoldMap::hit_row`, is an
  audit default, not a plan decision. Confirm or name another shape.
