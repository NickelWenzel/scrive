//! [`DirtyRanges`]: the rows a highlight cache has yet to converge, stored as
//! runs.

use core::ops::Range;

/// Disjoint, ascending dirty-line ranges: the set of lines whose highlight
/// state has not yet converged. Storing runs (not individual rows) keeps a
/// commit O(edit + #runs) even when a large file carries a long dirty tail.
/// All operations are front-biased: `tokenize_until` always consumes the first
/// dirty row, and edits splice near the front of whatever tail remains, so the
/// run list stays single-digit in practice (a canary pins that).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DirtyRanges(Vec<Range<u32>>);

impl DirtyRanges {
    /// Every row of an `n`-line document dirty (load / theme change) — ONE
    /// run covering the document (a `Vec` holding a single `Range` element,
    /// which is exactly the point of the range-run representation).
    pub(crate) fn all(n: u32) -> Self {
        Self((n > 0).then_some(0..n).into_iter().collect())
    }

    /// The first dirty row, if any — the tokenize frontier.
    pub(crate) fn first(&self) -> Option<u32> {
        self.0.first().map(|r| r.start)
    }

    /// Remove `row`, which MUST be the current first dirty row (the only
    /// consumption order `tokenize_until` uses). O(1) amortized.
    pub(crate) fn remove_first(&mut self, row: u32) {
        debug_assert_eq!(self.first(), Some(row), "consumption is front-only");
        let r = &mut self.0[0];
        r.start += 1;
        if r.start >= r.end {
            self.0.remove(0);
        }
    }

    /// Mark one row dirty (the cascade step) — merging into a neighbouring
    /// run so the list stays disjoint and non-adjacent. O(log + shift), and
    /// the cascade inserts at/near the front in practice.
    pub(crate) fn insert(&mut self, row: u32) {
        let i = self.0.partition_point(|r| r.end < row);
        if i < self.0.len() {
            let r = &mut self.0[i];
            if r.start <= row && row < r.end {
                return; // already dirty
            }
            if r.end == row {
                r.end += 1; // extend this run right…
                if i + 1 < self.0.len() && self.0[i].end == self.0[i + 1].start {
                    let next_end = self.0[i + 1].end;
                    self.0[i].end = next_end; // …merging into the next run
                    self.0.remove(i + 1);
                }
                return;
            }
            if r.start == row + 1 {
                r.start = row; // extend this run left
                return;
            }
        }
        self.0.insert(i, row..row + 1);
    }

    /// The commit splice: `spans` is the per-edit list of pre-edit line
    /// spans `(pre_start, old_lines, new_lines)`, ascending and disjoint. Shifts
    /// existing runs through the combined splices and marks ONLY each edit's own
    /// post-edit line span dirty — so a scattered multi-caret transaction marks
    /// O(carets) lines, not the whole first-to-last covering range. One pass and
    /// one sort of the O(runs + edits) boundary set. A single covering span
    /// reproduces the classic clip-shift-mark commit splice exactly.
    pub(crate) fn apply_splices(&mut self, spans: &[(u32, u32, u32)]) {
        if spans.is_empty() {
            return;
        }
        // `pref[i]` = accumulated line delta of `spans[..i]`. `shift_at(pre)` is
        // the post-edit displacement of a pre-edit row *not inside* any edit
        // (rows inside an edit are covered by that edit's new span below), which
        // is the delta of all edits entirely above it.
        let mut pref: Vec<i64> = Vec::with_capacity(spans.len() + 1);
        pref.push(0);
        for &(_, o, n) in spans {
            pref.push(pref[pref.len() - 1] + i64::from(n) - i64::from(o));
        }
        let shift_at = |pre: u32| -> i64 {
            let i = spans.partition_point(|s| s.0 + s.1 <= pre);
            pref[i]
        };
        let old_runs = std::mem::take(&mut self.0);
        let mut out: Vec<Range<u32>> = Vec::with_capacity(old_runs.len() + spans.len());
        // Existing runs, shifted. `[a + shift(a), b + shift(b))` is a superset
        // of the shifted gap rows and any spanned edit region (shift is
        // non-decreasing), which is safe: over-marking dirty only costs re-work,
        // under-marking would leave a stale color.
        for r in &old_runs {
            let a = (i64::from(r.start) + shift_at(r.start)) as u32;
            let b = (i64::from(r.end) + shift_at(r.end)) as u32;
            if a < b {
                out.push(a..b);
            }
        }
        // Each edit's own post-edit new span.
        for (j, &(ps, _o, n)) in spans.iter().enumerate() {
            if n > 0 {
                let post_start = (i64::from(ps) + pref[j]) as u32;
                out.push(post_start..post_start + n);
            }
        }
        out.sort_unstable_by_key(|r| r.start);
        // Coalesce touching/overlapping runs back into the ascending-disjoint
        // invariant `splice`/`clear_range` maintain.
        let mut merged: Vec<Range<u32>> = Vec::with_capacity(out.len());
        for r in out {
            match merged.last_mut() {
                Some(last) if last.end >= r.start => last.end = last.end.max(r.end),
                _ => merged.push(r),
            }
        }
        self.0 = merged;
    }

    /// Total dirty rows (sum of run lengths) — the invalidation size a commit
    /// scheduled, which the sweep must eventually walk.
    #[cfg(test)]
    pub(crate) fn total_rows(&self) -> u32 {
        self.0.iter().map(|r| r.end - r.start).sum()
    }

    /// Whether `row` is dirty. Binary search over the ascending disjoint runs.
    pub(crate) fn contains(&self, row: u32) -> bool {
        let i = self.0.partition_point(|r| r.end <= row);
        i < self.0.len() && self.0[i].start <= row
    }

    /// Clear rows `[range)` from the dirty set — the verified-absorb path: those
    /// rows' states are proven, so they leave the frontier without being walked.
    /// Clips each run around the range; the output stays ascending, disjoint,
    /// and non-adjacent (inputs are, and clipping only shrinks), so front-biased
    /// consumption is preserved.
    pub(crate) fn clear_range(&mut self, range: Range<u32>) {
        if range.start >= range.end {
            return;
        }
        let old = std::mem::take(&mut self.0);
        let mut out: Vec<Range<u32>> = Vec::with_capacity(old.len() + 1);
        for r in old {
            let lo = r.start..r.end.min(range.start); // part below the cleared span
            let hi = r.start.max(range.end)..r.end; // part at/above it
            if lo.start < lo.end {
                out.push(lo);
            }
            if hi.start < hi.end {
                out.push(hi);
            }
        }
        self.0 = out;
    }

    /// Number of disjoint runs — the canary probe.
    #[cfg(test)]
    pub(crate) fn runs(&self) -> usize {
        self.0.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirty_ranges_shift_merge_unit() {
        let mut d = DirtyRanges::default();
        d.insert(5);
        d.insert(7);
        d.insert(6); // bridges 5..6 and 7..8 into one run
        assert_eq!(d.0, vec![5..8]);
        d.insert(4); // extends left
        assert_eq!(d.0, vec![4..8]);
        // Splice via a single covering span: rows [5,7) became 3 rows → tail
        // shifts by +1 (apply_splices reproduces the classic commit splice).
        d.apply_splices(&[(5, 2, 3)]);
        assert_eq!(d.0, vec![4..9], "clip + shift + new-block merge into one run");
        // A pure delete ahead of the run shifts it left.
        d.apply_splices(&[(0, 2, 0)]);
        assert_eq!(d.0, vec![2..7]);
        // Front consumption.
        assert_eq!(d.first(), Some(2));
        d.remove_first(2);
        assert_eq!(d.0, vec![3..7]);
    }
}
