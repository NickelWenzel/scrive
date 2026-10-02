# Phase-doc open questions — resolutions (orchestrator, 2026-10-01)

Binding for every MAP_PHASE_N.md. Where a phase doc disagrees, this file wins; the audit pass
edits the docs to match. User decisions are marked **(user)**.

## Shared API (names every phase must use)

R1. **Hint accessors (Phase 1).** Read-only getters on `Hint`: `kind()`, `parts() -> &[Part]`,
    `padded() -> Padding`, `insertable() -> bool`, `key() -> Key`, `width() -> u32` (stored at
    construction, D5). On `Part`: `text() -> &str`, `link() -> Link`. Setters stay `.padding(..)`,
    `.insert(..)`, plus a new `.placement(Placement)` (R5).
R2. **Laid-out hints for painting (Phase 3).** `RowLayout::inlays() -> impl Iterator<Item = &row_layout::Inlay>`
    with `row_layout::Inlay { key, offset, cell, width, padding, hint: &Hint }` (`cell` = first
    display cell including left padding). Phase 4 paints from this only.
R3. **Hit result (Phase 3, used by Phase 5).** `Rows::inlay_at(row, cell) -> Option<inlay::At>`,
    `enum At { Label { key, part: u32, offset, link: Link, insert: Insert, cells: Range<u32> },
    Padding { key, offset } }`. `Padding` is inert: no hover, no link, no fall-through to the word
    (D7); clicks still use `hit`.
R4. **Render offset (Phase 1).** `Anchor::render_offset(&self, range: Range<u32>, row_start: u32,
    row_end: u32, line: &str) -> Option<u32>` (`None` = doesn't render on this row).
R5. **Placement.** `Hint::new(kind, label, key)` defaults placement from kind (`Type` → `Suffix`,
    `Parameter` → `Prefix`, `Other` → `Auto`); `.placement(Placement)` overrides. The **client**
    computes `Other` placement from the **raw** server padding (Zed's rule, D1) before collapsing
    padding (D16), and passes it with `.placement`. Core resolves `Auto` at install (D1).
R6. **Interaction.** Phase 1's shape is adopted: `Interaction { ticket, key, gesture }`,
    `Gesture::{Tooltip { part: u32 }, Jump { part: u32 }, Insert { offset: u32 }}`. Tooltip always
    has a part (padding isn't hoverable), so the D12 slot is `inlay_tooltip: Option<(Ticket, Key, u32)>`
    and Phase 5's `Editor::inlay_tooltip` takes `(Key, u32, &str)`. The client falls back from the
    part's tooltip to the hint's (Phase 6 Q6: yes).
R7. `Action::InlayHover { key, part }`: the unused `offset` field is dropped.
R8. **Borrowing visitor (Phase 1).** Phase 1 adds the borrowing row query in decorations.rs (and the
    `filter_visit` lifetime change in sum_tree.rs if needed); Phase 3 only uses it. Add sum_tree.rs to
    Phase 1's file list if touched.
R9. **Scheduling entry.** Phase 5's `CodeEditor::wait_inlays(delay, cap)` (private) is the one
    scheduler; Phase 6's `InlayRefresh` arm calls `self.wait_inlays(INLAY_EDIT_DELAY, None)`. Phase 5
    keys the edit trigger on a stored revision and also calls it from `accept_completion` and `load`
    (they bypass `after_edit`).
R10. **Test hook for hosts/examples (Phase 5).** `CodeEditor::pending_wake() -> Option<Wake>`, public
     and documented (hosts with their own timers can use it); Phase 7's example tests fire
     `Action::Wake(generation)` from it.

## Behaviour

R11. **Render guard removed.** It hides correct hints after typing (Zed's `||X -> fn()<…>f`), and the
     only case it covered (two touching multi-cursor edits splitting one anchor) is rare and repaired
     by the refetch. D3's whole-anchor drop rule stays. Update MAP_PLAN D3, the expert-round logs stay
     as history.
R12. **Box selection steps by character everywhere** within content, by cell past EOL **(user)**.
R13. **Toggle key Ctrl+I** (`command()` modifier, Cmd+I on macOS) in both examples **(user)**.
R14. **Pill + dimmed text** as planned, chip pill colour and rounding, label alpha 0.6, pill hidden
     only inside selection washes (strict interior) **(user)**. On a washed hint keep the dimmed
     colour (Phase 4 Q4: confirmed). Find/scope washes keep the pill (Q5: confirmed).
R15. **Squiggle continuation rows** start at `End` of the row start (strictly interior hints are
     underlined); zero-width interior rows stay skipped (Phase 4 Q2, Q3: confirmed).
R16. **Declined or empty insert.** An `Edits(vec![])` answer whose ticket matches `inlay_insert`
     settles the slot and returns without `try_edit` (no hint removal, no popup close).
R17. **Stale/not-running gestures** answer the empty answer for the action so the slot settles
     (D18 as written); `interact` declines when the server isn't running; refresh fan-out only while
     running; a failed resolve does not mark the hint resolved; no location-hover fallback without
     `hoverProvider` (Phase 6 Q3, Q4, Q8, Q9, Q10).
R18. **Stable key identity** = position, kind and label part texts, ordered multiset, same revision
     only (Phase 6 Q7). "Sort" test = the client does not reorder (Q12).
R19. **`open_lsp` wait-0 trigger** belongs to Phase 7 (lsp.rs). `close_lsp` also clears the pending
     wait (Phase 7 Q5).
R20. **Window in folded viewports:** pads are counted in display rows and converted to buffer rows;
     hidden fold interiors inside the window are still requested (bounded by span clipping). "Inner
     half" = the window shrunk by half of each pad (Phase 5 Q7, Q8: confirmed).
R21. Phase 5's other defaults are confirmed: private `HoverTarget` + `Editor::inlay_tooltip`; a
     refetch without the card's key closes it; `command()` modifier; `set_inlay_hints(true)` while on
     is a no-op.

## Phase 2

R22. Confirmed as proposed: `_inlays` field and `_edge` params until Phase 3 consumes them (OQ-1, 2);
     `RowLayout::display_cell` takes `Edge` in Phase 2 (OQ-3); keep `header_layout`/`hit_row` in use,
     delete `display_position` (OQ-4); field name `inlays` (OQ-5); OQ-6 corrections, OQ-7 extra
     helpers, OQ-8 `column_box(&Rows, col) -> SelectionSet` adopted; perf.rs / perf_gate.rs prose
     updated (OQ-9); no top-level re-export (OQ-10).

## Plan fixes

R23. MAP_PLAN D11: drop the stale `due: Option<u64>` from the `Inlays` struct (pending wait instead).
R24. MAP_PLAN line drift: chips' span test is editor.rs:3134; the focus gate is editor.rs:2573.

## Audit follow-ups

R25. **Hint-free row layout for visibility probes:** `RowLayout::new` takes
     `Option<&DecorationStore>`; `FoldMap::row_layout` passes `None` (it runs inside `rebase_views`
     while the inlay store is borrowed mutably); `FoldMap::hit_row` is deleted once it is test-only
     (Phase 3). Confirmed.
R26. `intel/inlay.rs` re-exports `Request` and `Interaction` (`pub use`), so callers write
     `inlay::Request` / `inlay::Interaction` as the plan does. Confirmed (RUST_STYLE has no rule
     against it; the types still live one per module).

## rust-analyzer probes (2026-10-01, `02dede3ce5`, every hint kind on; scripts in the session scratchpad)

R27. **`Auto` side rule revised (D1, Phase 1).** `Suffix` when a word char precedes `p`, or the char
     at `p` is whitespace, end of line/buffer, or one of `) ] } , ; .`; otherwise `Prefix`. The old
     rule ("Prefix only when a word starts at p") put every adjustment before `& * ( " [ |` and the
     binding-mode `&` before `(x, y)` on the wrong side. Checked against all 137 hints in all four
     adjustment modes; the only miss is range-exclusive `<` at `0..‸10` (off by default, renders the
     same). Phase 1 tests: `&*` at `‸&s` → Prefix, `<'0>` at `foo‸(` → Suffix, `= 0` at `A‸,` →
     Suffix, `drop(w)` at `}‸`(EOL) → Suffix, `&` at `let ‸(x, y)` → Prefix, `<fn-item-to-fn-pointer>`
     at `||‸f` → Prefix. Server order matched visual order in every co-located group (default modes).
R28. **Request span (D11, D16, Phase 5/6).** Closing-brace hints come back whenever the range
     covers the `}` (no top-padding change); chaining hints are dropped when the range starts inside
     the chain (already covered by the top pad). The span runs from the start of the first window
     row to the end of the last window row **including its last char** (the start of the next row,
     or the buffer end), and the client **clamps the end to the buffer end**: rust-analyzer answers
     `-32603 "Invalid offset LineCol"` for an end past the last line instead of clamping. Phase 6
     test: a window ending past the last line sends an end at the buffer end.
