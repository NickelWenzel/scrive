# Phase 9 — scrive-iced `lsp` feature and the CodeEditor glue

> **Settled plan points (now stated in MAP_PLAN.md D19).**
> 1. **No recursion.** Local answers (including D12's `Definition(None)` decline) land through a
>    private `land` that never syncs, so `apply_lsp` → `sync_lsp` cannot recurse by construction.
> 2. **`Refusal::Overlap` covers every `TransactionError`**, `WouldOverflow` included; its doc says
>    "the batch was rejected (overlapping ranges, or growth past the u32 offset space)".
> 3. **`close_lsp` and rename:** it closes an open rename *field* and drops an unsent rename
>    request, but keeps the `rename(bool)` opt-in, so a server restart (D5: close, then `open_lsp`
>    on every tab) keeps F2.

## Prerequisites

- Phases 1–8 are merged and green.
- Read `crates/scrive-iced/src/code_editor.rs` **in full**. Phases 1–3 changed it a lot. Also
  read `crates/scrive-iced/{Cargo.toml, src/lib.rs}`, `.github/workflows/ci.yml` (Phase 4 edited
  it), and `crates/scrive-lsp/src/{lib.rs, client.rs, update.rs}`.
- Read the iced skill. It governs this code: `.map(Message::Variant)`, `#[must_use]`, no aliased
  imports.

### Names assumed from Phases 1–8

This doc refers to earlier phases' items by these names. **Use the real names** where they differ.
Don't add a second path to the same fact.

| Role | Name used here | Owner |
|---|---|---|
| `Document` identity / change log | `doc.doc_id()`, `doc.observe_changes(bool)`, `doc.drain_changes() -> document::Changes` | Phase 1 |
| Select + reveal | `CodeEditor::select(range)` (runs `after_edit(CaretOrClose)`) | Phase 3 |
| Ticket acceptance | private `fn accepts(&self, kind: Awaited, ticket: Ticket) -> bool` and `fn abandon(&mut self, kind: Awaited)` (clears the awaited ticket and the unsent request), with `Awaited::{Completion, Signature, Hover, Definition}` | Phases 2/3 |
| Awaiting slots | `self.awaiting: Awaiting { completion, signature, hover, definition }` | Phases 2/3 |
| Ingest | `set_completions(ticket, items)`, `set_signature(ticket, info)`, `set_hover(ticket, info)`, `set_definition(ticket, Option<Range<u32>>)`, `set_diagnostics(revision, diags) -> DiagnosticsOutcome` | Phases 2/3 |
| Pending request slots | `take_{completion,signature,hover,definition,rename,format}_request()`; fields `pending_rename_request`, `pending_format_request` | Phases 2/3 |
| Rename field state | `self.rename: Option<Rename>` (the open field) and `self.rename_enabled` (set by `CodeEditor::rename(bool)`) | Phase 3 |
| Rename-field events | `Event::RenameText(String)`, `Event::SubmitRename` | Phase 3 |
| Command actions | `Action::{TriggerCompletion, GotoDefinition, Rename, Format}` | Phases 2/3 |
| Client | `Client::{builder, id, open, sync, close, receive}` (Phase 5); `complete`, `signature_help`, `hover`, `definition`, `rename`, `format`, each `(&mut self, &Snapshot, &…Request) -> Output` | Phases 5–8 |
| `Client::open` shape | `open(&mut self, snapshot: &Snapshot, uri: &Uri, language: impl Into<String>) -> Result<Output, Error>` | Phase 5 |
| Output | `Output { pub messages: Vec<Message>, pub updates: Vec<Update> }` | Phase 5 |
| Update bundle | `update::Document::{doc_id, stamp, change, into_parts}` | Phase 5 |
| Definition targets | `update::{Target, jump::Open, jump::Unopened}` | Phase 8 |

## Goal and exit criteria

Behind `features = ["lsp"]`, a `CodeEditor` talks to one `scrive_lsp::Client` through five
methods, and the host's `update` shrinks to the loop in MAP_PLAN.md "Target state". The methods are
`open_lsp`, `sync_lsp`, `apply_lsp`, `jump` and `close_lsp`. `try_edit` is public and ungated.
`update::{Applied, Jump, Refusal}` live in scrive-lsp. CI checks the feature.

**Exit.** These tests pass under `cargo test -p scrive-iced --features lsp`. All of them live in
`crates/scrive-iced/src/code_editor/lsp.rs` under `#[cfg(test)]`, so they exist only with the
feature.
- `diagnostics_publish_lands_in_the_editor`
- `completion_reply_opens_the_popup`
- `signature_reply_opens_the_box`
- `hover_reply_shows_the_card`
- `local_definition_selects_the_target`
- `format_reply_edits_the_document_and_syncs_it`
- `two_editors_on_one_client_sync_independently`
- `background_rename_emits_its_did_change_then_a_second_rename_lands_in_both`
- `open_lsp_drops_a_stale_change_log`
- `failed_open_lsp_leaves_the_editor_untouched`
- `apply_lsp_refuses_a_foreign_document`
- `typing_twice_before_the_reply_continues_the_completion`
- `a_click_after_f12_drops_the_late_local_definition`
- `a_click_after_f12_drops_the_late_cross_document_jump`
- `jump_refuses_a_target_whose_document_moved`
- `sync_lsp_lands_local_declines`
- `overlapping_format_edits_are_refused_as_overlap`
- `close_lsp_clears_diagnostics_and_sends_did_close`
- `try_edit_reports_an_overlapping_batch` (code_editor.rs; ungated)

Also: every workspace command in "Verification" is green, both with and without `--all-features`.

## Design decisions implemented

- **D1.**
  - `lsp = ["dep:scrive-lsp"]`.
  - `#[cfg(feature = "lsp")] pub use scrive_lsp as lsp;`. This is the facade re-export, the one
    allowed rename.
  - The glue lives in `code_editor/lsp.rs`, a child module of `code_editor`.
- **D5.**
  - One `Client` per `CodeEditor`.
  - `open_lsp` stores the `client::Id` and debug-asserts that no other client is registered.
  - A restarted server means `close_lsp` (or dropping the client), then `open_lsp` again.
- **D18.**
  - `apply_lsp` handles `Target::Local` through `set_definition`.
  - `Open` and `Unopened` come back as `Applied.jump: Option<update::Jump>`, and only when the
    ticket was accepted. `Jump` has no `Local` case.
  - `CodeEditor::jump(client, open) -> Result<Vec<Message>, update::Refusal>` checks the `DocId`
    and the revision, then selects and syncs.
- **D19, in full:**
  - **`open_lsp`**
    - It registers the document first.
    - Only on success does it reset the log (off, then on) and store the id.
    - It lands its own updates (the cached diagnostics).
  - **`sync_lsp`**
    - It drains, syncs, and dispatches every pending kind.
    - It lands its local answers through the same checks as `apply_lsp`.
  - **`apply_lsp`**
    - It refuses a foreign document.
    - It checks the stamp:
      - a ticket on completion, signature, hover or definition goes through `accepts`;
      - a ticket on edits checks `revision()`;
      - a revision stamp checks the revision.
    - It applies the change, with edits going through `try_edit`.
    - It **always** finishes with the `sync_lsp` path.
  - **`close_lsp`**
    - It turns off observing and closes the rename field.
    - It clears the awaiting and pending slots and the diagnostics.
    - It calls `Client::close`.
  - All five methods are `#[must_use]`.
- **Constraints.** scrive-iced depends on scrive-lsp only through the optional dependency.
  serde_json is a dev-dependency. The rename opt-in is untouched by `open_lsp`.

Decisions this doc makes where the plan is open:

- **Decision: every stamp failure reports `Refusal::Stale`, including ticketed intel results
  that fail `accepts`.** One rule, one log line for the host. A late completion is not an error,
  but it's worth seeing in a traffic log.
- **Decision: a stamp of the wrong kind** (a `Revision` on completions, a `Ticket` on
  diagnostics) is `Refusal::Stale`. The client never produces one, and `let … else` handles it
  without a wildcard arm.
- **Decision: `open_lsp` hands `Client::open` one snapshot and resets the log right after.**
  The reset doesn't change the revision, so the log's `from` equals the registered snapshot's
  revision. A `debug_assert_eq!` pins that.
- **Decision: `land` returns `update::Applied` with empty `messages`.** `apply_lsp` fills in the
  messages, so there is no private tuple type.
- **Decision: `try_edit(ops) -> Result<(), TransactionError>` is public and ungated, and `edit`
  delegates to it.** A non-LSP host benefits too, and `Committed` stays internal.
- **Decision: the client-id field is named `lsp_client` and typed `scrive_lsp::client::Id`.**
  Inside code_editor.rs the name `lsp` means the child module, so the extern crate path is used.
- **Decision: the CI steps are added next to the default ones, except wasm.** Test, clippy and
  doc run twice: a default-only doc build is what catches an intra-doc link to a gated item. The
  wasm build is replaced by its all-features superset, which is the build the plan's
  verification lists.
- **Decision: serde_json is a direct dev-dependency (`"1"`),** listed like scrive-iced's other
  dev-dependencies.

## Step-by-step changes

### 1. `crates/scrive-iced/Cargo.toml`

Phase 4 already moved scrive-core to `{ workspace = true }` and added `scrive-lsp` to
`[workspace.dependencies]`. Diff against that state:

```diff
 [dependencies]
 scrive-core = { workspace = true }
+# The Language Server Protocol bridge, behind the `lsp` feature: re-exported as
+# `scrive_iced::lsp`, and driven by the `CodeEditor` glue in `code_editor/lsp.rs`.
+scrive-lsp = { workspace = true, optional = true }
 # `advanced` unlocks the low-level `Widget` API (iced::advanced::*) the editor
 # widget implements directly, rather than as a canvas::Program.
 iced = { version = "0.15.0-dev", features = ["advanced"] }

+[features]
+# Language-server support: re-exports scrive-lsp as `scrive_iced::lsp` and adds
+# `CodeEditor::{open_lsp, sync_lsp, apply_lsp, jump, close_lsp}`. Off by default,
+# so a host without a language server compiles none of it.
+lsp = ["dep:scrive-lsp"]
+
 [dev-dependencies]
 ...
 gif = "0.13"
+# JSON-RPC fixtures: the glue tests (and the `lsp` example) write server replies
+# as JSON and read the client's requests back as JSON.
+serde_json = "1"
```

Keep `[package.metadata.docs.rs] all-features = true`, which documents the glue on docs.rs.
Don't add `[[example]] lsp` yet (Phase 10).

### 2. `crates/scrive-iced/src/lib.rs`

After `pub mod popup;` and the other `pub use` lines:

```rust
/// The Language Server Protocol bridge, re-exported so a host names one crate:
/// `scrive_iced::lsp::Client`, `scrive_iced::lsp::Message`, `scrive_iced::lsp::update`. It
/// comes with [`CodeEditor::open_lsp`] and the rest of the glue. Needs the `lsp` feature.
#[cfg(feature = "lsp")]
pub use scrive_lsp as lsp;
```

The intra-doc link to `CodeEditor::open_lsp` is fine here, because the item it sits on is gated
too. Don't link a gated item from ungated docs (see Pitfalls). If the crate docs get a sentence
about the feature, write the names as plain code spans.

### 3. `crates/scrive-iced/src/code_editor.rs`

**a. Declare the child module** after the `use` block:

```rust
#[cfg(feature = "lsp")]
mod lsp;
```

It's a child of `code_editor`, so it sees `CodeEditor`'s private fields and methods (`doc`,
`awaiting`, `rename`, `accepts`, `after_edit`, …) with no widening of visibility. It adds an
inherent `impl CodeEditor` block, which Rust allows anywhere in the defining crate.

**b. The client-id field**, last in the struct:

```rust
    /// The language-server client this editor's document is registered with. One per editor:
    /// the change log and the request slots each have a single consumer (D5).
    #[cfg(feature = "lsp")]
    lsp_client: Option<scrive_lsp::client::Id>,
```

And in `CodeEditor::new`'s struct literal, last:

```rust
            #[cfg(feature = "lsp")]
            lsp_client: None,
```

`scrive_lsp::client::Id` must be `Copy + Eq + Debug`, which Phase 5 derived. If `CodeEditor`
derives or implements `Debug` by hand, add the field there under the same `cfg`.

**c. `try_edit`, with `edit` delegating to it.** Move whatever body `edit` has now (Phases 2/3 may
have added lines, such as closing the rename field when the revision moves) into `try_edit`:

```rust
    /// Apply a programmatic batch of edits as one transaction, then run the post-edit tail.
    /// A rejected batch (overlapping ranges) is dropped silently; use
    /// [`try_edit`](CodeEditor::try_edit) to see why.
    pub fn edit(&mut self, ops: Vec<EditOp>) {
        let _ = self.try_edit(ops);
    }

    /// [`edit`](CodeEditor::edit), but a rejected batch comes back as the error instead of
    /// being swallowed. Nothing is applied on `Err`: a transaction is all-or-nothing.
    ///
    /// # Errors
    /// [`TransactionError::Overlap`] when two ops overlap in the pre-edit text, and
    /// [`TransactionError::WouldOverflow`] when the result would pass the `u32` offset space.
    pub fn try_edit(&mut self, ops: Vec<EditOp>) -> Result<(), TransactionError> {
        let before = self.doc.revision();
        self.doc.edit(ops)?;
        self.after_edit(CompletionEvent::CaretOrClose);
        if self.doc.revision() != before {
            self.dirty = true;
        }
        Ok(())
    }
```

Add `TransactionError` to the `scrive_core` import. Plus the ungated test:

```rust
    /// `try_edit` reports an overlapping batch instead of swallowing it, and applies nothing.
    #[test]
    fn try_edit_reports_an_overlapping_batch() {
        let mut ed = CodeEditor::new("abcd\n");
        let result = ed.try_edit(vec![EditOp::new(0..3, "x"), EditOp::new(1..4, "y")]);
        assert!(matches!(result, Err(TransactionError::Overlap { .. })), "overlap is reported");
        assert_eq!(ed.document().text().into_owned(), "abcd\n", "nothing was applied");
        assert!(!ed.is_dirty(), "a rejected batch doesn't dirty the document");
    }
```

### 4. `crates/scrive-lsp/src/update.rs`: `Applied`, `Jump`, `Refusal`

```rust
/// A definition target outside the requesting editor, for the host to route: [`Open`](Jump::Open)
/// goes to the tab holding that document (`CodeEditor::jump`), and [`Unopened`](Jump::Unopened)
/// means reading the file first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Jump {
    /// Another document open on this client.
    Open(jump::Open),
    /// A document this client has not opened.
    Unopened(jump::Unopened),
}

/// Why an editor did not apply an update. Informational: nothing was changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// The update is for another document.
    #[error("the update is for another document")]
    Foreign,
    /// The update was computed for text the editor has moved past, or for a request the
    /// editor has abandoned.
    #[error("the update is stale")]
    Stale,
    /// The edit batch was rejected: overlapping ranges, or growth past the `u32` offset space.
    #[error("the edit batch was rejected")]
    Overlap,
}

/// What an editor did with one [`Document`] update. The host sends `messages`, routes `jump`
/// and logs `refused`.
#[must_use = "Applied.messages must reach the server, and Applied.jump must be routed"]
#[derive(Debug, Default)]
pub struct Applied {
    /// Messages to send, from the sync that follows every application.
    pub messages: Vec<crate::Message>,
    /// A definition target in another document, present only when the definition ticket was
    /// accepted.
    pub jump: Option<Jump>,
    /// Why the update was not applied, if it wasn't.
    pub refused: Option<Refusal>,
}
```

`Applied` has public fields: it carries no invariant (D4's opaque list doesn't include it), and
the host destructures it. `thiserror` is already in scrive-lsp's budget.

### 5. `crates/scrive-iced/src/code_editor/lsp.rs` (new): the glue module

```rust
//! The language-server glue: a [`CodeEditor`]'s side of a [`scrive_lsp::Client`].
//!
//! A host calls [`open_lsp`](CodeEditor::open_lsp) once per document,
//! [`sync_lsp`](CodeEditor::sync_lsp) after every [`update`](CodeEditor::update), and
//! [`apply_lsp`](CodeEditor::apply_lsp) for each `Update::Document` the client returns. Each call
//! returns the messages to send. The transport stays the host's.

use scrive_core::DiagnosticsOutcome;
use scrive_lsp::lsp_types::Uri;
use scrive_lsp::update::{self, Change, Refusal, Stamp, Target};
use scrive_lsp::{Client, Error, Message, Output, Update};

use super::{Awaited, CodeEditor};

impl CodeEditor {
    /// Register this editor's document with `client` under `uri`, in language `language`, and
    /// start mirroring its edits. Returns the messages to send (a `didOpen`, once the client
    /// is initialized).
    ///
    /// Call it once per document, and again after [`load`](CodeEditor::load)ing a different file
    /// (after [`close_lsp`](CodeEditor::close_lsp)). One editor talks to one client.
    ///
    /// # Errors
    /// `Error::DuplicateUri` when another document already claims `uri`. The editor is then left
    /// exactly as it was: its change log is not reset, and no client is recorded.
    pub fn open_lsp(&mut self, client: &mut Client, uri: &Uri, language: &str) -> Result<Vec<Message>, Error> {
        debug_assert!(
            self.lsp_client.is_none_or(|id| id == client.id()),
            "a CodeEditor talks to one Client: close_lsp before opening with another",
        );
        let snapshot = self.doc.snapshot();
        let output = client.open(&snapshot, uri, language)?;
        // Restart the log at the registered text: entries logged before it would replay edits the
        // server already has.
        self.doc.observe_changes(false);
        self.doc.observe_changes(true);
        debug_assert_eq!(self.doc.revision(), snapshot.revision(), "the log starts at the registered snapshot");
        self.lsp_client = Some(client.id());
        Ok(self.route(output))
    }

    /// Mirror everything since the last sync to `client`, then send every request the editor
    /// recorded: completion, signature help, hover, definition, rename, format. Call it after
    /// every [`update`](CodeEditor::update). With nothing new, it returns no messages. Answers the
    /// client gives locally (declines, reused completion lists) land before it returns.
    #[must_use = "the messages must be sent to the server"]
    pub fn sync_lsp(&mut self, client: &mut Client) -> Vec<Message> {
        debug_assert_eq!(self.lsp_client, Some(client.id()), "sync_lsp needs the Client this editor was opened with");
        let snapshot = self.doc.snapshot();
        // Sync before dispatching: every request is checked against the synced revision.
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
        let mut messages = Vec::new();
        for output in outputs {
            messages.extend(self.route(output));
        }
        messages
    }

    /// Apply one update from [`Client::receive`] to this editor, then sync. The update is
    /// refused if it is for another document, stale, or an edit batch that doesn't apply.
    /// A definition in another document comes back as [`Applied::jump`](update::Applied::jump)
    /// for the host to route.
    #[must_use = "Applied.messages must be sent, and Applied.jump routed"]
    pub fn apply_lsp(&mut self, client: &mut Client, document: update::Document) -> update::Applied {
        let mut applied = self.land(document);
        // Always sync: an applied edit must reach the server, and it's a no-op otherwise.
        applied.messages = self.sync_lsp(client);
        applied
    }

    /// Select a definition that another editor's [`apply_lsp`](CodeEditor::apply_lsp) returned
    /// as [`Jump::Open`](update::Jump::Open), then sync. Refused if `open` is for another
    /// document, or this one moved since the server answered.
    ///
    /// # Errors
    /// [`Refusal::Foreign`] for another document's target and [`Refusal::Stale`] for a moved
    /// one. Nothing is selected either way.
    pub fn jump(&mut self, client: &mut Client, open: update::jump::Open) -> Result<Vec<Message>, Refusal> {
        if open.doc_id() != self.doc.doc_id() {
            return Err(Refusal::Foreign);
        }
        if open.revision() != self.doc.revision() {
            return Err(Refusal::Stale);
        }
        self.select(open.span());
        Ok(self.sync_lsp(client))
    }

    /// Unregister this editor's document. Mirroring stops, the rename field closes, requests in
    /// flight are forgotten, and the server's diagnostics are cleared. Returns the `didClose`.
    /// The `rename(bool)` opt-in is kept, so a later [`open_lsp`](CodeEditor::open_lsp) restores
    /// F2.
    #[must_use = "the didClose must be sent to the server"]
    pub fn close_lsp(&mut self, client: &mut Client) -> Vec<Message> {
        self.doc.observe_changes(false);
        self.rename = None;
        // `abandon` clears each kind's awaited ticket and its unsent request together.
        self.abandon(Awaited::Completion);
        self.abandon(Awaited::Signature);
        self.abandon(Awaited::Hover);
        self.abandon(Awaited::Definition);
        self.pending_rename_request = None;
        self.pending_format_request = None;
        let _ = self.set_diagnostics(self.doc.revision(), Vec::new());
        self.lsp_client = None;
        let output = client.close(self.doc.doc_id());
        debug_assert!(output.updates.is_empty(), "close answers with messages only");
        output.messages
    }

    /// Land the updates the client answered locally for this document, and return its
    /// messages. `open` and `sync` answer only for the document they were given, and local
    /// answers carry the current ticket, so nothing here is refused in practice.
    fn route(&mut self, output: Output) -> Vec<Message> {
        let Output { messages, updates } = output;
        debug_assert!(
            updates.iter().all(|update| matches!(update, Update::Document(_))),
            "open and sync produce document updates only",
        );
        for update in updates {
            if let Update::Document(document) = update {
                let applied = self.land(document);
                debug_assert!(applied.jump.is_none(), "local answers never point into another document");
            }
        }
        messages
    }

    /// Check `document`'s identity and stamp against this editor, then apply its change.
    /// Never syncs, which is what lets both `route` and `apply_lsp` use it.
    fn land(&mut self, document: update::Document) -> update::Applied {
        if document.doc_id() != self.doc.doc_id() {
            return update::Applied { refused: Some(Refusal::Foreign), ..Default::default() };
        }
        let stale = update::Applied { refused: Some(Refusal::Stale), ..Default::default() };
        let (_, stamp, change) = document.into_parts();
        match change {
            Change::Diagnostics(diagnostics) => {
                let Stamp::Revision(revision) = stamp else { return stale };
                match self.set_diagnostics(revision, diagnostics) {
                    DiagnosticsOutcome::Applied { .. } => update::Applied::default(),
                    DiagnosticsOutcome::Stale { .. } => stale,
                }
            }
            Change::Completions(items) => {
                let Stamp::Ticket(ticket) = stamp else { return stale };
                let accepted = self.accepts(Awaited::Completion, ticket);
                self.set_completions(ticket, items);
                if accepted { update::Applied::default() } else { stale }
            }
            Change::Signature(info) => {
                let Stamp::Ticket(ticket) = stamp else { return stale };
                let accepted = self.accepts(Awaited::Signature, ticket);
                self.set_signature(ticket, info);
                if accepted { update::Applied::default() } else { stale }
            }
            Change::Hover(info) => {
                let Stamp::Ticket(ticket) = stamp else { return stale };
                let accepted = self.accepts(Awaited::Hover, ticket);
                self.set_hover(ticket, info);
                if accepted { update::Applied::default() } else { stale }
            }
            Change::Definition(target) => {
                let Stamp::Ticket(ticket) = stamp else { return stale };
                if !self.accepts(Awaited::Definition, ticket) {
                    return stale;
                }
                let jump = match target {
                    Some(Target::Local(span)) => {
                        self.set_definition(ticket, Some(span));
                        None
                    }
                    // The jump leaves this editor: settle the slot here, and let the host select
                    // the span in the target's editor.
                    Some(Target::Open(open)) => {
                        self.set_definition(ticket, None);
                        Some(update::Jump::Open(open))
                    }
                    Some(Target::Unopened(unopened)) => {
                        self.set_definition(ticket, None);
                        Some(update::Jump::Unopened(unopened))
                    }
                    None => {
                        self.set_definition(ticket, None);
                        None
                    }
                };
                update::Applied { jump, ..Default::default() }
            }
            Change::Edits(ops) => {
                // A format carries its ticket and a rename carries a revision. Either way the
                // edits are byte offsets into exactly one revision.
                let revision = match stamp {
                    Stamp::Ticket(ticket) => ticket.revision(),
                    Stamp::Revision(revision) => revision,
                };
                if revision != self.doc.revision() {
                    return stale;
                }
                match self.try_edit(ops) {
                    Ok(()) => update::Applied::default(),
                    Err(_) => update::Applied { refused: Some(Refusal::Overlap), ..Default::default() },
                }
            }
        }
    }
}
```

Notes on the module:
- `set_definition` (Phase 3) runs `accepts` itself, then `abandon`s the slot and `select`s a
  `Some` range. `land` calls `accepts` first only to learn the outcome, because `set_*` return
  `()`. Both calls see the same state, so they agree.
- `is_none_or` needs Rust 1.82. The repo already uses `is_multiple_of` (1.87).

### 6. `.github/workflows/ci.yml`

Read the file as Phase 4 left it: it has the cargo-tree gate and the scrive-lsp wasip1 step. Make
these edits and nothing else.

**`test` job**: add a step after "Test workspace":

```yaml
      # The `lsp` feature adds the CodeEditor glue, its tests, and the `lsp`
      # example; none of them compile without it.
      - name: Test workspace (all features)
        run: cargo test --workspace --all-features
```

**`wasm` job**: replace the build step with its all-features superset:

```yaml
      # The browser target: the libraries (every feature), their tests, and the
      # examples must build.
      - name: Build for wasm32-unknown-unknown
        run: cargo build --workspace --all-targets --all-features --target wasm32-unknown-unknown
```

**`lints` job**: add one step after "Clippy (no warnings)" and one after "Docs build clean":

```yaml
      # The glue is `cfg(feature = "lsp")`: lint it too.
      - name: Clippy, all features (no warnings)
        run: cargo clippy --workspace --all-targets --all-features -- -D warnings
```

```yaml
      # The default build above catches links to gated items from ungated docs;
      # this one checks the gated docs themselves.
      - name: Docs build clean (all features)
        run: cargo doc --no-deps --workspace --all-features
        env:
          RUSTDOCFLAGS: -D warnings
```

### 7. `Cargo.lock`

It updates on the first build (scrive-iced gains an optional scrive-lsp edge, and serde_json
becomes a dev edge). Never run `cargo update`.

### 8. Tests (in `code_editor/lsp.rs`)

Harness:

```rust
#[cfg(test)]
mod tests {
    use iced::time::Instant;
    use serde_json::{json, Value};

    use scrive_lsp::lsp_types::Uri;
    use scrive_lsp::update::{self, Refusal};
    use scrive_lsp::{Client, Message, Output, Update};

    use crate::editor::Action;
    use crate::{CodeEditor, Event};

    const A: &str = "file:///w/a.rs";
    const B: &str = "file:///w/b.rs";

    fn uri(text: &str) -> Uri {
        text.parse().expect("test URIs parse")
    }

    /// A message as the JSON it serializes to: how the tests read requests.
    fn wire(message: &Message) -> Value {
        serde_json::to_value(message).expect("envelopes serialize")
    }

    /// A JSON fixture as an envelope.
    fn envelope(value: Value) -> Message {
        serde_json::from_value(value).expect("fixtures are envelopes")
    }

    /// The first outgoing message with `method`, as JSON.
    fn sent(messages: &[Message], method: &str) -> Value {
        messages.iter().map(wire).find(|m| m["method"] == method).unwrap_or_else(|| panic!("no {method} was sent"))
    }

    /// A success response to `request`.
    fn reply(request: &Value, result: Value) -> Message {
        envelope(json!({ "jsonrpc": "2.0", "id": request["id"], "result": result }))
    }

    /// A client past the handshake: every provider, incremental sync, byte columns (utf-8), so
    /// fixture positions are byte offsets.
    fn ready() -> Client {
        let (mut client, initialize) = Client::builder().root(uri("file:///w/")).build();
        let result = json!({ "capabilities": {
            "positionEncoding": "utf-8",
            "textDocumentSync": { "openClose": true, "change": 2 },
            "completionProvider": { "triggerCharacters": ["."] },
            "signatureHelpProvider": { "triggerCharacters": ["("] },
            "hoverProvider": true,
            "definitionProvider": true,
            "renameProvider": true,
            "documentFormattingProvider": true
        }});
        let output = client.receive(reply(&wire(&initialize), result)).expect("initialize result decodes");
        let _ = output.messages; // `initialized`
        client
    }

    /// An editor over `text`, opened on `client` under `path`, with the rename field enabled.
    fn opened(client: &mut Client, path: &str, text: &str) -> (CodeEditor, Vec<Message>) {
        let mut editor = CodeEditor::new(text).rename(true);
        let messages = editor.open_lsp(client, &uri(path), "rust").expect("the URI is free");
        (editor, messages)
    }

    /// Feed `event` through the editor's real update path, then sync.
    fn drive(editor: &mut CodeEditor, client: &mut Client, event: Event) -> Vec<Message> {
        let _ = editor.update(event, Instant::now());
        editor.sync_lsp(client)
    }

    /// The document updates in `output`, one per touched document, in order.
    fn documents(output: Output) -> Vec<update::Document> {
        output
            .updates
            .into_iter()
            .map(|update| match update {
                Update::Document(document) => document,
                other => panic!("expected document updates, got {other:?}"),
            })
            .collect()
    }
}
```

`documents` is the one helper through which the tests reach `Update`. Later variants then touch
one place.

| Test | Assertion | Body sketch |
|---|---|---|
| `diagnostics_publish_lands_in_the_editor` | 1 diagnostic in `0..11`; `refused == None` | open `"let x = 1;\n"`; read `version` from the `didOpen`; receive `publishDiagnostics` for A at that version, `0:4–0:5`; `apply_lsp` each document |
| `completion_reply_opens_the_popup` | `ed.completion.state()` is `Open(_)`; `refused == None` | `drive(Type('g'))` → `sent(.., "textDocument/completion")`; reply `{"isIncomplete":false,"items":[{"label":"greet"}]}`; apply |
| `signature_reply_opens_the_box` | `ed.signature.is_some()` | `drive(Type('f'))`, `drive(Type('('))` → `signatureHelp`; reply one signature `"f(a)"` |
| `hover_reply_shows_the_card` | `ed.hover.is_some()` | open `"hello\n"`; `drive(HoverQuery(2))` → `hover`; reply `{"contents":{"kind":"markdown","value":"**hi**"}}` |
| `local_definition_selects_the_target` | `ed.selection() == 3..4`; `jump == None` | open `"fn f() {}\nf();\n"`; `drive(PlaceCaret(10))`, `drive(GotoDefinition)`; reply `{"uri":A,"range":0:3–0:4}` |
| `format_reply_edits_the_document_and_syncs_it` | text `"a\n"`; `applied.messages` has a `didChange` whose change deletes `0:1–0:3` | open `"a  \n"`; `drive(Format)`; reply `[{"range":0:1–0:3,"newText":""}]` |
| `two_editors_on_one_client_sync_independently` | typing in A sends a `didChange` for A only; typing in B one for B only; each document's versions go up by one | open A and B on one client; type in each |
| `background_rename_emits_its_did_change_then_a_second_rename_lands_in_both` | after rename 1: A = `"welcome();\n"`, B = `"fn welcome() {}\n"`; B's `apply_lsp` returns a `didChange` for B (the background document); after rename 2: A = `"hail();\n"`, B = `"fn hail() {}\n"` | A = `"greet();\n"`, B = `"fn greet() {}\n"`. In A: `PlaceCaret(1)`, `Rename`, `Event::RenameText("welcome")`, `Event::SubmitRename` → `rename`. Reply `documentChanges` for A and B at their synced versions (read from `didOpen`/`didChange`); apply each to its editor. Repeat with `hail` at the new versions. |
| `open_lsp_drops_a_stale_change_log` | the post-open `didChange` has exactly one content change, with `text == "y"` | open with the editor observing and `Type('x')` logged before `open_lsp`; then `drive(Type('y'))` |
| `failed_open_lsp_leaves_the_editor_untouched` | `Err(Error::DuplicateUri{..})`; `b.lsp_client.is_none()`; B's log still has the pre-open entry | open A on `A`; B: `observe_changes(true)`, `Type('x')`, then `open_lsp(&mut client, &uri(A), ..)`; `b.drain_changes()` still holds one entry |
| `apply_lsp_refuses_a_foreign_document` | `refused == Some(Refusal::Foreign)`; A's diagnostics unchanged (0) | publish diagnostics for B; `a.apply_lsp(&mut client, doc_for_b)` |
| `typing_twice_before_the_reply_continues_the_completion` | the second sync sends no `completion` and no `$/cancelRequest`; the reply to the first id lands (`refused == None`) and the popup is open | `drive(Type('g'))` → completion `id1`; `drive(Type('r'))`; reply to `id1` with `greet`, `grow` |
| `a_click_after_f12_drops_the_late_local_definition` | `refused == Some(Refusal::Stale)`; selection is still the clicked caret `5..5` | `drive(GotoDefinition)` at 10; `drive(PlaceCaret(5))`; reply local `0:3–0:4` |
| `a_click_after_f12_drops_the_late_cross_document_jump` | `jump == None`; `refused == Some(Refusal::Stale)` | F12 in A; click in A; reply points into B |
| `jump_refuses_a_target_whose_document_moved` | `b.jump(..) == Err(Refusal::Stale)`; B's selection unchanged | F12 in A → `Jump::Open` into B; type in B (unsynced) before `b.jump` |
| `sync_lsp_lands_local_declines` | no `completion` or `definition` request is sent; `ed.awaiting.definition` is `None` (an accepted `Definition(None)` retires the slot); the popup is closed (the empty list lands; the completion slot itself stays until the next edit, D11) | a client that never got its `initialize` reply; `open_lsp` (deferred); `drive(TriggerCompletion)`; `drive(GotoDefinition)` |
| `overlapping_format_edits_are_refused_as_overlap` | `refused == Some(Refusal::Overlap)`; text unchanged | open `"abcd\n"`; `drive(Format)`; reply `[{0:0–0:3,"x"},{0:1–0:4,"y"}]` |
| `close_lsp_clears_diagnostics_and_sends_did_close` | `didClose` for A; 0 diagnostics; `lsp_client == None`; typing afterwards logs nothing (`drain_changes` empty) | publish + apply one diagnostic; `close_lsp` |

Every assert has a string message, and every test a `///` doc stating its invariant, such as
"A late local definition, after a click moved the caret, is refused as stale and doesn't move the
selection." Pass actions through `Event::Editor(Action::…)`. Build the `F12 → click` pair and
every other key path through `update`, never by calling `set_*` directly.

## Files changed

| File | Change |
|---|---|
| crates/scrive-iced/Cargo.toml | optional `scrive-lsp`, `[features] lsp`, dev-dep `serde_json` |
| crates/scrive-iced/src/lib.rs | `#[cfg(feature = "lsp")] pub use scrive_lsp as lsp;` |
| crates/scrive-iced/src/code_editor.rs | `#[cfg(feature = "lsp")] mod lsp;`, `lsp_client` field + init, `try_edit`, `edit` delegates, test |
| crates/scrive-iced/src/code_editor/lsp.rs | new: `open_lsp`, `sync_lsp`, `apply_lsp`, `jump`, `close_lsp`, `route`, `land` + tests |
| crates/scrive-lsp/src/update.rs | `Applied`, `Jump`, `Refusal` |
| .github/workflows/ci.yml | all-features test, wasm, clippy and doc steps |
| Cargo.lock | new edges (from the build) |

## Verification

```
cargo test --workspace
cargo test --workspace --all-features
cargo test -p scrive-iced --features lsp
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --all-features
cargo build --workspace --all-targets --all-features --target wasm32-unknown-unknown
for c in scrive-core scrive-lsp; do out=$(cargo tree -p $c -e normal --prefix none) || exit 1; if grep -qE '^(iced|winit|wgpu)' <<<"$out"; then echo "$c LEAK"; exit 1; fi; done
cargo tree -p scrive-iced -e normal --prefix none | grep -c '^scrive-lsp'           # 0: off by default
cargo tree -p scrive-iced -e normal --features lsp --prefix none | grep -c '^scrive-lsp'  # 1
rustfmt --edition 2021 crates/scrive-iced/src/code_editor/lsp.rs
```

## Spot-check tables

### `apply_lsp` stamp gate

`rev` is the editor's current `doc.revision()`.

| `doc_id` | Stamp | Change | Condition | Effect | `refused` | `jump` |
|---|---|---|---|---|---|---|
| foreign | any | any | — | none | `Foreign` | `None` |
| own | `Revision(r)` | `Diagnostics` | `r == rev` | squiggles replaced | `None` | `None` |
| own | `Revision(r)` | `Diagnostics` | `r != rev` | none | `Stale` | `None` |
| own | `Ticket(t)` | `Completions` / `Signature` / `Hover` | `accepts(kind, t)` | popup / box / card set | `None` | `None` |
| own | `Ticket(t)` | `Completions` / `Signature` / `Hover` | not accepted | `set_*` drops it | `Stale` | `None` |
| own | `Ticket(t)` | `Definition(Some(Local(s)))` | accepted | `s` selected, slot cleared | `None` | `None` |
| own | `Ticket(t)` | `Definition(Some(Open(o)))` | accepted | slot cleared | `None` | `Some(Jump::Open(o))` |
| own | `Ticket(t)` | `Definition(Some(Unopened(u)))` | accepted | slot cleared | `None` | `Some(Jump::Unopened(u))` |
| own | `Ticket(t)` | `Definition(None)` | accepted | slot cleared | `None` | `None` |
| own | `Ticket(t)` | `Definition(_)` | not accepted (click, edit, second F12) | none | `Stale` | `None` |
| own | `Ticket(t)` | `Edits(ops)` | `t.revision() == rev`, disjoint | one transaction, then a `didChange` | `None` | `None` |
| own | `Ticket(t)` | `Edits(ops)` | `t.revision() != rev` | none | `Stale` | `None` |
| own | `Revision(r)` | `Edits(ops)` | `r == rev`, disjoint | one transaction, then a `didChange` | `None` | `None` |
| own | `Revision(r)` | `Edits(ops)` | `r == rev`, overlapping | none | `Overlap` | `None` |
| own | `Revision(r)` | `Edits(ops)` | `r != rev` | none | `Stale` | `None` |
| own | `Revision(_)` | `Completions` / `Signature` / `Hover` / `Definition` | never produced | none | `Stale` | `None` |
| own | `Ticket(_)` | `Diagnostics` | never produced | none | `Stale` | `None` |

In every row, `apply_lsp` then runs `sync_lsp`, and `messages` holds whatever that produced.

### `jump`

| `open.doc_id()` | `open.revision()` | Result |
|---|---|---|
| ≠ editor's | any | `Err(Foreign)`, nothing selected |
| = editor's | ≠ `doc.revision()` (the editor has unsynced edits) | `Err(Stale)` |
| = editor's | = `doc.revision()` | span selected and revealed (unfolds), `Ok(sync_lsp messages)` |

### `open_lsp` ordering

| Step | On `Err(DuplicateUri)` | On `Ok` |
|---|---|---|
| `client.open(&snapshot, uri, lang)` | returns the error: the log, `lsp_client` and diagnostics are untouched | registered |
| `observe_changes(false)` then `observe_changes(true)` | not reached | log restarts at `snapshot.revision()` |
| `lsp_client = Some(client.id())` | not reached | stored |
| `route(output)` | not reached | cached diagnostics land |

## What NOT to change

- No scrive-lsp behavior. Phase 9 adds only `Applied`, `Jump` and `Refusal` to update.rs.
- Don't change `edit`'s swallowing behavior for existing callers. It delegates.
- Don't touch the rename opt-in (`CodeEditor::rename(bool)`) from `open_lsp` or `close_lsp`.
- Don't widen visibility of `CodeEditor` internals (`pub(crate)`, getters) for the glue's sake.
  The child module already sees them.
- No example, README or version changes (Phase 10). No `[[example]]` entry.
- Don't make the feature default, and don't gate `try_edit`.
- CI: add the listed steps only. Don't remove the default-feature test, clippy or doc steps.

## Pitfalls

- **Dead code under default features.** Everything only the glue uses must live in
  `code_editor/lsp.rs` or carry the same `#[cfg(feature = "lsp")]`: imports, private helpers,
  constants. A private helper added to code_editor.rs for the glue alone is `dead_code` in the
  default build, and clippy `-D warnings` fails CI. `lsp_client` is gated on both the field and
  its initializer.
- **Name clash inside code_editor.rs.** After `mod lsp;`, the path `lsp::…` in code_editor.rs
  means the child module. Refer to the crate as `scrive_lsp::…` there (the field type). Inside
  lsp.rs, import from `scrive_lsp` directly. Never `use scrive_lsp as …`.
- **Intra-doc links across the gate.** A doc comment on an ungated item that links
  ``[`CodeEditor::open_lsp`]`` breaks the default `cargo doc -D warnings`. Use plain code
  spans there.
- **`#[must_use]` propagation.**
  - `sync_lsp`, `apply_lsp` and `close_lsp` carry `#[must_use = "…"]`. `open_lsp` and `jump`
    return `Result`, which is already must-use.
  - Inside the glue, every result is consumed: `apply_lsp` stores `sync_lsp`'s messages, and
    `route` returns them.
  - The one intentional discard is `set_diagnostics`' outcome in `close_lsp` (it clears the
    unsent requests through `abandon` and by assigning `None`, not through `take_*`).
  - Don't write `let _ = editor.sync_lsp(..)` in tests: assert on the result, or bind it to a
    named variable used by the next assert.
- **Borrowing in the host.** In tests with two editors, `a` and `b` are separate locals, and
  `client` is a third, so `a.apply_lsp(&mut client, doc)` then `b.apply_lsp(&mut client, doc)`
  borrow fine. In an app, destructure `let Self { tabs, client, .. } = self;` so `tabs.iter_mut()`
  and `client` are disjoint field borrows. A `&mut self` method call inside that scope won't
  compile. Phase 10's example shows the full shape.
- **Order inside `sync_lsp`.** Sync first, then dispatch. A request checked before the sync
  sees an old synced revision and is declined (D12).
- **`accepts` before `set_*`.** `set_*` clears or consumes the slot, so calling `accepts`
  afterwards always says no.
- **wasm.** The glue has no I/O or clocks. The tests use `iced::time::Instant`, never
  `std::time`, because `--all-targets --all-features` builds them for wasm32 too.
- **Never run `cargo fmt`.** Only `rustfmt --edition 2021` on `code_editor/lsp.rs`, the file
  this phase creates.
