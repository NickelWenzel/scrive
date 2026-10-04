//! The line splices a commit hands the highlight caches: one
//! `(pre_start, old_lines, new_lines)` per edit, in pre-edit rows.

use crate::buffer::Buffer;
use crate::transaction::Committed;

/// Per-edit pre-edit line spans `(pre_start, old_lines, new_lines)`,
/// ascending and coalesced disjoint, so the highlight commit
/// invalidates only the actually-edited lines — not the whole
/// first-to-last covering range (which would over-invalidate the
/// lines between scattered multi-caret edits). `old_lines` comes from
/// each edit's replaced text (its inverse op); `new_lines` from the
/// post-edit buffer.
pub(crate) fn line_splices(buffer: &Buffer, committed: &Committed) -> Vec<(u32, u32, u32)> {
    let edits = committed.patch().edits();
    let mut spans: Vec<(u32, u32, u32)> = Vec::with_capacity(edits.len());
    let mut acc: i64 = 0;
    for (e, inv) in edits.iter().zip(committed.inverse_ops()) {
        let post_sr = buffer.offset_to_point(e.new.start).row;
        let post_er = buffer.offset_to_point(e.new.end).row;
        let new_lines = post_er - post_sr + 1;
        let old_lines = inv.text.bytes().filter(|&b| b == b'\n').count() as u32 + 1;
        let pre_start = (i64::from(post_sr) - acc) as u32;
        acc += i64::from(new_lines) - i64::from(old_lines);
        match spans.last_mut() {
            // Same-line / touching edits share a pre-edit line — coalesce
            // so the span list stays disjoint (the merge walks need it).
            Some(last) if pre_start < last.0 + last.1 => {
                let merged_end = (last.0 + last.1).max(pre_start + old_lines);
                let combined_delta = (i64::from(last.2) - i64::from(last.1))
                    + (i64::from(new_lines) - i64::from(old_lines));
                last.1 = merged_end - last.0;
                last.2 = (i64::from(last.1) + combined_delta) as u32;
            }
            _ => spans.push((pre_start, old_lines, new_lines)),
        }
    }
    spans
}
