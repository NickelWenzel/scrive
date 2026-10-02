# scrive-lsp — a Language Server Protocol bridge for `CodeEditor`

> **Consolidated revision.** It went through six critique rounds, a FOSS comparison (Helix,
> CodeMirror) and two expert passes. Each decision below is stated once, in its final form. The log at
> the end records how each one was reached.

## Context

scrive's `CodeEditor` (crates/scrive-iced) already has an async intel seam: it records requests for a
host to pull, and it ingests results stamped with a revision. Nothing speaks LSP to it yet.

This plan adds a new crate, `scrive-lsp`, behind an `lsp` feature on `scrive-iced`. An iced app's
`update` gets a `Message::Lsp(lsp::Message)` variant carrying a JSON-RPC message, and the editors
adjust themselves. A minimal example drives it with scripted LSP messages.

Interview decisions (2026-09-26):

| Question | Answer |
|---|---|
| What does the bridge own? | **Requests and sync, no I/O.** The app owns the transport. |
| What is "an LSP message"? | **A JSON-RPC envelope.** The bridge matches response ids and decodes payloads with lsp-types. |
| Crate layout | **`scrive-lsp` depends on scrive-core only.** scrive-iced's `lsp` feature pulls it in, re-exports it, and adds the `CodeEditor` glue. |
| Scope | Diagnostics, completion, signature help and hover, **plus** a multi-document client, Ctrl+Space, the didClose/shutdown lifecycle, completion-list reuse, and **goto-definition, rename and formatting**. |
| Editor-side seam fixes | Keep all of them. |
| Conventions | The `/iced` skill. |

## Current state — verified ground truth

Read from source at HEAD `6cf4f2c` (branch `lsp_bridge`) and re-verified by every critique round.
"TODO before dispatch" re-checks the line numbers.

**The async seam (scrive-iced/src/code_editor.rs, 2017 lines).**

*Requests and triggers*
- `CompletionRequest { revision, position }`, `SignatureRequest { revision, position }` and
  `HoverRequest { revision, offset }` sit at :227-269. They are `#[non_exhaustive]`, and each has one
  `Option` slot.
- The async branch drops the trigger (:1391-1407). The trigger set is hard-coded to `( , = : . ␣`
  (:1352-1383).
- Deletions re-request as `Typed(last char)` (:1365-1377).
- There is no manual invoke: Space always types a space (editor.rs:3510).
- Signature help fires only on `(` or while its box shows (:1411-1428).
- Hover fires after a 300 ms dwell (:693-722).
- Retrigger works only with synchronous providers (:1478-1484).

*Staleness and dismissal*
- Staleness is a revision-equality check (`set_*`, :535-579), and revisions start at 0 in every buffer
  (buffer.rs:202).
- Dismissals clear only the slot (:1378-1381, :723-726, :687-690). The widget publishes `HoverDismiss`
  only while a card shows (editor.rs:2210-2226, :2515-2517).
- `ViewportChanged` closes only hover (:629).
- `set_items` overrides an Escape dismissal, and async mode never refilters (completion.rs:97-153).
  Filtering is a label-prefix match (:46-65), with no filter field.

*Accepting a completion*
- `accept_completion` (:1434-1485) applies `replace` verbatim (:1436) and reads the indent before
  editing (:1448). It uses the pre-edit `replace.start` as the snippet base and caret (:1443-1466).
- It inserts through `insert_text`, a selection verb whose `run_edit` places a caret after every patch
  edit (verbs.rs:747-748).
- An unparseable snippet is inserted raw (:1451-1454). The snippet grammar has no nesting and no
  mirrors, and a bare `$VAR` is literal (snippet.rs:7-12, :174-178).
- The hover markdown toggles on every `**` and backtick, and has no escapes (editor.rs:3628-3659).
  Diagnostic hover lines use `format!("**{}:** {msg}")` (:697).

*Commands and navigation*
- There is no goto-definition, rename or format. F2 and F12 are unbound; F8 is next-diagnostic
  (editor.rs:3513). Alt+Shift+F falls through to typing.
- `interpret_key(key, text, mods)` (editor.rs:3440) receives no `physical_key`. It is in scope in the
  `KeyPressed` handler (editor.rs:2307).
- There is no public select or reveal. `set_selection_range` (:1521) and `Document::request_reveal`
  (document.rs:224-227) are private.
- The jump verbs `step_find` and `next_diagnostic` (document.rs:2119-2126, :2151-2156) repeat the
  sequence `set_single` → `reset_transient` → `unfold_to_reveal` → `request_reveal(Center)`.
- `CodeEditor::edit` swallows `TransactionError` (:464-472).

*Find bar*
- The bar floats over the editor (:916-1070).
- Its chords come from the global `find_chord` (:1679-1700), which is subscribed only if
  `find_enabled` (:1138-1153).
- `CloseFind` is handled only if `find_open` (:810).
- PointerDown (:799), `sync_rings`/`resync_focus` (:1220-1237) and `Focused { replace: bool }`
  (:121-126) are find-only.

*Widget and API*
- The widget `State` (editor.rs:456-538) holds per-document state, and there is no `diff` override
  (:1174-1180). The pinned iced hook is `fn diff(&mut self, tree: &mut Tree)`.
- `Action` is a public exhaustive enum (editor.rs:222). Adding variants is a semver break, so this
  ships as 0.4.0.

**The change log (scrive-core/src/document.rs).**
- `observe_changes`/`drain_changes` (:1057-1078): `(true)` doesn't clear the log.
- Only `edit_grouped` logs (:1034-1044), sorted by `Reverse(start)`.
- **Undo/redo are not logged.** `history::replay` (history.rs:377-388) bumps the revision per step,
  and `on_step` sees only the post-step buffer. Typing runs are multi-step undo elements
  (history.rs:207).
- **Tied starts replay in the wrong order.** The rope applies batches in reverse (rope.rs:370-400),
  while `apply`'s ties keep caller order (transaction.rs:184).
- `transaction::apply` is the only revision bump (transaction.rs:231), and an empty batch doesn't bump
  (:179-181). Its only callers are `edit_grouped` (document.rs:960) and `replay` (history.rs:384).
  `apply` stable-sorts by start and rejects overlap (:184-194).

**Core types.**
- `Point.col` is in bytes, and line endings are LF only.
- The line model matches LSP's: a document has at least one line, and a trailing `\n` makes an empty
  last line (buffer.rs:137-141).
- `Snapshot` (buffer.rs:102-160) is an O(1) `Arc` rope clone (sum_tree.rs:94-100). It has
  `text/slice/line/line_count/doc_id/revision`, but no point conversion, chunk access or clipping.
- `slice`/`line` allocate across 128-byte chunks (rope.rs:21, :151-170).
- `Rope::point_to_offset` clamps the row and keeps the column (rope.rs:240-244).
- `TextSummary` has no utf16 or char dimensions (rope.rs:68-72).
- `bracket_tree::enclosing_openers` (:151-153) returns the prefix stack **including unmatched
  openers**; `enclosing_pairs` (:374-386) returns only matched ones. String and comment skipping is
  line-local and requires the grammar's `BracketConfig`.
- `DocId` is stable for a `CodeEditor`, because `load` is an edit. `Document` has no `doc_id()`.

**Repo.**
- Edition 2021; version 0.3.0; internal path deps pin `version = "0.3.0"`; iced is pinned to `10e9b99`.
- `#![deny(missing_docs)]` and `#![forbid(unsafe_code)]`.
- **The code is not rustfmt-clean** (41 files).
- **Baseline:** `cargo test --workspace` passes in about 10 s. **`cargo clippy --workspace
  --all-targets -- -D warnings` fails at HEAD.** Clippy 0.1.98's `manual_slice_fill` lint rejects
  crates/scrive-core/src/highlight.rs:1097. Changing it to `self.ret.win_states.fill(None);` makes the
  workspace clean (verified in a scratch copy). Phase 1 fixes this as its first step.
- CI:
  - `cargo test --workspace` on ubuntu and windows;
  - the wasm32 build of `--workspace --all-targets`;
  - scrive-core lib tests on wasip1;
  - clippy and doc with `-D warnings`.
  - It has **no `--all-features`** runs and no `cargo tree` gate.

**Dependencies.** lsp-types 0.97 is cached and wasm-safe.
- Its `Uri` compares by `as_str()`, with no normalization.
- It has no `itemDefaults`.
- It uses `Params = ()` for shutdown and exit.

`lsp-server` is unsuitable.

## Target state

```
 iced app (host)                     scrive-iced [feature "lsp"]                      scrive-lsp (one Client per server)
 boot ─────────────────────────────► lsp::Client::builder().root(..).build() ───────► (Client, initialize)
 per tab ──────────────────────────► editor.open_lsp(&mut client, uri, lang) ───────► Client::open
 after every editor.update(..) ────► editor.sync_lsp(&mut client) ──────────────────► Client::sync(&Snapshot, Changes) + requests
 Message::Lsp(m) ─────────────────────────────────────────────────────────────────────► Client::receive(m) → Output
   Update::Document(doc) ──► tab with doc.doc_id() ──► editor.apply_lsp(&mut client, doc) → Applied { messages, jump, refused }
   Applied.jump: Jump::Open ──► target tab.editor.jump(&mut client, open);  Jump::Unopened ──► host opens the file
   Update::FileEdits(edits) ──► host applies edits.apply(&disk_text);  Update::Notification(n) ──► host logs
 app-owned transport ◄───────────── Vec<lsp::Message>
```

```rust
enum Message {
    Editor(tab::Id, scrive_iced::Event),
    Lsp(lsp::Message),
}

fn update(&mut self, message: Message, now: Instant) -> Task<Message> {
    let Self { tabs, active, lsp, transport, .. } = self;
    match message {
        Message::Editor(id, event) => {
            let Some(tab) = tabs.iter_mut().find(|t| t.id == id) else { return Task::none() };
            let task = tab.editor.update(event, now).map(Message::Editor.with(id));
            let outgoing = tab.editor.sync_lsp(lsp);
            Task::batch([task, transport.send(outgoing)])
        }
        Message::Lsp(message) => match lsp.receive(message) {
            Ok(output) => {
                let mut outgoing = output.messages;
                for update in output.updates {
                    match update {
                        lsp::Update::Document(doc) => {
                            let Some(tab) = tabs.iter_mut().find(|t| t.editor.document().doc_id() == doc.doc_id())
                            else { continue };
                            let applied = tab.editor.apply_lsp(lsp, doc);
                            outgoing.extend(applied.messages);
                            if let Some(refusal) = applied.refused { log(refusal) }
                            match applied.jump {
                                Some(lsp::update::Jump::Open(open)) => {
                                    if let Some(target) = tabs.iter_mut().find(|t| t.editor.document().doc_id() == open.doc_id()) {
                                        if let Ok(messages) = target.editor.jump(lsp, open) {
                                            outgoing.extend(messages);
                                            *active = target.id;
                                        }
                                    }
                                }
                                Some(lsp::update::Jump::Unopened(unopened)) => { /* read file, new tab, open_lsp, then select(unopened.span(&text)) */ }
                                None => {}
                            }
                        }
                        lsp::Update::FileEdits(edits) => { /* write edits.apply(&disk_text) */ }
                        lsp::Update::Notification(notification) => log(notification),
                    }
                }
                transport.send(outgoing)
            }
            Err(error) => { log(error); Task::none() }
        },
    }
}
```

## Key design decisions

### Architecture

**D1 — Crate layout.**
- `scrive-lsp` depends on scrive-core only.
- scrive-iced gets `lsp = ["dep:scrive-lsp"]` and `#[cfg(feature = "lsp")] pub use scrive_lsp as lsp;`
  (a facade re-export, per the iced skill).
- The glue lives in `code_editor/lsp.rs`.
- Internal crates move to `[workspace.dependencies]` in Phase 4, so the Phase 10 bump touches only
  the root manifest.

**D2 — Our own tolerant JSON-RPC envelope.**
- `lsp::Message` is `Request | Response | Notification`. It is serde, and `Clone + Debug + Send`.
- On input:
  - unknown fields are ignored, and `jsonrpc` is optional;
  - ids may be integers, zero-fraction floats, strings or `null`, so error responses with `id: null`
    parse;
  - when both `result` and `error` are present, the error wins;
  - `params` may be an object, an array or absent.
- On output:
  - request and notification params that serialize to `null` are omitted;
  - a response always carries `"result"`, even when it is `null`.

**D3 — lsp-types 0.97 carries the payloads**, re-exported as `scrive_lsp::lsp_types`.

**D4 — A pure state machine that returns its effects.**
- Every entry point returns `#[must_use] Output { messages, updates }`. `receive` and `open` return
  `Result<Output, Error>`.
- There is no outbox, no I/O, no clock and no threads.
- `receive` takes no snapshot:
  - request results are converted against the snapshot captured when the request was sent, which the
    pending entry stores;
  - anything that lands in *another* document is converted against that document's last synced
    snapshot.
- Document-bound results are `update::Document` bundles.
  - The fields are private: `doc_id`, `stamp`, `change`.
  - Accessors are `doc_id()`, `stamp()` and `change()`, plus `into_parts()`, the documented boundary
    exception for hosts that don't use the glue.
- `update::Stamp` is `Ticket(Ticket)` (completion, signature, hover, definition, format) or
  `Revision(Revision)` (diagnostics, workspace/rename edits).
- `update::Change` is one of:
  - `Diagnostics(Vec<Diagnostic>)`
  - `Completions(Vec<CompletionItem>)`
  - `Signature(Option<SignatureInfo>)`
  - `Hover(Option<HoverInfo>)`
  - `Definition(Option<update::Target>)`
  - `Edits(Vec<EditOp>)`
- `Update` is one of `Document(update::Document)`, `FileEdits(update::FileEdits)` or
  `Notification(message::Notification)`.

**D5 — Many documents per client, one client per editor.**
- There is one `Client` per server. Documents are keyed by `DocId`, and `open` registers each with a
  `uri::Key` and a language id.
- `Error::DuplicateUri` is returned when two documents claim the same URI.
- Before `initialized`, `open` and `sync` only update the stored snapshot. The deferred `didOpen` then
  sends the latest text.
- **Each `CodeEditor` has exactly one `Client`**, because the change log and the request slots each
  have a single consumer.
  - `Client` has a process-unique `client::Id`.
  - `open_lsp` stores that id in the editor and debug-asserts that no other client is registered.
  - Several servers per editor is a follow-up.
- If a server dies or restarts, drop the `Client`, build a new one, and call `open_lsp` on every tab.
  Stale tickets never land.
- `load()` of a different file requires `close_lsp`, then `open_lsp`.
- Phase 1 adds `Document::doc_id()`.

**D6 — URI identity.** `uri::normalize(&Uri) -> uri::Key` is a smart-constructor newtype. It is used
for registration, for lookup and for every URI we send.
- For `file:` URIs it:
  - lowercases the scheme and drive letter;
  - decodes every escape except those that decode to `/`, `%`, `?` or `#` (so `%3A`, `%40` and `%2B`
    match their characters);
  - drops `localhost`;
  - re-encodes with one fixed RFC 3986 set: non-ASCII is percent-encoded, and invalid-UTF-8 escapes
    are kept.
- Other schemes pass through unchanged, and hosts may receive them.
- A round-trip test checks that `Uri::from_str` accepts everything we emit.

**D7 — Position encoding.**
- We advertise utf-8, then utf-16, then utf-32. An absent or unknown encoding means utf-16.
- `Encoding` is public, and `Client::encoding()` returns the negotiated one.
- Clamping rules:
  - a character past the line end → the line end;
  - **a line at or past `line_count`** → the end of the document. This is checked explicitly, because
    the rope's row clamp is wrong for LSP;
  - a position inside a character snaps left;
  - an inverted range (`start > end`) collapses to `end`.
- The conversion core works over an iterator of `&str` chunks, **without allocating**, with an ASCII
  fast path. It serves both `Snapshot` (via Phase 1's `Snapshot::chunks`) and disk text
  (`update::FileEdits`, `Jump::Unopened`). utf-8 needs no walk.

### Sync and versions

**D8 — A per-commit change log (Phase 1).**
- `Document` holds a `ChangeLog { on, from: Option<Revision>, entries }` field. Its
  `record(before, &[EditOp])` is the single owner, and `edit_grouped`, `undo` and `redo` all call it.
- An entry is `document::Change { before: Snapshot, ops }`.
  - `ops` are the forward ops **reversed**, so they are descending and ties replay in the rope's order.
  - `before` is an O(1) clone, captured only while observing.
  - Undo and redo take `buffer.snapshot()` before `history.undo`, and each `on_step` swaps in the next
    one.
- `from` is set to the current revision by `observe_changes(true)` and by every drain.
- **Cap:** above 1024 entries the log clears and sets `from = None` ("broken"). That forces one full
  sync, and bounds the memory an observer that never drains can pin.
- `drain_changes()` returns an opaque `document::Changes { doc_id, from, entries }` with private fields
  and an iterator. `CodeEditor::drain_changes` forwards it, and Phase 1 updates it and its test.

**D9 — Document sync.**
- `didOpen` sends the LF text at the next version. An unchanged revision sends nothing.
- **Incremental** sync is used when the server supports it and `Client::sync(&Snapshot, Changes)`
  passes the chain check:
  - `changes.doc_id() == snapshot.doc_id()`;
  - `changes.from == synced.revision`;
  - each entry's `before.revision()` equals a cursor that advances by one, and the final cursor equals
    `snapshot.revision()`.
- Each entry is converted against its own `before`, and all entries go into one `didChange`. That
  covers undo and redo of typing runs, multi-commit drains, and debounced syncing.
- **Full text** is sent for `Full` servers and whenever the chain check fails or the log is broken.
- **`NONE`, an omitted `change`, or `openClose: false`** send nothing but *still advance the synced
  snapshot*. A bare `TextDocumentSyncKind` means `openClose: true`.

**D10 — Versions.**
- Each `uri::Key` keeps a **high-water mark that survives `close`**, and every `didOpen`/`didChange`
  takes the next number. A reopened document continues the count instead of restarting it.
- We advertise `versionSupport`.
- Diagnostics apply only if their `version`, when present, equals the last synced version. A missing
  version is taken to mean the last synced one (Risk 10).
- `publishDiagnostics` without a version for a URI that isn't open is **cached**: the latest set per
  URI, where an empty set removes the entry. It is converted and applied at `open`, unless a versioned
  publish arrives first.

### Requests, tickets, abandonment

**D11 — Tickets.**
- scrive-core's `Ticket` has private fields, a per-editor monotonic `seq`, and a `revision()`
  accessor. Every request carries one.
- `intel::ticket::Counter` (`new()`, `issue(Revision) -> Ticket`) is the **only** way to create a
  `Ticket`; there is no raw `Ticket::new`. Each `CodeEditor` owns one, and tests outside the editor
  (scrive-lsp's) mint through a per-test `Counter`.
- The editor keeps an `awaiting: Option<(Ticket, …)>` per kind: completion, signature, hover
  (`(ticket, offset, word)`) and definition.
- One private owner, `CodeEditor::accepts(kind, ticket)`, accepts a result only when
  **`ticket == awaited && ticket.revision() == doc.revision()`**.
- `set_completions`, `set_signature`, `set_hover` and `set_definition(ticket, Option<Range<u32>>)` all
  go through it. They are public and not feature-gated. `set_diagnostics` keeps its revision check.
- An accepted `None` or empty result **clears** the signature, hover and definition slots.
- For completion, an empty list closes the popup and the slot retires at the next edit. Other landings
  don't clear the slot, because continuation can deliver twice under one ticket.
- **Abandonment table:**

  | Event | completion | signature | hover | definition |
  |---|---|---|---|---|
  | CaretOrClose (moves, clicks, paste, undo, `edit`) | clear | re-query if showing or awaited | clear | clear |
  | A deletion that empties the word; a boundary `Type` | clear | — | — | — |
  | PopupDismiss, accept (inside `accept_completion`, *before* a retrigger records) | clear | — | — | — |
  | SignatureClose; `Collapse` (dedicated arm, *before* `after_edit`) | — | clear | — | — |
  | HoverDismiss; a `HoverQuery` that records nothing | — | — | clear | — |
  | ViewportChanged | — | — | clear | — |

- A deletion re-requests completion when the popup is open **or** a completion is awaited.
- `drive_signature` queries on `Typed('(') || signature.is_some() || awaiting.signature.is_some()`.
- The widget publishes `HoverDismiss` whenever it cancels a pending or open hover (`hover_queried`).
  `Editor::hover_pending(Option<Range<u32>>)` passes the in-flight word, so a re-arm inside that word
  doesn't cancel it.

**D12 — The client's request lifecycle.**
- **Pending entries** are
  `Pending { id, doc_id, request_snapshot, latest_ticket, latest_caret, query, reissued_for, versions }`.
  `versions: Vec<(uri::Key, Revision)>` records the synced **revision** (not the LSP version) of
  every open document at request time, and is filled only for definition and rename. A revision
  moves in every case a version does, and also for `openClose: false`/`NONE` servers whose tracked
  version stays `None`. It arrives in Phase 8, its first reader.
- **One pending entry per (document, kind).** A new request supersedes the old one with
  `$/cancelRequest`, except for continuations (D14).
- **Silent replies.** None of these is ever an `Error`; each returns `Ok(Output::default())`:
  - a response whose id has no pending entry;
  - `RequestCancelled` (−32800);
  - `ServerCancelled` (−32802);
  - `ContentModified` (−32801). One exception: if the entry's `latest_ticket.revision` is still the
    synced revision, the request is re-issued, at most once per ticket (`reissued_for`, which the new
    entry inherits).
- **Where entries are dropped.** A pending entry whose latest ticket has fallen behind the synced
  revision is dropped **in `receive`**, never in `sync`. That keeps continuations alive, since
  `sync_lsp` syncs before it dispatches.
- **Stale requests.** Every `Client::<kind>` declines a request whose revision differs from its
  snapshot or from the synced revision.
- **Local declines answer with the request's ticket**, so every awaited slot settles:
  - `Completions(vec![])` when the client isn't ready, the server has no provider, or the trigger char
    isn't one the server registered. Matching uses `ends_with`, so multi-character triggers work.
  - `Signature(None)`, `Hover(None)` and `Definition(None)` when the client isn't ready or the server
    has no provider.
- **Server errors.** On a user command (definition, rename, format), any error other than the
  cancellations above returns `Err(Error::Server { doc_id, method, error })`. A failed signature
  request becomes `Signature(None)`.

### Completion

**D13 — Completion items (Phases 2 and 6).**
- `CompletionItem` gains, all through builders: `filter: Option<String>`, `additional: Vec<EditOp>`,
  `signature_after: bool`, and `matches(word)`, the one public filtering predicate that `refilter`
  uses.
- **Conversion rules:**
  - `replace` is `None` when the edit range equals the request word, and `Some` otherwise.
  - An `InsertReplaceEdit` decodes as its `insert` range. An edit range that spans lines or doesn't
    contain the request position falls back to the word.
  - `filterText` → `filter`, and `sortText` → `sort_key`.
  - Commands: `triggerSuggest` → `retrigger`, and `triggerParameterHints` → `signature_after`.
  - `additionalTextEdits` → `additional`.
  - Snippets are lowered before parsing: variables become their default or nothing, mirrors become
    plain text after the first, nesting is flattened, and choices become their first option. Anything
    left unparseable falls back to plain text.
- **Advertised:** snippets, plaintext documentation and `contextSupport`. **Not advertised:**
  insertReplace, itemDefaults and labelDetails.
- **Accept (Phase 2):**
  - The main op and the `additional` edits go into one `edit_grouped` batch, sorted by `(start, end)`.
  - Additional edits that touch the closed interval `[replace.start, replace.end]` are dropped. Those
    at or past the popup anchor shift by the live caret delta.
  - The snippet base and the plain-insert caret come from the main edit's patch `new.start`. The
    indent is still read before the edit. Accept does not go through `insert_text`.
  - A `Some` range whose end lies inside the live word extends to the caret.
  - `signature_after` runs `drive_signature` as if `(` had been typed.
- **Conversion cost (Phase 6):**
  - Item ranges lie on the request line. That line is materialized once per reply, and conversions are
    memoized per `Position`.
  - Items are decoded one at a time from a `Vec<Value>`; items that fail are skipped.
  - Snippet lowering and `to_plain` run only for items that pass `matches`.

**D14 — Completion sessions: reuse and continuation (Phase 6).**
- `CompletionRequest` has private fields and is built with `new(ticket, word, trigger, start)`.
  - `start: intel::completion::Start` is `Fresh` or `Continuing`. It is sampled before the refilter,
    and it is `Continuing` only if the popup is open or a completion is awaited.
  - There is no position field: `word.end` is the caret, and the client derives the position from it.
- A **session** holds `{ word_start, caret, len, prefix, items, incomplete }`, with ranges in request
  coordinates.
- A request **continues** the session only if all of the following hold:
  - it is `Typed` + `Continuing`;
  - the word start is the same;
  - `caret >= session.caret`;
  - the text still begins with `prefix`;
  - `snapshot.len() − session.len == caret − session.caret`, which rules out forward deletes and
    secondary carets;
  - the caret is within 32 bytes of the request caret. Beyond that, the request supersedes.
- **Reuse** answers a complete list locally.
  - It pre-filters with `matches` before cloning.
  - It shifts ranges by the delta while answering: `end += delta`, and `start` only if
    `start > old caret`.
  - An incomplete list re-requests instead, with `TriggerForIncompleteCompletions`.
- **Continuation.** When the session's own request is still in flight, the new request updates that
  entry's `latest_ticket` and `latest_caret` instead of superseding it.
  - The reply is converted against `request_snapshot`, stored, and answered through reuse under
    `latest_ticket`.
  - An incomplete reply whose caret has moved also re-requests.

### Signature help, hover

**D15 — Signature help (Phases 3 and 7).**
- `SignatureRequest { ticket, position, call: Option<u32> }` has private fields and `new`.
- `call` is the offset of the innermost `(` enclosing the caret. It comes from a new
  `Brackets::innermost_open(offset, b'(')`, implemented as
  `enclosing_openers(offset).iter().rev().find(|e| e.ch == b'(')`.
  - Unmatched openers count.
  - It is exactly as string- and comment-aware as bracket colouring, which is line-local.
- **Continuation:**
  - A request with the same `call` updates the in-flight entry instead of superseding it, and the reply
    is stamped with `latest_ticket`.
  - If `latest_caret` moved, the reply is delivered and the request re-issued at the synced snapshot.
    This terminates when typing stops.
  - The client never scans text.
- **Conversion:**
  - Empty `signatures` → `None`.
  - The per-signature `activeParameter` wins, clamped per `SignatureInfo`.
  - `Simple` labels are searched after the previous parameter, starting after the first `(`.
  - `LabelOffsets` are **UTF-16**, clamped and snapped.
- **Advertised:** `documentationFormat`, `labelOffsetSupport`, `activeParameterSupport`.

**D16 — Hover (Phases 3 and 7).**
- **The markdown grammar has one owner.**
  - Phase 3 makes the widget markdown safe: escapes for `\*`, `` \` `` and `\\`, and no bold inside
    code.
  - `scrive_core::intel::hover::escape_markdown` owns the grammar, documented on
    `HoverInfo::markdown`. The parser, `hover_card` and `to_hover` all use it.
- **The hover card.** `hover_card(offset, docs)` merges escaped diagnostics and never drops them on
  `None`. `set_hover` takes its offset from the awaited slot.
- **Conversion (Phase 7):**
  - `MarkupKind::PlainText` is escaped.
  - Markdown goes through `to_hover`: fences → code lines, `---` stripped, links → their text.
  - A `MarkedString` array is joined with blank lines, and a `LanguageString` becomes code lines.
  - The range is the server's if it contains the offset, and the word otherwise.
- **Advertised:** `contentFormat: [markdown, plaintext]`.

### Commands, edits, navigation

**D17 — Editor commands (Phases 2 and 3).**
- **Bindings.** These are bound on the focused widget, and each has a dedicated `update` arm:
  - Ctrl+Space → `Action::TriggerCompletion`;
  - F12 → `GotoDefinition`;
  - F2 → `Rename`;
  - Shift+Alt+F → `Format`, matched on the physical `KeyF` in the `KeyPressed` handler before
    `interpret_key`.
- **New requests.** Plain data, one module each, all with `new`:
  - `DefinitionRequest { ticket, offset }`;
  - `RenameRequest { ticket, offset, new_name }`;
  - `FormatRequest { ticket, tab_size }`. The client always sends `insertSpaces: true`, because scrive
    indents with spaces.
- **The rename field.**
  - It is opt-in through `CodeEditor::rename(bool)`. Builder toggles are exempt from the no-bool rule,
    and `open_lsp` doesn't touch this setting.
  - It shares the find bar's plumbing:
    - Escape through the find chord closes rename if it is open, and find otherwise; only one bar is
      open at a time;
    - the subscription runs when `find_enabled || rename.is_some()`;
    - PointerDown and the focus helpers cover `RENAME_INPUT`;
    - `Focused { field, on }` replaces `Focused { replace, on }`.
  - It closes when the revision moves.
- **Selecting a range.** `CodeEditor::select(range)` calls the new core verb
  `Document::select_and_reveal` and then `after_edit(CaretOrClose)`. The verb clamps, snaps, and runs
  the jump sequence. `step_find` and `next_diagnostic` share its private helper.
- **Widget `diff`.** When the rendered `DocId` changes, `State` is rebuilt from its default, keeping
  focus, metrics, font and modifiers. The widget then autoscrolls, centered if the document ever
  revealed.

**D18 — Results (Phase 8).**
- **Definition.**
  - The first `Location` wins; for a `LocationLink`, its `targetSelectionRange` is used.
  - The result goes to the requester as a ticketed `Change::Definition(Option<Target>)`.
  - `Target` is one of:
    - `Local(span)`;
    - `Open(jump::Open)`: another open document, with private fields, converted against its synced
      snapshot. It is dropped (→ `Definition(None)`) if that document's synced revision moved since
      the request, or it was opened after it (`Pending::versions`).
    - `Unopened(jump::Unopened)`: a `uri::Key`, an LSP range and the negotiated `Encoding`, with
      `span(&str)`.
  - `apply_lsp` handles `Local` through `set_definition`. It returns `Open`/`Unopened` as
    `Applied.jump: Option<update::Jump>`, and only if the ticket was accepted. `Jump` has no `Local`
    case.
  - `CodeEditor::jump(client, open) -> Result<Vec<Message>, update::Refusal>` checks the `DocId` and
    the revision, then selects and syncs.
- **Format.**
  - The result is a ticketed `Change::Edits`, applied only if `ticket.revision()` is still current.
  - **Edit hygiene:**
    - `\r\n` and `\r` in `newText` become `\n`;
    - edits are trimmed on char boundaries;
    - a (nearly) whole-document edit becomes a **line diff**: trim the common lines, then run Myers on
      the middle with `max_d` around 1000, falling back to the trimmed edit;
    - the batch is sorted by `(start, end)`.
- **Rename.**
  - Accepted shapes:
    - `documentChanges`, as plain edits or as operations that are all edits;
    - the `changes` map, sorted by URI;
    - several edits per URI;
    - `AnnotatedTextEdit`;
    - `version: null`.
  - Create, rename and delete operations reject the whole edit (`Error::Unsupported`).
  - **The requester first:** a rename whose *requesting* document moved is dropped silently by
    D12's drop in `receive` (`Ok(Output::default())`). The editor never applies a stale rename; the
    user can press F2 again.
  - **All-or-nothing** for the *other* touched documents: the rename fails with `Error::StaleEdit`
    if any touched document's synced revision moved since the request, it has no recorded revision
    (opened after the request), it was open at the request and has closed since, or a server-named
    non-null `TextDocumentEdit.version` differs from its tracked LSP version.
  - Open documents get a revision-stamped `Change::Edits`.
  - Documents that aren't open get `Update::FileEdits(update::FileEdits)`: a `uri::Key`, the edits
    and the `Encoding`, with a CRLF-aware `apply(&str) -> String`. `apply` cannot refuse: of two
    overlapping edits, the first by `(start, end)` wins and the other is skipped (documented on the
    method; the spec forbids overlaps anyway).
- **Advertised:**
  - `definition.linkSupport`;
  - `rename`, without prepare;
  - `formatting`;
  - `workspaceEdit { documentChanges: true, failureHandling: "transactional" }`, with no resource
    operations.
- **Decoding:** Locations and edit entries whose URI fails to parse are skipped.

**D19 — Glue (Phase 9).**
- **`open_lsp(client, uri, lang) -> Result<Vec<Message>, Error>`**
  - It registers the document first.
  - Only on success does it reset the log (off, then on) and store the `client::Id`. The reset
    doesn't move the revision, so the log's new `from` equals the registered snapshot's revision.
  - It routes its own updates, i.e. the cached diagnostics.
- **`sync_lsp(client) -> Vec<Message>`**
  - It drains `Changes`, syncs, and dispatches every pending request kind: completion, signature,
    hover, definition, rename and format.
  - It applies its own local answers through `apply_lsp`'s checks.
- **`apply_lsp(client, doc) -> update::Applied { messages, jump, refused }`**
  - It refuses a document with a foreign `DocId`.
  - It checks the stamp:
    - a ticket on completion, signature, hover or definition goes through `accepts`;
    - a ticket on edits checks `revision()`;
    - a revision stamp checks the revision.
  - It applies the change. Edits go through a **non-swallowing `CodeEditor::try_edit`**, so a
    rejected batch becomes `refused: Some(Refusal::Overlap)`. Any `TransactionError`, including
    `WouldOverflow`, reports as `Overlap`, and `Overlap`'s doc says so.
  - It **always** finishes with the `sync_lsp` path. That is a no-op when nothing moved, and it can't
    recurse: `sync_lsp`'s local answers (declines such as `Definition(None)`, reused completion
    lists) land through a private non-syncing path (`land`), never through `apply_lsp`.
  - `update::Refusal` is `Foreign`, `Stale` or `Overlap`.
- **`jump(client, open)`**, as described in D18.
- **`close_lsp(client)`**
  - It turns off observing, closes an open rename field and drops an unsent rename request. It
    keeps the `rename(bool)` opt-in, so a server restart (close, then `open_lsp`) keeps F2.
  - It clears the awaiting slots, the unsent requests and the editor's diagnostics.
  - It calls `Client::close`, which drops the session and cancels the pending requests.
- All of these are `#[must_use]`.

**D20 — Lifecycle and initialize (Phase 5).**
- **Builder.** `Client::builder()` takes `.root(Uri)`, `.initialization_options(Value)`,
  `.configuration(Value)` and `.process_id(u32)`, and `.build()` returns `(Client, initialize)`. All of
  them are `#[must_use]`.
- **Initialize params:** `processId`, `rootUri`, `rootPath` (under a justified
  `#[allow(deprecated)]`, because pyright reads it), `workspaceFolders`, `clientInfo` and
  `initializationOptions`.
- **Advertised:** `workspace.configuration`, `workspace.workspaceFolders`,
  `general.positionEncodings`, synchronization, and `publishDiagnostics.versionSupport`.
- **After `initialized`**, the client sends `workspace/didChangeConfiguration` if a configuration is
  set.
- **A failed `initialize`** returns `Err(Error::Server)`, and the deferred opens are dropped.
- **Closing and shutdown:**
  - `close(doc_id)` sends `didClose`.
  - `shutdown()` sends only `shutdown`. `exit` goes out when its response arrives, including an error
    response.
  - Before initialization completes, `shutdown()` moves straight to `Exited` and sends nothing.
- **After `shutdown()`**, server requests get a `null` success, and every client call declines.

**D21 — Server→client requests** are always answered inside `Ok`:

| Request | Answer |
|---|---|
| `workspace/configuration` | per item, the dotted section of the configured value (an empty section returns the whole value, a missing one returns `null`) |
| `workspace/workspaceFolders` | the root folder |
| `client/(un)registerCapability`, `window/workDoneProgress/create`, `window/showMessageRequest`, `workspace/*/refresh` | `null` |
| `workspace/applyEdit` | `applied: false` |
| params that don't decode | `InvalidParams` |
| anything else | `MethodNotFound` |

- Unhandled notifications become `Update::Notification`.
- `Err` is only for cases where there is nothing to send:
  - notification or response payloads that don't decode;
  - `StaleEdit` and `Unsupported`;
  - server errors on commands;
  - `DuplicateUri`.

## Constraints

- **scrive-lsp is headless and does no I/O.** It builds for wasm32, and its lib tests run on wasip1
  in CI.
- **Dependency budget.**
  - scrive-lsp: scrive-core, lsp-types 0.97, serde (derive), serde_json and thiserror. Each carries a
    justification comment.
  - scrive-iced: scrive-lsp (optional), and serde_json as a dev-dependency from Phase 9.
- **Edition 2021.** The version becomes **0.4.0** in Phase 10.
- `#![deny(missing_docs)]` and `#![forbid(unsafe_code)]`. Comments explain *why*, in the present
  tense, and never narrate the plan.
- **Never run `cargo fmt`.** Only files you *create* may be run through `rustfmt --edition 2021`.
- RUST_STYLE.md and OPAQUE.md apply:
  - no `mod.rs`;
  - no aliased imports, except D1's facade;
  - no `unwrap()` in library code;
  - errors use `thiserror`;
  - new scrive-lsp types use module-path names;
  - no raw `bool` flag parameters (builder toggles are exempt).
- **Types that carry invariants have private fields and accessors:** `document::Changes`, `Ticket`,
  `CompletionRequest`, `SignatureRequest`, `update::Document`, `jump::Open` and `uri::Key`.
- New scrive-core request types keep the established `…Request` names, one module per service.
- The iced skill governs the example, the glue and the rename field:
  - function helpers;
  - `.map(Message::Variant)`;
  - id-tagged envelopes;
  - a boot function that returns `(State, Task)`;
  - `iced::time::Instant`.
- **Tests:**
  - they are colocated;
  - their names read as sentences;
  - each has a `///` doc stating its invariant, and string assert messages;
  - a test that must survive later phases reaches single-variant enums only through one helper.
- **Enum variants arrive in the phase that first produces them.** The one exception is
  `update::Stamp::Ticket`, which comes in Phase 5; that is why Phase 5 needs Phase 2.
- Commits use Conventional Commits with **no AI attribution**. Phase agents don't commit.

## Phases

Each phase compiles, lints clean with `-D warnings`, and tests green on its own. **The phases run
sequentially.**

### Phase 1 — scrive-core: a per-commit change log, snapshot primitives, one jump verb
*Doc: MAP_PHASE_1.md*
- **Step 0:** fix the baseline clippy failure (highlight.rs:1097 → `.fill(None)`) in its own commit,
  `fix(core): satisfy clippy 1.98 manual_slice_fill`.
- `Snapshot`:
  - `offset_to_point` and `point_to_offset`, with docs noting the row clamp;
  - `chunks(range)`;
  - `clip_offset`.
- `Document`:
  - `doc_id()`;
  - `select_and_reveal(range)`, with a private helper shared by `step_find` and `next_diagnostic`.
- Change log (D8):
  - `ChangeLog`, `document::Change` and `document::Changes`;
  - reversed ops and `before` snapshots;
  - logging for undo and redo;
  - the cap.
- `CodeEditor::drain_changes` and its test are updated.
- **Exit:**
  - A replay property holds per entry, across random batches with ties, single- and multi-step
    undo/redo, and drains after one or many commits: replaying each entry's ops onto `before.text()`
    yields the next text.
  - Hitting the cap breaks the log.
  - A reveal into a fold unfolds it and bumps `reveal_seq`.
  - The existing tests stay green.

### Phase 2 — the async seam: tickets, abandonment, parity, completion items, Ctrl+Space
*Doc: MAP_PHASE_2.md*
- **Request types move to scrive-core.**
  - `intel/ticket.rs` defines `Ticket` and `ticket::Counter`, its only minter (D11).
  - `CompletionRequest` (private fields, `new(ticket, word, trigger, start)`) and
    `intel::completion::Start`.
  - `SignatureRequest { ticket, position }`, which gains `call` in Phase 3.
  - `HoverRequest { ticket, offset, word }`.
  - All have `new`, scrive-iced re-exports them, and doc links resolve inside scrive-core.
- **Awaiting slots and abandonment.**
  - The awaiting slots and `accepts`, following the D11 table.
  - `hover_queried` and `Editor::hover_pending`.
  - `set_*` take a `Ticket`.
- **Parity with sync mode.**
  - Dismissed + `Typed` → no request.
  - Open + `Typed` → `refilter`, then record.
  - A trigger or manual invoke clears the dismissal, and `set_items` respects it.
- **`CompletionItem` additions.**
  - `filter`, `additional`, `signature_after` and `matches`.
  - Accept per D13: the explicit batch, patch mapping, and `signature_after`.
- **Retrigger and Ctrl+Space.** Retrigger uses `request_completions(Manual)`. Ctrl+Space maps to
  `Action::TriggerCompletion`.
- **Stale docs** fixed at intel.rs:3,7-10, providers.rs:49-53, hover.rs:2-6 and signature.rs:6-9.
- **Exit** — tests through real event paths:
  - `Type('.')` → `PlaceCaret` → the stale ticket is dropped.
  - Hover with no card: moving away emits `HoverDismiss`, and a re-arm inside the word keeps the
    request.
  - Two Ctrl+Space presses at one revision.
  - `f` ⌫ `g` samples `Fresh`.
  - A deletion while awaited re-requests.
  - Signature: a click-away is dropped, typing `foo(a` fast still opens the box, and `Collapse`
    doesn't re-query.
  - Accepting a retrigger item opens the popup when the reply lands.
  - An auto-import above a snippet leaves the tab stops on the placeholders.
  - A revision-only stale drop.
  - The existing tests pass on tickets.

### Phase 3 — editor commands, call identity, widget hardening, safe hover
*Doc: MAP_PHASE_3.md*
- **Command requests.**
  - `intel/definition.rs`, `intel/rename.rs` and `intel/format.rs` (D17).
  - `take_definition_request`, `take_rename_request` and `take_format_request`.
  - `set_definition(ticket, Option<Range<u32>>)`.
- **Call identity.** `Brackets::innermost_open` and `SignatureRequest.call`.
- **Keys and the rename field.**
  - Actions and bindings, including the physical `KeyF` match.
  - The rename field and `CodeEditor::rename(bool)`.
  - `CodeEditor::select`.
- **Widget hardening.** The `diff` reset.
- **Safe hover.** `escape_markdown` in `intel/hover.rs`, a safe widget parser, and `hover_card`.
- **Exit:**
  - The command state machines work.
  - Rename works through `find_chord(Escape)`, including precedence over find.
  - `innermost_open` works with no closing paren and with parens inside a string on the same line.
  - Swapping documents resets the widget and reveals.
  - The markdown escape tables pass.
  - The hover card keeps its diagnostics on `None`.

### Phase 4 — scrive-lsp: crate, envelope, encoding
*Doc: MAP_PHASE_4.md*
- **The crate.**
  - A new workspace member.
  - `[workspace.dependencies]` for the internal crates.
  - The manifest budget, a README and `lib.rs`.
- **Modules.**
  - `message.rs` (D2).
  - `encoding.rs` (D7): public, chunk-based and allocation-free.
- **CI.**
  - For each headless crate: `out=$(cargo tree -p X -e normal --prefix none)`, then
    `if grep -qE '^(iced|winit|wgpu)' <<<"$out"; then exit 1; fi`.
  - scrive-lsp lib tests on wasip1.
- **Exit:**
  - The wasm build passes.
  - The envelope tolerance tables pass.
  - The encoding tables pass, covering `"aé€😀b"`, a mid-surrogate position, `"" (1,0)→0`,
    `"\n\n" (3,0)→2`, `"a\nbc" (5,0)→4`, `(u32::MAX, u32::MAX)` and an inverted range.

### Phase 5 — Client core: multi-document sync, diagnostics, lifecycle
*Doc: MAP_PHASE_5.md*
- **`client.rs`:**
  - `Client` with `builder`, `encoding`, `open`, `close`, `sync`, `receive` and `shutdown`;
  - `client::{Builder, Id}`;
  - `Output` and `Error`.
- **`update.rs`:**
  - `Update::{Document, Notification}` and `update::Document`;
  - `Stamp` with both variants;
  - `Change::Diagnostics`.
- **Supporting modules:**
  - `uri.rs`: `Key`, `normalize`, and the round-trip check;
  - `client/capabilities.rs`;
  - `diagnostics.rs`, with `severity: None` → `Error`.
- **Behavior:** D9 sync with the chain check, D10 versions and the diagnostics cache, the D20
  lifecycle, and the D21 server requests.
- **Exit** — conversation tests for:
  - the handshake, including a deferred open followed by sync;
  - incremental UTF-16 sync;
  - **incremental undo of a typing run**;
  - a multi-commit drain sent as one `didChange`;
  - a broken chain or foreign `DocId` falling back to full sync;
  - NONE still advancing the synced snapshot;
  - the diagnostics gate and cache, including the version high-water mark across a reopen;
  - URIs: `c%3A`, `%40`, `localhost`, and the round-trip;
  - `close` and `shutdown`, including requests after shutdown;
  - the `configuration` and `workspaceFolders` answers, and notification passthrough.

### Phase 6 — Completion, sessions, the pending machinery
*Doc: MAP_PHASE_6.md*
- **Pending machinery (D12).** Supersede, cancel, and the ContentModified re-issue. `close` cancels
  pending requests.
- **`Client::complete`,** with the D12 declines and the D14 reuse and continuation.
- **New modules and variants.** `completion.rs`, `snippet.rs`, `markdown::to_plain` and
  `Change::Completions`.
- **Exit:**
  - Conversion tables pass. They include:
    - clangd's `•printf(…)`;
    - `additionalTextEdits` and `triggerParameterHints`;
    - a bad item being skipped;
    - `InsertReplaceEdit`.
  - Snippet cases pass.
  - Conversations cover:
    - supersede, cancel and server cancel;
    - one ContentModified re-issue;
    - decline;
    - reuse with the range shift, and incomplete lists;
    - continuation;
    - a forward delete ending the session;
    - the 32-byte cap, caret-left, and the prefix check;
    - the drop in `receive`.

### Phase 7 — Signature help and hover
*Doc: MAP_PHASE_7.md*
- `Client::signature_help`, with call continuation and a re-issue when the caret moved.
- `Client::hover`.
- `signature.rs`, `hover.rs`, `markdown::to_hover` and `Change::{Signature, Hover}`.
- **Exit:**
  - Conversion tables pass. They cover:
    - empty signatures;
    - the per-signature active parameter;
    - `Simple` labels with repeats;
    - UTF-16 offsets;
    - plaintext escaping;
    - the hover content shapes.
  - The continuation conversations pass.

### Phase 8 — Definition, rename, formatting
*Doc: MAP_PHASE_8.md*
- `Client::definition`, `Client::rename` and `Client::format`.
- `edits.rs` (edit hygiene and the capped Myers diff) and `workspace.rs`.
- `Change::{Edits, Definition}`, `update::{Target, jump::Open, jump::Unopened, FileEdits}`, and
  `Error::{StaleEdit, Unsupported}`.
- **Exit:**
  - **Definition:** local; another open document, dropped when that document has moved; an unopened
    document; `LocationLink`.
  - **Rename:** two documents via `documentChanges` and via `changes`; rejections for stale documents,
    for documents opened after the request, and for resource operations.
  - **Format:** `é`→`è`, a descending batch, CRLF, the whole-document diff, the Myers cap, and a stale
    format.

### Phase 9 — scrive-iced `lsp` feature and the CodeEditor glue
*Doc: MAP_PHASE_9.md*
- The feature and the re-export.
- `code_editor/lsp.rs` per D19, including `try_edit`, `jump`, `update::{Applied, Jump, Refusal}` in
  scrive-lsp, and storage for the `client::Id`.
- The serde_json dev-dependency.
- CI runs clippy with default and all features, and runs test, doc and the wasm build with all
  features.
- **Exit** — glue tests cover:
  - every change kind;
  - two editors on one client;
  - a background rename emitting its `didChange`, then a second rename landing in both;
  - a stale log dropped at `open_lsp`;
  - a failed `open_lsp` leaving the editor untouched;
  - foreign documents refused;
  - `Type`, `Type`, reply → continuation;
  - F12 → click → a late jump dropped, including across documents;
  - local answers from `sync_lsp`;
  - `Refusal::Overlap`;
  - `close_lsp` clearing diagnostics.

### Phase 10 — The example, docs, release bump
*Doc: MAP_PHASE_10.md*
- `crates/scrive-iced/examples/lsp/{main.rs, server.rs}`, with `required-features = ["lsp"]` and
  `test = true`:
  - two id-tagged tabs using `.rename(true)`;
  - a scripted server: canned JSON for initialize, completion, signature and hover, and computed
    diagnostics, definition, rename and format;
  - a traffic panel;
  - headless tests.
- The READMEs.
- The bump to 0.4.0.
- **Exit:** `cargo test -p scrive-iced --features lsp --example lsp` passes, and the wasm build and
  docs are green.

## Execution order

The phases run **strictly in sequence:** `1 → 2 → 3 → 4 → 5 → 6 → 7 → 8 → 9 → 10`. Each depends on the
one before it. The notable cross-phase dependencies:
- Phase 3 needs Phase 1 (`select_and_reveal`, `doc_id`) and Phase 2 (tickets).
- Phase 4 needs Phase 1 (`Snapshot::chunks`/`clip_offset`).
- Phase 5 needs Phase 1 (`Changes`) and Phase 2 (`Ticket`).
- Phase 6 needs Phase 2 (`CompletionRequest`, `matches`).
- Phases 7 and 8 need Phase 3: `call` and `escape_markdown` for 7, the command requests for 8.

## Testing strategy

- **Contract tests live with the fact they own.**
  - Phase 1: log replay and reveal.
  - Phase 2: abandonment, parity and accept, all driven through real widget event paths.
  - Phase 3: the commands, rename through `find_chord`, `innermost_open`, the widget reset, and
    markdown.
- **scrive-lsp is tested as data in, data out.** JSON fixtures, plus real `Document` snapshots and
  `Changes`. Multi-document tests use two documents.
- **The glue tests** are feature-gated and drive real key paths.
- **The example tests itself headlessly:**
  - diagnostics land in both documents;
  - F12 switches tabs and selects;
  - rename changes both documents, and the server sees both `didChange`s;
  - format strips whitespace.
- **wasm:** CI does an all-features build and runs the scrive-lsp lib tests on wasip1 (which needs
  wasmtime, so it runs only in CI).

## Risks

1. URIs that a server rewrites in ways D6 doesn't normalize look foreign.
2. Signature label offsets are read as UTF-16, following the spec text and rust-analyzer. A
   non-conforming server gets clamped offsets.
3. Completion filters by case-insensitive prefix (on `filter` or the label), while servers match
   fuzzily.
4. Hard-coded triggers (`( , = : . ␣`): server triggers such as `' < " / @` never fire. Because `-` is
   a word char, `None` replaces a whole `a-b`, and the filter hides labels after a `-`. Follow-up:
   `open_lsp` installs the server's triggers.
5. Full sync is O(document). It happens only for `Full` servers and broken chains. With a `Full`-only
   server every keystroke sends the whole text; syncing less often delays requests.
6. fluent-uri is strict, so hosts must percent-encode paths.
7. A rejected edit is reported as a `Refusal`, but none of it is applied. There is no partial rename.
8. Global chords (Ctrl+F) reach every subscribed editor. The example subscribes only the active tab.
9. After `FileEdits`, nothing tells the server (`didChangeWatchedFiles` is out of scope). The host
   must reopen or re-save.
10. A publish without a version, computed for older text, is applied at the current revision. The
    diagnostics mover limits the damage to drift until the next publish.
11. `preselect`, and auto-retrigger after inserting a trigger char (`crate::`), are follow-ups.
12. utf-16/32 conversion is O(column) on long lines, though allocation-free (D7). Follow-up: rope
    summary dimensions.
13. One `Client` per `CodeEditor` (D5). Several servers per editor is a follow-up.
14. `innermost_open` is only line-locally aware of strings and comments, so a `(` inside a multi-line
    string or block comment can mis-identify the call. The re-issue on caret movement corrects it on
    the next reply.

## Out of scope

- The transport.
- Other LSP features:
  - references, code actions, semantic tokens, inlay hints, symbols, folding, code lens, highlights;
  - pull diagnostics;
  - `resolve`, commit characters, `prepareRename`;
  - range and on-type formatting, and format-on-save;
  - a multi-location peek;
  - resource operations, and annotations beyond unwrapping them;
  - server `applyEdit`;
  - dynamic registration;
  - save notifications and `didChangeWatchedFiles`.
- Configurable triggers.
- Several servers per editor.
- Richer popup rendering.
- A UI for progress or messages.
- Unrelated stale docs.

## Files touched

| Phase | Files |
|---|---|
| 1 | crates/scrive-core/src/{highlight.rs, buffer.rs, document.rs}, crates/scrive-iced/src/code_editor.rs |
| 2 | crates/scrive-core/src/{intel.rs, intel/ticket.rs, intel/providers.rs, intel/signature.rs, intel/hover.rs, intel/completion.rs, lib.rs}, crates/scrive-iced/src/{code_editor.rs, editor.rs} |
| 3 | crates/scrive-core/src/{bracket.rs, bracket_tree.rs, intel.rs, intel/definition.rs, intel/rename.rs, intel/format.rs, intel/signature.rs, intel/hover.rs, lib.rs}, crates/scrive-iced/src/{code_editor.rs, editor.rs} |
| 4 | Cargo.toml, Cargo.lock, crates/scrive-iced/Cargo.toml, crates/scrive-lsp/{Cargo.toml, README.md, src/lib.rs, src/message.rs, src/encoding.rs}, .github/workflows/ci.yml |
| 5 | crates/scrive-lsp/{README.md, src/lib.rs, src/client.rs, src/client/capabilities.rs, src/client/tests.rs, src/update.rs, src/uri.rs, src/diagnostics.rs} |
| 6 | crates/scrive-lsp/src/{lib.rs, client.rs, client/capabilities.rs, client/tests.rs, update.rs, completion.rs, snippet.rs, markdown.rs} |
| 7 | crates/scrive-lsp/src/{lib.rs, client.rs, client/capabilities.rs, client/tests.rs, update.rs, signature.rs, hover.rs, markdown.rs} |
| 8 | crates/scrive-lsp/src/{lib.rs, client.rs, client/capabilities.rs, client/tests.rs, update.rs, edits.rs, workspace.rs} |
| 9 | crates/scrive-iced/{Cargo.toml, src/lib.rs, src/code_editor.rs, src/code_editor/lsp.rs}, crates/scrive-lsp/src/update.rs, .github/workflows/ci.yml, Cargo.lock |
| 10 | crates/scrive-iced/{Cargo.toml, examples/lsp/main.rs, examples/lsp/server.rs}, Cargo.toml, README.md, crates/scrive-iced/README.md, crates/scrive-lsp/README.md, Cargo.lock |

## Verification

```
cargo test --workspace
cargo test --workspace --all-features
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --all-features
cargo build --workspace --all-targets --all-features --target wasm32-unknown-unknown
for c in scrive-core scrive-lsp; do out=$(cargo tree -p $c -e normal --prefix none) || exit 1; if grep -qE '^(iced|winit|wgpu)' <<<"$out"; then echo "$c LEAK"; exit 1; fi; done
```

## FOSS comparison

We compared against **Helix** (`079a789e8c`), **CodeMirror `@codemirror/lsp-client`** 6.2.2
(`97bb453`), and vscode-ws-jsonrpc.

**Where the plan was already stronger:**
- negotiated encodings with clamping; CodeMirror assumes UTF-16 and doesn't clamp;
- URI normalization; CodeMirror compares raw strings;
- server-request defaults; CodeMirror answers `MethodNotFound`;
- shutdown and exit; CodeMirror sends neither;
- `$/cancelRequest` supersession; Helix drops requests silently;
- `isIncomplete`, `filterText` and per-item ranges, all of which CodeMirror lacks;
- snippet lowering; Helix inserts nothing on failure;
- `LocationLink`, which breaks CodeMirror;
- all-or-nothing handling of every WorkspaceEdit shape;
- fine-grained ticket staleness.

**Adopted:**
- envelope tolerance;
- collapsing inverted ranges;
- full `file:` URI decoding;
- a version high-water mark across reopen;
- answering server requests after shutdown;
- `configuration` and `workspaceFolders`;
- notification passthrough;
- capability flags;
- signature details, including UTF-16 label offsets and continuation (CodeMirror keeps late signature
  results);
- the ContentModified re-issue;
- `additionalTextEdits`;
- the whole-document line diff;
- `targetSelectionRange`;
- the diagnostics cache;
- `severity: None`.

**Rejected:**
- a request timeout: we have no clock, and supersession bounds the pending table;
- general mapping of results through edits: continuation plus the diagnostics mover cover it.

CodeMirror's composed multi-commit sync was first rejected, then adopted in a different form as the
per-commit log (D8).

## Critique resolution log

- **Round 0 (exploration).** Findings:
  - no `--all-features` in CI;
  - the wasm build covers every member;
  - the code is not rustfmt-clean;
  - `lsp-server` is unsuitable;
  - the trigger is discarded, and staleness is revision-only;
  - undo/redo are unlogged and ties are misordered;
  - `Snapshot` lacks points;
  - dead code forces a vertical build.
- **Round 0.5 (scope).** The user pulled in multi-document, Ctrl+Space, lifecycle, list reuse,
  definition, rename and format.
- **Round 1 (10 majors):**
  - `apply_lsp` re-syncs edits;
  - `DocId` bundles;
  - editor-side abandonment;
  - rename-field plumbing;
  - a fold-aware jump verb;
  - the widget `diff` reset;
  - `filterText`;
  - rename made opt-in;
  - the log reset at open;
  - completion continuation.
- **Round 2 (4 majors):**
  - tickets replace the booleans;
  - `Start` samples "open or awaited";
  - Phase 3 needs Phase 1, and the phases run sequentially;
  - `%3A` normalization.
- **Round 3 (5 majors):**
  - the prefix check;
  - signature re-query while awaited;
  - definition routed through the requester;
  - acceptance requires ticket and revision;
  - public `set_definition`.
- **Round 4 (8 minors):**
  - ordering of `Collapse` and accept;
  - deletion re-request;
  - `hover_pending`;
  - `Jump` without `Local`;
  - `apply_lsp` runs the full sync.
- **Round 5 (6 minors):**
  - the signature trigger rule;
  - no-op sync;
  - the CI gate;
  - unknown ids;
  - format options;
  - `accepts` as the single owner.
- **Round 6 (approved, 6 minors):**
  - rename version snapshots;
  - `escape_markdown` ownership;
  - no raw bools;
  - D4 stamps;
  - rename opt-in untouched by `open_lsp`;
  - `Ok` for server requests.
- **Step 4 (FOSS).** See "Adopted" above.
- **Step 5, expert pass 1 (10 majors):**
  - the per-commit log, which withdrew a rejection that rested on a wrong premise;
  - one client per editor;
  - the bracket-stack call identity;
  - the Myers cap;
  - the `(start, end)` sort;
  - char-boundary trims;
  - the remainder check;
  - the explicit accept batch;
  - signature re-issue;
  - a bounded ContentModified re-issue;
  - `CodeEditor::jump`.
- **Step 5, expert pass 2 (1 major: the layered text contradicted itself).** Fixed by **this
  consolidated rewrite**, which also folds in these minors:
  - `Changes` carries `doc_id` and an explicit `from`, with the chain fold;
  - the log cap and the `ChangeLog` owner;
  - the redundant deletion-`Fresh` clause removed;
  - additional edits dropped on the word's closed interval, and the snippet base taken from
    `new.start`;
  - the indent read before the edit, and no `insert_text`;
  - the `enclosing_openers` citation and its line-local caveat;
  - `reissued_for` inherited;
  - `Refusal::Foreign`;
  - an accepted `None` clears the slots;
  - Phase 1 owns the `code_editor.rs` drain change;
  - `Snapshot::chunks` and `clip_offset`;
  - `matches` moved to Phase 2;
  - the files table;
  - `FileEdits` carries the encoding;
  - the `open_lsp` failure order;
  - the `client::Id` mechanism;
  - the list of invariant types;
  - NONE advancing the synced snapshot.


### Step 7 — Cross-reference audit

- **Fixes:** the ten docs and the plan were reconciled:
  - `ticket::Counter` is the only way to mint a `Ticket`;
  - `HoverRequest` has public fields;
  - `Pending::versions` holds revisions;
  - the rename and `FileEdits` rules;
  - the `close_lsp` wording;
  - `Refusal::Overlap` covers every `TransactionError`;
  - test-helper name clashes;
  - the files table.
- **Silent points the docs decided, now adopted as plan:**
  1. A rename that touches a document closed since the request fails with `StaleEdit`.
  2. If the editor rejects the combined accept batch, accept retries with the main op alone. The
     completion still lands; the auto-import is lost.
  3. `Error::Server { doc_id: Option<DocId>, .. }`, because a failed `initialize` has no document.
  4. In `apply_lsp`, every stamp failure, including an unaccepted ticket, reports `Refusal::Stale`.
  5. Server errors: completion answers `Completions(vec![])`, hover `Hover(None)` and signature
     `Signature(None)`. Every awaited slot settles, and only user commands (definition, rename,
     format) return `Err`.

## TODO before dispatch

- [ ] Re-read RUST_STYLE.md, OPAQUE.md and the `/iced` skill.
- [ ] `git log --oneline -3` shows `6cf4f2c` or a descendant, and `git status` is clean.
- [ ] Baseline: `cargo test --workspace` is green. Clippy is green except the known
      highlight.rs:1097 lint, which Phase 1 Step 0 fixes. Any *other* clippy failure means the
      toolchain moved; fix it or report it before dispatch.
- [ ] Grep the cited constructs and update the docs if they drifted:
  - code_editor.rs: 121-126, 227-269, 464-472, 535-603, 629, 687-726, 773-910, 916-1070, 1138-1153,
    1352-1428, 1434-1485, 1521-1547, 1597, 1679-1700, 1892-2016
  - editor.rs: 222, 456-538, 1174-1215, 2205-2226, 2307-2348, 2490-2525, 3440-3560, 3628-3659
  - document.rs: 224-227, 960, 1034-1151, 2119-2170
  - history.rs: 207, 376-388
  - transaction.rs: 164-234
  - completion.rs: 46-153
  - buffer.rs: 102-202
  - rope.rs: 21, 68-72, 151-170, 219, 240-244, 370-400
  - bracket_tree.rs: 151-153, 374-386
  - verbs.rs: 747-748
- [ ] Confirm the gaps are still there: undo/redo still unlogged, and F2, F12, Ctrl+Space and
      Alt+Shift+F still unbound.
- [ ] Confirm lsp-types 0.97.0 is still cached and `ci.yml` is unchanged.

---

## Appendix A — Standard agent dispatch preamble

Paste this verbatim, then append the phase doc path.

> You are implementing one phase of a planned feature in the scrive workspace
> (`/home/nickel/Programming/github/scrive`, branch `lsp_bridge`).
>
> **Read these fully, in order, before writing any code:**
> 1. `~/.claude/guides/RUST_STYLE.md` and `~/.claude/guides/OPAQUE.md`
> 2. The `/iced` skill (Skill tool, `iced`). Its conventions govern all iced-facing code.
> 3. `.claude/map/lsp-bridge/MAP_PLAN.md`, especially "Current state", "Key design decisions",
>    "Constraints" and "Risks"
> 4. Your phase doc: `.claude/map/lsp-bridge/MAP_PHASE_<N>.md`
> 5. `CLAUDE.md` and `MEMORY.md`, if present (none exist today)
>
> **Ground truth is source, not memory.**
> - Read every file before you change it.
> - Read lsp-types 0.97 from `~/.cargo/registry/src/*/lsp-types-0.97.0/`, and iced from the pinned
>   checkout under `~/.cargo/git/checkouts/`.
> - Match the surrounding code:
>   - comments make up about 25% of the lines and explain *why*;
>   - every `#[allow]` carries a justification;
>   - tests carry `///` invariant docs and string asserts.
>
> **You may run:**
> - `cargo build`, `cargo test`, `cargo clippy --workspace --all-targets [--all-features] -- -D warnings`
> - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace [--all-features]`
> - `cargo build --workspace --all-targets [--all-features] --target wasm32-unknown-unknown`
> - `rustfmt --edition 2021 <a file you created in this phase>`
> - read-only inspection: `git diff`, `git show`, `grep`, `cargo tree`
>
> **You must NOT run:**
> - `cargo run --example …`, because it opens a GUI and hangs;
> - `cargo fmt`, because the code isn't rustfmt-clean;
> - `git commit`, `git push`, `git stash` or `git checkout -- …`, because the orchestrator commits;
> - `cargo update`, or anything that changes the iced pin;
> - `cargo add` for a crate outside the budget.
>
> **Self-review before you report.** Check for:
> - composite names on new scrive-lsp types;
> - `use foo as bar` (only D1's facade is allowed);
> - `unwrap()` in library code;
> - wildcard arms over enums you own;
> - missing docs;
> - comments that narrate the plan or its history;
> - `std::time`, threads or I/O in scrive-lsp;
> - `Widget::new` where an iced helper exists;
> - an unjustified `#[allow]`;
> - dead code;
> - raw `bool` parameters;
> - a missing `#[must_use]` on anything that returns messages;
> - public fields on the invariant types listed in Constraints.
>
> **Forbidden:**
> - traits over closed sets;
> - shims, apart from the specified request-type re-exports;
> - `todo!()`;
> - speculative helpers or variants;
> - `#[allow(dead_code)]`;
> - AI attribution.
>
> **If you hit a real design decision the plan doesn't cover, STOP and report it.** Report:
> - what changed, file by file;
> - the commands you ran and their results;
> - what you couldn't verify;
> - what you think the plan got wrong.

---

## Appendix B — Resume guidance (`/goon lsp-bridge`)

1. Read `SESSION_HANDOFF.md` and `goon.yaml`, then run the `quick` checklist.
2. Skim this plan's "Current state", "Key design decisions" and "Risks".
3. Take the next phase from the handoff's status table, work through "TODO before dispatch", and
   dispatch it with Appendix A.
4. After the agent reports:
   - review the diff against the phase doc's exit criteria and its "What NOT to change";
   - run `verify` and `lint`;
   - commit with a Conventional Commit and no attribution;
   - update the handoff.
5. Run the phases strictly in order, 1 through 10.
