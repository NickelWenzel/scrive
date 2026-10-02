# Phase 2 — the async seam: tickets, abandonment, parity, completion items, Ctrl+Space

Read `MAP_PLAN.md` first (Current state → "The async seam", D11, D13, D14's `Start` rule, D17's
Ctrl+Space binding, Constraints). This doc specifies Phase 2 only. Line numbers were read at
HEAD `6cf4f2c`; Phase 1 does not touch any file below except code_editor.rs (only
`observe_changes`/`drain_changes` at :581-595 and its test), so they still hold. Re-grep anyway.

## 1. Prerequisites

- **Phase 1 is committed** (and its Step 0 clippy fix). Verify:
  - `grep -n "pub fn doc_id\|pub fn select_and_reveal\|pub struct Changes" crates/scrive-core/src/document.rs`
    finds all three;
  - `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` are green.
- Confirm the gaps are still open:
  - `CompletionRequest`/`SignatureRequest`/`HoverRequest` are defined in
    crates/scrive-iced/src/code_editor.rs:227-269 with a public `revision` field;
  - `set_completions` (code_editor.rs:542) takes a `Revision`;
  - `interpret_key` maps every Space to `Type(' ')` (editor.rs:3510);
  - `accept_completion` calls `insert_text` (code_editor.rs:1441, :1448).

## 2. Goal and exit criteria

**Goal.** Make the editor's async request seam precise enough for a language server: every
request carries a `Ticket`, each service has one awaited slot, a reply lands only through one
gate, and every way a user abandons a request clears its slot. Async completion behaves like the
synchronous path (dismissal, refilter). Completion items carry a filter text, additional edits
and a signature-help follow-up, and accepting one is a single explicit batch. Ctrl+Space asks for
completions.

**When this phase is done** (all tests drive the real `CodeEditor::update` / widget paths):

1. A reply is accepted only when its ticket is the awaited one **and** its revision is current.
   - `a_click_after_a_trigger_char_drops_the_stale_completion` — `Type('.')` → `PlaceCaret` →
     the reply for the `.` ticket is dropped.
   - `a_reply_whose_revision_moved_is_dropped_even_with_the_awaited_ticket` — the revision-only
     case (hover ticket still awaited, a typed char moved the revision).
   - `two_manual_invokes_at_one_revision_accept_only_the_second` — two Ctrl+Space presses at one
     revision.
2. Abandonment follows the D11 table.
   - `moving_off_an_unanswered_hover_publishes_hover_dismiss` (widget, editor.rs) — hover with
     no card: moving away emits `HoverDismiss`.
   - `a_rearm_inside_the_pending_word_keeps_the_hover_request` (widget, editor.rs) — a re-arm
     inside the in-flight word keeps it.
   - `hover_dismiss_abandons_the_pending_hover` (code_editor.rs).
   - `a_signature_reply_after_a_click_away_is_dropped`.
   - `typing_through_a_call_before_the_reply_still_opens_the_signature_box` — `foo(a` typed
     fast.
   - `escape_retires_an_awaited_signature_without_re_querying` — `Collapse` does not re-query.
3. `Start` sampling and deletions.
   - `deleting_back_to_an_empty_word_starts_the_next_request_fresh` — `f` ⌫ `g` samples
     `Fresh`.
   - `a_deletion_while_awaited_re_requests_completion`.
4. Parity with sync mode.
   - `typing_while_the_popup_is_open_refilters_before_the_reply_lands`.
   - `a_dismissed_popup_asks_for_nothing_until_a_trigger`.
   - `set_items_respects_an_escape_dismissal` (completion.rs).
   - `refilter_narrows_an_open_popup_and_closes_when_nothing_matches` (completion.rs).
5. Completion items and accept.
   - `accepting_a_retrigger_item_opens_the_popup_when_the_reply_lands`.
   - `an_auto_import_above_a_snippet_keeps_the_tab_stops_on_the_placeholders`.
   - `accepting_a_signature_after_item_asks_for_signature_help`.
   - `matches_uses_the_filter_text_when_set` and
     `completion_item_builder_defaults_and_overrides` (extended) in providers.rs.
6. Ctrl+Space and tickets.
   - `ctrl_space_is_trigger_completion` (editor.rs, `interpret_key`).
   - `tickets_at_one_revision_are_distinct` (ticket.rs).
7. The existing async tests pass on tickets (`async_completion_request_and_ingest_round_trip`,
   `stale_set_completions_is_dropped`, `async_snippet_item_starts_a_session_on_accept`,
   `typing_open_paren_records_async_signature_request`,
   `hover_over_a_word_records_async_request`), and the whole workspace is clippy/doc clean.

## 3. Design decisions implemented here

- **D11 — tickets and one gate.** scrive-core's `Ticket` has private fields (a per-editor
  monotonic `seq` and the `Revision`), a `revision()` accessor, and equality. The editor keeps
  one awaited slot per kind: completion `Option<Ticket>`, signature `Option<Ticket>`, hover
  `Option<(Ticket, u32 offset, Range<u32> word)>` (definition arrives in Phase 3). One private
  owner, `CodeEditor::accepts(kind, ticket)`, accepts only when
  `Some(ticket) == awaited && ticket.revision() == doc.revision()`. `set_completions`,
  `set_signature` and `set_hover` take a `Ticket` and go through it; `set_diagnostics` keeps its
  revision check. An accepted `None` clears the signature and hover slots. Completion landings
  never clear the slot (continuation, Phase 6, can deliver twice under one ticket); an empty list
  closes the popup and the slot retires at the next edit (it is replaced or abandoned there).
- **D11 — the abandonment table**, implemented exactly:

  | Event | completion | signature | hover |
  |---|---|---|---|
  | CaretOrClose (moves, clicks, paste, undo, `edit`) | clear | re-query if showing or awaited | clear |
  | a deletion that empties the word; a boundary `Type` | clear | — | — |
  | PopupDismiss; accept (inside `accept_completion`, *before* a retrigger records) | clear | — | — |
  | SignatureClose; `Collapse` (dedicated arm, *before* `after_edit`) | — | clear | — |
  | HoverDismiss; a `HoverQuery` that records nothing | — | — | clear |
  | ViewportChanged | — | — | clear |

  "Clear" drops both the awaited ticket and the not-yet-pulled pending request.
  A deletion re-requests completion when the popup is open **or** a completion is awaited.
  `drive_signature` queries on `Typed('(') || signature.is_some() || awaiting.signature.is_some()`.
  The widget publishes `HoverDismiss` whenever it cancels a pending or open hover
  (`State::hover_queried`), and `Editor::hover_pending(Option<Range<u32>>)` passes the in-flight
  word so a re-arm inside it doesn't cancel.
- **Request types move to scrive-core** (one module per service, established `…Request` names):
  - `intel/ticket.rs`: `Ticket`, and `ticket::Counter` that mints them.
  - `intel/completion.rs`: `CompletionRequest` with **private** fields,
    `new(ticket, word, trigger, start)` and accessors; `intel::completion::Start { Fresh,
    Continuing }`. No position field: `word.end` is the caret.
  - `intel/signature.rs`: `SignatureRequest { ticket, position }`, **private** fields, `new` and
    accessors (Phase 3 adds `call`).
  - `intel/hover.rs`: `HoverRequest { ticket, offset, word }` with `new`.
  - scrive-iced re-exports the three from `code_editor` (the one allowed shim), and every doc link
    in scrive-core resolves inside scrive-core.
- **D14 (part) — `Start`** is sampled before the refilter and is `Continuing` only if the popup
  is open or a completion is awaited.
- **Parity with sync mode.** Dismissed + `Typed` → no request. Open + `Typed` → `refilter`, then
  record. A trigger char or a manual invoke clears the dismissal, and `set_items` respects a
  dismissal.
- **D13 (Phase 2 part) — `CompletionItem`** gains `filter: Option<String>`,
  `additional: Vec<EditOp>`, `signature_after: bool` (all through builders) and `matches(word)`,
  the one public filtering predicate `refilter` uses. **Accept:** the main op and the additional
  edits go into one `edit` batch sorted by `(start, end)`; additional edits touching the closed
  interval `[replace.start, replace.end]` are dropped, and those at or past the popup anchor
  shift by the live caret delta; the snippet base and plain-insert caret come from the main
  edit's patch `new.start`; the indent is read before the edit; no `insert_text`; a `Some` range
  whose end lies inside the live word extends to the caret; `signature_after` runs
  `drive_signature` as if `(` had been typed.
- **Retrigger and Ctrl+Space.** Retrigger calls `request_completions(Manual)`, so it works with
  async hosts. Ctrl+Space maps to the new `Action::TriggerCompletion`.
- **Stale docs** fixed at intel.rs:3,7-10, providers.rs:49-53, hover.rs:2-6, signature.rs:6-9.

**Decisions this doc makes where the plan is silent:**

- Decision: tickets are minted by `intel::ticket::Counter` (`issue(&mut self, Revision) ->
  Ticket`), one per `CodeEditor`. Private fields plus a public minter is the only way scrive-iced
  (and later scrive-lsp tests) can create tickets without exposing a raw constructor.
- Decision: `HoverRequest` is plain data — `#[non_exhaustive]` with public fields and `new` —
  matching today's request style; only `CompletionRequest` and `SignatureRequest` are in the
  plan's list of invariant types with private fields.
- Decision: `CompletionItem::matches(word)` is a case-insensitive (ASCII) prefix match on
  `filter` when set, else on `label` (Risk 3's "on `filter` or the label").
- Decision: a `TriggerChar` request in async mode **closes** the popup (the open list described
  the word the trigger char just ended; refiltering it with the new empty word would show every
  stale item). `Manual` only clears a dismissal and leaves an open popup until the reply
  replaces it.
- Decision: the "live caret delta" is measured against `items_caret`, a new private
  `CodeEditor` field holding the caret the current item list was produced against (set when
  `set_completions` lands and when a sync provider produces a fresh list). `PopupList` is not
  changed. Additional edits are shifted first, then the touch filter runs in live coordinates.
- Decision: if the accept batch is rejected (`TransactionError::Overlap` from a malformed set of
  additional edits), accept retries with the main op alone, so a bad auto-import never costs the
  user the completion.
- Decision: in this phase `set_hover` still *replaces* the card (today's behavior) behind the
  ticket gate. Merging diagnostics into the card (`hover_card`) and escaping are Phase 3.

## 4. Step-by-step changes

Order: scrive-core first (it must compile alone), then editor.rs, then code_editor.rs.

### Step 1 — `intel/ticket.rs` (new file) and `intel.rs`

crates/scrive-core/src/intel/ticket.rs:

```rust
//! Request tickets — the stamp an async language-service reply must carry to
//! land. A ticket names one request: the revision it was asked at plus a
//! sequence number unique within the editor that issued it, so two requests at
//! the same revision (Ctrl+Space pressed twice) are still told apart and only
//! the newer reply lands.

use crate::buffer::Revision;

/// One request's identity. Opaque: only a [`Counter`] mints tickets, and the
/// only readable part is the [`revision`](Self::revision) the request was made
/// at. Two tickets are equal only if they are the same request.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Ticket {
    seq: u64,
    revision: Revision,
}

impl Ticket {
    /// The document revision the request was made at. A reply computed for it
    /// is only meaningful while the document is still at this revision.
    #[must_use]
    pub fn revision(&self) -> Revision {
        self.revision
    }
}

/// Mints [`Ticket`]s with a sequence number that never repeats for this
/// counter. One counter per editor: tickets from one editor never collide.
#[derive(Debug, Default)]
pub struct Counter {
    issued: u64,
}

impl Counter {
    /// A counter that has issued nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Issue a fresh ticket for a request made at `revision`.
    #[must_use]
    pub fn issue(&mut self, revision: Revision) -> Ticket {
        self.issued += 1;
        Ticket { seq: self.issued, revision }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two requests at one revision get different tickets that both report
    /// that revision — the property that drops the first of two Ctrl+Space replies.
    #[test]
    fn tickets_at_one_revision_are_distinct() {
        let mut counter = Counter::new();
        let (a, b) = (counter.issue(Revision(3)), counter.issue(Revision(3)));
        assert_ne!(a, b, "each request is its own ticket");
        assert_eq!((a.revision(), b.revision()), (Revision(3), Revision(3)), "both remember the revision");
        assert_eq!(a, a, "a ticket equals itself");
    }
}
```

crates/scrive-core/src/intel.rs — current (whole file):

```rust
//! Language services — completion, signature help, hover.
//!
//! In-process, no LSP. Each service is **one small trait defined here in the
//! core and satisfied by the app** — not a god-trait with no-op defaults; a
//! service ruled out of scope is simply not built. The plain data the traits
//! exchange lives here too, so a provider never reaches an editor internal.
//! Completion is **synchronous by contract** (the classifier is a regex
//! ladder over a few dozen lines — microseconds), so the widget calls the
//! provider in `update()` and opens the popup from the returned `Vec` the same
//! frame; no async reply, no revision guard.
//!
//! The controller state machines (completion / snippet session) that consume
//! these providers are core view-state and land in the submodules below.

pub mod completion;
pub mod hover;
pub mod providers;
pub mod signature;
pub mod snippet;
```

New:

```rust
//! Language services — completion, signature help, hover.
//!
//! Each service has two ways in. A **provider** is one small trait defined here
//! and satisfied by the app (not a god-trait with no-op defaults): the editor
//! calls it in `update()` and shows the answer the same frame. A host whose
//! answers come from elsewhere — a background thread, a language server —
//! leaves the provider unset; the editor then records a **request** stamped
//! with a [`Ticket`](ticket::Ticket), and the reply lands only if it carries the
//! ticket the editor still awaits, at the same revision. The plain data both
//! paths exchange lives here, so neither reaches an editor internal.
//!
//! The controller state machines (completion / snippet session) that consume
//! these providers are core view-state and land in the submodules below.

pub mod completion;
pub mod hover;
pub mod providers;
pub mod signature;
pub mod snippet;
pub mod ticket;
```

### Step 2 — `CompletionItem` additions (crates/scrive-core/src/intel/providers.rs)

2a. Imports, providers.rs:5-7 — add `use crate::transaction::EditOp;` after
`use crate::{DocId, Point};`.

2b. Stale doc, providers.rs:49-53. Current:

```rust
/// The completion seam. **Synchronous by contract** (see the module docs): the
/// widget calls `complete()` in `update()` and opens/refreshes the popup from
/// the returned `Vec` the same frame — no async reply, no revision guard. A
/// genuinely slow provider would motivate an async variant of the seam; the
/// synchronous contract holds until one is needed.
```

New:

```rust
/// The synchronous completion seam: the editor calls `complete()` in
/// `update()` and opens or refreshes the popup from the returned `Vec` the same
/// frame. A source that can't answer that fast doesn't implement this; it
/// answers the editor's
/// [`CompletionRequest`](crate::intel::completion::CompletionRequest) instead.
```

2c. Fields: append to `pub struct CompletionItem` after `retrigger` (providers.rs:118-121):

```rust
    /// The text the popup filters on, when it differs from the label (a
    /// language server's `filterText`, e.g. `printf` for a `•printf(…)` label).
    /// `None` filters on the label. See [`matches`](Self::matches).
    pub filter: Option<String>,
    /// Edits elsewhere in the document that accepting the item also applies —
    /// an auto-import at the top, say — in the same coordinates as
    /// [`replace`](Self::replace). Applied in the same transaction as the
    /// insertion, so one undo reverts both. An edit touching the replaced word
    /// is dropped.
    pub additional: Vec<EditOp>,
    /// When set, accepting the item asks for signature help as if `(` had just
    /// been typed — for items that insert a call's opening paren.
    pub signature_after: bool,
```

`CompletionItem::new` (providers.rs:128-140) initializes them: `filter: None,
additional: Vec::new(), signature_after: false,`. Update its doc to "detail/doc/replace/filter
default absent; `retrigger` and `signature_after` false; no additional edits."

2d. Builders and the predicate, appended to `impl CompletionItem` after `with_retrigger`
(providers.rs:177-182):

```rust
    /// Filter on `filter` instead of the label.
    #[must_use]
    pub fn with_filter(mut self, filter: impl Into<String>) -> Self {
        self.filter = Some(filter.into());
        self
    }

    /// Edits elsewhere that accepting the item also applies (see
    /// [`additional`](Self::additional)).
    #[must_use]
    pub fn with_additional(mut self, additional: Vec<EditOp>) -> Self {
        self.additional = additional;
        self
    }

    /// Ask for signature help after accepting the item.
    #[must_use]
    pub fn with_signature_after(mut self, signature_after: bool) -> Self {
        self.signature_after = signature_after;
        self
    }

    /// Whether the item survives filtering by `word`: its filter text (or,
    /// without one, its label) starts with `word`, folding ASCII case —
    /// completion labels are code identifiers. The one filtering rule: the
    /// popup's refilter and any list reuse both use it.
    #[must_use]
    pub fn matches(&self, word: &str) -> bool {
        prefix_matches(self.filter.as_deref().unwrap_or(&self.label), word)
    }
```

2e. Move `prefix_matches` (completion.rs:58-65, doc and body unchanged) into providers.rs as a
private free fn after `is_completion_word_char`, and delete it from completion.rs.

### Step 3 — `CompletionRequest`, `Start`, parity (crates/scrive-core/src/intel/completion.rs)

3a. Imports, completion.rs:8. Current:

```rust
use super::providers::{CompletionCx, CompletionItem, CompletionTrigger, Completions};
```

New:

```rust
use core::ops::Range;

use super::providers::{CompletionCx, CompletionItem, CompletionTrigger, Completions};
use super::ticket::Ticket;
```

Update the module doc (completion.rs:1-6) by appending one sentence: "It also defines the
[`CompletionRequest`] an editor records when no provider is set."

3b. After `CompletionState`/before `PopupList`, add:

```rust
/// Whether a completion request continues the list already on screen (or on
/// its way) or starts a new one. The editor samples it before it refilters, so
/// a list reuse knows the popup was live when the request was made.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Start {
    /// No popup was open and none was awaited: ask from scratch.
    Fresh,
    /// A popup was open, or a completion was awaited: this request extends it.
    Continuing,
}

/// A completion request an editor records for an async source (a background
/// thread, a language server) when no synchronous [`Completions`] provider is
/// set. The host pulls it with the editor's `take_completion_request`, answers
/// it, and hands the items back with the editor's `set_completions` together
/// with [`ticket`](Self::ticket); a reply whose ticket the editor no longer
/// awaits is dropped.
#[derive(Clone, Debug)]
pub struct CompletionRequest {
    ticket: Ticket,
    word: Range<u32>,
    trigger: CompletionTrigger,
    start: Start,
}

impl CompletionRequest {
    /// A request for the completion word `word` (whose end is the caret), made
    /// under `ticket` because of `trigger`.
    #[must_use]
    pub fn new(ticket: Ticket, word: Range<u32>, trigger: CompletionTrigger, start: Start) -> Self {
        Self { ticket, word, trigger, start }
    }

    /// The ticket the reply must carry.
    #[must_use]
    pub fn ticket(&self) -> Ticket {
        self.ticket
    }

    /// The completion word under the caret (empty at a boundary). Its end is
    /// the caret; its start is where an item's insertion begins by default.
    #[must_use]
    pub fn word(&self) -> Range<u32> {
        self.word.clone()
    }

    /// What caused the request.
    #[must_use]
    pub fn trigger(&self) -> CompletionTrigger {
        self.trigger
    }

    /// Whether the request continues the current list.
    #[must_use]
    pub fn start(&self) -> Start {
        self.start
    }
}
```

3c. `PopupList::refilter`, completion.rs:46-55: replace the filter closure

```rust
                .filter(|&i| prefix_matches(&self.items[i as usize].label, word)),
```

with `.filter(|&i| self.items[i as usize].matches(word)),` and change the doc's first sentence to
"Rebuild `filtered` as the items that [`match`](CompletionItem::matches) `word`, …".

3d. `CompletionController::on_input`, completion.rs:106-112. Current:

```rust
            CompletionTrigger::Typed(_) => match &mut self.state {
                CompletionState::Open(list) => {
                    list.refilter(word);
                    if list.filtered.is_empty() {
                        self.state = CompletionState::Closed;
                    }
                }
                CompletionState::DismissedUntilBoundary => {} // stay dismissed
```

New:

```rust
            CompletionTrigger::Typed(_) => match self.state {
                CompletionState::Open(_) => self.refilter(word),
                CompletionState::DismissedUntilBoundary => {} // stay dismissed
```

(the `Closed` arm is unchanged; `match self.state` with non-binding patterns moves nothing).

3e. `set_items`, completion.rs:128-137. Current body: `self.set_from_items(items, word, anchor);`.
New:

```rust
    /// Ingest an externally-produced item list — an off-thread or
    /// language-server completion result — as if a provider had returned it:
    /// open and filter against the live `word` (with `anchor` the word start),
    /// or close if nothing matches. An Escape dismissal wins: a reply for the
    /// word the user dismissed stays hidden, as a provider's would. The
    /// controller stays document-agnostic; the caller owns staleness.
    pub fn set_items(&mut self, items: Vec<CompletionItem>, word: &str, anchor: u32) {
        if matches!(self.state, CompletionState::DismissedUntilBoundary) {
            return;
        }
        self.set_from_items(items, word, anchor);
    }

    /// Narrow an open popup to the items matching the live `word`, closing it
    /// when none do — the local half of typing while a list shows. A no-op when
    /// closed or dismissed.
    pub fn refilter(&mut self, word: &str) {
        if let CompletionState::Open(list) = &mut self.state {
            list.refilter(word);
            if list.filtered.is_empty() {
                self.state = CompletionState::Closed;
            }
        }
    }
```

### Step 4 — `SignatureRequest` (crates/scrive-core/src/intel/signature.rs)

4a. Stale doc, signature.rs:1-9. Replace lines 6-9

```rust
//! Synchronous by contract, same rationale as `Completions`: the query is
//! an `enclosingCall` + active-parameter count over a few lines of lookback —
//! microseconds — so the widget calls it and renders the reply the same frame,
//! with no reply envelope to go stale.
```

with

```rust
//! A provider answers synchronously, the same frame. With no provider set, the
//! editor records a [`SignatureRequest`] instead; its reply lands only under the
//! request's ticket, so an answer for a call the caret has left is dropped.
```

4b. Imports: `use crate::{DocId, Point};` → add `use crate::intel::ticket::Ticket;`.

4c. Append after `impl SignatureInfo`:

```rust
/// A signature-help request an editor records for an async source when no
/// synchronous [`SignatureHelp`] provider is set. Answer it through the editor's
/// `set_signature` with [`ticket`](Self::ticket); `None` closes the box.
#[derive(Clone, Debug)]
pub struct SignatureRequest {
    ticket: Ticket,
    position: Point,
}

impl SignatureRequest {
    /// A request for signature help at `position` (the caret), made under
    /// `ticket`.
    #[must_use]
    pub fn new(ticket: Ticket, position: Point) -> Self {
        Self { ticket, position }
    }

    /// The ticket the reply must carry.
    #[must_use]
    pub fn ticket(&self) -> Ticket {
        self.ticket
    }

    /// The caret position (row, byte column) to query at.
    #[must_use]
    pub fn position(&self) -> Point {
        self.position
    }
}
```

### Step 5 — `HoverRequest` (crates/scrive-core/src/intel/hover.rs)

5a. Stale doc, hover.rs:1-6. Current:

```rust
//! Hover — the [`Hover`] trait the app satisfies plus the plain data it
//! returns. Like the other language services it is **synchronous by contract**:
//! a hover query is an in-memory lookup keyed by the word under the pointer,
//! so the widget calls it on the mouse-idle tick and renders the reply the same
//! frame. Because the answer is produced synchronously there is no reply
//! envelope and nothing in flight, so a reply can never arrive stale.
```

New:

```rust
//! Hover — the [`Hover`] trait the app satisfies, the plain data it returns,
//! and the [`HoverRequest`] an editor records when no provider is set. A
//! provider answers on the mouse-idle tick and the card shows the same frame.
//! An async answer lands only under the request's ticket, so a card for a word
//! the pointer has left, or for text that has changed, is dropped.
```

5b. Imports: add `use crate::intel::ticket::Ticket;`. `Range` is imported (hover.rs:8).

5c. Append:

```rust
/// A hover request an editor records for an async source when no synchronous
/// [`Hover`] provider is set. Answer it through the editor's `set_hover` with
/// `ticket`; `None` means no docs.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct HoverRequest {
    /// The ticket the reply must carry.
    pub ticket: Ticket,
    /// The byte offset the pointer rested over.
    pub offset: u32,
    /// The word under the pointer (never empty — an empty word asks nothing).
    pub word: Range<u32>,
}

impl HoverRequest {
    /// A request for docs on `word`, asked at `offset` under `ticket`.
    #[must_use]
    pub fn new(ticket: Ticket, offset: u32, word: Range<u32>) -> Self {
        Self { ticket, offset, word }
    }
}
```

### Step 6 — crate-root exports (crates/scrive-core/src/lib.rs:82-84)

Current:

```rust
pub use intel::completion::{CompletionController, CompletionState, PopupList};
pub use intel::hover::{Hover, HoverCx, HoverInfo, HOVER_IDLE_DELAY_MS};
pub use intel::signature::{SignatureCx, SignatureHelp, SignatureInfo};
```

New (plus one line for the ticket):

```rust
pub use intel::completion::{CompletionController, CompletionRequest, CompletionState, PopupList};
pub use intel::hover::{Hover, HoverCx, HoverInfo, HoverRequest, HOVER_IDLE_DELAY_MS};
pub use intel::signature::{SignatureCx, SignatureHelp, SignatureInfo, SignatureRequest};
pub use intel::ticket::Ticket;
```

`Start` and `Counter` stay module-path only (`intel::completion::Start`,
`intel::ticket::Counter`) — bare `Start`/`Counter` at the root would be meaningless names.

### Step 7 — the widget (crates/scrive-iced/src/editor.rs)

7a. `Action` (editor.rs:222-383): add after `PopupDismiss` (editor.rs:339-340):

```rust
    /// Ask for completions at the caret (Ctrl+Space). A manual invoke: it also
    /// overrides an Escape dismissal.
    TriggerCompletion,
```

7b. `moves_caret` (editor.rs:393-432): add `| Action::TriggerCompletion` to the excluded list next
to `| Action::PopupDismiss`. (Every variant *not* listed autoscrolls.)

7c. `interpret_key`, editor.rs:3510. Current:

```rust
        Key::Named(Named::Space) => Some(Action::Type(' ')),
```

New:

```rust
        // Ctrl+Space asks for completions; before the plain arm, which types.
        // The `!alt` guard keeps Ctrl+Alt (AltGr) layouts typing.
        Key::Named(Named::Space) if mods.control() && !mods.alt() => Some(Action::TriggerCompletion),
        Key::Named(Named::Space) => Some(Action::Type(' ')),
```

7d. `State` (editor.rs:505-512): after `hover_at: Option<Instant>,` add

```rust
    /// Whether a `HoverQuery` went out that no `HoverDismiss` has cancelled yet.
    /// The host may be waiting on an async answer with no card on screen, so
    /// leaving the spot must still publish `HoverDismiss` to retire it.
    hover_queried: bool,
```

and `hover_queried: false,` in `impl Default for State` after `hover_at: None,` (editor.rs:560).

7e. `Editor` struct (editor.rs:637-639): after `hover: Option<&'a HoverInfo>,` add

```rust
    /// The word an in-flight async hover request is about, if any (app-supplied).
    /// A pointer move that stays inside it keeps the request instead of
    /// cancelling and re-arming.
    hover_pending: Option<Range<u32>>,
```

`Editor::new` (editor.rs:646-659) initializes `hover_pending: None,`. Add the builder after
`hover()` (editor.rs:685-690):

```rust
    /// Supply the word an in-flight async hover request is about, so moving the
    /// pointer within it doesn't cancel the request.
    #[must_use]
    pub fn hover_pending(mut self, word: Option<Range<u32>>) -> Self {
        self.hover_pending = word;
        self
    }
```

7f. `CursorMoved` hover branch, editor.rs:2201-2226. Current:

```rust
                    let off = self.hit_test(&geo, pos);
                    // Keep the hover open while the pointer is over its word OR over
                    // the hover box itself — so it can be moved into and scrolled.
                    let still_in = self.hover.is_some_and(|h| {
                        (off >= h.range.start && off < h.range.end)
                            || self.hover_layout(h, &geo).rect.contains(pos)
                    });
                    if !still_in {
                        if self.hover.is_some() {
                            shell.publish((self.on_action)(Action::HoverDismiss));
                        }
                        state.hover_pos = Some(pos);
                        state.hover_rearm = true;
                        state.hover_scroll = 0.0; // a fresh hover starts un-scrolled
                        shell.request_redraw();
                    }
                } else {
                    // Pointer over the gutter or off the widget — cancel any
                    // pending / open hover.
                    state.hover_pos = None;
                    state.hover_at = None;
                    state.hover_scroll = 0.0;
                    if self.hover.is_some() {
                        shell.publish((self.on_action)(Action::HoverDismiss));
                    }
                }
```

New:

```rust
                    let off = self.hit_test(&geo, pos);
                    // Keep the hover open while the pointer is over its word OR over
                    // the hover box itself — so it can be moved into and scrolled —
                    // and keep an unanswered request while the pointer stays on the
                    // word it asked about.
                    let still_in = self.hover.is_some_and(|h| {
                        (off >= h.range.start && off < h.range.end)
                            || self.hover_layout(h, &geo).rect.contains(pos)
                    }) || self.hover_pending.as_ref().is_some_and(|w| off >= w.start && off < w.end);
                    if !still_in {
                        if self.hover.is_some() || state.hover_queried {
                            state.hover_queried = false;
                            shell.publish((self.on_action)(Action::HoverDismiss));
                        }
                        state.hover_pos = Some(pos);
                        state.hover_rearm = true;
                        state.hover_scroll = 0.0; // a fresh hover starts un-scrolled
                        shell.request_redraw();
                    }
                } else {
                    // Pointer over the gutter or off the widget — cancel any
                    // pending / open hover.
                    state.hover_pos = None;
                    state.hover_at = None;
                    state.hover_scroll = 0.0;
                    if self.hover.is_some() || state.hover_queried {
                        state.hover_queried = false;
                        shell.publish((self.on_action)(Action::HoverDismiss));
                    }
                }
```

7g. `RedrawRequested` hover firing, editor.rs:2509-2521. Current:

```rust
                            if let Some(opener) = self.collapsed_chip_at(&geo, pos) {
                                if state.fold_preview != Some(opener) {
                                    state.fold_preview = Some(opener);
                                    shell.request_redraw();
                                }
                                if self.hover.is_some() {
                                    shell.publish((self.on_action)(Action::HoverDismiss));
                                }
                            } else {
                                let off = self.hit_test(&geo, pos);
                                shell.publish((self.on_action)(Action::HoverQuery(off)));
                            }
```

New:

```rust
                            if let Some(opener) = self.collapsed_chip_at(&geo, pos) {
                                if state.fold_preview != Some(opener) {
                                    state.fold_preview = Some(opener);
                                    shell.request_redraw();
                                }
                                if self.hover.is_some() || state.hover_queried {
                                    state.hover_queried = false;
                                    shell.publish((self.on_action)(Action::HoverDismiss));
                                }
                            } else {
                                let off = self.hit_test(&geo, pos);
                                state.hover_queried = true;
                                shell.publish((self.on_action)(Action::HoverQuery(off)));
                            }
```

### Step 8 — `CodeEditor` (crates/scrive-iced/src/code_editor.rs)

8a. Imports, code_editor.rs:40-47. Current:

```rust
use scrive_core::{
    default_indent_size, is_completion_word_char, CompletionController, CompletionCx, CompletionItem,
    CompletionState, CompletionTrigger, Completions, Diagnostic, DiagnosticsOutcome, Document, EditOp,
    FindQuery, Hover,
    HoverCx, HoverInfo, InsertText, Point, Revision, Selection, SelectionId, SelectionSet, Severity,
    SignatureCx, SignatureHelp, SignatureInfo, Snippet, SnippetSession, SyntaxDef, TabOutcome,
    TokenTheme, LOOKBACK_LINES,
};
```

New (only add names; keep this hand formatting):

```rust
use scrive_core::intel::completion::Start;
use scrive_core::intel::ticket;
use scrive_core::{
    default_indent_size, is_completion_word_char, Bias, CompletionController, CompletionCx, CompletionItem,
    CompletionState, CompletionTrigger, Completions, Diagnostic, DiagnosticsOutcome, Document, EditOp,
    FindQuery, Hover,
    HoverCx, HoverInfo, InsertText, Point, Revision, Selection, SelectionId, SelectionSet, Severity,
    SignatureCx, SignatureHelp, SignatureInfo, Snippet, SnippetSession, SyntaxDef, TabOutcome, Ticket,
    TokenTheme, LOOKBACK_LINES,
};
```

(`Revision` stays — `set_diagnostics` uses it.)

8b. Delete the three struct definitions, code_editor.rs:227-269 (`CompletionRequest`,
`SignatureRequest`, `HoverRequest` with their docs) and put in their place:

```rust
/// The async request types a host pulls from a [`CodeEditor`]. They live in
/// scrive-core so a language-service client can use them without iced.
pub use scrive_core::{CompletionRequest, HoverRequest, SignatureRequest};
```

8c. Private types, after `enum CompletionEvent` (code_editor.rs:215-225):

```rust
/// The ticket each async service's reply must carry to land — one slot per
/// service. A slot is set when a request is recorded and cleared when the user
/// abandons it (see `abandon`'s callers); [`CodeEditor::accepts`] reads it.
#[derive(Default)]
struct Awaiting {
    completion: Option<Ticket>,
    signature: Option<Ticket>,
    /// The hover ticket, plus the pointer offset and word it asked about: the
    /// word keeps a pointer move inside it from cancelling the request.
    hover: Option<(Ticket, u32, Range<u32>)>,
}

/// Which awaited slot an `accepts` / `abandon` call addresses.
#[derive(Clone, Copy)]
enum Awaited {
    Completion,
    Signature,
    Hover,
}
```

8d. Fields: after `pending_hover_request` (code_editor.rs:206-208) add

```rust
    /// Mints the ticket every async request carries. Per editor, so two
    /// requests at one revision still differ.
    tickets: ticket::Counter,
    /// What each async service is waiting for — the ticket a reply must carry.
    awaiting: Awaiting,
    /// The caret the popup's current items were produced against. Accepting an
    /// item shifts its additional edits by how far the caret has moved since.
    items_caret: u32,
```

and in `new` (code_editor.rs:312-315): `tickets: ticket::Counter::new(), awaiting:
Awaiting::default(), items_caret: 0,`. Update the three `pending_*` field docs to mention the
ticket ("…recorded with a fresh ticket in `awaiting`").

8e. The ingest API, code_editor.rs:526-579. Replace `set_completions`, `set_signature` and
`set_hover` (keep the three `take_*_request` fns, but fix their docs to say "returns the result
through `set_…` with the request's ticket"):

```rust
    /// Ingest completion items from an async source, stamped with the
    /// request's `ticket`. The items land only if the editor still awaits that
    /// ticket and the document has not moved; otherwise they are dropped (a
    /// newer request, or none, is in charge). Landed items open or refilter the
    /// popup against the *live* word — snippet-format items
    /// ([`InsertText::Snippet`](scrive_core::InsertText)) expand into a tab-stop
    /// session on accept exactly like a synchronous provider's. An empty list
    /// closes the popup; an Escape dismissal keeps it closed.
    pub fn set_completions(&mut self, ticket: Ticket, items: Vec<CompletionItem>) {
        if !self.accepts(Awaited::Completion, ticket) {
            return;
        }
        self.items_caret = self.doc.selections().newest().head();
        let word = self.completion_word_text();
        let anchor = self.completion_word().start;
        self.completion.set_items(items, &word, anchor);
    }

    /// Ingest a signature-help result from an async source, stamped with the
    /// request's `ticket` (see [`set_completions`](Self::set_completions) for
    /// when it lands). `None` closes the box and stops re-querying.
    pub fn set_signature(&mut self, ticket: Ticket, info: Option<SignatureInfo>) {
        if !self.accepts(Awaited::Signature, ticket) {
            return;
        }
        if info.is_none() {
            self.abandon(Awaited::Signature);
        }
        self.signature = info;
    }

    /// Ingest a hover card from an async source, stamped with the request's
    /// `ticket`. Replaces the shown card; `None` clears it.
    pub fn set_hover(&mut self, ticket: Ticket, info: Option<HoverInfo>) {
        if !self.accepts(Awaited::Hover, ticket) {
            return;
        }
        if info.is_none() {
            self.abandon(Awaited::Hover);
        }
        self.hover = info;
    }
```

8f. `update` arms. Each change, in file order:

- `ViewportChanged` (code_editor.rs:629): `self.hover = None; // scroll closes the hover` →

  ```rust
                  self.hover = None; // scroll closes the hover…
                  self.abandon(Awaited::Hover); // …and retires a pending one
  ```

- After the `Collapse if self.find_open` arm (code_editor.rs:635-638) add a dedicated arm:

  ```rust
              // Escape with no popup or box showing (the widget sends those as
              // PopupDismiss / SignatureClose) still retires an in-flight
              // signature request — before the tail below would re-query it.
              Event::Editor(Action::Collapse) => {
                  self.signature = None;
                  self.abandon(Awaited::Signature);
                  self.apply(Action::Collapse);
                  Task::none()
              }
  ```

- `PopupDismiss` (code_editor.rs:659-662) → `self.completion.escape();
  self.abandon(Awaited::Completion);`.
- New arm after `PopupClickAccept`:

  ```rust
              // Ctrl+Space: a manual invoke at the caret, in any popup state.
              Event::Editor(Action::TriggerCompletion) => {
                  self.request_completions(CompletionTrigger::Manual);
                  Task::none()
              }
  ```

- `SignatureClose` (code_editor.rs:687-690) → `self.signature = None;
  self.abandon(Awaited::Signature);`.
- `HoverQuery`, code_editor.rs:714-720. Current:

  ```rust
                  if self.hover_provider.is_none() && cx.word.start != cx.word.end {
                      self.pending_hover_request =
                          Some(HoverRequest { revision: self.doc.revision(), offset });
                  }
  ```

  New:

  ```rust
                  if self.hover_provider.is_none() && cx.word.start != cx.word.end {
                      let ticket = self.tickets.issue(self.doc.revision());
                      self.awaiting.hover = Some((ticket, offset, cx.word.clone()));
                      self.pending_hover_request = Some(HoverRequest::new(ticket, offset, cx.word.clone()));
                  } else {
                      // Nothing asked here, so a late reply to an earlier query
                      // would be for a spot the pointer has left.
                      self.abandon(Awaited::Hover);
                  }
  ```

- `HoverDismiss` (code_editor.rs:723-726) → `self.hover = None; self.abandon(Awaited::Hover);`.

8g. `view` (code_editor.rs:921-928): add
`.hover_pending(self.awaiting.hover.as_ref().map(|(_, _, word)| word.clone()))` after
`.hover(self.hover.as_ref())`.

8h. `apply` (code_editor.rs:1301-1314): add `| Action::TriggerCompletion` to the exhaustive
no-op arm (it is handled in `update`).

8i. `after_edit` (code_editor.rs:1340-1343). Current:

```rust
        self.drive_completion(comp_event);
        self.drive_signature(comp_event);
        self.reconcile_snippet();
        self.hover = None;
```

New:

```rust
        self.drive_completion(comp_event);
        self.drive_signature(comp_event);
        self.reconcile_snippet();
        self.hover = None;
        // A caret jump abandons a pending hover; typing keeps it (its reply is
        // then dropped by revision, and the pointer re-arm asks again).
        if matches!(comp_event, CompletionEvent::CaretOrClose) {
            self.abandon(Awaited::Hover);
        }
```

8j. `drive_completion` (code_editor.rs:1352-1383), full new body:

```rust
    fn drive_completion(&mut self, event: CompletionEvent) {
        match event {
            CompletionEvent::Typed(c) => {
                let trigger = if is_completion_word_char(c) {
                    CompletionTrigger::Typed(c)
                } else if matches!(c, '(' | ',' | '=' | ':' | '.' | ' ') {
                    CompletionTrigger::TriggerChar(c)
                } else {
                    self.completion.on_boundary();
                    self.abandon(Awaited::Completion);
                    return;
                };
                self.request_completions(trigger);
            }
            // A deletion re-asks while a list is showing or on its way, so the
            // answer tracks the shorter word; emptying the word ends the session.
            CompletionEvent::Deleting => {
                if self.completion.is_open() || self.awaiting.completion.is_some() {
                    let word = self.completion_word_text();
                    match word.chars().last() {
                        Some(c) => self.request_completions(CompletionTrigger::Typed(c)),
                        None => {
                            self.completion.close();
                            self.abandon(Awaited::Completion);
                        }
                    }
                }
            }
            CompletionEvent::CaretOrClose => {
                self.completion.close();
                self.abandon(Awaited::Completion);
            }
        }
    }
```

8k. `request_completions` (code_editor.rs:1385-1407), full new body and doc:

```rust
    /// Query completions for `trigger`: a synchronous provider fills the popup
    /// inline; with none set, record an async [`CompletionRequest`] under a
    /// fresh ticket for the host to pull via
    /// [`take_completion_request`](Self::take_completion_request) and answer
    /// through [`set_completions`](Self::set_completions). The async path keeps
    /// sync-mode behavior: an Escape-dismissed word asks for nothing, an open
    /// popup narrows immediately, and a trigger char or manual invoke overrides
    /// the dismissal.
    fn request_completions(&mut self, trigger: CompletionTrigger) {
        let head = self.doc.selections().newest().head();
        let Some(mut provider) = self.comp_provider.take() else {
            // Sampled before the refilter below can close the popup: a request
            // continues the session only if one is visibly open or in flight.
            let start = if self.completion.is_open() || self.awaiting.completion.is_some() {
                Start::Continuing
            } else {
                Start::Fresh
            };
            match trigger {
                CompletionTrigger::Typed(_) => {
                    if matches!(self.completion.state(), CompletionState::DismissedUntilBoundary) {
                        return; // dismissed until a boundary: extending the word asks nothing
                    }
                    // Narrow the open popup now, from the items it has, instead of
                    // lagging a round trip behind the typing.
                    let word = self.completion_word_text();
                    self.completion.refilter(&word);
                }
                // A trigger char starts a new word; the open list described the
                // one it just ended. Closing also clears a dismissal.
                CompletionTrigger::TriggerChar(_) => self.completion.close(),
                // Ctrl+Space overrides a dismissal; an open popup stays until
                // the fresh list replaces it.
                CompletionTrigger::Manual => self.completion.on_boundary(),
            }
            let ticket = self.tickets.issue(self.doc.revision());
            self.awaiting.completion = Some(ticket);
            self.pending_completion_request =
                Some(CompletionRequest::new(ticket, self.completion_word(), trigger, start));
            return;
        };
        // A provider call replaces the list — unless this is a word char
        // refiltering an open popup, which keeps the items (and their caret).
        let fresh = !(self.completion.is_open() && matches!(trigger, CompletionTrigger::Typed(_)));
        let cx = self.build_cx(trigger);
        let word = self.completion_word_text();
        self.completion.on_input(&cx, &word, &mut *provider);
        self.comp_provider = Some(provider);
        if fresh {
            self.items_caret = head;
        }
    }
```

8l. `drive_signature` (code_editor.rs:1409-1428), full new body:

```rust
    /// Drive the signature-help box: `(` opens it; while it shows — or while a
    /// request for it is still in flight — every edit or move re-queries, and a
    /// `None` reply closes it. Re-querying while awaited is what lets `foo(a`
    /// typed faster than the reply still open the box: the reply to the `(`
    /// request is stale by then, and only the newest request can land.
    fn drive_signature(&mut self, event: CompletionEvent) {
        let query = matches!(event, CompletionEvent::Typed('('))
            || self.signature.is_some()
            || self.awaiting.signature.is_some();
        if !query {
            return;
        }
        if let Some(mut provider) = self.sig_provider.take() {
            let cx = self.build_sig_cx();
            self.signature = provider.signature(&cx);
            self.sig_provider = Some(provider);
        } else {
            // No synchronous provider: record an async request for the host.
            let head = self.doc.selections().newest().head();
            let ticket = self.tickets.issue(self.doc.revision());
            self.awaiting.signature = Some(ticket);
            self.pending_signature_request =
                Some(SignatureRequest::new(ticket, self.doc.buffer().offset_to_point(head)));
        }
    }
```

8m. `accept_completion` (code_editor.rs:1430-1485), full new body and doc:

```rust
    /// Accept the popup's selected item as ONE edit: the main replacement and
    /// the item's additional edits (an auto-import, say) in a single batch, so
    /// one undo reverts all of it. A snippet expands and starts a tab-stop
    /// session at the first stop. Fires the retrigger and the signature-help
    /// follow-up if the item asks for them.
    ///
    /// Positions: the item's ranges were produced at `items_caret`. Since then
    /// the user may have typed on inside the word, so a replace range ending in
    /// the live word stretches to the caret, and additional edits at or past the
    /// popup anchor shift by the caret's movement. Where the main replacement
    /// starts after the batch is read from the patch — an import above it moves
    /// it — so the snippet base and the caret land on the inserted text.
    fn accept_completion(&mut self) {
        let CompletionState::Open(list) = self.completion.state() else { return };
        let anchor = list.anchor;
        let Some(item) = self.completion.accept() else { return };
        // The accept retires the in-flight request; a retrigger below records
        // a fresh one, so this must come first.
        self.abandon(Awaited::Completion);

        let caret = self.doc.selections().newest().head();
        let word = self.completion_word();
        let replace = match item.replace.clone() {
            None => word.clone(),
            Some(r) if (word.start..=word.end).contains(&r.end) => r.start..word.end,
            Some(r) => r,
        };
        let delta = i64::from(caret) - i64::from(self.items_caret);
        let shift = |o: u32| (i64::from(o) + delta).clamp(0, i64::from(u32::MAX)) as u32;
        let mut batch: Vec<EditOp> = item
            .additional
            .iter()
            .map(|op| {
                if op.range.start >= anchor {
                    EditOp::new(shift(op.range.start)..shift(op.range.end), op.text.clone())
                } else {
                    op.clone()
                }
            })
            // An edit touching the replaced word would fight the insertion; the
            // insertion wins.
            .filter(|op| op.range.end < replace.start || op.range.start > replace.end)
            .collect();

        // Read the indent before any edit can move the line.
        let indent = self.line_indent(replace.start);
        let (text, expanded) = match &item.insert {
            InsertText::Plain(s) => (s.clone(), None),
            InsertText::Snippet(body) => match Snippet::parse(body) {
                Ok(snip) => {
                    let e = snip.for_insertion(&indent, default_indent_size() as usize);
                    (e.text.clone(), Some(e))
                }
                Err(_) => (body.clone(), None),
            },
        };
        let main = EditOp::new(replace.clone(), text.clone());
        batch.push(main.clone());
        batch.sort_by_key(|op| (op.range.start, op.range.end));
        let before = self.doc.revision();
        // A malformed set of additional edits must not cost the user the
        // completion itself: fall back to the insertion alone.
        let committed = match self.doc.edit(batch) {
            Ok(c) => c,
            Err(_) => match self.doc.edit(vec![main]) {
                Ok(c) => c,
                Err(_) => return,
            },
        };
        let base = committed.patch().map_offset(replace.start, Bias::Left);

        if let Some(mut s) = self.snippet.take() {
            s.cancel(self.doc.decorations_mut());
        }
        match expanded {
            Some(e) => match SnippetSession::start(&e, base, self.doc.decorations_mut()) {
                Some((session, first)) => {
                    self.set_selection_range(first);
                    self.snippet = Some(session);
                }
                None => {
                    let fin = e.stops.last().map_or(e.text.len() as u32, |s| s.range.start);
                    self.set_caret(base + fin);
                }
            },
            None => self.set_caret(base + text.len() as u32),
        }
        self.doc.tokenize_highlight(self.viewport.end);
        let now = self.now_ms;
        self.doc.maybe_rescan_find(now);
        if self.doc.revision() != before {
            self.dirty = true;
        }

        if item.retrigger && self.snippet.is_none() {
            self.request_completions(CompletionTrigger::Manual);
        }
        if item.signature_after {
            self.drive_signature(CompletionEvent::Typed('('));
        }
    }
```

Borrow note: `let CompletionState::Open(list) = self.completion.state() else { return };` borrows
`self.completion` immutably; copy `anchor` out (u32) before `self.completion.accept()` needs
`&mut` — NLL ends the borrow at the last use of `list`.

8n. The gate and the abandon helper — private methods, place them after `accept_completion`:

```rust
    /// Whether a reply stamped `ticket` may land: it must be the request this
    /// editor still awaits for `kind`, and the document must not have moved
    /// since it was made. The one gate every async `set_*` goes through.
    fn accepts(&self, kind: Awaited, ticket: Ticket) -> bool {
        let awaited = match kind {
            Awaited::Completion => self.awaiting.completion,
            Awaited::Signature => self.awaiting.signature,
            Awaited::Hover => self.awaiting.hover.as_ref().map(|(t, ..)| *t),
        };
        awaited == Some(ticket) && ticket.revision() == self.doc.revision()
    }

    /// Stop waiting for `kind`: drop the request the host hasn't pulled yet and
    /// the ticket a late reply would carry.
    fn abandon(&mut self, kind: Awaited) {
        match kind {
            Awaited::Completion => {
                self.awaiting.completion = None;
                self.pending_completion_request = None;
            }
            Awaited::Signature => {
                self.awaiting.signature = None;
                self.pending_signature_request = None;
            }
            Awaited::Hover => {
                self.awaiting.hover = None;
                self.pending_hover_request = None;
            }
        }
    }
```

`set_selection_range` (code_editor.rs:1520-1525) remains used by `snippet_tab` and accept; keep
it private (Phase 3 adds the public `select`).

## 5. Files that change

| File | Change |
|---|---|
| crates/scrive-core/src/intel/ticket.rs | **new**: `Ticket`, `Counter`, test |
| crates/scrive-core/src/intel.rs | `pub mod ticket;`, module doc |
| crates/scrive-core/src/intel/completion.rs | `Start`, `CompletionRequest`; `refilter` uses `matches`; public `refilter`; `set_items` respects dismissal; `prefix_matches` moved out; tests |
| crates/scrive-core/src/intel/providers.rs | `filter`/`additional`/`signature_after` + builders; `matches`; `prefix_matches` moved in; stale doc; tests |
| crates/scrive-core/src/intel/signature.rs | `SignatureRequest`; stale doc |
| crates/scrive-core/src/intel/hover.rs | `HoverRequest`; stale doc |
| crates/scrive-core/src/lib.rs | root re-exports of the three requests and `Ticket` |
| crates/scrive-iced/src/editor.rs | `Action::TriggerCompletion`, `moves_caret`, Ctrl+Space, `State::hover_queried`, `Editor::hover_pending`, hover cancel paths; tests |
| crates/scrive-iced/src/code_editor.rs | request structs → re-export; `Awaiting`/`Awaited`; `tickets`, `items_caret`; `accepts`/`abandon`; `set_*` on tickets; arms; `drive_completion`, `request_completions`, `drive_signature`, `accept_completion`; tests |

## 6. Tests to add

Conventions: sentence names, a `///` doc stating the invariant, string assert messages.

### intel/ticket.rs

- `tickets_at_one_revision_are_distinct` — shown in Step 1.

### intel/providers.rs

- **`matches_uses_the_filter_text_when_set`** — `CompletionItem::plain("•printf(…)", Symbol)
  .with_filter("printf")` matches `"pri"` and `"PRI"`, not `"•"`; a plain `"foo"` matches
  `"FO"` and `""`, not `"x"`.
- Extend **`completion_item_builder_defaults_and_overrides`** (providers.rs:211-232): defaults
  `filter.is_none()`, `additional.is_empty()`, `!signature_after`; the built item chains
  `.with_filter("sz").with_additional(vec![EditOp::insert(0, "use x;\n")])
  .with_signature_after(true)` and asserts each.

### intel/completion.rs (helpers `kw`, `Stub`, `cx`, `labels` at :230-264)

- **`set_items_respects_an_escape_dismissal`** — open via `on_input(Typed)`, `escape()`, then
  `set_items(vec![kw("send", "1")], "s", 0)`: state stays `DismissedUntilBoundary`. After
  `on_boundary()`, the same `set_items` opens.
- **`refilter_narrows_an_open_popup_and_closes_when_nothing_matches`** — open with `send`, `set`
  on `"s"`; `refilter("se")` keeps both; `refilter("set")` → `["set"]`; `refilter("x")` closes;
  `refilter` on a closed controller stays closed.

### code_editor.rs (`mod tests`)

Add helpers once:

```rust
    /// Feed one widget action through the real `update` path.
    fn act(ed: &mut CodeEditor, action: Action) {
        let _ = ed.update(Event::Editor(action), Instant::now());
    }

    /// A keyword item with `label` as its insertion.
    fn item(label: &str) -> CompletionItem {
        CompletionItem::plain(label, scrive_core::CompletionKind::Keyword)
    }

    /// The labels the open popup shows, in order (empty when closed).
    fn shown(ed: &CodeEditor) -> Vec<String> {
        match ed.completion.state() {
            CompletionState::Open(list) => {
                list.filtered.iter().map(|&i| list.items[i as usize].label.clone()).collect()
            }
            _ => Vec::new(),
        }
    }

    /// A one-parameter signature for the async signature tests.
    fn sig() -> SignatureInfo {
        SignatureInfo { label: "foo(a)".into(), params: vec![4..5], active: 0, doc: None }
    }
```

Update the existing async tests to tickets:
`async_completion_request_and_ingest_round_trip` → `ed.set_completions(req.ticket(), …)`;
`stale_set_completions_is_dropped` → `req.ticket()` (its doc now says the reply is dropped
because a newer request superseded it and the revision moved);
`async_snippet_item_starts_a_session_on_accept` → `req.ticket()`.

New tests (sketches):

- **`a_click_after_a_trigger_char_drops_the_stale_completion`**
  ```rust
  let mut ed = CodeEditor::new("a\n");
  act(&mut ed, Action::PlaceCaret(1));
  act(&mut ed, Action::Type('.'));
  let req = ed.take_completion_request().expect("'.' is a trigger char");
  act(&mut ed, Action::PlaceCaret(0)); // no revision change — only the abandonment stops it
  ed.set_completions(req.ticket(), vec![item("len")]);
  assert!(matches!(ed.completion.state(), CompletionState::Closed), "a click abandons the '.' request");
  ```
- **`a_reply_whose_revision_moved_is_dropped_even_with_the_awaited_ticket`** — `"hello world\n"`,
  `HoverQuery(2)` → `req = take_hover_request()`; `act(Type('x'))` (typing keeps the hover slot
  but moves the revision); `set_hover(req.ticket, Some(HoverInfo { markdown: "doc".into(), range:
  0..5 }))` → `ed.hover.is_none()`. Control: a fresh `HoverQuery` then an immediate `set_hover`
  with its ticket shows the card.
- **`two_manual_invokes_at_one_revision_accept_only_the_second`**
  ```rust
  let mut ed = CodeEditor::new("");
  act(&mut ed, Action::TriggerCompletion);
  let first = ed.take_completion_request().expect("Ctrl+Space asks");
  act(&mut ed, Action::TriggerCompletion);
  let second = ed.take_completion_request().expect("again");
  assert_eq!(first.ticket().revision(), second.ticket().revision(), "same revision");
  assert_eq!(second.trigger(), CompletionTrigger::Manual);
  ed.set_completions(first.ticket(), vec![item("stale")]);
  assert!(shown(&ed).is_empty(), "the superseded reply is dropped");
  ed.set_completions(second.ticket(), vec![item("fresh")]);
  assert_eq!(shown(&ed), ["fresh"], "the newest reply lands");
  ```
- **`hover_dismiss_abandons_the_pending_hover`** — `HoverQuery(2)`, take, `HoverDismiss`, then
  `set_hover(req.ticket, Some(card))` leaves `ed.hover` `None`.
- **`deleting_back_to_an_empty_word_starts_the_next_request_fresh`** — `Type('f')`: take, its
  `start() == Start::Fresh`; `Backspace`: `take_completion_request()` is `None` (the empty word
  clears); `Type('g')`: take, `start() == Start::Fresh`.
- **`a_deletion_while_awaited_re_requests_completion`** — `Type('f')`, `Type('o')`, take (slot
  awaited, popup closed); `Backspace`: take is `Some`, `trigger() == Typed('f')`,
  `start() == Continuing`, `word() == 0..1`.
- **`a_signature_reply_after_a_click_away_is_dropped`** — `"x\n"`; `Type('(')` → `paren`;
  `PlaceCaret(0)` re-queries (awaited) → `take_signature_request()` is `Some`;
  `set_signature(paren.ticket(), Some(sig()))` → `ed.signature.is_none()`.
- **`typing_through_a_call_before_the_reply_still_opens_the_signature_box`** — type `f o o (`,
  `paren = take`; `Type('a')`, `latest = take` (re-queried because awaited);
  `set_signature(paren.ticket(), Some(sig()))` → none shown ("the `(` reply is stale");
  `set_signature(latest.ticket(), Some(sig()))` → shown.
- **`escape_retires_an_awaited_signature_without_re_querying`** — `Type('(')`, take; `Collapse`:
  `take_signature_request()` is `None`; the old ticket no longer lands.
- **`typing_while_the_popup_is_open_refilters_before_the_reply_lands`** — `Type('s')`, land
  `[send, set]`; `Type('e')`, `Type('t')` with no replies: `shown == ["set"]`; the recorded
  request has `start() == Continuing`.
- **`a_dismissed_popup_asks_for_nothing_until_a_trigger`** — `Type('s')`, land `[send]`,
  `PopupDismiss`; `Type('e')`: no request; `Type('.')`: a request; landing it opens the popup.
- **`accepting_a_retrigger_item_opens_the_popup_when_the_reply_lands`**
  ```rust
  let mut ed = CodeEditor::new("");
  act(&mut ed, Action::Type('s'));
  let req = ed.take_completion_request().unwrap();
  ed.set_completions(req.ticket(), vec![item("size=").with_retrigger(true)]);
  act(&mut ed, Action::PopupAccept);
  assert_eq!(ed.document().text(), "size=");
  let again = ed.take_completion_request().expect("the retrigger asks the async host");
  assert_eq!(again.trigger(), CompletionTrigger::Manual);
  ed.set_completions(again.ticket(), vec![item("8")]);
  assert_eq!(shown(&ed), ["8"], "the retrigger reply opens the popup");
  ```
- **`an_auto_import_above_a_snippet_keeps_the_tab_stops_on_the_placeholders`**
  ```rust
  let mut ed = CodeEditor::new("\n");
  act(&mut ed, Action::PlaceCaret(1));
  act(&mut ed, Action::Type('i'));
  let req = ed.take_completion_request().unwrap();
  let snippet = CompletionItem::new("iflet", scrive_core::CompletionKind::Keyword,
      InsertText::Snippet("if ${1:cond} {\n\t$0\n}".into()))
      .with_additional(vec![EditOp::insert(0, "use a;\n")]);
  ed.set_completions(req.ticket(), vec![snippet]);
  act(&mut ed, Action::PopupAccept);
  let text = ed.document().text().into_owned();
  assert!(text.starts_with("use a;\n"), "the import landed");
  let cond = text.find("cond").unwrap() as u32;
  assert_eq!(ed.selection(), cond..cond + 4, "the first stop sits on its placeholder");
  let _ = ed.update(Event::Editor(Action::Undo), Instant::now());
  assert_eq!(ed.document().text(), "\ni", "one undo reverts import and insertion together");
  ```
- **`accepting_a_signature_after_item_asks_for_signature_help`** — land
  `item("foo(").with_signature_after(true)`, accept: `take_signature_request()` is `Some`.

### editor.rs (`mod tests`)

- **`ctrl_space_is_trigger_completion`**
  ```rust
  let space = Key::Named(Named::Space);
  assert_eq!(interpret_key(&space, Some(" "), Modifiers::CTRL), Some(Action::TriggerCompletion));
  assert_eq!(interpret_key(&space, Some(" "), Modifiers::empty()), Some(Action::Type(' ')), "plain Space types");
  assert_eq!(interpret_key(&space, Some(" "), Modifiers::CTRL | Modifiers::ALT), Some(Action::Type(' ')), "AltGr stays typing");
  ```
  (Check how the neighbouring `interpret_typing_and_chords` test at editor.rs:4018 calls
  `interpret_key` and match it.)
- Widget harness for the two hover tests, modeled on
  `draw_budget_ctrl_a_over_folded_doc_stays_windowed` (editor.rs:4349-4411):

  ```rust
  /// A headless tiny-skia renderer with the icon font loaded — the same setup
  /// the draw-budget test uses.
  fn headless_renderer() -> iced::Renderer {
      use iced::advanced::renderer;
      use iced::{Font, Pixels};
      iced_tiny_skia::graphics::text::font_system()
          .write()
          .expect("font system lock")
          .load_font(std::borrow::Cow::Borrowed(crate::CODICON_FONT));
      iced_renderer::fallback::Renderer::Secondary(iced_tiny_skia::Renderer::new(renderer::Settings {
          font: Font::default(),
          text_size: Pixels(14.0),
          ..renderer::Settings::default()
      }))
  }

  /// Run `events` through a one-editor UI over `doc`, with the pointer at
  /// `at`, keeping the widget state in `cache` across calls; return the
  /// published actions and the cache.
  fn pump(
      doc: &Document,
      pending: Option<Range<u32>>,
      cache: iced_runtime::user_interface::Cache,
      renderer: &mut iced::Renderer,
      at: Point,
      events: &[iced::Event],
  ) -> (Vec<Action>, iced_runtime::user_interface::Cache) {
      use iced::advanced::{mouse, shell};
      use iced_runtime::user_interface::UserInterface;
      let element: iced::Element<'_, Action, iced::Theme, iced::Renderer> =
          Editor::new(doc, |a| a).hover_pending(pending).into();
      let mut ui = UserInterface::build(element, Size::new(500.0, 320.0), cache, renderer);
      let mut bus = shell::Bus::new();
      let _ = ui.update(&iced::window::Headless, &shell::Waker::noop(), events,
          mouse::Cursor::Available(at), renderer, &mut bus);
      (bus.into_iter().collect(), ui.into_cache())
  }
  ```

  Document: `Document::new(&format!("{}\n", "word".repeat(60)))` — row 0 is one 240-char word,
  so any x in the code area of row 0 is over it. `over = Point::new(200.0, 5.0)`,
  `gutter = Point::new(1.0, 5.0)`, `t0 = Instant::now()`. The query sequence is
  `[CursorMoved { position: over }, RedrawRequested(t0),
  RedrawRequested(t0 + Duration::from_millis(HOVER_IDLE_DELAY_MS))]`.
  - **`moving_off_an_unanswered_hover_publishes_hover_dismiss`** — pump the query sequence with
    `Cache::new()`; assert some action is `HoverQuery(_)`. Pump `[CursorMoved { position: gutter
    }]` with the returned cache and `pending: None`: the actions contain `HoverDismiss`
    ("leaving an unanswered query retires it").
  - **`a_rearm_inside_the_pending_word_keeps_the_hover_request`** — pump the query sequence; then
    pump `[CursorMoved { position: Point::new(300.0, 5.0) }]` with `pending: Some(0..240)`: no
    `HoverDismiss`. Control (fresh cache, same query sequence, then the same move with
    `pending: None`): `HoverDismiss` is published.
  Filter with `iter().any(…)` — the first layout also publishes `ViewportChanged`.

## 7. Verification

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
cargo build --workspace --all-targets --target wasm32-unknown-unknown
out=$(cargo tree -p scrive-core -e normal --prefix none) || exit 1; if grep -qE '^(iced|winit|wgpu)' <<<"$out"; then echo LEAK; exit 1; fi
cargo test -p scrive-core -- ticket completion providers
cargo test -p scrive-iced -- code_editor editor::tests::ctrl_space editor::tests::moving_off editor::tests::a_rearm
grep -rn "revision: Revision" crates/scrive-iced/src/code_editor.rs   # only set_diagnostics' parameter remains
```

## 8. Spot checks

Ticket acceptance (`accepts(kind, t)`):

| Awaited slot | Reply ticket | Doc revision | Lands? | Why |
|---|---|---|---|---|
| `Some(t)` | `t` | `t.revision()` | yes | the awaited request, text unchanged |
| `Some(t2)` (same revision, later seq) | `t` | `t.revision()` | no | superseded (second Ctrl+Space) |
| `None` | `t` | `t.revision()` | no | abandoned (click, dismiss, Escape) |
| `Some(t)` | `t` | `t.revision() + 1` | no | revision-only stale (typed since) |

Completion `Start` and requests (async, no provider):

| Sequence | Request recorded | `start()` |
|---|---|---|
| `f` | `Typed('f')`, word `0..1` | Fresh |
| `f` `o` | `Typed('o')`, word `0..2` | Continuing (awaited) |
| `f` ⌫ | none (word empty → cleared) | — |
| `f` ⌫ `g` | `Typed('g')`, word `0..1` | Fresh |
| `f` `o` ⌫ | `Typed('f')`, word `0..1` | Continuing |
| `s` land → Esc → `e` | none (dismissed) | — |
| `s` land → Esc → `.` | `TriggerChar('.')`; the dismissal is cleared | Fresh (a dismissed popup is not open, and the dismiss cleared the slot) |
| Ctrl+Space twice | two `Manual`, same revision, different tickets | Fresh, then Continuing |

Accept positions (`items_caret` = caret when the list landed):

| Setup | Replace used | Additional edits | Result |
|---|---|---|---|
| word `1..2` (`i`), item `replace None`, import `ins 0 "use a;\n"` | `1..2` | kept (before anchor, no shift) | base = 8; stops shifted by 7 |
| landed at caret 3, typed 2 more (caret 5), item `replace Some(1..3)` | `1..5` (end 3 lies in live word `1..5`) | an edit at `10..10` (≥ anchor 1) → `12..12` | the typed tail is replaced too; the far edit lands where its text moved |
| item `replace Some(1..3)`, additional `ins 3 "x"` | `1..3` | dropped (touches `[1, 3]`) | |
| batch rejected (two overlapping additional edits) | — | — | retried with the main op alone |

Abandonment: see the D11 table in §3; the tests map to rows as follows — row 1:
`a_click_after_a_trigger_char…`, `a_signature_reply_after_a_click_away…`; row 2:
`deleting_back_to_an_empty_word…`; row 3: `accepting_a_retrigger_item…` (the retrigger's ticket
survives because `abandon` runs first); row 4: `escape_retires_an_awaited_signature…`; row 5:
`hover_dismiss_abandons…`, the widget tests; row 6: covered by the ViewportChanged arm (no
separate test required).

## 9. What NOT to change

- No scrive-lsp code; no LSP types, serde or lsp-types in scrive-core.
- Do not change the synchronous provider paths' behavior (the sync branch of
  `request_completions` only gains the `items_caret` bookkeeping; `drive_signature`'s sync branch
  is unchanged).
- Do not add definition/rename/format, `Brackets::innermost_open`, `SignatureRequest::call`,
  `escape_markdown` or `hover_card` — all Phase 3.
- Do not touch the find bar, `find_chord`, `Event` (the opaque message enum), or the widget
  `diff` (Phase 3).
- Do not change `PopupList`'s fields (the editor.rs test at :4434 builds one by literal).
- Do not rename existing public types; `CompletionRequest` etc. keep their names and move.
- Never run `cargo fmt`.

## 10. Known pitfalls

- **`Action` is a public exhaustive enum.** Adding `TriggerCompletion` is a semver break;
  that is expected (the release is 0.4.0, Phase 10). Inside the crate you must add it to
  `moves_caret`'s excluded list (otherwise Ctrl+Space autoscrolls) and to `apply`'s exhaustive
  no-op arm (otherwise it does not compile).
- **Doc links must resolve inside scrive-core.** The moved request docs used to link
  `[`CodeEditor::take_completion_request`]`; scrive-core cannot see scrive-iced. Write the editor
  method names as plain code spans. `cargo doc -D warnings` fails on a broken intra-doc link.
- **`#[non_exhaustive]` across crates.** `HoverRequest` is `#[non_exhaustive]`, so scrive-iced
  must build it with `HoverRequest::new`, never a literal. `CompletionItem` is already
  `#[non_exhaustive]`: tests build items with `CompletionItem::new/plain` + builders. New fields
  on `CompletionItem` must be initialized in `CompletionItem::new` or scrive-core stops compiling.
- **Ordering in `accept_completion`.** `abandon(Awaited::Completion)` must run before the
  retrigger's `request_completions`, or it wipes the ticket the retrigger just recorded.
- **The hover slot holds a `Range`, so it is not `Copy`.** Read the ticket with
  `self.awaiting.hover.as_ref().map(|(t, ..)| *t)`; clone the word for `hover_pending`.
- **`match self.state { CompletionState::Open(_) => self.refilter(word), … }`** compiles because
  the patterns bind nothing; `match &mut self.state` with a call to `self.refilter` in the arm
  does not (E0499).
- **The `Collapse` arm order.** The new unguarded `Event::Editor(Action::Collapse)` arm must come
  *after* `Event::Editor(Action::Collapse) if self.find_open`, and both before the catch-all
  `Event::Editor(action)`.
- **`TriggerCompletion` is handled in `update`, not `apply`.** Routing it through `apply` would
  run `after_edit(CaretOrClose)` and immediately abandon the request it just made.
- **clippy:** `Counter` has `new` and derives `Default` (`new_without_default` satisfied);
  `i64::from(u32)` rather than `as` for the delta; `Awaited` is a three-variant private enum, so
  there is no single-variant-enum lint to dodge in this phase.
- **Widget tests need the dev-deps that exist** (`iced_renderer`, `iced_tiny_skia`,
  `iced_runtime`); do not add `iced_test`.
