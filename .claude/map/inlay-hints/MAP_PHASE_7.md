# Phase 7 — glue, examples, docs

The last phase of the inlay-hint plan (`MAP_PLAN.md`, Draft 7). Phases 1–6 built the hint model
and store, the hint-aware layout, painting, gestures, the scheduler and the scrive-lsp client.
This phase connects them in the `CodeEditor` glue (`code_editor/lsp.rs`). It then moves every host
to the new `sync_lsp` return, adds hints to both LSP examples, and documents the feature.

Line numbers below are from HEAD `8e72665`, re-checked against the source while writing this doc.
Phases 1–6 move code, so **locate every site by function or arm name**. The line numbers are only
there to help you find them.

## Prerequisites

- Phases 1–6 are merged. `cargo test --workspace --all-features` and both clippy runs are green.
- Read `.claude/map/lsp-bridge/DISPATCH.md`, with the substitutions from `MAP_PLAN.md` Appendix A:
  the map directory is `.claude/map/inlay-hints/`, patches go to `.claude/map/inlay-hints/patches/`,
  and the `--all-features` clippy run applies. Override 1 governs every comment you write.
- Read `~/.claude/guides/RUST_STYLE.md`, `~/.claude/guides/OPAQUE.md`, the `/iced` skill and the
  `/commit-and-comment` skill.
- Read in full, as they are after Phase 6:
  - `crates/scrive-iced/src/code_editor/lsp.rs`;
  - `crates/scrive-iced/src/code_editor.rs`: `Awaiting`, `Awaited`, `accepts`, `abandon`,
    `after_edit`, `try_edit`, `select`, `update`'s `Inlay*` and `Wake` arms, the scheduler, the
    toggle;
  - `crates/scrive-iced/examples/lsp/{main.rs, server.rs}` and
    `crates/scrive-iced/examples/rust_analyzer.rs`;
  - `crates/scrive-lsp/src/{update.rs, lib.rs, client.rs}` (`inlays`, `interact`, the `Change`
    variants), and scrive-lsp's new `inlay.rs`;
  - `crates/scrive-core/src/intel/inlay.rs` (`Key`, `Shown`, `Placed`);
  - `README.md`, `crates/scrive-iced/README.md` (byte-identical) and `crates/scrive-lsp/README.md`.

### Names assumed from Phases 1–6

This doc uses the plan's names for earlier phases' items. **Use the real names** where they
differ, and never add a second path to the same fact.

| Role | Name used here | Owner |
|---|---|---|
| Hint key | `scrive_core::intel::inlay::Key`, `Copy + Eq` | Phase 1 |
| Shown hints | `Document::inlays_in(range)` yielding `inlay::Shown` with `key()` and `offset()` (the render offset) | Phase 1 |
| Store ops | `Document::{set_inlays, clear_inlays, remove_inlay(key, offset), inlays_revision}` | Phase 1 |
| Seam types | `inlay::Request` (`request.rs`), `inlay::Interaction` (`interaction.rs`) | Phase 1 |
| Widget actions | `Action::InlayHover { key, part }` (R7), `Action::InlayJump { key, part }`, `Action::InlayInsert { key, offset }`, `Action::Wake(u64)` | Phase 5 |
| Toggle | `CodeEditor::inlay_hints(bool)` (builder), `CodeEditor::set_inlay_hints(&mut self, bool)` | Phase 5 |
| Pulls | `CodeEditor::take_inlay_request() -> Option<inlay::Request>`, `CodeEditor::take_inlay_interaction() -> Option<inlay::Interaction>` | Phase 5 |
| Slots | `Awaiting { …, inlays: Option<Ticket>, inlay_tooltip: Option<(Ticket, Key, u32)>, inlay_insert: Option<(Ticket, Key, u32)> }` (R6), `Awaited::{Inlays, InlayTooltip, InlayInsert}`; `abandon` of each also drops the pending interaction made under that slot's ticket | Phase 5 |
| Scheduler | `Inlays { enabled, generation, wait: Option<Wake>, window, seen }` in the field `inlays`; the private `wait_inlays(delay, cap)` (R9); the public `pending_wake() -> Option<Wake>` (R10); the hint card `inlay_card: Option<InlayCard>` | Phase 5 |
| Ingest | `CodeEditor::set_inlays(ticket, Option<Vec<inlay::Placed>>)`, `CodeEditor::set_inlay_tooltip(ticket, Option<String>)` | Phase 5 |
| Client | `Client::inlays(&Snapshot, &inlay::Request) -> Output`, `Client::interact(&Snapshot, &inlay::Interaction) -> Output` | Phase 6 |
| Changes | `Change::{Inlays, InlayTooltip, InlayRefresh}` and their three `land` arms | Phase 6 |
| Existing glue | `open_lsp`, `sync_lsp`, `save_lsp`, `apply_lsp`, `jump`, `close_lsp`, the private `route` and `land`, the free `refused` | lsp-bridge Phase 9 |
| Host result | `update::Applied { messages, jump, refused }`, `#[must_use]`, `Default` (update.rs:95-106) | lsp-bridge Phase 9 |

## Goal and exit criteria

`sync_lsp` sends the inlay fetches and the hint gestures the editor recorded. It returns
`update::Applied`, so a label jump that the client answers without a round trip reaches the host.
A double-clicked hint inserts its text once, and the edit is synced in the same call. `close_lsp`
forgets the hints. Both LSP examples show hints and toggle them with a key, and the READMEs and
crate docs describe the feature.

**Exit.**
- Under `cargo test -p scrive-iced --features lsp`, these new glue tests in
  `code_editor/lsp.rs` pass, together with every existing test adapted to the new returns:
  - `sync_lsp_returns_an_inlay_label_jump_into_an_unopened_file`
  - `an_inlay_insert_lands_once_and_syncs_in_the_same_call`
  - `close_lsp_clears_the_hints_and_their_slots`
- `cargo test -p scrive-iced --features lsp --example lsp` passes the five existing tests plus:
  - `the_hint_script_lands_in_main_rs`
  - `ctrl_click_on_a_parameter_hint_opens_util_rs_at_the_parameter`
  - `ctrl_click_on_a_type_hint_jumps_into_an_unopened_file_through_sync_lsp`
  - `double_click_inserts_the_type_once_and_leaves_no_duplicate_hint`
  - `hovering_a_hint_resolves_its_tooltip`
  - `the_toggle_key_clears_and_restores_the_hints`
  - `toggle_chord_matches_only_plain_ctrl_i`
- `cargo test -p scrive-iced --features lsp --example rust_analyzer` passes, including
  `toggle_chord_matches_only_plain_ctrl_i`. The new `#[ignore]` test
  `rust_analyzer_hints_arrive_move_refresh_and_insert_once` compiles. The orchestrator runs it by
  hand.
- `cmp README.md crates/scrive-iced/README.md` succeeds.
- Every command in "Verification" is green.

## Design decisions implemented

The plan's decisions this phase carries out, restated so this doc stands alone.

**D19 — `sync_lsp` pulls the new requests and surfaces jumps.**
- `sync_lsp` returns `update::Applied` (messages and jump; `refused` stays `None`) instead of
  `Vec<Message>`. This is a breaking change, and it ships in the unreleased 0.4.0. A separate
  return type would be `Applied` minus one field.
- `update::Change` stays **exhaustive**. `land` lives in another crate, and the compile error on a
  missing arm is what keeps every new `Change` handled. Add no wildcard arm.
- `route` keeps the `Applied.jump` of a local answer. Today it asserts the jump is `None`
  (lsp.rs:195-198) and drops the `Applied`. Today's local answers never jump. An inlay label jump
  does, and in the rust-analyzer probe 25 of 61 linked parts pointed into unopened files. Hosts
  handle this jump like any other `Applied.jump`.
- **Declined or empty insert (R16).** An `Edits(vec![])` whose ticket equals
  `awaiting.inlay_insert` settles the slot and returns without `try_edit`: no hint removal, and no
  `after_edit` (which would close the completion popup and the hover card).
- **Insert removes its hint first.** In the `Edits` arm, when the stamp's ticket equals
  `awaiting.inlay_insert` and the ops are non-empty, `land` takes the slot and calls
  `remove_inlay(key, offset)` **before** `try_edit`. Doing it afterwards is too late:
  - `try_edit` runs `after_edit` (code_editor.rs:546-553), and an edit clears `inlay_insert` (D12).
  - D3's anchoring would keep the hint after the inserted text, so the line would read
    `let x: i32: i32`.

  The arm's revision check (lsp.rs:286) runs first, so the hint's offset is exact. If `try_edit`
  fails (`Overlap`), the hint stays removed until the next edit triggers a refetch. That is
  accepted: the edits come from one hint's `textEdits`, which `hygiene` has already ordered.
- **The loop.** A local answer that lands an edit (only an insert does) leaves a `didChange`
  unsent. It also leaves unsent the requests the edit's `after_edit` recorded, such as a
  signature re-query (`drive_signature`, code_editor.rs:1660-1685). So `sync_lsp` re-runs the
  whole sync-and-pull pass until the revision stops moving. **Termination:** only the first pass
  can edit.
  - Interactions come only from gestures, in `update`, and the first pass takes the single
    interaction slot.
  - The inlay fetch that a landed edit schedules waits 300 ms for a widget wake, so it is not
    pulled.
  - The second pass therefore lands no edit and sees a stable revision.
- **Jump merging.** `apply_lsp` sets `applied.messages = self.sync_lsp(client)` (lsp.rs:126-127),
  so both the landed answer and the sync can yield a jump. The landed answer's jump wins, else the
  sync's. `save_lsp` and `jump` also return `sync_lsp`'s output, so they change with it:
  - `save_lsp` returns `update::Applied`;
  - `jump` returns `Result<update::Applied, Refusal>`.
- `close_lsp` clears the hints, abandons the slots, closes the hint card and clears the pending
  wait (R19).
- `open_lsp` schedules a wait-0 fetch (D11; R19 gives it to this phase).

**D12 — the slots this phase reads and clears.**
- `Awaiting` gains `inlays: Option<Ticket>`, `inlay_tooltip: Option<(Ticket, Key, u32)>`
  and `inlay_insert: Option<(Ticket, Key, u32)>`.
- Label jumps reuse `awaiting.definition`, so `Local`, `Open` and `Unopened` targets go through the
  existing `Definition` arm unchanged.
- Abandon rows:
  - disabling hints and `close_lsp` clear all three slots;
  - `HoverDismiss` and `ViewportChanged` clear `inlay_tooltip`;
  - an edit clears `inlay_tooltip` and `inlay_insert`.
- `take_inlay_request` and `take_inlay_interaction` each hold one slot, and a newer gesture
  supersedes the older one.

**D11 / D13 — scheduling and the toggle, as the examples meet them.**
- All triggers are ignored while hints are disabled. The triggers:
  - enabling, `open_lsp` and `set_inlay_hints(true)` wait 0;
  - an edit and `InlayRefresh` wait 300 ms, trailing;
  - a viewport change that leaves the inner half of the requested window waits 75 ms, trailing,
    capped at 300 ms.
- No timer wakes the editor. The widget stamps the pending wait on its own clock and publishes
  `Action::Wake(generation)` on the redraw that crosses it. `update` then records an
  `inlay::Request`, and the host's `sync_lsp` sends it.
- **Headless tests have no widget**, so they fire the wake themselves: `Action::Wake(g)` with
  `g` from `CodeEditor::pending_wake()` (R10).
- A widget outside the view tree (the scripted example's background tab) never wakes, so its fetch
  waits until it is shown.
- `inlay_hints(bool)` is a builder and off by default. `set_inlay_hints(bool)` is the runtime
  toggle. Turning hints off clears the store, abandons the slots, closes a showing hint tooltip and
  clears the pending wait. The library has no built-in key, so **the examples bind one.**

**D14 — interactions only on a current set.** The editor records an `Interaction` only while
`doc.inlays_revision() == Some(doc.revision())`. After any edit, a test must refetch before it
sends another gesture.

**D10 — the gestures the tests send** (as `Event::Editor(Action::…)`; the widget publishes the same
actions):
- a hover over a label part sends `InlayHover { key, part }`;
- a Ctrl+click on a part with a location sends `InlayJump { key, part }`;
- a double-click on an insertable hint sends `InlayInsert { key, offset }`.

**D16 / D18 — what the client hands the glue.**
- `Client::inlays` answers `Change::Inlays(Some(placed))`. A decline or a `null` result answers
  `Some(vec![])`, and a failure answers `None`.
- `Client::interact` is gated on `ticket.revision() == synced == set revision` and a known key:
  - **Jump** answers `Change::Definition(target)` locally, with no round trip. The target is
    `Local`, `Open` (stale-checked) or `Unopened`.
  - **Insert** answers `Change::Edits(hygiene(text_edits))` locally, stamped with the ticket. A
    decline answers `Change::Edits(vec![])`, which settles the slot without editing (R16); D19
    removes the hint only when the ops are non-empty.
  - **Tooltip** answers locally when a tooltip is already known. Otherwise it sends
    `inlayHint/resolve` when the server resolves and the hint has `data`, or else a location
    hover. The answer is `Change::InlayTooltip(Option<String>)`.
- The client advertises lazy `tooltip` and `label.tooltip` only. Locations and text edits come
  inline.

**Constraints.**
- No library behaviour changes outside `code_editor/lsp.rs`.
- No `std::time`, threads or I/O in anything the wasm build compiles. That covers `examples/lsp`
  and its tests. `rust_analyzer`'s app module is native-only already.
- No new dependencies.

Implementation choices this doc makes where the plan leaves room (none changes behaviour the plan
specifies):

- **The pass is a private `sync_pass`, and `route` returns `update::Applied`.** A private
  `absorb(into, routed)` merges two results: messages append, and an earlier jump is kept. Within
  one `sync_lsp` at most one definition ticket can be accepted, because `set_definition` abandons
  the slot. "Earlier wins" therefore only matters in `apply_lsp`, where the plan says the landed
  answer wins.
- **The interaction is pulled last in a pass.** Every client call in a pass is made against one
  snapshot before anything lands, and outputs land in order. With the interaction last, an
  insert's edit lands after the other local answers. Those answers still match the revision they
  were made at.
- **A `debug_assert!` bounds the loop at two passes**, so a future edit-producing local answer
  fails the tests loudly instead of hanging them.
- **`open_lsp` keeps returning `Result<Vec<Message>, Error>`.** It takes `route(..).messages` and
  debug-asserts the jump is `None`: an open lands cached diagnostics only.
- **The examples route jumps through one helper (`follow`)**, used by the editor arm and the LSP
  arm alike. It also follows a jump that the target editor's own `jump` call returns, which ends
  the chain without an assert.

## Step-by-step changes

Commit boundaries are in "Commit boundaries" below. Steps 1–3 form commit 1, steps 4–6 commit 2,
and step 7 commit 3.

### 1. `crates/scrive-iced/src/code_editor/lsp.rs`: the glue

**a. Module doc** (lines 1-7). Before:

```rust
//! A host calls [`open_lsp`](CodeEditor::open_lsp) once per document,
//! [`sync_lsp`](CodeEditor::sync_lsp) after every [`update`](CodeEditor::update), and
//! [`apply_lsp`](CodeEditor::apply_lsp) for each `Update::Document` the client returns, and
//! [`save_lsp`](CodeEditor::save_lsp) after writing the document to disk. Each call returns the
//! messages to send; the transport stays the host's.
```

After:

```rust
//! A host calls [`open_lsp`](CodeEditor::open_lsp) once per document,
//! [`sync_lsp`](CodeEditor::sync_lsp) after every [`update`](CodeEditor::update),
//! [`apply_lsp`](CodeEditor::apply_lsp) for each `Update::Document` the client returns, and
//! [`save_lsp`](CodeEditor::save_lsp) after writing the document to disk. Each call returns the
//! messages to send, and all but `open_lsp` a jump into another document for the host to route;
//! the transport stays the host's.
```

**b. Imports.** `use iced::time::Duration;` for `open_lsp`'s trigger. `Ticket` is not needed if the
`Edits` branch is inline as shown below. Don't add an alias.

**c. `open_lsp`** (line 50, `Ok(self.route(output))`). After:

```rust
        self.lsp_client = Some(client.id());
        let routed = self.route(output);
        debug_assert!(routed.jump.is_none(), "an open lands cached diagnostics only");
        Ok(routed.messages)
```

Then D11's wait-0 trigger (R19), after `lsp_client` is stored and before `route`:
`self.wait_inlays(Duration::ZERO, None);`. It is a no-op while hints are disabled. It covers a tab
opened after the handshake and a reopen after `close_lsp`; documents opened before the handshake
are also re-armed by the post-`initialized` refresh (D16).

**d. `sync_lsp`** (lines 53-98). Before (abridged):

```rust
    #[must_use = "the messages must be sent to the server"]
    pub fn sync_lsp(&mut self, client: &mut Client) -> Vec<Message> {
        let Some(registered) = self.lsp_client else {
            return Vec::new();
        };
        debug_assert_eq!(registered, client.id(), "sync_lsp needs the Client this editor was opened with");
        let snapshot = self.doc.snapshot();
        let mut outputs = vec![client.sync(&snapshot, self.doc.drain_changes())];
        if let Some(request) = self.take_completion_request() { … }
        … signature, hover, definition, rename, format …
        let mut messages = Vec::new();
        for output in outputs {
            messages.extend(self.route(output));
        }
        messages
    }
```

After:

```rust
    /// Mirror every edit since the last sync to `client`, then send each request the editor
    /// recorded: completion, signature help, hover, definition, rename, format, an inlay-hint
    /// fetch, and an inlay-hint gesture. Call it after every [`update`](CodeEditor::update);
    /// with nothing new it returns nothing.
    ///
    /// Answers the client gives without asking the server land before it returns: declines,
    /// reused completion lists, and a hint's label jump or text edits. An edit that lands this
    /// way is synced too. A label jump into another document comes back as
    /// [`Applied::jump`](update::Applied::jump) for the host to route; `refused` is always
    /// `None`.
    ///
    /// On an editor that is not registered, before [`open_lsp`](CodeEditor::open_lsp) or after
    /// [`close_lsp`](CodeEditor::close_lsp), it does nothing. Its `client` must be the one the
    /// editor was opened with.
    #[must_use = "Applied.messages must be sent, and Applied.jump routed"]
    pub fn sync_lsp(&mut self, client: &mut Client) -> update::Applied {
        let mut synced = update::Applied::default();
        let Some(registered) = self.lsp_client else {
            return synced;
        };
        debug_assert_eq!(
            registered,
            client.id(),
            "sync_lsp needs the Client this editor was opened with",
        );
        // An inserted hint's edit lands inside a pass; its didChange, and the requests the
        // edit recorded, go out on the next one. That pass lands no edit: interactions come
        // only from gestures, the first pass took the one slot, and the fetch the edit
        // scheduled waits for a wake.
        let mut passes = 0;
        loop {
            passes += 1;
            debug_assert!(passes <= 2, "only the first pass lands an edit");
            let revision = self.doc.revision();
            absorb(&mut synced, self.sync_pass(client));
            if self.doc.revision() == revision {
                return synced;
            }
        }
    }

    /// One pass: drain and sync the edits, send every recorded request against one snapshot,
    /// then land the client's local answers in order.
    fn sync_pass(&mut self, client: &mut Client) -> update::Applied {
        let snapshot = self.doc.snapshot();
        // The client ignores a request from a revision it has not been synced to.
        let mut outputs = vec![client.sync(&snapshot, self.doc.drain_changes())];
        if let Some(request) = self.take_completion_request() {
            outputs.push(client.complete(&snapshot, &request));
        }
        if let Some(request) = self.take_signature_request() {
            outputs.push(client.signature_help(&snapshot, &request));
        }
        if let Some(request) = self.take_hover_request() {
            outputs.push(client.hover(&snapshot, &request));
        }
        if let Some(request) = self.take_definition_request() {
            outputs.push(client.definition(&snapshot, &request));
        }
        if let Some(request) = self.take_rename_request() {
            outputs.push(client.rename(&snapshot, &request));
        }
        if let Some(request) = self.take_format_request() {
            outputs.push(client.format(&snapshot, &request));
        }
        if let Some(request) = self.take_inlay_request() {
            outputs.push(client.inlays(&snapshot, &request));
        }
        // Last, so an insert's edit lands after the answers made at the revision it moves.
        if let Some(interaction) = self.take_inlay_interaction() {
            outputs.push(client.interact(&snapshot, &interaction));
        }
        let mut passed = update::Applied::default();
        for output in outputs {
            absorb(&mut passed, self.route(output));
        }
        passed
    }
```

Trim the doc comments to DISPATCH override 1. They are content hints.

**e. `save_lsp`** (lines 100-114). After:

```rust
    /// Tell `client` that the document was saved, syncing first so the server holds the saved
    /// text. Call it after writing the document to disk. Returns the sync's messages and jump,
    /// then a `didSave` when the server asks for saves.
    ///
    /// On an editor that is not registered it does nothing.
    #[must_use = "Applied.messages must be sent, and Applied.jump routed"]
    pub fn save_lsp(&mut self, client: &mut Client) -> update::Applied {
        if self.lsp_client.is_none() {
            return update::Applied::default();
        }
        let mut saved = self.sync_lsp(client);
        let output = client.save(&self.doc.snapshot());
        absorb(&mut saved, self.route(output));
        saved
    }
```

**f. `apply_lsp`** (lines 116-129). Before:

```rust
        let mut applied = self.land(document);
        applied.messages = self.sync_lsp(client);
        applied
```

After:

```rust
        let landed = self.land(document);
        let synced = self.sync_lsp(client);
        update::Applied {
            messages: synced.messages,
            // The answer the host passed in is the one it is waiting on.
            jump: landed.jump.or(synced.jump),
            refused: landed.refused,
        }
```

Amend the doc to say that `jump` may also be a hint's label jump that the sync landed.

**g. `jump`** (lines 131-150). The return type becomes `Result<update::Applied, Refusal>`, and the
body ends `Ok(self.sync_lsp(client))`. Change the doc's "Returns the messages to send" to "Returns
what the sync after the selection produced".

**h. `close_lsp`** (lines 152-180). After the existing `abandon` calls (lines 166-169), add the
same clearing as `set_inlay_hints(false)` without flipping `enabled` (R19):

```rust
        self.inlay_card = None;
        self.abandon(Awaited::Inlays);
        self.abandon(Awaited::InlayTooltip);
        self.abandon(Awaited::InlayInsert);
        self.pending_inlay_interaction = None;
        self.inlays.wait = None;
        self.inlays.window = None;
        self.doc.clear_inlays();
```

- `abandon(Awaited::Inlays)` also drops `pending_inlay_request`, and the tooltip and insert arms
  drop the interaction made under their ticket (Phase 5). The explicit
  `pending_inlay_interaction = None` covers a jump gesture whose ticket a newer gesture's slot
  replaced, as the function already does for `pending_rename_request` and
  `pending_format_request`.
- The hint card lives in `inlay_card`, not `self.hover`, so it is closed here.
- The toggle (`inlays.enabled`) stays as it is.

Doc: add "the inlay hints and their tooltip are cleared, and a scheduled hint fetch is dropped" to
the list of what closes and is forgotten.

**i. `route`** (lines 182-202). After:

```rust
    /// Land `output`'s updates, and return its messages with the jump a landed definition
    /// points to. `open` and the request methods answer only the document they were given,
    /// under its current ticket or revision; a refused local answer is dropped, as a late
    /// reply would be.
    fn route(&mut self, output: Output) -> update::Applied {
        let Output { messages, updates } = output;
        debug_assert!(
            updates
                .iter()
                .all(|update| matches!(update, Update::Document(_))),
            "open and requests answer with document updates only",
        );
        let mut routed = update::Applied {
            messages,
            ..update::Applied::default()
        };
        for update in updates {
            if let Update::Document(document) = update {
                let landed = self.land(document);
                if routed.jump.is_none() {
                    routed.jump = landed.jump;
                }
            }
        }
        routed
    }
```

The first `debug_assert!` stays. `interact` and `inlays` answer with document updates only (D18):
the `InlayRefresh` fan-out arrives through `Client::receive`, never through `route`.

**j. `land`'s `Edits` arm** (lines 281-293). Before:

```rust
            Change::Edits(ops) => {
                let revision = match stamp {
                    Stamp::Ticket(ticket) => ticket.revision(),
                    Stamp::Revision(revision) => revision,
                };
                if revision != self.doc.revision() {
                    return refused(Refusal::Stale);
                }
                match self.try_edit(ops) {
                    Ok(()) => update::Applied::default(),
                    Err(_) => refused(Refusal::Overlap),
                }
            }
```

After:

```rust
            Change::Edits(ops) => {
                let revision = match stamp {
                    Stamp::Ticket(ticket) => ticket.revision(),
                    Stamp::Revision(revision) => revision,
                };
                if revision != self.doc.revision() {
                    return refused(Refusal::Stale);
                }
                let inserting = match stamp {
                    Stamp::Ticket(ticket) => self
                        .awaiting
                        .inlay_insert
                        .as_ref()
                        .is_some_and(|(awaited, ..)| *awaited == ticket),
                    Stamp::Revision(_) => false,
                };
                if inserting && ops.is_empty() {
                    // A declined insert: nothing to apply, and `try_edit`'s tail would close the
                    // popup and the hover card for no edit.
                    self.abandon(Awaited::InlayInsert);
                    return update::Applied::default();
                }
                if inserting {
                    // The edits spell out the hint's label at its offset. Left in place, the
                    // hint would render beside its own text, and `try_edit`'s tail clears the
                    // slot that names it.
                    if let Some((_, key, offset)) = self.awaiting.inlay_insert.take() {
                        let _ = self.doc.remove_inlay(key, offset);
                    }
                }
                match self.try_edit(ops) {
                    Ok(()) => update::Applied::default(),
                    Err(_) => refused(Refusal::Overlap),
                }
            }
```

- `remove_inlay` returns whether it removed a hint; ignore it: a refetch at the same revision may
  already have replaced the set.
- An empty batch for the awaited ticket (a declined insert) settles the slot and returns before
  `try_edit` (R16). An empty batch under any other ticket goes through `try_edit` as today.
- The other `land` arms, including Phase 6's three, don't change.

**k. A free helper** next to `refused`:

```rust
/// Append `routed` to `into`: its messages follow, and `into` keeps an earlier jump.
fn absorb(into: &mut update::Applied, routed: update::Applied) {
    into.messages.extend(routed.messages);
    if into.jump.is_none() {
        into.jump = routed.jump;
    }
}
```

### 2. `code_editor/lsp.rs` tests: adapt, then add

**Adapt** (mechanical; keep every expectation):
- `drive` (lines 418-422) returns `editor.sync_lsp(client).messages`. The tests that use it expect
  no jump.
- `close_lsp_clears_diagnostics_and_sends_did_close` (line 917): `ed.sync_lsp(&mut client).messages.is_empty()`.
- `a_cross_document_definition_selects_in_the_target_editor` (line 820): bind
  `let applied = b.jump(..).expect(..)`, then assert `applied.messages.is_empty()` and
  `applied.jump.is_none()`.
- `jump_refuses_a_target_whose_document_moved` (line 835): `Err(Refusal::Stale)` still compares,
  as long as `update::Applied` implements `PartialEq`. **It doesn't** (`#[derive(Debug, Default)]`,
  update.rs:98). Write `assert!(matches!(b.jump(&mut client, open), Err(Refusal::Stale)), "…")`
  instead. Don't derive `PartialEq` on `Applied` for a test.
- `save_lsp_syncs_then_sends_did_save` (line 973): `ed.save_lsp(&mut client).messages.iter()`.
- `save_lsp_on_an_unregistered_editor_does_nothing` (line 998) and
  `sync_lsp_on_an_unregistered_editor_does_nothing` (line 1007): `.messages.is_empty()`.

**Add helpers:**

```rust
    /// The capabilities `ready` negotiates, plus an inlay-hint provider that resolves tooltips.
    fn ready_with_hints() -> Client { … }

    /// Feed `action` through the editor's update path, then sync, keeping the jump.
    fn gesture(editor: &mut CodeEditor, client: &mut Client, action: Action) -> update::Applied {
        let _ = editor.update(Event::Editor(action), Instant::now());
        editor.sync_lsp(client)
    }

    /// Fire the editor's scheduled hint fetch, as its widget does once the delay passes, then
    /// sync. Returns what went out.
    fn fetch_inlays(editor: &mut CodeEditor, client: &mut Client) -> Vec<Message> { … }

    /// The hints `editor` shows, as `(render offset, key)`.
    fn shown(editor: &CodeEditor) -> Vec<(u32, Key)> { … }
```

- Factor `ready()` (lines 386-407) into `handshake(capabilities: Value) -> Client`. `ready()` then
  passes today's JSON, and `ready_with_hints()` adds
  `"inlayHintProvider": { "resolveProvider": true }`. Don't add the provider to `ready()` itself:
  every existing test would then see the post-`initialized` refresh path for no reason.
- `fetch_inlays` reads `editor.pending_wake().expect("a fetch is scheduled").generation` (R10)
  and drives `Action::Wake(generation)`.

**Add three tests.** Fixture: text `"let x = f();\n"`, so `x` ends at byte 5, positions are byte
columns (`utf-8`), and the hint reply is:

```rust
json!([{
    "position": { "line": 0, "character": 5 },
    "kind": 1,
    "label": [
        { "value": ": " },
        { "value": "Foo", "location": { "uri": "file:///w/foo.rs", "range": on_line(0, 7, 10) } }
    ],
    "textEdits": [{ "range": on_line(0, 5, 5), "newText": ": Foo" }]
}])
```

The flow, shared by the first two tests:
1. `CodeEditor::new(TEXT).inlay_hints(true)` is opened on `ready_with_hints()`.
2. `fetch_inlays` returns the `textDocument/inlayHint` request.
3. `apply_lsp` of `reply(&request, fixture)` has `refused == None`.
4. `shown(&ed)` is `[(5, key)]`.

```rust
    /// A Ctrl+click on a label part that points into a file nobody opened comes back from
    /// `sync_lsp` itself, with no request to the server.
    #[test]
    fn sync_lsp_returns_an_inlay_label_jump_into_an_unopened_file() { … }
```
Asserts:
- `gesture(InlayJump { key, part: 1 })` returns `jump == Some(Jump::Unopened(u))` with
  `u.uri().as_str() == "file:///w/foo.rs"`;
- `refused == None`;
- `all_sent(&applied.messages, "textDocument/definition")` is empty.

```rust
    /// A double-clicked hint inserts its text once: the hint goes before the text lands, and the
    /// edit's didChange leaves in the same sync_lsp call.
    #[test]
    fn an_inlay_insert_lands_once_and_syncs_in_the_same_call() { … }
```
Asserts:
- after `gesture(InlayInsert { key, offset: 5 })`, the text is `"let x: Foo = f();\n"`;
- `shown(&ed)` is empty;
- `sent(&applied.messages, "textDocument/didChange")` exists and carries the next version;
- `applied.jump.is_none()`.

```rust
    /// close_lsp drops the shown hints and every inlay slot, so nothing waits on a reply that
    /// can no longer land.
    #[test]
    fn close_lsp_clears_the_hints_and_their_slots() { … }
```
Flow: fetch and land the hints, then send `InlayHover { key, part: 1 }` through
`update` only, **without** syncing, so the interaction is pending. Then `close_lsp`. Asserts:
- `shown(&ed)` is empty;
- `ed.take_inlay_interaction().is_none()`;
- `ed.take_inlay_request().is_none()`;
- `awaiting.inlays`, `awaiting.inlay_tooltip` and `awaiting.inlay_insert` are all `None`.

`part` is a `u32` and `InlayHover` carries no offset (R7).

### 3. Hosts move to the new return (part of commit 1, so it builds)

Clippy `--all-targets --all-features` builds both examples, so commit 1 must carry their mechanical
adoption.

**`examples/lsp/main.rs`.** Add `follow`, and use it in both arms.

```rust
/// Route `jump` to the tab whose document holds it, and so on for any jump that tab's sync
/// returns. Returns the messages to send. A jump into an unopened file is logged: a host with
/// files would read it, open a tab, `open_lsp` it, then `select(unopened.span(&text))`. Every
/// demo file is open, and wasm has no disk.
fn follow(
    tabs: &mut [Tab],
    active: &mut tab::Id,
    client: &mut lsp::Client,
    transport: &mut Transport,
    mut jump: Option<lsp::update::Jump>,
) -> Vec<lsp::Message> {
    let mut outgoing = Vec::new();
    while let Some(next) = jump.take() {
        match next {
            lsp::update::Jump::Open(open) => {
                let Some(target) = tabs
                    .iter_mut()
                    .find(|tab| tab.editor.document().doc_id() == open.doc_id())
                else {
                    break;
                };
                match target.editor.jump(client, open) {
                    Ok(applied) => {
                        outgoing.extend(applied.messages);
                        jump = applied.jump;
                        *active = target.id;
                    }
                    Err(refusal) => transport.note(format!("jump refused: {refusal}")),
                }
            }
            lsp::update::Jump::Unopened(unopened) => {
                transport.note(format!("definition in unopened {}", unopened.uri().as_str()));
            }
        }
    }
    outgoing
}
```

`Message::Editor` arm (lines 139-146). After:

```rust
            Message::Editor(id, event) => {
                let Some(tab) = tabs.iter_mut().find(|tab| tab.id == id) else {
                    return Task::none();
                };
                let task = tab.editor.update(event, now).map(Message::Editor.with(id));
                let synced = tab.editor.sync_lsp(client);
                let mut outgoing = synced.messages;
                outgoing.extend(follow(tabs, active, client, transport, synced.jump));
                Task::batch([task, transport.send(outgoing)])
            }
```

In the `Message::Lsp` arm, replace the whole `match applied.jump { … }` (lines 171-198) with
`outgoing.extend(follow(tabs, active, client, transport, applied.jump));`. The `refused` note
stays.

**`examples/rust_analyzer.rs`.** Add, inside `mod app`:

```rust
    /// The status line a jump earns. A host with several editors hands `Jump::Open` to the one
    /// that owns `open.doc_id()`; here every open document is local.
    fn jumped(jump: Option<lsp::update::Jump>) -> Option<String> {
        match jump? {
            lsp::update::Jump::Open(_) => Some("definition in another open document".to_owned()),
            lsp::update::Jump::Unopened(unopened) => {
                Some(format!("definition in {}, which is not open", unopened.uri()))
            }
        }
    }
```

- Editor arm (lines 479-483): call `let synced = editor.sync_lsp(client);`, then
  `link.send(synced.messages);`, then `if let Some(line) = jumped(synced.jump) { *status = line; }`.
- Save arm (line 489): the same with `editor.save_lsp(client)`. The save status line written
  after it wins, as it does today.
- Lsp arm (lines 529-542): replace the `match applied.jump` with the same `jumped` call.
- Ignored test (lines 921-929): `saved.messages.iter()` and `sender.send(saved.messages)`.

### 4. `examples/lsp/server.rs`: the hint script

**a. Module doc** (lines 1-8). Add to the computed list: inlay hints (a type hint after
`let x = f(…)` for a function some document defines, and a parameter hint before each call's
first argument) and their tooltips through `inlayHint/resolve`.

**b. `INITIALIZE`** (lines 17-29). Add `"inlayHintProvider": { "resolveProvider": true }`.

**c. A constant:**

```rust
/// Where the scripted standard library's `String` lives. No editor opens it, so a jump to it
/// comes back as `Jump::Unopened`.
const STRING_URI: &str = "file:///demo/std/string.rs";
```

**d. `answer`** (lines 122-137). Add two arms:

```rust
            "textDocument/inlayHint" => self.inlay_hints(params),
            "inlayHint/resolve" => resolve(params),
```

**e. New items** in `impl Scripted` and as free functions:

```rust
/// A function some document defines as `fn name(param: …) -> Returns`.
struct Signature<'a> {
    name: &'a str,
    param: &'a str,
    returns: &'a str,
    /// Where the parameter's name is, as an LSP location.
    param_location: Value,
}

impl Scripted {
    /// Every `fn name(param: …) -> Returns` in any document, with a first parameter and a
    /// return type on its own line.
    fn signatures(&self) -> Vec<Signature<'_>> {
        let mut found = Vec::new();
        for (uri, document) in &self.documents {
            let text = document.text.as_str();
            for (at, _) in text.match_indices("fn ") {
                let line = &text[..text[at..].find('\n').map_or(text.len(), |i| at + i)];
                let name_at = at + "fn ".len();
                let name = word(text, name_at);
                if name.is_empty() || !line[name_at + name.len()..].starts_with('(') {
                    continue;
                }
                let param_at = name_at + name.len() + "(".len();
                let Some(arrow) = line[param_at..].find(") -> ") else { continue };
                let param = word(text, param_at);
                let returns = word(text, param_at + arrow + ") -> ".len());
                if param.is_empty() || returns.is_empty() {
                    continue;
                }
                let range = json!({ "start": position(text, param_at), "end": position(text, param_at + param.len()) });
                found.push(Signature { name, param, returns, param_location: json!({ "uri": uri, "range": range }) });
            }
        }
        found
    }

    /// The hints for one document. The request's range is ignored: the client drops what falls
    /// outside the span it asked for.
    fn inlay_hints(&self, params: &Value) -> Value {
        let Some(document) = params["textDocument"]["uri"].as_str().and_then(|uri| self.documents.get(uri)) else {
            return Value::Null;
        };
        let text = document.text.as_str();
        let signatures = self.signatures();
        let mut hints = Vec::new();
        for signature in &signatures {
            for at in occurrences(text, signature.name) {
                let open = at + signature.name.len();
                let defines = text[..at].ends_with("fn ");
                if defines || !text[open..].starts_with('(') || text[open + 1..].starts_with(')') {
                    continue;
                }
                hints.push(parameter_hint(text, open + 1, signature));
            }
        }
        for (at, _) in text.match_indices("let ") {
            let name_at = at + "let ".len();
            let name_end = name_at + word(text, name_at).len();
            let rest = &text[name_end..text[name_end..].find('\n').map_or(text.len(), |i| name_end + i)];
            // `let x: T = …` already states its type.
            if name_end == name_at || !rest.starts_with(" = ") {
                continue;
            }
            if let Some(signature) = signatures.iter().find(|s| rest.contains(&format!("{}(", s.name))) {
                hints.push(type_hint(text, name_end, signature));
            }
        }
        Value::Array(hints)
    }
}

/// `param:` before a call's first argument; the name links to the parameter.
fn parameter_hint(text: &str, at: usize, signature: &Signature<'_>) -> Value {
    json!({
        "position": position(text, at),
        "kind": 2,
        "label": [
            { "value": signature.param, "location": signature.param_location },
            { "value": ":" }
        ],
        "paddingRight": true,
        "data": { "tooltip": format!("The `{}` parameter of `{}`.", signature.param, signature.name) }
    })
}

/// `: Returns` after a `let` name, insertable; `String` links into the unopened standard library.
fn type_hint(text: &str, at: usize, signature: &Signature<'_>) -> Value {
    let mut returns = json!({ "value": signature.returns });
    if signature.returns == "String" {
        returns["location"] = json!({
            "uri": STRING_URI,
            "range": { "start": { "line": 0, "character": 11 }, "end": { "line": 0, "character": 17 } }
        });
    }
    json!({
        "position": position(text, at),
        "kind": 1,
        "label": [{ "value": ": " }, returns],
        "textEdits": [{
            "range": { "start": position(text, at), "end": position(text, at) },
            "newText": format!(": {}", signature.returns)
        }],
        "data": { "tooltip": format!("What `{}` returns.", signature.name) }
    })
}

/// `inlayHint/resolve`: the hint, with the tooltip its `data` carries. A real server would look
/// the tooltip up; the script keeps it in `data` so the round trip stays visible.
fn resolve(params: &Value) -> Value {
    let mut hint = params.clone();
    hint["tooltip"] = json!({ "kind": "markdown", "value": params["data"]["tooltip"] });
    hint
}

/// The identifier starting at byte `at`, empty when none does.
fn word(text: &str, at: usize) -> &str {
    let end = text[at..].find(|c: char| !is_word(c)).map_or(text.len(), |i| at + i);
    &text[at..end]
}
```

What it yields on the demo files:
- **main.rs.** A type hint `: String` at the end of `message` in
  `let message = util::greet("scrive");`. It is insertable, and its `String` part links to
  `STRING_URI`. A parameter hint `name:` before `"scrive"`, whose `name` part links to `name` in
  util.rs's `greet(name: &str)`.
- **util.rs.** No hints: the `greet`/`farewell` occurrences there are definitions, and it has no
  `let`.
- **After a rename** (`greet` → `welcome`), the hints follow the new name.
- **After the insert**, the `let` reads `let message: String = …`, so no type hint comes back.

`word`, `signatures`, `parameter_hint`, `type_hint` and `resolve` are all reached from `answer`, so
the binary build has no dead code.

### 5. `examples/lsp/main.rs`: hints on, the toggle key, the tests

**a. Module doc** (lines 7-18). Add to "Try:":
- Ctrl+click `name` in the parameter hint: the util.rs tab opens with the parameter selected;
- Ctrl+click `String` in the type hint: the panel notes the jump into an unopened file;
- double-click the type hint: `: String` is inserted, and the hint goes;
- hover a hint: its tooltip, resolved by the server;
- Ctrl+I (Cmd+I on macOS): hints off and on.

**b. Editors start with hints on** (line 103):
`CodeEditor::new(source).language(rust()).rename(true).inlay_hints(true)`.

**c. State and message.** Add the field `hints: bool` to `App`, initialised `true` and documented
"Whether inlay hints show; Ctrl+I flips it for every tab". Add the variant
`Message::ToggleHints` ("Ctrl+I: turn inlay hints off or on").

**d. The arm:**

```rust
            Message::ToggleHints => {
                *hints = !*hints;
                for tab in tabs.iter_mut() {
                    tab.editor.set_inlay_hints(*hints);
                }
                transport.note(format!("inlay hints {}", if *hints { "on" } else { "off" }));
                Task::none()
            }
```

Turning hints on schedules a wait-0 fetch per editor, and the shown tab's widget wakes it. The
background tab fetches when shown (D11's documented limit). Nothing needs to be sent here.

**e. Subscription** (lines 237-248). Batch the active editor's subscription with
`keyboard::listen().filter_map(toggle_chord)`:

```rust
    /// Only the active tab listens, so global chords (Ctrl+F) reach one editor; Ctrl+I reaches
    /// the app.
    fn subscription(&self) -> Subscription<Message> {
        let editor = match self.tabs.iter().find(|tab| tab.id == self.active) {
            Some(tab) => tab.editor.subscription().with(tab.id).map(|(id, event)| Message::Editor(id, event)),
            None => Subscription::none(),
        };
        Subscription::batch([editor, keyboard::listen().filter_map(toggle_chord)])
    }
```

**f. The chord** (free function; `use iced::keyboard::{self, Key};`):

```rust
/// Ctrl+I, or Cmd+I on macOS, without Shift or Alt and not repeated. The editor binds no Ctrl+I,
/// so the key reaches `keyboard::listen`.
fn toggle_chord(event: keyboard::Event) -> Option<Message> {
    match event {
        keyboard::Event::KeyPressed { key: Key::Character(c), modifiers, repeat: false, .. }
            if c == "i" && modifiers.command() && !modifiers.shift() && !modifiers.alt() =>
        {
            Some(Message::ToggleHints)
        }
        _ => None,
    }
}
```

The chord is Ctrl+I (`command()`, Cmd+I on macOS), in both examples (R13). Check it the way the plan's verification does: `interpret_key`
(editor.rs:3519) returns `None` for an unbound Ctrl+letter, and `find_chord`
(code_editor.rs:2091) binds only `f`, `h`, Escape, Alt+Enter and Tab.

**g. Tests.** Add to the existing module. New helpers first:

```rust
    /// Fire tab `id`'s scheduled hint fetch, as its widget does once the delay has passed, and
    /// let the conversation finish.
    fn fetch(app: &mut App, id: tab::Id) {
        let tab = app.tabs.iter().find(|tab| tab.id == id).expect("the tab exists");
        let generation = tab.editor.pending_wake().expect("a fetch is scheduled").generation; // R10
        press(app, id, Event::Editor(Action::Wake(generation)));
    }

    /// The hints tab `id` shows, as `(render offset, key)`, in offset order.
    fn hints(app: &App, id: tab::Id) -> Vec<(u32, Key)> { … }

    /// Where `needle` starts in `text`, as an editor offset.
    fn at(text: &str, needle: &str) -> u32 {
        text.find(needle).unwrap_or_else(|| panic!("{needle:?} is in the demo")) as u32
    }

    /// main.rs after boot with its hints fetched: `(type hint key, parameter hint key)`.
    fn hinted() -> (App, Key, Key) { … }
```

`hinted` boots, `fetch`es MAIN, and picks the keys by offset:
- the type hint renders at `at(MAIN_RS, "let message") + "let message".len()`;
- the parameter hint renders at `at(MAIN_RS, "\"scrive\"")`.

Tests (each with a `///` doc stating its invariant, and a string message on every assert):

| Test | Steps | Asserts |
|---|---|---|
| `the_hint_script_lands_in_main_rs` | `hinted()` | exactly two hints in MAIN, at the two offsets above; `→ textDocument/inlayHint` in the traffic |
| `ctrl_click_on_a_parameter_hint_opens_util_rs_at_the_parameter` | `press(MAIN, InlayJump { key: param, part: 0 })` | `app.active == UTIL`; UTIL's selection is `at(UTIL_RS, "(name") + 1` plus 4 |
| `ctrl_click_on_a_type_hint_jumps_into_an_unopened_file_through_sync_lsp` | `press(MAIN, InlayJump { key: ty, part: 1 })` | the traffic holds `· definition in unopened file:///demo/std/string.rs`; no `→ textDocument/definition` line; `app.active == MAIN` |
| `double_click_inserts_the_type_once_and_leaves_no_duplicate_hint` | `press(MAIN, InlayInsert { key: ty, offset })`; then `fetch(MAIN)` | after the press: text is `MAIN_RS` with `": String"` inserted at `offset`, `matches(": String").count() == 1`, no hint keyed `ty`, no hint at `offset`, `server.text(MAIN_URI)` equals the editor's text (the loop's didChange went out in the same sync); after the refetch: one hint, the parameter one |
| `hovering_a_hint_resolves_its_tooltip` | `press(MAIN, InlayHover { key: ty, part: 1 })` | the traffic holds a line starting `→ inlayHint/resolve`, and no `· refused` line after it (the card itself is covered in-crate; see Resolved questions) |
| `the_toggle_key_clears_and_restores_the_hints` | `hinted()`; `update(ToggleHints)`; then `update(ToggleHints)`, `fetch(MAIN)` | after the first toggle: no hints in MAIN, and `app.hints` is false; after the second toggle and the fetch: two hints again |
| `toggle_chord_matches_only_plain_ctrl_i` | the `save_chord_matches_only_plain_ctrl_s` shape (rust_analyzer.rs:773-800) with `i` | Ctrl+I toggles; Ctrl+Shift+I, Ctrl+Alt+I, a repeated Ctrl+I and a plain `i` don't |

Notes:
- `use scrive_core::intel::inlay::Key;` inside the test module (or the path Phase 1 exports).
- The `server.text` check in the insert test is what pins D19's loop. Without the loop, nothing
  after the insert sends its `didChange`: the press's sync produced no messages, so `settle`
  returns at once and the server keeps the old text.
- The "unopened" test passes through the editor arm's `follow`. That is the "Unopened label jump
  through `sync_lsp`" the plan names.
- Fetch only MAIN. UTIL's widget never wakes in a test either, and it has no hints anyway.

### 6. `examples/rust_analyzer.rs`: hints, the toggle, the ignored test

**a. Module doc** (lines 8-13). Add: "Inlay hints are on: double-click a type hint to insert it,
Ctrl+click a part to jump, hover one for its tooltip. Ctrl+I (Cmd+I on macOS) turns them off and
on."

**b. App.** Add the editor builder `.inlay_hints(true)` (lines 450-452) and an `App` field
`hints: bool`, initialised `true`. Add `Message::ToggleHints`, and this arm:

```rust
                Message::ToggleHints => {
                    *hints = !*hints;
                    editor.set_inlay_hints(*hints);
                    *status = format!("inlay hints {}", if *hints { "on" } else { "off" });
                    Task::none()
                }
```

Add `keyboard::listen().filter_map(toggle_chord)` to the `Subscription::batch` (lines 579-585). It
sits next to the `save_chord` listener. Each `filter_map` hashes its function's `TypeId`, so the
two listeners stay distinct subscriptions. Add the same `toggle_chord` as step 5f, with its test.

**c. The ignored test.** Add
`rust_analyzer_hints_arrive_move_refresh_and_insert_once`, `#[ignore = "needs rust-analyzer on PATH
and a Rust toolchain; run with --ignored"]`, beside the existing one. Shape:

```rust
    /// Against a real rust-analyzer: the scratch file's `let sum = add(1, 2);` gets its `: i32`
    /// hint after load; an edit above moves it before any refetch; the refresh the edit
    /// triggers refetches at the new revision; and a double-click inserts `: i32` once, with no
    /// hint left beside it.
    #[test]
    #[ignore = "needs rust-analyzer on PATH and a Rust toolchain; run with --ignored"]
    fn rust_analyzer_hints_arrive_move_refresh_and_insert_once() { … }
```

1. **Setup**, as the existing test does (lines 863-882): a scratch crate under its own temp
   directory, `transport::spawn`, the client, and `CodeEditor::new(..).inlay_hints(true)` opened.
2. **A driver**, `settle_until(&mut editor, &mut client, &incoming, &sender, deadline, done)`, a
   test helper. Each round it:
   - pumps one message with the existing `pump`;
   - applies every `Update::Document` through `editor.apply_lsp` and sends `applied.messages`;
   - fires any scheduled wake (`g` from `editor.pending_wake()`, R10) through `editor.update(Event::Editor(Action::Wake(g)), now)`,
     then sends `editor.sync_lsp(&mut client).messages`.

   It returns once `done(&editor)` holds, or `false` at the deadline (120 s). Firing a wake
   before its delay is fine: `update` checks only the generation.
3. **Arrive.** With `sum_end = SCRATCH_MAIN.find("let sum").expect(..) + "let sum".len()`, settle
   until a hint shows at `sum_end`. rust-analyzer's first non-empty answer comes after indexing,
   through the client's ContentModified re-issue.
4. **Move.**
   - `editor.try_edit(vec![EditOp::insert(main_at, "// moved\n")])`, where `main_at` is `SCRATCH_MAIN.find("fn main")` (above `sum`);
   - assert at once, before any message is pumped, that a hint shows at `sum_end + 9`;
   - send `editor.sync_lsp(&mut client).messages`.
5. **Refresh.** Settle until `inlays_revision() == Some(revision())`, with a hint still at
   `sum_end + 9`. The fetch only happens because rust-analyzer sends
   `workspace/inlayHint/refresh` after the didChange.
6. **Insert.**
   - `editor.update(Event::Editor(Action::InlayInsert { key, offset: sum_end + 9 }), now)`, then
     send `sync_lsp`'s messages;
   - assert the text contains `let sum: i32 = add(1, 2);` exactly once;
   - assert no hint shows at `sum_end + 9` or at `sum_end + 9 + ": i32".len()`.
7. **No duplicate after the refetch.** Settle until the set is current again, then assert the same
   two offsets carry no hint.
8. **Shut down** and remove the directory, as the existing test does (lines 949-965). Extract that
   tail into a test helper `shut_down(client, incoming, sender)` and call it from both tests.
   Don't change the existing test otherwise.

Log arrivals with `eprintln!` and `started.elapsed()`, as the existing test does, so a failing run
shows the timeline.

### 7. Docs (commit 3)

**`README.md`**, then `cp README.md crates/scrive-iced/README.md` and check with `cmp`:

- **Features** (lines 54-57): the list becomes "… goto definition, rename across files,
  formatting, and inlay hints."
- **Language servers** (lines 115-145). Change the sync lines to:

```rust
// After every editor update: sync, send what it returns, and route its jump.
let task = editor.update(event, now).map(Message::Editor);
let synced = editor.sync_lsp(&mut client);
let mut outgoing = synced.messages;
// synced.jump: a hint's label part in another tab — call that editor's jump().
```

  After the code block, add one short paragraph. Inlay hints are opt-in per editor:
  `.inlay_hints(true)` or `set_inlay_hints` at runtime. The editor schedules the fetches itself,
  and `sync_lsp` sends them. A hint's tooltip shows on hover, Ctrl+click on a part jumps, and a
  double-click inserts the hint's text. The library binds no toggle key.
- **Examples** (lines 157-162). For `lsp`, add: "Its main.rs shows inlay hints: Ctrl+click
  `name:` to jump to the parameter, double-click `: String` to insert it, and Ctrl+I to turn them
  off and on." For `rust_analyzer`, add: "It shows rust-analyzer's inlay hints; Ctrl+I toggles
  them."

**`crates/scrive-lsp/README.md`**:
- **What it covers** (lines 19-37). Add a bullet: **Inlay hints** for a byte span, refetched when
  the server asks; tooltips resolved on demand; a label part's location as a jump; a hint's text
  edits as an edit batch.
- **With scrive-iced** (lines 39-58):
  - `sync_lsp` also sends the inlay fetches and hint gestures the editor recorded;
  - change "Each returns the messages to send" to: "`open_lsp` and `close_lsp` return the messages
    to send; the others return `update::Applied`, the messages plus a jump into another document
    for the host to route (`jump` wraps it in a `Result`)."
- **Without scrive-iced** (lines 60-82):
  - add `Client::inlays(&Snapshot, &inlay::Request)` and `Client::interact(&Snapshot,
    &inlay::Interaction)`, with the request types from `scrive_core::intel::inlay`;
  - `Change::InlayRefresh` asks the host to refetch.
- **What it does not do** (lines 84-88). Drop "inlay hints" from the list. Add "label-part
  commands" to it.

The crate-doc bullets are already in: Phase 1 adds "show inlay hints → [`intel::inlay`]" to
scrive-core's lib.rs, and Phase 6 adds the `Client::inlays` / `Client::interact` bullet to
scrive-lsp's. Check them; don't add a second.

The `code_editor/lsp.rs` module doc changed in step 1a. `code_editor.rs`'s module doc names no
LSP method: leave it.

## Files changed

| File | Change | Commit |
|---|---|---|
| crates/scrive-iced/src/code_editor/lsp.rs | `sync_lsp` → `Applied` + loop + inlay pulls; `sync_pass`; `route` → `Applied`; `absorb`; `apply_lsp` merge; `save_lsp`/`jump` returns; `close_lsp` clears hints, slots, card and the pending wait; `Edits` arm removes the inserted hint and settles a declined one without `try_edit`; `open_lsp` takes `.messages` and schedules the wait-0 fetch (R19); tests adapted + 3 new | 1 |
| crates/scrive-iced/examples/lsp/main.rs | `follow`; editor arm routes the sync's jump | 1 |
| crates/scrive-iced/examples/rust_analyzer.rs | `jumped`; editor/save/lsp arms; test reads `.messages` | 1 |
| crates/scrive-iced/examples/lsp/server.rs | inlay provider, `textDocument/inlayHint`, `inlayHint/resolve`, `Signature` | 2 |
| crates/scrive-iced/examples/lsp/main.rs | hints on, `hints` field, `ToggleHints`, `toggle_chord`, subscription, 7 tests | 2 |
| crates/scrive-iced/examples/rust_analyzer.rs | hints on, toggle, `toggle_chord` + test, `#[ignore]` hint test, `shut_down` helper | 2 |
| README.md, crates/scrive-iced/README.md | features, Language servers block and paragraph, Examples (byte-identical) | 3 |
| crates/scrive-lsp/README.md | covers / with / without / does-not-do | 3 |

## Commit boundaries

Patches go to `.claude/map/inlay-hints/patches/phase7-<k>.patch` against the base commit your
dispatch names, with `git add -N` for new files (none are expected). Each `.msg` holds a
Conventional Commits subject and a 1-3 line "why" body.

1. `feat(iced)!: sync_lsp sends inlay fetches and gestures and returns Applied`
   - body: "A hint's label jump is answered without a round trip, so the sync must hand the host
     its jump; an inserted hint goes before its text lands, and the edit is synced in the same
     call."
   - Steps 1–3. Breaking: `sync_lsp`, `save_lsp` and `jump` change their return types.
2. `feat(examples): inlay hints in the lsp and rust-analyzer examples`
   - body: "The scripted server covers every hint gesture headlessly, and the real-server test
     pins insert-once against rust-analyzer."
   - Steps 4–6.
3. `docs: inlay hints in the READMEs and crate docs`
   - Step 7.

If commit 1 can't be made green without part of step 5 (it should be green: the examples only
adopt the new return), merge 1 and 2 and say so in the report.

## Verification

At each boundary, and in full at the end (the plan's Verification section, plus the example
runs):

```
cargo test --workspace
cargo test --workspace --all-features
cargo test -p scrive-iced --features lsp --example lsp
cargo test -p scrive-iced --features lsp --example rust_analyzer
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --all-features
cargo build --workspace --all-targets --all-features --target wasm32-unknown-unknown
cmp README.md crates/scrive-iced/README.md
```

The default-feature doc build catches an intra-doc link from ungated code to a gated item. This
phase adds no such link, but run it anyway.

No file is created in this phase, so `rustfmt` runs on nothing. Never run `cargo fmt`.

Manual, for the orchestrator (an agent must not run these; they open windows or need
rust-analyzer):
- `cargo test -p scrive-iced --features lsp --example rust_analyzer -- --ignored --nocapture`;
- `cargo run -p scrive-iced --features lsp --example lsp`: the hints render in main.rs; Ctrl+click,
  double-click, hover and Ctrl+I behave as the module doc says;
- `cargo run -p scrive-iced --features lsp --example rust_analyzer`;
- `cd crates/scrive-iced && trunk serve --release --example lsp --features lsp`.

## Spot-check tables

### `land`'s `Edits` arm

`rev` is the editor's revision, and `slot` is `awaiting.inlay_insert`.

| Stamp | Condition | `remove_inlay` | `slot` after | Text | `refused` |
|---|---|---|---|---|---|
| any | stamp's revision ≠ `rev` | no | unchanged | unchanged | `Stale` |
| `Ticket(t)` | `slot = Some((t, k, o))`, ops non-empty, disjoint | `(k, o)`, before `try_edit` | `None` (taken) | edited | `None` |
| `Ticket(t)` | `slot = Some((t, k, o))`, ops non-empty, overlapping | `(k, o)` | `None` | unchanged; the hint stays gone until the refetch | `Overlap` |
| `Ticket(t)` | `slot = Some((t, ..))`, ops empty (declined insert) | no; no `try_edit` (R16) | `None` (abandoned) | unchanged; popup and hover card stay | `None` |
| `Ticket(t)` | `slot` holds another ticket, or `None` (format, rename) | no | as `try_edit`'s tail leaves it | edited | `None` / `Overlap` |
| `Revision(r)` | `r == rev` (a rename for this document) | no | as `try_edit`'s tail leaves it | edited | `None` / `Overlap` |

### `sync_lsp` passes

| What is pending after `update` | Pass 1 | Pass 2 | Returned |
|---|---|---|---|
| a typed character | `didChange`; completion etc. | — (revision stable) | messages |
| a `Wake` that matched | `textDocument/inlayHint` | — | messages; the answer lands later through `apply_lsp` |
| `InlayJump` on a part linking into this document | interaction → `Definition(Local)` lands, selects | — (selecting doesn't move the revision) | no messages, `jump: None` |
| `InlayJump` into another open document | `Definition(Open)` lands | — | `jump: Some(Open)` |
| `InlayJump` into an unopened file | `Definition(Unopened)` lands | — | `jump: Some(Unopened)` |
| `InlayInsert` (known key, current set) | `Edits(ops)` lands; hint removed; revision moves; `after_edit` may record a signature request | `didChange`; the signature request; no interaction | both passes' messages |
| `InlayInsert` declined by the client | `Edits([])` lands; the slot settles, no `try_edit` (R16) | — | messages |
| `InlayHover`, tooltip known | `InlayTooltip(Some)` lands | — | messages |
| `InlayHover`, needs resolve | `inlayHint/resolve` sent | — | messages; the answer lands through `apply_lsp` |
| a gesture on a moved set (D14) | nothing was recorded | — | messages |

### Jump merging in `apply_lsp`

| `land(document).jump` | `sync_lsp(..).jump` | `Applied.jump` |
|---|---|---|
| `None` | `None` | `None` |
| `Some(a)` | `None` | `Some(a)` |
| `None` | `Some(b)` | `Some(b)` |
| `Some(a)` | `Some(b)` | `Some(a)` (the landed answer wins) |

The last row can't arise today, because one definition ticket is awaited at a time. The rule is
the plan's, so a later change can't silently swap the order.

### Host routing (both examples)

| Source | Field | Scripted `lsp` example | `rust_analyzer` example |
|---|---|---|---|
| `sync_lsp` after `update` | `messages` | `transport.send` | `link.send` |
| `sync_lsp` after `update` | `jump` | `follow` | status via `jumped` |
| `save_lsp` | `messages` / `jump` | — (no save) | `link.send` / `jumped` |
| `apply_lsp` | `messages` / `refused` / `jump` | send / note / `follow` | send / status / `jumped` |
| `jump` (inside `follow`) | `Ok(applied)` | send, follow `applied.jump`, switch tab | — |
| `jump` (inside `follow`) | `Err(refusal)` | `jump refused: …` note | — |

### Scripted hint exchanges

Offsets and versions are illustrative.

| User action | Client → server | Server → client | Result |
|---|---|---|---|
| boot (hints on) | `initialize` … `initialized`, `didOpen` ×2 | diagnostics | an `InlayRefresh` per document (D16 fan-out) schedules a fetch |
| (the shown tab's wake) | `textDocument/inlayHint` main.rs | `[param hint, type hint]` | `name:` before `"scrive"`, `: String` after `message` |
| hover `: String` | `inlayHint/resolve` | the hint + `tooltip` | the card: "What `greet` returns." |
| Ctrl+click `String` | — | — | `· definition in unopened file:///demo/std/string.rs` |
| Ctrl+click `name` | — | — | util.rs tab, `name` selected |
| double-click `: String` | `didChange` (same sync) | diagnostics | `let message: String = …`; type hint gone |
| (300 ms later, shown tab) | `textDocument/inlayHint` | `[param hint]` | one hint |
| Ctrl+I | — | — | both tabs: hints cleared; `· inlay hints off` |
| Ctrl+I | `textDocument/inlayHint` after the wake | as above | hints back |

## What NOT to change

- No library behaviour in scrive-core or scrive-lsp. If the glue needs an API that doesn't exist,
  stop and report it.
- Don't make `update::Change` `#[non_exhaustive]`, and don't add a wildcard arm to `land`.
- Don't change `land`'s other arms, including Phase 6's three inlay arms.
- Don't change `open_lsp`'s or `close_lsp`'s return types.
- Don't derive new traits on `update::Applied` for tests.
- Don't touch `minimal.rs`, `scratch.rs`, `record_showcase.rs`, `shared/` or `index.html`.
- No version bump (0.4.0 is current and unreleased), no CI change, no new dependency.
- The READMEs keep their sections and wording outside the listed edits.
- The existing ignored test's behaviour stays. Only its shutdown tail moves into a shared helper.

## Pitfalls

- **Order in the `Edits` arm.**
  - The revision check runs first, so `remove_inlay` gets an exact offset.
  - `take()` the slot before `remove_inlay`. You can't hold `&self.awaiting` while calling
    `&mut self.doc`, so take first, then use the copied `(key, offset)`.
  - Call `try_edit` last. Its `after_edit` clears `inlay_insert` and schedules the refetch.
- **The loop compares revisions around the whole pass.** `drain_changes` doesn't move the
  revision, but a landed edit does. Don't compare against the snapshot taken inside the pass.
- **Every client call in a pass precedes every landing.** Keep the "collect outputs, then route"
  shape. Routing inside the `if let` chain would land an insert before later requests are made,
  and those would then go out against a stale snapshot.
- **`#[must_use]`.**
  - `update::Applied` is `#[must_use]`, and so are the three methods.
  - In tests, never `let _ = ed.sync_lsp(..)`: read `.messages` or `.jump`.
  - `absorb` and `route` consume their `Applied` by value, so nothing is silently dropped.
- **Borrows in the example's editor arm.** `tab` borrows `tabs`, and its last use is
  `tab.editor.sync_lsp(client)`, so `follow(tabs, …)` compiles afterwards (NLL). The `Task`
  holds no borrow.
- **D14 in tests.** A gesture on a moved set records nothing. Refetch (the `fetch` helpers) after any
  edit before the next gesture, and assert the set is current (`inlays_revision`) when a test
  depends on it.
- **Headless tests have no widget.** A wake never fires by itself: tests fire it from `pending_wake()` (R10). The delay
  isn't simulated, which is fine because `update` matches the generation only.
- **`keyboard::listen` sees only ignored events.** The editor must not bind the toggle chord, and
  the find chords (`find_chord`) must not either. Ctrl+H, for example, is replace, so Ctrl+Alt+H
  would also open replace. Check the real tables before settling on a key.
- **Two `keyboard::listen` subscriptions** (save and toggle in `rust_analyzer`) stay distinct
  because `filter_map` hashes the mapper's `TypeId`. Both chord functions must be non-capturing
  `fn`s.
- **wasm.**
  - `examples/lsp` and its tests build for wasm32, so no `std::time` there; use
    `iced::time::Instant`.
  - `rust_analyzer.rs`'s new code lives inside `mod app`, which is native-only. Its test
    helpers live inside `mod app::tests`.
- **Dead code.** Clippy `--all-targets` builds each example as a binary and as a test harness.
  Test-only helpers (`fetch`, `hints`, `hinted`, `settle_until`, `shut_down`) go inside
  `#[cfg(test)] mod tests`. Every server helper is reached from `answer`.
- **The server's JSON.**
  - Raw-string payloads must not contain `"#`.
  - `serde_json::Value` indexing yields `Null` for a missing key; `IndexMut` on an object inserts.
  - The script ignores the request range on purpose: the client clips to its span (D4).
- **Intra-doc links across the gate.** Only gated code (`code_editor/lsp.rs`) may link to
  `sync_lsp` and the other glue methods. code_editor.rs's ungated docs must use plain code spans.
- **READMEs drift.** Edit `README.md`, then copy it. Never hand-edit both.
- **Never run `cargo fmt`.**

## Resolved questions

- **Q1 — firing the fetch from out-of-crate tests:** `CodeEditor::pending_wake() -> Option<Wake>`
  is public and documented (R10, added by Phase 5); every hint test fires
  `Action::Wake(pending_wake().generation)`.
- **Q2 — `open_lsp`'s wait-0 trigger:** this phase owns it, in lsp.rs (R19, step 1c).
- **Q3 — the toggle chord:** Ctrl+I (`command()`, Cmd+I on macOS) in both examples, toggling
  every tab in the scripted one (R13, the user's call).
- **Q4 — the tooltip card from an example test:** the traffic assertion here plus Phase 5's
  in-crate `set_inlay_tooltip` tests and Phase 6's resolve-then-tooltip client test cover it; no
  extra glue test was asked for.
- **Q5 — `close_lsp` and the pending wait:** `close_lsp` clears it (R19, step 1h).
- **Declined insert:** `Edits(vec![])` under the insert ticket settles the slot without `try_edit`
  (R16, step 1j).

Still open: none.
