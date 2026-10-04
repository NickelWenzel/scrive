//! Turning highlights-query captures into per-row [`HighlightSpan`]s.
//!
//! Precedence (D5): a deeper node overrides the nodes enclosing it, which for
//! two nodes with the same range means the child wins. On one node the
//! earliest pattern wins, as in tree-sitter-highlight, whose semantics grammar
//! crates write their highlights queries for. Unlike tree-sitter-highlight, a
//! capture the theme doesn't style doesn't claim its node, so a later pattern
//! (or the enclosing node) still colors it: a partial theme colors as much as
//! it can.

use core::cmp::Reverse;
use core::ops::Range;

use tree_sitter::{Node, Query, QueryCursor, StreamingIterator, TextProvider};

use super::{HighlightSpan, SpanStyle};

/// One capture of one node, in document bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Capture {
    pub(crate) range: Range<u32>,
    /// Identifies the node, so captures of one node compete by pattern.
    pub(crate) node: usize,
    /// Depth among the captured nodes sharing exactly this range (0 for the
    /// outermost). Ranges alone order every other pair of nodes.
    pub(crate) rank: u32,
    pub(crate) pattern: u32,
    /// The capture's index in the query, which keys its style.
    pub(crate) index: u32,
}

/// The captures `cursor` yields for `query` under `node`, with each capture's
/// [`Capture::rank`] filled in. Set a byte range on `cursor` first to limit
/// the query; the captures can still reach outside it.
pub(crate) fn collect<'t, T, I>(cursor: &mut QueryCursor, query: &Query, node: Node<'t>, text: T) -> Vec<Capture>
where
    T: TextProvider<I>,
    I: AsRef<[u8]>,
{
    let mut found: Vec<(Capture, Node<'t>)> = Vec::new();
    let mut captures = cursor.captures(query, node, text);
    while let Some((m, i)) = captures.next() {
        let c = m.captures()[*i];
        let range = c.node.start_byte() as u32..c.node.end_byte() as u32;
        if range.is_empty() {
            continue;
        }
        let capture =
            Capture { range, node: c.node.id(), rank: 0, pattern: m.pattern_index as u32, index: c.index };
        found.push((capture, c.node));
    }
    found.sort_by_key(|(c, _)| (c.range.start, Reverse(c.range.end)));
    let mut nodes: Vec<Node<'t>> = Vec::new();
    for run in found.chunk_by_mut(|(a, _), (b, _)| a.range == b.range) {
        if run.len() < 2 {
            continue;
        }
        nodes.clear();
        nodes.extend(run.iter().map(|&(_, n)| n));
        nodes.sort_by_key(Node::id);
        nodes.dedup_by_key(|n| n.id());
        if nodes.len() < 2 {
            continue;
        }
        // Nodes with one non-empty range form an ancestor chain, so a node's
        // depth in the run is its number of ancestors in it.
        for (capture, node) in run {
            let ancestors =
                nodes.iter().filter(|a| a.id() != node.id() && a.child_with_descendant(*node).is_some()).count();
            capture.rank = ancestors as u32;
        }
    }
    found.into_iter().map(|(c, _)| c).collect()
}

/// Paint `captures` onto rows. `row_starts` holds the start offset of each
/// row to paint, then one past the end of the last one's newline (the text's
/// length plus one if it's the final row); spans are clipped to those rows
/// and split into line-relative ranges. `styles` is keyed by capture index.
pub(crate) fn paint_rows(
    mut captures: Vec<Capture>,
    row_starts: &[u32],
    styles: &[Option<SpanStyle>],
) -> Vec<Vec<HighlightSpan>> {
    let n_rows = row_starts.len() - 1;
    let lo = row_starts[0];
    let hi = row_starts[n_rows] - 1;

    // Sorted by original range, enclosing nodes come before the nodes they
    // contain (a preorder), and one node's captures sit together in pattern
    // order.
    captures.sort_by_key(|c| (c.range.start, Reverse(c.range.end), c.rank, c.node, c.pattern));
    let mut nodes: Vec<(Range<u32>, SpanStyle)> = Vec::new();
    let mut last_node = None;
    for c in &captures {
        if last_node == Some(c.node) {
            continue;
        }
        let Some(style) = styles[c.index as usize] else { continue };
        last_node = Some(c.node);
        let range = c.range.start.max(lo)..c.range.end.min(hi);
        if !range.is_empty() {
            nodes.push((range, style));
        }
    }

    // A sweep over the boundaries: the most recently opened node still open
    // is the deepest one covering the position.
    let mut runs: Vec<(Range<u32>, SpanStyle)> = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    let mut next = 0;
    let mut pos = lo;
    loop {
        while next < nodes.len() && nodes[next].0.start <= pos {
            open.push(next);
            next += 1;
        }
        while open.last().is_some_and(|&top| nodes[top].0.end <= pos) {
            open.pop();
        }
        let top = open.last().map(|&top| &nodes[top]);
        let to = match (nodes.get(next).map(|n| n.0.start), top.map(|n| n.0.end)) {
            (None, None) => break,
            (Some(a), Some(b)) => a.min(b),
            (a, b) => a.or(b).expect("one side is Some"),
        };
        if let Some(&(_, style)) = top {
            runs.push((pos..to, style));
        }
        pos = to;
    }

    let mut rows: Vec<Vec<HighlightSpan>> = vec![Vec::new(); n_rows];
    let mut row = 0;
    for (range, style) in runs {
        let mut at = range.start;
        while at < range.end {
            // A row's content ends before its newline.
            while row < n_rows && at >= row_starts[row + 1] - 1 {
                row += 1;
            }
            if row == n_rows {
                break;
            }
            let start = row_starts[row];
            let end = row_starts[row + 1] - 1;
            at = at.max(start);
            let to = range.end.min(end);
            if at < to {
                push_merged(&mut rows[row], at - start..to - start, style);
            }
            at = end;
        }
    }
    rows
}

/// Append a span, extending the previous one instead when it ends where this
/// one starts with the same style.
fn push_merged(row: &mut Vec<HighlightSpan>, range: Range<u32>, style: SpanStyle) {
    if let Some(last) = row.last_mut() {
        if last.range.end == range.start && last.style == style {
            last.range.end = range.end;
            return;
        }
    }
    row.push(HighlightSpan { range, style });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlight::Rgba;

    const RED: SpanStyle = SpanStyle { fg: Rgba { r: 0xff, g: 0, b: 0, a: 0xff }, bold: false, italic: false };
    const GREEN: SpanStyle = SpanStyle { fg: Rgba { r: 0, g: 0xff, b: 0, a: 0xff }, bold: false, italic: false };

    fn capture(range: Range<u32>, node: usize, index: u32) -> Capture {
        Capture { range, node, rank: 0, pattern: 0, index }
    }

    fn span(range: Range<u32>, style: SpanStyle) -> HighlightSpan {
        HighlightSpan { range, style }
    }

    #[test]
    fn captures_are_clipped_to_the_painted_rows() {
        // Rows 1 and 2 of "aaaa\nbbbb\ncccc\ndddd": starts 5 and 10, then 15.
        let captures = vec![capture(0..20, 0, 0), capture(2..7, 1, 1), capture(16..18, 2, 1)];
        let rows = paint_rows(captures, &[5, 10, 15], &[Some(RED), Some(GREEN)]);
        assert_eq!(rows, vec![vec![span(0..2, GREEN), span(2..4, RED)], vec![span(0..4, RED)]]);
    }

    #[test]
    fn touching_spans_with_one_style_merge() {
        let captures = vec![capture(0..2, 0, 0), capture(2..4, 1, 1), capture(5..6, 2, 0)];
        let rows = paint_rows(captures, &[0, 7], &[Some(RED), Some(RED)]);
        assert_eq!(rows, vec![vec![span(0..4, RED), span(5..6, RED)]]);
    }

    #[test]
    fn a_node_whose_captures_are_all_unstyled_leaves_its_parent_showing() {
        let captures = vec![capture(0..6, 0, 0), capture(2..4, 1, 1)];
        let rows = paint_rows(captures, &[0, 7], &[Some(RED), None]);
        assert_eq!(rows, vec![vec![span(0..6, RED)]]);
    }

    #[test]
    fn the_last_row_ends_at_the_text() {
        // "ab\ncd" with a capture over the whole text.
        let rows = paint_rows(vec![capture(0..5, 0, 0)], &[0, 3, 6], &[Some(RED)]);
        assert_eq!(rows, vec![vec![span(0..2, RED)], vec![span(0..2, RED)]]);
    }
}
