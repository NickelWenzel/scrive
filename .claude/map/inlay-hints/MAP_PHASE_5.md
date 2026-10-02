# Phase 5 — iced: gestures, the request seam, the toggle

This doc specifies Phase 5 of `.claude/map/inlay-hints/MAP_PLAN.md` (Draft 7) and stands on its own:
it restates every design rule the phase implements. Line numbers are HEAD `8e72665` (branch
`lsp_bridge`). Phases 2–4 move code in editor.rs (every geometry site takes a `&Rows` there), so
**locate each site by the function, arm or comment named**, not by the number.

## 1. Prerequisites

- **Phases 1–4 are committed** and `cargo test --workspace --all-features` is green.
- Read in full before writing code:
  - `~/.claude/guides/RUST_STYLE.md`, `~/.claude/guides/OPAQUE.md`;
  - the `/iced` and `/commit-and-comment` skills;
  - `.claude/map/lsp-bridge/DISPATCH.md` (its overrides apply: comments per /commit-and-comment,
    no commits, patches and `.msg` files at each boundary, under `.claude/map/inlay-hints/patches/`);
  - `crates/scrive-iced/src/editor.rs` and `crates/scrive-iced/src/code_editor.rs` as Phases 2–4
    left them;
  - `crates/scrive-core/src/intel/inlay.rs`, `intel/inlay/request.rs`,
    `intel/inlay/interaction.rs` (Phase 1) and the `Rows` / `inlay_at` part of
    `crates/scrive-core/src/row_layout.rs` (Phases 2–3).
- **Confirm the seam this phase consumes** (all from scrive-core; nothing from scrive-lsp). The
  names below are the ones MAP_PHASE_1–4 specify after the RESOLUTIONS.md audit.

  | Item | From | Used for |
  |---|---|---|
  | `intel::inlay::Key` (`Copy + Eq + Debug`, minted by the host) | Phase 1 | action payloads, slots, the card's identity |
  | `intel::inlay::{Hint, Part, Kind, Link, Insert, Padding, Placed}` | Phase 1 | `set_inlays` input; test fixtures |
  | `Placed::hint() -> &Hint`, `Hint::key() -> Key`, `Key::new(u64)` | Phase 1 | closing a card whose key left the set (§3 Decision 7); tests |
  | `intel::inlay::Request::new(ticket, span: Range<u32>)` (module `intel::inlay::request`) | Phase 1 | the fetch request |
  | `intel::inlay::Interaction::{tooltip(ticket, key, part: u32), jump(ticket, key, part: u32), insert(ticket, key, offset: u32)}`, `ticket()` (R6) | Phase 1 | the gesture slot |
  | `Document::{set_inlays(revision, Vec<Placed>) -> inlay::Outcome, clear_inlays(), inlays_revision() -> Option<Revision>}` | Phase 1 | install, clear, the D14 gate |
  | `Document::rows() -> Rows`, `Rows::folds()`, `Rows::inlay_at(row: BufferRow, cell: f32) -> Option<inlay::At>` with `At::{Label { key, part, offset, link, insert, cells }, Padding { key, offset }}` (R3) | Phases 2–3 | every hint hit test |
  | `hit_test`, `collapsed_chip_at`, `popup_anchor(rows, geo, offset, edge)` taking `&Rows` | Phase 2 | press, hover and card code |
  | `INLAY_TEXT_A` (the label alpha; the label colour is `Color { a: INLAY_TEXT_A, ..text_color }`) | Phase 4 | the link underline |

- Verify the HEAD sites this doc cites (they drift after Phases 2–4; find them by name):

  | Site | HEAD `8e72665` |
  |---|---|
  | `Action` enum / `moves_caret` | editor.rs:223-391 / 402-442 |
  | `State` / `impl Default for State` | editor.rs:469-559 / 561-592 |
  | `Editor` struct / `new` / `hover` / `hover_pending` builders | editor.rs:645-667 / 672-686 / 714-717 / 722-725 |
  | `popup_anchor` | editor.rs:815-825 |
  | `Widget::diff` | editor.rs:1217-1236 |
  | hover card draw call | editor.rs:1963-1966 |
  | `ViewportChanged` publish (top of `update`) | editor.rs:2011-2021 |
  | press order (`ButtonPressed(Left)` arm) | editor.rs:2024-2166; chip 2098-2108; Ctrl collapse 2109-2123; `hit_test` + `mouse::Click` 2124-2164 |
  | hover arming on `CursorMoved`, `still_in` | editor.rs:2248-2287, 2263-2266 |
  | wheel over the hover card | editor.rs:2315-2331 |
  | `ModifiersChanged` arm | editor.rs:2523-2530 |
  | `RedrawRequested` arm; the `is_focused()` hover gate | editor.rs:2563-2607; 2573 (the plan's "2571" is the comment above it) |
  | `mouse_interaction` | editor.rs:2612-2676 |
  | `hover_layout` / `draw_hover` / `HoverLayout` | editor.rs:2729-2762 / 2768-2812 / 3812-3827 |
  | test helpers `headless_renderer`, `pump`, `one_word_doc`, `rest_on` | editor.rs:4651-4706 |
  | `Awaiting` / `Awaited` | code_editor.rs:293-302 / 315-321 |
  | `try_edit` (runs `after_edit`) | code_editor.rs:546-554 |
  | `load` | code_editor.rs:561-576 |
  | builders `find` / `rename` | code_editor.rs:434-445 |
  | `take_*` / `set_*` pairs | code_editor.rs:614-714 |
  | `update`: `now_ms` stamp; `ViewportChanged`; `HoverQuery`; `HoverDismiss`; catch-all `apply` | code_editor.rs:751-753; 759-780; 857-877; 878-882; 913-916 |
  | `view` (`.hover`, `.hover_pending`) | code_editor.rs:1129-1169 |
  | `apply` (no-op list 1505-1522; ends in `after_edit` 1524) | code_editor.rs:1447-1530 |
  | `after_edit` (abandons Hover and Definition at 1555-1558) | code_editor.rs:1537-1566 |
  | `accept_completion` (its own tail, **no** `after_edit`) | code_editor.rs:1699-1787 |
  | `accepts` / `abandon` | code_editor.rs:1792-1800 / 1804-1823 |
  | `apply_lsp` (no `now`) | code_editor/lsp.rs:121-129 |

## 2. Goal and exit criteria

**Goal.** The widget turns gestures on hint cells into actions and gains a general, documented
timer (`Editor::wake_after` → `Action::Wake`). `CodeEditor` gets the hint toggle, the fetch
scheduler (pending waits, triggers, the request window), the three new `Awaiting` slots, the
`take_inlay_request` / `take_inlay_interaction` / `set_inlays` / `set_inlay_tooltip` seam, the
keyed tooltip card, and the D14 gate. Nothing here talks to scrive-lsp: Phase 6 adds the client and
the three `land` arms, Phase 7 the `sync_lsp` pull.

**Exit.**
1. The widget's timer (editor.rs tests):
   - `a_wake_fires_once_after_its_delay`
   - `a_new_generation_restarts_the_delay`
   - `a_wake_counts_its_delay_from_the_first_frame_that_sees_it`
   - `a_capped_wake_fires_at_the_max_wait_while_generations_keep_changing`
   - `a_one_second_scroll_drag_wakes_about_three_times`
   - `wake_after_none_restarts_the_max_wait_clock`
   - `a_wake_fires_while_the_editor_is_unfocused`
   - `rendering_another_document_resets_the_wake_timer`
2. Gestures on hint cells (editor.rs tests):
   - `resting_on_a_hint_label_publishes_inlay_hover_not_hover_query`
   - `resting_on_hint_padding_queries_nothing`
   - `the_inlay_card_stays_open_while_the_pointer_stays_on_its_part`
   - `hint_hover_arms_only_while_focused`
   - `ctrl_click_on_a_link_part_publishes_inlay_jump_and_captures`
   - `ctrl_click_on_a_part_without_a_link_places_the_caret`
   - `ctrl_over_a_link_part_shows_the_pointer_except_during_a_drag`
   - `a_single_click_on_a_hint_places_the_caret_at_its_offset`
   - `double_click_on_an_insertable_hint_publishes_inlay_insert`
   - `double_click_on_a_hint_without_edits_places_the_caret_without_selecting`
   - `gestures_on_a_stale_hint_set_do_nothing`
3. The seam (code_editor.rs tests):
   - `hints_are_off_by_default_and_ask_for_nothing`
   - `enabling_hints_waits_zero_and_a_wake_records_a_request_for_the_window`
   - `the_request_window_pads_one_view_above_and_two_below`
   - `a_wake_for_an_old_generation_records_nothing`
   - `an_edit_waits_three_hundred_ms_and_each_edit_restarts_it`
   - `accepting_a_completion_schedules_a_fetch`
   - `a_refresh_wait_is_a_delay_not_a_deadline`
   - `scrolling_inside_the_inner_window_asks_nothing`
   - `scrolling_out_of_the_inner_window_waits_with_a_cap_and_re_requests`
   - `triggers_are_ignored_while_disabled`
   - `set_inlays_lands_only_under_the_awaited_ticket`
   - `a_failed_fetch_keeps_the_shown_hints`
   - `an_empty_answer_clears_the_hints`
   - `a_wake_keeps_the_completion_popup_open`
   - `inlay_actions_never_reach_apply`
   - `inlay_hover_records_a_tooltip_interaction_and_the_answer_shows_a_keyed_card`
   - `the_tooltip_card_survives_a_refetch_that_keeps_its_key`
   - `a_refetch_without_the_key_closes_the_tooltip_card`
   - `inlay_jump_awaits_through_the_definition_slot`
   - `inlay_insert_records_an_insert_interaction`
   - `a_newer_gesture_supersedes_the_pending_interaction`
   - `interactions_on_a_stale_set_record_nothing`
   - `disabling_hints_clears_the_store_the_slots_the_card_and_the_wait`
   - `hover_dismiss_retires_the_inlay_tooltip`
   - `scrolling_retires_the_inlay_tooltip`
   - `an_edit_retires_the_inlay_tooltip_and_insert`
   - `load_clears_the_hints_and_waits_zero`
4. clippy (`-D warnings`, both feature sets), both doc builds, the wasm all-features build and the
   whole suite are green. Every new `Awaiting` field is read in non-test code.

## 3. Design decisions implemented

Restated from the plan; they are binding.

**D1 / D4 (what this phase relies on).** A hint carries no offset. Installing takes
`Vec<inlay::Placed>` (`Placed { offset, hint }`, private fields); the store owns the position
afterwards. `Document::set_inlays(revision, placed)` replaces the whole store when `revision ==
doc.revision()`, else returns `Stale` and changes nothing. `clear_inlays()` empties it.
`inlays_revision()` is the revision the current set was installed at. Keys are opaque to core and
minted by the host; the client keeps a hint's key across refetches when position, kind and label
match (Phase 6), which is what lets an open card survive a refresh.

**D7 (what this phase relies on).** `Rows::inlay_at(row, cell)` returns `inlay::At::Label` under a
label part and `inlay::At::Padding` over padding (R3). Padding is **inert**: it is editor
background (the LSP spec), so it is neither a hover target nor a link target, **and it doesn't
fall through to the word either** (Zed, inlay_hints.rs:686-729). A click on any hint cell (label or
padding) maps to the hint's offset through `hit`, so the caret renders on the canonical side.

**D9 (card anchor).** The inlay tooltip card anchors on the hovered part's own cells, not at
`popup_anchor`'s `Start` edge. Word cards keep `popup_anchor(range.start)`.

**D10 — Gestures.**
- Hint hover, like word hover, arms only while the widget is focused (the `is_focused()` gate in
  the `RedrawRequested` arm). Clicks work focused or not.
- **Hover.** When the idle timer fires, check the collapsed chip first (fold preview), then
  `inlay_at`, then the word. A hint hit publishes `Action::InlayHover { key, part }` (R7: no
  offset) instead of `HoverQuery`. The tooltip card has a keyed identity (`HoverTarget::Inlay { key, part }`
  beside today's range). `still_in` holds while the pointer stays on the same hint part or on the
  card. The card anchors on the part's cells. `HoverDismiss` retires both kinds of card.
- **Ctrl+click** on a link part publishes `Action::InlayJump { key, part }` and captures the event.
  While Ctrl is held over a link part, the part is underlined and the cursor is a pointer, unless a
  drag selection is pending (Zed's `!has_pending_nonempty_selection()` guard,
  inlay_hints.rs:792-810), so a Ctrl-drag neither underlines nor captures. The link test runs
  before the compiled-out Ctrl collapse affordance (`SHOW_CTRL_COLLAPSE_AFFORDANCE`).
- **Double-click** on an insertable hint publishes `Action::InlayInsert { key, offset }` instead of
  selecting a word. The press still goes through `mouse::Click::new`, so the second press counts as
  a double. A single click places the caret at the hint's offset (the plain path).
- Hint text is never a word for double-click, find or Ctrl+D: it is not in the buffer.

**D11 — The editor owns scheduling.**
- `CodeEditor` keeps the scheduler state: enabled, the pending wait, and the requested window (in
  buffer rows).
- **Window:** the visible rows padded by one viewport height above and two below, minimum 50 rows
  (Helix's shape, commands/lsp.rs:1362-1372). Chains that start above the visible top still get
  their hints, and scrolling down, the common direction, stays inside the window longer. Sent as a
  byte span.
- **Triggers**, all ignored while disabled:

  | Trigger | Wait |
  |---|---|
  | enabling (`inlay_hints(true)`, `set_inlay_hints(true)`) | 0 |
  | an edit | 300 ms, trailing (each edit restarts it), no cap |
  | `InlayRefresh` (Phase 6's `land` arm) | the same as an edit |
  | `ViewportChanged` whose rows leave the requested window's inner half | 75 ms, trailing, cap 300 ms |
  | `load` | clear the store (D13), then 0 |
  | `open_lsp` | 0 — Phase 7 adds it to lsp.rs (R19) |

  The trailing 75 ms with a 300 ms max-wait keeps a scrollbar drag from superseding and cancelling
  every request before it answers.
- **Delays, not deadlines.** `apply_lsp` takes no `now` (lsp.rs:121-129), and `now_ms` is stamped
  only in `update` (code_editor.rs:751-753). An absolute `due = now_ms + 300` set from
  `InlayRefresh` in an editor idle for minutes would already be past, and rust-analyzer refreshes
  after every `didChange`, so every visible editor would fetch on every keystroke. So `CodeEditor`
  keeps a **pending wait** `{ generation, delay, cap }`, and each trigger bumps the generation.
- **Timer.** No frame subscription. A general public widget API: `Editor::wake_after(Option<Wake>)`
  with `Wake { generation, delay, cap: Option<Duration> }`, and `Action::Wake(u64)`.
  - Only `ViewportChanged` sets a cap. Edits and refreshes don't, so an edit during a drag doesn't
    fire early.
  - On `RedrawRequested(now)` the widget **first** restamps when the generation is new: `at = now
    + delay`, capped at `first_seen + cap`, from its own clock (the `hover_rearm` pattern,
    editor.rs:2570-2579). **Then** it checks `now ≥ at`.
  - While waiting it calls `shell.request_redraw_at(at)`. On the redraw that crosses `at` it
    publishes `Action::Wake(generation)` once.
  - `first_seen` survives generation changes. It resets only when the widget publishes a `Wake` or
    receives `wake_after(None)`, so a drag that bumps the generation on every event still hits the
    max-wait.
  - `diff` resets the wake fields when the document changes (editor.rs:1217-1236); they are not
    kept like `focus`.
  - It arms **outside** the hover code's `is_focused()` gate.
  - `update` records `inlay::Request { ticket, span }` when the woken generation matches the
    pending one. The host's `sync_lsp` after `update` sends it on the same message (Phase 7).
  - `Wake` is not inlay-specific; find could use it for a real debounce later.
- **Limits, documented** on `wake_after`: a widget that isn't in the view tree never wakes, so a
  background tab's fetch waits until it is shown, and the first draw re-arms it. A minimised or
  occluded window may get no `RedrawRequested` (winit), so the fetch waits until it is visible.

**D12 — Slots and answers.**
- New `Awaiting` fields: `inlays: Option<Ticket>`, `inlay_tooltip: Option<(Ticket, Key, u32)>`
  (R6: a tooltip always has a part), `inlay_insert: Option<(Ticket, Key, u32)>`. Label jumps reuse
  `awaiting.definition`, so `Local`, `Open` and `Unopened` targets and `Applied.jump` work unchanged.
- `abandon` gains the new kinds. Abandon table additions:

  | Event | Clears |
  |---|---|
  | disabling hints, `close_lsp` (Phase 7) | `inlays`, `inlay_tooltip`, `inlay_insert` |
  | `HoverDismiss`, `ViewportChanged` | `inlay_tooltip` |
  | an edit | `inlay_tooltip`, `inlay_insert` |

- `take_inlay_request() -> Option<inlay::Request>`; `take_inlay_interaction() ->
  Option<inlay::Interaction>` (one slot: a newer gesture supersedes).
- `set_inlays(ticket, Option<Vec<inlay::Placed>>)`: `Some` replaces (`Some(vec![])` clears);
  `None` settles the slot and keeps what is shown, because a failed fetch must not blank the hints.
- `set_inlay_tooltip(ticket, Option<String>)` shows the markdown as a card anchored at the hint part.
- `InlayHover`, `InlayJump`, `InlayInsert` and `Wake` are handled in `update` **before** the
  catch-all `apply` (code_editor.rs:913-916), as `GotoDefinition` and `TriggerCompletion` already
  are. `apply` always ends in `after_edit(CaretOrClose)` (code_editor.rs:1524), which closes the
  completion popup, clears `self.hover` and abandons `Hover` and `Definition`. A falling-through
  `Wake` would close completion 300 ms after every typing pause, and `InlayHover` / `InlayJump`
  would lose their slots. In `apply`'s exhaustive no-op list they are unreachable.
- Every new `Awaiting` field is read in non-test code, so none is write-only under `-D warnings`
  (DISPATCH forbids `#[allow(dead_code)]`): `accepts` gets an arm per new `Awaited` variant, and
  the edit and abandon paths construct them.

**D13 — Toggle.** `CodeEditor::inlay_hints(bool)` (builder, default off, like `rename`) and
`set_inlay_hints(&mut self, bool)` (runtime). Off clears the store, abandons the slots, closes a
showing inlay tooltip card and clears the pending wait. There is no built-in key; the examples bind
one (Phase 7). `load` replaces the whole buffer, and the mover would keep hints at their old
relative offsets (patch.rs:206-211), so `load` calls `clear_inlays` too.

**D14 — Interactions only on a current set.** Hints move with edits, but the host's copy of their
tooltips, locations and edits is valid only at the revision it was fetched for. The editor records
`Interaction`s only while `doc.inlays_revision() == Some(doc.revision())`. On a moved set, gestures
on hint cells do nothing:
- no hover (and not the neighbouring word's);
- no jump (the Ctrl+click is still consumed);
- double-click places the caret at the hint's offset without selecting.

**Decisions this doc makes where the plan is silent** (each also listed in §12 where it is more
than mechanical):

1. **Ctrl is `modifiers.command()`** (Ctrl on Linux/Windows, Cmd on macOS), like the existing Ctrl
   collapse affordance. Shift or Alt with it falls through to the normal paths. (R21)
2. **The drag guard is "any drag in progress"**: `state.drag.is_some() ||
   state.column_drag_anchor.is_some()`. The widget never sees the host apply `DragSelect`, so it
   can't test the selection for emptiness. A press on a link with Ctrl held is captured before a
   drag is armed, so the only drags that reach a link are ones started elsewhere.
3. **`still_in` for a hint is widget-local.** The widget remembers the part it queried
   (`State::inlay_hover`), so a move inside that part keeps an unanswered tooltip query, as
   `hover_pending` does for words, with no host input.
4. **The card's geometry is the queried part's**, recorded at query time (buffer row and cell
   range). Scrolling closes every card (`ViewportChanged`), and an edit closes it (`after_edit`), so
   the record can't go stale while a card shows.
5. **Double-click on a hint:** an insertable label on a current set publishes `InlayInsert` and
   arms no drag. Every other hint cell (padding, a label without edits, any cell of a stale set)
   publishes `PlaceCaret(hit offset)` and arms a `Char` drag, exactly like a single click. A triple
   click keeps selecting the line.
6. **The edit trigger keys on the revision.** `Inlays::seen` holds the last revision the scheduler
   saw. `after_edit` compares it, and so does `accept_completion`, which commits an edit without
   running `after_edit`. `load` handles its own trigger. A caret move or `select` doesn't move the
   revision and triggers nothing.
7. **A refetch that drops the shown card's key closes the card.** A refetch that keeps the key
   leaves it open. (R21)
8. **Inner half** (R20): the requested window shrunk by half of each pad. The window is
   `start..end` around visible rows `vis`; the inner rows are `start + (vis.start − start)/2 ..
   end − (end − vis.end)/2`. A `ViewportChanged` re-requests when its rows aren't inside them. At
   the top or bottom of the document a pad is 0, so that edge never triggers. The pads (and the
   50-row minimum) are counted in **display** rows and converted back to buffer rows, so a block
   fold on screen doesn't widen them; the hidden interior of a fold inside the window is still
   requested (the client clips to the span).
9. **`set_inlay_hints(b)` with `b` already in force is a no-op.** (R21)
10. **The keyed card is widget-private.** `HoverTarget` is a private enum in editor.rs. The public
    input is a new builder, `Editor::inlay_tooltip(Option<(inlay::Key, u32, &str)>)` (R6), next
    to the unchanged `Editor::hover(Option<&HoverInfo>)`. `HoverInfo` lives in scrive-core, which
    this phase doesn't touch. (R21)
11. **`CodeEditor::pending_wake() -> Option<Wake>`** is public and documented (R10): what `view`
    hands the widget. Hosts with their own timers can drive the fetch with it, and Phase 7's
    example tests fire `Action::Wake(generation)` from it.

## 4. Step-by-step changes

### Step 1 — the widget timer (crates/scrive-iced/src/editor.rs)

**1a. `Wake`**, a public type next to `Action`:

```rust
/// A request to be woken, for [`Editor::wake_after`]: after `delay`, the widget publishes
/// [`Action::Wake`] with `generation`.
///
/// The delay counts from the first frame that sees a generation, on the widget's own clock, so a
/// request made from outside `update` (an answer from a server) never fires early or late. A new
/// generation restarts the delay. `cap` bounds the wait across generation changes: counted from
/// the first generation seen since the last wake, the widget wakes after at most `cap` however
/// often the generation changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wake {
    /// Names this request; the woken action carries it back.
    pub generation: u64,
    /// How long after the widget first sees this generation it wakes.
    pub delay: Duration,
    /// The longest wait across a run of generations, if any.
    pub cap: Option<Duration>,
}
```

`Duration` is the `iced::time::Duration` already imported (editor.rs:30). Export it from lib.rs:
`pub use editor::{default_autoscroll_margin, Action, Editor, Wake, SCROLLBAR_WIDTH};`.

**1b. `Action::Wake`**, after `FoldAtCarets` (editor.rs:387-390):

```rust
    /// The wait requested through [`Editor::wake_after`] with this generation is over.
    /// Published once per generation.
    Wake(u64),
```

Add `| Action::Wake(_)` to `moves_caret`'s excluded list (editor.rs:406-441).

**1c. `Editor` field and builder.** The struct (editor.rs:645-667) gains `wake: Option<Wake>`;
`new` sets it to `None`. Add the builder after `hover_pending`:

```rust
    /// Ask to be woken: [`Action::Wake`] is published once the [`Wake`]'s delay has passed.
    /// Pass the same request every frame until it is answered; a new generation restarts the
    /// delay, and `None` cancels it.
    ///
    /// The widget keeps time from the `RedrawRequested` frames it receives, so a widget that is
    /// not in the view tree (a hidden tab) never wakes, and a minimised or occluded window may not
    /// wake until it is shown again.
    #[must_use]
    pub fn wake_after(mut self, wake: Option<Wake>) -> Self {
        self.wake = wake;
        self
    }
```

**1d. `State`.** Add after `doc` (editor.rs:558):

```rust
    /// The pending wake: the generation the widget last stamped, and the instant it fires.
    wake: Option<(u64, Instant)>,
    /// When the widget first saw a generation since its last wake; the cap counts from here, so a
    /// run of generations still wakes.
    wake_first_seen: Option<Instant>,
    /// The generation already published, so a frame that sees it again stays quiet.
    wake_fired: Option<u64>,
```

`impl Default for State` sets all three to `None`. `diff` (editor.rs:1217-1236) builds the new
state from `..State::default()` and keeps only `focus`, `metrics`, `measured_font`, `modifiers`,
`doc` and `autoscroll`, so the wake fields reset with the document. Don't add them to the kept
list.

**1e. `RedrawRequested`.** In the arm at editor.rs:2563, after the blink block and **before** the
`if state.is_focused()` hover block, insert:

```rust
                self.drive_wake(state, *now, shell);
```

and add the method to the private `impl<Message> Editor<'_, Message>` block (after
`popup_layout`):

```rust
    /// Advance the `wake_after` timer on a frame at `now`: stamp a generation the first time it is
    /// seen, then wake once its instant has passed. Runs focused or not.
    fn drive_wake(&self, state: &mut State, now: Instant, shell: &mut Shell<'_, Message>) {
        let Some(wake) = self.wake else {
            state.wake = None;
            state.wake_first_seen = None;
            state.wake_fired = None;
            return;
        };
        if state.wake_fired == Some(wake.generation) {
            return;
        }
        // Restamp before checking: a generation seen for the first time on a late frame still
        // waits its whole delay.
        let at = match state.wake {
            Some((generation, at)) if generation == wake.generation => at,
            _ => {
                let first_seen = *state.wake_first_seen.get_or_insert(now);
                let at = now + wake.delay;
                let at = wake.cap.map_or(at, |cap| at.min(first_seen + cap));
                state.wake = Some((wake.generation, at));
                at
            }
        };
        if now >= at {
            state.wake = None;
            state.wake_first_seen = None;
            state.wake_fired = Some(wake.generation);
            shell.publish((self.on_action)(Action::Wake(wake.generation)));
        } else {
            shell.request_redraw_at(at);
        }
    }
```

`request_redraw_at` keeps the earliest request (core/src/shell.rs:135), so it combines with the
blink and hover timers.

**1f. `CodeEditor` stays inert for now** (code_editor.rs). In `update`, before the catch-all
`Event::Editor(action)` arm:

```rust
            Event::Editor(Action::Wake(_)) => Task::none(),
```

and add `| Action::Wake(_)` to `apply`'s exhaustive no-op list (code_editor.rs:1505-1522). Step 6
replaces the arm.

### Step 2 — gesture actions and the hint hit test (editor.rs)

**2a. Imports.** Add `use scrive_core::intel::inlay;`. The `inlay_at` result is `inlay::At` (R3).

**2b. `Action`**, after `Wake(u64)`:

```rust
    /// The pointer rested on part `part` of inlay hint `key`. The app shows the part's tooltip.
    InlayHover {
        /// The hint.
        key: inlay::Key,
        /// The label part under the pointer.
        part: u32,
    },
    /// Ctrl+click on a label part that links somewhere: go there.
    InlayJump {
        /// The hint.
        key: inlay::Key,
        /// The linked part.
        part: u32,
    },
    /// Double-click on a hint that can be inserted: apply its edits.
    InlayInsert {
        /// The hint.
        key: inlay::Key,
        /// The hint's offset.
        offset: u32,
    },
```

`Action` derives `Clone, Debug, PartialEq`, so `inlay::Key` must implement all three. Add
`| Action::InlayHover { .. } | Action::InlayJump { .. } | Action::InlayInsert { .. }` to
`moves_caret`'s excluded list.

**2c. The hit test.** Phase 3's `Rows::inlay_at(row: BufferRow, cell: f32) -> Option<inlay::At>`
answers for one buffer row and fractional display cell (R3):

```rust
// scrive_core::intel::inlay (Phase 3)
pub enum At {
    Label { key: Key, part: u32, offset: u32, link: Link, insert: Insert, cells: Range<u32> },
    Padding { key: Key, offset: u32 },
}
```

Add one widget helper beside `hit_test` so every gesture resolves the pointer the same way:

```rust
    /// The hint under `pos`, if any, and the buffer row it is on. Padding is reported as such, so
    /// callers can keep it from falling through to the word beneath.
    fn inlay_hit(&self, rows: &Rows<'_>, geo: &Geo, pos: Point) -> Option<(u32, inlay::At)> {
        let folds = rows.folds();
        let row = folds.to_buffer_row(folds.display_row_at(geo.rows_from_top(pos.y)));
        // `inlay_at` answers `None` left of the text (a negative cell).
        rows.inlay_at(row, geo.x_cell(pos.x)).map(|at| (row.0, at))
    }

    /// Whether the shown hints were fetched for the current text. A moved set's tooltips,
    /// locations and edits describe text that is gone, so its cells take no gesture.
    fn inlays_current(&self) -> bool {
        self.doc.inlays_revision() == Some(self.doc.revision())
    }
```


**2d. The remembered part.** A private struct and three `State` fields:

```rust
/// One hint label part on screen: which hint and part, its buffer row, and its display cells.
#[derive(Clone, Debug, PartialEq)]
struct InlayPart {
    key: inlay::Key,
    part: u32,
    row: u32,
    cells: Range<u32>,
}
```

```rust
    /// The hint part a tooltip query went out for. The pointer keeps that query, and the card it
    /// opens, while it stays on this part, and the card anchors on its cells.
    inlay_hover: Option<InlayPart>,
    /// The link part underlined under the held Ctrl, tracked so that entering, leaving or
    /// switching parts repaints once.
    inlay_link: Option<InlayPart>,
```

Both default to `None`, and `diff` resets them with the document.

### Step 3 — hover over hints and the keyed card (editor.rs)

**3a. `Editor` input.** Field `inlay_tooltip: Option<(inlay::Key, u32, &'a str)>` (`None`
in `new`), and the builder after `hover` (R6):

```rust
    /// Supply the open inlay-hint tooltip: the hint, the label part it describes and its
    /// markdown. The card anchors on that part's cells and stays open while the pointer is on
    /// the part or the card.
    #[must_use]
    pub fn inlay_tooltip(mut self, tooltip: Option<(inlay::Key, u32, &'a str)>) -> Self {
        self.inlay_tooltip = tooltip;
        self
    }
```

**3b. One card, two targets.** Add the widget-private enum (named in the plan) and split
`hover_layout` so the anchor is an input:

```rust
/// What the open hover card describes, which decides where the pointer keeps it open: a buffer
/// range (a word, a diagnostic), or an inlay hint part, which is not in the buffer and is known
/// by its key.
enum HoverTarget {
    Range(Range<u32>),
    Inlay { key: inlay::Key, part: u32 },
}
```

Replace `hover_layout` (editor.rs:2729-2762) with:

```rust
    /// The hover card's box and wrapped lines for `markdown`, placed above the anchor row
    /// `(x, row_top, row_bottom)` (below it when there is no room). One source of truth for
    /// `draw_hover`, the wheel handler and `still_in`.
    fn card_layout(&self, markdown: &str, anchor: (f32, f32, f32), geo: &Geo) -> HoverLayout {
        // today's body, with `info.markdown` → `markdown` and the `popup_anchor` call replaced
        // by `let (word_x, word_top, word_bottom) = anchor;`
    }

    /// The word card for `info`, anchored at its range's start.
    fn hover_layout(&self, rows: &Rows<'_>, info: &HoverInfo, geo: &Geo) -> HoverLayout {
        self.card_layout(&info.markdown, self.popup_anchor(rows, geo, info.range.start), geo)
    }

    /// The open card, inlay tooltip first, with its layout. An inlay card shows only on the part
    /// the widget queried, because only that part's cells anchor it.
    fn open_card(&self, state: &State, rows: &Rows<'_>, geo: &Geo) -> Option<(HoverTarget, HoverLayout)> {
        if let Some((key, part, markdown)) = self.inlay_tooltip {
            let spot = state
                .inlay_hover
                .as_ref()
                .filter(|p| p.key == key && p.part == part)?;
            let top = geo.row_y(rows.folds().to_display_row(BufferRow(spot.row)));
            let anchor = (geo.cell_x(spot.cells.start as f32), top, top + geo.line_h());
            return Some((HoverTarget::Inlay { key, part }, self.card_layout(markdown, anchor, geo)));
        }
        self.hover.map(|info| (HoverTarget::Range(info.range.clone()), self.hover_layout(rows, info, geo)))
    }

    /// Whether a card shows or a query is out: leaving the spot must then publish
    /// `HoverDismiss`.
    fn card_or_query(&self, state: &State) -> bool {
        self.hover.is_some() || self.inlay_tooltip.is_some() || state.hover_queried
    }
```

Change `draw_hover` to take the `&HoverLayout` instead of the `&HoverInfo`, and drop its own
`hover_layout` call. At the draw site (editor.rs:1963-1966):

```rust
        if let Some((_, l)) = self.open_card(state, &rows, &geo) {
            self.draw_hover(renderer, &l, &geo, text_color, state.hover_scroll);
        }
```

The wheel handler (editor.rs:2315-2331) reads `self.open_card(state, &rows, &geo)` in place of
`self.hover` + `hover_layout`. The test `popup_anchors_are_display_space_below_a_fold` calls
`hover_layout` with Phase 2's `rows` argument; its expectation is unchanged.

Replace every `self.hover.is_some() || state.hover_queried` in `update` (editor.rs:2268, 2283,
2592) with `self.card_or_query(state)`. Wherever `hover_queried` is reset, also set
`state.inlay_hover = None`.

**3c. `CursorMoved`, the plain-move branch** (editor.rs:2248-2276). Current:

```rust
                    let off = self.hit_test(&geo, pos);
                    let still_in = self.hover.is_some_and(|h| {
                        (off >= h.range.start && off < h.range.end)
                            || self.hover_layout(h, &geo).rect.contains(pos)
                    }) || self.hover_pending.as_ref().is_some_and(|w| w.contains(&off));
                    if !still_in {
                        if self.hover.is_some() || state.hover_queried {
                            state.hover_queried = false;
                            shell.publish((self.on_action)(Action::HoverDismiss));
                        }
                        // … re-arm
```

New (with Phase 2's `rows`):

```rust
                    let off = self.hit_test(&rows, &geo, pos);
                    let part_under = match self.inlay_hit(&rows, &geo, pos) {
                        Some((_, inlay::At::Label { key, part, .. })) => Some((key, part)),
                        _ => None,
                    };
                    let on_card = self.open_card(state, &rows, &geo).is_some_and(|(target, l)| {
                        l.rect.contains(pos)
                            || match target {
                                HoverTarget::Range(range) => range.contains(&off),
                                HoverTarget::Inlay { key, part } => {
                                    part_under == Some((key, part))
                                }
                            }
                    });
                    let on_queried_part = state
                        .inlay_hover
                        .as_ref()
                        .is_some_and(|p| part_under == Some((p.key, p.part)));
                    let still_in = on_card
                        || on_queried_part
                        || self.hover_pending.as_ref().is_some_and(|w| w.contains(&off));
                    if !still_in {
                        if self.card_or_query(state) {
                            state.hover_queried = false;
                            state.inlay_hover = None;
                            shell.publish((self.on_action)(Action::HoverDismiss));
                        }
                        // … re-arm, unchanged
```

`HoverTarget::Range` keeps today's word test (`off` inside the range), and `HoverTarget::Inlay`
holds while the pointer is on the card's part. `on_queried_part` keeps an unanswered tooltip query
alive on its part (Decision 3). The gutter/off-widget branch (editor.rs:2277-2287) clears `state.inlay_hover` along with the rest.

**3d. The idle timer** (`RedrawRequested`, the `else` at editor.rs:2596-2600). Current:

```rust
                            } else {
                                let off = self.hit_test(&geo, pos);
                                state.hover_queried = true;
                                shell.publish((self.on_action)(Action::HoverQuery(off)));
                            }
```

New: chip first (unchanged, above), then the hint, then the word:

```rust
                            } else {
                                match self.inlay_hit(&rows, &geo, pos) {
                                    Some((row, inlay::At::Label { key, part, cells, .. })) if self.inlays_current() => {
                                        state.hover_queried = true;
                                        state.inlay_hover = Some(InlayPart { key, part, row, cells });
                                        shell.publish((self.on_action)(Action::InlayHover { key, part }));
                                    }
                                    // Padding is background, and a moved set's labels describe
                                    // text that is gone; neither is the word beneath.
                                    Some(_) => {}
                                    None => {
                                        let off = self.hit_test(&rows, &geo, pos);
                                        state.hover_queried = true;
                                        shell.publish((self.on_action)(Action::HoverQuery(off)));
                                    }
                                }
                            }
```

This stays inside the `is_focused()` gate, so hint hover arms only while focused (D10). The chip
branch's dismissal uses `card_or_query` and clears `inlay_hover`.

### Step 4 — Ctrl+click, the underline and the pointer (editor.rs)

**4a. Press order.** In the `ButtonPressed(Left)` arm, between the collapsed-chip block
(editor.rs:2098-2108) and the `SHOW_CTRL_COLLAPSE_AFFORDANCE` block (2109-2123), insert:

```rust
                // Ctrl+click on a linked label part jumps through it. On a set the text has moved
                // past, the location is stale: the click still belongs to the link and does nothing.
                if state.modifiers.command() && !state.modifiers.shift() && !state.modifiers.alt() {
                    let rows = self.doc.rows();
                    if let Some((_, inlay::At::Label { key, part, link: inlay::Link::Jumps, .. })) =
                        self.inlay_hit(&rows, &geo, pos)
                    {
                        if self.inlays_current() {
                            shell.publish((self.on_action)(Action::InlayJump { key, part }));
                        }
                        shell.capture_event();
                        return;
                    }
                }
```

The new press order: unfocus, fold-preview clear, completion rows, scrollbars, gutter toggle,
collapsed chip, **inlay link**, Ctrl collapse (compiled out), `hit_test` plus click count.

**4b. Underline tracking.** A helper for the link under a held Ctrl:

```rust
    /// The linked label part the held Ctrl would follow, if the pointer is on one. None during a
    /// drag, so a Ctrl-drag across a link neither underlines nor captures, and none on a moved
    /// set.
    fn link_under(&self, state: &State, rows: &Rows<'_>, geo: &Geo, pos: Option<Point>) -> Option<InlayPart> {
        if !state.modifiers.command() || state.drag.is_some() || state.column_drag_anchor.is_some() || !self.inlays_current() {
            return None;
        }
        match self.inlay_hit(rows, geo, pos.filter(|p| !geo.in_gutter(p.x))?)? {
            (row, inlay::At::Label { key, part, link: inlay::Link::Jumps, cells, .. }) => Some(InlayPart { key, part, row, cells }),
            _ => None,
        }
    }
```

In the `CursorMoved` arm, next to the `hover_chip` tracking (editor.rs:2189-2197):

```rust
                let link = self.link_under(state, &rows, &geo, cursor.position_over(bounds));
                if link != state.inlay_link {
                    state.inlay_link = link;
                    shell.request_redraw();
                }
```

In the `ModifiersChanged` arm (editor.rs:2523-2530), after `state.modifiers = *mods;`, do the same
with a geometry built there (`let geo = self.geo(state, bounds); let rows = self.doc.rows();`).
Pressing or releasing Ctrl over a link then repaints without a mouse move.

**4c. Draw.** In `draw`, after the text pass and Phase 4's hint labels, inside the text clip:

```rust
        // The held Ctrl's link: underlined, like a hyperlink.
        if let Some(link) = &state.inlay_link {
            let top = geo.row_y(rows.folds().to_display_row(BufferRow(link.row)));
            let x = geo.cell_x(link.cells.start as f32);
            let width = (link.cells.end - link.cells.start) as f32 * advance;
            fill(renderer, Rectangle { x, y: top + line_h - 2.0, width, height: 1.0 }, Color { a: INLAY_TEXT_A, ..text_color });
        }
```

The colour is Phase 4's label colour (`INLAY_TEXT_A` over `text_color`). This is not a new pass
over rows, so the draw budget is unaffected.

**4d. The pointer.** In `mouse_interaction`, right after the thumb-drag early return
(editor.rs:2625-2627):

```rust
        if state.inlay_link.is_some() {
            return mouse::Interaction::Pointer;
        }
```

### Step 5 — single and double clicks on hints (editor.rs)

In the click-count `else` branch (editor.rs:2147-2164). Current:

```rust
                    let click = mouse::Click::new(pos, mouse::Button::Left, state.last_click);
                    state.last_click = Some(click);
                    let granularity = match click.kind() { … };
                    state.drag = Some(Drag { granularity, origin: offset });
                    let action = if granularity == Granularity::Char {
                        Action::PlaceCaret(offset)
                    } else {
                        Action::DragSelect { granularity, origin: offset, head: offset }
                    };
                    shell.publish((self.on_action)(action));
```

New:

```rust
                    let click = mouse::Click::new(pos, mouse::Button::Left, state.last_click);
                    state.last_click = Some(click);
                    let granularity = match click.kind() { … };
                    // A hint's text is not in the buffer, so a double click on it never selects a
                    // word: it inserts the hint when it can, else it is a plain click.
                    let on_hint = (granularity == Granularity::Word)
                        .then(|| self.inlay_hit(&rows, &geo, pos))
                        .flatten();
                    let action = match on_hint {
                        Some((_, inlay::At::Label { key, insert: inlay::Insert::Available, .. })) if self.inlays_current() => {
                            state.drag = None;
                            Action::InlayInsert { key, offset }
                        }
                        Some(_) => {
                            state.drag = Some(Drag { granularity: Granularity::Char, origin: offset });
                            Action::PlaceCaret(offset)
                        }
                        None => {
                            state.drag = Some(Drag { granularity, origin: offset });
                            if granularity == Granularity::Char {
                                Action::PlaceCaret(offset)
                            } else {
                                Action::DragSelect { granularity, origin: offset, head: offset }
                            }
                        }
                    };
                    shell.publish((self.on_action)(action));
```

`offset` is `hit_test`'s result. Phase 3's `hit` maps every hint cell to the hint's offset, so it
is also the hint offset `InlayInsert` carries. A single click needs no change: it already places
the caret at `hit_test`'s offset.

**5a. CodeEditor stays inert for the gestures** until Step 6. In `update`, before the catch-all:

```rust
            Event::Editor(Action::InlayHover { .. } | Action::InlayJump { .. } | Action::InlayInsert { .. }) => Task::none(),
```

and add the three to `apply`'s no-op list.

### Step 6 — `CodeEditor`: scheduler, slots, seam, toggle (code_editor.rs)

**6a. Imports.** `use iced::time::{Duration, Instant};` (replacing `use iced::time::Instant;`),
`use scrive_core::intel::inlay;`, `BufferRow` added to the `scrive_core::{…}` list (for
`inlay_window`), and `Wake` from `crate::editor::{Action, Editor, Wake}`.

**6b. Constants**, after `BAR_BOX_MARGIN`:

```rust
/// How long typing must pause before hints are fetched for the new text. Short enough that hints
/// follow the text, long enough not to fetch on every keystroke (Zed waits 700 ms, Helix 250 ms).
const INLAY_EDIT_DELAY: Duration = Duration::from_millis(300);
/// How long scrolling must pause before hints are fetched for the rows it reached…
const INLAY_SCROLL_DELAY: Duration = Duration::from_millis(75);
/// …and the longest a continuous scroll (a scrollbar drag) waits, so a drag fetches as it goes
/// instead of cancelling each request before it answers.
const INLAY_SCROLL_CAP: Duration = Duration::from_millis(300);
/// The fewest rows a hint request covers.
const INLAY_MIN_ROWS: u32 = 50;
```

**6c. Scheduler state.** Next to `Awaiting`:

```rust
/// Inlay-hint fetching: whether hints are on, the wait a fetch is pending on, and the rows the
/// viewport may move within before the last fetch's window needs replacing.
struct Inlays {
    enabled: bool,
    /// Bumped by every trigger, so the widget restarts its delay.
    generation: u64,
    /// The pending fetch: a delay and cap, never a deadline, because triggers arrive outside
    /// `update` where the clock is stale.
    wait: Option<Wake>,
    /// The inner half of the last requested window, in buffer rows.
    window: Option<Range<u32>>,
    /// The last revision the scheduler saw; a different one is an edit.
    seen: Revision,
}

/// The inlay tooltip card: the hint and part it describes, and its markdown.
struct InlayCard {
    key: inlay::Key,
    part: u32,
    markdown: String,
}
```

**6d. `Awaiting` and `Awaited`.** Current (code_editor.rs:293-302, 315-321):

```rust
struct Awaiting {
    completion: Option<Ticket>,
    signature: Option<Ticket>,
    hover: Option<(Ticket, u32, Range<u32>)>,
    definition: Option<Ticket>,
}
enum Awaited { Completion, Signature, Hover, Definition }
```

New fields (with short docs) and variants:

```rust
    /// The inlay fetch's ticket.
    inlays: Option<Ticket>,
    /// The inlay tooltip's ticket, and the hint and part it describes.
    inlay_tooltip: Option<(Ticket, inlay::Key, u32)>,
    /// The inlay insert's ticket, and the hint and its offset, which the edits' landing removes
    /// before applying them.
    inlay_insert: Option<(Ticket, inlay::Key, u32)>,
```

```rust
enum Awaited { Completion, Signature, Hover, Definition, Inlays, InlayTooltip, InlayInsert }
```

`accepts` (code_editor.rs:1792-1800) gains:

```rust
            Awaited::Inlays => self.awaiting.inlays,
            Awaited::InlayTooltip => self.awaiting.inlay_tooltip.as_ref().map(|(t, ..)| *t),
            Awaited::InlayInsert => self.awaiting.inlay_insert.as_ref().map(|(t, ..)| *t),
```

`abandon` (code_editor.rs:1804-1823) gains the three arms, and the `Definition` arm drops a pending
jump interaction too:

```rust
            Awaited::Definition => {
                let ticket = self.awaiting.definition.take();
                self.pending_definition_request = None;
                self.drop_interaction(ticket);
            }
            Awaited::Inlays => {
                self.awaiting.inlays = None;
                self.pending_inlay_request = None;
            }
            Awaited::InlayTooltip => {
                let ticket = self.awaiting.inlay_tooltip.take().map(|(t, ..)| t);
                self.drop_interaction(ticket);
            }
            Awaited::InlayInsert => {
                let ticket = self.awaiting.inlay_insert.take().map(|(t, ..)| t);
                self.drop_interaction(ticket);
            }
```

```rust
    /// Forget the unpulled gesture made under `ticket`; a newer gesture's stays.
    fn drop_interaction(&mut self, ticket: Option<Ticket>) {
        if ticket.is_some() && self.pending_inlay_interaction.as_ref().map(|i| i.ticket()) == ticket {
            self.pending_inlay_interaction = None;
        }
    }
```

**6e. Fields** on `CodeEditor`, after `pending_rename_request`:

```rust
    /// A pending inlay-hint fetch, for the host to pull via
    /// [`take_inlay_request`](CodeEditor::take_inlay_request).
    pending_inlay_request: Option<inlay::Request>,
    /// A pending gesture on a hint (tooltip, jump or insert), for the host to pull via
    /// [`take_inlay_interaction`](CodeEditor::take_inlay_interaction). One slot: a newer gesture
    /// replaces it.
    pending_inlay_interaction: Option<inlay::Interaction>,
    /// The inlay-hint scheduler.
    inlays: Inlays,
    /// The open inlay tooltip card. At most one card shows: this or `hover`.
    inlay_card: Option<InlayCard>,
```

`new` initializes them to `None`, `None`, `Inlays { enabled: false, generation: 0, wait: None,
window: None, seen: doc.revision() }` (bind `doc.revision()` before moving `doc` into `Self`), and
`None`.

**6f. Builder** after `rename`:

```rust
    /// Show inlay hints, the server's inline labels such as `: i32`. Default off: turn it on when
    /// the host answers [`take_inlay_request`](CodeEditor::take_inlay_request).
    #[must_use]
    pub fn inlay_hints(mut self, enabled: bool) -> Self {
        self.set_inlay_hints(enabled);
        self
    }
```

**6g. Public API**, after `select`:

```rust
    /// Turn inlay hints on or off at runtime. On asks for the visible rows' hints at once. Off
    /// removes the hints, closes their tooltip and forgets every hint request in flight.
    pub fn set_inlay_hints(&mut self, enabled: bool) {
        if enabled == self.inlays.enabled {
            return;
        }
        self.inlays.enabled = enabled;
        if enabled {
            self.wait_inlays(Duration::ZERO, None);
            return;
        }
        self.doc.clear_inlays();
        self.inlay_card = None;
        self.abandon(Awaited::Inlays);
        self.abandon(Awaited::InlayTooltip);
        self.abandon(Awaited::InlayInsert);
        self.pending_inlay_interaction = None;
        self.inlays.wait = None;
        self.inlays.window = None;
    }

    /// Take the pending inlay-hint fetch, if any: a byte span to fetch hints for. Answer it
    /// through [`set_inlays`](Self::set_inlays) with the request's ticket.
    pub fn take_inlay_request(&mut self) -> Option<inlay::Request> {
        self.pending_inlay_request.take()
    }

    /// Land an inlay-hint fetch stamped with the request's `ticket`: `Some` replaces the shown
    /// hints (an empty list clears them), and `None`, a failed fetch, keeps them. Dropped unless
    /// the editor still awaits `ticket` and the text has not changed since.
    pub fn set_inlays(&mut self, ticket: Ticket, hints: Option<Vec<inlay::Placed>>) {
        if !self.accepts(Awaited::Inlays, ticket) {
            return;
        }
        self.abandon(Awaited::Inlays);
        let Some(hints) = hints else { return };
        // A refetch keeps an unchanged hint's key, so its open card stays.
        if self.inlay_card.as_ref().is_some_and(|card| !hints.iter().any(|p| p.hint().key() == card.key)) {
            self.inlay_card = None;
        }
        // `accepts` checked the revision, so this always installs.
        let _ = self.doc.set_inlays(ticket.revision(), hints);
    }

    /// The wait a hint fetch is pending on, if any: what [`view`](Self::view) hands the widget
    /// through [`Editor::wake_after`]. A host that runs its own timer, or a test, fires
    /// [`Action::Wake`] with its generation once the delay has passed.
    #[must_use]
    pub fn pending_wake(&self) -> Option<Wake> {
        self.inlays.wait
    }

    /// Take the pending gesture on a hint, if any: a tooltip to show, a label to jump through, or
    /// a hint to insert. Answer a tooltip through [`set_inlay_tooltip`](Self::set_inlay_tooltip),
    /// a jump as a definition, and an insert as an edit batch.
    pub fn take_inlay_interaction(&mut self) -> Option<inlay::Interaction> {
        self.pending_inlay_interaction.take()
    }

    /// Land a hint tooltip stamped with the gesture's `ticket`: `Some` shows the markdown as a card
    /// on the hovered label part, and `None` shows nothing. Dropped once the pointer has left the
    /// part or the text has changed.
    pub fn set_inlay_tooltip(&mut self, ticket: Ticket, markdown: Option<String>) {
        if !self.accepts(Awaited::InlayTooltip, ticket) {
            return;
        }
        let Some((_, key, part)) = self.awaiting.inlay_tooltip else { return };
        self.abandon(Awaited::InlayTooltip);
        self.inlay_card = markdown.map(|markdown| InlayCard { key, part, markdown });
        if self.inlay_card.is_some() {
            self.hover = None;
        }
    }
```

Phase 1's `inlay::Outcome` may be `#[must_use]`; `let _ =` is right because `accepts` has already
checked the revision. If Phase 1 offers a cheap "is applied" check, a `debug_assert!` on it is
better.

**6h. Private scheduler helpers**, in the internals section:

```rust
    /// Ask for hints after `delay` (capped across a run of triggers by `cap`). Each call restarts
    /// the wait. Ignored while hints are off.
    fn wait_inlays(&mut self, delay: Duration, cap: Option<Duration>) {
        if !self.inlays.enabled {
            return;
        }
        self.inlays.generation += 1;
        self.inlays.wait = Some(Wake { generation: self.inlays.generation, delay, cap });
    }

    /// Note an edit for the hints: their tooltip and insert describe text that is gone, and the
    /// new text needs its own hints once typing pauses.
    fn inlays_after_edit(&mut self) {
        if self.doc.revision() == self.inlays.seen {
            return;
        }
        self.inlays.seen = self.doc.revision();
        self.inlay_card = None;
        self.abandon(Awaited::InlayTooltip);
        self.abandon(Awaited::InlayInsert);
        self.wait_inlays(INLAY_EDIT_DELAY, None);
    }

    /// The buffer rows to fetch hints for: the viewport padded by its height above and twice that
    /// below, at least `INLAY_MIN_ROWS`, within the document, all counted in display rows. Returns
    /// the rows and their inner half.
    fn inlay_window(&self) -> (Range<u32>, Range<u32>) {
        let lines = self.doc.buffer().line_count();
        let folds = self.doc.fold_map();
        let shown = folds.display_row_count();
        // Pads count display rows, so a block fold on screen doesn't widen them (R20).
        let to_display = |row: u32| if row >= lines { shown } else { folds.to_display_row(BufferRow(row)).index() };
        let vis = to_display(self.viewport.start)..to_display(self.viewport.end).max(to_display(self.viewport.start));
        let height = vis.end - vis.start;
        let mut start = vis.start.saturating_sub(height);
        let mut end = vis.end.saturating_add(2 * height).min(shown);
        if end - start < INLAY_MIN_ROWS {
            end = start.saturating_add(INLAY_MIN_ROWS).min(shown);
            start = end.saturating_sub(INLAY_MIN_ROWS);
        }
        // start <= vis.start <= vis.end <= end holds on every path above.
        let inner = start + (vis.start - start) / 2..end - (end - vis.end) / 2;
        // Back to buffer rows; a fold's hidden interior inside the window is requested too.
        let to_buffer = |d: u32| if d >= shown { lines } else { folds.to_buffer_row(folds.display_row_at(f64::from(d))).0 };
        (to_buffer(start)..to_buffer(end), to_buffer(inner.start)..to_buffer(inner.end))
    }

    /// Whether hints on screen describe the current text (D14): only then may a gesture on them
    /// be recorded.
    fn inlays_current(&self) -> bool {
        self.doc.inlays_revision() == Some(self.doc.revision())
    }
```

The ordering comment is for the reviewer; drop it if the arithmetic reads plainly. Check §9's
`inlay_window` table against the implementation with
`the_request_window_pads_one_view_above_and_two_below`.

**6i. Call the edit trigger.**
- `after_edit` (code_editor.rs:1537-1566): after `self.hover = None;` add `self.inlay_card = None;`
  and, at the end of the method, `self.inlays_after_edit();`.
- `accept_completion` (code_editor.rs:1774-1779): after `self.doc.maybe_rescan_find(now);` add
  `self.inlays_after_edit();`. It commits an edit without running `after_edit`.
- `load` (code_editor.rs:561-576): after the `doc.edit`, add:

  ```rust
          // The whole buffer was replaced; the edit would keep the old hints at stale offsets.
          self.doc.clear_inlays();
          self.inlay_card = None;
          self.abandon(Awaited::Inlays);
          self.abandon(Awaited::InlayTooltip);
          self.abandon(Awaited::InlayInsert);
          self.inlays.seen = self.doc.revision();
          self.inlays.window = None;
          self.wait_inlays(Duration::ZERO, None);
  ```

**6j. `update` arms.** Replace Step 1f's and Step 5a's inert arms with these, all before the
catch-all `Event::Editor(action)`:

```rust
            // The pending fetch's wait is over. Kept out of `apply`, whose tail would close the
            // completion popup after every typing pause.
            Event::Editor(Action::Wake(generation)) => {
                if self.inlays.wait.is_some_and(|w| w.generation == generation) {
                    self.inlays.wait = None;
                    let (rows, inner) = self.inlay_window();
                    let buffer = self.doc.buffer();
                    let start = buffer.point_to_offset(Point::new(rows.start, 0));
                    let end = if rows.end >= buffer.line_count() {
                        buffer.len()
                    } else {
                        buffer.point_to_offset(Point::new(rows.end, 0))
                    };
                    let ticket = self.tickets.issue(self.doc.revision());
                    self.awaiting.inlays = Some(ticket);
                    self.pending_inlay_request = Some(inlay::Request::new(ticket, start..end));
                    self.inlays.window = Some(inner);
                }
                Task::none()
            }
            // Gestures on hints: kept out of `apply`, whose tail would retire the slots they fill.
            Event::Editor(Action::InlayHover { key, part }) => {
                self.hover = None;
                self.abandon(Awaited::Hover);
                self.inlay_card = None;
                if self.inlays.enabled && self.inlays_current() {
                    let ticket = self.tickets.issue(self.doc.revision());
                    self.awaiting.inlay_tooltip = Some((ticket, key, part));
                    self.pending_inlay_interaction = Some(inlay::Interaction::tooltip(ticket, key, part));
                }
                Task::none()
            }
            Event::Editor(Action::InlayJump { key, part }) => {
                if self.inlays.enabled && self.inlays_current() {
                    let ticket = self.tickets.issue(self.doc.revision());
                    // The jump lands as a definition, so every target kind works.
                    self.awaiting.definition = Some(ticket);
                    self.pending_definition_request = None;
                    self.pending_inlay_interaction = Some(inlay::Interaction::jump(ticket, key, part));
                }
                Task::none()
            }
            Event::Editor(Action::InlayInsert { key, offset }) => {
                if self.inlays.enabled && self.inlays_current() {
                    let ticket = self.tickets.issue(self.doc.revision());
                    self.awaiting.inlay_insert = Some((ticket, key, offset));
                    self.pending_inlay_interaction = Some(inlay::Interaction::insert(ticket, key, offset));
                }
                Task::none()
            }
```

The `Interaction` constructors are Phase 1's (R6).

Change the existing arms:
- `ViewportChanged` (code_editor.rs:759-780). `rows` is moved into `reaim` further down, so
  decide first. Make this the arm's first statement:

  ```rust
                  let left_window = self.inlays.window.as_ref().is_some_and(|w| rows.start < w.start || rows.end > w.end);
  ```

  and after `self.abandon(Awaited::Hover);` add

  ```rust
                  self.inlay_card = None;
                  self.abandon(Awaited::InlayTooltip);
                  if left_window {
                      self.wait_inlays(INLAY_SCROLL_DELAY, Some(INLAY_SCROLL_CAP));
                  }
  ```
- `HoverQuery` (code_editor.rs:857-877): first line `self.inlay_card = None; self.abandon(Awaited::InlayTooltip);`.
- `HoverDismiss` (code_editor.rs:878-882): add `self.inlay_card = None;` and
  `self.abandon(Awaited::InlayTooltip);`.
- `apply`'s no-op list keeps `Wake`, `InlayHover`, `InlayJump` and `InlayInsert` (now unreachable).

**6k. `view`** (code_editor.rs:1134-1150): after `.hover_pending(…)` add

```rust
            .inlay_tooltip(self.inlay_card.as_ref().map(|card| (card.key, card.part, card.markdown.as_str())))
            .wake_after(self.inlays.wait)
```

`Wake` is `Copy`, so the pending wait is passed every frame until a `Wake` clears it.

**6l. What Phases 6 and 7 call** (no code here; keep these private items reachable from the
`lsp` child module, which already uses `accepts` and `Awaited`):
- Phase 6's `InlayRefresh` arm: `self.wait_inlays(INLAY_EDIT_DELAY, None)`. Because
  `wait_inlays` ignores triggers while disabled, the arm never needs to check.
- Phase 6's `Inlays` / `InlayTooltip` arms: `accepts` then `set_inlays` / `set_inlay_tooltip`.
- Phase 7's `Edits` arm: `self.awaiting.inlay_insert.take()` and `remove_inlay(key, offset)`
  before `try_edit`; an empty batch for that ticket settles the slot with
  `self.abandon(Awaited::InlayInsert)` and skips `try_edit` (R16). Phase 7's `close_lsp`: the
  same clearing as `set_inlay_hints(false)` without flipping `enabled`, the pending wait included
  (R19). Phase 7's `open_lsp`: `self.wait_inlays(Duration::ZERO, None)` (R19).
- Phase 7's example tests: `pending_wake()` (public, R10).

## 5. Files changed

| File | Change |
|---|---|
| crates/scrive-iced/src/editor.rs | `Wake`; `Action::{Wake, InlayHover, InlayJump, InlayInsert}`; `moves_caret`; `Editor::{wake_after, inlay_tooltip}`; `State` wake fields, `inlay_hover`, `inlay_link`; `drive_wake`; `InlayPart`, `HoverTarget`; `inlay_hit`, `inlays_current`, `link_under`, `card_layout`, `open_card`, `card_or_query`; press order (link test), click count (double-click on hints), `CursorMoved` (`still_in`, underline), `ModifiersChanged`, idle timer, wheel, `mouse_interaction`, draw (card, underline); tests |
| crates/scrive-iced/src/code_editor.rs | constants; `Inlays`, `InlayCard`; `Awaiting` + `Awaited` (3 each); `accepts`, `abandon`, `drop_interaction`; fields; `inlay_hints`, `set_inlay_hints`, `take_inlay_request`, `set_inlays`, `take_inlay_interaction`, `set_inlay_tooltip`, `pending_wake`; `wait_inlays`, `inlays_after_edit`, `inlay_window`, `inlays_current`; `update` arms (`Wake`, `InlayHover`, `InlayJump`, `InlayInsert`; `ViewportChanged`, `HoverQuery`, `HoverDismiss` additions); `apply` no-op list; `after_edit`, `accept_completion`, `load`; `view`; tests |
| crates/scrive-iced/src/lib.rs | export `Wake` |

## 6. Tests to add

Every test gets a one-line `///` doc stating the invariant and string assert messages (DISPATCH).

### editor.rs (`mod tests`)

**Helpers.** Generalize `pump` so a test can configure the editor, and read what the frame left:

```rust
    /// What one `pump_editor` call left: the published actions, the widget cache, the pointer
    /// shape and each event's capture status.
    struct Pumped {
        actions: Vec<Action>,
        cache: iced_runtime::user_interface::Cache,
        interaction: mouse::Interaction,
        statuses: Vec<iced::event::Status>,
    }

    /// Run `events` through a one-editor UI built from `editor`, pointer at `at`.
    fn pump_editor(
        editor: Editor<'_, Action>,
        cache: iced_runtime::user_interface::Cache,
        renderer: &mut iced::Renderer,
        at: Point,
        events: &[iced::Event],
    ) -> Pumped { … } // today's `pump` body; read `mouse_interaction` from `State::Updated`
```

`pump(doc, pending, …)` becomes `pump_editor(Editor::new(doc, |a| a).hover_pending(pending), …)`
returning `(p.actions, p.cache)`. The existing callers don't change.

```rust
    /// A frame at `t0 + ms`.
    fn frame(t0: Instant, ms: u64) -> iced::Event {
        iced::Event::Window(window::Event::RedrawRequested(t0 + Duration::from_millis(ms)))
    }

    /// The wakes among `actions`.
    fn wakes(actions: &[Action]) -> Vec<u64> {
        actions.iter().filter_map(|a| match a { Action::Wake(g) => Some(*g), _ => None }).collect()
    }

    /// `let x = 1;` with a `: i32` type hint after `x`: key 1, insertable, label parts `": "`
    /// (no link) and `"i32"` (links), installed at the current revision. Its cells are 5..10.
    fn hinted_doc() -> Document { … Phase 1's Hint::new / Part::new / Placed::new / set_inlays … }

    /// `foo(1)` with a parameter hint `n:` before `1` (key 2, padding right, no edits). Label
    /// cells 4..6, padding cell 6.
    fn padded_doc() -> Document { … }

    /// The screen point at the middle of display cell `cell` on row 0, in `pump_editor`'s
    /// 500×320 frame (measured metrics, unscrolled).
    fn cell_point(doc: &Document, cell: u32) -> Point {
        let m = Metrics::measure(crate::DEFAULT_FONT, DEFAULT_SIZE, default_line_height(DEFAULT_SIZE));
        let ed = Editor::new(doc, |a: Action| a);
        let geo = Geo::new(Rectangle::new(Point::ORIGIN, Size::new(500.0, 320.0)), ed.gutter_width(m.advance), m.advance, m.line_height, 0.0, ScrollAnchor::TOP);
        Point::new(geo.cell_x(cell as f32 + 0.5), m.line_height / 2.0)
    }
```

**Timer tests.** For these, `hinted_doc` isn't needed; use `Document::new("x\n")`.
- **`a_wake_fires_once_after_its_delay`**: `wake_after(Some(Wake { generation: 1, delay: 300 ms,
  cap: None }))`. Frames at 0, 299: no wake. 300: `[1]`. 400 (same request still passed): no
  second wake.
- **`a_new_generation_restarts_the_delay`**: generation 1 seen at 0, generation 2 passed from frame
  200 on. No wake at 300 (generation 1 is superseded) or at 499; `[2]` at 500.
- **`a_wake_counts_its_delay_from_the_first_frame_that_sees_it`** (D11's "idle for 10 s",
  widget half): pump frames at 0 and 10 000 with `wake_after(None)`, then pass generation 1 (300
  ms, no cap) from frame 10 000 on. No wake at 10 000 or 10 299; `[1]` at 10 300. The request
  isn't treated as already due.
- **`a_capped_wake_fires_at_the_max_wait_while_generations_keep_changing`**: frames every 16 ms
  from 0; frame *i* passes generation *i* with delay 75 ms and cap 300 ms. The first wake comes at
  the first frame ≥ 300 (frame 304), not at 75 and not never.
- **`a_one_second_scroll_drag_wakes_about_three_times`**: the same over 0..=1000 ms, the generation
  bumped every frame, including after a wake. Exactly 3 wakes, at 304, 624 and 944 (each wake
  restarts `first_seen`).
- **`wake_after_none_restarts_the_max_wait_clock`**: capped generations from 0 to 200, then `None`
  at 216, then capped generations from 232. The first wake comes at ≥ 532, not at 300.
- **`a_wake_fires_while_the_editor_is_unfocused`**: first event `ButtonPressed(Left)` with the
  cursor at `(600, 5)` (outside the 500-wide frame, so it unfocuses), then a frame at 0 with a
  delay-0 wake → `[1]`.
- **`rendering_another_document_resets_the_wake_timer`**: in the style of
  `rendering_another_document_resets_the_view_state_and_reveals`, set `wake`, `wake_first_seen`
  and `wake_fired` on a tree's state, `diff` with another document, assert all three are `None`.

**Gesture tests.** All use `hinted_doc()` or `padded_doc()` and `cell_point`. `rest_on(p)` is the
existing three-event idle rest.
- **`resting_on_a_hint_label_publishes_inlay_hover_not_hover_query`**: `rest_on(cell_point(8))`
  (on `i32`) → `InlayHover { key: 1, part: 1 }`, and no `HoverQuery`.
- **`resting_on_hint_padding_queries_nothing`**: `padded_doc`, `rest_on(cell_point(6))` → neither
  `InlayHover` nor `HoverQuery`.
- **`the_inlay_card_stays_open_while_the_pointer_stays_on_its_part`**: rest on cell 7, then pump
  `CursorMoved` to cell 9 with `inlay_tooltip(Some((key 1, 1, "**i32**")))`: no
  `HoverDismiss`. Then `CursorMoved` to cell 5 (part 0): `HoverDismiss`. A second run moves to
  cell 9 with no tooltip given: still no `HoverDismiss`, because the unanswered query is kept.
- **`hint_hover_arms_only_while_focused`**: unfocus with a press outside the frame, then
  `rest_on(cell_point(8))` → no `InlayHover`.
- **`ctrl_click_on_a_link_part_publishes_inlay_jump_and_captures`**: events
  `[ModifiersChanged(Modifiers::COMMAND), ButtonPressed(Left)]` at cell 8 → contains
  `InlayJump { key: 1, part: 1 }`, no `PlaceCaret`, and the press's status is `Captured`.
- **`ctrl_click_on_a_part_without_a_link_places_the_caret`**: the same at cell 5 → `PlaceCaret(5)`,
  no `InlayJump`.
- **`ctrl_over_a_link_part_shows_the_pointer_except_during_a_drag`**: `[ModifiersChanged(COMMAND),
  CursorMoved(cell 8)]` → `interaction == Pointer`. Then, fresh: `[ButtonPressed(Left)` at cell
  0, `ModifiersChanged(COMMAND)`, `CursorMoved(cell 8)]` → `interaction != Pointer`.
- **`a_single_click_on_a_hint_places_the_caret_at_its_offset`**: one press at cell 8 →
  `PlaceCaret(5)`.
- **`double_click_on_an_insertable_hint_publishes_inlay_insert`**: `[ButtonPressed,
  ButtonReleased, ButtonPressed]` at cell 8 in one pump → the last action is `InlayInsert { key:
  1, offset: 5 }`, with no `DragSelect`.
- **`double_click_on_a_hint_without_edits_places_the_caret_without_selecting`**: `padded_doc`, a
  double click on cell 4 → `PlaceCaret(<hint offset>)` twice, no `DragSelect`.
- **`gestures_on_a_stale_hint_set_do_nothing`**: `hinted_doc()`, then
  `doc.edit(vec![EditOp::insert(len, "\n")])` at the end so the hint keeps its cells but the set
  is stale.
  - `rest_on(cell 8)` → no `InlayHover`, no `HoverQuery`.
  - Ctrl+click on cell 8 → no `InlayJump` and no `PlaceCaret`, status `Captured`.
  - A double click on cell 8 → `PlaceCaret(5)`, no `InlayInsert`, no `DragSelect`.
  - Ctrl over cell 8 → `interaction != Pointer`.

### code_editor.rs (`mod tests`)

**Helpers.**

```rust
    /// The pending wait's generation (panics if none).
    fn wait_gen(ed: &CodeEditor) -> u64 {
        ed.inlays.wait.expect("a fetch is pending").generation
    }

    /// Fire the pending wait and return the request it recorded.
    fn fetch(ed: &mut CodeEditor) -> inlay::Request {
        act(ed, Action::Wake(wait_gen(ed)));
        ed.take_inlay_request().expect("the wake records a request")
    }

    /// An editor over `src` with hints on and `hints` installed through a real fetch.
    fn with_hints(src: &str, hints: Vec<inlay::Placed>) -> CodeEditor {
        let mut ed = CodeEditor::new(src).inlay_hints(true);
        let req = fetch(&mut ed);
        ed.set_inlays(req.ticket(), Some(hints));
        ed
    }

    /// The `: i32` hint after `x` in `let x = 1;` (key `k`, insertable, part 1 links).
    fn type_hint(k: u64) -> inlay::Placed { … }
```

**Tests.**
- **`hints_are_off_by_default_and_ask_for_nothing`**: `CodeEditor::new("a\n")`: `inlays.wait` is
  `None`; typing records no wait; `take_inlay_request()` is `None`.
- **`enabling_hints_waits_zero_and_a_wake_records_a_request_for_the_window`**: `.inlay_hints(true)`
  → `wait == Some(Wake { delay: ZERO, cap: None, .. })`. `fetch` → the request's ticket revision is
  the document's, and the span is the whole of a 10-line document.
- **`the_request_window_pads_one_view_above_and_two_below`**: `"x\n".repeat(1000)`, enabled,
  `ViewportChanged(400..436)`, fetch → span = bytes of rows 364..508 (`2 * 364 .. 2 * 508`). Also
  `ViewportChanged(980..1001)` → rows 951..1001.
- **`a_wake_for_an_old_generation_records_nothing`**: enable, note generation g, type a char (g+1),
  `Wake(g)` → no request; `Wake(g+1)` → request.
- **`an_edit_waits_three_hundred_ms_and_each_edit_restarts_it`**: enabled, fetch, type → `wait ==
  { delay: 300 ms, cap: None }` with generation g; type again → generation g+1. A caret move alone
  (`PlaceCaret`) changes nothing.
- **`accepting_a_completion_schedules_a_fetch`**: `.completions(OneCompletion).inlay_hints(true)`,
  fetch, type `h`, fire nothing, `PopupAccept` → the wait's generation moved past the typing's.
- **`a_refresh_wait_is_a_delay_not_a_deadline`** (D11's "idle for 10 s", editor half): enable and
  fetch at `t0`, then call `ed.wait_inlays(INLAY_EDIT_DELAY, None)` with no `update` in between, as
  Phase 6's `InlayRefresh` arm will 10 s later. `ed.inlays.wait` is `{ delay: 300 ms, cap: None }`,
  a duration with no clock in it, so the widget times it from the frame that first sees it. The
  widget half is `a_wake_counts_its_delay_from_the_first_frame_that_sees_it`.
- **`scrolling_inside_the_inner_window_asks_nothing`**: 1000 lines, `ViewportChanged(0..36)`,
  fetch (inner 0..72), `ViewportChanged(10..46)` → `wait` is `None`.
- **`scrolling_out_of_the_inner_window_waits_with_a_cap_and_re_requests`**: continuing,
  `ViewportChanged(40..76)` → `wait == { delay: 75 ms, cap: Some(300 ms) }`; fetch → span starts
  at row 4.
- **`triggers_are_ignored_while_disabled`**: hints off; type, `ViewportChanged`, `load`,
  `wait_inlays` → `wait` stays `None`.
- **`set_inlays_lands_only_under_the_awaited_ticket`**: two fetches (t1, then t2 after a
  trigger); `set_inlays(t1, …)` is dropped; `set_inlays(t2, Some([hint]))` installs
  (`document().inlays_revision() == Some(rev)`). A third `set_inlays(t2, …)` is dropped (settled).
- **`a_failed_fetch_keeps_the_shown_hints`**: install one hint, trigger + fetch,
  `set_inlays(t, None)` → the hint is still in the store, and the slot is settled
  (`awaiting.inlays` is `None`).
- **`an_empty_answer_clears_the_hints`**: `set_inlays(t, Some(vec![]))` → no hints.
- **`a_wake_keeps_the_completion_popup_open`**: `.completions(OneCompletion).inlay_hints(true)`,
  type `h` (popup open), `Wake(wait_gen)` → the popup is still `Open` and a request is recorded.
- **`inlay_actions_never_reach_apply`**: `with_hints` plus `.completions(OneCompletion)`. Open the
  popup with `TriggerCompletion`: Ctrl+Space doesn't move the revision, so the set stays current.
  Then send `InlayJump`, `InlayHover`, `InlayInsert` and `Wake(0)`. The popup stays open, the
  definition slot `InlayJump` armed is still awaited (`accepts(Definition, t)`), and so is the
  tooltip slot.
- **`inlay_hover_records_a_tooltip_interaction_and_the_answer_shows_a_keyed_card`**:
  `with_hints(.., [type_hint(1)])`, `InlayHover { key 1, part 1 }` → the interaction is a
  tooltip on part 1 under ticket t; `set_inlay_tooltip(t, Some("**i32**"))` → `inlay_card` has
  key 1, part 1, and `hover` is `None`. `set_inlay_tooltip(t, None)` on a fresh hover → no card.
- **`the_tooltip_card_survives_a_refetch_that_keeps_its_key`**: show the card, then trigger a
  refetch the way a refresh does, with `ed.wait_inlays(INLAY_EDIT_DELAY, None)`. A scroll or an edit
  would close the card on its own. Fetch, `set_inlays(t, Some([type_hint(1)]))` → card still
  shown.
- **`a_refetch_without_the_key_closes_the_tooltip_card`**: the same with `[type_hint(2)]` → card
  closed.
- **`inlay_jump_awaits_through_the_definition_slot`**: `InlayJump { key 1, part 1 }` → the
  interaction is a jump under t, `accepts(Awaited::Definition, t)`. `set_definition(t, Some(0..3))`
  selects `0..3`.
- **`inlay_insert_records_an_insert_interaction`**: `InlayInsert { key 1, offset 5 }` →
  `awaiting.inlay_insert == Some((t, key 1, 5))` and the interaction is an insert under t.
- **`a_newer_gesture_supersedes_the_pending_interaction`**: `InlayHover` then `InlayInsert` before
  any take → `take_inlay_interaction` is the insert. The tooltip slot still awaits, because only
  the unpulled gesture is replaced.
- **`interactions_on_a_stale_set_record_nothing`**: install, type at the end of the document (the
  set is stale), then each of the three actions → `take_inlay_interaction()` is `None` and no slot
  is set.
- **`disabling_hints_clears_the_store_the_slots_the_card_and_the_wait`**: install, show a card,
  record an insert, trigger a wait; `set_inlay_hints(false)` → no hints, `inlay_card` `None`, the
  three slots `None`, `wait` `None`, `take_inlay_interaction()` `None`.
- **`hover_dismiss_retires_the_inlay_tooltip`**: `InlayHover` (t), `HoverDismiss`,
  `set_inlay_tooltip(t, Some(..))` → no card.
- **`scrolling_retires_the_inlay_tooltip`**: the same with `ViewportChanged`; a shown card closes
  too.
- **`an_edit_retires_the_inlay_tooltip_and_insert`**: record both, type → both slots `None`, the
  card is closed. `accepts(InlayInsert, t)` is false.
- **`load_clears_the_hints_and_waits_zero`**: install, `load("new\n", None)` → no hints, `wait ==
  { delay: ZERO, .. }`.
- **`pending_wake_is_the_wait_the_widget_is_handed`**: off → `pending_wake()` is `None`;
  `.inlay_hints(true)` → `Some(Wake { delay: ZERO, cap: None, .. })`, equal to `inlays.wait`;
  `Action::Wake(pending_wake().generation)` records a request and `pending_wake()` is `None`.
- **`a_folded_viewport_pads_its_window_in_display_rows`** (R20): 1000 lines, a collapsed block
  fold with header row 10 and last row 500 (490 hidden rows), `ViewportChanged(0..520)`, i.e.
  display rows 0..30 → the window is display rows 0..90, so the span covers buffer rows 0..580
  (not 0..1000, which buffer-row pads would give), the fold's interior included; the inner window
  is buffer rows 0..550.

## 7. Commit boundaries

Base: the commit Phase 4 ended on. Each boundary must be green on its own (DISPATCH override 2):
patch `phase5-<k>.patch`, message `phase5-<k>.msg`.

| # | Content | Proposed subject |
|---|---|---|
| 5-1 | Step 1: `Wake`, `Action::Wake`, `Editor::wake_after`, `drive_wake`, State fields, the inert `CodeEditor` arm and `apply` entry, lib export; the eight timer tests | `feat(iced)!: Editor::wake_after wakes the host after a delay` |
| 5-2 | Steps 2–5: the three gesture actions, `inlay_hit`, the keyed card (`inlay_tooltip`, `HoverTarget`, `open_card`), `still_in`, link underline and pointer, double-click, the widget's D14 gate, the inert `CodeEditor` arms; the eleven gesture tests | `feat(iced)!: hover, Ctrl+click and double-click on inlay hints` |
| 5-3 | Step 6: the toggle, scheduler, slots, `take_*`/`set_*`, `pending_wake`, card wiring in `view`, `after_edit`/`accept_completion`/`load` triggers; the code_editor tests | `feat(iced): inlay hints in CodeEditor, fetched on a debounce` |

The `!` marks new variants on the public, exhaustive `Action`. They ride the unreleased 0.4.0.
Message bodies say why (e.g. 5-1: "Triggers arriving outside `update` have no current clock, so the
widget times the wait from its own frames.").

## 8. Verification

```
cargo test --workspace
cargo test --workspace --all-features
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --all-features
cargo build --workspace --all-targets --all-features --target wasm32-unknown-unknown
cargo test -p scrive-iced -- wake inlay hint ctrl_ double_click single_click stale
cargo test -p scrive-iced --all-features -- code_editor
grep -n "allow(dead_code)" crates/scrive-iced/src/*.rs   # no hits
```

## 9. Spot-check tables

### `drive_wake` on a fake clock

| Frames (ms) and the wake passed | Wakes published |
|---|---|
| g1 {300, —} at 0, 299, 300, 400 | `1` at 300 only |
| g1 at 0, g2 {300, —} from 200 | `2` at 500 |
| none at 0 and 10 000; g1 {300, —} from 10 000 | `1` at 10 300 |
| g = frame index every 16 ms, {75, cap 300} | 304, 624, 944 |
| g1 {0, —} at 0 | `1` at 0 |
| g1 {300, —} at 0; same g1 after the wake | nothing more |
| capped gens 0–200, `None` at 216, capped from 232 | first at ≥ 532 |

### `inlay_window` (1001-line document unless noted)

| `viewport` | window rows | inner |
|---|---|---|
| 0..0 (before any report) | 0..50 | 0..25 |
| 0..36 | 0..108 | 0..72 |
| 400..436 | 364..508 | 382..472 |
| 980..1001 | 951..1001 | 965..1001 |
| 11-line doc, 0..11 | 0..11 | 0..11 |

### Gestures on a hint cell

| Gesture | Current set, label | Current set, padding | Stale set (any hint cell) |
|---|---|---|---|
| rest (focused) | `InlayHover` | nothing | nothing |
| rest (unfocused) | nothing | nothing | nothing |
| click | `PlaceCaret(hint offset)` | `PlaceCaret(hint offset)` | `PlaceCaret(hint offset)` |
| double-click, insertable | `InlayInsert` | `PlaceCaret` | `PlaceCaret` |
| double-click, not insertable | `PlaceCaret` | `PlaceCaret` | `PlaceCaret` |
| Ctrl+click, link part | `InlayJump`, captured | `PlaceCaret` | captured, nothing published |
| Ctrl+click, no link | `PlaceCaret` | `PlaceCaret` | `PlaceCaret` |
| Ctrl held over a link | underline + pointer | — | — |
| Ctrl held over a link during a drag | nothing | — | — |

### `CodeEditor` slots

| Event | `inlays` | `inlay_tooltip` | `inlay_insert` | card | wait |
|---|---|---|---|---|---|
| `Wake(g)` matching | set | — | — | — | cleared |
| `set_inlays` accepted | cleared | — | — | closed if its key left | — |
| `InlayHover` (current) | — | set | — | closed | — |
| `set_inlay_tooltip` accepted | — | cleared | — | `Some` → shown | — |
| `InlayJump` (current) | — | — | — | — | — (`definition` set) |
| `InlayInsert` (current) | — | — | set | — | — |
| `HoverDismiss`, `HoverQuery` | — | cleared | — | closed | — |
| `ViewportChanged` | — | cleared | — | closed | 75/300 if it leaves the inner rows |
| edit (revision moved) | — | cleared | cleared | closed | 300 |
| `set_inlay_hints(false)` | cleared | cleared | cleared | closed | cleared |
| `load` | cleared | cleared | cleared | closed | 0 |

## 10. What NOT to change

- No scrive-core or scrive-lsp code. If a Phase 1–3 API this doc assumes is missing or differs,
  adapt to it; if it can't express what a step needs, stop and report.
- Don't touch `code_editor/lsp.rs`. The `land` arms are Phase 6. The `sync_lsp` pull, the insert
  removal, `close_lsp` and the `open_lsp` trigger are Phase 7 (R19).
- Don't change how hints are painted or laid out (Phases 3–4), `Rows`, `Edge` or any projection.
- Don't add a key binding for the toggle, and add no subscription: the timer is `wake_after` only.
- Don't change `HoverInfo` or the word-hover flow beyond the `card_or_query` / `open_card`
  refactor. `hover_pending` keeps its meaning.
- Don't make `HoverTarget`, `InlayPart`, `Inlays` or `InlayCard` public.
- Never run `cargo fmt`.

## 11. Pitfalls

- **Arm order in `CodeEditor::update`.** `Wake` and the three `Inlay*` arms go before the
  catch-all `Event::Editor(action)`. A fall-through compiles (the no-op list is exhaustive) and
  silently closes the popup and retires slots. `inlay_actions_never_reach_apply` and
  `a_wake_keeps_the_completion_popup_open` catch it.
- **`drive_wake` runs outside `is_focused()`.** Placed inside the hover block, an unfocused editor
  (a click in the find bar) never fetches.
- **Restamp, then check.** Checking `now ≥ at` against the previous generation's instant fires a
  restarted wait early.
- **`first_seen` resets only on a wake or `None`**, never on a generation change, or the drag test
  never wakes.
- **Double clicks in tests.** `mouse::Click::new` stamps `Instant::now()` itself, and two clicks
  count as consecutive only if the second is strictly later and within 300 ms and 6 px
  (core/src/mouse/click.rs). Send both presses in one `pump_editor` call; don't reuse a stale
  cache across a sleep.
- **`ViewportChanged` is published first.** Every widget event re-reports the viewport at the top
  of `update`, so assert with `contains` / `wakes()`, not on `actions[0]`.
- **`Rows` borrows.** `Document::rows()` holds a `Ref` into the fold cache. Take one per arm and
  pass it down. Don't hold it across a call that refreshes the cache (Phase 2's rule).
- **`accept_completion` and `load` skip `after_edit`.** Both need their own call (Step 6i), or
  accepting a completion or loading a file leaves the hints unfetched.
- **Dead code under `-D warnings`.** No `Trigger` enum with a `Refresh` variant: Phase 6 calls
  `wait_inlays` directly. `Inlays::window` stores only the inner rows, which are read; a stored
  outer range would be write-only. `Awaited::InlayInsert` is constructed by the `abandon` calls,
  and its field is read by `accepts`.
- **`Action: PartialEq`.** `inlay::Key` must derive `PartialEq` (and `Debug`, `Clone`). If Phase 1
  didn't, that is a Phase 1 fix to report, not a reason to drop the derive on `Action`.
- **`HoverLayout` call sites.** The draw path, the wheel handler, `still_in` and
  `popup_anchors_are_display_space_below_a_fold` all go through `card_layout`. Don't leave a second
  anchor computation behind.
- **`request_redraw_at` keeps the minimum**, so the blink, hover and wake timers coexist. Don't
  replace the redraw request (`Shell::replace_redraw_request`).
- **`wait_inlays` from the builder.** `inlay_hints(true)` runs before any `update`, so the first
  wake's window comes from `viewport == 0..0` unless a `ViewportChanged` lands first. In a real UI
  the frame that fires the wake publishes `ViewportChanged` before `Wake` (both in one
  `RedrawRequested`), so the host sees the real viewport first.

## 12. Resolved questions

1. **`inlay_at`'s result:** `Rows::inlay_at(row: BufferRow, cell: f32) -> Option<inlay::At>`
   with `Label { key, part, offset, link, insert, cells }` and `Padding { key, offset }` (R3),
   added by Phase 3. Padding is inert.
2. **Phase 1 constructors:** `Request::new(ticket, span)`; `Interaction::{tooltip(ticket, key,
   part: u32), jump(ticket, key, part), insert(ticket, key, offset)}` and `ticket()`;
   `Key::new(u64)`; `Placed::hint()`, `Hint::key()` (R1, R6).
3. **The tooltip slot's part** is `u32`: `inlay_tooltip: Option<(Ticket, Key, u32)>`,
   `Editor::inlay_tooltip(Option<(Key, u32, &str)>)` (R6).
4. **`InlayHover`'s `offset`** is dropped: `InlayHover { key, part }` (R7).
5. **`open_lsp`'s wait 0** belongs to Phase 7 (R19).
6. **The keyed card** stays widget-private behind `Editor::inlay_tooltip` (R21).
7. **Inner half** confirmed (R20).
8. **Folded viewports:** pads in display rows, converted to buffer rows; hidden interiors inside
   the window are requested (R20). `inlay_window` and its test implement it.
9. **A refetch without the card's key** closes it (R21).
10. **Ctrl vs Cmd:** `modifiers.command()` (R21).
11. **`set_inlay_hints(true)` while on** is a no-op (R21).
12. **The plan's `Inlays` struct** no longer lists `due` (R23).

Also settled: `wait_inlays(delay, cap)` is the one scheduler, keyed on the stored revision and
called from `after_edit`, `accept_completion` and `load` (R9); `pending_wake()` is public (R10).

Still open: none.
