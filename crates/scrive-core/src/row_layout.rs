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
//! Inlay hints are part of the row: they take cells but no bytes, so the text
//! after one shifts right and an offset with hints spans several cells (see
//! [`Edge`]).
//!
//! Everything here is in **cells and columns** — GUI-free. The widget's only
//! remaining job is `x = origin + cell × advance` (and its inverse).

use std::borrow::Cow;
use std::cell::{Ref, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use crate::buffer::Buffer;
use crate::coords::{Bias, Point};
use crate::decorations::{DecorationKind, DecorationStore};
use crate::display_map::{self, BufferRow, DisplayRow};
use crate::fold_map::{FoldMap, InlineFold};
use crate::intel::inlay;

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

/// One inlay hint laid out on a row.
#[derive(Copy, Clone, Debug)]
struct HintSpan<'a> {
    /// Byte column of the hint's render offset.
    col: u32,
    /// Tab-expanded, pre-collapse cell of `col`.
    raw_cell: u32,
    anchor: &'a inlay::Anchor,
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

/// A visible buffer row's horizontal projection: byte column ↔ display cell,
/// with tab expansion *and* the horizontal collapse of every root inline fold
/// on the row, and the row's inlay hints. Built by [`Rows::layout`], which
/// shares it for the view's lifetime; holds only the row's text (a [`Cow`] —
/// borrowed straight off the backing when the row is stored contiguously,
/// owned only when it spans a chunk boundary), copies of the row's folds and
/// the row's hints borrowed from the document, so it carries no derived state
/// that could drift out of sync with the document.
pub struct RowLayout<'a> {
    line: Cow<'a, str>,
    /// Byte offset of the row's first character (column 0), for offset-space
    /// boundary predicates.
    row_start: u32,
    tab: u32,
    /// This row's root inline folds, sorted by opening cell.
    spans: Vec<InlineSpan>,
    /// This row's laid-out inlay hints, in render order.
    hints: Vec<HintSpan<'a>>,
}

impl<'a> RowLayout<'a> {
    /// The layout of `row`, with the hints `inlays` holds for it; `None` lays
    /// the row out without hints.
    fn new(fold_map: &FoldMap, buffer: &'a Buffer, inlays: Option<&'a DecorationStore>, row: BufferRow, tab: u32) -> Self {
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
        let hints = match inlays {
            Some(store) if !fold_map.is_folded(row) => hints_on_row(store, &line, row_start, &spans, tab),
            _ => Vec::new(),
        };
        let mut layout = Self { line, row_start, tab, spans, hints };
        let mut prior = 0;
        for i in 0..layout.hints.len() {
            let cell = layout.cell_of(layout.hints[i].raw_cell) + prior;
            layout.hints[i].inlay.cell = cell;
            prior += layout.hints[i].width();
        }
        layout
    }

    /// Whether the row has no collapsed inline folds and no inlay hints (the
    /// identity projection).
    #[must_use]
    pub fn is_plain(&self) -> bool {
        self.spans.is_empty() && self.hints.is_empty()
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

    /// Total width of the hints left of byte column `col` under `edge`.
    fn hint_cells(&self, col: u32, edge: Edge) -> u32 {
        self.hints
            .iter()
            .take_while(|h| h.col <= col)
            .filter(|h| h.col < col || edge.passes(h.side()))
            .map(HintSpan::width)
            .sum()
    }

    /// Display cell of the chip for inline span `s`, one past its opening
    /// bracket.
    fn chip_cell(&self, s: &InlineSpan) -> u32 {
        self.cell_of(s.open_cell + 1) + self.hint_cells(s.fold.left_edge() - self.row_start, Edge::End)
    }

    /// Byte column → display cell (tab-expanded, inline-collapsed, inlay
    /// hints counted), on the `edge` side of any hints at `col`. Total and
    /// monotone for each edge, and `End <= Caret <= Start` at every column; a
    /// column hidden inside a chip maps into the chip's span (use
    /// [`Self::caret_cell`] for caret placement, which clips to the center).
    #[must_use]
    pub fn display_cell(&self, col: u32, edge: Edge) -> u32 {
        self.cell_of(display_map::expand(&self.line, col, self.tab)) + self.hint_cells(col, edge)
    }

    /// Where a byte column renders on the `edge` side of any hints there: its
    /// display cell, or the chip center when the column is hidden inside a
    /// collapsed inline fold's gap.
    #[must_use]
    pub fn edge_cell(&self, col: u32, edge: Edge) -> CaretCell {
        let off = self.row_start + col;
        match self.spans.iter().find(|s| s.fold.hides_caret_at(off)) {
            Some(s) => CaretCell::ChipCenter(self.chip_cell(s) as f32 + INLINE_CHIP_CELLS as f32 / 2.0),
            None => CaretCell::Cell(self.display_cell(col, edge)),
        }
    }

    /// Caret placement for a byte column: [`Self::edge_cell`] on the
    /// [`Edge::Caret`] side.
    #[must_use]
    pub fn caret_cell(&self, col: u32) -> CaretCell {
        self.edge_cell(col, Edge::Caret)
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
    /// sites. A cell on an inlay hint, label or padding, resolves to the
    /// hint's offset; a cell on a chip to just after the opening bracket;
    /// past-EOL clamps; mid-tab snaps by `bias`.
    #[must_use]
    pub fn hit(&self, cell: f32, bias: Bias) -> u32 {
        let dc = cell.round().max(0.0) as u32;
        let mut passed = 0;
        for group in self.hints.chunk_by(|a, b| a.col == b.col) {
            let left = self.cell_of(group[0].raw_cell) + passed;
            if dc < left {
                break;
            }
            let width: u32 = group.iter().map(HintSpan::width).sum();
            if dc <= left + width {
                return group[0].col;
            }
            passed += width;
        }
        self.hit_unhinted(dc - passed, bias)
    }

    /// [`Self::hit`] on the row without its hints: `dc` is a whole display
    /// cell with every hint left of it taken out.
    fn hit_unhinted(&self, dc: u32, bias: Bias) -> u32 {
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

    /// The row's rendered display width in cells (tab-expanded, collapsed),
    /// including the inlay hints at the line end. A collapsed block's inline tail begins [`FOLD_PLACEHOLDER_CELLS`] past
    /// this — see [`HeaderLayout::tail_cell`].
    #[must_use]
    pub fn width(&self) -> u32 {
        self.display_cell(self.line.len() as u32, Edge::Start)
    }

    /// The row's collapsed chips, in display order.
    pub fn chips(&self) -> impl Iterator<Item = Chip> + '_ {
        self.spans.iter().map(|s| {
            let cell = self.chip_cell(s);
            Chip {
                cell,
                center: cell as f32 + INLINE_CHIP_CELLS as f32 / 2.0,
                open_col: s.fold.open - self.row_start,
                close_col: s.fold.close - self.row_start,
            }
        })
    }

    /// This row's laid-out inlay hints, in render order.
    pub fn inlays(&self) -> impl Iterator<Item = &Inlay<'a>> + '_ {
        self.hints.iter().map(|h| &h.inlay)
    }

    /// What the inlay hint under fractional display cell `cell` holds there:
    /// a label part, or its padding. `None` off every hint.
    #[must_use]
    pub fn inlay_at(&self, cell: f32) -> Option<inlay::At> {
        if cell < 0.0 {
            return None;
        }
        let k = cell.floor() as u32;
        let h = self.hints.iter().map(|h| h.inlay).find(|h| k < h.cell + h.width)?;
        if k < h.cell {
            return None;
        }
        let mut at = h.cell + u32::from(h.padding.left);
        for (part, p) in h.hint.parts().iter().enumerate() {
            let n = p.text().chars().count() as u32;
            if (at..at + n).contains(&k) {
                let insert = if h.hint.insertable() { inlay::Insert::Available } else { inlay::Insert::Unavailable };
                return Some(inlay::At::Label {
                    key: h.key,
                    part: part as u32,
                    offset: h.offset,
                    link: p.link(),
                    insert,
                    cells: at..at + n,
                });
            }
            at += n;
        }
        Some(inlay::At::Padding { key: h.key, offset: h.offset })
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
    /// opener shrinks it, and inlay hints on the header widen it.
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
        RowLayout::new(self, buffer, None, row, tab)
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
    inlays: &'a DecorationStore,
    tab: u32,
    built: RefCell<HashMap<BufferRow, Rc<RowLayout<'a>>>>,
}

impl<'a> Rows<'a> {
    pub(crate) fn new(folds: Ref<'a, FoldMap>, buffer: &'a Buffer, inlays: &'a DecorationStore, tab: u32) -> Self {
        Self { folds, buffer, inlays, tab, built: RefCell::new(HashMap::new()) }
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
        let layout = Rc::new(RowLayout::new(&self.folds, self.buffer, Some(self.inlays), row, self.tab));
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
    pub fn position(&self, offset: u32, edge: Edge) -> Option<DisplayPosition> {
        #[cfg(any(test, debug_assertions))]
        DISPLAY_POSITION_PROBES.with(|c| c.set(c.get() + 1));
        crate::perf::charge(1); // complexity gate: one display-map probe
        let p = self.buffer.offset_to_point(offset);
        let row = BufferRow(p.row);
        if !self.folds.is_folded(row) {
            return Some(DisplayPosition { row: self.folds.to_display_row(row), x: self.layout(row).edge_cell(p.col, edge) });
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
        if let Some(header) = self.header(row) {
            match header.hit(cell, bias) {
                HeaderHit::Tail(col) => return self.buffer.point_to_offset(Point::new(header.last_row().0, col)),
                HeaderHit::Gap => return self.buffer.point_to_offset(Point::new(row.0, self.buffer.line_len(row.0))),
                HeaderHit::Head => {}
            }
        }
        self.buffer.point_to_offset(Point::new(row.0, self.layout(row).hit(cell, bias)))
    }

    /// What the inlay hint under `cell` on visible `row` holds there; `None`
    /// over text, or over a collapsed header's gap and tail.
    #[must_use]
    pub fn inlay_at(&self, row: BufferRow, cell: f32) -> Option<inlay::At> {
        if let Some(header) = self.header(row) {
            if header.hit(cell, Bias::Left) != HeaderHit::Head {
                return None;
            }
        }
        self.layout(row).inlay_at(cell)
    }
}

impl Edge {
    /// Whether a hint of `side` at the projected offset lies left of this edge.
    fn passes(self, side: inlay::Side) -> bool {
        match (self, side) {
            (Self::Start, inlay::Side::Prefix | inlay::Side::Suffix) | (Self::Caret, inlay::Side::Prefix) => true,
            (Self::End, inlay::Side::Prefix | inlay::Side::Suffix) | (Self::Caret, inlay::Side::Suffix) => false,
        }
    }
}

/// The hints that render on the row at `row_start` with text `line`, outside
/// its collapsed inline folds, in `(column, Prefix before Suffix, server
/// index)` order.
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
        let col = offset - row_start;
        debug_assert!(line.is_char_boundary(col as usize), "a hint renders on a char boundary");
        if spans.iter().any(|s| s.fold.open < offset && offset <= s.fold.close) {
            return;
        }
        let hint = anchor.hint();
        let inlay = Inlay { key: hint.key(), offset, cell: 0, width: hint.width(), padding: hint.padded(), hint };
        out.push(HintSpan { col, raw_cell: display_map::expand(line, col, tab), anchor, inlay });
    });
    out.sort_by_key(|h| (h.col, side_rank(h.side()), h.anchor.index()));
    out
}

/// Render order of the sides at one offset: prefixes, then suffixes.
fn side_rank(side: inlay::Side) -> u8 {
    match side {
        inlay::Side::Prefix => 0,
        inlay::Side::Suffix => 1,
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
    fn rows_hit_resolves_head_gap_and_tail() {
        let text = "ab {\nhidden\n}\n";
        let block_open = text.find('{').unwrap() as u32;
        let doc = doc_with_folds(text, &[block_open]);
        let rows = doc.rows();
        let buffer = doc.buffer();
        let hl = rows.header(BufferRow(0)).unwrap();
        // Head: cell 0 → offset 0.
        assert_eq!(rows.hit(BufferRow(0), 0.0, Bias::Left), 0);
        // Gap: between head end and tail → clamps to the header line's end.
        let gap_cell = hl.head_cells() as f32 + FOLD_PLACEHOLDER_CELLS as f32 / 2.0;
        assert_eq!(rows.hit(BufferRow(0), gap_cell, Bias::Left), buffer.line_len(0));
        // Tail: the tail cell → the `}` on the last row.
        let tail_off = buffer.point_to_offset(Point::new(2, 0));
        assert_eq!(rows.hit(BufferRow(0), hl.tail_cell() as f32, Bias::Left), tail_off);
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

    // ── Inlay hints ──

    fn hint(kind: inlay::Kind, parts: &[(&str, inlay::Link)], key: u64) -> inlay::Hint {
        let parts = parts.iter().map(|&(text, link)| inlay::Part::new(text, link)).collect();
        inlay::Hint::new(kind, parts, inlay::Key::new(key)).expect("a visible label")
    }

    fn plain(kind: inlay::Kind, label: &str, key: u64) -> inlay::Hint {
        hint(kind, &[(label, inlay::Link::None)], key)
    }

    fn pad(left: bool, right: bool) -> inlay::Padding {
        inlay::Padding { left, right }
    }

    /// Install `hints` at the current revision.
    fn install(doc: &mut Document, hints: Vec<(u32, inlay::Hint)>) {
        let count = hints.len();
        let placed = hints.into_iter().map(|(offset, hint)| inlay::Placed::new(offset, hint)).collect();
        let outcome = doc.set_inlays(doc.revision(), placed);
        assert_eq!(outcome, inlay::Outcome::Applied { count }, "a current set installs");
    }

    /// `let ab: i32 = f(n: cd); end` over two rows; the second has no hints.
    fn main_doc() -> Document {
        let mut doc = doc_with_folds("let ab = f(cd);\nlet abcdefghijklmnopqrstuvwxyz0123\n", &[]);
        install(
            &mut doc,
            vec![
                (6, hint(inlay::Kind::Type, &[(": ", inlay::Link::None), ("i32", inlay::Link::Jumps)], 1)),
                (11, plain(inlay::Kind::Parameter, "n:", 2).padding(pad(false, true))),
                (15, plain(inlay::Kind::Other, "end", 3).padding(pad(true, false))),
            ],
        );
        doc
    }

    /// `g(x)(a)` with a suffix `ss` and a prefix `ppp` both at offset 4, moved
    /// there by deleting the space between their fetch offsets.
    fn mixed_doc() -> Document {
        let mut doc = doc_with_folds("g(x) (a)", &[]);
        install(&mut doc, vec![(4, plain(inlay::Kind::Type, "ss", 1)), (5, plain(inlay::Kind::Parameter, "ppp", 2))]);
        doc.edit(vec![crate::EditOp::new(4..5, "")]).expect("delete the space");
        doc
    }

    /// `x[abcd]y` with the pair collapsed: hints at both brackets' outer sides
    /// show, the two inside the pair don't.
    fn chip_doc() -> Document {
        let mut doc = doc_with_folds("x[abcd]y", &[1]);
        install(
            &mut doc,
            vec![
                (1, plain(inlay::Kind::Type, "aa", 1)),
                (7, plain(inlay::Kind::Parameter, "bb", 2)),
                (2, plain(inlay::Kind::Parameter, "cc", 3)),
                (6, plain(inlay::Kind::Type, "dd", 4)),
            ],
        );
        doc
    }

    /// `fn f(a) {` collapsed over `    body` and `}`, with a hint on each row.
    fn header_doc() -> Document {
        let mut doc = doc_with_folds("fn f(a) {\n    body\n}\n", &[8]);
        install(
            &mut doc,
            vec![
                (5, plain(inlay::Kind::Parameter, "x:", 1).padding(pad(false, true))),
                (18, plain(inlay::Kind::Type, ": T", 2)),
                (20, plain(inlay::Kind::Other, "fn f", 3).padding(pad(true, false))),
            ],
        );
        doc
    }

    fn keys(layout: &RowLayout<'_>) -> Vec<inlay::Key> {
        layout.inlays().map(|i| i.key).collect()
    }

    fn key_list(raw: &[u64]) -> Vec<inlay::Key> {
        raw.iter().copied().map(inlay::Key::new).collect()
    }

    /// Byte columns a caret can land on: char boundaries outside a chip's gap.
    fn landable(layout: &RowLayout<'_>, line: &str) -> Vec<u32> {
        line.char_indices()
            .map(|(i, _)| i as u32)
            .chain([line.len() as u32])
            .filter(|&col| matches!(layout.caret_cell(col), CaretCell::Cell(_)))
            .collect()
    }

    const EDGES: [Edge; 3] = [Edge::Start, Edge::End, Edge::Caret];

    /// At one column prefixes render before suffixes whatever the server
    /// order; otherwise hints render by column, then in server order.
    #[test]
    fn hints_sort_by_column_then_prefix_then_server_index() {
        let doc = mixed_doc();
        assert_eq!(keys(&doc.rows().layout(BufferRow(0))), key_list(&[2, 1]), "the prefix P renders before the suffix S");
        let doc = main_doc();
        assert_eq!(keys(&doc.rows().layout(BufferRow(0))), key_list(&[1, 2, 3]), "by column");
    }

    /// Each edge counts the hints at a column it passes; every edge is
    /// monotone and `End <= Caret <= Start` everywhere.
    #[test]
    fn each_edge_places_the_hints_at_an_offset() {
        let doc = main_doc();
        let rows = doc.rows();
        let layout = rows.layout(BufferRow(0));
        let mut expected: Vec<(u32, u32, u32)> = (0..=5).map(|c| (c, c, c)).collect();
        expected.extend([(6, 6, 11), (12, 12, 12), (13, 13, 13), (14, 14, 14), (15, 15, 15)]);
        expected.extend([(16, 19, 19), (20, 20, 20), (21, 21, 21), (22, 22, 22), (23, 23, 27)]);
        for (col, &(end, caret, start)) in expected.iter().enumerate() {
            let col = col as u32;
            let got = (layout.display_cell(col, Edge::End), layout.display_cell(col, Edge::Caret), layout.display_cell(col, Edge::Start));
            assert_eq!(got, (end, caret, start), "col {col}: End, Caret, Start");
            assert!(got.0 <= got.1 && got.1 <= got.2, "col {col}: End <= Caret <= Start");
        }
        for edge in EDGES {
            let cells: Vec<u32> = (0..=15).map(|col| layout.display_cell(col, edge)).collect();
            assert!(cells.windows(2).all(|w| w[0] <= w[1]), "{edge:?} is monotone: {cells:?}");
        }
        assert_eq!(layout.caret_cell(6), CaretCell::Cell(6), "a suffix renders after the caret");
        assert_eq!(layout.caret_cell(11), CaretCell::Cell(19), "a prefix renders before the caret");
        assert_eq!(layout.caret_cell(15), CaretCell::Cell(23), "an end-of-line suffix renders after the caret");
        for (offset, edge, cell) in [(6, Edge::Start, 11), (11, Edge::End, 16), (15, Edge::Caret, 23)] {
            let p = rows.position(offset, edge).expect("visible");
            assert_eq!((p.row, p.x), (DisplayRow(0), CaretCell::Cell(cell)), "position({offset}, {edge:?})");
        }
    }

    /// With both groups at one offset the caret sits between them, where the
    /// next typed char lands; one-sided groups render in server order.
    #[test]
    fn a_shared_offset_puts_the_caret_between_the_prefix_and_suffix_groups() {
        let mut doc = mixed_doc();
        {
            let layout = doc.rows().layout(BufferRow(0));
            let cells = EDGES.map(|e| layout.display_cell(4, e));
            assert_eq!(cells, [9, 4, 7], "Start, End, Caret at the shared offset");
            assert_eq!(layout.display_cell(3, Edge::Start), 3);
            assert_eq!(layout.display_cell(5, Edge::End), 10);
            assert_eq!(layout.width(), 12);
        }
        doc.set_selections(crate::SelectionSet::new(4));
        doc.type_char('X');
        let rows = doc.rows();
        let layout = rows.layout(BufferRow(0));
        assert_eq!(layout.display_cell(4, Edge::Start), 7, "X renders at the old caret cell");
        let at: Vec<(inlay::Key, u32)> = layout.inlays().map(|i| (i.key, i.offset)).collect();
        assert_eq!(at, vec![(inlay::Key::new(2), 4), (inlay::Key::new(1), 5)], "P stays left of X, S moves right of it");

        let mut doc = doc_with_folds("let x = 1;", &[]);
        install(&mut doc, vec![(5, plain(inlay::Kind::Type, "a", 1)), (5, plain(inlay::Kind::Type, "bb", 2))]);
        let layout = doc.rows().layout(BufferRow(0));
        assert_eq!(keys(&layout), key_list(&[1, 2]), "server order");
        assert_eq!(layout.inlays().map(|i| i.cell).collect::<Vec<_>>(), vec![5, 6]);
        assert_eq!(layout.display_cell(5, Edge::Caret), layout.display_cell(5, Edge::End), "the caret precedes a suffix group");
        assert_eq!(layout.display_cell(5, Edge::Start), 8);
    }

    /// An empty range projects both ends with `Caret`: `Start..End` would be
    /// inverted over the hints at its offset.
    #[test]
    fn an_empty_range_uses_the_caret_edge_for_both_ends() {
        let doc = mixed_doc();
        let rows = doc.rows();
        let cell = |edge| rows.position(4, edge).expect("visible").x.cells();
        assert_eq!((cell(Edge::Caret), cell(Edge::Caret)), (7.0, 7.0), "an empty range at the caret");
        assert!(cell(Edge::Start) > cell(Edge::End), "Start..End is inverted");
    }

    /// A cell on a hint, label or padding, rounds onto the hint's offset;
    /// cells past it subtract the hint's width.
    #[test]
    fn a_hit_on_a_label_or_its_padding_lands_on_the_hint_offset() {
        let doc = main_doc();
        let rows = doc.rows();
        let layout = rows.layout(BufferRow(0));
        let table = [
            (5.0, 5),
            (6.0, 6),
            (8.4, 6),
            (10.6, 6),
            (11.6, 7),
            (15.0, 10),
            (16.0, 11),
            (18.0, 11),
            (18.5, 11),
            (20.0, 12),
            (23.0, 15),
            (26.4, 15),
            (40.0, 15),
        ];
        for (cell, col) in table {
            assert_eq!(layout.hit(cell, Bias::Left), col, "cell {cell}");
            assert_eq!(rows.hit(BufferRow(0), cell, Bias::Left), col, "cell {cell} through Rows");
        }
    }

    /// `hit` inverts `display_cell` and `position` at every landable column
    /// and every edge, around hints, shared offsets and chips.
    #[test]
    fn hit_round_trips_every_edge() {
        for doc in [main_doc(), mixed_doc(), chip_doc()] {
            let rows = doc.rows();
            let line = doc.buffer().line(0);
            let layout = rows.layout(BufferRow(0));
            for col in landable(&layout, &line) {
                for edge in EDGES {
                    let cell = layout.display_cell(col, edge);
                    assert_eq!(layout.hit(cell as f32, Bias::Left), col, "{line:?} col {col} {edge:?}");
                    let p = rows.position(col, edge).expect("visible");
                    assert_eq!(rows.hit(BufferRow(0), p.x.cells(), Bias::Left), col, "{line:?} position {col} {edge:?}");
                }
            }
        }
    }

    /// `inlay_at` floors the cell and names the label part under it, or
    /// reports padding; text and the space past the row are `None`.
    #[test]
    fn inlay_at_names_the_part_and_reports_padding() {
        let doc = main_doc();
        let rows = doc.rows();
        let label = |key, part, offset, link, cells| {
            Some(inlay::At::Label {
                key: inlay::Key::new(key),
                part,
                offset,
                link,
                insert: inlay::Insert::Unavailable,
                cells,
            })
        };
        let padding = |key, offset| Some(inlay::At::Padding { key: inlay::Key::new(key), offset });
        let table = [
            (-1.0, None),
            (5.9, None),
            (6.0, label(1, 0, 6, inlay::Link::None, 6..8)),
            (7.9, label(1, 0, 6, inlay::Link::None, 6..8)),
            (8.0, label(1, 1, 6, inlay::Link::Jumps, 8..11)),
            (10.5, label(1, 1, 6, inlay::Link::Jumps, 8..11)),
            (11.0, None),
            (16.2, label(2, 0, 11, inlay::Link::None, 16..18)),
            (17.0, label(2, 0, 11, inlay::Link::None, 16..18)),
            (18.0, padding(2, 11)),
            (23.5, padding(3, 15)),
            (24.0, label(3, 0, 15, inlay::Link::None, 24..27)),
            (26.9, label(3, 0, 15, inlay::Link::None, 24..27)),
            (27.0, None),
        ];
        for (cell, at) in table {
            assert_eq!(rows.inlay_at(BufferRow(0), cell), at, "cell {cell}");
        }
        let mut doc = doc_with_folds("let x = 1;", &[]);
        install(&mut doc, vec![(5, plain(inlay::Kind::Type, ": i32", 1).insert(inlay::Insert::Available))]);
        assert!(
            matches!(doc.rows().inlay_at(BufferRow(0), 5.0), Some(inlay::At::Label { insert: inlay::Insert::Available, .. })),
            "an insertable hint says so"
        );
    }

    /// The painting view yields each laid-out hint with its first cell,
    /// width, padding and label.
    #[test]
    fn inlays_yield_each_hint_with_its_first_cell() {
        let doc = main_doc();
        let rows = doc.rows();
        let layout = rows.layout(BufferRow(0));
        let got: Vec<(u32, u32, u32)> = layout.inlays().map(|i| (i.offset, i.cell, i.width)).collect();
        assert_eq!(got, vec![(6, 6, 5), (11, 16, 3), (15, 23, 4)], "(offset, cell, width)");
        assert_eq!(keys(&layout), key_list(&[1, 2, 3]));
        let paddings: Vec<inlay::Padding> = layout.inlays().map(|i| i.padding).collect();
        assert_eq!(paddings, vec![pad(false, false), pad(false, true), pad(true, false)]);
        let texts: Vec<Vec<&str>> = layout.inlays().map(|i| i.hint.parts().iter().map(inlay::Part::text).collect()).collect();
        assert_eq!(texts, vec![vec![": ", "i32"], vec!["n:"], vec!["end"]], "the installed parts");
        assert_eq!(rows.layout(BufferRow(1)).inlays().count(), 0, "row 1 has none");

        let doc = chip_doc();
        let layout = doc.rows().layout(BufferRow(0));
        let got: Vec<(u32, u32)> = layout.inlays().map(|i| (i.offset, i.cell)).collect();
        assert_eq!(got, vec![(1, 1), (7, 8)], "only the hints outside the pair");
    }

    /// The row's width reaches past its end-of-line hints, and a row with
    /// hints is not plain.
    #[test]
    fn width_includes_end_of_line_hints() {
        let doc = main_doc();
        let rows = doc.rows();
        assert_eq!(rows.layout(BufferRow(0)).width(), 27);
        assert!(!rows.layout(BufferRow(0)).is_plain(), "a hinted row is not plain");
        assert!(rows.layout(BufferRow(1)).is_plain());
    }

    /// Tab stops are measured on the buffer line, so a hint before a tab
    /// shifts it without widening it.
    #[test]
    fn tabs_after_a_hint_keep_their_buffer_space_width() {
        let mut doc = doc_with_folds("a\tb", &[]);
        install(&mut doc, vec![(1, plain(inlay::Kind::Type, "tt", 1))]);
        let layout = doc.rows().layout(BufferRow(0));
        assert_eq!(layout.display_cell(1, Edge::Start), 3, "the tab starts after the hint");
        assert_eq!(layout.display_cell(2, Edge::Start), 6, "and keeps its three cells");
    }

    /// Hints inside a collapsed pair are not laid out; those at its outer
    /// sides shift the chip and the text after it.
    #[test]
    fn hints_inside_a_collapsed_inline_fold_are_hidden_and_edges_shift() {
        let mut doc = chip_doc();
        {
            let rows = doc.rows();
            let layout = rows.layout(BufferRow(0));
            assert_eq!(keys(&layout), key_list(&[1, 2]), "C and D are inside the pair");
            let table = [(0, [0, 0, 0]), (1, [3, 1, 1]), (2, [4, 4, 4]), (6, [7, 7, 7]), (7, [10, 8, 10]), (8, [11, 11, 11])];
            for (col, cells) in table {
                assert_eq!(EDGES.map(|e| layout.display_cell(col, e)), cells, "col {col}: Start, End, Caret");
            }
            assert_eq!(layout.caret_cell(3), CaretCell::ChipCenter(5.5), "a hidden column clips to the chip");
            let chips: Vec<Chip> = layout.chips().collect();
            assert_eq!(chips, vec![Chip { cell: 4, center: 5.5, open_col: 1, close_col: 6 }]);
            assert_eq!(layout.width(), 11);
            let hits = [(0, 0), (1, 1), (2, 1), (3, 1), (4, 2), (5, 2), (6, 2), (7, 6), (8, 7), (9, 7), (10, 7), (11, 8)];
            for (cell, col) in hits {
                assert_eq!(layout.hit(cell as f32, Bias::Left), col, "cell {cell}");
            }
            let found: Vec<inlay::Key> = (0..12)
                .filter_map(|c| match layout.inlay_at(c as f32) {
                    Some(inlay::At::Label { key, .. } | inlay::At::Padding { key, .. }) => Some(key),
                    None => None,
                })
                .collect();
            assert_eq!(found, [1, 1, 2, 2].map(inlay::Key::new), "only A and B are under any cell");
        }
        assert!(doc.toggle_fold_opener(1), "unfold");
        assert_eq!(keys(&doc.rows().layout(BufferRow(0))), key_list(&[1, 3, 4, 2]), "unfolded, all four lay out");
    }

    /// A collapsed block's hidden rows and its tail row show no hints.
    #[test]
    fn hints_on_block_folded_rows_are_not_shown() {
        let doc = header_doc();
        let rows = doc.rows();
        assert!(rows.layout(BufferRow(1)).is_plain(), "the hidden body row");
        assert!(rows.layout(BufferRow(2)).is_plain(), "the tail row");
        for edge in EDGES {
            let p = rows.position(19, edge).expect("the tail renders");
            assert_eq!((p.row, p.x), (DisplayRow(0), CaretCell::Cell(16)), "{edge:?}");
        }
        assert_eq!(rows.inlay_at(BufferRow(0), 17.0), None, "the tail row's hint is not shown");
    }

    /// A hint on a collapsed header widens the head, so the gap and the tail
    /// move right by its width.
    #[test]
    fn a_header_hint_shifts_the_gap_and_the_tail() {
        let doc = header_doc();
        let rows = doc.rows();
        let header = rows.header(BufferRow(0)).expect("row 0 is a collapsed header");
        assert_eq!(header.head_cells(), 12);
        assert_eq!(header.gap_center(), 14.0);
        assert_eq!(header.tail_cell(), 16);
        assert_eq!(header.width(), 17);
        assert_eq!(rows.hit(BufferRow(0), 6.0, Bias::Left), 5, "on the hint");
        assert_eq!(rows.hit(BufferRow(0), 12.6, Bias::Left), 9, "the gap is the header's line end");
        assert_eq!(rows.hit(BufferRow(0), 16.0, Bias::Left), 19, "the tail");
        let label = Some(inlay::At::Label {
            key: inlay::Key::new(1),
            part: 0,
            offset: 5,
            link: inlay::Link::None,
            insert: inlay::Insert::Unavailable,
            cells: 5..7,
        });
        assert_eq!(rows.inlay_at(BufferRow(0), 5.0), label);
        assert_eq!(rows.inlay_at(BufferRow(0), 7.0), Some(inlay::At::Padding { key: inlay::Key::new(1), offset: 5 }));
    }

    /// Text typed between two hints at a shared offset leaves them between
    /// two word characters, and they still lay out there.
    #[test]
    fn a_hint_between_two_word_characters_is_laid_out() {
        let text = "fn f() {} fn main() { let c: fn() -> fn() = ||f; }";
        let at = text.find("||f").expect("the closure") as u32 + 2;
        let mut doc = doc_with_folds(text, &[]);
        install(&mut doc, vec![(at, plain(inlay::Kind::Type, " -> fn()", 1)), (at, plain(inlay::Kind::Other, "<fn-item-to-fn-pointer>", 2))]);
        doc.set_selections(crate::SelectionSet::new(at));
        doc.type_char('X');
        let rows = doc.rows();
        let layout = rows.layout(BufferRow(0));
        let got: Vec<(u32, u32)> = layout.inlays().map(|i| (i.offset, i.width)).collect();
        assert_eq!(got, vec![(at + 1, 8), (at + 1, 23)], "both after X, in server order");
        assert_eq!(layout.display_cell(at + 1, Edge::Start), at + 1 + 8 + 23, "f renders past both");
    }

    /// One `Rows` queries the inlay store once per row, whatever asks.
    #[test]
    fn rows_build_each_hinted_row_once() {
        use crate::decorations::DECORATION_VISITS;
        let doc = main_doc();
        let rows = doc.rows();
        let visits = || DECORATION_VISITS.with(std::cell::Cell::get);
        let before = visits();
        let _ = rows.layout(BufferRow(0));
        let v = visits() - before;
        assert!(v > 0, "the first build visits the row's hints");
        let _ = rows.layout(BufferRow(0));
        let _ = rows.position(6, Edge::Start);
        let _ = rows.position(15, Edge::Caret);
        let _ = rows.hit(BufferRow(0), 8.0, Bias::Left);
        let _ = rows.inlay_at(BufferRow(0), 7.0);
        let _ = rows.header(BufferRow(0));
        assert_eq!(visits() - before, v, "every later query reuses the build");
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
