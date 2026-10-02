# Phase 1 — scrive-core: a per-commit change log, snapshot primitives, one jump verb

Read `MAP_PLAN.md` first (Current state, D5, D7, D8, D17, Constraints). This doc is the
implementation spec for Phase 1 only. Every line number below was read from source at HEAD
`6cf4f2c`; re-grep before editing if the tree has moved.

## 1. Prerequisites

- **No earlier phase.** This is the first phase.
- `git log --oneline -1` shows `6cf4f2c` (or a descendant with no scrive-core changes), and
  `git status` is clean apart from `.claude/`.
- **Baseline:** `cargo test --workspace` passes at HEAD (about 10 s). **Clippy does not.** With
  clippy 0.1.98, `cargo clippy --workspace --all-targets -- -D warnings` fails on
  `crates/scrive-core/src/highlight.rs:1097` (`clippy::manual_slice_fill`). Step 0 fixes that, as
  its own commit, before any Phase 1 work. The orchestrator verified in a scratch copy that this
  single change makes the whole workspace clippy-clean.
- Confirm the gaps this phase closes are still open:
  - `grep -n "observe_changes\|changes: Vec<EditOp>" crates/scrive-core/src/document.rs` shows
    the `bool` + `Vec<EditOp>` log (document.rs:106-107);
  - `Document::undo`/`redo` (document.rs:1088, :1125) do not touch the log;
  - `Snapshot` (buffer.rs:108-161) has no `offset_to_point`, `chunks` or `clip_offset`;
  - `Document` has no `doc_id()` and no `select_and_reveal`.

## 2. Goal and exit criteria

**Goal.** scrive-core gains everything the LSP client (Phases 4-9) needs from a document, with no
LSP knowledge: a per-commit change log that covers undo and redo, point and chunk access on
`Snapshot`, the document's identity, and one public fold-aware jump verb.

**When this phase is done:**

1. Every committed transaction (a forward edit, and every *step* of an undo or redo) appends one
   `document::Change { before, ops }` while observing. Replaying an entry's `ops` in order onto
   `before.text()` yields the next entry's `before.text()`, and the last entry yields the live
   text. Entry revisions advance by exactly one, starting at `Changes::from()`.
   - `change_log_replays_each_commit_onto_its_before_snapshot` (random batches with tied inserts,
     typing runs, single- and multi-step undo/redo, drains after one or many commits)
   - `tied_inserts_replay_in_the_order_the_rope_applied_them`
   - `undo_and_redo_of_a_typing_run_log_one_entry_per_step`
   - `observing_starts_the_chain_at_the_current_revision`
2. Past 1024 undrained entries the log clears itself and reports `from() == None` ("broken"),
   then resumes at the next drain.
   - `hitting_the_cap_breaks_the_log_until_the_next_drain`
3. `Document::select_and_reveal(range)` selects a clamped, char-snapped range, unfolds whatever
   hides either end, and bumps `reveal_seq` with `RevealMode::Center`. `step_find` and
   `next_diagnostic` share its private helper.
   - `select_and_reveal_unfolds_the_target_and_requests_a_centered_reveal`
   - `select_and_reveal_clamps_and_snaps_to_char_boundaries`
   - the existing `reveal_classes_and_no_op_verbs` and find-navigation tests stay green
4. `Snapshot` has `offset_to_point`, `point_to_offset` (row clamp documented), `chunks(range)`
   and `clip_offset`, all matching `Buffer`.
   - `snapshot_points_round_trip_and_clamp_like_the_buffer`
   - `snapshot_chunks_concatenate_to_the_slice`
   - `snapshot_clip_offset_matches_the_buffer`
5. `Document::doc_id()` exists. — `doc_id_is_the_buffer_identity`
6. `CodeEditor::drain_changes` returns `document::Changes`; its test is updated
   (`drain_changes_mirrors_edits_when_observing`).
7. Every existing test stays green, and clippy/doc are clean with `-D warnings`.

## 3. Design decisions implemented here

- **D8 — per-commit change log.** `Document` owns a private `ChangeLog { on, from, entries }`.
  Its `record(before, &[EditOp])` is the only writer; `edit_grouped`, `undo` and `redo` all call
  it. An entry is `document::Change { before: Snapshot, ops: Vec<EditOp> }`:
  - `ops` are the transaction's applied forward ops **reversed**. `transaction::apply` sorts
    ascending with ties in caller order (transaction.rs:184), and the rope applies a batch in
    reverse (rope.rs:370-400, `rebuild_leaf`), so the reversed list is descending and replays
    sequentially in exactly the rope's order, ties included. The current
    `sort_by_key(Reverse(start))` keeps tie order and so replays two inserts at one offset in the
    wrong order.
  - `before` is an O(1) `Snapshot` (an `Arc` rope clone), taken only while observing.
  - Undo/redo take `buffer.snapshot()` before `history.undo/redo`; each `on_step` records the
    step against the held snapshot and swaps in the post-step one. A typing run is a multi-step
    undo element (history.rs:207), so one undo can log several entries.
  - `from` is set to the current revision by `observe_changes(true)` and by every drain.
  - **Cap:** above 1024 entries the log clears and sets `from = None`. While broken it logs
    nothing (bounded memory for an observer that never drains) until the next drain resets `from`.
  - `drain_changes()` returns an opaque `document::Changes { doc_id, from, entries }` with private
    fields, accessors and an iterator. `CodeEditor::drain_changes` forwards it.
- **D5 (part)** — `Document::doc_id()`.
- **D7 (part)** — the snapshot primitives the Phase 4 encoding core consumes: allocation-free
  `chunks(range)`, `clip_offset`, and point conversion documented with the rope's row clamp
  ("a row past the last line clamps to the last line", which D7 says is wrong for LSP; the
  client checks it explicitly).
- **D17 (part)** — `Document::select_and_reveal(range)`: clamps, snaps, then runs the jump
  sequence `set_single → reset_transient → unfold_to_reveal ×2 → request_reveal(Center)`, which
  `step_find` and `next_diagnostic` currently repeat inline (document.rs:2119-2126, :2150-2154).

**Decisions this doc makes where the plan is silent:**

- Decision: `Snapshot::chunks` and `Snapshot::clip_offset` are built on the existing
  `Rope::chunk_at` inside buffer.rs (two private free fns shared with `Buffer::clip_offset`), so
  rope.rs is untouched and the plan's file list holds.
- Decision: `Snapshot::chunks` clips its range (start snaps left, end snaps right, both clamped to
  the length), so it never panics on a mid-char offset. An inverted range yields nothing.
- Decision: `select_and_reveal` normalizes an inverted range by swapping the ends (it is a
  selection verb, not LSP conversion), then snaps start left and end right.
- Decision: `observe_changes(true)` while already observing restarts the chain (clears entries,
  `from` = now). It is how `open_lsp` (D19) resets the log.
- Decision: `document::Changes` exposes `doc_id()`, `from()`, `iter()`, `len()` and `is_empty()`;
  `document::Change` exposes `before()` and `ops()`. No public constructors.

## 4. Step-by-step changes

### Step 0 — fix the clippy 1.98 baseline (its own commit)

Commit message: `fix(core): satisfy clippy 1.98 manual_slice_fill`. Commit before Step 1; nothing
else goes in it.

`crates/scrive-core/src/highlight.rs:1095-1101`, current:

```rust
    pub fn set_theme(&mut self, theme: TokenTheme) {
        self.theme = Arc::new(theme);
        for s in &mut self.ret.win_states {
            *s = None;
        }
        self.ret.checkpoints.clear();
        self.ret.invalid = DirtyRanges::all(self.ret.n_lines);
    }
```

New:

```rust
    pub fn set_theme(&mut self, theme: TokenTheme) {
        self.theme = Arc::new(theme);
        self.ret.win_states.fill(None);
        self.ret.checkpoints.clear();
        self.ret.invalid = DirtyRanges::all(self.ret.n_lines);
    }
```

Verify: `cargo clippy --workspace --all-targets -- -D warnings` is clean, and
`cargo test --workspace` still passes.

### Step 1 — `Snapshot` primitives (crates/scrive-core/src/buffer.rs)

Current `impl Snapshot` (buffer.rs:108-161) has `len`, `is_empty`, `text`, `slice`, `line`,
`line_count`, `doc_id`, `revision`. Current `Buffer::clip_offset` (buffer.rs:324-335):

```rust
    /// Clamp a byte offset into `[0, len]` and snap it to a char boundary
    /// (direction from `bias`). Idempotent.
    #[must_use]
    pub fn clip_offset(&self, offset: u32, bias: Bias) -> u32 {
        let off = offset.min(self.len());
        // A non-boundary offset sits strictly inside one char, and chunk
        // boundaries are char boundaries — so the snap never leaves the chunk
        // containing `off`.
        let (chunk, chunk_start) = self.text.chunk_at(off);
        chunk_start + snap_char_boundary(chunk, off - chunk_start, bias)
    }
```

1a. Move that body into a private free fn at the bottom of the non-test code (before
`#[cfg(test)] mod tests`), next to a chunk-walk fn. Both `Buffer` and `Snapshot` call them, so
the snap rule and the walk have one owner:

```rust
/// Clamp `offset` into `[0, len]` and snap it to a char boundary in `bias`'s
/// direction — the one snap rule behind [`Buffer::clip_offset`] and
/// [`Snapshot::clip_offset`]. A non-boundary offset sits strictly inside one
/// char, and chunk boundaries are char boundaries, so the snap never leaves the
/// chunk containing it.
fn clip_in(text: &Rope, offset: u32, bias: Bias) -> u32 {
    let off = offset.min(text.len());
    let (chunk, chunk_start) = text.chunk_at(off);
    chunk_start + snap_char_boundary(chunk, off - chunk_start, bias)
}

/// The rope's text in `range` as borrowed chunk slices, in order — the
/// allocation-free twin of a ranged `slice`. The range is clipped first (start
/// snaps left, end snaps right), so a mid-char offset never splits a char; an
/// inverted range yields nothing. Each step is one `O(log chunks)` descent, and
/// nothing is copied.
fn chunks_in(text: &Rope, range: Range<u32>) -> impl Iterator<Item = &str> + '_ {
    let end = clip_in(text, range.end, Bias::Right);
    let mut pos = clip_in(text, range.start, Bias::Left);
    std::iter::from_fn(move || {
        if pos >= end {
            return None;
        }
        // `chunk_at` returns the chunk containing `pos`; `pos < len`, so the
        // tail slice is non-empty and the walk always advances.
        let (chunk, chunk_start) = text.chunk_at(pos);
        let from = (pos - chunk_start) as usize;
        let to = (end.min(chunk_start + chunk.len() as u32) - chunk_start) as usize;
        if to <= from {
            return None;
        }
        pos = chunk_start + to as u32;
        Some(&chunk[from..to])
    })
}
```

`Buffer::clip_offset` becomes `clip_in(&self.text, offset, bias)` (keep its doc comment).

1b. Append to `impl Snapshot`, after `revision()` (buffer.rs:157-160):

```rust
    /// Convert a byte offset to a [`Point`], as [`Buffer::offset_to_point`]
    /// does: an offset past the end clamps to the end of the snapshot.
    #[must_use]
    pub fn offset_to_point(&self, offset: u32) -> Point {
        self.text.byte_to_point(offset)
    }

    /// Convert a [`Point`] to a byte offset, as [`Buffer::point_to_offset`]
    /// does. **Both coordinates clamp:** a row past the last line becomes the
    /// last line (keeping the column), and a column past the line's end becomes
    /// the line end. A consumer with a different past-the-end rule (LSP maps a
    /// line at or past `line_count` to the document end) checks the row itself
    /// before calling this.
    #[must_use]
    pub fn point_to_offset(&self, point: Point) -> u32 {
        self.text.point_to_offset(point)
    }

    /// The text in `range` as borrowed chunk slices, in document order — for a
    /// consumer that walks text without materializing it (position-encoding
    /// conversion). The range is clamped to the snapshot and snapped outward to
    /// char boundaries; an inverted range yields nothing. Allocation-free.
    pub fn chunks(&self, range: Range<u32>) -> impl Iterator<Item = &str> + '_ {
        chunks_in(&self.text, range)
    }

    /// Clamp a byte offset into `[0, len]` and snap it to a char boundary in
    /// `bias`'s direction — [`Buffer::clip_offset`] on the frozen text.
    #[must_use]
    pub fn clip_offset(&self, offset: u32, bias: Bias) -> u32 {
        clip_in(&self.text, offset, bias)
    }
```

`Point`, `Bias`, `Range` and `snap_char_boundary` are already imported at buffer.rs:29-32.

### Step 2 — the change log types (crates/scrive-core/src/document.rs)

2a. Imports, document.rs:17. Current:

```rust
use crate::buffer::{Buffer, EolFlavor, LoadError, Revision, Snapshot};
```

New:

```rust
use crate::buffer::{Buffer, DocId, EolFlavor, LoadError, Revision, Snapshot};
```

2b. Fields, document.rs:101-107. Current:

```rust
    /// Opt-in incremental-change log. Off by default (zero overhead per edit);
    /// when [`Document::observe_changes`] enables it, every committed edit's
    /// applied ops are appended for a host to [`drain`](Document::drain_changes)
    /// — e.g. to mirror edits to a language server as `textDocument/didChange`.
    observe_changes: bool,
    changes: Vec<EditOp>,
```

New:

```rust
    /// Opt-in per-commit change log. Off by default (zero overhead per edit);
    /// when [`Document::observe_changes`] enables it, every committed
    /// transaction — forward edits and each undo/redo step alike — appends one
    /// [`Change`] for a host to [`drain`](Document::drain_changes), e.g. to
    /// mirror edits to a language server as `textDocument/didChange`.
    changes: ChangeLog,
```

2c. `Document::new`, document.rs:191-192. Replace

```rust
            observe_changes: false,
            changes: Vec::new(),
```

with `changes: ChangeLog::default(),`.

2d. New types. Put them after `struct CellCorner` (document.rs:160-164) and before
`impl Document`:

```rust
/// How many undrained commits the change log holds before it gives up. Past
/// this the observer is clearly not draining; keeping every `before` snapshot
/// would pin that many rope versions, so the log breaks instead and the next
/// drain reports [`Changes::from`] as `None` — the consumer's cue to resync
/// from a full snapshot.
const CHANGE_LOG_CAP: usize = 1024;

/// One committed transaction, as the change log saw it: the text just before
/// the commit, and the edits that turned it into the text just after.
///
/// `ops` are ordered for **sequential** replay: each op's range is in the
/// coordinates of `before` with every later-listed op already applied, so
/// applying them one by one onto `before.text()` reproduces the commit
/// exactly — including several inserts at one offset, which land in the
/// order the rope placed them.
#[derive(Clone, Debug)]
pub struct Change {
    before: Snapshot,
    ops: Vec<EditOp>,
}

impl Change {
    /// The document as it was just before this commit. Its revision is the
    /// commit's starting revision; the commit ends at `revision + 1`.
    #[must_use]
    pub fn before(&self) -> &Snapshot {
        &self.before
    }

    /// The commit's edits, descending, for sequential replay onto
    /// [`before`](Self::before).
    #[must_use]
    pub fn ops(&self) -> &[EditOp] {
        &self.ops
    }
}

/// Everything the change log recorded since the previous drain: the commits
/// in order, the document they belong to, and the revision the first one
/// starts from.
///
/// A consumer can trust the chain only if [`from`](Self::from) is `Some` and
/// equals the revision it last synced: then each entry's `before().revision()`
/// is one more than the previous, and the last entry ends at the live
/// revision. `from() == None` means the log was off or overflowed, so the
/// entries (if any) do not form a complete chain.
#[derive(Clone, Debug)]
pub struct Changes {
    doc_id: DocId,
    from: Option<Revision>,
    entries: Vec<Change>,
}

impl Changes {
    /// The document these changes belong to.
    #[must_use]
    pub fn doc_id(&self) -> DocId {
        self.doc_id
    }

    /// The revision the first entry starts from, or `None` when the chain is
    /// broken (the log was off, or overflowed its cap since the last drain).
    #[must_use]
    pub fn from(&self) -> Option<Revision> {
        self.from
    }

    /// The recorded commits, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &Change> + '_ {
        self.entries.iter()
    }

    /// How many commits were recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing was recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The change log's state: whether it is on, the revision its chain starts
/// from (`None` while off or broken), and the commits since the last drain.
/// [`record`](Self::record) is the one writer, so every commit path logs the
/// same way.
#[derive(Debug, Default)]
struct ChangeLog {
    on: bool,
    from: Option<Revision>,
    entries: Vec<Change>,
}

impl ChangeLog {
    /// The snapshot a commit must hand to [`record`](Self::record), or `None`
    /// when nothing would be logged — off, or broken until the next drain — so
    /// the common path takes no snapshot at all.
    fn capture(&self, buffer: &Buffer) -> Option<Snapshot> {
        (self.on && self.from.is_some()).then(|| buffer.snapshot())
    }

    /// Log one committed transaction: `before` is the text it was applied to,
    /// `applied` its normalized forward ops (ascending, ties in caller order).
    /// Reversing them gives the sequential replay order (see [`Change`]).
    fn record(&mut self, before: Snapshot, applied: &[EditOp]) {
        if self.entries.len() >= CHANGE_LOG_CAP {
            // Nobody is draining; drop the backlog (and the snapshots it pins)
            // and mark the chain broken so the consumer resyncs in full.
            self.entries = Vec::new();
            self.from = None;
            return;
        }
        self.entries.push(Change { before, ops: applied.iter().rev().cloned().collect() });
    }

    /// Turn the log on (a fresh chain starting at `revision`) or off.
    fn set(&mut self, on: bool, revision: Revision) {
        self.on = on;
        self.entries = Vec::new();
        self.from = on.then_some(revision);
    }

    /// Hand out everything since the last drain; the next chain starts at
    /// `revision`, which also heals a broken log.
    fn drain(&mut self, doc_id: DocId, revision: Revision) -> Changes {
        let changes = Changes { doc_id, from: self.from, entries: std::mem::take(&mut self.entries) };
        if self.on {
            self.from = Some(revision);
        }
        changes
    }
}
```

Composite name note: `ChangeLog` is private, so the module-path rule for public names does not
bite; the public types are `document::Change` and `document::Changes`. Do **not** add them to
the crate-root `pub use` list (lib.rs stays untouched this phase).

### Step 3 — `doc_id`, `edit_grouped`, `observe_changes`, `drain_changes` (document.rs)

3a. After `snapshot()` (document.rs:932-937) add:

```rust
    /// This document's identity — its buffer's [`DocId`]. Stable for the
    /// document's lifetime: replacing the whole text is an edit, not a reload.
    #[must_use]
    pub fn doc_id(&self) -> DocId {
        self.buffer.doc_id()
    }
```

3b. `edit_grouped`, document.rs:957-960. Current:

```rust
        // The (revision, fold generation) the fold cache would need to be at to
        // shift it in place instead of rebuilding — captured before the edit.
        let pre_fold_key = (self.buffer.revision().0, self.folds.generation());
        let committed = apply(&mut self.buffer, ops)?;
```

New (insert one line before `apply`):

```rust
        // The (revision, fold generation) the fold cache would need to be at to
        // shift it in place instead of rebuilding — captured before the edit.
        let pre_fold_key = (self.buffer.revision().0, self.folds.generation());
        // The pre-commit text for the change log — an O(1) rope clone, taken
        // only while observing.
        let before = self.changes.capture(&self.buffer);
        let committed = apply(&mut self.buffer, ops)?;
```

3c. `edit_grouped`, document.rs:1034-1044. Current:

```rust
            // Log the applied deltas for a host draining incremental changes
            // (LSP `didChange`) — only when observing, so the common path pays
            // nothing. Descending start order so sequential application within
            // this batch is offset-stable; ops move into history next, so read
            // them here. Their ranges are in the pre-edit coordinates the batch
            // was applied against — exactly what a content-change wants.
            if self.observe_changes {
                let mut ops = committed.forward_ops().to_vec();
                ops.sort_by_key(|op| core::cmp::Reverse(op.range.start));
                self.changes.extend(ops);
            }
```

New:

```rust
            // Log the commit for a host draining incremental changes (LSP
            // `didChange`); ops move into history next, so read them here.
            if let Some(before) = before {
                self.changes.record(before, committed.forward_ops());
            }
```

3d. `observe_changes` / `drain_changes`, document.rs:1054-1078. Replace both with:

```rust
    /// Turn the per-commit change log on or off. Off by default, so the common
    /// path pays nothing. Turning it on (again, too) starts a fresh chain at
    /// the current revision; turning it off drops anything pending. A host
    /// mirroring edits to a language server turns it on and drains
    /// [`drain_changes`](Document::drain_changes) after each round of edits;
    /// a full-document-sync host leaves it off and re-reads
    /// [`snapshot`](Document::snapshot).
    pub fn observe_changes(&mut self, on: bool) {
        let revision = self.buffer.revision();
        self.changes.set(on, revision);
    }

    /// Drain the change log: one [`Change`] per commit since the last drain
    /// (forward edits and every undo/redo step), oldest first, each with the
    /// snapshot it was applied to. Empty, with [`Changes::from`] `None`, unless
    /// [`observe_changes`](Document::observe_changes) is on.
    #[must_use]
    pub fn drain_changes(&mut self) -> Changes {
        let (doc_id, revision) = (self.buffer.doc_id(), self.buffer.revision());
        self.changes.drain(doc_id, revision)
    }
```

### Step 4 — log undo and redo (document.rs:1088-1152)

Current `undo` (document.rs:1088-1121):

```rust
    pub fn undo(&mut self) -> bool {
        self.reset_transient();
        let tab = self.tab_size();
        // Where to land the caret: ...
        let mut caret_home: Option<u32> = None;
        let bracket_cfg = self.bracket_config();
        let Self {
            history, buffer, selections, highlight, brackets, decorations, autoclose, folds, find, ..
        } = self;
        let mut views = Views { highlight, brackets, decorations, autoclose, folds, find };
        let undone = history.undo(buffer, selections, |committed, buffer| {
            // The one mover — highlight, brackets, decorations, folds (position +
            // fold reveal) all ride it, so undo needs no per-feature resync.
            rebase_views(&mut views, buffer, tab, committed, &bracket_cfg);
            if let Some(e) = committed.patch().edits().first() {
                caret_home = Some(e.new.start);
            }
        });
        // Reverting text can invalidate a fold's bracket pair too — ...
        if undone {
            self.reconcile_folds();
            ...
            if let Some(off) = caret_home {
                self.selections = SelectionSet::new(off);
            }
        }
        undone
    }
```

New `undo` — full body; the added lines are `changes` in the destructure, `before`, and the
logging block in the callback. Keep the existing comments that are elided here verbatim.

```rust
    pub fn undo(&mut self) -> bool {
        self.reset_transient();
        let tab = self.tab_size();
        // (existing caret_home comment, unchanged)
        let mut caret_home: Option<u32> = None;
        let bracket_cfg = self.bracket_config();
        let Self {
            history, buffer, selections, highlight, brackets, decorations, autoclose, folds, find,
            changes, ..
        } = self;
        let mut views = Views { highlight, brackets, decorations, autoclose, folds, find };
        // Each reverted step is its own commit (its own revision), so the log
        // gets one entry per step: record against the text before the step,
        // then hold the post-step text for the next one.
        let mut before = changes.capture(buffer);
        let undone = history.undo(buffer, selections, |committed, buffer| {
            // The one mover — highlight, brackets, decorations, folds (position +
            // fold reveal) all ride it, so undo needs no per-feature resync.
            rebase_views(&mut views, buffer, tab, committed, &bracket_cfg);
            if let Some(prev) = before.as_mut() {
                let prev = std::mem::replace(prev, buffer.snapshot());
                changes.record(prev, committed.forward_ops());
            }
            if let Some(e) = committed.patch().edits().first() {
                caret_home = Some(e.new.start);
            }
        });
        // (existing reconcile / caret_home tail, unchanged)
        if undone {
            self.reconcile_folds();
            if let Some(off) = caret_home {
                self.selections = SelectionSet::new(off);
            }
        }
        undone
    }
```

`redo` (document.rs:1125-1152) gets the identical three changes: add `changes` to the
destructure, `let mut before = changes.capture(buffer);` before `history.redo(...)`, and the same
`if let Some(prev) = before.as_mut() { ... }` block inside the callback, before the
`caret_home` update.

Why this is correct: `history::replay` (history.rs:377-388) calls `apply(buffer, step.clone())`
per step, and the `Committed` it hands `on_step` still carries its forward ops (it is the value
`apply` returned, not the patch-only one `edit_grouped` returns), so `committed.forward_ops()` is
exactly what that step applied. `forward_ops` is `pub(crate)` (transaction.rs:106), visible here.

### Step 5 — one jump verb (document.rs)

5a. Add the private helper and the public verb next to `unfold_to_reveal` (before
document.rs:2159, the `unfold_to_reveal` doc comment):

```rust
    /// Select `range` from any document state and reveal it centered — the
    /// public jump for a host (goto-definition). The range is clamped to the
    /// document and snapped outward to char boundaries; an inverted range is
    /// normalized. The head lands at the end, like a find match.
    pub fn select_and_reveal(&mut self, range: Range<u32>) {
        let (a, b) = (range.start.min(range.end), range.start.max(range.end));
        let start = self.buffer.clip_offset(a, Bias::Left);
        let end = self.buffer.clip_offset(b, Bias::Right);
        self.jump_to(start..end);
    }

    /// The jump sequence every jump-class verb shares: select `range` (head at
    /// its end), treat the jump as a gesture boundary (clears gesture state and
    /// seals the undo group, so typing after it never merges with typing
    /// before), unfold anything hiding either end — the reveal must land on a
    /// visible position — and request a centered reveal. `range` must already
    /// be valid.
    fn jump_to(&mut self, range: Range<u32>) {
        self.selections
            .set_single(Selection::from_anchor(SelectionId(0), range.start, range.end));
        self.reset_transient();
        self.unfold_to_reveal(range.start);
        self.unfold_to_reveal(range.end);
        self.request_reveal(RevealMode::Center);
    }
```

5b. `step_find`, document.rs:2116-2127. Current:

```rust
        if let Some(range) = &found {
            // Set the selection to the match, head at its end.
            self.selections
                .set_single(Selection::from_anchor(SelectionId(0), range.start, range.end));
            self.reset_transient(); // clears gesture state + seals (a jump is a boundary)
            // A match inside a collapsed fold unfolds its chain first — the
            // reveal below must land on a VISIBLE position.
            self.unfold_to_reveal(range.start);
            self.unfold_to_reveal(range.end);
            self.request_reveal(RevealMode::Center); // find navigation centers
        }
        found
```

New:

```rust
        if let Some(range) = &found {
            self.jump_to(range.clone());
        }
        found
```

5c. `next_diagnostic`, document.rs:2150-2155. Current:

```rust
        self.selections
            .set_single(Selection::from_anchor(SelectionId(0), target.start, target.end));
        self.reset_transient(); // a jump is an undo-group boundary
        self.unfold_to_reveal(target.start);
        self.unfold_to_reveal(target.end);
        self.request_reveal(RevealMode::Center); // diagnostic jumps center
        Some(target)
```

New:

```rust
        self.jump_to(target.clone());
        Some(target)
```

Keep `unfold_to_reveal`'s return value; if `step_find`/`next_diagnostic` were its only callers
that used the result and clippy now flags it as unused, leave its signature alone (other callers
exist — grep `unfold_to_reveal(` before changing anything).

### Step 6 — forward the new drain type (crates/scrive-iced/src/code_editor.rs:581-595)

Current:

```rust
    /// Enable or disable the incremental-change log — for a host mirroring edits
    /// to a language server (`textDocument/didChange`). Off by default (zero
    /// overhead); forwards to [`Document::observe_changes`]. Full-document sync
    /// hosts leave this off and re-read `document().snapshot()` instead.
    pub fn observe_changes(&mut self, on: bool) {
        self.doc.observe_changes(on);
    }

    /// Drain the incremental-change log: every applied edit since the last drain,
    /// as `EditOp` deltas ready to translate into LSP content changes. Empty
    /// unless [`observe_changes`](CodeEditor::observe_changes) is on. Forwards to
    /// [`Document::drain_changes`].
    pub fn drain_changes(&mut self) -> Vec<EditOp> {
        self.doc.drain_changes()
    }
```

New:

```rust
    /// Enable or disable the per-commit change log — for a host mirroring
    /// edits to a language server (`textDocument/didChange`). Off by default
    /// (zero overhead); turning it on starts a fresh chain at the current
    /// revision. Forwards to [`Document::observe_changes`]. Full-document sync
    /// hosts leave this off and re-read `document().snapshot()` instead.
    pub fn observe_changes(&mut self, on: bool) {
        self.doc.observe_changes(on);
    }

    /// Drain the change log: one entry per commit since the last drain —
    /// edits, undos and redos alike — each with the snapshot it applied to,
    /// ready to translate into LSP content changes. Empty unless
    /// [`observe_changes`](CodeEditor::observe_changes) is on. Forwards to
    /// [`Document::drain_changes`].
    pub fn drain_changes(&mut self) -> scrive_core::document::Changes {
        self.doc.drain_changes()
    }
```

`EditOp` stays imported (it is used by `edit`/`load`).

## 5. Files that change

| File | Change |
|---|---|
| crates/scrive-core/src/highlight.rs | Step 0: `win_states.fill(None)` (own commit) |
| crates/scrive-core/src/buffer.rs | `clip_in`/`chunks_in` free fns; `Buffer::clip_offset` delegates; `Snapshot::{offset_to_point, point_to_offset, chunks, clip_offset}`; tests |
| crates/scrive-core/src/document.rs | `Change`, `Changes`, private `ChangeLog` + cap; `doc_id`; `edit_grouped`/`undo`/`redo` log through `record`; `observe_changes`/`drain_changes` rewritten; `select_and_reveal` + private `jump_to`; `step_find`/`next_diagnostic` use it; tests |
| crates/scrive-iced/src/code_editor.rs | `drain_changes` returns `document::Changes`; `observe_changes` doc; the drain test |

## 6. Tests to add

All tests carry a `///` doc stating the invariant and string assert messages.

### buffer.rs (`mod tests`, uses `buf(s)` at buffer.rs:458)

- **`snapshot_points_round_trip_and_clamp_like_the_buffer`** — for every offset of
  `"ab\ncd\n"`, `s.offset_to_point(o) == b.offset_to_point(o)` and
  `s.point_to_offset(s.offset_to_point(o)) == o`. Pins the documented clamps:
  `s.point_to_offset(Point::new(9, 0)) == 6` (row clamps to the last line, the empty line after
  the trailing `\n`), `s.point_to_offset(Point::new(0, 99)) == 2`, and
  `s.offset_to_point(99) == Point::new(2, 0)`.
- **`snapshot_chunks_concatenate_to_the_slice`** — build a multi-chunk text (e.g. 300 bytes
  mixing `"é"` and `"😀"`; > `CHUNK_MAX` = 128). For a sweep of ranges on char boundaries,
  `s.chunks(r.clone()).collect::<String>() == s.slice(r)`. Also: an empty range yields no
  chunk; `s.chunks(0..u32::MAX)` equals the whole text; an inverted range yields nothing; a
  range whose ends fall mid-`"😀"` equals the slice of the outward-snapped range.
- **`snapshot_clip_offset_matches_the_buffer`** — on `"aé😀b"`, for every offset in
  `0..=len + 2` and both biases, `s.clip_offset(o, bias) == b.clip_offset(o, bias)`.

### document.rs (`mod tests`, uses `doc(s)` at :2526 and `xorshift` at :5204)

Shared test helpers (add once in `mod tests`):

```rust
    /// Apply `ops` one by one — the consumer's replay of one logged commit.
    fn replay(text: &str, ops: &[EditOp]) -> String {
        let mut s = text.to_owned();
        for op in ops {
            s.replace_range(op.range.start as usize..op.range.end as usize, &op.text);
        }
        s
    }

    /// The drained chain is complete and replays: each entry starts where the
    /// previous ended, its ops turn its `before` into the next entry's
    /// `before`, and the last one reaches `live`.
    fn assert_chain(changes: &Changes, live: &Document) {
        let entries: Vec<&Change> = changes.iter().collect();
        let mut cursor = changes.from().expect("an unbroken log has a start revision");
        for (i, entry) in entries.iter().enumerate() {
            assert_eq!(entry.before().revision(), cursor, "entry {i} starts where the chain is");
            let next = entries
                .get(i + 1)
                .map_or_else(|| live.text().into_owned(), |n| n.before().text().into_owned());
            assert_eq!(replay(&entry.before().text(), entry.ops()), next, "entry {i} replays");
            cursor = Revision(cursor.0 + 1);
        }
        assert_eq!(cursor, live.revision(), "the chain ends at the live revision");
        assert_eq!(changes.doc_id(), live.doc_id(), "the drain names its document");
    }
```

- **`change_log_replays_each_commit_onto_its_before_snapshot`** — `let mut d = doc("fn a() {\n
  é\n}\n"); d.observe_changes(true);` then 300 steps driven by `xorshift(seed)`:
  - *batch with ties:* pick 1-4 distinct clipped offsets, ascending; at each push
    `EditOp::insert(p, "A")`, sometimes also `EditOp::insert(p, "Bé")` (a tie), sometimes
    also `EditOp::delete(p..q)` where `q = d.buffer().clip_offset(p + 1, Bias::Right)` and
    `q` stays below the next chosen offset (inserts before the range at one start, never after —
    `apply` rejects a range followed by an insert at its start as overlap). `d.edit(batch)`;
  - *typing run:* `d.type_char` 1-5 times (one merged undo element);
  - *undo* / *redo*;
  - *drain:* with probability 1/3, `assert_chain(&d.drain_changes(), &d)`; otherwise keep
    accumulating, so some drains cover many commits.
  End with a final drain + `assert_chain`.
- **`tied_inserts_replay_in_the_order_the_rope_applied_them`** — `doc("")`, observe,
  `d.edit(vec![EditOp::insert(0, "A"), EditOp::insert(0, "B")])`; text is `"AB"`; the one
  entry's `ops()` are `[insert(0,"B"), insert(0,"A")]`; `replay("", ops) == "AB"`. The assert
  message names the old failure: "a Reverse(start) sort would replay this as BA".
- **`undo_and_redo_of_a_typing_run_log_one_entry_per_step`** — `doc("")`, type `a b c` with
  `type_char` (one merged element), drain and discard, `d.undo()`: drained `len() == 3`,
  `assert_chain`, text `""`. `d.redo()`: `len() == 3`, `assert_chain`, text `"abc"`.
- **`observing_starts_the_chain_at_the_current_revision`** — edit twice unobserved, then
  `observe_changes(true)`: an immediate drain is empty with `from() == Some(d.revision())`; one
  more edit drains as one entry whose `before().revision()` is that revision. Then
  `observe_changes(false)`: drain is empty with `from() == None`, and further edits log nothing.
- **`hitting_the_cap_breaks_the_log_until_the_next_drain`** — observe, 1025 single-insert edits
  with no drain: the drain has `from() == None` and `is_empty()`. One more edit: the next drain
  has `from() == Some(rev before that edit)`, `len() == 1`, and passes `assert_chain`.
- **`doc_id_is_the_buffer_identity`** — `d.doc_id() == d.buffer().doc_id()` and
  `== d.snapshot().doc_id()`, and it survives `d.edit(vec![EditOp::new(0..len, "x")])`.
- **`select_and_reveal_unfolds_the_target_and_requests_a_centered_reveal`** — the
  `folded_doc` shape from editor.rs tests: `let text = "x\na {\nb\nc\n}\nword\n"; let mut d =
  doc(text); assert!(d.toggle_fold_opener(text.find('{').unwrap() as u32));` Record
  `seq0 = d.reveal_seq()`. `let b = text.find("b\n").unwrap() as u32;
  d.select_and_reveal(b..b + 1);` Assert `d.folds().is_empty()` ("a reveal into a fold unfolds
  it"), `d.reveal_seq() > seq0`, `d.reveal_mode() == RevealMode::Center`, and the newest
  selection spans `b..b + 1` with head `b + 1`.
- **`select_and_reveal_clamps_and_snaps_to_char_boundaries`** — `doc("aé")`:
  `select_and_reveal(2..99)` selects `1..3` (start snapped left out of `é`, end clamped);
  `select_and_reveal(3..1)` selects `1..3` (inverted normalized).

### code_editor.rs (`mod tests`)

Replace `drain_changes_mirrors_edits_when_observing` (code_editor.rs:2004-2016) with:

```rust
    /// The change log (LSP `didChange`) is off by default and, once enabled,
    /// logs one entry per commit and drains clean. Full-sync hosts never touch it.
    #[test]
    fn drain_changes_mirrors_edits_when_observing() {
        let mut ed = CodeEditor::new("hello\n");
        let off = ed.drain_changes();
        assert!(off.is_empty() && off.from().is_none(), "the change log is off by default");
        ed.observe_changes(true);
        let _ = ed.update(Event::Editor(Action::Type('X')), Instant::now()); // insert 'X' at the caret (offset 0)
        let changes = ed.drain_changes();
        assert_eq!(changes.len(), 1, "one keystroke logs one commit");
        assert_eq!(changes.doc_id(), ed.document().doc_id(), "the drain names its document");
        let entry = changes.iter().next().expect("one entry");
        assert_eq!(entry.ops()[0].text, "X");
        assert_eq!(entry.before().text(), "hello\n", "the entry carries the pre-edit text");
        assert!(ed.drain_changes().is_empty(), "draining clears the log");
    }
```

## 7. Verification

```
cargo clippy --workspace --all-targets -- -D warnings      # after Step 0 alone, then again at the end
cargo test --workspace
cargo test -p scrive-core -- change_log select_and_reveal snapshot_ doc_id
cargo test -p scrive-iced -- drain_changes
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
cargo build --workspace --all-targets --target wasm32-unknown-unknown
out=$(cargo tree -p scrive-core -e normal --prefix none) || exit 1; if grep -qE '^(iced|winit|wgpu)' <<<"$out"; then echo LEAK; exit 1; fi
```

(`--all-features` variants are identical this phase: no feature exists yet.)

## 8. Spot checks

Change-log replay (ops shown in logged order; replay applies left to right):

| Before | Commit (caller order) | Logged `ops` | Sequential replay | Result |
|---|---|---|---|---|
| `""` | `ins 0 "A"`, `ins 0 "B"` | `ins 0 "B"`, `ins 0 "A"` | `"B"` → `"AB"` | `"AB"` ✓ (the old `Reverse(start)` order gives `"BA"`) |
| `"abcd"` | `ins 2 "X"`, `del 2..4` | `del 2..4`, `ins 2 "X"` | `"ab"` → `"abX"` | `"abX"` ✓ |
| `"abcd"` | `del 2..4`, `ins 2 "X"` | — | — | rejected by `apply` (`Overlap`), nothing logged |
| `"a b a"` | `0..1 "X"`, `4..5 "Y"` | `4..5 "Y"`, `0..1 "X"` | `"a b Y"` → `"X b Y"` | ✓ |
| `"abc"` (typed as one run, rev 3) | undo | 3 entries: `del 2..3` (before rev 3), `del 1..2` (rev 4), `del 0..1` (rev 5) | `"ab"`, `"a"`, `""` | ends at rev 6 ✓ |
| any | 1025 commits, no drain | drain: `from None`, empty | — | broken; next drain resumes |

Snapshot conversions on `"ab\ncd\n"` (len 6, 3 lines):

| Call | Result |
|---|---|
| `offset_to_point(4)` | `(1, 1)` |
| `offset_to_point(99)` | `(2, 0)` |
| `point_to_offset((1, 1))` | `4` |
| `point_to_offset((0, 99))` | `2` (column clamps to the line end) |
| `point_to_offset((9, 0))` | `6` (row clamps to the last line — LSP would want doc end; same here by coincidence) |
| `point_to_offset((9, 5))` on `"ab\ncd"` | `5` (row clamp keeps the column: `(1, 5)` → clamped `(1, 2)`) |

`select_and_reveal` on `"aé"` (`é` = bytes 1..3):

| Range | Selection |
|---|---|
| `2..99` | `1..3` |
| `3..1` | `1..3` |
| `0..0` | caret at 0 |

## 9. What NOT to change

- No scrive-lsp code, no LSP types, no `lsp-types`/`serde` dependency in scrive-core.
- Do not touch lib.rs: `Change`/`Changes` are reached as `scrive_core::document::…`.
- Do not change `transaction::apply`, `history.rs`, the rope, or `Rope::chunk_at`.
- Do not change `step_find`/`next_diagnostic` behavior — only route them through `jump_to`.
- Do not make `request_reveal` public; `select_and_reveal` is the public entry.
- Do not add a `CodeEditor::select` (Phase 3) or anything ticket-related (Phase 2).
- Never run `cargo fmt`. Do not reformat lines you did not change.
- Do not rename existing public items.

## 10. Known pitfalls

- **Destructuring `Self` in undo/redo.** `changes` must be *in* the `let Self { … } = self;`
  pattern; calling `self.changes` inside the callback while `history`/`buffer` are borrowed from
  the destructure is E0499/E0500. Take `before` from the destructured `buffer`
  (`changes.capture(buffer)`) *before* passing `buffer` into `history.undo`. Inside the callback
  use the callback's own `buffer: &Buffer` parameter (it shadows the outer one). Everything
  after the `history.undo(...)` call may use `self` again because the closure has been consumed.
- **Only `edit_grouped` sees the batch; `record` must see `forward_ops()`, not the caller's
  `ops`.** The caller's ops are unclipped and unsorted; `committed.forward_ops()` is the
  normalized batch. Read it before `committed.into_ops()` moves it into history (the new code
  sits where the old logging did, before `into_ops`).
- **An empty or no-op batch does not bump the revision and must not log.** The capture happens
  before `apply`, but `record` only runs inside `if !committed.is_empty()`; the unused snapshot is
  just dropped.
- **`Snapshot::chunks` returns `impl Iterator + '_`.** It borrows the snapshot; callers that
  store it need the lifetime. No `Box`, no `Vec`.
- **Doc links must resolve inside scrive-core.** Link `[`Document::observe_changes`]`,
  `[`Changes::from`]`, `[`Buffer::clip_offset`]`; never link to `CodeEditor`. `cargo doc` runs
  with `-D warnings`.
- **clippy:** `Changes` has `len` so it needs `is_empty` (`len_without_is_empty`); `iter` must
  not be named `into_iter`; `ChangeLog` derives `Default` so no `new` is needed. `then_some`
  (not `then(|| …)`) for the `from` of `set`, `then(|| …)` for the snapshot in `capture`
  (it must stay lazy).
- **`Changes::from(&self)`** is the plan's name (D9's chain check reads `changes.from`). It takes
  `&self`, so clippy's `should_implement_trait` (which targets a self-less `fn from(T) -> Self`)
  should not fire; if a clippy version does flag it, allow it on that method with a one-line
  justification rather than renaming.
- **`unfold_to_reveal` returns `bool`.** Calling it for its effect inside `jump_to` without using
  the result is fine (it is not `#[must_use]`); do not add `let _ =`.
