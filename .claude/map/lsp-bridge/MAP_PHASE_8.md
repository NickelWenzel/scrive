# Phase 8 — scrive-lsp: goto definition, rename, formatting

> **Settled plan points (now stated in MAP_PLAN.md D12/D18).**
> 1. **Rename staleness has two layers.** D12's drop *in `receive`* runs first for every kind, rename
>    included: a rename whose **requesting** document moved is dropped silently
>    (`Ok(Output::default())`; the user can press F2 again). `Error::StaleEdit` fires only for the
>    *other* documents the edit touches.
> 2. **`Pending::versions` holds synced revisions, not LSP versions.** A server-named non-null
>    `TextDocumentEdit.version` is still checked against the tracked LSP version. See the Decision
>    at the end of step 4.
> 3. **`FileEdits::apply(&str) -> String` cannot refuse.** Of two overlapping edits, the first by
>    `(start, end)` wins and the other is skipped; the method's doc says so.

## Prerequisites

- Phases 1–7 are merged and green. Read these **in full** before you write anything:
  `crates/scrive-lsp/src/{lib.rs, client.rs, client/capabilities.rs, client/tests.rs, update.rs,
  encoding.rs, uri.rs, message.rs}`.
- Read MAP_PHASE_5.md §4.5–4.6 and MAP_PHASE_6.md §4.4–4.5. This phase extends the machinery
  those two phases define, and the names below come from them. If Phase 7 renamed anything, use
  the real code's names. Don't add a parallel helper.
- Phase 3 shipped `scrive_core::intel::{definition::DefinitionRequest, rename::RenameRequest,
  format::FormatRequest}` with public fields: `ticket`, `offset`, `new_name`, `tab_size: u32`
  (MAP_PHASE_3.md, "Command requests").
- MAP_PLAN.md: "Key design decisions" (D4, D7, D10, D12, D18, D21), "Constraints", and "Risks" 7
  and 9.

### Names this doc uses from Phases 2–7

| Role | Name | Owner |
|---|---|---|
| LSP range → byte span against a `Snapshot` (clamps, collapses inverted ranges) | `Encoding::span(self, &Snapshot, lsp_types::Range) -> Range<u32>` | Phase 4 §4.7 |
| LSP range → byte span against plain text (CRLF-aware) | `Encoding::text_span(self, &str, lsp_types::Range) -> Range<usize>` | Phase 4 §4.7 |
| Offset → LSP position | `Encoding::position(self, &Snapshot, u32) -> Position` | Phase 4 §4.7 |
| Registered documents | `self.tracked: Vec<Tracked>`, where `Tracked { doc_id, key: uri::Key, language, synced: Snapshot, version: Option<i32>, session }` | Phases 5/6 |
| Connection state and server capabilities | `State::Running(capabilities::Server)`, a struct of `pub(crate)` fields (`open_close`, `change`, `completion`, …) filled in `Server::new(&ServerCapabilities)` | Phases 5–7 |
| Pending table | `self.pending: Vec<Pending>`, where `Pending { id, doc_id, request_snapshot, latest_ticket, latest_caret, query: Query, reissued_for }` | Phase 6 |
| Kinds | private `Kind` (compared with `==`) and `Query` (one variant per kind, carrying what the request and reply need); `Query::kind()`, `Query::request(&self, id, uri, encoding, snapshot) -> message::Request` | Phase 6 (+7) |
| Send and supersede | `fn send(&mut self, doc_id, snapshot, ticket, caret, query, reissued_for) -> Output` | Phase 6 |
| Reply routing | `settled` (drops entries behind the synced revision, handles the cancellation codes) → `reissue(entry)` / `failed(entry)` / `resolved(entry, value)` | Phase 6 |
| A ticketed local answer | `Output::answer(doc_id, ticket, change) -> Output`, and `Output::append(&mut self, other)` | Phase 6 |
| Update bundle | `update::Document::new(doc_id, stamp, change)` (`pub(crate)`) | Phase 5 |
| Errors | `Error::Decode { method: String, source: serde_json::Error }`, `Error::Server { doc_id: Option<DocId>, method: String, error: message::Error }`, `Error::DuplicateUri { uri }` | Phase 5 |
| URI key | `uri::normalize(&Uri) -> uri::Key`; `Key::uri() -> &Uri`, `Key::as_str()`, and `impl Display` | Phase 5 |
| Request envelopes | `message::Request::new::<R: lsp_types::request::Request>(id, params)` | Phase 4 |
| Mint tickets in tests | one `scrive_core::intel::ticket::Counter` per test, `.issue(revision)` (the only minter; no `Ticket::new`) | Phase 2 |
| Client tests | `crates/scrive-lsp/src/client/tests.rs` (`#[cfg(test)] mod tests;` in client.rs) and its fixtures | Phase 5 |

## Goal and exit criteria

`Client::definition`, `Client::rename` and `Client::format` send their requests and turn the replies
into updates. The edits come out hygienic: LF only, trimmed on char boundaries, whole-document
replacements line-diffed, and sorted.

This phase ships:
- `edits.rs`: edit hygiene and a capped Myers line diff;
- `workspace.rs`: `Location`/`LocationLink` decoding, and `WorkspaceEdit` decoding in every accepted
  shape;
- `Change::{Definition, Edits}`, `update::{Target, jump::Open, jump::Unopened, FileEdits}` and
  `Update::FileEdits`;
- `Error::{StaleEdit, Unsupported}`;
- the D18 capabilities.

**Exit.** These tests exist and pass. `cargo test -p scrive-lsp` is green, and clippy, doc and the
wasm build are clean.

*Definition* (client/tests.rs):
- `definition_in_the_same_document_is_a_local_target`
- `definition_in_another_open_document_is_an_open_target`
- `definition_in_an_open_document_that_moved_is_dropped`
- `definition_in_a_document_opened_after_the_request_is_dropped`
- `definition_in_an_unopened_document_is_an_unopened_target`
- `location_link_definition_uses_the_target_selection_range`
- `definition_without_a_provider_declines_with_none`
- `definition_server_error_is_a_server_error`

*Rename* (client/tests.rs):
- `rename_via_document_changes_edits_two_open_documents`
- `rename_via_the_changes_map_edits_two_open_documents`
- `rename_rejects_a_document_that_moved_since_the_request`
- `rename_rejects_a_document_opened_after_the_request`
- `rename_rejects_a_document_closed_since_the_request`
- `rename_rejects_a_version_the_server_did_not_see`
- `rename_rejects_resource_operations`
- `rename_of_an_unopened_document_yields_file_edits`
- `rename_whose_requester_moved_is_dropped_silently`
- `null_rename_result_changes_nothing`

*Format* (client/tests.rs, edits.rs):
- `format_edit_replacing_e_acute_with_e_grave_trims_to_one_char`
- `descending_edit_batch_is_sorted_by_start_then_end`
- `tied_inserts_keep_the_servers_order`
- `crlf_and_lone_cr_in_new_text_become_lf`
- `whole_document_edit_becomes_per_line_hunks`
- `line_diff_past_the_myers_cap_falls_back_to_the_trimmed_edit`
- `line_diff_hunks_rebuild_the_new_text`
- `edit_that_changes_nothing_is_dropped`
- `format_reply_is_stamped_with_the_request_ticket`
- `format_reply_after_an_edit_is_dropped`

*Shapes and helpers* (workspace.rs, update.rs, capabilities.rs):
- `document_changes_decode_per_uri_in_server_order`
- `operations_that_are_all_edits_decode`
- `resource_operations_reject_the_whole_edit`
- `changes_map_decodes_sorted_by_uri`
- `several_edits_for_one_uri_merge_in_order`
- `annotated_edits_and_null_versions_decode`
- `entries_with_unparseable_uris_are_skipped`
- `first_parseable_location_wins`
- `file_edits_apply_keeps_crlf_line_endings`
- `file_edits_apply_skips_an_overlapping_edit`
- `unopened_span_converts_against_the_given_text`
- `initialize_advertises_definition_links_rename_formatting_and_transactional_edits`

## Design decisions implemented

- **D4.** Definition and format results are `update::Document`s stamped `Stamp::Ticket`. Rename
  edits to open documents are stamped `Stamp::Revision` (the target's synced revision). New
  variants: `Change::Definition(Option<update::Target>)`, `Change::Edits(Vec<EditOp>)` and
  `Update::FileEdits(update::FileEdits)`.
- **D7.** Every conversion clamps. A character past the line end goes to the line end. A line at or
  past `line_count` goes to the end of the document. A position inside a char snaps left. An
  inverted range collapses to its end. Disk text goes through the same core.
- **D10.** Versions: `Pending::versions` records where every open document stood (see the
  Decision below: its synced revision) when a
  definition or rename is sent.
- **D12.**
  - One pending entry per (document, kind).
  - An entry whose latest ticket fell behind the synced revision is dropped in `receive`.
  - Local declines answer with the request's ticket: `Definition(None)` when the client isn't
    ready or the server has no definition provider. Rename and format have no awaiting slot, so
    their declines are an empty `Output`.
  - A server error on any of the three commands (other than the cancellations) is
    `Err(Error::Server)`.
- **D17.**
  - `DefinitionRequest { ticket, offset }`, `RenameRequest { ticket, offset, new_name }` and
    `FormatRequest { ticket, tab_size }`.
  - Format always sends `insertSpaces: true`.
- **D18, in full:**
  - **Definition.**
    - The first `Location` wins. For a `LocationLink`, `targetSelectionRange` is used.
    - `Local(span)` is converted against the request snapshot.
    - `Open(jump::Open)` is converted against the target's synced snapshot. It is dropped
      (→ `Definition(None)`) if the target's synced revision moved since the request, or it was
      opened after it.
    - `Unopened(jump::Unopened)` carries the key, the raw range and the encoding, and has
      `span(&str)`.
  - **Format.**
    - Each `newText` gets `\r\n`/`\r` → `\n`.
    - Edits are trimmed on char boundaries.
    - A nearly whole-document edit becomes a line diff: trim the common lines, then Myers with
      `MAX_D = 1000`, falling back to the trimmed edit.
    - The batch is sorted by `(start, end)`.
  - **Rename.**
    - Accepted shapes:
      - `documentChanges` (plain edits, or operations that are all edits);
      - `changes`, sorted by URI;
      - several edits per URI;
      - `AnnotatedTextEdit`;
      - `version: null`.
    - Resource operations → `Error::Unsupported`.
    - A rename whose requesting document moved is dropped silently by D12's drop in `receive`.
    - All-or-nothing for the other touched documents: `Error::StaleEdit` if any moved (synced
      revision), has no recorded revision, closed since the request, or is named at a non-null
      version other than its tracked LSP version.
    - Open documents → `Change::Edits`. Unopened ones → `Update::FileEdits`, with a CRLF-aware
      `apply` that resolves overlaps first-by-`(start, end)`-wins.
  - **Decoding.** Locations and edit entries whose URI fails to parse are skipped.
  - **Advertised capabilities:**
    - `definition.linkSupport`;
    - `rename`, without prepare;
    - `formatting`;
    - `workspaceEdit { documentChanges: true, failureHandling: "transactional" }`, with no
      resource operations.
- **D21.** `StaleEdit` and `Unsupported` are `Err`: there is nothing to send.

Decisions this doc makes where the plan is open:

- **Decision: "nearly whole-document" means exactly one edit that starts at offset 0 and ends at
  or past the start of the last line.** This catches the `{0:0 → N:0}` and `{0:0 → last:len}`
  shapes that formatters send, with no heuristics on size.
- **Decision: hunks from the line diff also go through the char-boundary trim.** It is the same
  pipeline step, so a whitespace-only change lands as a whitespace-only op and carets on the line
  stay put.
- **Decision: `Pending::versions` is `Vec<(uri::Key, Revision)>`, keyed by `uri::Key`, holding
  each open document's synced revision.** The key survives `close`, like the D10 high-water mark,
  which is what lets rename reject a document that was open at request time and has since closed
  (its unsaved text is gone, so disk isn't what the server saw). Why a revision rather than the
  LSP version: see the end of step 4.
- **Decision: a `TextDocumentEdit` whose non-null `version` differs from the document's tracked LSP
  version (`Tracked::version`) is stale** (`Error::StaleEdit`). The server named the version it edited. If entries merged for
  one URI disagree on a non-null version, that is stale too.
- **Decision: any malformed entry other than a bad URI fails the whole decode** (`Error::Decode`).
  All-or-nothing extends to decoding: a partly decoded rename is a partial rename.
- **Decision: `workspace.rs` owns `Location`/`LocationLink` decoding as well as `WorkspaceEdit`.**
  Both are results that name documents by URI. `client.rs` keeps the routing.
- **Decision: `update::jump` is an inline `pub mod jump { … }` in update.rs.** The plan's files
  table has no `update/jump.rs`.
- **Decision: a null or empty format result produces no update.** Format has no awaiting slot to
  settle.
- **Decision: `Unopened::span(text)` expects the LF text the editor will hold.** The host loads the
  file into a `CodeEditor` (which normalizes to LF) and selects the span there.
- **Decision: `FileEdits::apply` mirrors `Buffer::new` for line endings.** The text is CRLF iff it
  contains `"\r\n"`. `\r\n` and lone `\r` are normalized to `\n`, the edits are applied, and the
  result is re-joined with `\r\n` for CRLF text.

## Step-by-step changes

### 1. `crates/scrive-lsp/src/update.rs`: new variants and types

Add to `Change`:

```rust
    /// Where the definition under the requested offset lives, or `None` when there is none (or its
    /// document moved since the request). Stamped with the definition request's ticket.
    Definition(Option<Target>),
    /// A batch of edits, ready for one `edit_grouped` transaction: LF text, char-boundary trims,
    /// sorted by `(start, end)`. Stamped with a format ticket or a rename's revision.
    Edits(Vec<EditOp>),
```

Add to `Update`:

```rust
    /// A rename's edits for a document that isn't open. The host applies them to the file on disk.
    FileEdits(FileEdits),
```

New types, in this order after `Change`:

```rust
/// Where a definition lives, relative to the requesting document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// In the requesting document: the byte span to select.
    Local(Range<u32>),
    /// In another document open on this client.
    Open(jump::Open),
    /// In a document this client has not opened.
    Unopened(jump::Unopened),
}

/// Definition targets outside the requesting document.
pub mod jump {
    use std::ops::Range;

    use scrive_core::{DocId, Revision};

    use crate::{uri, Encoding};

    /// A definition in another open document, converted against that document's synced snapshot.
    /// Valid only while that document is still at [`revision`](Open::revision).
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Open {
        doc_id: DocId,
        revision: Revision,
        span: Range<u32>,
    }

    impl Open {
        pub(crate) fn new(doc_id: DocId, revision: Revision, span: Range<u32>) -> Self {
            Self { doc_id, revision, span }
        }

        /// The document the definition is in.
        #[must_use]
        pub fn doc_id(&self) -> DocId {
            self.doc_id
        }

        /// The revision [`span`](Open::span) was converted against.
        #[must_use]
        pub fn revision(&self) -> Revision {
            self.revision
        }

        /// The byte span of the definition in that document.
        #[must_use]
        pub fn span(&self) -> Range<u32> {
            self.span.clone()
        }
    }

    /// A definition in a document this client has not opened. The host reads the file, and
    /// [`span`](Unopened::span) converts the server's range against its text.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Unopened {
        uri: uri::Key,
        range: lsp_types::Range,
        encoding: Encoding,
    }

    impl Unopened {
        pub(crate) fn new(uri: uri::Key, range: lsp_types::Range, encoding: Encoding) -> Self {
            Self { uri, range, encoding }
        }

        /// The file the definition is in.
        #[must_use]
        pub fn uri(&self) -> &uri::Key {
            &self.uri
        }

        /// The definition's byte span in `text`, which must be the LF text the editor will hold
        /// (what `CodeEditor::new`/`load` makes of the file). The conversion clamps like every
        /// other one (D7), so a file that changed on disk still yields a valid span.
        #[must_use]
        pub fn span(&self, text: &str) -> Range<u32> {
            let span = self.encoding.text_span(text, self.range);
            span.start as u32..span.end as u32
        }
    }
}

/// A rename's edits for a file that isn't open: the host reads the file, calls
/// [`apply`](FileEdits::apply), and writes the result back.
#[derive(Clone, Debug)]
pub struct FileEdits {
    uri: uri::Key,
    edits: Vec<lsp_types::TextEdit>,
    encoding: Encoding,
}

impl FileEdits {
    pub(crate) fn new(uri: uri::Key, edits: Vec<lsp_types::TextEdit>, encoding: Encoding) -> Self {
        Self { uri, edits, encoding }
    }

    /// The file to edit.
    #[must_use]
    pub fn uri(&self) -> &uri::Key {
        &self.uri
    }

    /// `text` with the edits applied. Line endings survive: text containing `\r\n` comes back
    /// with `\r\n`, as `Buffer::serialize` writes it. Edits go through the same hygiene as an
    /// open document's. Of two overlapping edits, the one that sorts first by `(start, end)` wins
    /// and the other is skipped, since the spec forbids overlaps and a `String` result has no
    /// room to refuse.
    #[must_use]
    pub fn apply(&self, text: &str) -> String {
        let crlf = text.contains("\r\n");
        let lf = if text.contains('\r') {
            Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
        } else {
            Cow::Borrowed(text)
        };
        let ops = edits::hygiene(edits::Text::Str(&lf), self.encoding, &self.edits);
        // `ops` is sorted by `(start, end)`: keep each edit that starts at or after the end of
        // the last kept one.
        let mut end = 0;
        let kept: Vec<EditOp> = ops
            .into_iter()
            .filter(|op| {
                let keep = op.range.start >= end;
                if keep {
                    end = op.range.end;
                }
                keep
            })
            .collect();
        let mut out = lf.into_owned();
        // Descending, so earlier offsets stay valid.
        for op in kept.iter().rev() {
            out.replace_range(op.range.start as usize..op.range.end as usize, &op.text);
        }
        if crlf { out.replace('\n', "\r\n") } else { out }
    }
}
```

`Cow` comes from
`std::borrow::Cow`, and `edits` is `crate::edits`. `Target`, `jump::Open`, `jump::Unopened` and
`FileEdits` are the invariant carriers: fields stay private (OPAQUE N2), and constructors are
`pub(crate)`.

Keep `Change` without `PartialEq`: `scrive_core::Diagnostic` doesn't implement it. Tests match
with `matches!` or through the accessors.

### 2. `crates/scrive-lsp/src/edits.rs` (new): hygiene and the capped line diff

```rust
//! Edit hygiene: server `TextEdit`s become an `edit_grouped`-ready batch.
//!
//! Servers send CRLF inside `newText`, replace whole documents to change three characters, and
//! list edits in any order. Applying that verbatim moves every caret and squiggle in the file and
//! can split a UTF-8 sequence. The pipeline: convert (D7 clamps) → normalize line endings →
//! line-diff a whole-document edit → trim each op to what changes, on char boundaries → drop
//! no-ops → stable sort by `(start, end)`, which keeps the server's order for tied inserts.

use std::borrow::Cow;
use std::ops::Range;

use scrive_core::{EditOp, Point, Snapshot};

use crate::Encoding;

/// The longest line-level edit script the diff computes before falling back to one trimmed
/// edit. Memory is about `MAX_D² / 2` words (4 MB at 1000), and time is `O((N + M) · MAX_D)`.
const MAX_D: usize = 1000;

/// The text edits are converted against: an open document's snapshot, or a file's LF text.
pub(crate) enum Text<'a> {
    Snapshot(&'a Snapshot),
    Str(&'a str),
}

impl Text<'_> {
    fn range(&self, encoding: Encoding, range: lsp_types::Range) -> Range<u32> {
        match self {
            Text::Snapshot(snapshot) => encoding.span(snapshot, range),
            Text::Str(text) => {
                let span = encoding.text_span(text, range);
                span.start as u32..span.end as u32
            }
        }
    }

    fn slice(&self, range: Range<u32>) -> Cow<'_, str> {
        match self {
            Text::Snapshot(snapshot) => snapshot.slice(range),
            Text::Str(text) => Cow::Borrowed(&text[range.start as usize..range.end as usize]),
        }
    }

    fn last_line_start(&self) -> u32 {
        match self {
            Text::Snapshot(snapshot) => snapshot.point_to_offset(Point::new(snapshot.line_count() - 1, 0)),
            Text::Str(text) => text.rfind('\n').map_or(0, |i| i as u32 + 1),
        }
    }
}

/// `edits` as one hygienic batch against `text`.
pub(crate) fn hygiene(text: Text<'_>, encoding: Encoding, edits: &[lsp_types::TextEdit]) -> Vec<EditOp> {
    let mut ops: Vec<EditOp> = edits
        .iter()
        .map(|edit| EditOp::new(text.range(encoding, edit.range), lf(&edit.new_text)))
        .collect();
    if let [op] = ops.as_slice() {
        if op.range.start == 0 && op.range.end >= text.last_line_start() {
            let old = text.slice(op.range.clone());
            ops = line_diff(&old, &op.text, 0);
        }
    }
    let mut ops: Vec<EditOp> = ops
        .into_iter()
        .map(|op| trim(&text.slice(op.range.clone()), op))
        .filter(|op| !(op.range.is_empty() && op.text.is_empty()))
        .collect();
    ops.sort_by_key(|op| (op.range.start, op.range.end)); // stable: ties keep the server's order
    ops
}

/// `\r\n` and lone `\r` as `\n`: the buffer is LF-only, and trimming compares against LF text.
fn lf(text: &str) -> String {
    if text.contains('\r') { text.replace("\r\n", "\n").replace('\r', "\n") } else { text.to_owned() }
}

/// `op` narrowed to the bytes that change. The common prefix and suffix of the replaced text
/// `old` and the new text are cut, backing off to char boundaries in both, so `é`→`è` (which
/// share the lead byte `0xC3`) stays one whole-char replacement.
fn trim(old: &str, op: EditOp) -> EditOp {
    let new = op.text.as_str();
    let mut prefix = old.bytes().zip(new.bytes()).take_while(|(a, b)| a == b).count();
    while !old.is_char_boundary(prefix) || !new.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let (old, new) = (&old[prefix..], &new[prefix..]);
    let mut suffix = old.bytes().rev().zip(new.bytes().rev()).take_while(|(a, b)| a == b).count();
    while !old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix) {
        suffix -= 1;
    }
    let start = op.range.start + prefix as u32;
    EditOp::new(start..op.range.end - suffix as u32, &new[..new.len() - suffix])
}

/// `old` → `new` as line hunks at byte offset `base`. Common leading and trailing lines are cut
/// first, so the Myers pass only sees the changed middle. Past `MAX_D` it returns the middle
/// as one edit.
fn line_diff(old: &str, new: &str, base: u32) -> Vec<EditOp> {
    let a: Vec<&str> = old.split_inclusive('\n').collect();
    let b: Vec<&str> = new.split_inclusive('\n').collect();
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..].iter().rev().zip(b[prefix..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (a_mid, b_mid) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    let mut starts = Vec::with_capacity(a.len() + 1);
    let mut at = base;
    for line in &a {
        starts.push(at);
        at += line.len() as u32;
    }
    starts.push(at);
    let old_at = |line: usize| starts[prefix + line];
    match script(a_mid, b_mid) {
        Some(hunks) => hunks
            .into_iter()
            .map(|hunk| EditOp::new(old_at(hunk.old.start)..old_at(hunk.old.end), b_mid[hunk.new].concat()))
            .collect(),
        None => vec![EditOp::new(old_at(0)..old_at(a_mid.len()), b_mid.concat())],
    }
}

/// One replaced run: old lines `old` become new lines `new` (half-open line indices).
struct Hunk {
    old: Range<usize>,
    new: Range<usize>,
}

/// Myers' greedy shortest edit script over lines, as hunks, or `None` past `MAX_D`. `rows[d]`
/// holds the furthest `x` on diagonals `k = -d, -d+2, …, d` after `d` edits (only diagonals with
/// the parity of `d` are reachable, which halves the trace).
fn script(a: &[&str], b: &[&str]) -> Option<Vec<Hunk>> {
    let (n, m) = (a.len(), b.len());
    let mut rows: Vec<Vec<usize>> = Vec::new();
    for d in 0..=(n + m).min(MAX_D) {
        let mut row = vec![0; d + 1];
        for (i, slot) in row.iter_mut().enumerate() {
            let k = 2 * i as isize - d as isize;
            let mut x = match rows.last() {
                None => 0,
                Some(prev) if down(prev, d, k) => at(prev, d - 1, k + 1),
                Some(prev) => at(prev, d - 1, k - 1) + 1,
            };
            let mut y = (x as isize - k) as usize;
            while x < n && y < m && a[x] == b[y] {
                x += 1;
                y += 1;
            }
            *slot = x;
            if x >= n && y >= m {
                rows.push(row);
                return Some(hunks(&rows, n, m));
            }
        }
        rows.push(row);
    }
    None
}

/// The furthest `x` on diagonal `k` in the row for `d` edits.
fn at(row: &[usize], d: usize, k: isize) -> usize {
    row[((k + d as isize) / 2) as usize]
}

/// Whether diagonal `k` at `d` edits is reached by an insertion (a step down from `k + 1`)
/// rather than a deletion (a step right from `k - 1`).
fn down(prev: &[usize], d: usize, k: isize) -> bool {
    let d = d as isize;
    k == -d || (k != d && at(prev, (d - 1) as usize, k - 1) < at(prev, (d - 1) as usize, k + 1))
}

/// Walk the trace back from `(n, m)` and merge adjacent steps into hunks.
fn hunks(rows: &[Vec<usize>], n: usize, m: usize) -> Vec<Hunk> {
    let (mut x, mut y) = (n, m);
    let mut steps = Vec::with_capacity(rows.len());
    for d in (1..rows.len()).rev() {
        let k = x as isize - y as isize;
        let prev = &rows[d - 1];
        let insert = down(prev, d, k);
        let from_k = if insert { k + 1 } else { k - 1 };
        let from_x = at(prev, d - 1, from_k);
        let from_y = (from_x as isize - from_k) as usize;
        steps.push((from_x, from_y, insert));
        x = from_x;
        y = from_y;
    }
    let mut hunks: Vec<Hunk> = Vec::new();
    for (x, y, insert) in steps.into_iter().rev() {
        let (dx, dy) = if insert { (0, 1) } else { (1, 0) };
        match hunks.last_mut() {
            Some(hunk) if hunk.old.end == x && hunk.new.end == y => {
                hunk.old.end += dx;
                hunk.new.end += dy;
            }
            _ => hunks.push(Hunk { old: x..x + dx, new: y..y + dy }),
        }
    }
    hunks
}
```

The `script`/`hunks`/`trim` bodies above were prototyped and checked by a 3000-case random
property (hunks rebuild the new text, trimmed ops stay disjoint), plus the cap and `é`→`è` rows.
`insert` in `steps` is a tuple field, not a parameter, so the no-raw-`bool` rule doesn't apply.
If you'd rather be explicit, a private `enum Step { Delete, Insert }` is fine.

`Snapshot::point_to_offset` and `Point::new` come from Phase 1. `EditOp::new(range, text)` takes
`impl Into<String>`, so `&str` works.

### 3. `crates/scrive-lsp/src/workspace.rs` (new): URI-bearing result shapes

```rust
//! Results that name documents by URI: definition locations and workspace edits.
//!
//! Both decode by hand from `serde_json::Value` rather than into lsp-types wholesale, because
//! one unparseable URI must skip its entry (D18), and lsp-types' `Uri` fails the whole
//! structure. Everything else stays strict: a rename that half-decoded would be a half rename.

use serde::Deserialize;
use serde_json::Value;

use crate::{uri, Error};

/// The first location in a `textDocument/definition` result whose URI parses: a `Location`,
/// a `Location[]` or a `LocationLink[]` (its `targetSelectionRange`). `None` for `null`, `[]`,
/// or a list with no parseable entry.
pub(crate) fn location(result: Value) -> Option<(uri::Key, lsp_types::Range)> {
    let entries = match result {
        Value::Array(entries) => entries,
        Value::Null => return None,
        single => vec![single],
    };
    entries.into_iter().find_map(|entry| {
        let (uri, range) = if entry.get("targetUri").is_some() {
            (entry.get("targetUri")?, entry.get("targetSelectionRange")?)
        } else {
            (entry.get("uri")?, entry.get("range")?)
        };
        let uri: lsp_types::Uri = uri.as_str()?.parse().ok()?;
        let range = lsp_types::Range::deserialize(range).ok()?;
        Some((uri::normalize(&uri), range))
    })
}

/// A decoded `WorkspaceEdit`: text edits grouped per file. Resource operations never get here.
pub(crate) struct Edit {
    files: Vec<File>,
}

/// One file's edits, with the version the server claims to have edited (`None` for
/// `version: null` and for the `changes` map).
pub(crate) struct File {
    key: uri::Key,
    version: Option<i32>,
    edits: Vec<lsp_types::TextEdit>,
}

/// The wire form of a `TextDocumentEdit`, with the URI kept as a string so a bad one can be
/// skipped. `AnnotatedTextEdit` flattens `TextEdit`'s fields, so it decodes as a `TextEdit`
/// and its `annotationId` is ignored.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Wire {
    text_document: WireDocument,
    edits: Vec<lsp_types::TextEdit>,
}

#[derive(Deserialize)]
struct WireDocument {
    uri: String,
    #[serde(default)]
    version: Option<i32>,
}

impl Edit {
    /// Decode a `textDocument/rename` result. `Ok(None)` for `null`.
    ///
    /// # Errors
    /// [`Error::Unsupported`] if `documentChanges` carries a create, rename or delete
    /// operation. [`Error::StaleEdit`] if two entries for one file claim different non-null
    /// versions. The decode error when anything else is malformed.
    pub(crate) fn decode(result: Value) -> Result<Option<Self>, Error> {
        if result.is_null() {
            return Ok(None);
        }
        let mut files: Vec<File> = Vec::new();
        if let Some(changes) = result.get("documentChanges") {
            let entries = changes.as_array().ok_or_else(|| decode_error("documentChanges is not an array"))?;
            for entry in entries {
                if let Some(kind) = entry.get("kind") {
                    return Err(Error::Unsupported { operation: kind.as_str().unwrap_or("unknown").to_owned() });
                }
                let wire = Wire::deserialize(entry).map_err(decode)?;
                let Ok(uri) = wire.text_document.uri.parse::<lsp_types::Uri>() else { continue };
                merge(&mut files, uri::normalize(&uri), wire.text_document.version, wire.edits)?;
            }
        } else if let Some(changes) = result.get("changes") {
            let map = changes.as_object().ok_or_else(|| decode_error("changes is not an object"))?;
            let mut entries: Vec<(uri::Key, Vec<lsp_types::TextEdit>)> = Vec::with_capacity(map.len());
            for (uri, edits) in map {
                let Ok(uri) = uri.parse::<lsp_types::Uri>() else { continue };
                let edits = Vec::<lsp_types::TextEdit>::deserialize(edits).map_err(decode)?;
                entries.push((uri::normalize(&uri), edits));
            }
            entries.sort_by(|(a, _), (b, _)| a.as_str().cmp(b.as_str()));
            for (key, edits) in entries {
                merge(&mut files, key, None, edits)?;
            }
        }
        Ok(Some(Self { files }))
    }

    pub(crate) fn files(&self) -> &[File] {
        &self.files
    }

    pub(crate) fn into_files(self) -> Vec<File> {
        self.files
    }
}

impl File {
    pub(crate) fn key(&self) -> &uri::Key { &self.key }
    pub(crate) fn version(&self) -> Option<i32> { self.version }
    pub(crate) fn edits(&self) -> &[lsp_types::TextEdit] { &self.edits }
    pub(crate) fn into_parts(self) -> (uri::Key, Vec<lsp_types::TextEdit>) { (self.key, self.edits) }
}

/// Append `edits` to `key`'s file, keeping server order across entries.
fn merge(files: &mut Vec<File>, key: uri::Key, version: Option<i32>, edits: Vec<lsp_types::TextEdit>) -> Result<(), Error> {
    match files.iter_mut().find(|file| file.key == key) {
        Some(file) => {
            match (file.version, version) {
                (Some(a), Some(b)) if a != b => return Err(Error::StaleEdit { uri: key }),
                (None, Some(b)) => file.version = Some(b),
                _ => {}
            }
            file.edits.extend(edits);
        }
        None => files.push(File { key, version, edits }),
    }
    Ok(())
}
```

The two decode helpers wrap Phase 5's `Error::Decode`:

```rust
fn decode(source: serde_json::Error) -> Error {
    Error::Decode { method: "textDocument/rename".to_owned(), source }
}

fn decode_error(message: &str) -> Error {
    decode(serde::de::Error::custom(message))
}
```

`uri::normalize(&Uri) -> uri::Key` is Phase 5's (D6). `uri::Key` derives `Eq + Hash` but not
`Ord`, so sort by `as_str()` as shown.

### 4. `crates/scrive-lsp/src/client.rs`: requests, replies, errors

**a. Errors.** Add to `Error`:

```rust
    /// A rename's edit touches a document whose text moved since the request, opened after it, or
    /// closed since. Nothing is applied.
    #[error("the rename's edit for `{uri}` is stale: the document changed since the request")]
    StaleEdit {
        /// The document that moved.
        uri: uri::Key,
    },
    /// A workspace edit asks for a file operation (create, rename or delete). scrive performs none,
    /// so nothing is applied.
    #[error("unsupported workspace edit: a `{operation}` file operation")]
    Unsupported {
        /// The operation's `kind`.
        operation: String,
    },
```

**b. Kinds, queries, versions.** Extend Phase 6's private types:

```rust
enum Kind {
    Completion,
    Signature,   // Phase 7
    Hover,       // Phase 7
    Definition,
    Rename,
    Format,
}

enum Query {
    // … Phase 6/7 variants …
    /// Goto definition at `offset`.
    Definition { offset: u32 },
    /// Rename the symbol at `offset`.
    Rename { offset: u32, new_name: String },
    /// Format the whole document.
    Format { tab_size: u32 },
}
```

Update `Query::kind()` with the three arms. `Query::request` gets:

```rust
            Query::Definition { offset } => message::Request::new::<lsp_types::request::GotoDefinition>(id, GotoDefinitionParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position: encoding.position(snapshot, *offset),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            }),
            Query::Rename { offset, new_name } => message::Request::new::<lsp_types::request::Rename>(id, RenameParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position: encoding.position(snapshot, *offset),
                },
                new_name: new_name.clone(),
                work_done_progress_params: Default::default(),
            }),
            Query::Format { tab_size } => message::Request::new::<lsp_types::request::Formatting>(id, DocumentFormattingParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                // scrive indents with spaces, so the server must too.
                options: FormattingOptions { tab_size: *tab_size, insert_spaces: true, ..Default::default() },
                work_done_progress_params: Default::default(),
            }),
```

`Pending` gains a field:

```rust
    /// For definition and rename: the synced revision of every open document when the request
    /// went out, keyed by URI. A reply that points into a document which moved since then is
    /// stale. Empty for every other kind.
    versions: Vec<(uri::Key, Revision)>,
```

`send` fills it, so its signature doesn't change:

```rust
        let versions = if matches!(query.kind(), Kind::Definition | Kind::Rename) {
            self.tracked.iter().map(|t| (t.key.clone(), t.synced.revision())).collect()
        } else {
            Vec::new()
        };
        // … push Pending { …, versions }
```

(`matches!` on `Kind` is fine now: `Kind` has more than one variant, so its `_ => false` arm is
reachable.)

**c. Server capabilities.** In `capabilities::Server`, add three fields, each read in this phase:

```rust
    /// Whether the server answers `textDocument/definition`.
    pub(crate) definition: bool,
    /// Whether the server answers `textDocument/rename`.
    pub(crate) rename: bool,
    /// Whether the server answers `textDocument/formatting`.
    pub(crate) formatting: bool,
```

Fill them in `Server::new`:

```rust
        let offered = |provider: Option<&OneOf<bool, _>>| matches!(provider, Some(OneOf::Left(true) | OneOf::Right(_)));
```

(Or write the three `matches!` inline: the three `OneOf`s have different right-hand types, so one
closure can't serve all three. Inline is the honest form.)

**d. Requests.** These follow `complete`'s shape (MAP_PHASE_6.md §4.5). They take the request by
reference, answer `Output::default()` for an unregistered document or a revision mismatch (the
editor has moved on), and decline with the ticket when the client isn't ready or the server has no
provider.

```rust
impl Client {
    /// Ask where the symbol at `request.offset` is defined. The answer is a ticketed
    /// [`Change::Definition`](update::Change::Definition) for the requesting document.
    #[must_use = "the Output's messages must be sent and its updates applied"]
    pub fn definition(&mut self, snapshot: &Snapshot, request: &DefinitionRequest) -> Output {
        let doc_id = snapshot.doc_id();
        let Some(tracked) = self.tracked.iter().find(|t| t.doc_id == doc_id) else { return Output::default() };
        if request.ticket.revision() != snapshot.revision() || snapshot.revision() != tracked.synced.revision() {
            return Output::default();
        }
        let State::Running(server) = &self.state else {
            return Output::answer(doc_id, request.ticket, update::Change::Definition(None));
        };
        if !server.definition {
            return Output::answer(doc_id, request.ticket, update::Change::Definition(None));
        }
        self.send(doc_id, snapshot, request.ticket, request.offset, Query::Definition { offset: request.offset }, None)
    }

    /// Ask the server to rename the symbol at `request.offset` to `request.new_name`. The answer
    /// is edits: [`Change::Edits`](update::Change::Edits) for open documents and
    /// [`Update::FileEdits`] for the rest, or [`Error::StaleEdit`] from `receive` if any open
    /// document moved.
    #[must_use = "the Output's messages must be sent"]
    pub fn rename(&mut self, snapshot: &Snapshot, request: &RenameRequest) -> Output {
        let doc_id = snapshot.doc_id();
        let Some(tracked) = self.tracked.iter().find(|t| t.doc_id == doc_id) else { return Output::default() };
        if request.ticket.revision() != snapshot.revision() || snapshot.revision() != tracked.synced.revision() {
            return Output::default();
        }
        // No awaiting slot to settle: a declined rename just sends nothing.
        let State::Running(server) = &self.state else { return Output::default() };
        if !server.rename {
            return Output::default();
        }
        let query = Query::Rename { offset: request.offset, new_name: request.new_name.clone() };
        self.send(doc_id, snapshot, request.ticket, request.offset, query, None)
    }

    /// Ask the server to format the whole document. The answer is a ticketed
    /// [`Change::Edits`](update::Change::Edits).
    #[must_use = "the Output's messages must be sent"]
    pub fn format(&mut self, snapshot: &Snapshot, request: &FormatRequest) -> Output {
        let doc_id = snapshot.doc_id();
        let Some(tracked) = self.tracked.iter().find(|t| t.doc_id == doc_id) else { return Output::default() };
        if request.ticket.revision() != snapshot.revision() || snapshot.revision() != tracked.synced.revision() {
            return Output::default();
        }
        let State::Running(server) = &self.state else { return Output::default() };
        if !server.formatting {
            return Output::default();
        }
        let caret = snapshot.len(); // unused by a format reply; any in-range offset will do
        self.send(doc_id, snapshot, request.ticket, caret, Query::Format { tab_size: request.tab_size }, None)
    }
}
```

The three preambles repeat `complete`'s. If Phase 7 already pulled that preamble into a private
helper (for `signature_help`/`hover`), use it here. Otherwise leave it inline: see the iced
skill's "Don't extract single-use helpers", which applies to logic. `latest_caret` means nothing to
a format reply. Pass `snapshot.len()` or `0`, whichever Phase 6's field doc permits.

**e. Replies.** Extend Phase 6's routing:

- `reissue` (ContentModified, at most once per ticket): the three command arms send the same
  query again at the synced snapshot, with `reissued_for: Some(entry.latest_ticket)`. This mirrors
  the completion arm.
- **Server errors on commands are `Err`** (D12). Change `settled`'s catch-all error arm so that it
  checks the kind first:

  ```rust
            Err(error) if matches!(entry.query.kind(), Kind::Definition | Kind::Rename | Kind::Format) => Err(Error::Server {
                doc_id: Some(entry.doc_id),
                method: entry.query.method().to_owned(),
                error,
            }),
            Err(_) => Ok(self.failed(entry)),
  ```

  Add `Query::method(&self) -> &'static str`, returning `R::METHOD` per variant. If Phase 7 already
  added an equivalent, use it. `failed` then never sees a command kind. Give its match explicit
  command arms that return `Output::default()`, with a comment saying `settled` routes command
  errors to `Error::Server` first. Don't use a wildcard.
- `resolved` gets the three success arms:

  ```rust
            Query::Definition { .. } => Ok(self.defined(&entry, value)),
            Query::Rename { .. } => self.renamed(&entry, value),
            Query::Format { .. } => self.formatted(&entry, value),
  ```

```rust
impl Client {
    /// A definition result, as the requester's ticketed answer.
    fn defined(&self, entry: &Pending, value: Value) -> Output {
        let target = workspace::location(value).and_then(|(key, range)| self.target(entry, key, range));
        Output::answer(entry.doc_id, entry.latest_ticket, update::Change::Definition(target))
    }

    /// Where `range` in `key` lands relative to the requester. `None` when it points into an
    /// open document that moved since the request, or that opened after it.
    fn target(&self, entry: &Pending, key: uri::Key, range: lsp_types::Range) -> Option<Target> {
        let requester = self.tracked.iter().find(|t| t.doc_id == entry.doc_id)?;
        if key == requester.key {
            return Some(Target::Local(self.encoding.span(&entry.request_snapshot, range)));
        }
        let Some(tracked) = self.tracked.iter().find(|t| t.key == key) else {
            return Some(Target::Unopened(jump::Unopened::new(key, range, self.encoding)));
        };
        let recorded = entry.versions.iter().find(|(k, _)| *k == key).map(|&(_, revision)| revision);
        if recorded != Some(tracked.synced.revision()) {
            return None;
        }
        let span = self.encoding.span(&tracked.synced, range);
        Some(Target::Open(jump::Open::new(tracked.doc_id, tracked.synced.revision(), span)))
    }

    /// A rename result. Every touched open document must still be where it was at the request
    /// before any update is built: the rename lands whole or not at all.
    fn renamed(&self, entry: &Pending, value: Value) -> Result<Output, Error> {
        let Some(edit) = workspace::Edit::decode(value)? else { return Ok(Output::default()) };
        for file in edit.files() {
            let recorded = entry.versions.iter().find(|(k, _)| k == file.key()).map(|&(_, revision)| revision);
            let stale = match self.tracked.iter().find(|t| t.key == *file.key()) {
                // Open now: it must be the text the server saw, at the version it names.
                Some(tracked) => {
                    recorded != Some(tracked.synced.revision())
                        || file.version().is_some_and(|version| Some(version) != tracked.version)
                }
                // Closed since the request: the server edited editor text that no longer exists.
                None => recorded.is_some(),
            };
            if stale {
                return Err(Error::StaleEdit { uri: file.key().clone() });
            }
        }
        let mut output = Output::default();
        for file in edit.into_files() {
            let (key, text_edits) = file.into_parts();
            match self.tracked.iter().find(|t| t.key == key) {
                Some(tracked) => {
                    let ops = edits::hygiene(edits::Text::Snapshot(&tracked.synced), self.encoding, &text_edits);
                    if !ops.is_empty() {
                        output.updates.push(Update::Document(update::Document::new(
                            tracked.doc_id,
                            update::Stamp::Revision(tracked.synced.revision()),
                            update::Change::Edits(ops),
                        )));
                    }
                }
                None => output.updates.push(Update::FileEdits(FileEdits::new(key, text_edits, self.encoding))),
            }
        }
        Ok(output)
    }

    /// A formatting result, as the requester's ticketed edits.
    fn formatted(&self, entry: &Pending, value: Value) -> Result<Output, Error> {
        let text_edits: Option<Vec<lsp_types::TextEdit>> = serde_json::from_value(value)
            .map_err(|source| Error::Decode { method: "textDocument/formatting".to_owned(), source })?;
        let ops = edits::hygiene(edits::Text::Snapshot(&entry.request_snapshot), self.encoding, &text_edits.unwrap_or_default());
        if ops.is_empty() {
            return Ok(Output::default());
        }
        Ok(Output::answer(entry.doc_id, entry.latest_ticket, update::Change::Edits(ops)))
    }
}
```

`settled` has already dropped an entry whose latest ticket fell behind the synced revision. So by
the time `formatted` runs, `request_snapshot` is the synced text, and a rename's requester hasn't
moved. `Output::answer` stamps `Stamp::Ticket`, which is what D4 gives format.

**Decision: `Pending::versions` records the synced *revision*, not the LSP version.** D12 names the
field `versions`, and the check it serves is "moved since the request". `Tracked::version` is
`None` whenever the server was never told (an `openClose: false` or `NONE` server, D9), so version
equality can't see a document move there. The synced revision moves with every `didChange`, and
also in the cases where nothing is sent. The server-named `TextDocumentEdit.version` is still
compared against `Tracked::version`.

### 5. `crates/scrive-lsp/src/client/capabilities.rs`: advertise D18

In the `TextDocumentClientCapabilities` literal:

```rust
            definition: Some(lsp_types::GotoCapability { link_support: Some(true), ..Default::default() }),
            // No `prepareSupport`: prepareRename is out of scope.
            rename: Some(lsp_types::RenameClientCapabilities::default()),
            formatting: Some(lsp_types::DocumentFormattingClientCapabilities::default()),
```

In the `WorkspaceClientCapabilities` literal:

```rust
            // Versioned edits let rename refuse stale documents. No resource operations: create,
            // rename and delete are refused whole (`Error::Unsupported`).
            workspace_edit: Some(lsp_types::WorkspaceEditClientCapabilities {
                document_changes: Some(true),
                failure_handling: Some(lsp_types::FailureHandlingKind::Transactional),
                ..Default::default()
            }),
```

### 6. `crates/scrive-lsp/src/lib.rs`

- Add `mod edits;` and `mod workspace;`. Both are private.
- The crate docs' feature list gains goto-definition, rename and formatting.
- If lib.rs re-exports `update` items at the root, don't add new root re-exports. Hosts write
  `lsp::update::Target`.

## Files changed

| File | Change |
|---|---|
| crates/scrive-lsp/src/update.rs | `Change::{Definition, Edits}`, `Update::FileEdits`, `Target`, `pub mod jump { Open, Unopened }`, `FileEdits` + tests |
| crates/scrive-lsp/src/edits.rs | new: `Text`, `hygiene`, `trim`, `line_diff`, Myers `script`, `MAX_D` + tests |
| crates/scrive-lsp/src/workspace.rs | new: `location`, `Edit`, `File`, `merge` + tests |
| crates/scrive-lsp/src/client.rs | `definition`, `rename`, `format`; `Kind`/`Query` variants, `Query::method`; `Pending::versions` filled in `send`; command errors → `Error::Server`; `defined`/`target`/`renamed`/`formatted`; `Error::{StaleEdit, Unsupported}` |
| crates/scrive-lsp/src/client/tests.rs | the definition, rename and format conversations |
| crates/scrive-lsp/src/client/capabilities.rs | definition/rename/formatting/workspaceEdit client capabilities; `Server::{definition, rename, formatting}` + test |
| crates/scrive-lsp/src/lib.rs | `mod edits; mod workspace;`, crate docs |

## Tests

Every test has a `///` doc stating its invariant and string assert messages.

**Conversation harness (`client/tests.rs`, reusing Phase 5's fixtures).**
- Build two real `Document`s, then `open` them on a client that completed the handshake. The
  initialize result grants `definitionProvider`, `renameProvider`,
  `documentFormattingProvider`, `textDocumentSync: 2` and `positionEncoding: "utf-8"` (so fixture
  columns are bytes). Use one utf-16 fixture for the `é`→`è` row.
- Pull request ids out of outgoing messages with `serde_json::to_value`.
- Mint tickets from one `Counter` per test (`let mut tickets = Counter::new();`,
  `tickets.issue(snapshot.revision())`), as Phases 6–7 do; keep the ticket to compare stamps.
- Reach `Change` variants only through one helper per variant. For example (not named `document`:
  Phase 5's `document(text) -> Document` helper already has that name):

```rust
/// The single `Update::Document` in `output`, as (doc id, stamp, change).
fn sole_update(output: Output) -> (DocId, update::Stamp, update::Change) { /* assert exactly one; into_parts() */ }
```

| Test | Location | Assertion | Body sketch |
|---|---|---|---|
| `definition_in_the_same_document_is_a_local_target` | client/tests.rs | `Change::Definition(Some(Target::Local(3..4)))`, ticket stamp | A = `"fn f() {}\nf();\n"`; `definition(&snap, &DefinitionRequest::new(t, 10))`; reply `{"uri": A, "range": 0:3–0:4}` |
| `definition_in_another_open_document_is_an_open_target` | client/tests.rs | `Target::Open` with `doc_id() == B`, `revision() == B synced`, `span()` = B's `greet` | A calls `greet`, B defines it; reply points into B |
| `definition_in_an_open_document_that_moved_is_dropped` | client/tests.rs | `Definition(None)` | request from A; `sync` B with an edit (new version); reply into B |
| `definition_in_a_document_opened_after_the_request_is_dropped` | client/tests.rs | `Definition(None)` | request from A; `open` B; reply into B |
| `definition_in_an_unopened_document_is_an_unopened_target` | client/tests.rs | `Target::Unopened`, `uri().as_str() == "file:///w/c.rs"`, `span("x\nfn c() {}\n") == 5..6` | reply `{"uri":"file:///w/c.rs","range":1:3–1:4}` |
| `location_link_definition_uses_the_target_selection_range` | client/tests.rs | span = `targetSelectionRange`, not `targetRange` | reply `[{"targetUri":B,"targetRange":0:0–2:1,"targetSelectionRange":0:3–0:8}]` |
| `definition_without_a_provider_declines_with_none` | client/tests.rs | no message sent; `Definition(None)` with the request's ticket | handshake without `definitionProvider` |
| `definition_server_error_is_a_server_error` | client/tests.rs | `Err(Error::Server { method: "textDocument/definition", .. })` | reply `{"error":{"code":-32603,"message":"boom"}}` |
| `rename_via_document_changes_edits_two_open_documents` | client/tests.rs | two `Update::Document`s, `Stamp::Revision` = each synced revision; ops replace `greet` in both | reply `documentChanges` with both URIs and their didOpen versions |
| `rename_via_the_changes_map_edits_two_open_documents` | client/tests.rs | same, updates in URI order (A before B) | reply `changes: {B: [...], A: [...]}` |
| `rename_rejects_a_document_that_moved_since_the_request` | client/tests.rs | `Err(Error::StaleEdit { uri })` with `uri == B`, no updates | request from A; `sync` B edited; reply touches both |
| `rename_rejects_a_document_opened_after_the_request` | client/tests.rs | `Err(StaleEdit)` for B | request from A; `open` B; reply touches B |
| `rename_rejects_a_document_closed_since_the_request` | client/tests.rs | `Err(StaleEdit)` for B | request from A; `close` B; reply touches B |
| `rename_rejects_a_version_the_server_did_not_see` | client/tests.rs | `Err(StaleEdit)` | reply names B at `version + 7` |
| `rename_rejects_resource_operations` | client/tests.rs | `Err(Error::Unsupported { operation: "create" })` | `documentChanges: [edit A, {"kind":"create","uri":"file:///w/n.rs"}]` |
| `rename_of_an_unopened_document_yields_file_edits` | client/tests.rs | one `Update::Document` (A) + one `Update::FileEdits` with `uri() == c.rs`; `apply("fn greet() {}\n") == "fn hail() {}\n"` | reply touches A and `file:///w/c.rs` |
| `rename_whose_requester_moved_is_dropped_silently` | client/tests.rs | `Ok(Output::default())` (no updates, no messages) | request from A; `sync` A edited; reply |
| `null_rename_result_changes_nothing` | client/tests.rs | `Ok`, no updates | reply `result: null` |
| `format_reply_is_stamped_with_the_request_ticket` | client/tests.rs | `Stamp::Ticket(t)`, `Change::Edits([9..11 ""])` | A = `"fn a() {}  \n"`; reply `[{0:9–0:11, ""}]` |
| `format_reply_after_an_edit_is_dropped` | client/tests.rs | `Ok(Output::default())` | request; `sync` A edited; reply |
| `format_edit_replacing_e_acute_with_e_grave_trims_to_one_char` | edits.rs | `[EditOp::new(3..5, "è")]` | `Text::Str("café\n")`, utf-16, `{0:0–0:4, "cafè"}` |
| `descending_edit_batch_is_sorted_by_start_then_end` | edits.rs | `[0..1 "A", 4..5 "C"]` | `"a\nb\nc\n"`, edits `[{2:0–2:1,"C"}, {0:0–0:1,"A"}]` |
| `tied_inserts_keep_the_servers_order` | edits.rs | `[1..1 "X", 1..1 "Y"]` | `"abc"`, `[{0:1–0:1,"X"}, {0:1–0:1,"Y"}]` |
| `crlf_and_lone_cr_in_new_text_become_lf` | edits.rs | `[1..1 "\ny\n"]`; a second edit's `"a\rb"` → `1..1 "\n"` | `"x\n"` insert `"\r\ny\r\n"`; `"ab"` replace all with `"a\rb"` |
| `whole_document_edit_becomes_per_line_hunks` | edits.rs | `[1..3 "", 7..9 ""]` | `"a  \nb\nc  \n"`, `{0:0–3:0, "a\nb\nc\n"}` |
| `line_diff_past_the_myers_cap_falls_back_to_the_trimmed_edit` | edits.rs | exactly one op; applying it yields the new text | 1100 lines `o{i}` → 1100 lines `n{i}` (D = 2200 > 1000) |
| `line_diff_hunks_rebuild_the_new_text` | edits.rs | for 2000 xorshift-seeded pairs of ≤ 8 lines over `a b c é`, with or without a final newline: applying `line_diff` then `trim` rebuilds `new`, and ops are disjoint | seed 7 like scratch.rs `gen_large` |
| `edit_that_changes_nothing_is_dropped` | edits.rs | `vec![]` | `"abc"`, `{0:0–0:3, "abc"}` |
| `document_changes_decode_per_uri_in_server_order` | workspace.rs | keys `[B, A]`, versions `[Some(2), Some(5)]` | `documentChanges: [B v2, A v5]` |
| `operations_that_are_all_edits_decode` | workspace.rs | 2 files | `documentChanges` shaped as operations, none with `kind` |
| `resource_operations_reject_the_whole_edit` | workspace.rs | `Err(Unsupported{"rename"})` and `Err(Unsupported{"delete"})` | one fixture per kind |
| `changes_map_decodes_sorted_by_uri` | workspace.rs | keys `[a, b]`, versions `None` | `changes: {b: [..], a: [..]}` |
| `several_edits_for_one_uri_merge_in_order` | workspace.rs | one file, edits in entry order | two `TextDocumentEdit`s for A |
| `annotated_edits_and_null_versions_decode` | workspace.rs | edit decodes, version `None` | `{"textDocument":{"uri":A,"version":null},"edits":[{"range":..,"newText":"x","annotationId":"a1"}]}` |
| `entries_with_unparseable_uris_are_skipped` | workspace.rs | only the good file | one entry with `"uri":"file:///bad path.rs"` (a space fails fluent-uri) |
| `first_parseable_location_wins` | workspace.rs | the second entry's key | `[{"uri":"file:///bad path.rs",..}, {"uri":B,..}]`; also `null` and `[]` → `None` |
| `file_edits_apply_keeps_crlf_line_endings` | update.rs | `"a\r\nX\r\n"` | text `"a\r\nb\r\n"`, edit `{1:0–1:1, "X"}` |
| `file_edits_apply_skips_an_overlapping_edit` | update.rs | first edit applied, second skipped | `"abcd"`, `[{0:0–0:3,"x"}, {0:1–0:4,"y"}]` → `"xd"` |
| `unopened_span_converts_against_the_given_text` | update.rs | `span("é\nab") == 4..5` under utf-16, range `1:1–1:2` | `Unopened::new(key, range, Encoding::Utf16)` |
| `initialize_advertises_definition_links_rename_formatting_and_transactional_edits` | capabilities.rs | JSON pointers: `/capabilities/textDocument/definition/linkSupport == true`, `/…/rename` is `{}`, `/…/formatting` present, `/capabilities/workspace/workspaceEdit/documentChanges == true`, `failureHandling == "transactional"`, `resourceOperations` absent | serialize the initialize params |

## Verification

```
cargo test -p scrive-lsp
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
cargo build --workspace --all-targets --target wasm32-unknown-unknown
for c in scrive-core scrive-lsp; do out=$(cargo tree -p $c -e normal --prefix none) || exit 1; if grep -qE '^(iced|winit|wgpu)' <<<"$out"; then echo "$c LEAK"; exit 1; fi; done
# wasip1 lib tests need wasmtime; they run in CI. Locally, only if it's installed:
CARGO_TARGET_WASM32_WASIP1_RUNNER=wasmtime cargo test -p scrive-lsp --lib --target wasm32-wasip1
rustfmt --edition 2021 crates/scrive-lsp/src/edits.rs crates/scrive-lsp/src/workspace.rs
```

## Spot-check tables

### Edit hygiene

| Document | Server edits | Encoding | Resulting batch | Why |
|---|---|---|---|---|
| `café\n` | `{0:0–0:4, "cafè"}` | utf-16 | `[3..5 "è"]` | prefix `caf` + lead byte `C3`, backed off to the char boundary |
| `café\n` | `{0:3–0:4, "è"}` | utf-16 | `[3..5 "è"]` | utf-16 col 4 = byte 5 |
| `a\nb\nc\n` | `[{2:0–2:1,"C"}, {0:0–0:1,"A"}]` | any | `[0..1 "A", 4..5 "C"]` | descending batch sorted |
| `abc` | `[{0:1–0:1,"X"}, {0:1–0:1,"Y"}]` | any | `[1..1 "X", 1..1 "Y"]` | stable sort; the rope applies ties descending, so the text reads `aXYbc` |
| `ab` | `[{0:1–0:1,"X"}, {0:1–0:2,"Z"}]` | any | `[1..1 "X", 1..2 "Z"]` | `(start, end)`: insert before replace at the same start |
| `x\n` | `{0:1–0:1, "\r\ny\r\n"}` | any | `[1..1 "\ny\n"]` | CRLF → LF |
| `ab` | `{0:0–0:2, "a\rb"}` | any | `[1..1 "\n"]` | lone CR → LF, then trim |
| `abc` | `{0:0–0:3, "abc"}` | any | `[]` | no-op dropped |
| `a  \nb\nc  \n` | `{0:0–3:0, "a\nb\nc\n"}` | any | `[1..3 "", 7..9 ""]` | whole document → hunks → trimmed |
| `a  \nb\nc  \n` | `{0:0–99:0, "a\nb\nc\n"}` | any | `[1..3 "", 7..9 ""]` | line ≥ `line_count` clamps to the end (D7) |
| `a\nb\n` | `{0:0–0:1, "A"}` | any | `[0..1 "A"]` | ends on line 0, before the last line start (4): not whole-document |

### Myers cap

| Old (lines) | New (lines) | D | Result |
|---|---|---|---|
| 1100 × `o{i}` | same | 0 | `[]` (prefix trim eats everything) |
| 1100 × `o{i}` | every 3rd line → `n{i}` | 734 | per-hunk ops (Myers ran) |
| 1100 × `o{i}` | 1100 × `n{i}` | 2200 | 1 op: the trimmed middle (fallback) |
| 1 line | 1 line, one char changed | 2 | 1 op, trimmed to the char |

### WorkspaceEdit shapes

| Shape | Outcome |
|---|---|
| `documentChanges: [TextDocumentEdit A v5, B v2]`, both current | 2 × `Update::Document(Revision, Edits)` |
| `documentChanges` as operations, none with `kind` | accepted |
| `documentChanges` containing `{"kind":"create"|"rename"|"delete"}` | `Err(Unsupported { operation })` |
| `changes: {B: [...], A: [...]}` | accepted, A then B |
| both `documentChanges` and `changes` | `documentChanges` used, `changes` ignored |
| two entries for A | merged, entry order kept |
| entries for A with versions 5 and 6 | `Err(StaleEdit { A })` |
| `AnnotatedTextEdit` | accepted as a `TextEdit` |
| `version: null` for an open document | accepted if its synced revision still matches `Pending::versions` |
| entry with `file:///bad path.rs` | skipped |
| edit for B, B synced a newer version since | `Err(StaleEdit { B })` |
| edit for B, B opened after the request | `Err(StaleEdit { B })` |
| edit for B, B closed since the request | `Err(StaleEdit { B })` |
| edit naming B at a version ≠ synced | `Err(StaleEdit { B })` |
| edit for a URI never opened | `Update::FileEdits` |
| edit touching A (requester), A moved | dropped by D12 in `receive`: `Ok(Output::default())` |
| `result: null` | `Ok`, no updates |
| `documentChanges: "x"` | `Err(Decode)` |

## What NOT to change

- No scrive-iced or scrive-core files. The glue is Phase 9, and the request types are Phase 3's.
- Don't touch the envelope (`message.rs`), `encoding.rs` semantics or `uri.rs` normalization.
  If a conversion entry point for `&str` is missing, add it to `encoding.rs` as a thin wrapper
  over the existing chunk core, and say so in your report.
- Don't change Phase 6's pending machinery (supersede, cancel, ContentModified, the drop in
  `receive`) beyond adding the three `Query` variants, `versions`, and `settled`'s command-error
  arm (step 4e).
- No `update::Applied`, `Jump` or `Refusal`. They are Phase 9's.
- No `prepareRename`, range formatting, resource operations or `workspace/applyEdit` changes. D21
  keeps answering `applied: false`.
- No new dependencies.

## Pitfalls

- **Char boundaries on both sides.** The prefix can be a boundary in `old` but not in `new`
  (`é`/`è` share `0xC3`). Back off until *both* are boundaries. Do the same for the suffix,
  measured from the ends of the post-prefix slices, so the prefix and suffix can't overlap.
- **Sort after trimming.** Trimming moves `start`. Sorting before trimming can reorder a
  replace ahead of an insert at the same trimmed start.
- **Only diagonals with `d`'s parity exist.** Store `d + 1` entries per row and index
  `(k + d) / 2`. Indexing all `2d + 1` diagonals reads `k − 1` at `d − 1`, which doesn't exist,
  and panics at `d = 1`.
- **Both line lists come from `split_inclusive('\n')`.** Lines compare with their newline, so a
  missing final newline is a real difference, and offsets are prefix sums of line lengths.
- **`Text::Str` slicing** indexes `&str` by byte range. D7's conversion guarantees char
  boundaries, but never slice with an unconverted range.
- **lsp-types `Uri` in a map key fails the whole map.** That's why `changes` is walked as a
  `serde_json::Map` and parsed per key.
- **`DocumentChanges` is `#[serde(untagged)]` in lsp-types.** Decoding it wholesale hides
  which entry was a resource op. Check `kind` per entry.
- **Borrowing in `rename_reply`.** Build all updates from `&self` after the check loop. Don't
  mutate sessions there: the synced snapshot only moves in `sync`.
- **Cast hygiene.** `line.len() as u32` is fine: the document fits `u32` by construction. Don't
  sprinkle `try_into().unwrap()` (no `unwrap()` in library code).
- **`#[must_use]` on the new client methods.** `definition`, `rename` and `format` return an
  `Output` the caller must send. Inside the client, compose `Output`s and don't drop them. The
  local decline paths return one too.
- **No features in scrive-lsp.** Nothing here is `cfg`-gated, so dead code shows up in the one
  default build: every private helper in `edits.rs`/`workspace.rs` needs a caller in this phase.
- **Wasm.** No `std::time`, threads or I/O. Everything here is pure. `HashMap` iteration order
  never reaches output, because `changes` is sorted explicitly.
