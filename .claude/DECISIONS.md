# Decisions

Design calls made during implementation that MAP_PLAN.md didn't settle. Entries are append-only (see
`~/.claude/guides/DECISIONS.md`).

## D1 — Undecodable intel results settle the awaited slot

**Date:** 2026-09-26 · **Context:** lsp-bridge Phase 6, base 4503f39 · **Status:** decided

### Issue
Per D21, a payload that doesn't decode returns `Err(Error::Decode)`. An `Err` carries no update, so a
completion reply that fails to decode never settles the editor's awaited slot.

For signature help this is worse. While a signature request is awaited, `drive_signature` re-queries on
every editor event, so every one of those replies would fail again the same way.

### Options

| | A — keep `Err` | **B — settle with the empty answer** |
|---|---|---|
| Slot | stays until the next edit (completion) or forever re-queried (signature) | settles at once |
| Diagnosability | host sees the decode error | host still sees the raw message it routed in |
| Consistency | differs from server-error handling | same as the audit's rule for server errors |

### Decision
B. Intel requests (completion, signature, hover) always settle with their empty answer. The empty
answers are `Completions([])`, `Signature(None)` and `Hover(None)`, stamped with the pending
ticket. Only these still return `Err`:
- user commands (definition, rename, format), because a user asked for them;
- payloads that are not replies to a request.

### Followups
Phase 7 applies the same rule to signature and hover.

## D2 — Kind-3 completion requests require a list

**Date:** 2026-09-26 · **Context:** lsp-bridge Phase 6 · **Status:** decided

### Issue
`Session::new` starts marked incomplete. Suppose an in-flight request is dropped as stale, and a
later `Continuing` request still passes the D14 checks. That request goes out as
`TriggerForIncompleteCompletions`, even though no list was ever received.

### Decision
A request continues a session only if the session has received a list, or if its own request is
still in flight. Otherwise it starts a fresh session and is sent as `Invoked`, or as
`TriggerCharacter` if a trigger char matched.

### Why
LSP defines kind 3 as a re-trigger of an incomplete list the server already sent. Sending it
without a list misstates the context to a strict server.

## D3 — Glue is lenient on unregistered editors; close closes the popups

**Date:** 2026-09-26 · **Context:** lsp-bridge Phase 9, base 824fa24 · **Status:** decided

### Issue
Two problems with the draft `sync_lsp` and `close_lsp`:
- `sync_lsp` debug-asserted that the editor was registered, so a host that syncs every tab the same
  way panicked in debug builds.
- `close_lsp` left the completion popup, the signature box and the hover card open. Nothing can ever
  land in them after close, and a signature box that stays open keeps recording requests that are
  never sent.

### Decision
- `sync_lsp`, `apply_lsp` and `jump` on an editor that isn't registered do nothing (they return empty).
- A debug-assert remains only for an editor registered with a *different* client. That is the misuse
  D5's one-client-per-editor rule forbids.
- `close_lsp` closes all three popups, alongside the cleanup D19 already requires.

## D4 — `didSave` is in scope

**Date:** 2026-09-27 · **Context:** minimap stale-diagnostics-after-edit, base ae46e1d · **Status:** decided

### Issue
MAP_PLAN listed save notifications as out of scope. In the `rust_analyzer` example a fixed type
error kept its squiggle. The error comes from rust-analyzer's flycheck (`cargo check` over the file
on disk, `source: "rustc"`), which re-runs only on `textDocument/didSave`. A probe against
rust-analyzer 02dede3ce5 showed sync and diagnostics gating were exact; the server advertises
`"save": {}` and was simply never told about a save.

### Decision
- The client advertises `synchronization.didSave` (no `dynamicRegistration`) and reads the server's
  `save` option into an owned enum, `capabilities::Save { Never, Notify, WithText }`:
  - a missing `save`, `save: false`, and a bare sync kind → `Never`;
  - `save: true`, and `SaveOptions` without `includeText: true` → `Notify`;
  - `includeText: true` → `WithText`, carrying the synced (LF-normalized) text.
- `Client::save(&self, &Snapshot)` sends `didSave` only while running, for a document the server
  has open, when the server asks for saves, and when the snapshot's revision is the synced one, so
  the saved text provably is the text the server has. It changes no state, and a save before the
  handshake is not replayed: the server reads the disk when it starts.
- `CodeEditor::save_lsp` syncs, then saves, in one batch. Like the rest of the glue (D3) it does
  nothing on an unregistered editor. Writing the file stays the host's job.
- `willSave` and `willSaveWaitUntil` stay out of scope.

### Why
Without `didSave`, rust-analyzer's check diagnostics never refresh. The revision gate mirrors the
requests' gates and turns "sync first" from a documented contract into a checked one.
