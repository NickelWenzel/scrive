# Phase 4 — scrive-iced: painting inlay hints

Read `.claude/map/lsp-bridge/DISPATCH.md` first, with the substitutions in MAP_PLAN.md Appendix A:
- the map directory is `.claude/map/inlay-hints/`;
- patches go to `.claude/map/inlay-hints/patches/`;
- the `--all-features` clippy run applies to this phase too.

This doc is self-contained for Phase 4. It restates the parts of MAP_PLAN.md (Draft 7) that the phase
implements. Line numbers are from HEAD `8e72665` (branch `lsp_bridge`), re-verified for this doc.
Phases 1–3 have moved code since: Phase 2 replaced every `fold_map` geometry call in editor.rs with
a `Rows` view and an `Edge`. So **locate each site by the function or arm named**, not by the
number. The "before" snippets show HEAD; the shape after Phase 2 is described next to each one.

## 1. Prerequisites

Phases 1–3 are committed and green. Check each of these before writing code. If one fails, stop
and report it.

- **Phase 1 (hint model).** `crates/scrive-core/src/intel/inlay.rs` exists with `Hint`, `Part`,
  `Padding`, `Kind`, `Key`, `Placed`, `Outcome`.
  - `Document::set_inlays(revision, Vec<inlay::Placed>) -> inlay::Outcome` exists
    (`grep -n "pub fn set_inlays" crates/scrive-core/src/document.rs`).
  - The constructors (MAP_PHASE_1, R1): `Hint::new(kind, parts, key) -> Result<Hint, Error>`,
    `.padding(Padding)`, `Part::new(text, Link)`, `Padding { left: bool, right: bool }` (public
    fields, `Default`), `Key::new(u64)`, `Placed::new(offset, hint)`; `Outcome::Applied { count }`.
    Getters: `Hint::parts() -> &[Part]`, `Hint::padded() -> Padding`, `Part::text() -> &str`.
    Only the test helper `hinted` (Step 4) calls the constructors.
- **Phase 2 (`Rows` and `Edge`).**
  - `row_layout::Edge { Start, End, Caret }` and `row_layout::Rows` exist
    (`grep -n "pub enum Edge\|pub struct Rows" crates/scrive-core/src/row_layout.rs`).
  - `Document::rows()` exists.
  - `Rows` has `layout(row)`, `header(row)`, `position(offset, Edge)`, `hit(row, cell, Bias)`
    and `folds()` (Phase 3 adds `inlay_at(row, cell)`). Layouts come back as `Rc<RowLayout>`.
  - editor.rs's draw helpers take one `&Rows` passed down from `draw`. These include
    `offset_screen_x`, `popup_anchor`, `draw_selection`, `draw_wash_row`, `hit_test`,
    `collapsed_chip_at`, `armed_boxes` and `max_line_px`.
  - `offset_screen_x` takes an `Edge`; `max_line_px(&self, rows, advance, line_h, bounds,
    scroll_rows)` and `max_scroll_x(&self, rows, bounds, advance, line_h, scroll_rows)` take the
    view first.
  - `grep -n "display_position\|row_layout(\|header_layout(\|hit_row(" crates/scrive-iced/src/editor.rs`
    finds no geometry call (the tests may still name `Rows` methods).
- **Phase 3 (hint-aware layout).**
  - `RowLayout::display_cell(col, Edge)` adds hint widths.
  - `RowLayout::is_plain()` is false on a row with laid-out hints.
  - `RowLayout::width()` includes hints at the line end.
  - `RowLayout::hit` maps any cell on a hint (label or padding) to the hint's offset.
  - `HeaderLayout` derives from the hinted head layout.
- **The label accessor (Phase 3, R2).** `RowLayout::inlays() -> impl Iterator<Item =
  &row_layout::Inlay<'_>>` in render order, with public fields `key`, `offset` (the buffer offset
  it renders at), `cell` (its first display cell, left padding included), `width`, `padding`
  (`inlay::Padding`) and `hint: &inlay::Hint` (for `parts()`). A hint's byte column on the row is
  `inlay.offset - layout.row_start()`. **If it is missing, stop and report.** Don't add it to
  scrive-core in this phase: Phase 4 touches only editor.rs and geo.rs.
- Read in full before changing anything:
  - `~/.claude/guides/RUST_STYLE.md` and `~/.claude/guides/OPAQUE.md`;
  - the `/iced` and `/commit-and-comment` skills;
  - `crates/scrive-iced/src/editor.rs`: `draw`, `draw_selection`, `draw_wash_row`,
    `draw_row_inline`, `draw_spans`, `draw_fold_preview`, `expand_tabs`, `range_covered_by`, and
    the `mod tests` helpers `test_geo`, `headless_renderer`, `pump`;
  - `crates/scrive-iced/src/geo.rs`;
  - `crates/scrive-core/src/row_layout.rs` as Phase 3 left it.
- The baseline is green:
  - `cargo test --workspace --all-features`;
  - `cargo clippy --workspace --all-targets [--all-features] -- -D warnings`.

## 2. Goal and exit criteria

**Goal.** Hints installed through `Document::set_inlays` appear in the editor:
- each label is drawn in the code font on the cell grid, in a dimmed text colour on a rounded pill
  over its label cells;
- padding cells stay editor background;
- text after a hint is pushed right;
- every overlay (caret, washes, squiggles, bracket boxes, horizontal scroll range) sits at the
  edge the plan assigns it.

First, the tab-expansion bug in the run painter is fixed in its own commit, because the hint split
rewrites the same code.

**Exit criteria.** All of these pass, plus the full suite:

1. Commit 1 (tab fix): `a_run_expands_its_tabs_from_its_raw_cell`.
2. Commit 3 (painting):
   - `split_at_columns_cuts_a_run_at_each_inner_column`
   - `a_hint_pill_is_dropped_only_strictly_inside_a_selection`
3. Commit 4 (projection sites), all on the fixture from Step 4.1:
   - `caret_x_sits_on_the_edge_its_selection_names`
   - `washes_exclude_boundary_hints_and_cover_interior_ones`
   - `an_interior_row_wash_runs_past_end_of_line_hints`
   - `squiggles_span_start_to_end_and_an_empty_one_sits_at_the_caret_edge`
   - `the_bracket_box_sits_after_a_hint_at_the_bracket`
   - `max_scroll_reaches_an_end_of_line_hint`
   - `a_click_on_a_hint_places_the_caret_at_its_offset`
4. With no hints, every existing editor.rs test passes unchanged.
5. clippy (both feature sets), the doc build and the wasm build are clean.

## 3. Design decisions implemented

Restated from MAP_PLAN.md. They are binding.

**Model background (D1, D3, D4).**
- A hint sits at one buffer offset `p` and has a side:
  - `Suffix` annotates the token ending at `p` (type hints, `let x: i32`);
  - `Prefix` annotates the token starting at `p` (parameter hints, `foo(n: 1)`).
- Width in cells = `padding.left + Σ part chars + padding.right`, one cell per Unicode scalar.
  Labels are sanitised (control chars → `' '`), so a label never contains a tab.
- At install, every offset holds hints of one side only.
- After edits, a mixed group renders as the Prefix hints, then the Suffix hints, each in server
  order.
- Phase 3's layout owns all of this. Phase 4 only reads cells.

**D6 — projections name an edge.** For offset `p`:
- `Start` is after every hint at `p`. Used for glyphs, bracket colours and boxes, the start of any
  range, and popup anchors at a word start.
- `End` is before every hint at `p`. Used for the end of any range.
- `Caret` is after the `Prefix` hints at `p` and before the `Suffix` hints: where the next typed
  character lands.
- An **empty** selection's caret uses `Caret`. A non-empty selection's caret renders at its wash
  edge: `End` when the head is the end, `Start` when it is reversed.
- An **empty range** (a zero-width diagnostic, an empty find match) uses `Caret` for both ends,
  since `Start..End` would be inverted.
- Consequence: hints strictly inside a range are washed and underlined; boundary hints are not. On
  a multi-row range, an interior row ends at `Start` of the line end, after end-of-line hints.

**D7 (what Phase 4 relies on).**
- `display_cell(col, edge)` adds the widths of hints before `col`, plus the hints at `col` that
  the edge selects.
- `hit(cell, bias)` maps a cell on a hint's label or padding to the hint's offset.
- `width()` includes end-of-line hints.
- `is_plain()` is false on a row with hints.
- Tabs after a hint keep their buffer-space width (the fixed-shift model), as they do after a chip.
- Hints inside a collapsed inline fold's hidden interior, and hints on block-folded rows, are not
  laid out.

**D8 — painting.**
- Hints are drawn in the code font at the code size, on the cell grid.
- Each label part is one `draw_line` in a dimmed text colour, on a pill spanning the label cells.
  Padding cells stay editor background (the LSP spec's rule).
- Rows with hints take the inline path. Spans are split at hint columns.
- The tab bug is fixed first, in its own commit. The fix takes each run's tab phase from
  `display_map::expand(line, start_col)`, the **raw** cell before hints and chips, not from the
  display cell. So tabs after a hint keep their buffer-space width.
- On selected rows the pill is suppressed the way chips are (editor.rs:3127-3141), so it doesn't
  island inside the wash. The test is **strict interior** (`a < p < b`), not the chips' span test
  (editor.rs:3134), which would suppress the pills of unwashed boundary hints.

**D9 — every projection site picks an edge.** The plan's table:

| Site | Edge |
|---|---|
| empty-selection carets (`offset_xy`), autoscroll, signature anchor, box-drag corner, vertical motion | `Caret` |
| non-empty selection carets | the wash edge at the head (`End` forward, `Start` reversed) |
| selection / occurrence / find / scope washes, squiggles | `Start` … `End`; interior rows end at `Start` of the line end; empty ranges use `Caret` for both |
| glyph runs, bracket colours, matching-bracket box, collapsible box (`xo`/`xc`), chip pill rects, completion and hover anchors | `Start` |
| row width, `max_line_px` | `width()` |

Phase 2 picked these edges when it moved the sites onto `Rows`. With no hints every edge gives the
same cell, so a wrong pick was invisible until now. **Phase 4 verifies every site against this
table (Step 4.2) and fixes any that differ.**

**Decisions this doc makes where the plan is open.** Each follows an existing precedent in
editor.rs.
- **Decision: the label colour is `Color { a: INLAY_TEXT_A, ..text_color }` with
  `INLAY_TEXT_A = 0.6`, on and off the wash alike** (R14: a washed hint keeps the dimmed colour).
  - This is the fold preview's "… N more lines" colour (editor.rs:3326).
  - The chip's `dim` (`palette.background.strong.color`) is the selection colour itself
    (editor.rs:1393, 1397), so dim labels would vanish into a wash. That is why chips switch to
    `text_color` there (editor.rs:3132-3135).
  - One translucent colour stays readable on both surfaces, and it never reads as buffer text.
- **Decision: the pill is `POPUP_SELECT` with `CHIP_PILL_RADIUS`, inset vertically like
  `Geo::chip_pill`** (`row_top + 2`, height `line_h − 4`). Horizontally it spans exactly the label
  cells. That is the chip's pill colour and rounding, so the two placeholders read alike.
- **Decision: only selections suppress the pill**, as with chips, and only strictly inside them.
  Find, occurrence and scope washes keep the pill (R14).
- **Decision: a continuation row of a multi-row squiggle starts at `End` of its column 0** (R15).
  - The table names no edge for a continuation row's start.
  - D6's consequence rule ("hints strictly inside a range are underlined") requires `End`. A hint
    at column 0 of a continuation row is strictly inside the range.
  - Washes already start continuation rows at `cell_x(0.0)`, which equals `End` there.
- **Decision: the extents are measured by pure helpers** (`caret_edge`, `wash_extent`,
  `squiggle_extent`, `bracket_box`, `inlay_pill`, `split_at_columns`) that the painter calls. The
  headless tests call the same helpers. `collapsed_chip_rect` and `chip_pill_rect` are the
  precedent.

## 4. Step-by-step changes

Four commits, in this order. Each must build and pass clippy and tests on its own (DISPATCH override
2).

| # | Subject | Scope |
|---|---|---|
| 1 | `fix(iced): expand a run's tabs from its own raw cell` | `expand_tabs` gains a phase; 4 call sites; regression test |
| 2 | `refactor(iced): measure caret, wash, squiggle and bracket-box extents in pure helpers` | no behaviour change; the suite is the oracle |
| 3 | `feat(iced): paint inlay hints` | span split, labels, pills, strict-interior suppression, `Geo::inlay_pill`; two unit tests |
| 4 | `test(iced): hint-aware projection sites` | the Step 4.2 site audit (subject becomes `fix(iced): …` if a site needed a fix); the seven headless tests |

### Step 1 — the tab phase (commit 1)

**The bug.** `expand_tabs` (editor.rs:3460-3481) counts tab stops from cell 0 of the run it gets.
`draw_spans` (3346-3351) and `draw_row_inline`'s `seg` (3100-3101) place each run at its true start
cell. A tab inside a run that doesn't start on a tab stop is painted too wide, so the glyphs after
it drift right of the caret.

Example: line `x\ty` with spans `[0,1)` and `[1,3)`. The second run starts at cell 1. Its tab
reaches the stop at 4, so `y` belongs at cell 4. Today the run expands `"\ty"` from 0 to
`"    y"`, which paints `y` at cell 5.

**1a. `expand_tabs`** (editor.rs:3460-3481). Before:

```rust
/// Expand tabs to spaces for display (the caret math uses the display map, so
/// they agree).
fn expand_tabs(line: &str) -> String {
    if !line.contains('\t') {
        return line.to_owned();
    }
    let mut out = String::with_capacity(line.len());
    let mut cell = 0u32;
    for ch in line.chars() {
        if ch == '\t' {
            let w = display_map::tab_width(cell, TAB);
```

After:

```rust
/// Expand the tabs of a run that starts at raw cell `start_cell` (its tab-expanded
/// cell before chips and hints shift it), so each tab reaches the same stop the
/// display map measures.
fn expand_tabs(run: &str, start_cell: u32) -> String {
    if !run.contains('\t') {
        return run.to_owned();
    }
    let mut out = String::with_capacity(run.len());
    let mut cell = start_cell;
    for ch in run.chars() {
        if ch == '\t' {
            let w = display_map::tab_width(cell, TAB);
```

The rest of the body is unchanged: it pushes `w` spaces and advances `cell`.

**1b. The four call sites.** HEAD numbers; Phase 2 may have touched the neighbouring lines.

| Site | Before | After |
|---|---|---|
| plain unhighlighted row (`draw`, 1614) | `expand_tabs(&line)` | `expand_tabs(&line, 0)` |
| `draw_row_inline`'s `seg` (3101) | `expand_tabs(text)` | `expand_tabs(text, display_map::expand(line, start_col, TAB))` |
| `draw_fold_preview`, unhighlighted line (3318) | `expand_tabs(l)` where `l = line.trim_start()` | `expand_tabs(l, line_indent_cells(&line))` |
| `draw_spans` (3351) | `expand_tabs(text)` | `expand_tabs(text, cell)` (the `cell` computed at 3346) |

In `seg`, the phase is the **raw** cell (`display_map::expand`), not
`row_layout.display_cell(..)`. After a chip, the display cell is shifted but the tab stops are not
(D7's fixed-shift model). `line_indent_cells` (editor.rs:3648-3651) is exactly
`display_map::expand(line, ws, TAB)`, the raw cell where the trimmed text begins.

**1c. Regression test** (`mod tests`, next to the other free-function tests):

```rust
/// A run that starts off a tab stop expands its tabs from its own raw cell, so
/// it paints exactly as wide as the display map measures it.
#[test]
fn a_run_expands_its_tabs_from_its_raw_cell() {
    assert_eq!(expand_tabs("\ty", 1), "   y", "a tab at cell 1 reaches the stop at 4");
    let line = "a\tbc\t\td\te";
    let len = line.len() as u32;
    for start in 0..=len {
        for end in start..=len {
            let cell = display_map::expand(line, start, TAB);
            let painted = expand_tabs(&line[start as usize..end as usize], cell).chars().count() as u32;
            assert_eq!(cell + painted, display_map::expand(line, end, TAB), "run {start}..{end}");
        }
    }
}
```

Against the old code the sweep fails on run `1..3` (`"\tb"`: painted 5 cells from cell 1, where
the display map ends it at 5). The line is ASCII, so every slice is on a char boundary.

### Step 2 — pure extent helpers (commit 2, no behaviour change)

Extract the geometry each overlay computes inline today, so commit 4 can test it headlessly. Each
helper keeps **exactly** the edge Phase 2 chose at that site. Correcting edges is commit 4's job.

**2a. `caret_edge`** (free fn, near `range_covered_by` at editor.rs:1203):

```rust
/// The edge a selection's caret renders on: an empty selection at the caret
/// edge, a non-empty one at its wash edge, so it never floats past a boundary
/// hint the wash excludes.
fn caret_edge(sel: &scrive_core::Selection) -> Edge {
    if sel.is_empty() {
        Edge::Caret
    } else if sel.head() == sel.end() {
        Edge::End
    } else {
        Edge::Start
    }
}
```

The caret loop in `draw` (HEAD 1841-1847) then reads
`offset_xy(sel.head(), caret_edge(sel))`. If Phase 2 inlined this logic in the loop, replace it
with the call. If Phase 2 used `Caret` for every caret, keep that behaviour in this commit: write
the helper to return what the loop does today, and fix it in commit 4.

**2b. `wash_extent`.** Factor the x math out of `draw_wash_row` (HEAD 2990-3002). HEAD for
reference:

```rust
let x0 = if row == a.row { self.offset_screen_x(fold_map, geo, start) } else { geo.cell_x(0.0) };
let x1 = if row == b.row {
    self.offset_screen_x(fold_map, geo, end)
} else if let Some(hl) = fold_map.header_layout(buffer, BufferRow(row), TAB) {
    geo.cell_x(hl.tail_cell() as f32)
} else {
    let line_end = buffer.point_to_offset(scrive_core::Point::new(row, buffer.line_len(row)));
    self.offset_screen_x(fold_map, geo, line_end) + advance * 0.5
};
fill(renderer, Rectangle { x: x0, y, width: (x1 - x0).max(1.0), height: line_h }, color);
```

After, on a visible, unfolded `row`, with Phase 2's edges carried over unchanged:

```rust
/// The `(x0, x1)` screen span a range `start..end` (points `a..b`) washes on
/// visible, unfolded buffer `row`: `Start` at its start, `End` at its end,
/// `Caret` for both ends of an empty range; an interior row runs to `Start` of
/// its line end plus half a cell, or across a collapsed header's placeholder.
#[allow(clippy::too_many_arguments)] // the wash's row, both points and both offsets are all read
fn wash_extent(&self, rows: &Rows<'_>, geo: &Geo, row: u32, a: BufPoint, b: BufPoint, start: u32, end: u32) -> (f32, f32) {
    // body: the HEAD lines above, with `rows` for `fold_map` and Phase 2's edges
}
```

`draw_wash_row` keeps the folded-row hand-off, the budget bump, the y cull and the `fill`, and calls
`wash_extent` for `(x0, x1)`.

**2c. `squiggle_extent`.** Factor the per-row body of the squiggle loop (HEAD 1739-1758):

```rust
/// The `(x0, x1)` screen span diagnostic `start..end` (rows `sp_row..=ep_row`)
/// underlines on visible buffer `row`, at least one cell wide; `None` on a
/// boundary row the range doesn't actually cover.
#[allow(clippy::too_many_arguments)] // the row plus the span's offsets and rows
fn squiggle_extent(&self, rows: &Rows<'_>, geo: &Geo, row: u32, start: u32, end: u32, sp_row: u32, ep_row: u32) -> Option<(f32, f32)> {
    // HEAD 1743-1756: row_start / row_end, the zero-width-row skip, x0, x1.max(x0 + advance)
}
```

The loop becomes:

```rust
for &(start, end, sp_row, ep_row, _sev, color) in &diags {
    if row < sp_row || row > ep_row {
        continue;
    }
    let Some((x0, x1)) = self.squiggle_extent(&rows, &geo, row, start, end, sp_row, ep_row) else { continue };
    let baseline = row_y + line_h - SQUIGGLE_AMPLITUDE - 0.5;
    squiggle_spans(x0, x1, baseline, |rect| fill(renderer, rect, color));
}
```

**2d. `bracket_box`.** Factor the matching-bracket box (HEAD 1703-1711). Today it goes through
the `offset_xy` closure, which also culls to the visible window. Keep that cull in `draw`:

```rust
/// The matching-bracket outline around the bracket at `offset`, in screen
/// space, or `None` if the offset doesn't render.
fn bracket_box(&self, rows: &Rows<'_>, geo: &Geo, offset: u32) -> Option<Rectangle> {
    let p = rows.position(offset, Edge::Start)?; // Phase 2's edge here
    Some(Rectangle { x: geo.cell_x(p.x.cells()), y: geo.row_y(p.row), width: geo.advance(), height: geo.line_h() })
}
```

In `draw`, after the change:

```rust
for off in [a, b] {
    let Some(rect) = self.bracket_box(&rows, &geo, off) else { continue };
    if !window.contains(&rows.folds().display_row_at(geo.rows_from_top(rect.y + 1.0)).index()) {
        continue;
    }
    fill_border(renderer, rect, match_box, 1.0);
}
```

That cull is one option. If it reads awkwardly, have `bracket_box` return the `DisplayPosition`
alongside the rect and test `window.contains(&p.row.index())` as `offset_xy` does. Either way, the
culling must stay identical.

The full suite passes unchanged. That is this commit's whole verification.

### Step 3 — painting (commit 3)

**3a. Constants** (editor.rs, next to `OCCURRENCE_MATCH` at 199):

```rust
/// Alpha of an inlay hint's label over the text colour: readable on its pill
/// and on a selection wash, and never mistaken for buffer text.
const INLAY_TEXT_A: f32 = 0.6;
```

**3b. `Geo::inlay_pill`** (geo.rs, after `chip_pill`, 171-181):

```rust
/// The rounded pill behind an inlay hint's label, spanning screen x `x0..x1`
/// (the label's cells, padding excluded) on the row at `row_top`, inset
/// vertically like [`Self::chip_pill`] so the two placeholders line up.
pub(crate) fn inlay_pill(&self, x0: f32, x1: f32, row_top: f32) -> Rectangle {
    Rectangle { x: x0, y: row_top + 2.0, width: x1 - x0, height: self.line_h - 4.0 }
}
```

**3c. `split_at_columns`** (free fn near `expand_tabs`):

```rust
/// `range` cut at every column of `cols` (sorted, duplicates allowed) strictly
/// inside it: the runs a highlight span paints as, so the text after a hint
/// starts past the hint.
fn split_at_columns(range: Range<u32>, cols: &[u32]) -> impl Iterator<Item = Range<u32>> + '_ {
    let mut from = range.start;
    cols.iter()
        .copied()
        .filter(move |&c| c > range.start && c < range.end)
        .chain(std::iter::once(range.end))
        .filter_map(move |c| (c > from).then(|| std::mem::replace(&mut from, c)..c))
}
```

If the iterator form trips the borrow checker, return a `Vec<Range<u32>>`. The row is short, and
the text pass already allocates a `String` per run.

**3d. `hint_selected` and `inlay_pill`** (methods, next to `range_selected` at 2929-2935):

```rust
/// Whether offset `p` lies strictly inside some selection (`start < p < end`):
/// exactly when a hint there is washed, so its pill would island in the wash.
fn hint_selected(&self, p: u32) -> bool {
    p > 0 && range_covered_by(self.doc.selections().all(), p - 1, p + 1)
}

/// The pill behind `inlay`'s label on the row at `row_top`, or `None` where a
/// selection wash already backs it.
fn inlay_pill(&self, inlay: &row_layout::Inlay<'_>, geo: &Geo, row_top: f32) -> Option<Rectangle> {
    if self.hint_selected(inlay.offset) {
        return None;
    }
    let first = inlay.cell + u32::from(inlay.padding.left);
    let label = inlay.width - u32::from(inlay.padding.left) - u32::from(inlay.padding.right);
    Some(geo.inlay_pill(geo.cell_x(first as f32), geo.cell_x((first + label) as f32), row_top))
}
```

`range_covered_by(sels, s, e)` (editor.rs:1203-1206) is **inclusive**: some selection has
`start ≤ s` and `end ≥ e`. With `s = p − 1` and `e = p + 1` that is `start < p < end` over
integers. `p = 0` can never be strictly inside. `inlay.width` is padding plus label cells (D1),
so the label span is the width minus the padding flags.

**3e. `draw_row_inline`** (HEAD 3077-3142). This is the inline path every hinted row takes, because
Phase 3 makes `is_plain()` false there. Before (HEAD, with Phase 2's `Edge::Start` in `seg`, and
commit 1's phase):

```rust
let seg = |renderer: &mut iced::Renderer, text: &str, start_col: u32, color: Color| {
    if text.is_empty() || row_layout.glyph_hidden(start_col) {
        return;
    }
    let x = origin.x + row_layout.display_cell(start_col, Edge::Start) as f32 * advance;
    self.draw_line(renderer, expand_tabs(text, display_map::expand(line, start_col, TAB)), Point::new(x, origin.y), color, Alignment::Left, clip);
};
match spans {
    Some(spans) if !spans.is_empty() => {
        for s in spans {
            let fg = s.style.fg;
            let text = &line[s.range.start as usize..s.range.end as usize];
            seg(renderer, text, s.range.start, Color::from_rgb8(fg.r, fg.g, fg.b));
        }
    }
    _ => {
        let mut cursor = 0u32;
        for chip in row_layout.chips() {
            seg(renderer, &line[cursor as usize..=chip.open_col as usize], cursor, text_color);
            cursor = chip.close_col;
        }
        seg(renderer, &line[cursor as usize..], cursor, text_color);
    }
}
```

After: `seg` takes a column range and splits it at hint columns. Both arms pass ranges.

```rust
let hint_cols: Vec<u32> = row_layout.inlays().map(|inlay| inlay.offset - row_layout.row_start()).collect();
let seg = |renderer: &mut iced::Renderer, cols: Range<u32>, color: Color| {
    for run in split_at_columns(cols, &hint_cols) {
        if row_layout.glyph_hidden(run.start) {
            continue;
        }
        let x = geo.cell_x(row_layout.display_cell(run.start, Edge::Start) as f32);
        let text = &line[run.start as usize..run.end as usize];
        let phase = display_map::expand(line, run.start, TAB);
        self.draw_line(renderer, expand_tabs(text, phase), Point::new(x, origin.y), color, Alignment::Left, clip);
    }
};
match spans {
    Some(spans) if !spans.is_empty() => {
        for s in spans {
            let fg = s.style.fg;
            seg(renderer, s.range.clone(), Color::from_rgb8(fg.r, fg.g, fg.b));
        }
    }
    _ => {
        let mut cursor = 0u32;
        for chip in row_layout.chips() {
            seg(renderer, cursor..chip.open_col + 1, text_color);
            cursor = chip.close_col;
        }
        seg(renderer, cursor..line.len() as u32, text_color);
    }
}
```

Notes:
- `origin.x + cell * advance` and `geo.cell_x(cell)` are the same number (`origin` is
  `geo.cell_x(0.0)`, editor.rs:1601). Use whichever the surrounding code uses. Don't mix them in one
  expression.
- `split_at_columns` drops empty runs, which replaces the old `text.is_empty()` check.
- A run starting exactly at a hint column is placed at `Start`, after every hint there. A run
  ending there stops before them. So the label cells are left free for 3f.
- Hint columns are char boundaries: they come from LSP positions converted through
  `Encoding::offset`, and the store anchors on words or whole chars. So `&line[run]` can't panic.
  Don't "fix" that with `get(..)`. A panic there would mean a core bug.

**3f. Labels and pills.** Append to `draw_row_inline`, after the chip loop:

```rust
// Hint labels sit on the cells the layout reserved for them; padding stays
// editor background, per the LSP spec's `paddingLeft` / `paddingRight`.
let label_color = Color { a: INLAY_TEXT_A, ..text_color };
for inlay in row_layout.inlays() {
    if let Some(pill) = self.inlay_pill(inlay, geo, origin.y) {
        fill_rounded(renderer, pill, POPUP_SELECT, CHIP_PILL_RADIUS);
    }
    let mut cell = inlay.cell + u32::from(inlay.padding.left);
    for part in inlay.hint.parts() {
        let text = part.text();
        self.draw_line(renderer, text.to_owned(), Point::new(geo.cell_x(cell as f32), origin.y), label_color, Alignment::Left, clip);
        cell += text.chars().count() as u32;
    }
}
```

- `draw_row_inline` is called for every non-plain visible row, collapsed fold headers included
  (editor.rs:1605-1607), so hints on a header row paint here too. The header placeholder that
  follows (1619-1662) already sits past them, because `HeaderLayout` derives from the hinted head.
- Labels need no tab expansion: Phase 1 turns control characters into spaces.
- Washes are drawn before the text pass and carets after, so a pill covers the wash under an
  unselected hint (there is none: boundary hints are outside the wash) and never covers a caret.

**3g. The two unit tests.** Rows are built from a real document, so these use the Step 4.1
fixture helper. Add `hinted` and `fixture` in this commit, and the remaining helpers in commit 4.

```rust
/// A span is cut at each hint column strictly inside it; columns at its ends
/// and repeated columns cut nothing extra.
#[test]
fn split_at_columns_cuts_a_run_at_each_inner_column() {
    let runs: Vec<_> = split_at_columns(0..5, &[0, 2, 3, 3, 5]).collect();
    assert_eq!(runs, vec![0..2, 2..3, 3..5]);
    assert_eq!(split_at_columns(2..2, &[2]).count(), 0, "an empty span paints nothing");
}

/// A pill is dropped only where a selection strictly contains the hint's
/// offset; a hint at a selection's edge keeps its pill (it is not washed).
#[test]
fn a_hint_pill_is_dropped_only_strictly_inside_a_selection() {
    let mut doc = fixture();
    let pills = |doc: &Document| {
        let ed = Editor::new(doc, |_: Action| ());
        let rows = doc.rows();
        let layout = rows.layout(BufferRow(0));
        layout.inlays().map(|inlay| ed.inlay_pill(inlay, &test_geo(), 0.0).is_some()).collect::<Vec<_>>()
    };
    select(&mut doc, 2, 3);
    assert_eq!(pills(&doc), vec![true, true, true], "boundary hints keep their pills");
    select(&mut doc, 1, 4);
    assert_eq!(pills(&doc), vec![false, false, true], "T and P are washed; E is outside");
}
```

`inlays()` yields in render order, which is T (col 2), P (col 3), E (col 5) on the fixture.

### Step 4 — projection sites at their edges, and the headless tests (commit 4)

**4.1 The fixture** (`mod tests`, next to `folded_doc` / `test_geo`).

`test_geo()` is gutter 50, `TEXT_PAD` 6, advance 10, line_h 10, unscrolled. So
`cell_x(c) = 56 + 10·c` and `row_y(r) = 10·r`.

```rust
/// `text` with `hints` installed at the current revision, each `(offset, kind,
/// label, padding)`. The one place the tests build hints.
fn hinted(text: &str, hints: &[(u32, inlay::Kind, &str, inlay::Padding)]) -> Document {
    let mut doc = Document::new(text).expect("doc fits");
    let placed = hints
        .iter()
        .enumerate()
        .map(|(i, &(offset, kind, label, padding))| {
            let hint = inlay::Hint::new(kind, vec![inlay::Part::new(label, inlay::Link::None)], inlay::Key::new(i as u64))
                .expect("non-empty label")
                .padding(padding);
            inlay::Placed::new(offset, hint)
        })
        .collect();
    let outcome = doc.set_inlays(doc.revision(), placed);
    assert!(matches!(outcome, inlay::Outcome::Applied { .. }), "installed at the current revision");
    doc
}

/// `ab cd\nef\n` with three hints on row 0:
/// T = type `: i32` at 2 (Suffix on `ab`, width 5),
/// P = parameter `n:` + right padding at 3 (Prefix on `cd`, width 3),
/// E = type `: T` at 5, the line end (Suffix on `cd`, width 3).
fn fixture() -> Document {
    hinted("ab cd\nef\n", &[
        (2, inlay::Kind::Type, ": i32", inlay::Padding::default()),
        (3, inlay::Kind::Parameter, "n:", inlay::Padding { left: false, right: true }),
        (5, inlay::Kind::Type, ": T", inlay::Padding::default()),
    ])
}

/// Replace the selections with one from `anchor` to `head`.
fn select(doc: &mut Document, anchor: u32, head: u32) {
    let mut set = scrive_core::SelectionSet::new(0);
    set.set_single(scrive_core::Selection::from_anchor(scrive_core::SelectionId(0), anchor, head));
    doc.set_selections(set);
}
```

`Padding` is plain data with public fields and `Default` (MAP_PHASE_1), and the success variant
is `Outcome::Applied { count }`.

The fixture's cells on row 0 (`display_cell(col, edge)`):

| col | glyph | `End` | `Caret` | `Start` | hints at col |
|---|---|---|---|---|---|
| 0 | `a` | 0 | 0 | 0 | — |
| 1 | `b` | 1 | 1 | 1 | — |
| 2 | ` ` | 2 | 2 | 7 | T (Suffix, 5) |
| 3 | `c` | 8 | 11 | 11 | P (Prefix, 3) |
| 4 | `d` | 12 | 12 | 12 | — |
| 5 | EOL | 13 | 13 | 16 | E (Suffix, 3) |

`width()` = 16. The label cells are:
- T at 2..7;
- P's label `n:` at 8..10, then its padding cell at 10..11;
- E at 13..16.

Row 1 (`ef`, offsets 6..8) has no hints.

**4.2 The site audit.** For each row, find the site in `draw` or its helper. Confirm it uses the
edge in the right-hand column. Fix any that differ. Record every fix in the commit body, and make
the subject `fix(iced): …` if there is one.

| Site (HEAD location) | Required |
|---|---|
| carets in `draw` (1841-1847, via `offset_xy`) | `caret_edge(sel)` (Step 2a) |
| autoscroll in `layout` (1286, cell at 1324) | `Caret` |
| `popup_anchor` callers: completion (2718), hover (2749) | `Start` |
| `popup_anchor` caller: signature box (2832) | `Caret` |
| bracket colouring (1688: `row_layout.display_cell(col)`) | `Start` |
| glyph runs: `draw_row_inline`'s `seg` | `Start` |
| matching-bracket box (1708-1709, now `bracket_box`) | `Start` |
| collapsible box `xo` / `xc` (909-910) | `Start` |
| `collapsed_chip_rect` inline `xo` / `xc` (3186-3187) | `Start` |
| `wash_extent`: first row's `x0` (2990) | `Start` at `start`, or `Caret` if `start == end` |
| `wash_extent`: last row's `x1` (2992) | `End` at `end`, or `Caret` if `start == end` |
| `wash_extent`: interior row's `x1` (3000-3001) | `Start` at the line end, `+ advance * 0.5` |
| `wash_extent`: continuation row's `x0` (2990 else-arm) | `geo.cell_x(0.0)` (unchanged) |
| `squiggle_extent`: `x0` on the first row (1755) | `Start` at `start`, or `Caret` if `start == end` |
| `squiggle_extent`: `x0` on a continuation row (1743, 1755) | `End` at the row's column 0 (Decision, §3) |
| `squiggle_extent`: `x1` on the last row (1756) | `End` at `end`, or `Caret` if `start == end`; then `.max(x0 + advance)` |
| `squiggle_extent`: `x1` on an interior row (1744, 1756) | `Start` at the line end |
| `max_line_px` (960-974) | `layout.width()` / `header.width()` |
| `hit_test` (3071-3075) | `rows.hit(row, cell, Bias::Left)` (no edge) |

The squiggle's zero-width-row skip (HEAD 1750: `row_start == row_end && start != end`) stays as
it is (R15).

The `wash_extent` x math after the audit:

```rust
let empty = start == end;
let x0 = if row == a.row {
    self.offset_screen_x(rows, geo, start, if empty { Edge::Caret } else { Edge::Start })
} else {
    geo.cell_x(0.0)
};
let x1 = if row == b.row {
    self.offset_screen_x(rows, geo, end, if empty { Edge::Caret } else { Edge::End })
} else if let Some(header) = rows.header(BufferRow(row)) {
    geo.cell_x(header.tail_cell() as f32)
} else {
    let line_end = buffer.point_to_offset(scrive_core::Point::new(row, buffer.line_len(row)));
    // An interior row's end is after its end-of-line hints: they are inside the range.
    self.offset_screen_x(rows, geo, line_end, Edge::Start) + geo.advance() * 0.5
};
```

The `squiggle_extent` body after the audit:

```rust
let buffer = self.doc.buffer();
let empty = start == end;
let (row_start, start_edge) = if row == sp_row {
    (start, if empty { Edge::Caret } else { Edge::Start })
} else {
    // A hint at column 0 of a continuation row is inside the range.
    (buffer.point_to_offset(BufPoint { row, col: 0 }), Edge::End)
};
let (row_end, end_edge) = if row == ep_row {
    (end, if empty { Edge::Caret } else { Edge::End })
} else {
    // The interior row's end is after its end-of-line hints: they are inside the range.
    (buffer.point_to_offset(BufPoint { row, col: buffer.line_len(row) }), Edge::Start)
};
if row_start == row_end && !empty {
    return None;
}
let x0 = self.offset_screen_x(rows, geo, row_start, start_edge);
let x1 = self.offset_screen_x(rows, geo, row_end, end_edge).max(x0 + geo.advance());
Some((x0, x1))
```

Keep comments only where they carry a why (DISPATCH override 1). The two above do.

**4.3 The seven headless tests.** All use `fixture()`, `select`, `test_geo()` and `doc.rows()`.
Expected values are in pixels from `cell_x(c) = 56 + 10·c`.

```rust
/// An empty caret sits on the caret edge (before a Suffix hint, after a Prefix
/// one); a non-empty selection's caret sits on its wash edge instead.
#[test]
fn caret_x_sits_on_the_edge_its_selection_names() {
    let mut doc = fixture();
    let geo = test_geo();
    let mut caret_x = |anchor: u32, head: u32| {
        select(&mut doc, anchor, head);
        let ed = Editor::new(&doc, |_: Action| ());
        let rows = doc.rows();
        let sel = *doc.selections().newest();
        ed.offset_screen_x(&rows, &geo, sel.head(), caret_edge(&sel))
    };
    assert_eq!(caret_x(2, 2), 76.0, "before the Suffix hint T: typed text lands before it");
    assert_eq!(caret_x(3, 3), 166.0, "after the Prefix hint P: typed text lands after it");
    assert_eq!(caret_x(5, 5), 186.0, "before the end-of-line Suffix hint E");
    assert_eq!(caret_x(0, 3), 136.0, "forward selection ends at End, before P");
    assert_eq!(caret_x(5, 2), 126.0, "reversed selection's head is at Start, after T");
}
```

Two practical points:
- If `Selection` isn't `Copy`, clone it.
- `doc.rows()` holds a `Ref` on the fold cache. Drop it before the next `select` (the closure
  scope does). Otherwise `set_selections` may panic or fail to borrow.

```rust
/// A one-row wash runs from Start to End: hints at its ends stay outside, hints
/// strictly inside are covered; an empty range sits at the caret edge.
#[test]
fn washes_exclude_boundary_hints_and_cover_interior_ones() {
    let doc = fixture();
    let ed = Editor::new(&doc, |_: Action| ());
    let rows = doc.rows();
    let geo = test_geo();
    let p = |off: u32| doc.buffer().offset_to_point(off);
    let wash = |start: u32, end: u32| ed.wash_extent(&rows, &geo, 0, p(start), p(end), start, end);
    assert_eq!(wash(2, 3), (126.0, 136.0), "just the space: T and P are boundary hints");
    assert_eq!(wash(1, 4), (66.0, 176.0), "T and P are inside and washed");
    assert_eq!(wash(3, 3), (166.0, 166.0), "an empty range sits at Caret (after P)");
}

/// On a multi-row range, the first row's wash ends after its end-of-line hint
/// (it is inside the range), and the next row starts at the text origin.
#[test]
fn an_interior_row_wash_runs_past_end_of_line_hints() {
    let doc = fixture();
    let ed = Editor::new(&doc, |_: Action| ());
    let rows = doc.rows();
    let geo = test_geo();
    let (a, b) = (doc.buffer().offset_to_point(4), doc.buffer().offset_to_point(7));
    assert_eq!(ed.wash_extent(&rows, &geo, 0, a, b, 4, 7), (176.0, 221.0), "to Start of EOL (16) + half a cell");
    assert_eq!(ed.wash_extent(&rows, &geo, 1, a, b, 4, 7), (56.0, 66.0));
}

/// Squiggles share the wash's edges; a zero-width diagnostic is one cell wide
/// from the caret edge, so it marks the side typing would land on.
#[test]
fn squiggles_span_start_to_end_and_an_empty_one_sits_at_the_caret_edge() {
    let doc = fixture();
    let ed = Editor::new(&doc, |_: Action| ());
    let rows = doc.rows();
    let geo = test_geo();
    let on_row_0 = |start: u32, end: u32| ed.squiggle_extent(&rows, &geo, 0, start, end, 0, 0);
    assert_eq!(on_row_0(2, 3), Some((126.0, 136.0)), "Start of 2 to End of 3");
    assert_eq!(on_row_0(2, 2), Some((76.0, 86.0)), "empty at T's offset: before T, one cell");
    assert_eq!(on_row_0(3, 3), Some((166.0, 176.0)), "empty at P's offset: after P, one cell");
    assert_eq!(on_row_0(5, 5), Some((186.0, 196.0)), "empty at the line end: before E");
    // A diagnostic over `d` … `e` (4..7): the first row runs past E, the second
    // starts at column 0.
    assert_eq!(ed.squiggle_extent(&rows, &geo, 0, 4, 7, 0, 1), Some((176.0, 216.0)));
    assert_eq!(ed.squiggle_extent(&rows, &geo, 1, 4, 7, 0, 1), Some((56.0, 66.0)));
    // One that starts at row 0's line end covers nothing there.
    assert_eq!(ed.squiggle_extent(&rows, &geo, 0, 5, 7, 0, 1), None);
}

/// The matching-bracket box sits on the bracket glyph, after a Suffix hint that
/// shares the bracket's offset, and its partner shifts with the row.
#[test]
fn the_bracket_box_sits_after_a_hint_at_the_bracket() {
    let mut doc = hinted("ab(c)\n", &[(2, inlay::Kind::Type, ": i32", inlay::Padding::default())]);
    select(&mut doc, 3, 3);
    assert_eq!(doc.brackets().active_pair(3).map(|(x, y)| (x.min(y), x.max(y))), Some((2, 4)), "the caret is next to `(`");
    let ed = Editor::new(&doc, |_: Action| ());
    let rows = doc.rows();
    let geo = test_geo();
    let x = |off: u32| ed.bracket_box(&rows, &geo, off).expect("visible").x;
    assert_eq!(x(2), 126.0, "`(` at Start: cell 2 + 5");
    assert_eq!(x(4), 146.0, "`)` at cell 4 + 5");
}

/// An end-of-line hint widens the row, so horizontal scroll reaches it.
#[test]
fn max_scroll_reaches_an_end_of_line_hint() {
    let line = "x".repeat(100);
    let plain = Document::new(&format!("{line}\n")).expect("doc fits");
    let doc = hinted(&format!("{line}\n"), &[(100, inlay::Kind::Type, ": usize", inlay::Padding::default())]);
    let vp = Rectangle { x: 0.0, y: 0.0, width: 300.0, height: 200.0 };
    let without = Editor::new(&plain, |_: Action| ());
    let with = Editor::new(&doc, |_: Action| ());
    let (rows, plain_rows) = (doc.rows(), plain.rows());
    assert_eq!(with.max_line_px(&rows, 10.0, 20.0, vp, 0.0), 107.0 * 10.0, "100 cells of text + 7 of hint");
    assert_eq!(
        with.max_scroll_x(&rows, vp, 10.0, 20.0, 0.0) - without.max_scroll_x(&plain_rows, vp, 10.0, 20.0, 0.0),
        70.0,
        "the scroll range grows by exactly the hint",
    );
}

/// A click anywhere on a hint, label or padding, places the caret at the hint's
/// offset; the caret then renders on that offset's caret edge.
#[test]
fn a_click_on_a_hint_places_the_caret_at_its_offset() {
    let doc = fixture();
    let mut r = headless_renderer();
    // The geometry `pump`'s widget uses: real metrics, a 500x320 viewport at the origin, unscrolled.
    let ed = Editor::new(&doc, |a: Action| a);
    let mut state = State::default();
    ed.ensure_metrics(&mut state);
    let geo = ed.geo(&state, Rectangle { x: 0.0, y: 0.0, width: 500.0, height: 320.0 });
    let y = geo.line_h() / 2.0;
    for (cell, offset, what) in [(4.3, 2, "T's label"), (8.3, 3, "P's label"), (10.3, 3, "P's padding"), (14.3, 5, "E at the line end")] {
        let at = Point::new(geo.cell_x(cell), y);
        let press = [iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))];
        let (actions, _) = pump(&doc, None, iced_runtime::user_interface::Cache::new(), &mut r, at, &press);
        assert!(actions.contains(&Action::PlaceCaret(offset)), "a click on {what} lands at {offset}: {actions:?}");
    }
}
```

Notes on the click test:
- A fresh `Cache` per press keeps each press a single click.
- The cells are `.3` into a cell, so neither rounding direction can cross a hint boundary:
  - 4.3 is in T (2..7);
  - 8.3 is in P's label (8..10);
  - 10.3 is in P's padding cell (10..11), and it rounds to 10 or 11, both offset 3;
  - 14.3 is in E (13..16).
- If `State::default()` doesn't give the scroll `pump`'s layout settles on, fall back to
  `test_geo`-style metrics. Or read the metrics out of the cache's tree after one `pump` with no
  events. The point is to use the same `Geo` the widget uses.

If one of these fails because a Phase 2 site uses the wrong edge, fix the site (4.2), not the
expectation. The expectations come from D6 and the fixture table.

## 5. Files changed

| File | Change |
|---|---|
| crates/scrive-iced/src/editor.rs | `expand_tabs(run, start_cell)` and its 4 callers; `caret_edge`, `wash_extent`, `squiggle_extent`, `bracket_box`, `split_at_columns`, `hint_selected`, `inlay_pill`; `INLAY_TEXT_A`; `draw_row_inline` splits runs at hint columns and paints labels and pills; edge fixes from the 4.2 audit; tests and the `hinted` / `fixture` / `select` helpers |
| crates/scrive-iced/src/geo.rs | `Geo::inlay_pill` |

No other file changes. If a prerequisite API is missing in scrive-core, stop (§1).

## 6. Verification

At each commit boundary (DISPATCH override 2):

```
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p scrive-iced
cargo test -p scrive-iced --all-features
```

Then save the cumulative patch and the message:

```
git add -N <new files, if any>
git diff <BASE> -- . ':(exclude).claude' > .claude/map/inlay-hints/patches/phase4-<k>.patch
# .claude/map/inlay-hints/patches/phase4-<k>.msg: the subject from the §4 table, plus a 1-3 line why
```

At the end of the phase:

```
cargo test --workspace
cargo test --workspace --all-features
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --all-features
cargo build --workspace --all-targets --all-features --target wasm32-unknown-unknown
cargo test -p scrive-iced -- a_run_expands split_at_columns hint_pill caret_x washes_exclude interior_row_wash squiggles_span bracket_box_sits max_scroll_reaches click_on_a_hint
cargo test -p scrive-iced -- draw_budget max_line_px hit_test_inverts popup_anchors collapsed_chip
```

The last line re-runs the fold-geometry tests that pass through the changed helpers.

Don't run `cargo run --example …`. The orchestrator checks it by eye:
- `cargo run -p scrive-iced --features lsp --example rust_analyzer` once Phase 7 lands. Until then
  there is no host that installs hints.
- Labels sit on pills.
- A tabbed line with a highlighted span no longer drifts right of the caret.
- Selecting across a hint washes it with no dark pill inside.

## 7. Spot-check tables

### Fixture row 0 (`ab cd`, T `: i32` at 2, P `n:␠` at 3, E `: T` at 5), `cell_x(c) = 56 + 10c`

| Query | Edge | Cell | px |
|---|---|---|---|
| caret at 2 (empty) | `Caret` | 2 | 76 |
| caret at 3 (empty) | `Caret` | 11 | 166 |
| caret at 5 (empty) | `Caret` | 13 | 186 |
| head of 0→3 | `End` | 8 | 136 |
| head of 5→2 | `Start` | 7 | 126 |
| wash 2..3 | `Start`..`End` | 7..8 | 126..136 |
| wash 1..4 | `Start`..`End` | 1..12 | 66..176 |
| wash 3..3 | `Caret`..`Caret` | 11..11 | 166..166 (filled 1 px wide) |
| wash 4..7, row 0 | `Start`..`Start`(EOL) + ½ | 12..16.5 | 176..221 |
| wash 4..7, row 1 | origin..`End` | 0..1 | 56..66 |
| squiggle 2..2 | `Caret`, + 1 cell | 2..3 | 76..86 |
| squiggle 4..7, row 0 | `Start`..`Start`(EOL) | 12..16 | 176..216 |
| squiggle 5..7, row 0 | — | skipped | — |
| row width | `width()` | 16 | 160 px of text |

### Pill suppression (`hint_selected(p)`: some selection with `start < p < end`)

| Selection | T (p=2) | P (p=3) | E (p=5) |
|---|---|---|---|
| none / caret | pill | pill | pill |
| 2..3 | pill (start) | pill (end) | pill |
| 1..4 | washed, no pill | washed, no pill | pill |
| 4..7 | pill | pill | washed, no pill |
| 0..5 | no pill | no pill | pill (end) |

### Run splitting (`split_at_columns`)

| Span | Hint cols | Runs, each at `display_cell(run.start, Start)` |
|---|---|---|
| `0..5` (`ab cd`, one span) | 2, 3, 5 | `0..2`@0, `2..3`@7, `3..5`@11 |
| `0..2` | 2, 3, 5 | `0..2`@0 (2 is the end, not inside) |
| `2..5` | 2, 3, 5 | `2..3`@7, `3..5`@11 |

### Tab phase (`TAB = 4`)

| Line | Run | Raw start cell | Painted (old) | Painted (new) |
|---|---|---|---|---|
| `x\ty` | `\ty` | 1 | `····y` (y at 5) | `···y` (y at 4) |
| `a\tbc\t\td` | `\t\td` | 6 | `········d` (d at 14) | `······d` (d at 12) |
| `\tx` | `\tx` | 0 | `····x` | `····x` (unchanged) |

## 8. What NOT to change

- No scrive-core or scrive-lsp code. In particular, don't add the label accessor here (Phase 3
  owns `RowLayout::inlays()`, R2), and don't touch `RowLayout`'s edge math.
- No gestures, hover, `Action` variants or `CodeEditor` code. Hint hover, Ctrl+click and
  double-click are Phase 5. A click on a hint goes through the plain `hit_test` path, as specified.
- Don't change the chip pill or its suppression (`range_selected`, editor.rs:3134). Chips keep
  their span test; only hints use strict interior.
- The fold preview (`draw_fold_preview`) and the collapsed header's tail show no hints (out of
  scope). They change only by commit 1's tab phase.
- `draw_spans` stays the plain-row painter. Rows with hints never reach it, because `is_plain()` is
  false there.
- Don't change the draw-budget gate, the wash colours, `squiggle_spans`, or the zero-width-row skip
  rule's offset test (R15).
- No visible-whitespace pass exists. If one is ever added, it must skip hint cells (FOSS notes). It
  is not part of this phase.
- Never run `cargo fmt`.

## 9. Pitfalls

- **Raw cell vs display cell.** The tab phase is `display_map::expand(line, col, TAB)`, the raw
  cell. The x position is `display_cell(col, Edge::Start)`, the display cell. Swapping them
  reintroduces the drift after every chip and hint.
- **`dim` is the selection colour.** `dim` and `selection_color` are both
  `palette.background.strong.color` (editor.rs:1393, 1397). Never paint labels in `dim`.
- **`range_covered_by` is inclusive.** Strict interior is `range_covered_by(sels, p - 1, p + 1)`
  with `p > 0`. `range_selected(p, p)` would also match selections that merely touch `p`, which
  is the chip test D8 rules out.
- **`Rc<RowLayout>`, not `Ref`.** `Rows::layout` returns an `Rc`. Keep it as a local and pass
  `&*layout` / `&layout`. Never hold a `RefCell` borrow from `Rows` across another projection.
  `inlay_pill` → `hint_selected` only reads selections, which is fine.
- **`doc.rows()` borrows the fold cache.** In tests, drop `rows` before mutating the document
  (`select`, `set_inlays`). The `select` → `Editor::new` → `rows()` order in the tests is
  deliberate.
- **Empty ranges.** `Start..End` on an empty range is inverted: End ≤ Caret ≤ Start, so `x1 < x0`
  when hints sit there. Every range helper must check `start == end` first and use `Caret` for both
  ends. The existing `.max(1.0)` (wash) and `.max(x0 + advance)` (squiggle) widths then apply.
- **`split_at_columns` with repeated columns.** Two hints at one column give that column twice. The
  `c > from` guard drops the empty run. Don't `dedup` the `Vec` in place on the shared layout data.
  Collect to a local `Vec` first, as 3e does.
- **Header rows.** A collapsed header with a hint takes `draw_row_inline` (not plain), then the
  placeholder block. Don't add a second hint pass for headers.
- **clippy.**
  - The new helpers with many parameters carry
    `#[allow(clippy::too_many_arguments)] // <reason>` like `draw_wash_row`.
  - Write `u32::from(bool)`, not `as u32`, on padding flags.
  - `INLAY_TEXT_A` must be used outside tests, or it fails `-D warnings` (dead code).
- **Test float equality.** The fixture values are exact in `f32` (integers and halves under 1024).
  `assert_eq!` on `f32` is fine there. Don't introduce scroll or fractional advances in these tests.
- **`Selection` copies.** `doc.selections().newest()` returns `&Selection`. Copy or clone it before
  the `Ref` from `rows()` matters.
- **Comments.** Per DISPATCH override 1: no "Phase 4", "D8" or "now" in code comments. Doc comments
  on the new private helpers say what they return, not how.

## 10. Resolved questions

1. **The label accessor:** Phase 3 adds `RowLayout::inlays()` yielding `&row_layout::Inlay { key,
   offset, cell, width, padding, hint }` (R2); Phase 1's getters are `parts()`, `padded()`,
   `Part::text()` (R1). Phase 4 still touches no core file.
2. **Continuation-row start of a multi-row squiggle:** `End` at column 0 (R15).
3. **An empty interior line with a hint:** the zero-width-row skip stays (R15).
4. **Label colour on a washed hint:** the dimmed colour, `a = 0.6`, on and off the wash (R14).
5. **Find, occurrence and scope washes:** they keep the pill; only selection washes hide it,
   strictly inside (R14).
6. **Plan line drift:** MAP_PLAN now cites editor.rs:3134 for the chips' span test (R24).
7. **Phase 1–3 shapes:** the snippets now use the names those docs settled.

Still open: none.
