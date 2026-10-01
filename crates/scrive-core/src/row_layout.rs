//! Per-row horizontal projection — the **one owner** of byte-column ↔
//! display-cell math on rows with collapsed inline folds, and of the collapsed
//! block header's `head … tail` one-line layout.
//!
//! Every consumer that needs to place something horizontally — render, caret
//! placement, hit testing, selection, and movement — consults the functions
//! here rather than recomputing the inline-collapse shift, its inverse, the
//! chip-center formula, the caret/glyph boundary predicates, or the header
//! width that feeds a collapsed block's inline tail. Because each of those
//! facts has exactly one definition, a change to the chip layout or a boundary
//! rule is a one-site edit and every site stays in agreement by construction.
//!
//! Everything here is in **cells and columns** — GUI-free. The widget's only
//! remaining job is `x = origin + cell × advance` (and its inverse).

use std::borrow::Cow;
use std::cell::{Ref, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use crate::buffer::Buffer;
use crate::coords::{Bias, Point};
use crate::decorations::DecorationStore;
use crate::display_map::{self, BufferRow, DisplayRow};
use crate::fold_map::{FoldMap, InlineFold};

// Op-count canary: counts `FoldMap::renders` and `Rows::position` probes on
// this thread so a test can assert `expand_folds_touched` probes O(edit points)
// per commit — once per point, for the hidden-gap check — and never O(candidates · edits), which
// would make a document-scale multi-caret edit over a folded document cost the
// product of the two. Debug/test only; zero-cost in release.
#[cfg(any(test, debug_assertions))]
thread_local! {
    pub(crate) static DISPLAY_POSITION_PROBES: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
}

/// Display cells a collapsed inline fold's `…` chip occupies. The chip
/// starts one cell past the opening bracket; the interior collapses to this.
pub const INLINE_CHIP_CELLS: u32 = 3;

/// Display cells between a collapsed block header's end and its inline closing
/// tail — the ` … ` placeholder gap.
pub const FOLD_PLACEHOLDER_CELLS: u32 = 4;

/// Where a caret at some byte column renders horizontally: a whole display
/// cell, or — for a column hidden inside a collapsed inline fold — the center
/// of that fold's `…` chip. The chip center is the **only** fractional cell in
/// the system; fencing it in this enum keeps `f32` out of the inverse maps.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum CaretCell {
    /// An exact display cell.
    Cell(u32),
    /// The fractional cell at a collapsed chip's visual center.
    ChipCenter(f32),
}

impl CaretCell {
    /// The (possibly fractional) display-cell value, for pixel projection.
    #[must_use]
    pub fn cells(self) -> f32 {
        match self {
            Self::Cell(c) => c as f32,
            Self::ChipCenter(c) => c,
        }
    }
}

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

/// Where a buffer offset renders: its display row plus its horizontal
/// [`CaretCell`]. THE owner of "where does offset O show on screen" — a caret,
/// selection endpoint, squiggle bound, popup anchor, and autoscroll target all
/// read the same value (see [`Rows::position`]).
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct DisplayPosition {
    /// The visible display row (a hidden closing tail resolves to its header's).
    pub row: DisplayRow,
    /// The horizontal position on that row.
    pub x: CaretCell,
}

/// One collapsed inline fold's `…` chip on a row, in display cells, as named
/// fields the widget destructures to render and hit-test the chip.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Chip {
    /// The chip's first display cell (one past the opening bracket).
    pub cell: u32,
    /// The chip's visual center, in fractional display cells.
    pub center: f32,
    /// Byte column of the opening bracket on its line.
    pub open_col: u32,
    /// Byte column of the closing bracket on its line.
    pub close_col: u32,
}

/// One visible glyph of a collapsed block's inline closing tail, resolved to
/// the display cell it occupies on the *header* row.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct TailGlyph {
    /// Byte column on the fold's *last* buffer row.
    pub col: u32,
    /// Display cell on the header's display row.
    pub cell: u32,
    /// The character itself.
    pub ch: char,
}

/// Which region of a collapsed block header's display line a cell falls in.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum HeaderHit {
    /// On the header's own text — resolve against the header row's [`RowLayout`].
    Head,
    /// In the ` … ` placeholder gap — clamps to the header line's end.
    Gap,
    /// On the inline closing tail: the byte column on the fold's *last* row.
    Tail(u32),
}

/// The caret slot just after a pair's opening bracket — the LEFT landable edge
/// of a collapsed inline gap. The one place `open + 1` is written.
#[must_use]
pub fn gap_left_edge(open: u32) -> u32 {
    open + 1
}

/// Whether caret offset `off` is strictly inside the collapsed gap of the pair
/// `(open, close)`. THE caret-boundary rule: `open+1` and `close` are
/// landable; strictly between them hides. (Its off-by-one sibling is
/// [`gap_hides_glyph`] — a *glyph* at `open+1` hides even though the caret slot
/// there is landable. The two variants exist as exactly these two functions.)
#[must_use]
pub fn gap_hides_caret(open: u32, close: u32, off: u32) -> bool {
    off > gap_left_edge(open) && off < close
}

/// Whether the glyph at offset `off` is hidden by the collapsed pair
/// `(open, close)`: everything strictly between the brackets.
#[must_use]
pub fn gap_hides_glyph(open: u32, close: u32, off: u32) -> bool {
    off > open && off < close
}

/// Byte column where a line's visible content starts (its leading whitespace
/// length) — where a collapsed block's closing tail begins on its last row.
/// The one `trim_start` rule, shared by movement and the widget.
#[must_use]
pub fn tail_start_col(line: &str) -> u32 {
    (line.len() - line.trim_start().len()) as u32
}

/// Whether a bracket pair `(open, close)` has a *non-empty interior* — the one
/// foldability rule: there must be something between the brackets to
/// hide. Shared by the document's foldable-pair enumeration and the fold map's
/// inline/block resolution.
#[must_use]
pub fn pair_has_interior(open: u32, close: u32) -> bool {
    close > open + 1
}

/// Round a fractional display cell to the nearest cell boundary, floored at 0
/// — the quantization for a column (box) selection's *virtual* corner cells,
/// which may lie past any line's content and so can't resolve through
/// [`RowLayout::hit`] yet. The same round-half-up rule `hit` applies over real
/// content, so a box edge and a click at the same pixel agree on the boundary.
#[must_use]
pub fn virtual_cell(cell: f32) -> u32 {
    cell.round().max(0.0) as u32
}

/// One collapsed inline fold on a row, with its bracket cells precomputed.
#[derive(Copy, Clone, Debug)]
struct InlineSpan {
    /// Tab-expanded (pre-collapse) cell of the opening bracket.
    open_cell: u32,
    /// Tab-expanded (pre-collapse) cell of the closing bracket.
    close_cell: u32,
    fold: InlineFold,
}

/// A visible buffer row's horizontal projection: byte column ↔ display cell,
/// with tab expansion *and* the horizontal collapse of every root inline fold
/// on the row. Built by [`Rows::layout`], which shares it for the view's
/// lifetime; holds only the row's text (a [`Cow`] — borrowed straight off the
/// backing when the row is stored contiguously, owned only when it spans a
/// chunk boundary) and copies of the row's folds, so it carries no derived
/// state that could drift out of sync with the document.
pub struct RowLayout<'a> {
    line: Cow<'a, str>,
    /// Byte offset of the row's first character (column 0), for offset-space
    /// boundary predicates.
    row_start: u32,
    tab: u32,
    /// This row's root inline folds, sorted by opening cell.
    spans: Vec<InlineSpan>,
}

impl<'a> RowLayout<'a> {
    fn new(fold_map: &FoldMap, buffer: &'a Buffer, row: BufferRow, tab: u32) -> Self {
        let line = buffer.line(row.0);
        let row_start = buffer.point_to_offset(Point::new(row.0, 0));
        // This row's inline folds — an O(log n + hits) windowed descent into the
        // fold tree, not an O(all inline folds) scan/materialize per row per frame.
        let mut spans: Vec<InlineSpan> = fold_map
            .inline_folds_on_row(row.0)
            .into_iter()
            .map(|fold| InlineSpan {
                open_cell: display_map::expand(&line, fold.open - row_start, tab),
                close_cell: display_map::expand(&line, fold.close - row_start, tab),
                fold,
            })
            .collect();
        spans.sort_by_key(|s| s.open_cell);
        Self { line, row_start, tab, spans }
    }

    /// Whether the row has no collapsed inline folds (the identity projection).
    #[must_use]
    pub fn is_plain(&self) -> bool {
        self.spans.is_empty()
    }

    /// Byte offset of the row's column 0 — for turning a chip's byte columns back
    /// into document offsets (e.g. to test a chip against the selection set).
    #[must_use]
    pub fn row_start(&self) -> u32 {
        self.row_start
    }

    /// Collapse shift at pre-collapse cell `cell`: every fold whose closing
    /// bracket sits at/before it has hidden `close−open−1` cells behind an
    /// [`INLINE_CHIP_CELLS`]-wide chip.
    fn shift_at(&self, cell: u32) -> i32 {
        self.spans
            .iter()
            .filter(|s| cell >= s.close_cell)
            .map(|s| (s.close_cell as i32 - s.open_cell as i32 - 1) - INLINE_CHIP_CELLS as i32)
            .sum()
    }

    /// Pre-collapse (tab-expanded) cell → post-collapse display cell.
    fn cell_of(&self, raw_cell: u32) -> u32 {
        (raw_cell as i32 - self.shift_at(raw_cell)).max(0) as u32
    }

    /// Byte column → display cell (tab-expanded, inline-collapsed), on the
    /// `edge` side of any hints at `col`. Total and monotone; a column hidden
    /// inside a chip maps into the chip's span (use [`Self::caret_cell`] for
    /// caret placement, which clips to the center).
    #[must_use]
    pub fn display_cell(&self, col: u32, _edge: Edge) -> u32 {
        self.cell_of(display_map::expand(&self.line, col, self.tab))
    }

    /// Caret placement for a byte column: its display cell, or the chip center
    /// when the column is hidden inside a collapsed inline fold's gap.
    #[must_use]
    pub fn caret_cell(&self, col: u32) -> CaretCell {
        let off = self.row_start + col;
        match self.spans.iter().find(|s| s.fold.hides_caret_at(off)) {
            Some(s) => CaretCell::ChipCenter(
                self.cell_of(s.open_cell + 1) as f32 + INLINE_CHIP_CELLS as f32 / 2.0,
            ),
            None => CaretCell::Cell(self.display_cell(col, Edge::Caret)),
        }
    }

    /// Whether the glyph at byte column `col` is hidden inside a collapsed
    /// inline fold (the deliberately-different sibling of the caret rule: the
    /// glyph at `open+1` hides while its caret slot stays landable).
    #[must_use]
    pub fn glyph_hidden(&self, col: u32) -> bool {
        let off = self.row_start + col;
        self.spans.iter().any(|s| s.fold.hides_glyph_at(off))
    }

    /// Inverse projection: a (fractional, unrounded) display cell → the byte
    /// column a click there lands on. Rounding policy lives HERE, not at call
    /// sites. A cell on a chip resolves to just after the opening bracket;
    /// past-EOL clamps; mid-tab snaps by `bias`.
    #[must_use]
    pub fn hit(&self, cell: f32, bias: Bias) -> u32 {
        let dc = cell.round().max(0.0) as u32;
        let mut extra = 0i32;
        for s in &self.spans {
            // Compare in DISPLAY space: prior collapsed folds shift this
            // fold's bracket cells left before the click can be tested.
            let d_open = self.cell_of(s.open_cell);
            let d_chip_end = d_open + 1 + INLINE_CHIP_CELLS;
            if dc >= d_chip_end {
                extra += (s.close_cell as i32 - s.open_cell as i32 - 1) - INLINE_CHIP_CELLS as i32;
            } else if dc > d_open {
                return s.fold.left_edge() - self.row_start; // on the chip → just after `[`
            }
        }
        let raw_cell = (dc as i32 + extra).max(0) as u32;
        display_map::collapse(&self.line, raw_cell, self.tab, bias)
    }

    /// The row's rendered display width in cells (tab-expanded, collapsed).
    /// A collapsed block's inline tail begins [`FOLD_PLACEHOLDER_CELLS`] past
    /// this — see [`HeaderLayout::tail_cell`].
    #[must_use]
    pub fn width(&self) -> u32 {
        self.display_cell(self.line.len() as u32, Edge::Start)
    }

    /// The row's collapsed chips, in display order.
    pub fn chips(&self) -> impl Iterator<Item = Chip> + '_ {
        self.spans.iter().map(|s| {
            let cell = self.cell_of(s.open_cell + 1);
            Chip {
                cell,
                center: cell as f32 + INLINE_CHIP_CELLS as f32 / 2.0,
                open_col: s.fold.open - self.row_start,
                close_col: s.fold.close - self.row_start,
            }
        })
    }
}

/// A collapsed block fold's one-line display layout: the (inline-fold-
/// aware) header text, the ` … ` placeholder gap, then the fold's real closing
/// tail — `fn main() { … }`. The single source for the placeholder render,
/// caret placement on the tail, selection washes, hit-testing, the hover-chip
/// rect, and the preview anchor, so they agree to the pixel by construction.
pub struct HeaderLayout<'a> {
    /// The header row's own horizontal projection, shared with the view that
    /// built it.
    head: Rc<RowLayout<'a>>,
    /// The fold's last buffer row — the line the tail glyphs come from.
    last: BufferRow,
    tail_line: Cow<'a, str>,
    /// Byte column where the visible tail starts on `last` (its leading ws).
    tail_lead: u32,
    tab: u32,
}

impl<'a> HeaderLayout<'a> {
    fn new(head: Rc<RowLayout<'a>>, last: BufferRow, buffer: &'a Buffer, tab: u32) -> Self {
        let tail_line = buffer.line(last.0);
        let tail_lead = tail_start_col(&tail_line);
        Self { head, last, tail_line, tail_lead, tab }
    }

    /// The header row's projection (for hits resolving to [`HeaderHit::Head`]).
    #[must_use]
    pub fn head(&self) -> &RowLayout<'a> {
        &self.head
    }

    /// Rendered display width of the header text — where the placeholder gap
    /// begins. Inline-fold aware: a collapsed inline fold before the block's
    /// opener shrinks it.
    #[must_use]
    pub fn head_cells(&self) -> u32 {
        self.head.width()
    }

    /// The fractional cell at the center of the ` … ` gap, where the chip and
    /// its `…` glyph both center.
    #[must_use]
    pub fn gap_center(&self) -> f32 {
        self.head_cells() as f32 + FOLD_PLACEHOLDER_CELLS as f32 / 2.0
    }

    /// The display cell where the inline closing tail begins.
    #[must_use]
    pub fn tail_cell(&self) -> u32 {
        self.head_cells() + FOLD_PLACEHOLDER_CELLS
    }

    /// The fold's last buffer row (the tail's real home).
    #[must_use]
    pub fn last_row(&self) -> BufferRow {
        self.last
    }

    /// Byte column on the last row where the visible tail starts.
    #[must_use]
    pub fn tail_start_col(&self) -> u32 {
        self.tail_lead
    }

    /// Tab-expanded cell of the tail's first visible column on its own row.
    fn lead_cells(&self) -> u32 {
        display_map::expand(&self.tail_line, self.tail_lead, self.tab)
    }

    /// Byte column on the *last* row → display cell on the header row, using
    /// full-line tab stops (the caret/hit/selection convention). `None` for a
    /// column in the leading whitespace before the visible tail.
    #[must_use]
    pub fn tail_col_cell(&self, col: u32) -> Option<u32> {
        (col >= self.tail_lead)
            .then(|| self.tail_cell() + display_map::expand(&self.tail_line, col, self.tab) - self.lead_cells())
    }

    /// The tail's rendered width in cells.
    #[must_use]
    pub fn tail_cells(&self) -> u32 {
        display_map::expand(&self.tail_line, self.tail_line.len() as u32, self.tab) - self.lead_cells()
    }

    /// The collapsed line's total rendered width (head + gap + tail), in cells.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.tail_cell() + self.tail_cells()
    }

    /// The visible tail glyphs at their header-row display cells.
    pub fn tail_glyphs(&self) -> impl Iterator<Item = TailGlyph> + '_ {
        self.tail_line[self.tail_lead as usize..].char_indices().map(move |(i, ch)| {
            let col = self.tail_lead + i as u32;
            TailGlyph {
                col,
                cell: self.tail_col_cell(col).expect("tail glyph is at/after the lead"),
                ch,
            }
        })
    }

    /// Resolve a (fractional) display cell on the collapsed header line to the
    /// region it falls in, with the ±half-cell boundaries the hit-test has
    /// always used: at/after the tail (minus half a cell) → the tail column;
    /// past the header text (plus half a cell) → the gap; else the head.
    #[must_use]
    pub fn hit(&self, cell: f32, bias: Bias) -> HeaderHit {
        if cell >= self.tail_cell() as f32 - 0.5 {
            let cell_in_tail = (cell - self.tail_cell() as f32).round().max(0.0) as u32;
            let col = display_map::collapse(&self.tail_line, self.lead_cells() + cell_in_tail, self.tab, bias);
            HeaderHit::Tail(col)
        } else if cell > self.head_cells() as f32 + 0.5 {
            HeaderHit::Gap
        } else {
            HeaderHit::Head
        }
    }
}

impl FoldMap {
    /// A fresh horizontal projection of visible buffer `row`; [`Rows::layout`]
    /// shares one per row for a view's lifetime.
    #[must_use]
    pub(crate) fn row_layout<'a>(&self, buffer: &'a Buffer, row: BufferRow, tab: u32) -> RowLayout<'a> {
        RowLayout::new(self, buffer, row, tab)
    }

    /// The `head … tail` one-line layout of `row`, iff it is a collapsed block
    /// fold's header.
    #[must_use]
    pub(crate) fn header_layout<'a>(&self, buffer: &'a Buffer, row: BufferRow, tab: u32) -> Option<HeaderLayout<'a>> {
        let last = self.fold_at_header(row)?;
        Some(HeaderLayout::new(Rc::new(self.row_layout(buffer, row, tab)), last, buffer, tab))
    }

    /// Whether buffer `offset` renders anywhere: `false` only inside a
    /// collapsed block's gap or before the visible tail on its last row.
    /// Hint-free, so it answers inside `rebase_views` while the inlay store is
    /// being moved; geometry goes through [`Rows::position`].
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

    /// Inverse of [`Rows::position`] on one visible row: a (fractional,
    /// unrounded) display cell → the byte offset a click there lands on,
    /// resolving a collapsed header's gap (→ header line end) and tail
    /// (→ the last row's column) before the plain row projection.
    #[must_use]
    pub(crate) fn hit_row(&self, buffer: &Buffer, row: BufferRow, cell: f32, bias: Bias, tab: u32) -> u32 {
        if let Some(layout) = self.header_layout(buffer, row, tab) {
            match layout.hit(cell, bias) {
                HeaderHit::Tail(col) => return buffer.point_to_offset(Point::new(layout.last_row().0, col)),
                HeaderHit::Gap => return buffer.point_to_offset(Point::new(row.0, buffer.line_len(row.0))),
                HeaderHit::Head => {}
            }
        }
        let layout = self.row_layout(buffer, row, tab);
        buffer.point_to_offset(Point::new(row.0, layout.hit(cell, bias)))
    }

    /// THE pixel-y inversion policy, in row units: a (fractional) count of
    /// display rows from the content top → the display row it falls on,
    /// floored and clamped to the valid range. Every y-driven hit (clicks,
    /// hover, gutter, boxes) routes through this one rule.
    ///
    /// `f64`, deliberately: `f32` holds fractional rows exactly only below
    /// ~2²³ rows — past that, hits land on the wrong line and (via the
    /// widget's px↔row maps) rendered rows visibly skip. `f64` is exact for
    /// every u32-addressable document.
    #[must_use]
    pub fn display_row_at(&self, rows_from_top: f64) -> DisplayRow {
        DisplayRow((rows_from_top.floor().max(0.0) as u32).min(self.display_row_count().saturating_sub(1)))
    }
}

/// A document's rows as they render: the fold projection, the buffer, the
/// inlay hints and the tab width in one view, so no geometry query can leave
/// one of them out. Get it from [`Document::rows`](crate::Document::rows) and
/// keep it for one pass (a frame, an event, an edit): it borrows the
/// document's fold cache.
///
/// Each row's layout is built once per view and shared, so the passes of one
/// frame don't rebuild it.
pub struct Rows<'a> {
    folds: Ref<'a, FoldMap>,
    buffer: &'a Buffer,
    _inlays: &'a DecorationStore,
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
    pub fn position(&self, offset: u32, _edge: Edge) -> Option<DisplayPosition> {
        #[cfg(any(test, debug_assertions))]
        DISPLAY_POSITION_PROBES.with(|c| c.set(c.get() + 1));
        crate::perf::charge(1); // complexity gate: one display-map probe
        let p = self.buffer.offset_to_point(offset);
        let row = BufferRow(p.row);
        if !self.folds.is_folded(row) {
            return Some(DisplayPosition { row: self.folds.to_display_row(row), x: self.layout(row).caret_cell(p.col) });
        }
        // A hidden row renders only a collapsed fold's closing tail, on the
        // header's display line.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    /// A document with the inline pair at `inline_at` and/or the block pair at
    /// `block_at` collapsed (each the byte offset of an opening bracket).
    fn doc_with_folds(text: &str, fold_openers: &[u32]) -> Document {
        let mut doc = Document::new(text).expect("test doc fits");
        for &o in fold_openers {
            assert!(doc.toggle_fold_opener(o), "opener {o} must be foldable");
        }
        doc
    }

    fn fold_map(doc: &Document) -> FoldMap {
        FoldMap::new(doc.folds(), doc.brackets(), doc.buffer())
    }

    // ── RowLayout: the caret boundary rule — open+1 lands on the chip's left
    //    edge (a cell), never its center ──

    #[test]
    fn caret_cell_lands_on_chip_edge_not_center() {
        // `a[bcdef]g` — fold the [..] pair (opener at byte 1).
        let doc = doc_with_folds("a[bcdef]g", &[1]);
        let fm = fold_map(&doc);
        let rl = fm.row_layout(doc.buffer(), BufferRow(0), 4);
        // col 2 == open+1: the LEFT landable edge — a cell, not the center.
        assert_eq!(rl.caret_cell(2), CaretCell::Cell(2), "open+1 is landable at the chip's left edge");
        // col 3 (strict interior) clips to the chip center: cell 2 + 1.5.
        assert_eq!(rl.caret_cell(3), CaretCell::ChipCenter(2.0 + INLINE_CHIP_CELLS as f32 / 2.0));
        // col 7 == close: the RIGHT landable edge.
        assert_eq!(rl.caret_cell(7), CaretCell::Cell(rl.display_cell(7, Edge::Caret)));
        // Glyphs: open+1's glyph hides even though its caret slot is landable.
        assert!(rl.glyph_hidden(2));
        assert!(!rl.glyph_hidden(7), "the closing bracket stays visible");
        assert!(!rl.glyph_hidden(1), "the opening bracket stays visible");
    }

    // ── RowLayout: the inverse round-trips on multi-chip rows — the hit-test
    //    compares in display space, so a later chip resolves correctly ──

    #[test]
    fn hit_round_trips_display_cell_on_multi_chip_rows() {
        // Two inline pairs on one row, a tab up front, a multibyte char after.
        let text = "\tf([aa], [bb]) é";
        let open1 = text.find("[aa").unwrap() as u32;
        let open2 = text.find("[bb").unwrap() as u32;
        let doc = doc_with_folds(text, &[open1, open2]);
        let fm = fold_map(&doc);
        let rl = fm.row_layout(doc.buffer(), BufferRow(0), 4);
        assert_eq!(rl.chips().count(), 2);
        // Every landable char-boundary column round-trips through the inverse.
        let line = doc.buffer().line(0);
        for (i, _) in line.char_indices() {
            let col = i as u32;
            if rl.glyph_hidden(col) {
                continue; // interior columns resolve to the chip instead
            }
            assert_eq!(rl.hit(rl.display_cell(col, Edge::Start) as f32, Bias::Left), col, "round-trip col {col}");
        }
        // Every cell strictly on a chip resolves to just after its `[`. The
        // second chip is the discriminating case: it resolves correctly only
        // because the compare is done in display space, not buffer space.
        for chip in rl.chips().collect::<Vec<_>>() {
            for dc in chip.cell..chip.cell + INLINE_CHIP_CELLS {
                assert_eq!(rl.hit(dc as f32, Bias::Left), chip.open_col + 1, "chip cell {dc}");
            }
        }
    }

    // ── HeaderLayout: an inline fold preceding a block fold shrinks the head,
    //    so the tail is placed against the collapsed width, not the raw one ──

    #[test]
    fn header_layout_shrinks_with_preceding_inline_fold() {
        let text = "\tcall([a, b, c]) {\n\tbody\n\t}\n";
        let inline_open = text.find('[').unwrap() as u32;
        let block_open = text.find('{').unwrap() as u32;
        let doc = doc_with_folds(text, &[inline_open, block_open]);
        let fm = fold_map(&doc);
        let hl = fm.header_layout(doc.buffer(), BufferRow(0), 4).expect("row 0 is a collapsed header");
        let raw = display_map::expand(&doc.buffer().line(0), doc.buffer().line(0).len() as u32, 4);
        // The rendered head shrank: the [..] interior collapsed to a chip.
        assert!(hl.head_cells() < raw, "head {} must be < raw {raw}", hl.head_cells());
        assert_eq!(hl.head_cells(), fm.row_layout(doc.buffer(), BufferRow(0), 4).width());
        assert_eq!(hl.tail_cell(), hl.head_cells() + FOLD_PLACEHOLDER_CELLS);
        // The tail's `}` cell agrees between the glyph list and the col map.
        let g: Vec<TailGlyph> = hl.tail_glyphs().collect();
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].ch, '}');
        assert_eq!(Some(g[0].cell), hl.tail_col_cell(g[0].col));
    }

    #[test]
    fn tail_glyph_cells_match_tail_col_cell_with_tab_in_tail() {
        // A tab INSIDE the visible tail: full-line tab stops, not trimmed-line.
        let text = "f() {\nbody\n}\tx\n";
        let block_open = text.find('{').unwrap() as u32;
        let doc = doc_with_folds(text, &[block_open]);
        let fm = fold_map(&doc);
        let hl = fm.header_layout(doc.buffer(), BufferRow(0), 4).expect("collapsed header");
        for g in hl.tail_glyphs() {
            assert_eq!(Some(g.cell), hl.tail_col_cell(g.col), "glyph at col {} agrees with the col map", g.col);
        }
        // And the hit resolution lands back on each glyph's column.
        for g in hl.tail_glyphs() {
            assert_eq!(hl.hit(g.cell as f32, Bias::Left), HeaderHit::Tail(g.col));
        }
    }

    // ── Rows::position: the one offset→(row, x) owner — an offset below a
    //    fold resolves onto its visible display-space row ──

    #[test]
    fn position_follows_tail_and_hides_gap() {
        let text = "a {\nhidden\n} tail\nafter\n";
        let block_open = text.find('{').unwrap() as u32;
        let doc = doc_with_folds(text, &[block_open]);
        let rows = doc.rows();
        let buffer = doc.buffer();
        // An offset inside the fold's gap is unrepresentable.
        let hidden = buffer.point_to_offset(Point::new(1, 2));
        assert_eq!(rows.position(hidden, Edge::Caret), None);
        // The tail `}` rides the header's display row at the tail cell.
        let tail = buffer.point_to_offset(Point::new(2, 0));
        let p = rows.position(tail, Edge::Caret).expect("tail is visible");
        assert_eq!(p.row, DisplayRow(0));
        let hl = rows.header(BufferRow(0)).unwrap();
        assert_eq!(p.x, CaretCell::Cell(hl.tail_cell()));
        // A row below the fold is shifted up by the hidden count.
        let after = buffer.point_to_offset(Point::new(3, 0));
        let p = rows.position(after, Edge::Caret).expect("visible");
        assert_eq!(p.row, DisplayRow(1), "rows 1..=2 hidden ⇒ row 3 displays at 1");
    }

    #[test]
    fn hit_row_resolves_head_gap_and_tail() {
        let text = "ab {\nhidden\n}\n";
        let block_open = text.find('{').unwrap() as u32;
        let doc = doc_with_folds(text, &[block_open]);
        let fm = fold_map(&doc);
        let buffer = doc.buffer();
        let hl = fm.header_layout(buffer, BufferRow(0), 4).unwrap();
        // Head: cell 0 → offset 0.
        assert_eq!(fm.hit_row(buffer, BufferRow(0), 0.0, Bias::Left, 4), 0);
        // Gap: between head end and tail → clamps to the header line's end.
        let gap_cell = hl.head_cells() as f32 + FOLD_PLACEHOLDER_CELLS as f32 / 2.0;
        assert_eq!(fm.hit_row(buffer, BufferRow(0), gap_cell, Bias::Left, 4), buffer.line_len(0));
        // Tail: the tail cell → the `}` on the last row.
        let tail_off = buffer.point_to_offset(Point::new(2, 0));
        assert_eq!(fm.hit_row(buffer, BufferRow(0), hl.tail_cell() as f32, Bias::Left, 4), tail_off);
    }

    #[test]
    fn display_row_at_floors_and_clamps() {
        let doc = doc_with_folds("a\nb\nc\n", &[]);
        let fm = fold_map(&doc);
        assert_eq!(fm.display_row_at(-2.0), DisplayRow(0));
        assert_eq!(fm.display_row_at(0.9), DisplayRow(0));
        assert_eq!(fm.display_row_at(1.0), DisplayRow(1));
        assert_eq!(fm.display_row_at(99.0), fm.max_display_row());
    }

    // ── plain rows: the projection is the identity over tab expansion ──

    #[test]
    fn plain_row_layout_is_tab_expansion() {
        let doc = doc_with_folds("\tx = 1\n", &[]);
        let fm = fold_map(&doc);
        let rl = fm.row_layout(doc.buffer(), BufferRow(0), 4);
        assert!(rl.is_plain());
        assert_eq!(rl.display_cell(0, Edge::Start), 0);
        assert_eq!(rl.display_cell(1, Edge::Start), 4, "tab expands to the stop");
        assert_eq!(rl.width(), 4 + "x = 1".len() as u32);
        assert_eq!(rl.hit(4.0, Bias::Left), 1);
    }

    // ── Rows: the view every geometry projection goes through ──

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
        assert_eq!(*doc.rows().folds(), *doc.fold_map(), "the view reads the cached fold map");
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

    /// Every projection of a document with tabs, a multibyte char, an inline
    /// chip and a collapsed block, and no inlay hints.
    const GOLDEN: &str = r"position 0 Some((0, Cell(0)))
position 1 Some((0, Cell(4)))
position 2 Some((0, Cell(5)))
position 3 Some((0, Cell(6)))
position 4 Some((0, Cell(7)))
position 5 Some((0, Cell(8)))
position 6 Some((0, Cell(9)))
position 7 Some((0, Cell(10)))
position 8 Some((0, Cell(11)))
position 9 Some((0, Cell(12)))
position 10 Some((0, Cell(13)))
position 11 Some((0, ChipCenter(14.5)))
position 12 Some((0, ChipCenter(14.5)))
position 13 Some((0, ChipCenter(14.5)))
position 14 Some((0, ChipCenter(14.5)))
position 15 Some((0, ChipCenter(14.5)))
position 16 Some((0, ChipCenter(14.5)))
position 17 Some((0, Cell(16)))
position 18 Some((0, Cell(17)))
position 19 Some((0, Cell(18)))
position 20 Some((0, Cell(19)))
position 22 Some((0, Cell(20)))
position 23 Some((1, Cell(0)))
position 24 Some((1, Cell(1)))
position 25 Some((1, Cell(2)))
position 26 Some((1, Cell(3)))
position 27 Some((1, Cell(4)))
position 28 Some((1, Cell(5)))
position 29 Some((1, Cell(6)))
position 30 Some((1, Cell(7)))
position 31 Some((1, Cell(8)))
position 32 None
position 33 None
position 34 None
position 35 None
position 36 None
position 37 None
position 38 Some((1, Cell(12)))
position 39 Some((1, Cell(13)))
position 40 Some((1, Cell(16)))
position 41 Some((1, Cell(17)))
position 42 Some((1, Cell(18)))
position 43 Some((1, Cell(19)))
position 44 Some((1, Cell(20)))
position 45 Some((2, Cell(0)))
position 46 Some((2, Cell(1)))
position 47 Some((3, Cell(0)))
layout BufferRow(0) width 20 plain false
hit BufferRow(0) Left [0, 0, 0, 0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 10, 10, 10, 10, 17, 17, 18, 18, 19, 19, 20, 20, 22, 22, 22, 22, 22, 22]
hit BufferRow(0) Right [0, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 10, 10, 10, 10, 17, 17, 18, 18, 19, 19, 20, 20, 22, 22, 22, 22, 22, 22]
layout BufferRow(1) width 8 plain true
header BufferRow(1) head_cells 8 tail_cell 12 width 20
hit BufferRow(1) Left [23, 24, 24, 25, 25, 26, 26, 27, 27, 28, 28, 29, 29, 30, 30, 31, 31, 31, 31, 31, 31, 31, 31, 38, 38, 39, 39, 39, 39, 39, 39, 40, 40, 41, 41, 42, 42, 43, 43, 44, 44, 44, 44, 44, 44]
hit BufferRow(1) Right [23, 24, 24, 25, 25, 26, 26, 27, 27, 28, 28, 29, 29, 30, 30, 31, 31, 31, 31, 31, 31, 31, 31, 38, 38, 39, 39, 40, 40, 40, 40, 40, 40, 41, 41, 42, 42, 43, 43, 44, 44, 44, 44, 44, 44]
layout BufferRow(4) width 1 plain true
hit BufferRow(4) Left [45, 46, 46, 46, 46, 46, 46]
hit BufferRow(4) Right [45, 46, 46, 46, 46, 46, 46]
layout BufferRow(5) width 0 plain true
hit BufferRow(5) Left [47, 47, 47, 47, 47]
hit BufferRow(5) Right [47, 47, 47, 47, 47]
";

    /// With no hints, every projection matches [`GOLDEN`] and the three edges
    /// agree at every offset.
    #[test]
    fn hint_free_projections_match_the_pre_hint_golden() {
        let text = "\tlet x = [1, 2, 3]; é\nfn f() {\n\tbody\n}\ttail\nz\n";
        let inline_open = text.find('[').unwrap() as u32;
        let block_open = text.find('{').unwrap() as u32;
        let doc = doc_with_folds(text, &[inline_open, block_open]);
        let rows = doc.rows();
        let mut out = String::new();
        for (i, _) in text.char_indices().chain([(text.len(), ' ')]) {
            let offset = i as u32;
            let edges = [Edge::Start, Edge::End, Edge::Caret].map(|e| rows.position(offset, e));
            assert!(edges.iter().all(|p| *p == edges[0]), "offset {offset}: every edge agrees without hints");
            out.push_str(&format!("position {offset} {:?}\n", edges[0].map(|p| (p.row.0, p.x))));
        }
        for d in 0..rows.folds().display_row_count() {
            let row = rows.folds().to_buffer_row(DisplayRow(d));
            let layout = rows.layout(row);
            out.push_str(&format!("layout {row:?} width {} plain {}\n", layout.width(), layout.is_plain()));
            let mut reach = layout.width();
            if let Some(header) = rows.header(row) {
                out.push_str(&format!(
                    "header {row:?} head_cells {} tail_cell {} width {}\n",
                    header.head_cells(),
                    header.tail_cell(),
                    header.width()
                ));
                reach = reach.max(header.width());
            }
            for bias in [Bias::Left, Bias::Right] {
                let hits: Vec<u32> = (0..=2 * (reach + 2)).map(|half| rows.hit(row, half as f32 / 2.0, bias)).collect();
                out.push_str(&format!("hit {row:?} {bias:?} {hits:?}\n"));
            }
        }
        if out != GOLDEN {
            println!("{out}");
        }
        assert_eq!(out, GOLDEN, "hint-free projections changed");
    }
}
