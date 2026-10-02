# Phase 6 — Completion, sessions (reuse and continuation), the pending machinery, snippet lowering, `markdown::to_plain`

Read `MAP_PLAN.md` first, then MAP_PHASE_5.md §4.6 (this phase edits that code). This doc implements
the Phase 6 bullet list and decisions D11 (client side), D12, D13 (conversion, advertised, cost)
and D14. It does not change any design decision; open details are marked **Decision:** with a
one-line reason.

> **Minting tickets in scrive-lsp tests.** Every conversation below builds a `CompletionRequest`,
> which needs a `scrive_core::Ticket`. Phase 2's `scrive_core::intel::ticket::Counter`
> (`new()`, `issue(Revision) -> Ticket`) is the only way to create one (D11; there is no
> `Ticket::new`). Each test holds one `Counter` and mints through the `request` helper in §6.1.
> Tickets cannot be rebuilt from a sequence number, so a test keeps each request and compares
> stamps against `request.ticket()`.

## 1. Prerequisites

Phases 1–5 are committed. Verify:

- Green baseline: `cargo test --workspace`, clippy `-D warnings`, the wasm32 build.
- Phase 5's `Client` exists with `State`, `Tracked`, `Output`, `Error`, `next_request`, `responded`
  and the `diagnostics(update)` test helper in `client/tests.rs`.
- Phase 2's request and item API. Grep and adapt call sites (not the design) if names differ:

  ```rust
  // scrive_core (re-exported at the root)
  impl CompletionRequest {                        // private fields
      pub fn new(ticket: Ticket, word: Range<u32>, trigger: CompletionTrigger, start: intel::completion::Start) -> Self;
      pub fn ticket(&self) -> Ticket;
      pub fn word(&self) -> Range<u32>;            // word.end is the caret
      pub fn trigger(&self) -> CompletionTrigger;
      pub fn start(&self) -> intel::completion::Start;   // Fresh | Continuing
  }
  impl Ticket { pub fn revision(&self) -> Revision; }    // Copy + Eq
  impl CompletionItem {
      pub fn with_filter(self, filter: impl Into<String>) -> Self;
      pub fn with_additional(self, additional: Vec<EditOp>) -> Self;
      pub fn with_signature_after(self, on: bool) -> Self;
      pub fn matches(&self, word: &str) -> bool;
      // fields readable: replace, insert, additional (pub, like the existing ones)
  }
  ```

  `grep -n "pub fn with_filter\|pub fn with_additional\|pub fn with_signature_after\|pub fn matches\|pub additional" crates/scrive-core/src/intel/providers.rs`.
  If `additional` is not a readable field, add nothing to scrive-core — read it through whatever
  accessor Phase 2 provides.

## 2. Goal and exit criteria

`Client::complete` answers the editor's `CompletionRequest`s: it declines locally when it cannot
ask, sends `textDocument/completion` otherwise, reuses a complete list while the user keeps typing,
continues an in-flight request instead of superseding it, and turns replies into
`Change::Completions` stamped with the latest ticket. The pending-request table it introduces —
supersede with `$/cancelRequest`, silent cancellations, one `ContentModified` re-issue per ticket,
the drop in `receive` — is the machinery Phases 7 and 8 reuse.

Exit criteria:

1. `cargo test -p scrive-lsp` passes everything from Phases 4–5 plus:
   - `completion.rs`: `clangd_bullet_item_filters_on_filter_text_and_keeps_the_word_range`,
     `additional_text_edits_convert_against_the_request_snapshot`,
     `trigger_parameter_hints_command_sets_signature_after`,
     `trigger_suggest_command_sets_retrigger`, `undecodable_item_is_skipped`,
     `insert_replace_edit_uses_its_insert_range`,
     `edit_range_spanning_lines_falls_back_to_the_word`,
     `edit_range_not_containing_the_caret_falls_back_to_the_word`,
     `edit_range_other_than_the_word_is_kept_as_replace`,
     `missing_text_edit_inserts_insert_text_or_label`,
     `sort_text_becomes_the_sort_key_and_defaults_to_the_label`,
     `item_kinds_map_to_scrive_kinds`, `null_array_and_list_replies_decode`,
     `finishing_lowers_snippets_and_flattens_documentation`,
     `answer_shifts_ranges_by_the_caret_delta`.
   - `snippet.rs`: `tab_stops_and_final_stop_pass_through`,
     `nested_placeholders_flatten_to_text`, `mirrors_become_text_after_the_first`,
     `choices_become_their_first_option`, `variables_become_their_default_or_nothing`,
     `transforms_are_dropped`, `escapes_survive_lowering`,
     `unterminated_element_falls_back_to_the_raw_body`,
     `lowering_scrive_rejects_falls_back_to_plain_text`,
     `every_lowered_snippet_parses_with_scrive`.
   - `markdown.rs`: `fenced_code_keeps_its_lines`, `bold_and_code_markers_are_removed`,
     `links_and_images_become_their_text`, `thematic_breaks_are_dropped`,
     `backslash_escapes_become_literals`, `headings_lose_their_hashes`,
     `snake_case_underscores_survive`, `documentation_kinds_lower_to_plain_text`.
   - `client/tests.rs`: `completion_request_carries_position_and_context`,
     `completion_advertises_snippets_plaintext_docs_and_context`,
     `second_request_supersedes_the_first_with_a_cancel`,
     `reply_to_a_superseded_request_is_silent`,
     `request_cancelled_and_server_cancelled_replies_are_silent`,
     `content_modified_is_reissued_once_per_ticket`,
     `other_server_errors_answer_an_empty_list`, `completion_declines_before_initialize`,
     `completion_declines_without_a_provider`, `unregistered_trigger_character_declines`,
     `multi_character_trigger_matches_by_suffix`, `stale_completion_request_is_ignored`,
     `complete_list_is_reused_with_shifted_ranges`,
     `incomplete_list_re_requests_with_trigger_for_incomplete`,
     `continuation_answers_the_latest_ticket_filtered_to_the_latest_word`,
     `incomplete_reply_after_the_caret_moved_answers_and_re_requests`,
     `forward_delete_ends_the_session`, `typing_past_thirty_two_bytes_supersedes`,
     `caret_left_supersedes`, `changed_prefix_supersedes`,
     `manual_request_never_continues`,
     `reply_behind_the_synced_revision_is_dropped_in_receive`,
     `close_cancels_pending_requests`, `shutdown_forgets_pending_requests`.
2. Clippy and doc clean with `-D warnings`; the wasm32 build passes.

## 3. Design decisions implemented

- **D12 — request lifecycle.** Pending entries hold `{ id, doc_id, request_snapshot,
  latest_ticket, latest_caret, query, reissued_for }` (`versions` arrives with Phase 8, its first
  reader). One pending entry per (document, kind); a new request supersedes the old one with
  `$/cancelRequest`, except a continuation. Silent replies — never `Error`, always
  `Ok(Output::default())`: an id with no pending entry; `RequestCancelled` (−32800);
  `ServerCancelled` (−32802); `ContentModified` (−32801), except that it is re-issued once per
  ticket when `latest_ticket.revision()` is still the synced revision (`reissued_for`, inherited by
  the new entry). An entry whose latest ticket has fallen behind the synced revision is dropped
  **in `receive`**, never in `sync`. `Client::complete` declines a request whose revision differs
  from its snapshot or the synced revision. Local declines answer with the request's ticket:
  `Completions(vec![])` when the client isn't ready, the server has no provider, or a trigger
  character isn't registered (matched with `ends_with`, so multi-character triggers work).
- **D13 — conversion.** `replace` is `None` when the edit range equals the request word, `Some`
  otherwise. `InsertReplaceEdit` decodes as its `insert` range. An edit range spanning lines or not
  containing the request position falls back to the word. `filterText` → `filter`, `sortText` →
  `sort_key`. `editor.action.triggerSuggest` → `retrigger`; `editor.action.triggerParameterHints` →
  `signature_after`. `additionalTextEdits` → `additional`. Snippets are lowered before parsing
  (variables → default or nothing, mirrors → text after the first, nesting flattened, choices → the
  first option); anything left unparseable falls back to plain text. Advertised: snippets,
  plaintext documentation, `contextSupport`; not advertised: insertReplace, itemDefaults,
  labelDetails. Cost: item ranges lie on the request line, which is materialized once per reply,
  with conversions memoized per position; items decode one at a time from a `Vec<Value>` and
  failures are skipped; snippet lowering and `to_plain` run only for items that pass `matches`.
- **D14 — sessions.** A session holds `{ word_start, caret, len, prefix, items, incomplete }`, in
  request coordinates. A request continues it only if all hold: `Typed` + `Continuing`; same word
  start; `caret >= session.caret`; the text still begins with `prefix`;
  `snapshot.len() − session.len == caret − session.caret`; `caret − session.caret <= 32`. Reuse
  answers a complete list locally (pre-filter with `matches`, then clone; shift `end += delta`, and
  `start` only if `start > old caret`). An incomplete list re-requests with
  `TriggerForIncompleteCompletions`. When the session's own request is in flight, a continuing
  request updates that entry's `latest_ticket`/`latest_caret` instead; its reply converts against
  `request_snapshot`, is stored, and answers through reuse under `latest_ticket`; an incomplete reply
  whose caret moved also re-requests.

## 4. Step-by-step changes

Module layout additions:

```
crates/scrive-lsp/src/
├── completion.rs     completion::{Query, Session, Candidate, Reply, convert} (crate-private)
├── snippet.rs        snippet::lower (crate-private)
└── markdown.rs       markdown::{to_plain, documentation} (crate-private)
```

### 4.1 `lib.rs`

```rust
mod completion;
mod markdown;
mod snippet;
```

All three are private: nothing in them is public API. Extend the crate doc's "where do I…" list
with completion.

### 4.2 `update.rs`

```rust
pub enum Change {
    /// The document's full diagnostic set, replacing the previous one.
    Diagnostics(Vec<Diagnostic>),
    /// Completion items for the ticket's request; empty closes the popup.
    Completions(Vec<scrive_core::CompletionItem>),
}
```

### 4.3 `client/capabilities.rs`

In `client()`, add to `TextDocumentClientCapabilities`:

```rust
completion: Some(CompletionClientCapabilities {
    completion_item: Some(CompletionItemCapability {
        snippet_support: Some(true),
        // Items render documentation as plain text; markdown is lowered if a server ignores this.
        documentation_format: Some(vec![MarkupKind::PlainText]),
        ..CompletionItemCapability::default()
    }),
    context_support: Some(true),
    ..CompletionClientCapabilities::default()
}),
```

Leave `insert_replace_support`, `label_details_support` and `completion_list` (itemDefaults) unset.

In `Server`:

```rust
/// The server's completion trigger strings; `None` when it has no completion provider.
pub(crate) completion: Option<Vec<String>>,
```

set in `Server::new` from
`capabilities.completion_provider.as_ref().map(|options| options.trigger_characters.clone().unwrap_or_default())`.

### 4.4 `client.rs` — the pending machinery

New private types:

```rust
use scrive_core::intel::completion::Start;   // path per Phase 2
use scrive_core::{Bias, CompletionRequest, CompletionTrigger, Ticket};
use lsp_types::error_codes::{CONTENT_MODIFIED, REQUEST_CANCELLED, SERVER_CANCELLED};
use lsp_types::{CompletionContext, CompletionParams, CompletionTriggerKind, TextDocumentPositionParams};

use crate::completion;

/// One request in flight. The table holds at most one per (document, [`Kind`]).
#[derive(Debug)]
struct Pending {
    id: message::Id,
    doc_id: DocId,
    /// What the request's positions were computed against; the reply converts against it.
    request_snapshot: Snapshot,
    /// The newest editor ticket this entry answers. A continuation moves it forward.
    latest_ticket: Ticket,
    /// The caret that goes with `latest_ticket`.
    latest_caret: u32,
    query: Query,
    /// The ticket this request was re-issued for after `ContentModified`; inherited, so a
    /// re-issue happens at most once per ticket.
    reissued_for: Option<Ticket>,
}

/// Which kind of request an entry is — the table's second key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Completion,
}

/// What an entry asked, with what its reply needs.
#[derive(Debug)]
enum Query {
    Completion(completion::Query),
}

impl Query {
    fn kind(&self) -> Kind {
        match self {
            Query::Completion(_) => Kind::Completion,
        }
    }
}
```

`Kind` is compared with `==`, never pattern-matched with a wildcard: `matches!(query, Query::Completion(_))`
expands to a `match` with a `_ => false` arm that is unreachable while `Query` has one variant, and
`-D warnings` would reject it. Phase 7 adds `Kind::{Signature, Hover}` and the matching `Query` arms.

`Client` gains `pending: Vec<Pending>` (initialized empty in `Builder::build`). `Tracked` gains
`session: Option<completion::Session>` (initialized `None` in `open`).

**Decision:** the table is a `Vec`, scanned linearly. Supersession bounds it to (open documents ×
kinds), a handful of entries.

Outgoing requests and supersession:

```rust
impl Client {
    /// Sends `query` for `doc_id`, cancelling the entry it supersedes.
    fn send(&mut self, doc_id: DocId, snapshot: &Snapshot, ticket: Ticket, caret: u32, query: Query, reissued_for: Option<Ticket>) -> Output {
        let mut output = Output::default();
        if let Some(index) = self.pending.iter().position(|p| p.doc_id == doc_id && p.query.kind() == query.kind()) {
            output.messages.push(cancel(self.pending.swap_remove(index).id));
        }
        let Some(tracked) = self.tracked.iter().find(|t| t.doc_id == doc_id) else { return output };
        let id = self.next_request();
        output.messages.push(Message::Request(query.request(id.clone(), tracked.key.uri(), self.encoding, snapshot)));
        self.pending.push(Pending { id, doc_id, request_snapshot: snapshot.clone(), latest_ticket: ticket, latest_caret: caret, query, reissued_for });
        output
    }
}

impl Query {
    /// The request message for this query at `snapshot`.
    fn request(&self, id: message::Id, uri: &Uri, encoding: Encoding, snapshot: &Snapshot) -> message::Request {
        match self {
            Query::Completion(query) => message::Request::new::<lsp_types::request::Completion>(id, CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position: encoding.position(snapshot, query.word.end),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: Some(query.context.clone()),
            }),
        }
    }
}

/// `$/cancelRequest` for `id`. Built by hand: lsp-types' `CancelParams` holds an `i32` id,
/// narrower than [`message::Id`].
fn cancel(id: message::Id) -> Message {
    Message::Notification(message::Notification {
        method: "$/cancelRequest".to_owned(),
        params: Some(serde_json::json!({ "id": id })),
    })
}
```

`self.next_request()` needs `&mut self` while `tracked` borrows `self.tracked`; take the id before
looking up `tracked`, or clone the `Uri`, whichever reads cleaner.

Local answers:

```rust
impl Output {
    /// A ticket-stamped change for one document, with nothing to send.
    fn answer(doc_id: DocId, ticket: Ticket, change: update::Change) -> Self {
        Self { messages: Vec::new(), updates: vec![Update::Document(update::Document::new(doc_id, update::Stamp::Ticket(ticket), change))] }
    }

    /// Appends `other`'s messages and updates after this output's.
    fn append(&mut self, other: Output) {
        self.messages.extend(other.messages);
        self.updates.extend(other.updates);
    }
}
```

Replies. Phase 5's `responded` ends with `_ => Ok(Output::default())`; replace that arm with
`_ => self.settled(id, response.result)`:

```rust
impl Client {
    /// Routes the reply to a pending request.
    fn settled(&mut self, id: message::Id, result: Result<Value, message::Error>) -> Result<Output, Error> {
        let Some(index) = self.pending.iter().position(|p| p.id == id) else {
            return Ok(Output::default());
        };
        let entry = self.pending.swap_remove(index);
        let Some(synced) = self.tracked.iter().find(|t| t.doc_id == entry.doc_id).map(|t| t.synced.revision()) else {
            return Ok(Output::default());
        };
        // Dropped here and never in `sync`: `sync_lsp` syncs before it dispatches, so a
        // continuation moves `latest_ticket` forward only *after* the sync. Only at reply time is
        // it certain that no newer request adopted this entry.
        if entry.latest_ticket.revision() != synced {
            return Ok(Output::default());
        }
        match result {
            Err(error) if error.code == REQUEST_CANCELLED || error.code == SERVER_CANCELLED => Ok(Output::default()),
            Err(error) if error.code == CONTENT_MODIFIED => Ok(self.reissue(entry)),
            Err(_) => Ok(self.failed(entry)),
            Ok(value) => self.resolved(entry, value),
        }
    }

    /// Sends `entry`'s query again, at most once per ticket.
    fn reissue(&mut self, entry: Pending) -> Output {
        if entry.reissued_for == Some(entry.latest_ticket) {
            return Output::default();
        }
        match entry.query {
            Query::Completion(query) => {
                let word = query.word.start..entry.latest_caret;
                let context = query.at(entry.latest_caret);
                self.request_completion(entry.doc_id, entry.latest_ticket, word, context, Some(entry.latest_ticket))
            }
        }
    }

    /// Settles the editor's slot after a server error on a non-command request.
    fn failed(&mut self, entry: Pending) -> Output {
        match entry.query {
            Query::Completion(_) => Output::answer(entry.doc_id, entry.latest_ticket, update::Change::Completions(Vec::new())),
        }
    }

    fn resolved(&mut self, entry: Pending, value: Value) -> Result<Output, Error> {
        match &entry.query {
            Query::Completion(query) => {
                let query = query.clone();
                self.completed(&entry, &query, value)
            }
        }
    }
}
```

The ContentModified condition "the entry's `latest_ticket.revision` is still the synced revision"
is guaranteed by the drop check above it, so only the `reissued_for` test remains in `reissue`.

**Decision:** a completion request that fails with any other server error answers
`Completions(vec![])` under the latest ticket. D12 says what command and signature errors do but
not completion; an empty list settles the editor's awaited slot the same way a decline does.

**Decision:** a re-issue whose caret moved since the original request is sent as `Invoked`; one at
the same caret keeps the original context (`completion::Query::at`). A trigger character describes
the original keystroke, not the new caret.

`close` (Phase 5) gains the cancels, sent before `didClose`:

```rust
let mut output = Output::default();
self.pending.retain(|p| {
    let keep = p.doc_id != doc_id;
    if !keep {
        output.messages.push(cancel(p.id.clone()));
    }
    keep
});
// then remove `tracked` (its session goes with it) and push `didClose` as before
```

`shutdown()` on `Running` clears `self.pending` without sending cancels (D20: `shutdown()` sends
only `shutdown`). Their replies then hit the unknown-id path and are silent.

**Decision:** `shutdown()` forgets pending requests silently. After it, every client call declines,
so no reply could be delivered anyway.

### 4.5 `client.rs` — `Client::complete`

```rust
/// The completion reach: a continuing request more than this many bytes past the session's caret
/// supersedes instead, so a stale list is not refiltered forever.
const CONTINUATION_REACH: u32 = 32;
```

(Put the constant in `completion.rs`, next to `Session::continues`, which is its only reader.)

```rust
impl Client {
    /// Answers the editor's completion request: locally from a reusable list, by adopting it into
    /// an in-flight request, or with a new `textDocument/completion`.
    pub fn complete(&mut self, snapshot: &Snapshot, request: &CompletionRequest) -> Output {
        let doc_id = snapshot.doc_id();
        let ticket = request.ticket();
        let Some(tracked) = self.tracked.iter().find(|t| t.doc_id == doc_id) else {
            return Output::default();
        };
        // A request from another revision cannot be answered correctly; the editor has moved on.
        if ticket.revision() != snapshot.revision() || snapshot.revision() != tracked.synced.revision() {
            return Output::default();
        }
        let decline = || Output::answer(doc_id, ticket, update::Change::Completions(Vec::new()));
        let State::Running(server) = &self.state else { return decline() };
        let Some(triggers) = &server.completion else { return decline() };
        let word = request.word();
        let mut context = match request.trigger() {
            CompletionTrigger::TriggerChar(_) => match matched_trigger(snapshot, word.end, triggers) {
                Some(trigger) => CompletionContext {
                    trigger_kind: CompletionTriggerKind::TRIGGER_CHARACTER,
                    trigger_character: Some(trigger),
                },
                None => return decline(),
            },
            CompletionTrigger::Typed(_) | CompletionTrigger::Manual => {
                CompletionContext { trigger_kind: CompletionTriggerKind::INVOKED, trigger_character: None }
            }
        };
        if let Some(session) = tracked.session.as_ref().filter(|s| s.continues(snapshot, request)) {
            if let Some(entry) = self.pending.iter_mut().find(|p| p.doc_id == doc_id && p.query.kind() == Kind::Completion) {
                entry.latest_ticket = ticket;
                entry.latest_caret = word.end;
                return Output::default();
            }
            if !session.incomplete() {
                return Output::answer(doc_id, ticket, update::Change::Completions(session.answer(snapshot, word.end)));
            }
            context = CompletionContext {
                trigger_kind: CompletionTriggerKind::TRIGGER_FOR_INCOMPLETE_COMPLETIONS,
                trigger_character: None,
            };
        }
        self.request_completion(doc_id, ticket, word, context, None)
    }

    /// Starts a new session at `word` and sends its request at the synced snapshot (the same
    /// revision as the editor's, which every caller has checked).
    fn request_completion(&mut self, doc_id: DocId, ticket: Ticket, word: Range<u32>, context: CompletionContext, reissued_for: Option<Ticket>) -> Output {
        let Some(tracked) = self.tracked.iter_mut().find(|t| t.doc_id == doc_id) else {
            return Output::default();
        };
        let snapshot = tracked.synced.clone();
        tracked.session = Some(completion::Session::new(&snapshot, word.clone()));
        let caret = word.end;
        self.send(doc_id, &snapshot, ticket, caret, Query::Completion(completion::Query { word, context }), reissued_for)
    }

    fn completed(&mut self, entry: &Pending, query: &completion::Query, value: Value) -> Result<Output, Error> {
        let reply = completion::Reply::decode(value)
            .map_err(|source| Error::Decode { method: "textDocument/completion".to_owned(), source })?;
        let incomplete = reply.incomplete();
        let candidates = completion::convert(self.encoding, &entry.request_snapshot, query.word.clone(), reply);
        let Some(tracked) = self.tracked.iter_mut().find(|t| t.doc_id == entry.doc_id) else {
            return Ok(Output::default());
        };
        let Some(session) = tracked.session.as_mut() else { return Ok(Output::default()) };
        session.fill(candidates, incomplete);
        let items = session.answer(&tracked.synced, entry.latest_caret);
        let mut output = Output::answer(entry.doc_id, entry.latest_ticket, update::Change::Completions(items));
        if incomplete && entry.latest_caret != query.word.end {
            let context = CompletionContext {
                trigger_kind: CompletionTriggerKind::TRIGGER_FOR_INCOMPLETE_COMPLETIONS,
                trigger_character: None,
            };
            output.append(self.request_completion(entry.doc_id, entry.latest_ticket, query.word.start..entry.latest_caret, context, entry.reissued_for));
        }
        Ok(output)
    }
}

/// The longest registered trigger the text before `caret` ends with.
fn matched_trigger(snapshot: &Snapshot, caret: u32, triggers: &[String]) -> Option<String> {
    let reach = triggers.iter().map(|t| t.len() as u32).max()?;
    let start = snapshot.clip_offset(caret.saturating_sub(reach), Bias::Left);
    let before = snapshot.slice(start..caret);
    triggers.iter().filter(|t| !t.is_empty() && before.ends_with(t.as_str())).max_by_key(|t| t.len()).cloned()
}
```

The borrow of `tracked` (from `self.tracked`) must end before `self.pending.iter_mut()` and
`self.request_completion`; copy what you need (`session.continues(..)`, `session.incomplete()`,
the answer) into locals first. Do not clone the session.

The session in `tracked.session` always belongs to the pending completion entry, if there is one:
`request_completion` replaces both together, and `close` removes both.

**Decision:** `complete` on an unregistered `DocId` returns an empty `Output` (no answer): there is
no document to stamp.

### 4.6 `completion.rs`

```rust
//! Completion replies converted to scrive items, and the session that lets one reply serve the
//! keystrokes that follow it.

use core::ops::Range;
use std::borrow::Cow;
use std::collections::HashMap;

use lsp_types::{CompletionContext, CompletionItemKind, CompletionTextEdit, CompletionTriggerKind, Documentation, InsertTextFormat};
use scrive_core::intel::completion::Start;
use scrive_core::{Bias, CompletionItem, CompletionKind, CompletionRequest, CompletionTrigger, EditOp, InsertText, Snapshot};
use serde_json::Value;

use crate::{markdown, snippet, Encoding};

/// See `Session::continues`.
const CONTINUATION_REACH: u32 = 32;

/// What a pending completion request asked.
#[derive(Clone, Debug)]
pub(crate) struct Query {
    /// The request word; `word.end` is the caret the request was made at.
    pub(crate) word: Range<u32>,
    pub(crate) context: CompletionContext,
}

impl Query {
    /// The context for sending this query again with the caret at `caret`.
    pub(crate) fn at(&self, caret: u32) -> CompletionContext {
        if caret == self.word.end {
            self.context.clone()
        } else {
            CompletionContext { trigger_kind: CompletionTriggerKind::INVOKED, trigger_character: None }
        }
    }
}

/// One reply's items and whether the server will refine them as the word grows.
#[derive(Debug)]
pub(crate) struct Session {
    word_start: u32,
    caret: u32,
    len: u32,
    prefix: String,
    items: Vec<Candidate>,
    incomplete: bool,
}

/// A converted item before the per-answer work (snippet lowering, documentation, range shift).
#[derive(Debug)]
pub(crate) struct Candidate {
    item: CompletionItem,
    /// `item.insert` holds the raw LSP snippet body, lowered only when the item is shown.
    snippet: bool,
    documentation: Option<Documentation>,
}

/// A decoded `textDocument/completion` result, items still undecoded.
#[derive(Debug)]
pub(crate) struct Reply {
    items: Vec<Value>,
    incomplete: bool,
}
```

`Reply`:

```rust
impl Reply {
    /// `null`, an item array, or a `CompletionList`. Items stay `Value`s so one bad item costs
    /// only itself.
    pub(crate) fn decode(value: Value) -> Result<Self, serde_json::Error> {
        match value {
            Value::Null => Ok(Self { items: Vec::new(), incomplete: false }),
            Value::Array(items) => Ok(Self { items, incomplete: false }),
            Value::Object(mut list) => {
                let incomplete = list.get("isIncomplete").and_then(Value::as_bool).unwrap_or(false);
                match list.remove("items") {
                    Some(Value::Array(items)) => Ok(Self { items, incomplete }),
                    _ => Err(serde::de::Error::custom("completion list has no `items` array")),
                }
            }
            _ => Err(serde::de::Error::custom("completion result is not null, an array or a list")),
        }
    }

    pub(crate) fn incomplete(&self) -> bool { self.incomplete }
}
```

`Session`:

```rust
impl Session {
    /// A session for a request at `word`, awaiting its reply. It starts incomplete, so a
    /// continuing request before any reply lands re-requests rather than answering nothing.
    pub(crate) fn new(snapshot: &Snapshot, word: Range<u32>) -> Self {
        Self {
            word_start: word.start,
            caret: word.end,
            len: snapshot.len(),
            prefix: snapshot.slice(word).into_owned(),
            items: Vec::new(),
            incomplete: true,
        }
    }

    pub(crate) fn fill(&mut self, items: Vec<Candidate>, incomplete: bool) {
        self.items = items;
        self.incomplete = incomplete;
    }

    pub(crate) fn incomplete(&self) -> bool { self.incomplete }

    /// Whether `request` only extends this session's word by typing at its caret. The length
    /// check rules out forward deletes and edits at secondary carets, which the other checks
    /// cannot see.
    pub(crate) fn continues(&self, snapshot: &Snapshot, request: &CompletionRequest) -> bool {
        let word = request.word();
        let caret = word.end;
        matches!(request.trigger(), CompletionTrigger::Typed(_))
            && request.start() == Start::Continuing
            && word.start == self.word_start
            && caret >= self.caret
            && caret - self.caret <= CONTINUATION_REACH
            && i64::from(snapshot.len()) - i64::from(self.len) == i64::from(caret - self.caret)
            && snapshot.clip_offset(self.caret, Bias::Left) == self.caret
            && snapshot.slice(self.word_start..self.caret) == self.prefix
    }

    /// The items matching the word typed so far, with ranges moved by the bytes typed since the
    /// request.
    pub(crate) fn answer(&self, snapshot: &Snapshot, caret: u32) -> Vec<CompletionItem> {
        let delta = caret - self.caret;
        let word = snapshot.slice(self.word_start..caret);
        self.items
            .iter()
            .filter(|candidate| candidate.item.matches(&word))
            .map(|candidate| candidate.finish(self.caret, delta))
            .collect()
    }
}
```

The `clip_offset` guard runs before the `slice`, so a session caret that no longer sits on a char
boundary never reaches `slice`. `matches!` on `CompletionTrigger` is fine: it is a multi-variant
scrive-core enum. If `Start` does not derive `PartialEq`, use `matches!(request.start(),
Start::Continuing)`.

`Candidate::finish` and the shift:

```rust
impl Candidate {
    /// The item as shown: snippet lowered, documentation flattened, ranges shifted by `delta`
    /// bytes typed at `caret`.
    fn finish(&self, caret: u32, delta: u32) -> CompletionItem {
        let mut item = self.item.clone();
        if self.snippet {
            if let InsertText::Plain(body) = &self.item.insert {
                item.insert = snippet::lower(body);
            }
        }
        if let Some(documentation) = &self.documentation {
            item = item.with_doc(markdown::documentation(documentation));
        }
        if delta > 0 {
            if let Some(replace) = item.replace.clone() {
                item = item.with_replace(shift(replace, caret, delta));
            }
            if !item.additional.is_empty() {
                let additional = item.additional.iter().map(|op| EditOp::new(shift(op.range.clone(), caret, delta), op.text.clone())).collect();
                item = item.with_additional(additional);
            }
        }
        item
    }
}

/// `range` after `delta` bytes were inserted at `caret`: an end at or past the caret moves, a
/// start moves only when strictly past it (a range starting at the caret keeps its start and
/// grows).
fn shift(range: Range<u32>, caret: u32, delta: u32) -> Range<u32> {
    let start = if range.start > caret { range.start + delta } else { range.start };
    let end = if range.end >= caret { range.end + delta } else { range.end };
    start..end
}
```

For `replace` — which always contains the caret — this is D14's rule exactly (`end += delta`;
`start` only if `start > old caret`). **Decision:** additional edits use the same insertion map.
D14 states the rule for "ranges" without distinguishing them; applied uniformly it leaves an
auto-import above the word untouched and moves one below it, which is what the typed bytes did.

Conversion:

```rust
/// Converts reply items against the request snapshot. Items that fail to decode are skipped.
pub(crate) fn convert(encoding: Encoding, snapshot: &Snapshot, word: Range<u32>, reply: Reply) -> Vec<Candidate> {
    let mut line = Line::new(encoding, snapshot, word.end);
    reply
        .items
        .into_iter()
        .filter_map(|value| serde_json::from_value::<lsp_types::CompletionItem>(value).ok())
        .map(|item| candidate(item, &word, &mut line, encoding, snapshot))
        .collect()
}

fn candidate(item: lsp_types::CompletionItem, word: &Range<u32>, line: &mut Line<'_>, encoding: Encoding, snapshot: &Snapshot) -> Candidate {
    let (text, edit) = match item.text_edit {
        Some(CompletionTextEdit::Edit(edit)) => (edit.new_text, line.span(edit.range)),
        Some(CompletionTextEdit::InsertAndReplace(edit)) => (edit.new_text, line.span(edit.insert)),
        None => (item.insert_text.clone().unwrap_or_else(|| item.label.clone()), None),
    };
    let sort_key = item.sort_text.clone().unwrap_or_else(|| item.label.clone());
    let mut out = CompletionItem::new(item.label, kind(item.kind), InsertText::Plain(text)).with_sort_key(sort_key);
    if let Some(detail) = item.detail {
        out = out.with_detail(detail);
    }
    if let Some(filter) = item.filter_text {
        out = out.with_filter(filter);
    }
    if let Some(replace) = edit.filter(|span| span != word) {
        out = out.with_replace(replace);
    }
    let additional: Vec<EditOp> = item
        .additional_text_edits
        .unwrap_or_default()
        .into_iter()
        .map(|edit| EditOp::new(encoding.span(snapshot, edit.range), edit.new_text))
        .collect();
    if !additional.is_empty() {
        out = out.with_additional(additional);
    }
    match item.command.as_ref().map(|command| command.command.as_str()) {
        Some("editor.action.triggerSuggest") => out = out.with_retrigger(true),
        Some("editor.action.triggerParameterHints") => out = out.with_signature_after(true),
        _ => {}
    }
    Candidate { item: out, snippet: item.insert_text_format == Some(InsertTextFormat::SNIPPET), documentation: item.documentation }
}
```

Reorder field moves as the compiler requires (for example read `insert_text_format` and
`documentation` before `item.label` is moved); the logic above is what must hold. The `_ => {}` arm
is over `Option<&str>`, not an enum we own.

The request line (D13's conversion cost):

```rust
/// The request's line, materialized once per reply. Item edit ranges almost always share one
/// range on this line, so conversions are memoized by character.
struct Line<'a> {
    encoding: Encoding,
    row: u32,
    start: u32,
    caret: u32,
    text: Cow<'a, str>,
    memo: HashMap<u32, u32>,
}

impl<'a> Line<'a> {
    fn new(encoding: Encoding, snapshot: &'a Snapshot, caret: u32) -> Self {
        let point = snapshot.offset_to_point(caret);
        Self { encoding, row: point.row, start: caret - point.col, caret, text: snapshot.line(point.row), memo: HashMap::new() }
    }

    fn offset(&mut self, character: u32) -> u32 {
        if let Some(&offset) = self.memo.get(&character) {
            return offset;
        }
        let offset = self.start + self.encoding.bytes([self.text.as_ref()], character);
        self.memo.insert(character, offset);
        offset
    }

    /// The byte span of `range` when it lies on this line and contains the caret; `None` sends the
    /// item back to the request word.
    fn span(&mut self, range: lsp_types::Range) -> Option<Range<u32>> {
        if range.start.line != self.row || range.end.line != self.row {
            return None;
        }
        let end = self.offset(range.end.character);
        let start = self.offset(range.start.character).min(end);
        (start <= self.caret && self.caret <= end).then_some(start..end)
    }
}
```

`Encoding::bytes` is Phase 4's `pub(crate)` chunk core; one line is one chunk here.

Kind mapping:

```rust
/// Maps LSP's 25 kinds onto scrive's 9 popup categories.
fn kind(kind: Option<CompletionItemKind>) -> CompletionKind {
    match kind {
        Some(CompletionItemKind::KEYWORD) => CompletionKind::Keyword,
        Some(CompletionItemKind::SNIPPET) => CompletionKind::Construct,
        Some(CompletionItemKind::METHOD) => CompletionKind::Method,
        Some(CompletionItemKind::FIELD | CompletionItemKind::PROPERTY) => CompletionKind::Field,
        Some(
            CompletionItemKind::CLASS | CompletionItemKind::INTERFACE | CompletionItemKind::STRUCT
            | CompletionItemKind::ENUM | CompletionItemKind::TYPE_PARAMETER,
        ) => CompletionKind::Type,
        Some(
            CompletionItemKind::VALUE | CompletionItemKind::ENUM_MEMBER | CompletionItemKind::CONSTANT
            | CompletionItemKind::COLOR | CompletionItemKind::UNIT,
        ) => CompletionKind::Value,
        Some(CompletionItemKind::EVENT) => CompletionKind::Event,
        _ => CompletionKind::Symbol,
    }
}
```

**Decision:** the mapping above; everything else (function, variable, module, file, text, …, and
an absent kind) is `Symbol`. scrive has no `Param` source in LSP, so `Param` is never produced.
`CompletionItemKind` derives `PartialEq + Eq`, so its consts are valid patterns.

### 4.7 `snippet.rs`

The input grammar (LSP 3.17 snippet syntax) and what each element lowers to:

```
any         ::= tabstop | placeholder | choice | variable | text
tabstop     ::= '$' int | '${' int '}' | '${' int transform '}'      → `$N` first time, else the mirror text
placeholder ::= '${' int ':' any* '}'                                 → `${N:flat}` first time, else the mirror text
choice      ::= '${' int '|' text (',' text)* '|}'                    → `${N:first}` (same mirror rule)
variable    ::= '$' var | '${' var '}' | '${' var ':' any* '}' | '${' var transform '}'   → default text, or nothing
transform   ::= '/' regex '/' format '/' options                      → dropped
var         ::= [_a-zA-Z] [_a-zA-Z0-9]*
int         ::= [0-9]+
text        ::= escapes `\$` `\}` `\\` (and `\,` `\|` inside a choice); an unescaped `}` ends a nested `any*`
```

Output grammar is scrive's (see `scrive_core::intel::snippet` module docs): `$N`, `${N}`,
`${N:literal}`, `${N|a,b|}`, escapes `\$ \} \\`, a bare `$` not followed by `{` or a digit is
literal, no nesting, no duplicate index. `$0` / index 0 is the final stop.

```rust
//! LSP snippets lowered to scrive's snippet subset: variables become their default (or nothing),
//! a repeated index becomes plain text after its first occurrence, nesting is flattened into the
//! outer default, choices become their first option and transforms are dropped.

use std::collections::HashMap;

use scrive_core::{InsertText, Snippet};

/// Lowers an LSP snippet body. A body that is not a well-formed LSP snippet is inserted raw; one
/// whose lowering scrive still rejects (an index past `u16`) is inserted as its plain text.
pub(crate) fn lower(body: &str) -> InsertText {
    let mut lowering = Lowering { chars: body.chars().collect(), at: 0, seen: HashMap::new(), snippet: String::new(), plain: String::new() };
    if lowering.top().is_err() {
        return InsertText::Plain(body.to_owned());
    }
    match Snippet::parse(&lowering.snippet) {
        Ok(_) => InsertText::Snippet(lowering.snippet),
        Err(_) => InsertText::Plain(lowering.plain),
    }
}

/// The body is not a well-formed LSP snippet.
struct Malformed;

/// One parsed element.
enum Element {
    Text(String),
    Tabstop(u32),
    /// A placeholder or a choice: its index and flattened default.
    Placeholder(u32, String),
    /// A variable's default text (empty when it has none).
    Variable(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Context {
    Top,
    /// Inside `${…:` — an unescaped `}` ends the text.
    Nested,
}

struct Lowering {
    chars: Vec<char>,
    at: usize,
    /// First-occurrence text per index; later occurrences insert it as plain text.
    seen: HashMap<u32, String>,
    /// scrive snippet syntax.
    snippet: String,
    /// The same body with every stop replaced by its text — the fallback.
    plain: String,
}

impl Lowering {
    fn peek(&self) -> Option<char> { self.chars.get(self.at).copied() }
    fn peek_at(&self, ahead: usize) -> Option<char> { self.chars.get(self.at + ahead).copied() }
    fn bump(&mut self) -> Option<char> { let c = self.peek()?; self.at += 1; Some(c) }

    fn top(&mut self) -> Result<(), Malformed> {
        while self.peek().is_some() {
            let element = self.element(Context::Top)?;
            self.emit(element);
        }
        Ok(())
    }

    /// Writes a top-level element to both outputs.
    fn emit(&mut self, element: Element) {
        match element {
            Element::Text(text) | Element::Variable(text) => self.literal(&text),
            Element::Tabstop(index) => match self.seen.get(&index).cloned() {
                Some(mirror) => self.literal(&mirror),
                None => {
                    self.seen.insert(index, String::new());
                    self.snippet.push_str(&format!("${index}"));
                }
            },
            Element::Placeholder(index, text) => match self.seen.get(&index).cloned() {
                Some(mirror) => self.literal(&mirror),
                None => {
                    self.snippet.push_str(&format!("${{{index}:"));
                    escape_into(&mut self.snippet, &text);
                    self.snippet.push('}');
                    self.plain.push_str(&text);
                    self.seen.insert(index, text);
                }
            },
        }
    }

    fn literal(&mut self, text: &str) {
        escape_into(&mut self.snippet, text);
        self.plain.push_str(text);
    }

    /// The flattened text of a nested `any*`, consuming its closing `}`.
    fn nested(&mut self) -> Result<String, Malformed> {
        let mut text = String::new();
        loop {
            match self.peek() {
                None => return Err(Malformed),
                Some('}') => {
                    self.at += 1;
                    return Ok(text);
                }
                Some(_) => {
                    let element = self.element(Context::Nested)?;
                    text.push_str(&self.flatten(element));
                }
            }
        }
    }

    /// An element's text when nested inside another default. Nested indices are not registered:
    /// a flattened stop no longer exists as a stop.
    fn flatten(&self, element: Element) -> String {
        match element {
            Element::Text(text) | Element::Variable(text) | Element::Placeholder(_, text) => text,
            Element::Tabstop(index) => self.seen.get(&index).cloned().unwrap_or_default(),
        }
    }

    fn element(&mut self, context: Context) -> Result<Element, Malformed> {
        match (self.peek(), self.peek_at(1)) {
            (Some('$'), Some(c)) if c.is_ascii_digit() => {
                self.at += 1;
                Ok(Element::Tabstop(self.int()?))
            }
            (Some('$'), Some('{')) => {
                self.at += 2;
                self.braced()
            }
            (Some('$'), Some(c)) if c == '_' || c.is_ascii_alphabetic() => {
                self.at += 1;
                self.name();
                Ok(Element::Variable(String::new()))
            }
            _ => Ok(Element::Text(self.text(context))),
        }
    }

    /// Literal text up to the next element or, nested, the closing `}`. Always consumes at least
    /// one character: a `$` that starts no element is literal.
    fn text(&mut self, context: Context) -> String {
        let mut text = String::new();
        while let Some(c) = self.peek() {
            match c {
                '\\' if matches!(self.peek_at(1), Some('$' | '}' | '\\')) => {
                    text.push(self.chars[self.at + 1]);
                    self.at += 2;
                }
                '}' if context == Context::Nested => break,
                '$' if !text.is_empty() && self.starts_element() => break,
                c => {
                    text.push(c);
                    self.at += 1;
                }
            }
        }
        text
    }

    fn starts_element(&self) -> bool {
        matches!(self.peek_at(1), Some(c) if c == '{' || c == '_' || c.is_ascii_alphanumeric())
    }

    /// After `${`.
    fn braced(&mut self) -> Result<Element, Malformed> {
        if self.peek().is_some_and(|c| c.is_ascii_digit()) {
            let index = self.int()?;
            match self.bump() {
                Some('}') => Ok(Element::Tabstop(index)),
                Some(':') => Ok(Element::Placeholder(index, self.nested()?)),
                Some('|') => Ok(Element::Placeholder(index, self.choice()?)),
                Some('/') => self.transform().map(|()| Element::Tabstop(index)),
                _ => Err(Malformed),
            }
        } else if self.peek().is_some_and(|c| c == '_' || c.is_ascii_alphabetic()) {
            self.name();
            match self.bump() {
                Some('}') => Ok(Element::Variable(String::new())),
                Some(':') => Ok(Element::Variable(self.nested()?)),
                Some('/') => self.transform().map(|()| Element::Variable(String::new())),
                _ => Err(Malformed),
            }
        } else {
            Err(Malformed)
        }
    }

    /// `a,b|}` after `${N|`: the first option.
    fn choice(&mut self) -> Result<String, Malformed> {
        let mut first = String::new();
        let mut in_first = true;
        loop {
            match self.bump() {
                None => return Err(Malformed),
                Some('\\') if matches!(self.peek(), Some('$' | '}' | '\\' | ',' | '|')) => {
                    let c = self.bump().ok_or(Malformed)?;
                    if in_first {
                        first.push(c);
                    }
                }
                Some(',') => in_first = false,
                Some('|') => return if self.bump() == Some('}') { Ok(first) } else { Err(Malformed) },
                Some(c) => {
                    if in_first {
                        first.push(c);
                    }
                }
            }
        }
    }

    /// Skips `regex/format/options}` after `${N/` or `${VAR/`. A format may hold `${1:/upcase}`,
    /// whose `/` must not end the format.
    fn transform(&mut self) -> Result<(), Malformed> {
        self.skip_past('/')?;
        loop {
            match self.bump() {
                None => return Err(Malformed),
                Some('\\') => {
                    self.bump();
                }
                Some('$') if self.peek() == Some('{') => self.skip_past('}')?,
                Some('/') => break,
                Some(_) => {}
            }
        }
        self.skip_past('}')
    }

    fn skip_past(&mut self, end: char) -> Result<(), Malformed> {
        loop {
            match self.bump() {
                None => return Err(Malformed),
                Some('\\') => {
                    self.bump();
                }
                Some(c) if c == end => return Ok(()),
                Some(_) => {}
            }
        }
    }

    fn int(&mut self) -> Result<u32, Malformed> {
        let start = self.at;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.at += 1;
        }
        self.chars[start..self.at].iter().collect::<String>().parse().map_err(|_| Malformed)
    }

    fn name(&mut self) {
        while self.peek().is_some_and(|c| c == '_' || c.is_ascii_alphanumeric()) {
            self.at += 1;
        }
    }
}

/// Escapes the characters scrive's snippet grammar treats as syntax.
fn escape_into(out: &mut String, text: &str) {
    for c in text.chars() {
        if matches!(c, '$' | '}' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
}
```

The `'$' if !text.is_empty() && self.starts_element()` guard is what makes `text` always consume
something: `element` only calls `text` when the current `$` starts no element (or the current
character is not `$`), so the first character is always taken; a later `$` that starts an element
ends the run.

**Decision:** an index that overflows `u32` makes the body malformed (raw fallback); one that fits
`u32` but not scrive's `u16` survives lowering and is caught by the `Snippet::parse` check (plain
fallback). Both are "anything left unparseable falls back to plain text" (D13).

### 4.8 `markdown.rs`

Phase 7 adds `to_hover` to this module and reuses `lines`. Build the line classifier so both can
use it.

```rust
//! Server markdown lowered to what scrive renders: plain text for completion and signature docs
//! (and, from `to_hover`, scrive's hover subset).

use lsp_types::{Documentation, MarkupContent, MarkupKind};

/// One source line after block-level lowering.
enum Line<'a> {
    /// A line inside a fenced code block, verbatim.
    Code(&'a str),
    /// Any other kept line.
    Text(&'a str),
}

/// Classifies lines: fence lines (```` ``` ```` or `~~~`, with any info string) are dropped and
/// toggle code; thematic breaks (`---`, `***`, `___`, three or more, spaces allowed) outside code
/// are dropped. An unterminated fence runs to the end.
fn lines(markdown: &str) -> impl Iterator<Item = Line<'_>> {
    let mut fence: Option<char> = None;
    markdown.lines().filter_map(move |line| {
        let trimmed = line.trim_start();
        let opener = ['`', '~'].into_iter().find(|&c| trimmed.starts_with(&c.to_string().repeat(3)));
        match (fence, opener) {
            (None, Some(c)) => { fence = Some(c); None }
            (Some(open), Some(c)) if open == c => { fence = None; None }
            (Some(_), _) => Some(Line::Code(line)),
            (None, None) if is_rule(trimmed) => None,
            (None, None) => Some(Line::Text(line)),
        }
    })
}

fn is_rule(line: &str) -> bool {
    let marks: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    marks.len() >= 3 && ["-", "*", "_"].iter().any(|m| marks.chars().all(|c| c.to_string() == *m))
}

/// Markdown as plain text: code lines verbatim; in other lines, `**` and backticks removed, links
/// and images reduced to their text, headings' `#`s dropped, and backslash escapes resolved.
/// Single `*` and `_` stay: they are more often literal (`a * b`, `snake_case`, `__init__`) than
/// emphasis.
pub(crate) fn to_plain(markdown: &str) -> String {
    lines(markdown)
        .map(|line| match line {
            Line::Code(code) => code.to_owned(),
            Line::Text(text) => plain_inline(strip_heading(text)),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Completion and signature documentation as plain text. A bare string is plain text per the
/// spec; markdown is lowered.
pub(crate) fn documentation(documentation: &Documentation) -> String {
    match documentation {
        Documentation::String(text) => text.clone(),
        Documentation::MarkupContent(MarkupContent { kind: MarkupKind::PlainText, value }) => value.clone(),
        Documentation::MarkupContent(MarkupContent { kind: MarkupKind::Markdown, value }) => to_plain(value),
    }
}

/// `# Title` → `Title` (1–6 hashes then a space).
fn strip_heading(line: &str) -> &str { … }

/// Inline lowering for plain text.
fn plain_inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if let Some((label, after)) = link(rest) {
            out.push_str(&plain_inline(label));
            rest = after;
            continue;
        }
        match c {
            '\\' => match rest[1..].chars().next() {
                Some(escaped) if escaped.is_ascii_punctuation() => {
                    out.push(escaped);
                    rest = &rest[1 + escaped.len_utf8()..];
                }
                _ => {
                    out.push('\\');
                    rest = &rest[1..];
                }
            },
            '`' => rest = &rest[1..],
            '*' if rest.starts_with("**") => rest = &rest[2..],
            c => {
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    out
}

/// `[label](target)` or `![label](target)` at the start of `text`: the label and the text after
/// the closing `)`. Brackets do not nest.
fn link(text: &str) -> Option<(&str, &str)> {
    let text = text.strip_prefix('!').unwrap_or(text);
    let text = text.strip_prefix('[')?;
    let close = text.find(']')?;
    let after = text[close + 1..].strip_prefix('(')?;
    let end = after.find(')')?;
    Some((&text[..close], &after[end + 1..]))
}
```

The `lines` sketch is compact; write it plainly (a small struct iterator is fine) as long as the
behavior matches the table in §8. `link` must not match a bare `!` that is not followed by `[`:
`strip_prefix('!')` then `strip_prefix('[')?` returns `None`, and the `!` is then emitted as text.

## 5. Files changed

| File | Change |
|---|---|
| `crates/scrive-lsp/src/lib.rs` | `mod completion; mod markdown; mod snippet;`, crate doc |
| `crates/scrive-lsp/src/client.rs` | `Pending`, `Kind`, `Query`, `send`, `cancel`, `settled`, `reissue`, `failed`, `resolved`, `complete`, `request_completion`, `completed`, `matched_trigger`, `Output::{answer, append}`; `close` cancels; `shutdown` forgets; `responded` routes to `settled` |
| `crates/scrive-lsp/src/client/capabilities.rs` | completion capabilities; `Server::completion` |
| `crates/scrive-lsp/src/client/tests.rs` | conversations; `diagnostics` helper gains its panic arm; new `completions`, `request` (mints through a `Counter`), `labels`, `list` and `completion_capabilities` helpers |
| `crates/scrive-lsp/src/update.rs` | `Change::Completions` |
| `crates/scrive-lsp/src/completion.rs` | new, with tests |
| `crates/scrive-lsp/src/snippet.rs` | new, with tests |
| `crates/scrive-lsp/src/markdown.rs` | new (`to_plain`, `documentation`), with tests |

## 6. Tests

### 6.1 Helper changes (`client/tests.rs`)

```rust
use scrive_core::intel::completion::Start;
use scrive_core::intel::ticket::Counter;
use scrive_core::{CompletionItem, CompletionRequest, CompletionTrigger};

/// A completion request for `word` in `doc` at its current revision, under a fresh ticket from the
/// test's `tickets` — the one place completion tests mint (see the note at the top).
fn request(tickets: &mut Counter, doc: &Document, word: core::ops::Range<u32>, trigger: CompletionTrigger, start: Start) -> CompletionRequest {
    CompletionRequest::new(tickets.issue(doc.revision()), word, trigger, start)
}

/// The one place tests read a `Change` (see Phase 5); now two variants.
fn diagnostics(update: &Update) -> (DocId, update::Stamp, Vec<scrive_core::Diagnostic>) {
    let Update::Document(document) = update else { panic!("expected a document update, got {update:?}") };
    match document.change() {
        update::Change::Diagnostics(set) => (document.doc_id(), document.stamp(), set.clone()),
        other => panic!("expected diagnostics, got {other:?}"),
    }
}

fn completions(update: &Update) -> (update::Stamp, Vec<CompletionItem>) {
    let Update::Document(document) = update else { panic!("expected a document update, got {update:?}") };
    match document.change() {
        update::Change::Completions(items) => (document.stamp(), items.clone()),
        other => panic!("expected completions, got {other:?}"),
    }
}

/// Labels, for compact assertions.
fn labels(items: &[CompletionItem]) -> Vec<&str> { items.iter().map(|i| i.label.as_str()).collect() }
```

`request` takes 5 parameters in a test helper; that is acceptable test scaffolding, but if clippy's
`too_many_arguments` threshold is hit, bundle `(trigger, start)`. A test that needs a ticket at a
chosen (stale) revision mints it directly: `tickets.issue(Revision(0))`.

Fixture for the conversations:

- capabilities: `json!({"textDocumentSync": 2, "completionProvider": {"triggerCharacters": [".", "::"]}})`
- document: `"let v = pr"` (bytes 0..10); the request word is `8..10`, the caret 10, the LSP position
  `(0,10)`.
- `open` → `didOpen` v1. Requests are numbered from id 2 (1 was `initialize`).
- reply fixture `LIST`:

  ```rust
  json!({"jsonrpc": "2.0", "id": 2, "result": {"isIncomplete": false, "items": [
      {"label": "print", "textEdit": {"range": {"start": {"line": 0, "character": 8}, "end": {"line": 0, "character": 10}}, "newText": "print"}},
      {"label": "println", "insertText": "println!($0)", "insertTextFormat": 2},
      {"label": "= prim", "filterText": "prim", "textEdit": {"range": {"start": {"line": 0, "character": 6}, "end": {"line": 0, "character": 10}}, "newText": "= prim"}},
      {"label": "other"}
  ]}})
  ```

  `print`'s range equals the word → `replace: None`; `= prim` replaces `6..10` → `Some(6..10)`;
  `other` does not match `pr`.

Typing uses the real document: `doc.edit_grouped(vec![EditOp::insert(10, "i")], GroupingHint::mergeable(OpClass::Type))`,
then `client.sync(&doc.snapshot(), doc.drain_changes())`, then `client.complete(&doc.snapshot(), &request(&mut tickets, &doc, 8..11, CompletionTrigger::Typed('i'), Start::Continuing))`.
Below, "ticket N" means the ticket of the test's N-th request (keep the request and read
`.ticket()`).

### 6.2 Conversations

| Test | Assertion |
|---|---|
| `completion_request_carries_position_and_context` | `Manual` → `{"id":2,"method":"textDocument/completion","params":{"textDocument":{"uri":…},"position":{"line":0,"character":10},"context":{"triggerKind":1}}}` |
| `completion_advertises_snippets_plaintext_docs_and_context` | `initialize` params: `completion.completionItem.snippetSupport == true`, `documentationFormat == ["plaintext"]`, `contextSupport == true`; `insertReplaceSupport`, `labelDetailsSupport`, `completionList` absent |
| `second_request_supersedes_the_first_with_a_cancel` | second `Manual` at the same revision (a second ticket from the test's `Counter`) → messages `[{"method":"$/cancelRequest","params":{"id":2}}, {"id":3,…completion…}]` |
| `reply_to_a_superseded_request_is_silent` | reply to id 2 after the above → `Ok`, no messages, no updates |
| `request_cancelled_and_server_cancelled_replies_are_silent` | error replies with codes −32800 and −32802 (two runs) → empty `Output` |
| `content_modified_is_reissued_once_per_ticket` | −32801 on id 2 → a new request id 3 with the same position and context; −32801 on id 3 → empty `Output` |
| `other_server_errors_answer_an_empty_list` | −32603 → one update `Completions([])` stamped with the request's ticket |
| `completion_declines_before_initialize` | before the initialize answer → `Completions([])` with the ticket, no messages |
| `completion_declines_without_a_provider` | capabilities without `completionProvider` → `Completions([])` |
| `unregistered_trigger_character_declines` | doc `"let v = "`, `TriggerChar(' ')`, word `8..8` → `Completions([])`, no messages |
| `multi_character_trigger_matches_by_suffix` | doc `"std::"`, `TriggerChar(':')`, word `5..5` → request with `"context":{"triggerKind":2,"triggerCharacter":"::"}` |
| `stale_completion_request_is_ignored` | a request whose ticket is `tickets.issue(Revision(0))`, after an edit + sync → empty `Output` (no answer, no request) |
| `complete_list_is_reused_with_shifted_ranges` | after `LIST`, type `i` + continuing request → no messages; one update stamped with ticket 2 whose labels are `["print", "println", "= prim"]`; `print.replace == None`; `= prim`'s `replace == Some(6..11)` |
| `incomplete_list_re_requests_with_trigger_for_incomplete` | `LIST` with `"isIncomplete": true`; type `i` + continuing → a request id 3 at `(0,11)` with `"context":{"triggerKind":3}`, no update |
| `continuation_answers_the_latest_ticket_filtered_to_the_latest_word` | request (id 2) → type `i` + sync + continuing request → **no messages** (no cancel, no request); then `LIST` for id 2 → one update stamped with ticket 2 (the latest), labels filtered by `pri`, `= prim.replace == Some(6..11)` |
| `incomplete_reply_after_the_caret_moved_answers_and_re_requests` | as above but `LIST` is incomplete → the update **and** a request id 3 at `(0,11)` with `triggerKind` 3 |
| `forward_delete_ends_the_session` | doc `"let v = prx"`, word `8..10`, reply; delete `10..11` + sync; continuing request word `8..10` → a new request (length check failed) |
| `typing_past_thirty_two_bytes_supersedes` | after the reply, one edit inserting 32 × `i` + continuing request `8..42` → reused locally (no messages); then one more `i` + continuing request `8..43` (33 past the session caret) → a new request |
| `caret_left_supersedes` | after the reply, delete `9..10` (backspace) + continuing request word `8..9` → a new request |
| `changed_prefix_supersedes` | after the reply, one edit `[8..9 → "q", insert 10 "i"]` (len +1, caret +1) + continuing request `8..11` → a new request |
| `manual_request_never_continues` | two `Manual` requests at one revision → the second sends cancel + request (Ctrl+Space twice) |
| `reply_behind_the_synced_revision_is_dropped_in_receive` | request id 2; insert at 0 + sync; no new request; reply `LIST` for id 2 → empty `Output` |
| `close_cancels_pending_requests` | request id 2; `close` → `[$/cancelRequest {id:2}, textDocument/didClose]` |
| `shutdown_forgets_pending_requests` | request id 2; `shutdown()` → only `shutdown`; reply to id 2 → empty `Output` |

Body sketch:

```rust
/// While a request is in flight, typing that extends the word adopts it: no cancel, no second
/// request, and the reply answers the newest ticket against the newest word.
#[test]
fn continuation_answers_the_latest_ticket_filtered_to_the_latest_word() {
    let mut tickets = Counter::new();
    let mut doc = document("let v = pr");
    let (mut client, _) = running(Client::builder(), completion_capabilities());
    let _ = client.open(&doc.snapshot(), &uri("file:///a.rs"), "rust").expect("opens");
    let first = request(&mut tickets, &doc, 8..10, CompletionTrigger::Typed('r'), Start::Fresh);
    let sent = client.complete(&doc.snapshot(), &first);
    assert_eq!(sent.messages.len(), 1, "the first request goes out");

    doc.edit_grouped(vec![EditOp::insert(10, "i")], GroupingHint::mergeable(OpClass::Type)).expect("types");
    let _ = client.sync(&doc.snapshot(), doc.drain_changes());
    let second = request(&mut tickets, &doc, 8..11, CompletionTrigger::Typed('i'), Start::Continuing);
    let adopted = client.complete(&doc.snapshot(), &second);
    assert!(adopted.messages.is_empty() && adopted.updates.is_empty(), "a continuation sends nothing");

    let output = client.receive(from_server(list(2, false))).expect("reply is accepted");
    let (stamp, items) = completions(&output.updates[0]);
    assert_eq!(stamp, update::Stamp::Ticket(second.ticket()), "the reply answers the latest ticket");
    assert_eq!(labels(&items), ["print", "println", "= prim"], "items are filtered by `pri`");
    let prim = items.iter().find(|i| i.label == "= prim").expect("kept");
    assert_eq!(prim.replace, Some(6..11), "a range containing the caret grows by the typed byte");
}
```

(`list(id, incomplete)` builds the `LIST` fixture; `completion_capabilities()` returns the
capabilities fixture above. Phase 7 adds its own fixture function beside it, so keep the name
specific.)

### 6.3 `completion.rs` unit tests

Use `Document::new(text).expect(..).snapshot()` and `Reply::decode(json!(…))`, then `convert`, then
`Candidate::finish(caret, 0)` (or `Session::answer`) to read the final item.

| Test | Fixture | Assertion |
|---|---|---|
| `clangd_bullet_item_filters_on_filter_text_and_keeps_the_word_range` | doc `"int x;\npri"`, word `7..10`; item `{"label":"•printf(const char *restrict, ...)","kind":3,"detail":"int","filterText":"printf","sortText":"3f2b1c3cprintf","insertTextFormat":2,"textEdit":{"range":(1,0)-(1,3),"newText":"printf(${1:const char *restrict format, ...})"}}` | `matches("pri")` true (via `filter`); `replace == None`; `sort_key == "3f2b1c3cprintf"`; `kind == Symbol`; finished `insert == Snippet("printf(${1:const char *restrict format, ...})")` |
| `additional_text_edits_convert_against_the_request_snapshot` | same doc; `"additionalTextEdits":[{"range":(0,0)-(0,0),"newText":"#include <stdio.h>\n"}]` | `additional == [EditOp::insert(0, "#include <stdio.h>\n")]` |
| `trigger_parameter_hints_command_sets_signature_after` | `"command":{"title":"","command":"editor.action.triggerParameterHints"}` | `signature_after` true, `retrigger` false |
| `trigger_suggest_command_sets_retrigger` | `editor.action.triggerSuggest` | `retrigger` true |
| `undecodable_item_is_skipped` | `[{"label": 5}, {"label": "ok"}]` | one candidate, `ok` |
| `insert_replace_edit_uses_its_insert_range` | doc `"fobar"`, word `0..2`; `"textEdit":{"newText":"foo","insert":(0,0)-(0,2),"replace":(0,0)-(0,5)}` | `replace == None` (insert range is the word) |
| `edit_range_spanning_lines_falls_back_to_the_word` | range `(0,0)-(1,3)` | `replace == None` |
| `edit_range_not_containing_the_caret_falls_back_to_the_word` | range `(1,0)-(1,1)` with caret at 10 | `replace == None` |
| `edit_range_other_than_the_word_is_kept_as_replace` | doc `"a.pr"`, word `2..4`; range `(0,1)-(0,4)` | `replace == Some(1..4)` |
| `missing_text_edit_inserts_insert_text_or_label` | `{"label":"a","insertText":"b"}`, `{"label":"c"}` | inserts `Plain("b")`, `Plain("c")` |
| `sort_text_becomes_the_sort_key_and_defaults_to_the_label` | with/without `sortText` | as named |
| `item_kinds_map_to_scrive_kinds` | table over the §4.6 mapping incl. absent | as mapped |
| `null_array_and_list_replies_decode` | `null`, `[…]`, `{"isIncomplete":true,"items":[…]}`; `{"items":3}` and `"x"` are errors | |
| `finishing_lowers_snippets_and_flattens_documentation` | `{"label":"f","insertText":"f(${1:x})$0","insertTextFormat":2,"documentation":{"kind":"markdown","value":"**Bold** `code`"}}` | `insert == Snippet("f(${1:x})$0")`, `doc == Some("Bold code")` |
| `answer_shifts_ranges_by_the_caret_delta` | session at caret 10, candidate with `replace 6..10` and an additional edit at `0..0` and one at `12..12`; answer at caret 11 | `replace 6..11`, additional `0..0` unchanged, `12..12` → `13..13` |

### 6.4 `snippet.rs` tests

Every row asserts `lower(input) == expected`; `every_lowered_snippet_parses_with_scrive` loops over
all `Snippet` rows and asserts `scrive_core::Snippet::parse(&out).is_ok()`.

| Test | Input | Expected |
|---|---|---|
| `tab_stops_and_final_stop_pass_through` | `foo($1, ${2})$0` | `Snippet("foo($1, $2)$0")` |
| `nested_placeholders_flatten_to_text` | `${1:a${2:b}c}` | `Snippet("${1:abc}")` |
| `mirrors_become_text_after_the_first` | `$1 = $1;` / `${1:x} + $1` | `Snippet("$1 = ;")` / `Snippet("${1:x} + x")` |
| `choices_become_their_first_option` | `${1\|one,two\|}` / `${1\|a\,b,c\|}` | `Snippet("${1:one}")` / `Snippet("${1:a,b}")` |
| `variables_become_their_default_or_nothing` | `${TM_FILENAME:main.rs}` / `$TM_SELECTED_TEXT!` / `${CLIPBOARD}` | `Snippet("main.rs")` / `Snippet("!")` / `Snippet("")` |
| `transforms_are_dropped` | `${1/(.*)/${1:/upcase}/g}x` | `Snippet("$1x")` |
| `escapes_survive_lowering` | `\$x \} \\` / `a}b` / `cost: $` | `Snippet("\\$x \\} \\\\")` / `Snippet("a\\}b")` / `Snippet("cost: \\$")` |
| `unterminated_element_falls_back_to_the_raw_body` | `${1:abc` / `${x` / `${1|a` | `Plain` of the input |
| `lowering_scrive_rejects_falls_back_to_plain_text` | `x${70000:y}z` | `Plain("xyz")` |

(`|` is escaped in the table for Markdown; the Rust strings contain plain `|`.)

### 6.5 `markdown.rs` tests

| Test | Input | `to_plain` |
|---|---|---|
| `fenced_code_keeps_its_lines` | "```rust\nlet x = 1;\n```\nafter" | `"let x = 1;\nafter"` |
| `bold_and_code_markers_are_removed` | "**Note**: use `x`" | `"Note: use x"` |
| `links_and_images_become_their_text` | `"see [docs](http://a) ![logo](l.png)"` | `"see docs logo"` |
| `thematic_breaks_are_dropped` | `"a\n---\nb"`, `"a\n* * *\nb"` | `"a\nb"` |
| `backslash_escapes_become_literals` | `"\\*lit\\* \\_x"` | `"*lit* _x"` |
| `headings_lose_their_hashes` | `"## Title\nbody"` | `"Title\nbody"` |
| `snake_case_underscores_survive` | `"call __init__ or a_b"` | unchanged |
| `documentation_kinds_lower_to_plain_text` | `Documentation::String("**a**")` / plaintext markup `"**a**"` / markdown markup `"**a**"` | `"**a**"` / `"**a**"` / `"a"` |

## 7. Verification

```
cargo test -p scrive-lsp
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
cargo build --workspace --all-targets --target wasm32-unknown-unknown
```

## 8. Spot-check tables

`complete` decision (after the stale check passes):

| Client | Provider | Trigger | Session continues | Completion in flight | Session list | Result |
|---|---|---|---|---|---|---|
| not Running | — | any | — | — | — | `Completions([])` |
| Running | none | any | — | — | — | `Completions([])` |
| Running | yes | `TriggerChar`, no registered suffix | — | — | — | `Completions([])` |
| Running | yes | `TriggerChar`, suffix matches | never | — | — | request, `triggerKind 2` (supersedes) |
| Running | yes | `Manual` | never | — | — | request, `triggerKind 1` (supersedes) |
| Running | yes | `Typed` | no | — | — | request, `triggerKind 1` (supersedes) |
| Running | yes | `Typed` | yes | yes | — | adopt: update `latest_ticket`/`latest_caret`, send nothing |
| Running | yes | `Typed` | yes | no | complete | answer locally, shifted |
| Running | yes | `Typed` | yes | no | incomplete | request, `triggerKind 3` |

`Session::continues` (session: word start 8, caret 10, len 10, prefix `pr`):

| Request | Text now | Continues? | Failing check |
|---|---|---|---|
| `Typed`, `Continuing`, word `8..11` | `let v = pri` | yes | — |
| `Typed`, `Fresh`, `8..11` | `let v = pri` | no | start |
| `Manual`, `Continuing`, `8..11` | `let v = pri` | no | trigger |
| `Typed`, `Continuing`, `7..11` | … | no | word start |
| `Typed`, `Continuing`, `8..9` | `let v = p` | no | caret-left |
| `Typed`, `Continuing`, `8..10` | `let v = pr` minus a char after it | no | length (forward delete) |
| `Typed`, `Continuing`, `8..43` | 33 bytes typed | no | reach (> 32) |
| `Typed`, `Continuing`, `8..42` | 32 bytes typed | yes | — |
| `Typed`, `Continuing`, `8..11` | `let v = qri` | no | prefix |
| `Typed`, `Continuing`, `8..11` | a second caret also typed | no | length |

Reply routing (`settled`):

| Entry found | `latest_ticket.revision == synced` | Result | Output |
|---|---|---|---|
| no | — | any | empty |
| yes | no | any | empty (dropped in `receive`) |
| yes | yes | −32800 / −32802 | empty |
| yes | yes | −32801, `reissued_for != latest` | re-issued request (inherits `reissued_for = latest`) |
| yes | yes | −32801, `reissued_for == latest` | empty |
| yes | yes | other error | `Completions([])` under `latest_ticket` |
| yes | yes | result | `Completions(answer at latest_caret)` under `latest_ticket` (+ re-request if incomplete and the caret moved) |

Range shift (`shift`, caret 10, delta 3):

| Range | After |
|---|---|
| `8..10` | `8..13` |
| `10..10` | `10..13` |
| `6..12` | `6..15` |
| `0..0` | `0..0` |
| `11..12` | `14..15` |

Markdown lowering (`to_plain`):

| Input | Output |
|---|---|
| "```\ncode\n```" | `code` |
| `**b**` | `b` |
| `` `c` `` | `c` |
| `[t](u)` | `t` |
| `![a](u)` | `a` |
| `---` | (line removed) |
| `# H` | `H` |
| `\*` | `*` |
| `a_b`, `__x__` | unchanged |

## 9. What NOT to change

- scrive-core and scrive-iced. If a Phase 2 builder or accessor is missing, stop and report.
- `message.rs`, `encoding.rs`, `uri.rs`, `diagnostics.rs` behavior.
- No signature help, hover, `Kind::{Signature, Hover}`, `to_hover` (Phase 7); no definition,
  rename, format, `Pending::versions` (Phase 8).
- No `Change` variants beyond `Completions`.
- Do not advertise insertReplace, itemDefaults or labelDetails; do not add `resolve`, commit
  characters or `preselect` (out of scope / Risk 11).

## 10. Pitfalls

- **Dead code under `-D warnings`.** Every `pub(crate)` item and every field must be read in this
  phase. `Pending::versions` is Phase 8's; `Kind` has only `Completion`; `Query::at` is read by
  `reissue`; `Reply::incomplete` by `completed`. `Candidate::finish` is private to `completion.rs`
  and read by `Session::answer`.
- **Single-variant enums.** `Kind`, `Query` and (in tests) nothing else have one variant. Compare
  `Kind` with `==`. Match `Query` exhaustively without `_`. Never `matches!(query, Query::Completion(_))`
  or `let Query::Completion(q) = … else`: both warn while the enum has one variant.
- **Test helpers for `Change`.** Only `diagnostics(..)` and `completions(..)` read `Change`; both
  now carry an `other => panic!` arm, which is reachable because `Change` has two variants.
- **Borrow splitting in `complete`/`completed`.** `self.tracked` and `self.pending` are borrowed
  in turn; end each borrow (copy the booleans, compute the answer) before the next. Never clone a
  `Session` or the candidates to satisfy the borrow checker.
- **Error-code constants** are `i64` in lsp-types (`lsp_types::error_codes::CONTENT_MODIFIED`), the
  same type as `message::Error::code`.
- **`CancelParams`** uses `NumberOrString::Number(i32)`; build `$/cancelRequest` from `json!({"id": id})`
  as shown so `message::Id` round-trips.
- **`lsp_types::CompletionItem` vs `scrive_core::CompletionItem`.** Import scrive's; always write
  the lsp one as `lsp_types::CompletionItem`. No aliased imports.
- **Deprecated lsp-types fields.** `lsp_types::CompletionItem::deprecated` is not `#[deprecated]`;
  do not read it anyway (no consumer). `TextDocumentContentChangeEvent::range_length` is set to
  `None` only.
- **`Snapshot::slice` on a non-boundary.** Only `Session::continues` slices at a stored offset; its
  `clip_offset` guard runs first.
- **Snippet parser progress.** Every loop in `Lowering` must consume at least one character per
  iteration; the `text` guard and the `nested` `peek` check are what guarantee it. The table test
  catches hangs only as a timeout, so reason about it.
- **wasm.** No `std::time` (no request timeouts — supersession bounds the table), no threads.
