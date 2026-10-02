# Phase 6 — scrive-lsp: fetch, refresh, interactions (+ the minimal `land` arms)

This doc implements the Phase 6 entry of `MAP_PLAN.md` (Draft 7): decisions D15–D18 on the client
side, and the part of D19 that keeps `--all-features` building. Everything an implementer needs
from the plan is restated here. Where the plan left a choice open, the doc cites the
RESOLUTIONS.md answer (Rn) that settled it; see "Resolved questions" at the end.

Line numbers are from HEAD `8e72665` (branch `lsp_bridge`), re-verified for this doc. Phases 1–5
move code in scrive-core and scrive-iced, so locate those sites by function or arm name. The
scrive-lsp citations hold until this phase changes them, since no earlier phase touches that crate.

## Prerequisites

- Phases 1–5 are committed, and the tree is green: `cargo test --workspace --all-features`, clippy
  with `-D warnings` in both feature sets, the doc build, and the wasm32 build.
- Read in full before writing code:
  - `.claude/map/lsp-bridge/DISPATCH.md`, with the inlay-hints substitutions from `MAP_PLAN.md`
    Appendix A: the map directory is `.claude/map/inlay-hints/`, patches go to
    `.claude/map/inlay-hints/patches/`, and the `--all-features` clippy run applies to this phase;
  - `~/.claude/guides/RUST_STYLE.md` and `~/.claude/guides/OPAQUE.md`;
  - the `/commit-and-comment` skill, which governs every comment you write (DISPATCH override 1);
  - `crates/scrive-lsp/src/{client.rs, client/capabilities.rs, client/tests.rs, update.rs,
    workspace.rs, edits.rs, encoding.rs, hover.rs, markdown.rs, lib.rs}`;
  - `crates/scrive-iced/src/code_editor/lsp.rs`, plus the Phase 5 parts of
    `crates/scrive-iced/src/code_editor.rs` (`Awaiting`, `Awaited`, `accepts`, `set_inlays`,
    `set_inlay_tooltip` and the inlay scheduler);
  - `crates/scrive-core/src/intel/inlay.rs`, `intel/inlay/request.rs` and
    `intel/inlay/interaction.rs` (Phase 1);
  - lsp-types 0.97 `src/inlay_hint.rs` and `request.rs:832-866`, under
    `~/.cargo/registry/src/*/lsp-types-0.97.0/`.
- **The Phase 1 API this phase consumes** (MAP_PHASE_1 after the audit; R1, R5, R6):

  ```rust
  // scrive_core::intel::inlay (Phase 1)
  pub struct Hint;                              // private fields, immutable once built
  impl Hint {
      pub fn new(kind: Kind, label: Vec<Part>, key: Key) -> Result<Hint, Error>; // rejects a label with no visible text; placement defaults from `kind`
      pub fn padding(self, padding: Padding) -> Hint;                           // builder
      pub fn insert(self, insert: Insert) -> Hint;                              // builder
      pub fn placement(self, placement: Placement) -> Hint;                     // builder (R5)
      pub fn kind(&self) -> Kind;  pub fn parts(&self) -> &[Part];  pub fn padded(&self) -> Padding;
      pub fn insertable(&self) -> bool;  pub fn key(&self) -> Key;  pub fn width(&self) -> u32; // getters (R1)
  }
  pub struct Part;   impl Part { pub fn new(text: impl Into<String>, link: Link) -> Part; pub fn text(&self) -> &str; pub fn link(&self) -> Link; }
  pub enum Kind { Type, Parameter, Other }      // Copy
  pub enum Placement { Suffix, Prefix, Auto }
  pub enum Link { Jumps, None }
  pub enum Insert { Available, Unavailable }
  pub struct Padding { pub left: bool, pub right: bool }   // Default
  pub struct Key;    // Copy + Eq + Hash + Debug; `Key::new(u64)`
  pub struct Placed; impl Placed { pub fn new(offset: u32, hint: Hint) -> Placed; pub fn offset(&self) -> u32; pub fn hint(&self) -> &Hint; } // Clone + Debug
  // intel::inlay::request
  pub struct Request; impl Request { pub fn new(ticket: Ticket, span: Range<u32>) -> Request; pub fn ticket(&self) -> Ticket; pub fn span(&self) -> Range<u32>; }
  // intel::inlay::interaction
  pub struct Interaction; impl Interaction {
      pub fn tooltip(ticket: Ticket, key: Key, part: u32) -> Self;  pub fn jump(ticket: Ticket, key: Key, part: u32) -> Self;
      pub fn insert(ticket: Ticket, key: Key, offset: u32) -> Self;
      pub fn ticket(&self) -> Ticket;  pub fn key(&self) -> Key;  pub fn gesture(&self) -> Gesture;
  }
  pub enum Gesture {                            // Copy
      Tooltip { part: u32 },                    // pointer resting on a label part (padding isn't hoverable)
      Jump { part: u32 },                       // Ctrl+click on a part with a link
      Insert { offset: u32 },                   // double-click on an insertable hint (the offset is the editor's)
  }
  ```

  `update::Change` derives `Clone, Debug`, so `Placed` must be `Clone + Debug`. If it isn't, stop
  and report. If Phase 1's `Hint` doesn't keep the label's parts one to one (it merges, drops or
  reorders parts), stop and report: the part indices in `Interaction` index the server's label
  (Pitfalls).
- **The Phase 5 API the `land` arms call:**
  - `CodeEditor::set_inlays(&mut self, ticket, Option<Vec<inlay::Placed>>)`: `Some` replaces the
    hints, and `Some(vec![])` clears them; `None` settles the slot and keeps what is shown;
  - `CodeEditor::set_inlay_tooltip(&mut self, ticket, Option<String>)`;
  - the `Awaited` variants for the inlay slots, with `accepts` arms for them;
  - `CodeEditor::wait_inlays(&mut self, delay, cap)` (private, reachable from the `lsp` child
    module) and the private const `INLAY_EDIT_DELAY`: the `InlayRefresh` arm calls
    `self.wait_inlays(INLAY_EDIT_DELAY, None)` (R9), which is ignored while hints are disabled.

## Goal and exit criteria

`scrive-lsp` fetches inlay hints for a byte span, converts them to scrive-core's hint model, keeps
the last set per document, forwards the server's refresh requests to every open document, and
answers the three hint gestures: tooltip, label jump and insert. scrive-iced's `land` gains the
three arms the new `update::Change` variants require. Nothing in scrive-iced calls the new client
entry points yet; `sync_lsp` pulls them in Phase 7.

Exit criteria:

1. `cargo test -p scrive-lsp` passes every existing test plus the tests listed in Step 10.
2. `cargo test --workspace` and `cargo test --workspace --all-features` pass.
3. Clippy `-D warnings` passes in both feature sets, both doc builds pass with `-D warnings`, and
   the wasm32 all-features build passes.
4. `update::Change` stays exhaustive (no `#[non_exhaustive]`), and `land` matches it with no `_`
   arm.

### Commit boundaries

Four commits, each green on its own (DISPATCH override 2). Save
`.claude/map/inlay-hints/patches/phase6-<k>.patch` and `.msg` at each boundary.

| k | Subject | Contents | Why it is separate |
|---|---|---|---|
| 1 | `refactor(lsp): resolve definition targets from a snapshot and recorded revisions` | Step 1 only: `target` takes `(requester, &Snapshot, revisions)`; a free `revision_of` | A pure refactor; the existing definition and rename tests are its oracle |
| 2 | `feat(lsp)!: fetch inlay hints` | capabilities (Step 2), `Kind::Inlays`, `Query::Inlays`, `inlay.rs` (decode, convert, keys, the `Set` with only `revision` and `{key, hint}`), `Client::inlays`, `Change::Inlays`, the `Inlays` `land` arm, the fetch tests | Adds one `Change` variant and its arm |
| 3 | `feat(lsp)!: forward inlay hint refreshes to every open editor` | the `respond` arm, the `answer` fan-out, the post-`initialized` fan-out, `Change::InlayRefresh` and its `land` arm, the refresh tests | Adds one `Change` variant and its arm |
| 4 | `feat(lsp)!: answer inlay hint tooltips, label jumps and inserts` | `Kind::Tooltip`, `Query::{Resolve, LocationHover}`, `revisions` recorded for `Inlays`, the `Set`'s `snapshot`, `revisions`, `raw` and `resolved` fields, `Client::interact`, `Change::InlayTooltip` and its `land` arm, the interaction tests | Every field this commit adds is read only by `interact` |

Suggested "why" bodies (1-3 lines each, never narrating the diff):
1. "Label-part jumps resolve targets from a stored inlay set, not from a pending request."
2. "Hints for the visible window, converted to scrive's model, with keys that survive a refetch so
   an open tooltip card stays put."
3. "rust-analyzer's first non-empty hints and its post-edit updates arrive through refreshes;
   servers without refresh support get one after `initialized`."
4. "Tooltips resolve on demand, links jump through the definition path, and text edits insert the
   hint."

Commit 2 must not contain fields that only commit 4 reads. Under `-D warnings` they fail as dead
code, and DISPATCH forbids `#[allow(dead_code)]`. Step 4 says which fields arrive when. If a
boundary can't be made green on its own, merge it into the next one and say so in your report.

## Design decisions implemented

Restated from `MAP_PLAN.md`; these are binding.

**D15 — Capabilities.**
- Advertise `textDocument.inlayHint` as
  `{ dynamicRegistration: false, resolveSupport: { properties: ["tooltip", "label.tooltip"] } }`,
  and `workspace.inlayHint.refreshSupport: true`.
- Read `inlay_hint_provider` into `capabilities::Server.inlay: Option<Resolve>`. It is `Some` for
  `OneOf::Left(true)` or any options object; `Resolve` says whether `resolveProvider` is true.
- Only tooltips are lazy. Locations and text edits come inline: 37% more bytes, but no round
  trips and no stale-resolve trap, and the resolve request covers the tooltip requirement.
- rust-analyzer (probe at `02dede3ce5`) announces `{"resolveProvider": true}` only when the client
  lists a lazily resolvable property, and defers exactly the advertised properties. With default
  settings it sends no tooltips at all, resolved or not.

**D16 — Fetch.** `Client::inlays(&Snapshot, &inlay::Request) -> Output`.
- It applies the usual gates (tracked, revision, capability). A decline answers
  `Change::Inlays(Some(vec![]))` under the ticket; an answer is
  `Change::Inlays(Some(Vec<inlay::Placed>))`.
- `Kind::Inlays` is an intel kind, not a command, so a failure answers `Inlays(None)`.
  ContentModified re-issues once through the existing `reissue`. That re-issue is what gets
  rust-analyzer's first hints: the post-refresh request fails with ContentModified at the end of
  indexing, and the single re-issue succeeds.
- A `null` result (the spec allows it) decodes to `Inlays(Some(vec![]))` and clears. It is not
  `None`; Lapce keeps stale hints on `null`.
- Decoding is per entry. The result decodes as `Vec<serde_json::Value>`, and each entry decodes
  alone, so one malformed hint (a bad location URI) doesn't lose the set. This is the
  `workspace.rs` rule (workspace.rs:1-5, 82-83).
- Entries outside the request span, give or take one line, are dropped before conversion. The
  spec doesn't forbid hints outside the range. Clipping keeps the editor's store bounded by its
  window, a few hundred hints whatever the file size, which the per-row query costs rely on (D4).
- Conversion runs against the request snapshot:
  - positions go through `Encoding::offset`, and hints on a line at or past `line_count` are
    dropped;
  - kinds `1 → Type`, `2 → Parameter`, anything else or absent → `Other`;
  - a part is `Link::Jumps` iff it has a location;
  - `Insert::Available` iff `textEdits` is non-empty;
  - placement (D1, R5): `Hint::new` defaults `Type → Suffix`, `Parameter → Prefix`,
    `Other → Auto`. For `Other` the client applies Zed's padding rule (`hint_position_and_bias`,
    lsp_command.rs:3897-3933 at zed `1399a80`) to the **raw** server flags, before the collapse
    below: right-only padding → `Prefix`, left-only → `Suffix`, symmetric (`false/false`,
    `true/true`) → `Auto`, and passes it with `.placement(..)`. Core resolves `Auto` against the
    buffer;
  - a padding flag is cleared when the label's first part already starts with whitespace, or its
    last part already ends with it, so the hint's width (`padding.left + Σ part chars +
    padding.right`, D1) doesn't double the space (Zed, editor/src/inlays.rs:60-75);
  - sanitising (control characters → `' '`) and rejecting a label with no visible text are
    core's job, in `Hint::new`.
- **Keys are stable across refetches.** A hint with the same position, kind and label as one in
  the previous set keeps its key. Matching treats the hints as an ordered multiset, so repeated
  identical hints at one offset keep their keys in order, and it applies only when both sets are
  at the same revision. That way an open tooltip card (`HoverTarget::Inlay { key, part }`)
  survives a refresh. New keys come from a client counter.
- The client keeps the last set per document: its revision, the request snapshot, every open
  document's synced revision at send time, and the raw hints by key. `send` records `revisions`
  for `Kind::Inlays` as it does for `Definition | Rename`. The set is replaced on each answer and
  dropped on `close`.
- **Not running yet.** Hosts open documents before `initialize` finishes (the `rust_analyzer`
  example starts in `Link::Connecting`), so the first request declines. `initialized` therefore
  emits one `Change::InlayRefresh` per tracked document when the server has an inlay provider,
  which re-arms the editors. A server without refresh support still shows hints at startup.

**D17 — Refresh reaches the editors.**
- `workspace/inlayHint/refresh` gets a dedicated `respond` arm, still answered with `null`.
  `answer` returns one `Update::Document { stamp: Revision(synced), change:
  Change::InlayRefresh }` per open document.
- The editor treats the refresh as an edit trigger (D11) and never refuses it as stale. The
  generic refresh arm keeps answering the other refreshes.
- rust-analyzer sends the refresh twice at load, 1-2 ms after every `didChange`, and after a
  workspace reload.

**D18 — Interactions.** `Client::interact(&Snapshot, &inlay::Interaction) -> Output`.
- It is gated on `ticket.revision() == synced == set revision` and on a known key. Otherwise it
  declines with the action's empty answer: `InlayTooltip(None)`, `Definition(None)`, or
  `Edits(vec![])` for an insert. Phase 7's `land` removes the hint only when the ops are
  non-empty.
- **Foreign targets.** `send` and `Query::request` always target the requester's URI and snapshot
  (client.rs:963-970, 1441-1448), and `hovered` converts against `entry.request_snapshot`
  (client.rs:1048-1056). So the new `Query` variants carry what they need:
  - `Resolve` carries the raw hint JSON;
  - `LocationHover` carries the target URI and the raw LSP position. It converts no range for a
    foreign URI, and applies the `revision_of` stale check for an open target, as `target` does
    at client.rs:1095.
  - Both get arms in `reissue`, `kind`, `method` and `caret`.
- **Tooltip** → `Change::InlayTooltip(Option<String>)`:
  1. A known tooltip (the hovered part's, else the hint's; R6) answers at once,
     lowered to the hover card's markdown subset.
  2. Otherwise, if the server resolves, the hint has `data` and it isn't resolved yet, the client
     sends `inlayHint/resolve` (`Kind::Tooltip`). The reply only **adds tooltips** to the stored
     hint; then step 1 or step 3 runs. The fetched label owns the parts: a resolve may restructure
     the label (Zed's own test turns a string label into five parts, hover_popover.rs:2650-2700),
     so part tooltips are copied only when the part count and texts match. Otherwise only the
     hint-level tooltip is used. A resolve never changes links or text edits after install.
  3. Otherwise, if the hovered part has a location, the client sends `textDocument/hover` at that
     location, for any URI (the spec says a part's location drives its hover).
  4. Otherwise the answer is `None`.

  `Kind::Tooltip` is an intel kind, so failures answer `None`. rust-analyzer quietly returns a
  stale resolve without its deferred fields, so a resolve goes out only at the current revision,
  and a reply that lands after an edit is dropped.
- **Jump** → `Change::Definition(target)`, with no round trip. `target` is refactored to take
  `(requester, &Snapshot, &[(uri::Key, Revision)])` instead of `&Pending`. The definition path
  passes its pending entry's fields, and the jump passes the stored set's, so `Local`, `Open`
  (with the stale check against the other document's recorded revision) and `Unopened` all work.
  In the probe, 25 of 61 linked parts pointed into unopened sysroot files.
- **Insert** → `Change::Edits(hygiene(text_edits))`, stamped with the ticket, with no round trip.

**Related decisions this phase relies on.**
- **D1:** `Hint` carries no offset. `Placed { offset, hint }` pairs a fetch-time offset with its
  payload and is what crosses the seam. `Key` is opaque to core and minted by the host.
- **D4:** `set_inlays` replaces the whole store at the ticket's revision. Installing keeps server
  order: core mints its ids in server order and normalises mixed sides at one offset. So **the
  client must not sort, dedupe or regroup hints**.
- **D12:** a failed fetch (`None`) must not blank the hints; `Some(vec![])` clears them.
- **D14:** the editor records interactions only while its hint set is current. The client gate
  above is the second line of defence.
- **D19:** `update::Change` stays exhaustive. `land` lives in another crate, and the compile error
  on a missing arm is what keeps every new `Change` handled (critique round 1, B1). The arms:
  `Inlays` (ticket gate → `set_inlays`), `InlayTooltip` (ticket gate → `set_inlay_tooltip`),
  `InlayRefresh` (schedule; never `Stale`).
- **Constraints:** scrive-lsp stays pure (no `std::time`, threads or I/O), and every message goes
  out through `#[must_use] Output`. No new dependencies: lsp-types 0.97 has every inlay type.
  Module paths, not composite names; no aliased imports; `expect` over `unwrap`; tests in-file
  with sentence names; `#![deny(missing_docs)]`.

## Step-by-step changes

Module layout after this phase:

```
crates/scrive-lsp/src/
├── client.rs      + Kind::{Inlays, Tooltip}, Query::{Inlays, Resolve, LocationHover},
│                    Client::{inlays, interact}, refactored target, free revision_of
├── client/capabilities.rs   + inlayHint advertised, Server.inlay: Option<Resolve>
├── inlay.rs       new, crate-private: decode, conversion, keys, the per-document Set
├── update.rs      + Change::{Inlays, InlayRefresh, InlayTooltip}
├── hover.rs       `contents` becomes pub(crate)
└── lib.rs         + mod inlay; crate doc bullets
crates/scrive-iced/src/code_editor/lsp.rs   + three land arms
```

Naming: scrive-lsp's new module is `crate::inlay`, and scrive-core's is
`scrive_core::intel::inlay`. Import `scrive_core::intel` and write `intel::inlay::Placed`; never
`use … as …`. client.rs's private `Kind` and `intel::inlay::Kind` are told apart by path in the
same way.

### Step 1 — the `target` refactor (commit 1)

`crates/scrive-lsp/src/client.rs`. Before (client.rs:1074-1103):

```rust
    /// Where `range` in `key` lies relative to `entry`'s document; `None` when it is in another
    /// open document whose text is not the one the server answered for.
    fn target(
        &self,
        entry: &Pending,
        key: uri::Key,
        range: lsp_types::Range,
    ) -> Option<update::Target> {
        let requester = self.tracked.iter().find(|t| t.doc_id == entry.doc_id)?;
        if key == requester.key {
            return Some(update::Target::Local(
                self.encoding.span(&entry.request_snapshot, range),
            ));
        }
        let Some(tracked) = self.tracked.iter().find(|t| t.key == key) else { /* Unopened */ };
        if entry.revision_of(&key) != Some(tracked.synced.revision()) {
            return None;
        }
        Some(update::Target::Open(/* … */))
    }
```

After:

```rust
    /// Where `range` in `key` lies relative to the `requester` document, whose positions convert
    /// against `snapshot`; `None` when it is in another open document whose synced revision is
    /// not the one `revisions` recorded when the server was asked.
    fn target(
        &self,
        requester: DocId,
        snapshot: &Snapshot,
        revisions: &[(uri::Key, Revision)],
        key: uri::Key,
        range: lsp_types::Range,
    ) -> Option<update::Target> {
        let requester = self.tracked.iter().find(|t| t.doc_id == requester)?;
        if key == requester.key {
            return Some(update::Target::Local(self.encoding.span(snapshot, range)));
        }
        let Some(tracked) = self.tracked.iter().find(|t| t.key == key) else {
            return Some(update::Target::Unopened(jump::Unopened::new(key, range, self.encoding)));
        };
        if revision_of(revisions, &key) != Some(tracked.synced.revision()) {
            return None;
        }
        Some(update::Target::Open(jump::Open::new(
            tracked.doc_id,
            tracked.synced.revision(),
            self.encoding.span(&tracked.synced, range),
        )))
    }
```

- `defined` (client.rs:1062-1072) calls
  `self.target(entry.doc_id, &entry.request_snapshot, &entry.revisions, key, range)`.
- `Pending::revision_of` (client.rs:1385-1391) becomes a free function next to `cancel`, with the
  same doc comment adapted: "The synced revision `key` was at when `revisions` were recorded, if
  it was open then." `renamed` (client.rs:1115) calls `revision_of(&entry.revisions, file.key())`.
  One owner for the lookup: the jump and the location hover use it in commit 4.

```rust
fn revision_of(revisions: &[(uri::Key, Revision)], key: &uri::Key) -> Option<Revision> {
    revisions.iter().find(|(k, _)| k == key).map(|&(_, revision)| revision)
}
```

`Pending` loses its `impl` method only; `Pending::failed` stays. No test changes. The definition
and rename suites (`definition_in_*`, `rename_rejects_*`) must pass unchanged.

### Step 2 — capabilities (commit 2)

`crates/scrive-lsp/src/client/capabilities.rs`.

Imports gain `InlayHintClientCapabilities, InlayHintResolveClientCapabilities,
InlayHintServerCapabilities, InlayHintWorkspaceClientCapabilities`.

In `client()` (capabilities.rs:106-175), inside `WorkspaceClientCapabilities` (108-119):

```rust
            // The server asks for a refetch after its analysis changes (rust-analyzer: after
            // every `didChange`); the editors treat it like an edit.
            inlay_hint: Some(InlayHintWorkspaceClientCapabilities {
                refresh_support: Some(true),
            }),
```

Inside `TextDocumentClientCapabilities` (120-163), after `formatting`:

```rust
            // Only tooltips are lazy: locations and text edits arrive with the hint, so a jump or
            // an insert needs no round trip and cannot meet a stale resolve.
            inlay_hint: Some(InlayHintClientCapabilities {
                dynamic_registration: Some(false),
                resolve_support: Some(InlayHintResolveClientCapabilities {
                    properties: vec!["tooltip".to_owned(), "label.tooltip".to_owned()],
                }),
            }),
```

The wire shapes these produce, which the capability test asserts:

```json
"/params/capabilities/textDocument/inlayHint":
  {"dynamicRegistration": false, "resolveSupport": {"properties": ["tooltip", "label.tooltip"]}}
"/params/capabilities/workspace/inlayHint":
  {"refreshSupport": true}
```

In `Server` (capabilities.rs:16-37), after `formatting`:

```rust
    /// Whether the server answers `textDocument/inlayHint`, and whether it resolves hints.
    pub(crate) inlay: Option<Resolve>,
```

A new enum beside `Save` (capabilities.rs:39-48):

```rust
/// Whether the server answers `inlayHint/resolve`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Resolve {
    /// Hints arrive whole.
    Unsupported,
    /// Hints arrive without the lazily resolvable properties, which `inlayHint/resolve` fills in.
    Supported,
}
```

In `Server::new` (capabilities.rs:79-101), follow the `OneOf` template at 89-100:

```rust
            inlay: match &capabilities.inlay_hint_provider {
                None | Some(OneOf::Left(false)) => None,
                Some(OneOf::Left(true)) => Some(Resolve::Unsupported),
                Some(OneOf::Right(InlayHintServerCapabilities::Options(options))) => {
                    Some(resolve(options.resolve_provider))
                }
                Some(OneOf::Right(InlayHintServerCapabilities::RegistrationOptions(options))) => {
                    Some(resolve(options.inlay_hint_options.resolve_provider))
                }
            },
```

```rust
fn resolve(provider: Option<bool>) -> Resolve {
    if provider == Some(true) { Resolve::Supported } else { Resolve::Unsupported }
}
```

Commit 2 reads only `inlay.is_some()`, and commit 4 reads the `Resolve` inside. That raises no
dead-code lint: the field is read, and both variants are constructed in `Server::new`.

### Step 3 — `update::Change` (commits 2, 3, 4)

`crates/scrive-lsp/src/update.rs` (Change at update.rs:39-56). Add `use scrive_core::intel::inlay;`
and append one variant per commit:

```rust
    /// The inlay hints answering the ticket's request, replacing the shown set; an empty list
    /// clears it. `None` means the fetch failed: the editor keeps what it shows.
    Inlays(Option<Vec<inlay::Placed>>),                    // commit 2
    /// The server's hints changed: fetch them again. Stamped with the synced revision, but valid
    /// whatever the editor's revision.
    InlayRefresh,                                          // commit 3
    /// The tooltip for the ticket's hint gesture, in the hover card's markdown subset; `None`
    /// means there is nothing to show.
    InlayTooltip(Option<String>),                          // commit 4
```

The test helpers in client/tests.rs already end their matches with `other => panic!`, so the new
variants break no test.

### Step 4 — `crates/scrive-lsp/src/inlay.rs` (new; commit 2, grown in commit 4)

Crate-private. It holds the conversion and the per-document `Set`; `client.rs` keeps the request
machinery. The full commit-4 shape is shown below, and each field is marked with the commit that
adds it.

```rust
//! Inlay hints: a `textDocument/inlayHint` answer converted to scrive's hint model, and the set a
//! document keeps so hint gestures can be answered from what the server sent.

use std::collections::HashMap;
use std::ops::Range;

use lsp_types::{
    InlayHintKind, InlayHintLabel, InlayHintLabelPartTooltip, InlayHintTooltip, MarkupKind,
    Position,
};
use scrive_core::intel::hover::escape_markdown;
use scrive_core::{intel, Revision, Snapshot};
use serde_json::Value;

use crate::{markdown, uri, Encoding};

/// The hints of one answer, kept by key for the gestures on them.
#[derive(Debug)]
pub(crate) struct Set {
    revision: Revision,                       // commit 2; commit 4 may derive it from `snapshot`
    snapshot: Snapshot,                       // commit 4: what positions and edits convert against
    revisions: Vec<(uri::Key, Revision)>,     // commit 4: every open document's synced revision at send time
    hints: Vec<Stored>,                       // server order
}

/// One installed hint as the server sent it.
#[derive(Debug)]
pub(crate) struct Stored {
    key: intel::inlay::Key,                   // commit 2
    hint: lsp_types::InlayHint,               // commit 2: identity for key matching; tooltips, locations, edits
    raw: Value,                               // commit 4: sent back verbatim by `inlayHint/resolve`
    resolved: bool,                           // commit 4: a resolve reply has been absorbed
}

/// One entry that decoded and lies in the clipped span, at its offset in the request snapshot.
pub(crate) struct Fetched {
    offset: u32,
    hint: lsp_types::InlayHint,
    raw: Value,                               // commit 4
}
```

**Decoding and clipping** (D16):

```rust
/// The entries of an answer over `span` that decode and lie within one line of it, in server
/// order. An entry that does not decode is skipped alone. Hints past the last line are dropped
/// rather than clamped to the document end.
pub(crate) fn decode(encoding: Encoding, snapshot: &Snapshot, span: Range<u32>, entries: Vec<Value>) -> Vec<Fetched> {
    // The spec allows hints outside the requested range; clipping keeps the editor's store
    // bounded by its window.
    let first = snapshot.offset_to_point(span.start.min(snapshot.len())).row.saturating_sub(1);
    let last = snapshot.offset_to_point(span.end.min(snapshot.len())).row.saturating_add(1);
    entries
        .into_iter()
        .filter_map(|raw| {
            let hint: lsp_types::InlayHint = serde_json::from_value(raw.clone()).ok()?;
            let line = hint.position.line;
            if line < first || line > last || line >= snapshot.line_count() {
                return None;
            }
            let offset = encoding.offset(snapshot, hint.position);
            Some(Fetched { offset, hint, raw })
        })
        .collect()
}
```

In commit 2, decode with `serde_json::from_value(raw)` (no clone) and leave out `raw`.

**Conversion of one hint** (D16, D1). The client passes every hint's placement with
`.placement(..)` (R5); for `Type` and `Parameter` it equals `Hint::new`'s default.

```rust
/// `fetched` as scrive's hint under `key`, or `None` when core rejects its label.
fn placed(fetched: &Fetched, key: intel::inlay::Key) -> Option<intel::inlay::Placed> {
    let hint = &fetched.hint;
    let label: Vec<intel::inlay::Part> = match &hint.label {
        InlayHintLabel::String(text) => vec![intel::inlay::Part::new(text.clone(), intel::inlay::Link::None)],
        InlayHintLabel::LabelParts(parts) => parts
            .iter()
            .map(|part| {
                let link = if part.location.is_some() { intel::inlay::Link::Jumps } else { intel::inlay::Link::None };
                intel::inlay::Part::new(part.value.clone(), link)
            })
            .collect(),
    };
    let kind = match hint.kind {
        Some(InlayHintKind::TYPE) => intel::inlay::Kind::Type,
        Some(InlayHintKind::PARAMETER) => intel::inlay::Kind::Parameter,
        _ => intel::inlay::Kind::Other,
    };
    let left = hint.padding_left == Some(true);
    let right = hint.padding_right == Some(true);
    let placement = match (kind, left, right) {
        (intel::inlay::Kind::Type, ..) => intel::inlay::Placement::Suffix,
        (intel::inlay::Kind::Parameter, ..) => intel::inlay::Placement::Prefix,
        // Zed's rule: padding faces away from the token the hint annotates.
        (intel::inlay::Kind::Other, false, true) => intel::inlay::Placement::Prefix,
        (intel::inlay::Kind::Other, true, false) => intel::inlay::Placement::Suffix,
        (intel::inlay::Kind::Other, ..) => intel::inlay::Placement::Auto,
    };
    let texts = label_texts(&hint.label);
    // A label that already carries the space at an edge gets no padding there, so its cells
    // match its text.
    let padding = intel::inlay::Padding {
        left: left && !texts.first().is_some_and(|t| t.starts_with(char::is_whitespace)),
        right: right && !texts.last().is_some_and(|t| t.ends_with(char::is_whitespace)),
    };
    let insert = if hint.text_edits.as_ref().is_some_and(|edits| !edits.is_empty()) {
        intel::inlay::Insert::Available
    } else {
        intel::inlay::Insert::Unavailable
    };
    let hint = intel::inlay::Hint::new(kind, label, key).ok()?.padding(padding).insert(insert).placement(placement);
    Some(intel::inlay::Placed::new(fetched.offset, hint))
}

fn label_texts(label: &InlayHintLabel) -> Vec<&str> {
    match label {
        InlayHintLabel::String(text) => vec![text.as_str()],
        InlayHintLabel::LabelParts(parts) => parts.iter().map(|part| part.value.as_str()).collect(),
    }
}
```

- The side comes from the **raw** padding flags, and the collapse applies afterwards (R5). This
  matches Zed, whose `hint_position_and_bias` reads the LSP flags.
- `InlayHintKind` derives `PartialEq, Eq`, so its associated consts work as patterns.
- `Kind` is `Copy`, so the `match (kind, …)` tuple works.

**Installing a set, with stable keys** (D16):

```rust
impl Set {
    /// The set for an answer at `snapshot`, and its hints for the editor, in server order. A hint
    /// matching one of `previous` by position, kind and label texts keeps that hint's key, first
    /// unused match first, so repeated identical hints keep theirs in order; every other hint
    /// gets the next key from `counter`. `previous` must be at the same revision.
    pub(crate) fn install(
        snapshot: &Snapshot,
        revisions: Vec<(uri::Key, Revision)>,  // commit 4
        fetched: Vec<Fetched>,
        previous: Option<Set>,
        counter: &mut u64,
    ) -> (Set, Vec<intel::inlay::Placed>) {
        let mut reusable: HashMap<Position, Vec<Stored>> = HashMap::new();
        for stored in previous.into_iter().flat_map(|set| set.hints) {
            reusable.entry(stored.hint.position).or_default().push(stored);
        }
        let mut hints = Vec::with_capacity(fetched.len());
        let mut placed = Vec::with_capacity(fetched.len());
        for fetched in fetched {
            let key = reusable
                .get_mut(&fetched.hint.position)
                .and_then(|same| {
                    let at = same.iter().position(|stored| same_hint(&stored.hint, &fetched.hint))?;
                    Some(same.remove(at).key)
                })
                .unwrap_or_else(|| {
                    *counter += 1;
                    intel::inlay::Key::new(*counter)
                });
            if let Some(hint) = placed(&fetched, key) {
                placed.push(hint);
                hints.push(Stored { key, hint: fetched.hint, raw: fetched.raw, resolved: false });
            }
        }
        (Set { revision: snapshot.revision(), snapshot: snapshot.clone(), revisions, hints }, placed)
    }

    /// The revision the set was fetched at.
    pub(crate) fn revision(&self) -> Revision { self.revision }
}

/// Same position, kind and label texts: the same hint, refetched.
fn same_hint(a: &lsp_types::InlayHint, b: &lsp_types::InlayHint) -> bool {
    a.position == b.position && a.kind == b.kind && label_texts(&a.label) == label_texts(&b.label)
}
```

- `lsp_types::InlayHint` has no `PartialEq`, hence `same_hint`. `Position` is `Hash`, so the map
  keeps matching at O(hints at one position) per hint. Identity compares label **texts** only,
  not tooltips or locations (R18: position, kind and label part texts, ordered multiset, same
  revision only).
- A hint whose label core rejects consumes no stored key it could have matched: an identical
  hint in the previous set would have been rejected too. A minted key that goes unused is
  harmless.
- `same_hint` is the inherent part of the multiset rule. Keep the `HashMap` grouping; a
  quadratic scan over a few hundred hints is wasteful.

**Commit 4 additions** to `Set` and `Stored`:

```rust
impl Set {
    pub(crate) fn snapshot(&self) -> &Snapshot { &self.snapshot }
    pub(crate) fn revisions(&self) -> &[(uri::Key, Revision)] { &self.revisions }
    pub(crate) fn get(&self, key: intel::inlay::Key) -> Option<&Stored> { self.hints.iter().find(|s| s.key == key) }
    pub(crate) fn get_mut(&mut self, key: intel::inlay::Key) -> Option<&mut Stored> { self.hints.iter_mut().find(|s| s.key == key) }
}

impl Stored {
    /// The tooltip for a gesture on `part`, lowered to the hover card's subset: the part's own,
    /// else the hint's. Whitespace-only tooltips count as none.
    pub(crate) fn tooltip(&self, part: u32) -> Option<String> {
        let own = self.part(part).and_then(|part| part.tooltip.as_ref()).and_then(part_tooltip);
        own.or_else(|| self.hint.tooltip.as_ref().and_then(hint_tooltip))
    }

    /// The location of label part `part`, if it has one.
    pub(crate) fn location(&self, part: u32) -> Option<&lsp_types::Location> {
        self.part(part)?.location.as_ref()
    }

    /// The edits that insert the hint into the text; empty when it has none.
    pub(crate) fn text_edits(&self) -> &[lsp_types::TextEdit] {
        self.hint.text_edits.as_deref().unwrap_or_default()
    }

    /// Whether a resolve could still add a tooltip: the server keeps `data` on the hint for that.
    pub(crate) fn resolvable(&self) -> bool { !self.resolved && self.hint.data.is_some() }

    /// The raw hint, for `inlayHint/resolve`.
    pub(crate) fn raw(&self) -> &Value { &self.raw }

    /// Takes the tooltips of `resolved` that this hint lacks. The fetched label owns the parts:
    /// a resolve may restructure the label, so part tooltips are taken only from the same label.
    /// Links and text edits never change after install.
    pub(crate) fn absorb(&mut self, resolved: lsp_types::InlayHint) {
        self.resolved = true;
        if self.hint.tooltip.is_none() {
            self.hint.tooltip = resolved.tooltip;
        }
        if let (InlayHintLabel::LabelParts(mine), InlayHintLabel::LabelParts(theirs)) = (&mut self.hint.label, resolved.label) {
            let same = mine.len() == theirs.len() && mine.iter().zip(&theirs).all(|(a, b)| a.value == b.value);
            if same {
                for (mine, theirs) in mine.iter_mut().zip(theirs) {
                    if mine.tooltip.is_none() {
                        mine.tooltip = theirs.tooltip;
                    }
                }
            }
        }
    }

    fn part(&self, part: u32) -> Option<&lsp_types::InlayHintLabelPart> {
        match &self.hint.label {
            InlayHintLabel::LabelParts(parts) => parts.get(part as usize),
            InlayHintLabel::String(_) => None,
        }
    }
}

/// The spec makes a bare string tooltip plain text.
fn hint_tooltip(tooltip: &InlayHintTooltip) -> Option<String> {
    match tooltip {
        InlayHintTooltip::String(text) => card(&MarkupKind::PlainText, text),
        InlayHintTooltip::MarkupContent(content) => card(&content.kind, &content.value),
    }
}

fn part_tooltip(tooltip: &InlayHintLabelPartTooltip) -> Option<String> {
    match tooltip {
        InlayHintLabelPartTooltip::String(text) => card(&MarkupKind::PlainText, text),
        InlayHintLabelPartTooltip::MarkupContent(content) => card(&content.kind, &content.value),
    }
}

fn card(kind: &MarkupKind, text: &str) -> Option<String> {
    let markdown = match kind {
        MarkupKind::Markdown => markdown::to_hover(text),
        MarkupKind::PlainText => escape_markdown(text),
    };
    (!markdown.trim().is_empty()).then_some(markdown)
}
```

- A string label has no parts, so `tooltip(0)` falls through to the hint's tooltip. That is why
  the fallback exists (R6).
- `resolved` is set only by a successful resolve reply. A failed resolve leaves the hint
  resolvable, so the next hover tries again (R17); Zed reverts to `CanResolve` on
  error.
- `lsp_types::InlayHintTooltip` and `InlayHintLabelPartTooltip` are distinct types with the same
  shape, hence the two small functions. `MarkupKind` is a two-variant lsp-types enum.

### Step 5 — `client.rs`: the fetch (commit 2)

Imports: add `InlayHintParams` to the lsp-types list (client.rs:13-23) and `intel` to the
scrive-core list (24-27). Add `inlay` to `use crate::{…}` (32).

**State.**
- `Client` (49-66) gains a field, initialised to `0` in `Builder::build` (1306-1317):

  ```rust
  /// The last inlay hint key minted; keys are unique per client.
  next_inlay: u64,
  ```
- `Tracked` (148-161) gains a field, set to `None` in `open`'s literal (262-269). `close` drops it
  with the `Tracked`, which is D16's "dropped on close":

  ```rust
  /// The document's last inlay hint answer, which hint gestures are answered from.
  inlays: Option<inlay::Set>,
  ```

**`Kind` and `Query`** (184-207):

```rust
enum Kind { Completion, Signature, Hover, Definition, Rename, Format, Inlays /*, Tooltip (commit 4) */ }

enum Query {
    // … existing …
    /// The inlay hints over `span`.
    Inlays { span: Range<u32> },
}
```

**Arms in the existing matches** (commit 2):

| Site | Arm |
|---|---|
| `send`'s `revisions` (955-962) | `Kind::Inlays` joins the empty-`Vec` arm in commit 2. Commit 4 moves it to `Kind::Definition \| Kind::Rename \| Kind::Inlays` |
| `reissue` (812-820) | `Query::Inlays { .. }` joins the `query @ (Query::Hover(_) \| …)` group: re-sent unchanged under the same ticket |
| `resolved` (824-833) | `Query::Inlays { span } => Ok(self.inlaid(entry, span, value))` |
| `Pending::failed` (1361-1383) | `Query::Inlays { .. } => Output::answer(self.doc_id, self.latest_ticket, update::Change::Inlays(None))` |
| `Kind::is_command` (1396-1401) | `Kind::Inlays` is `false` |
| `Query::kind` (1405-1414) | `Query::Inlays { .. } => Kind::Inlays` |
| `Query::method` (1417-1427) | `Query::Inlays { .. } => lsp_types::request::InlayHintRequest::METHOD` |
| `Query::caret` (1430-1439) | `Query::Inlays { span } => span.start`, with the comment "A fetch covers a span; its start stands in for the caret." |
| `Query::request` (1442-1525) | below |

```rust
            Query::Inlays { span } => message::Request::new::<lsp_types::request::InlayHintRequest>(
                id,
                InlayHintParams {
                    work_done_progress_params: Default::default(),
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    range: encoding.range(snapshot, span.clone()),
                },
            ),
```

**The entry point**, after `format` (525-543), in the same shape as `hover`:

```rust
    /// Asks the server for the inlay hints over the request's byte span. The answer is an
    /// [`update::Change::Inlays`] under the request's ticket, replacing the hints the editor
    /// shows; a request already in flight for the document is cancelled.
    ///
    /// When the server is not running or has no inlay hint provider, the request is declined
    /// with an empty [`update::Change::Inlays`] under its ticket. A request from a revision other
    /// than `snapshot`'s or the last synced one, or for a document that is not registered, gets
    /// nothing.
    pub fn inlays(&mut self, snapshot: &Snapshot, request: &intel::inlay::Request) -> Output {
        let doc_id = snapshot.doc_id();
        let ticket = request.ticket();
        let Some(tracked) = self.tracked.iter().find(|t| t.doc_id == doc_id) else {
            return Output::default();
        };
        if ticket.revision() != snapshot.revision() || snapshot.revision() != tracked.synced.revision() {
            return Output::default();
        }
        if !matches!(&self.state, State::Running(server) if server.inlay.is_some()) {
            return Output::answer(doc_id, ticket, update::Change::Inlays(Some(Vec::new())));
        }
        self.send(doc_id, ticket, Query::Inlays { span: request.span() }, None)
    }
```

**The reply**, after `hovered` (1048-1056):

```rust
    /// Answers the ticket with the reply's hints, and keeps them as the document's set. `null`
    /// is an empty answer, which clears the editor's hints.
    fn inlaid(&mut self, entry: &Pending, span: &Range<u32>, value: Value) -> Output {
        let Ok(entries) = serde_json::from_value::<Option<Vec<Value>>>(value) else {
            return entry.failed();
        };
        let snapshot = &entry.request_snapshot;
        let fetched = inlay::decode(self.encoding, snapshot, span.clone(), entries.unwrap_or_default());
        let Self { tracked, next_inlay, .. } = self;
        let Some(tracked) = tracked.iter_mut().find(|t| t.doc_id == entry.doc_id) else {
            return Output::default();
        };
        let previous = tracked.inlays.take().filter(|set| set.revision() == snapshot.revision());
        let (set, placed) = inlay::Set::install(snapshot, entry.revisions.clone(), fetched, previous, next_inlay);
        tracked.inlays = Some(set);
        Output::answer(entry.doc_id, entry.latest_ticket, update::Change::Inlays(Some(placed)))
    }
```

- `settled` (753-788) has already dropped a reply whose ticket is behind the synced revision.
  So `entry.request_snapshot` is at the synced revision, and `previous` matches only a refetch at
  that revision.
- A failed or undecodable answer goes through `failed` and leaves `tracked.inlays` as it was. The
  editor keeps showing the old hints (D12), and gestures on them decline, because that set's
  revision is no longer current.
- `revisions` goes into `install` only from commit 4 on.

### Step 6 — `client.rs`: refresh (commit 3)

**`respond`** (1196-1250). Add a dedicated arm **before** the generic refresh arm at 1236-1240,
and drop `workspace/inlayHint/refresh` from that arm's comment:

```rust
            // Answered like every refresh; `answer` also tells each open document to refetch.
            lsp_types::request::InlayHintRefreshRequest::METHOD => message::Response::ok(id, ()),
            // `workspace/semanticTokens/refresh`, …: nothing is cached that a refresh would
            // invalidate.
            method if method.starts_with("workspace/") && method.ends_with("/refresh") => {
                message::Response::ok(id, ())
            }
```

`InlayHintRefreshRequest::METHOD` is an associated const, so it can't be a match pattern
directly. Bring `use lsp_types::request::Request;` into the function (as `Query::method` does at
1418) and use a guard: `method if method == lsp_types::request::InlayHintRefreshRequest::METHOD
=> …`. Alternatively, match the literal `"workspace/inlayHint/refresh"` and pin it to the const in
the test.

**`answer`** (1184-1194):

```rust
    fn answer(&self, request: message::Request) -> Output {
        use lsp_types::request::Request;
        let refresh = request.method == lsp_types::request::InlayHintRefreshRequest::METHOD;
        let (response, updates) = match self.state {
            // After `shutdown()` the client promises nothing; `null` keeps the server unblocked.
            State::ShuttingDown { .. } | State::Exited => (message::Response::ok(request.id, ()), Vec::new()),
            State::Initializing { .. } => (self.respond(request), Vec::new()),
            State::Running(_) => {
                let updates = if refresh { self.refreshes() } else { Vec::new() };
                (self.respond(request), updates)
            }
        };
        Output { messages: vec![Message::Response(response)], updates }
    }

    /// An inlay refresh for every open document, at its synced revision.
    fn refreshes(&self) -> Vec<Update> {
        self.tracked
            .iter()
            .map(|t| {
                Update::Document(update::Document::new(
                    t.doc_id,
                    update::Stamp::Revision(t.synced.revision()),
                    update::Change::InlayRefresh,
                ))
            })
            .collect()
    }
```

The fan-out happens only while `Running` (R17). During `Initializing` the
editors' fetches would decline anyway, and `initialized` re-arms them.

**`initialized`** (835-878). After the deferred-`didOpen` loop (872-876):

```rust
        // Requests from before the handshake were declined; this re-arms every editor, which
        // also covers servers that never send `workspace/inlayHint/refresh`.
        if matches!(&self.state, State::Running(server) if server.inlay.is_some()) {
            output.updates.extend(self.refreshes());
        }
```

**The `land` arm** for `InlayRefresh` is in Step 8.

### Step 7 — `client.rs`: interactions (commit 4)

**`Kind` and `Query`:**

```rust
enum Kind { /* … */ Inlays, Tooltip }

enum Query {
    // …
    /// `inlayHint/resolve` of the hint under `key`, for the tooltip of `part`. `hint` is the raw
    /// hint as the server sent it.
    Resolve { key: intel::inlay::Key, part: u32, hint: Value },
    /// `textDocument/hover` at a label part's location, in any document, for a hint tooltip.
    LocationHover { uri: Uri, position: lsp_types::Position },
}
```

**Arms in the existing matches:**

| Site | `Resolve` | `LocationHover` |
|---|---|---|
| `send`'s `revisions` | `Kind::Tooltip` → empty `Vec`; `Kind::Inlays` moves to the recording arm with `Definition \| Rename` | — |
| `reissue` | joins the `query @ (…)` group | joins the `query @ (…)` group |
| `resolved` | `Query::Resolve { key, part, .. } => Ok(self.tooltip_resolved(entry, *key, *part, value))` | `Query::LocationHover { .. } => Ok(self.location_hovered(entry, value))` |
| `Pending::failed` | `update::Change::InlayTooltip(None)` under the latest ticket | same |
| `Kind::is_command` | `Kind::Tooltip` is `false` | — |
| `Query::kind` | `Kind::Tooltip` | `Kind::Tooltip` |
| `Query::method` | `lsp_types::request::InlayHintResolveRequest::METHOD` | `lsp_types::request::HoverRequest::METHOD` |
| `Query::caret` | `0` | `0` |
| `Query::request` | below | below |

Both share `Kind::Tooltip`, so a new tooltip gesture supersedes a resolve or location hover still
in flight, with `$/cancelRequest`. For `caret`, extend the `Format` comment: "Formatting, resolves
and location hovers ask at no caret in the requesting document."

```rust
            // The hint goes back exactly as it arrived: its `data` belongs to the server.
            Query::Resolve { hint, .. } => message::Request {
                id,
                method: lsp_types::request::InlayHintResolveRequest::METHOD.to_owned(),
                params: Some(hint.clone()),
            },
            // The location is the server's own, in its own document and encoding, so it goes
            // out unconverted.
            Query::LocationHover { uri: target, position } => {
                message::Request::new::<lsp_types::request::HoverRequest>(
                    id,
                    HoverParams {
                        text_document_position_params: TextDocumentPositionParams {
                            text_document: TextDocumentIdentifier { uri: target.clone() },
                            position: *position,
                        },
                        work_done_progress_params: Default::default(),
                    },
                )
            }
```

Inside `request`, the `uri` parameter shadows nothing if the pattern binds `uri: target`; keep the
parameter name `uri` for the existing arms.

**`inlay.rs` additions** for this commit: `Set`'s `snapshot` and `revisions` (with
`install(snapshot, revisions, …)`), and `Stored`'s `raw` and `resolved`, plus the `Set`/`Stored`
methods from Step 4.

**The entry point**, after `inlays`:

```rust
    /// Answers a gesture on an inlay hint from the document's last hint answer.
    ///
    /// - A tooltip answers [`update::Change::InlayTooltip`]: the tooltip the hint already has,
    ///   else one `inlayHint/resolve` when the server resolves, else the hover at the hovered
    ///   part's location, else `None`.
    /// - A jump answers [`update::Change::Definition`] with the part's location as a target.
    /// - An insert answers [`update::Change::Edits`] with the hint's text edits.
    ///
    /// Each answers under the gesture's ticket. When the ticket, the synced text and the hint
    /// answer are not all at one revision, the hint is unknown, or the server is not running, the
    /// gesture is declined with its empty answer: no tooltip, no target, or no edits. A gesture
    /// for a document that is not registered gets nothing.
    pub fn interact(&mut self, snapshot: &Snapshot, interaction: &intel::inlay::Interaction) -> Output {
        let doc_id = snapshot.doc_id();
        let ticket = interaction.ticket();
        let key = interaction.key();
        let Some(tracked) = self.tracked.iter().find(|t| t.doc_id == doc_id) else {
            return Output::default();
        };
        let current = ticket.revision() == snapshot.revision() && snapshot.revision() == tracked.synced.revision();
        let set = tracked.inlays.as_ref().filter(|set| current && set.revision() == ticket.revision());
        let (Some(set), State::Running(server)) = (set, &self.state) else {
            return Output::answer(doc_id, ticket, declined(interaction));
        };
        let Some(stored) = set.get(key) else {
            return Output::answer(doc_id, ticket, declined(interaction));
        };
        match interaction.gesture() {
            intel::inlay::interaction::Gesture::Tooltip { part } => {
                if let Some(markdown) = stored.tooltip(part) {
                    return Output::answer(doc_id, ticket, update::Change::InlayTooltip(Some(markdown)));
                }
                if server.inlay == Some(capabilities::Resolve::Supported) && stored.resolvable() {
                    let hint = stored.raw().clone();
                    return self.send(doc_id, ticket, Query::Resolve { key, part, hint }, None);
                }
                let location = stored.location(part).cloned();
                self.hover_location(doc_id, ticket, location, None)
            }
            intel::inlay::interaction::Gesture::Jump { part } => {
                let target = stored.location(part).and_then(|location| {
                    self.target(doc_id, set.snapshot(), set.revisions(), uri::normalize(&location.uri), location.range)
                });
                Output::answer(doc_id, ticket, update::Change::Definition(target))
            }
            intel::inlay::interaction::Gesture::Insert { .. } => {
                let ops = edits::hygiene(edits::Text::Snapshot(set.snapshot()), self.encoding, stored.text_edits());
                Output::answer(doc_id, ticket, update::Change::Edits(ops))
            }
        }
    }
```

Borrows: `stored` and `set` borrow `self.tracked` immutably, and `self.send` needs `&mut self`.
Clone what the `send` arms need (`raw`, the `Location`) into locals before calling, as above. If
the borrow checker still objects, compute the arm's decision (an enum of "answer now / resolve /
hover at location") in a block, then act on it.

```rust
/// The empty answer to a gesture that cannot be served.
fn declined(interaction: &intel::inlay::Interaction) -> update::Change {
    match interaction.gesture() {
        intel::inlay::interaction::Gesture::Tooltip { .. } => update::Change::InlayTooltip(None),
        intel::inlay::interaction::Gesture::Jump { .. } => update::Change::Definition(None),
        // Phase 7's `land` settles the slot without editing for an empty batch (R16).
        intel::inlay::interaction::Gesture::Insert { .. } => update::Change::Edits(Vec::new()),
    }
}
```

**Step 3 of the tooltip chain** (shared by `interact` and the resolve reply):

```rust
    /// Asks for the hover at a hint part's `location`, or answers no tooltip when there is none,
    /// the server has no hover, or the location is in another open document that moved since
    /// the hints were fetched.
    fn hover_location(
        &mut self,
        doc_id: DocId,
        ticket: Ticket,
        location: Option<lsp_types::Location>,
        reissued_for: Option<Ticket>,
    ) -> Output {
        let none = || Output::answer(doc_id, ticket, update::Change::InlayTooltip(None));
        let Some(location) = location else { return none() };
        if !matches!(&self.state, State::Running(server) if server.hover) {
            return none();
        }
        let key = uri::normalize(&location.uri);
        let Some(set) = self.tracked.iter().find(|t| t.doc_id == doc_id).and_then(|t| t.inlays.as_ref()) else {
            return none();
        };
        let moved = self
            .tracked
            .iter()
            .find(|t| t.key == key && t.doc_id != doc_id)
            .is_some_and(|other| revision_of(set.revisions(), &key) != Some(other.synced.revision()));
        if moved {
            return none();
        }
        self.send(doc_id, ticket, Query::LocationHover { uri: location.uri, position: location.range.start }, reissued_for)
    }
```

- A hover in the requesting document needs no stale check: the gate already tied the set to the
  synced revision.
- A server without `hoverProvider` gets no location hover (R17).

**The replies**, after `inlaid`:

```rust
    /// Adds the resolved hint's tooltips to the stored hint, then answers from it, or falls back
    /// to the hover at the part's location.
    fn tooltip_resolved(&mut self, entry: &Pending, key: intel::inlay::Key, part: u32, value: Value) -> Output {
        let Ok(resolved) = serde_json::from_value::<lsp_types::InlayHint>(value) else {
            return entry.failed();
        };
        let revision = entry.latest_ticket.revision();
        let stored = self
            .tracked
            .iter_mut()
            .find(|t| t.doc_id == entry.doc_id)
            .and_then(|t| t.inlays.as_mut())
            .filter(|set| set.revision() == revision)
            .and_then(|set| set.get_mut(key));
        let Some(stored) = stored else {
            return Output::answer(entry.doc_id, entry.latest_ticket, update::Change::InlayTooltip(None));
        };
        stored.absorb(resolved);
        if let Some(markdown) = stored.tooltip(part) {
            return Output::answer(entry.doc_id, entry.latest_ticket, update::Change::InlayTooltip(Some(markdown)));
        }
        let location = stored.location(part).cloned();
        self.hover_location(entry.doc_id, entry.latest_ticket, location, entry.reissued_for)
    }

    /// Answers the ticket with the hover at a label part's location, as tooltip markdown.
    fn location_hovered(&self, entry: &Pending, value: Value) -> Output {
        let Ok(reply) = serde_json::from_value::<Option<lsp_types::Hover>>(value) else {
            return entry.failed();
        };
        let markdown = reply.map(|reply| hover::contents(reply.contents)).filter(|markdown| !markdown.trim().is_empty());
        Output::answer(entry.doc_id, entry.latest_ticket, update::Change::InlayTooltip(markdown))
    }
```

- `hover.rs:42` `fn contents` becomes `pub(crate) fn contents`, with a one-line doc: "Hover
  contents in the hover card's subset."
- `tooltip_resolved` passes `entry.reissued_for` on to the chained location hover, as `signed`
  does (client.rs:1042), so the chain can't re-issue twice for one ticket.
- The **stale-resolve guard** has three layers. `interact` resolves only at `ticket == synced ==
  set revision`. `settled` drops the reply once the synced revision moved past the ticket
  (client.rs:767-772). `tooltip_resolved` absorbs only into a set at the ticket's revision. A
  dropped reply never marks the hint resolved.

### Step 8 — the `land` arms (scrive-iced; one per commit)

`crates/scrive-iced/src/code_editor/lsp.rs`, `land` (204-295). Model the two ticket arms on the
`Hover` arm (248-259). Read the verdict before `set_*`, which may retire the slot.

```rust
            Change::Inlays(placed) => {                                         // commit 2
                let Stamp::Ticket(ticket) = stamp else {
                    return refused(Refusal::Stale);
                };
                let accepted = self.accepts(Awaited::Inlays, ticket);
                self.set_inlays(ticket, placed);
                if accepted { update::Applied::default() } else { refused(Refusal::Stale) }
            }
            Change::InlayRefresh => {                                           // commit 3
                // The server's hints changed whatever text it saw, so a refresh is never stale.
                self.wait_inlays(INLAY_EDIT_DELAY, None);
                update::Applied::default()
            }
            Change::InlayTooltip(markdown) => {                                 // commit 4
                let Stamp::Ticket(ticket) = stamp else {
                    return refused(Refusal::Stale);
                };
                let accepted = self.accepts(Awaited::InlayTooltip, ticket);
                self.set_inlay_tooltip(ticket, markdown);
                if accepted { update::Applied::default() } else { refused(Refusal::Stale) }
            }
```

Update the comment at lsp.rs:211-212 to "The client stamps diagnostics, rename edits and inlay
refreshes with a revision, and every other change with a ticket; any other pairing is treated as
stale."

`land` is in a child module of `code_editor`, so it can call `CodeEditor`'s private methods and
read its private consts: extend the import to `use super::{Awaited, CodeEditor, INLAY_EDIT_DELAY};`. The
`InlayRefresh` arm reads no stamp; `stamp` is still used by the other arms. Use the existing
`Edits` arm unchanged: the insert-removes-its-hint logic is Phase 7.

### Step 9 — `lib.rs` (commit 2; one bullet in commit 4)

Add `mod inlay;` in alphabetical order (lib.rs:24-36). Extend the crate doc list (7-18):

```rust
//! - inlay hints → [`Client::inlays`]; tooltips, label jumps and inserts → [`Client::interact`]
```

In commit 2 the bullet names only `Client::inlays`; commit 4 adds the `interact` half, since an
intra-doc link to a missing method fails the doc build.

### Step 10 — tests (`crates/scrive-lsp/src/client/tests.rs`)

**Helpers** (add near the definition helpers, after client/tests.rs:2681):

```rust
use scrive_core::intel::inlay;

/// Server capabilities with utf-16 positions (the default), incremental sync, hover, and inlay
/// hints whose tooltips resolve lazily.
fn inlay_capabilities() -> Value {
    json!({"textDocumentSync": 2, "hoverProvider": true, "inlayHintProvider": {"resolveProvider": true}})
}

/// `let a = f(1);` / `let b = a;`: `a` ends at 5, `1` starts at 10, `b` ends at 19; 25 bytes, 3 lines.
const INLAY_TEXT: &str = "let a = f(1);\nlet b = a;\n";

/// `: i32` after `a`; its `i32` part links into the unopened `core.rs`.
fn type_hint() -> Value {
    json!({"position": {"line": 0, "character": 5}, "kind": 1, "label": [
        {"value": ": "},
        {"value": "i32", "location": {"uri": "file:///w/core.rs", "range": span_on(3, 4, 7)}},
    ], "paddingLeft": false, "paddingRight": false, "data": {"id": 1}})
}

/// `x:` before `1`, padded on the right.
fn parameter_hint() -> Value {
    json!({"position": {"line": 0, "character": 10}, "kind": 2, "label": "x:",
        "paddingLeft": false, "paddingRight": true, "data": {"id": 2}})
}

/// `: i32` after `b`, insertable: the edit rewrites `b` as `b: i32`, which hygiene trims to an
/// insert at 19.
fn insertable_hint() -> Value {
    json!({"position": {"line": 1, "character": 5}, "kind": 1, "label": ": i32",
        "textEdits": [text_edit((1, 4), (1, 5), "b: i32")], "data": {"id": 3}})
}

/// A running client with `text` open as `file:///a.rs`.
fn hinting(text: &str, capabilities: Value) -> (Client, Document);

/// An inlay request over the whole document at its revision.
fn inlay_request(tickets: &mut Counter, doc: &Document) -> inlay::Request; // inlay::Request::new(ticket, 0..len)

/// Requests the hints of `doc`, answers request `id` with `hints`, and returns the installed hints.
fn fetch(client: &mut Client, tickets: &mut Counter, doc: &Document, id: i64, hints: Value) -> Vec<inlay::Placed>;

fn inlays(update: &Update) -> (update::Stamp, Option<Vec<inlay::Placed>>);   // `other => panic!`
fn tooltip(update: &Update) -> (update::Stamp, Option<String>);              // `other => panic!`
fn refreshed(update: &Update) -> (DocId, update::Stamp);                    // InlayRefresh only
/// The one update in `output`, which sends nothing.
fn only(output: &Output) -> &Update;
```

Reuse the existing `uri`, `document`, `wire`, `from_server`, `running`, `reply` (2510),
`failure` (1170), `cancel_request` (1182), `assert_silent` (1186), `type_ops` (1688),
`text_edit` (2515), `span_on` (2668), `open_as` (2661), `CALLEE` (2658) and the per-variant
helpers `definition` and `edits`. Build `Interaction`s with `inlay::Interaction::{tooltip, jump, insert}` from a ticket
issued at the current revision. Request ids: `initialize` is 1, so a test's first request is 2.

**The exchanges.** Fetch request over `INLAY_TEXT` (span `0..25`):

```json
{"jsonrpc": "2.0", "id": 2, "method": "textDocument/inlayHint", "params": {
  "textDocument": {"uri": "file:///a.rs"},
  "range": {"start": {"line": 0, "character": 0}, "end": {"line": 2, "character": 0}}}}
```

The fetch reply is `reply(2, json!([type_hint(), parameter_hint(), insertable_hint()]))`, which
installs offsets `[5, 10, 19]`.

Resolve of `type_hint` (gesture: tooltip on part 0) — the params are the raw hint, byte for byte:

```json
{"jsonrpc": "2.0", "id": 3, "method": "inlayHint/resolve", "params": <type_hint()>}
```

Location hover for the `i32` part (gesture: tooltip on part 1, with no resolve):

```json
{"jsonrpc": "2.0", "id": 3, "method": "textDocument/hover", "params": {
  "textDocument": {"uri": "file:///w/core.rs"}, "position": {"line": 3, "character": 4}}}
```

Hover reply: `{"contents": {"kind": "markdown", "value": "```rust\nstruct i32\n```"}}` →
`InlayTooltip(Some("`struct i32`"))`.

Refresh from the server, and the client's answer:

```json
← {"jsonrpc": "2.0", "id": 7, "method": "workspace/inlayHint/refresh"}
→ {"jsonrpc": "2.0", "id": 7, "result": null}
```

plus one `InlayRefresh` update per open document, stamped `Revision(synced)`.

**The tests.** Every test has a `///` line stating its invariant and string assert messages.

`Hint` has no placement getter (R1). Tests that assert a placement compare the installed hint with
`==` (`Hint` derives `PartialEq`) against the expected build, e.g.
`Hint::new(Kind::Other, parts, key).expect(..).padding(p).insert(i).placement(Placement::Prefix)`.

Capabilities (commit 2):

| Test | Fixture → assertion |
|---|---|
| `initialize_advertises_inlay_hints_with_lazy_tooltips_and_refresh` | `Client::builder().build()`; `/params/capabilities/textDocument/inlayHint` and `/params/capabilities/workspace/inlayHint` equal the D15 JSON above |
| `inlay_provider_is_read_from_every_shape` | for each row of the provider table (Spot checks), `running(…)` then `matches!(&client.state, State::Running(server) if server.inlay == expected)` |

Fetch (commit 2):

| Test | Fixture → assertion |
|---|---|
| `inlay_request_carries_the_span_as_a_range` | `hinting(INLAY_TEXT, inlay_capabilities())`; `inlays` → exactly the fetch request above; no updates |
| `inlays_decline_before_initialize_and_without_a_provider` | an initializing client; one running with `{"textDocumentSync": 2}`; one with `"inlayHintProvider": false` → each answers `Inlays(Some([]))` under the ticket, with no messages |
| `stale_inlay_request_is_ignored` | ticket issued, then `type_ops` → `inlays` with the old ticket → `assert_silent` |
| `new_inlay_request_supersedes_the_previous_one` | two requests at one revision → the second sends `[cancel_request(2), fetch request id 3]` |
| `inlay_reply_answers_the_ticket_with_converted_hints` | reply `[type_hint, parameter_hint, insertable_hint]` → stamp `Ticket(first)`; offsets `[5, 10, 19]`; kinds `[Type, Parameter, Type]`; placements `[Suffix, Prefix, Suffix]`; `type_hint` parts `[(": ", None), ("i32", Jumps)]`; `parameter_hint` padding right only; inserts `[Unavailable, Unavailable, Available]` |
| `inlay_positions_convert_from_utf16_against_the_request_snapshot` | doc `"let 😀 = f(a);\n"`; hints at `(0,6)`, `(0,11)`, `(0,5)` (mid-surrogate) → offsets `[8, 13, 4]` in that order |
| `inlay_hints_on_lines_past_the_end_are_dropped` | doc `"a\n"` (2 lines), span `0..2`; hints at `(1,0)` and `(2,0)` → one hint, offset 2 |
| `inlay_hints_outside_the_request_span_are_clipped` | doc `"a\nb\nc\nd\ne\n"`, span `4..5` (line 2); hints labelled `"0"`…`"4"` at `(n,0)` → labels `["1", "2", "3"]`; the request range is `(2,0)-(2,1)` |
| `inlay_answer_keeps_the_server_order_including_ties` | reply `[parameter_hint, A(0,5) ": a", B(0,5) ": b", C(0,5) ": c"]` → offsets `[10, 5, 5, 5]`, labels in that order; the client does not sort |
| `inlay_labels_are_sanitised_by_core_and_empty_labels_dropped` | labels `"a\tb"`, `""`, `[]`, `[{"value": "c"}]` → two hints, texts `"a b"` and `"c"` |
| `other_inlay_hints_take_their_placement_from_padding` | kind absent with padding `(false,true)`, `(true,false)`, `(false,false)`, `(true,true)`, absent/absent, and kind `3` with `(false,true)` → `Prefix, Suffix, Auto, Auto, Auto, Prefix`, kind `Other` throughout |
| `inlay_padding_collapses_when_the_label_has_the_space` | the padding-collapse table (Spot checks) |
| `one_malformed_inlay_hint_does_not_lose_the_set` | the per-entry fixture (Spot checks) → offsets `[5, 19]` |
| `null_inlay_result_clears_the_hints` | `result: null` → `Inlays(Some([]))` |
| `failed_or_undecodable_inlay_result_answers_none` | `failure(2, -32603)` and `result: "x"` → `Inlays(None)` under the ticket |
| `content_modified_reissues_the_inlay_request_once` | `failure(2, -32801)` → the same request as id 3; `failure(3, -32801)` → silent |
| `inlay_keys_are_stable_across_a_refetch_at_the_same_revision` | fetch `[A, A, C]` → keys `k0, k1, k2`; refetch at the same revision `[C, A, A, D]` → `[k2, k0, k1, k3]` with `k3` new; A is `(0,5) ": i32" kind 1`, C is `(1,5) ": i32" kind 1`, D is `(0,5) ": i32" kind 2` |
| `inlay_keys_are_fresh_after_the_revision_moves` | fetch `[A]`, `type_ops` an insert at the end, refetch `[A]` → the key differs |

Refresh (commit 3):

| Test | Fixture → assertion |
|---|---|
| `inlay_refresh_answers_null_and_reaches_every_open_document` | `a.rs` and `b.rs` open, `b.rs` edited and synced → `[{"id": 7, "result": null}]` plus two `InlayRefresh` updates, `(a, Revision(a))` and `(b, Revision(b))`, in registration order. Don't use the `answer` helper (51-66): it asserts no updates |
| `other_refreshes_answer_null_without_updates` | `workspace/semanticTokens/refresh` → `null`, no updates |
| `initialized_refreshes_inlays_for_a_server_without_refresh_support` | `open` before `initialize`; initialize result `{"textDocumentSync": 2, "inlayHintProvider": true}` → `updates == [InlayRefresh(a.rs, Revision(doc))]` |
| `initialized_sends_no_inlay_refresh_without_a_provider` | the same with `{"textDocumentSync": 2}` → no updates |

Interactions (commit 4). The fetch is `[type_hint, parameter_hint, insertable_hint]` unless noted;
"tooltip on part p" means a tooltip gesture at a fresh ticket of the current revision.

| Test | Fixture → assertion |
|---|---|
| `inlay_tooltip_resolves_then_answers_from_the_stored_hint` | tooltip on `type_hint` part 0 → the resolve request above; reply `type_hint()` plus `"tooltip": {"kind": "markdown", "value": "**i32** is 32 bits"}` → `InlayTooltip(Some("**i32** is 32 bits"))`; a second tooltip → answered at once, no messages |
| `resolve_with_the_same_label_adds_part_tooltips` | the resolve reply has the same two parts, part 1 with `"tooltip": "the type"` → tooltip on part 1 answers `"the type"` |
| `resolve_that_restructures_the_label_keeps_the_fetched_parts` | the reply label is `[": ", "i", {"value": "32", "tooltip": "part"}]`, plus `"tooltip": "whole"` → tooltip on part 1 answers `"whole"`; a jump on part 1 still answers the `core.rs` `Unopened` target |
| `resolve_reply_after_an_edit_is_dropped` | tooltip → resolve id 3; `type_ops` an insert; reply to id 3 → silent; a tooltip at the new revision → `InlayTooltip(None)`, no messages |
| `resolve_without_a_tooltip_falls_back_to_the_location_hover` | tooltip on part 1 → resolve id 3; reply `type_hint()` unchanged → no updates, `[location hover id 4]`; hover reply → `InlayTooltip(Some("`struct i32`"))` |
| `inlay_tooltip_hovers_at_a_part_location_in_an_unopened_file` | capabilities `{"textDocumentSync": 2, "hoverProvider": true, "inlayHintProvider": true}` (no resolve); tooltip on part 1 → the location hover request (id 3); hover reply → `` "`struct i32`" `` |
| `inlay_tooltip_without_any_source_answers_none_at_once` | resolve-less server, tooltip on `parameter_hint` (string label, no tooltip) → `InlayTooltip(None)`, no messages |
| `location_hover_into_a_moved_open_document_declines` | `b.rs` open with `CALLEE`; a hint whose part links `file:///b.rs` `span_on(0,3,8)`; edit `b.rs` after the fetch; tooltip → `InlayTooltip(None)`, no messages |
| `failed_resolve_answers_no_tooltip_and_retries_on_the_next_hover` | `failure(3, -32603)` → `InlayTooltip(None)`; the next tooltip sends the resolve again (id 4) |
| `new_inlay_tooltip_supersedes_the_one_in_flight` | two tooltips while the resolve is out → the second sends `[cancel_request(3), resolve id 4]` |
| `label_jump_into_the_same_document_is_a_local_target` | the part links `file:///a.rs` `span_on(1,4,5)` → `Definition(Some(Local(18..19)))` under the gesture's ticket, no messages |
| `label_jump_into_another_open_document_is_an_open_target` | `b.rs` open with `CALLEE`, the part links `span_on(0,3,8)` → `Open { doc_id: b, revision: b's, span: 3..8 }` |
| `label_jump_into_an_open_document_that_moved_is_dropped` | as above, `b.rs` edited after the fetch → `Definition(None)` |
| `label_jump_into_a_document_opened_after_the_fetch_is_dropped` | `b.rs` opened after the fetch → `Definition(None)` |
| `label_jump_into_an_unopened_file_is_an_unopened_target` | `type_hint` part 1 → `Unopened`, `uri == file:///w/core.rs`, `span("a\nb\nc\nlet i32\n") == 10..13` |
| `jump_on_a_part_without_a_location_answers_none` | `type_hint` part 0 → `Definition(None)` |
| `inlay_insert_answers_the_hint_edits_through_hygiene` | insert on `insertable_hint` → `Edits([EditOp::insert(19, ": i32")])` under the gesture's ticket |
| `inlay_insert_without_edits_declines_with_no_edits` | insert on `parameter_hint` → `Edits([])` |
| `interactions_on_a_moved_set_decline` | fetch, `type_ops`, then tooltip / jump / insert at the new revision → `InlayTooltip(None)`, `Definition(None)`, `Edits([])`; no messages |
| `interaction_with_an_unknown_key_declines` | a key no fetch minted → the empty answer |
| `close_forgets_the_inlay_set` | fetch, `close`, `open` again at the same revision → a tooltip with the old key answers `InlayTooltip(None)` |

Body sketch for one of them:

```rust
/// A tooltip resolves the hint once; the resolved tooltip answers, and later hovers are
/// answered from the stored hint without asking again.
#[test]
fn inlay_tooltip_resolves_then_answers_from_the_stored_hint() {
    let mut tickets = Counter::new();
    let (mut client, doc) = hinting(INLAY_TEXT, inlay_capabilities());
    let hints = fetch(&mut client, &mut tickets, &doc, 2, json!([type_hint(), parameter_hint(), insertable_hint()]));
    let key = hints[0].hint().key();
    let gesture = inlay::Interaction::tooltip(tickets.issue(doc.revision()), key, 0);
    assert_eq!(
        wire(&client.interact(&doc.snapshot(), &gesture).messages),
        vec![json!({"jsonrpc": "2.0", "id": 3, "method": "inlayHint/resolve", "params": type_hint()})],
        "the hint goes back verbatim",
    );
    let mut resolved = type_hint();
    resolved["tooltip"] = json!({"kind": "markdown", "value": "**i32** is 32 bits"});
    let output = client.receive(reply(3, resolved)).expect("the reply is accepted");
    let (stamp, markdown) = tooltip(only(&output));
    assert_eq!(stamp, update::Stamp::Ticket(gesture.ticket()), "the tooltip answers the gesture");
    assert_eq!(markdown.as_deref(), Some("**i32** is 32 bits"), "the resolved tooltip, in the card's subset");
    let again = inlay::Interaction::tooltip(tickets.issue(doc.revision()), key, 0);
    let output = client.interact(&doc.snapshot(), &again);
    assert!(output.messages.is_empty(), "a resolved hint is not resolved again");
    assert_eq!(tooltip(only(&output)).1.as_deref(), Some("**i32** is 32 bits"), "answered from the stored hint");
}
```

No new tests in scrive-iced: the `land` arms run through `sync_lsp` only from Phase 7, which tests
them end to end. The Phase 6 gate for scrive-iced is that `--all-features` builds and its suite
stays green.

## Files changed

| File | Commit | Change |
|---|---|---|
| `crates/scrive-lsp/src/client.rs` | 1 | `target(requester, &Snapshot, revisions, key, range)`; free `revision_of`; `defined`/`renamed` call sites |
| | 2 | `Client.next_inlay`, `Tracked.inlays`; `Kind::Inlays`; `Query::Inlays`; arms in `reissue`, `resolved`, `failed`, `is_command`, `kind`, `method`, `caret`, `request`, `send`; `Client::inlays`; `inlaid` |
| | 3 | the dedicated `respond` arm; `answer` fan-out; `refreshes`; `initialized` fan-out |
| | 4 | `Kind::Tooltip`; `Query::{Resolve, LocationHover}` and their arms; `revisions` for `Inlays`; `Client::interact`; `declined`; `hover_location`; `tooltip_resolved`; `location_hovered` |
| `crates/scrive-lsp/src/client/capabilities.rs` | 2 | advertise `inlayHint` and `workspace.inlayHint`; `Resolve`; `Server.inlay` |
| `crates/scrive-lsp/src/inlay.rs` (new) | 2, 4 | `decode`, `placed`, `label_texts`, `same_hint`, `Set::install`; commit 4: `Set` getters, `Stored` methods, tooltip lowering |
| `crates/scrive-lsp/src/update.rs` | 2, 3, 4 | `Change::Inlays`, `Change::InlayRefresh`, `Change::InlayTooltip` |
| `crates/scrive-lsp/src/hover.rs` | 4 | `contents` → `pub(crate)` |
| `crates/scrive-lsp/src/lib.rs` | 2, 4 | `mod inlay;`; crate doc bullet |
| `crates/scrive-lsp/src/client/tests.rs` | 2, 3, 4 | helpers and the tests above |
| `crates/scrive-iced/src/code_editor/lsp.rs` | 2, 3, 4 | the `Inlays`, `InlayRefresh` and `InlayTooltip` arms; the stamp comment |

## Verification

At every commit boundary:

```
cargo test -p scrive-lsp
cargo test -p scrive-iced --features lsp
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

At the end of the phase:

```
cargo test --workspace
cargo test --workspace --all-features
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --all-features
cargo build --workspace --all-targets --all-features --target wasm32-unknown-unknown
for c in scrive-core scrive-lsp; do out=$(cargo tree -p $c -e normal --prefix none) || exit 1; if grep -qE '^(iced|winit|wgpu)' <<<"$out"; then echo "$c LEAK"; exit 1; fi; done
rustfmt --edition 2021 crates/scrive-lsp/src/inlay.rs
```

Patches: `git add -N crates/scrive-lsp/src/inlay.rs` before the first `git diff <BASE> -- .
':(exclude).claude' > .claude/map/inlay-hints/patches/phase6-<k>.patch`. `<BASE>` is Phase 5's
last commit, given in the dispatch.

## Spot-check tables

### `inlayHintProvider` → `Server.inlay`

| Server sends | `Server.inlay` |
|---|---|
| absent | `None` |
| `false` | `None` |
| `true` | `Some(Unsupported)` |
| `{}` | `Some(Unsupported)` |
| `{"resolveProvider": false}` | `Some(Unsupported)` |
| `{"resolveProvider": true}` | `Some(Supported)` |
| `{"resolveProvider": true, "documentSelector": null, "id": "inlays"}` | `Some(Supported)` (decodes as `Options`) |

### Conversion

| `kind` | `paddingLeft`/`Right` | label | → kind, placement, padding (L,R) |
|---|---|---|---|
| 1 | `false`/`false` | `": i32"` | Type, Suffix, (no, no) |
| 1 | `true`/`false` | `": i32"` | Type, Suffix, (yes, no) |
| 2 | `false`/`true` | `"x:"` | Parameter, Prefix, (no, yes) |
| 2 | `false`/`true` | `"x: "` | Parameter, Prefix, (no, **no**): the label carries the space |
| absent | `false`/`true` | `"<'_>"` | Other, Prefix, (no, yes) |
| absent | `true`/`false` | `" = usize"` | Other, Suffix (raw flags), (**no**, no) |
| absent | `false`/`false` | `"&*"` | Other, Auto, (no, no) |
| absent | `true`/`true` | `[" a", "b "]` | Other, Auto, (no, no) |
| absent | absent/absent | `"x"` | Other, Auto, (no, no) |
| 3 | `false`/`true` | `"y"` | Other, Prefix, (no, yes) |

| Other field | → |
|---|---|
| part with `location` | `Link::Jumps`; without, `Link::None`; a string label is one `Link::None` part |
| `textEdits` absent or `[]` | `Insert::Unavailable`; non-empty → `Available` |
| position line ≥ `line_count` | dropped |
| position line outside `[span start row − 1, span end row + 1]` | dropped |
| character past the line end / inside a surrogate pair | clamped / snapped left (`Encoding::offset`) |

### The per-entry decode fixture

```json
[
  {"position": {"line": 0, "character": 5}, "label": ": i32", "kind": 1},
  {"position": "nowhere", "label": "x"},
  {"position": {"line": 0, "character": 10}, "label": [{"value": "T", "location": {"uri": "file:///bad path.rs", "range": <span_on(0,0,1)>}}]},
  {"label": "no position"},
  7,
  {"position": {"line": 1, "character": 5}, "label": ": i32", "kind": 1}
]
```

The result is two hints, at offsets 5 and 19. A whole result that isn't an array or `null` (for
example `"x"`) answers `Inlays(None)`.

### Pending-table additions

| `Query` | `Kind` | method | `caret` | `failed` answers | ContentModified |
|---|---|---|---|---|---|
| `Inlays { span }` | `Inlays` | `textDocument/inlayHint` | `span.start` | `Inlays(None)` | re-sent once, same span |
| `Resolve { key, part, hint }` | `Tooltip` | `inlayHint/resolve` | `0` | `InlayTooltip(None)` | re-sent once, same raw hint |
| `LocationHover { uri, position }` | `Tooltip` | `textDocument/hover` | `0` | `InlayTooltip(None)` | re-sent once, same location |

`send` records `revisions` for `Definition`, `Rename` and `Inlays`.

### Gates

| Entry | Unregistered doc | Ticket/snapshot/synced disagree | Not running / no provider | Otherwise |
|---|---|---|---|---|
| `inlays` | nothing | nothing | `Inlays(Some([]))` | request (supersedes) |
| `interact` | nothing | the empty answer | the empty answer | set at the ticket's revision and key known → the gesture; else the empty answer |

### Tooltip chain

| Stored tooltip (part's, else hint's) | Server resolves, `data`, not yet resolved | Hovered part has a location, server hovers, target not moved | Result |
|---|---|---|---|
| yes | — | — | `InlayTooltip(Some)` at once |
| no | yes | — | `inlayHint/resolve`; then the row below or the first row |
| no | no | yes | `textDocument/hover` at the location → `InlayTooltip(markdown)` |
| no | no | no | `InlayTooltip(None)` at once |

### Refresh routing

| Event | Client output |
|---|---|
| `workspace/inlayHint/refresh` while `Running` | `null` response + `InlayRefresh` per tracked document, `Revision(synced)` |
| the same while `Initializing` | `null` response only |
| the same after `shutdown()` | `null` response only |
| `initialize` answered, provider present | the handshake messages + `InlayRefresh` per tracked document |
| `initialize` answered, no provider | the handshake messages only |
| `workspace/semanticTokens/refresh` | `null` response only |

### Key matching (one revision)

| Previous set (server order) | New answer | New keys |
|---|---|---|
| `A k0, A k1, C k2` | `C, A, A, D` | `k2, k0, k1, k3` |
| `A k0` | `A, A` | `k0`, then a new key |
| `A k0` at revision 3 | `A` at revision 4 | a new key |

## What NOT to change

- `sync_lsp`, `route` and its `debug_assert!(applied.jump.is_none())`, `apply_lsp`, `save_lsp`,
  `jump` and `close_lsp`. Phase 7 changes all of them. Nothing in scrive-iced calls
  `Client::inlays` or `Client::interact` in this phase.
- The `Edits` arm of `land`. Removing the hint before `try_edit` is Phase 7 (D19).
- scrive-core, and Phase 5's scheduler, slots and setters. If a `land` arm needs something Phase
  5 didn't provide, stop and report.
- READMEs, examples and the scrive-lsp README's "What it does not do" list (Phase 7).
- Don't advertise `label.location`, `label.command` or `textEdits` as lazily resolvable, and
  don't set `dynamicRegistration: true` (D15).
- Don't sort, dedupe or regroup hints in the client (D4), and don't send `$/cancelRequest` on
  anything but a same-kind supersede.
- No `$/progress` tracking, work-done refresh, label-part `command`s or chunk cache: all out of
  scope in the plan.
- Never run `cargo fmt`. Run `rustfmt --edition 2021` only on `inlay.rs`.

## Pitfalls

- **`Change` stays exhaustive.** Add each variant and its `land` arm in the same commit, or
  `--all-features` breaks. Don't add `#[non_exhaustive]`, and don't add a `_` arm to `land`.
- **Dead code per commit.** `Set.snapshot`, `Set.revisions`, `Stored.raw`, `Stored.resolved` and
  the `Resolve`/`LocationHover` machinery exist only in commit 4. Recording `revisions` for
  `Inlays` in commit 2 is harmless (`Pending.revisions` is read), but install it in commit 4
  together with `Set.revisions`.
- **Name clashes.** `crate::inlay` (scrive-lsp) vs `scrive_core::intel::inlay`; client.rs's
  private `Kind` vs `intel::inlay::Kind`; `lsp_types::request::HoverRequest` vs
  `scrive_core::HoverRequest`. Disambiguate by path. No `use … as …`.
- **Part indices.** `Interaction`'s `part` indexes the core label, and the client reads
  `hint.label` parts with the same index. Pass every server part to core in order. A string label
  has no parts on the LSP side, so `Stored::location` and the part-tooltip lookup return `None`
  for it.
- **`InlayHintServerCapabilities` is untagged**, and `InlayHintOptions` accepts any object. A
  registration-options object decodes as `Options`, which carries the same `resolve_provider`,
  so the result is right either way. Keep both arms.
- **`lsp_types::InlayHint` has no `PartialEq`.** Compare fields (`same_hint`). It derives
  `Serialize`, but a resolve must send the **raw** `Value`, not a re-serialisation that would
  drop unknown fields.
- **The `answer` test helper** (client/tests.rs:51-66) asserts that a server request yields no
  updates. The refresh tests call `client.receive` directly.
- **`settled` drops replies behind the synced revision** (client.rs:767-772). Tests that edit
  between a request and its reply must sync through `type_ops`, and expect silence.
- **The refresh arm must come before the generic `workspace/*/refresh` arm** in `respond`.
  Placed after it, the dedicated arm can never match. The fan-out still works, because it lives
  in `answer`, which hides the mistake from the tests.
- **Associated consts are not patterns.** Match `lsp_types::request::*::METHOD` with a guard, or
  with the literal pinned by a test.
- **`hygiene` line-diffs a single edit that starts at 0 and reaches the last line**
  (edits.rs:66-71). A hint edit on a one-line document can take that path. The result is still a
  correct trimmed batch, so don't special-case it.
- **Borrows in `interact` and `tooltip_resolved`.** `stored` borrows `self.tracked`. Clone the
  raw hint or the `Location` into a local before calling `self.send`/`self.hover_location`.
  `inlaid` destructures `let Self { tracked, next_inlay, .. } = self;` for disjoint borrows.
- **`Snapshot::offset_to_point` on a span end past the text.** Clamp with `.min(snapshot.len())`
  before the call. The editor's span is from the same revision, but the client must not trust
  it.
- **wasm and purity.** `HashMap` is fine. No clock and no threads; the debounce is the editor's.

## Resolved questions

1. **How the client hands `Placement` to `Hint`:** `Hint::new` defaults it from the kind, and the
   client passes every hint's placement with `.placement(..)` (R5).
2. **Padding rule vs collapse order:** the side comes from the raw flags, the collapse comes after
   (R5).
3. **Declines for a stale gesture:** the empty answer for the action under the ticket, so the slot
   settles; an unregistered document gets nothing (R17).
4. **`interact` when the server isn't running:** declines with the empty answer (R17).
5. **The scheduling entry for `InlayRefresh`:** `self.wait_inlays(INLAY_EDIT_DELAY, None)` (R9).
6. **"The part's, else the hint's":** yes, the part's tooltip, else the hint's, before and after a
   resolve (R6).
7. **Stable-key identity:** position, kind and label part texts, ordered multiset, same revision
   only (R18).
8. **Refresh fan-out outside `Running`:** only while running (R17).
9. **A failed resolve** doesn't mark the hint resolved; a successful one without a tooltip does
   (R17).
10. **Location hover without `hoverProvider`:** none (R17).
11. **`Edits(vec![])` under the insert ticket:** Phase 7's `Edits` arm settles the slot and returns
    without `try_edit` (R16).
12. **The plan's "sort" test** means the client does not reorder
    (`inlay_answer_keeps_the_server_order_including_ties`, R18).
13. **Line drift** in scrive-lsp citations: small, no plan change needed.

Still open: none.
