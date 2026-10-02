# Minimap: refresh check diagnostics on save

**Slug**: stale-diagnostics-after-edit
**Created**: 2026-09-27

## Checklist

- [x] Phase 1 — scrive-lsp: `didSave` (2738115)
- [x] Phase 2 — scrive-iced: `CodeEditor::save_lsp` (9b557c4)
- [x] Phase 3 — rust_analyzer example: Ctrl+S saves (8e72665)

> The orchestrator ticks each box once its phase is committed and has cleared review.

## Goal

**The bug.** In the `rust_analyzer` example, fixing the type error on line 11 leaves the error squiggle
in place.

**The cause.** The error comes from rust-analyzer's flycheck (`cargo check` over the file on disk,
`source: "rustc"`). rust-analyzer re-runs the check only on `textDocument/didSave`. scrive never sends
that notification, and the example never writes the buffer back to disk.

**What we ruled out.** A probe against real rust-analyzer showed that:
- our incremental sync is exact;
- every diagnostics set passes the client and glue gates;
- the editor applies each set.

**The fix.**
- Add save support to scrive-lsp.
- Add a glue method that syncs, then saves.
- Bind Ctrl+S in the example, so saving re-runs the check and clears the fixed error. In the probe it
  cleared about 0.1 s after `didSave`.

## Current state

The evidence comes from a probe against real rust-analyzer 02dede3ce5; the logs are in the session
scratchpad.

- **Server side.** rust-analyzer advertises `textDocumentSync = {"change":2,"openClose":true,"save":{}}`:
  it wants saves, without `includeText`.
- **What we advertise.** `crates/scrive-lsp/src/client/capabilities.rs:95` sends
  `synchronization: TextDocumentSyncClientCapabilities::default()`, so `didSave` is never advertised.
- **What we read from the server.** `Server::new` (`capabilities.rs:37-48`) reads `openClose` and
  `change`, and ignores `save`.
- **No save notification exists.** `crates/scrive-lsp/src/client.rs` has no `didSave`. `sync`
  (`client.rs:551`) is the only document notification after open.
- **The example never saves.**
  - `crates/scrive-iced/examples/rust_analyzer.rs:471-475` only calls `sync_lsp` on editor events.
  - The scratch file is written once, at `:421` inside `fn scratch`.
  - `App::new(&Workspace)` (`:439`) doesn't keep the file path.
- **Line endings.** `Document::text()` is LF-only. The buffer normalizes CRLF at load and keeps
  `EolFlavor` (`buffer.rs:213-230`). `Document::serialize(flavor)` (`document.rs:1358`) and
  `Buffer::eol_flavor()` (`buffer.rs:337`) re-expand it.
- **Stale entries between saves.** rust-analyzer republishes the stale `rustc` entries at their on-disk
  coordinates until the next save. That is the expected `checkOnSave` behaviour, and it's documented
  rather than fixed.
- **`registerCapability`** is acknowledged and ignored (`client.rs:1184`). We advertise `didSave`
  without `dynamicRegistration`, so servers use their static `save` option.
- **Ctrl+S is free.**
  - The widget's `interpret_key` returns `None` for Ctrl/Cmd chords it doesn't know.
  - `find_chord` only matches f, h, Escape, Alt+Enter and Tab.
  - text_input has no Ctrl+S.
  - So `keyboard::listen()`, which yields only ignored events, sees it.
- **Scope change.** MAP_PLAN listed "save notifications" as out of scope. This minimap brings `didSave`
  into scope, recorded as D4. `willSave` and `willSaveWaitUntil` stay out.

## Target state

**scrive-lsp**
- `capabilities::Server` models the server's save option as an owned enum, `Save { Never, Notify,
  WithText }`, rather than a bare `bool`:
  - `Supported(false)`, a missing `save`, and a bare `Kind` → `Never`;
  - `Supported(true)`, and `SaveOptions { include_text: None | Some(false) }` → `Notify`;
  - `include_text: Some(true)` → `WithText`.
- `pub fn save(&self, snapshot: &Snapshot) -> Output` sends `textDocument/didSave` only when all of
  these hold:
  - the client is `Running`;
  - the document is tracked and open;
  - the server's option isn't `Never`;
  - `snapshot.revision() == tracked.synced.revision()`, so the saved text provably is the synced text.
    This mirrors the revision gates on the requests.
- With `WithText`, `text` is the synced snapshot's (LF-normalized) text.
- Otherwise `save` returns `Output::default()`.
- It changes no state. The doc says the host syncs first, and that a save before the handshake needs no
  replay, because the server reads the disk at startup.
- It advertises `synchronization.didSave: Some(true)`.

**scrive-iced glue**
- `#[must_use = "the didChange and didSave must be sent to the server"] pub fn save_lsp(&mut self,
  client: &mut Client) -> Vec<Message>`, gated on `cfg(feature = "lsp")`.
- It runs `sync_lsp` first, then appends `self.route(client.save(&self.doc.snapshot()))`, so no future
  update is dropped.
- It is a no-op for an unregistered editor (D3).
- Its doc says to call it after the document has been written to disk.

**The rust_analyzer example**
- `App` keeps `file: PathBuf`.
- Ctrl+S (Cmd+S on macOS), with no Shift, no Alt and no key repeat, goes through a non-capturing
  `fn save_chord(keyboard::Event) -> Option<Message>` fed to `keyboard::listen().filter_map(save_chord)`.
- It writes `doc.serialize(doc.buffer().eol_flavor())` to `file`, preserving CRLF files, then sends
  `save_lsp`'s messages through the link.
- The status bar shows "saved <file name>". It adds " — cargo check running…" only while
  `Link::Connected`, and on failure it shows the write error.
- There is no dirty tracking, because `CodeEditor` doesn't expose `mark_saved`. That's a separate
  feature.

**Docs**
- scrive-lsp README: "six methods", and a save line under "What it covers".
- The module doc of the glue, the `Client` struct doc, and the `Server::new` doc.
- The READMEs' rust_analyzer mention: check diagnostics refresh on Ctrl+S.
- D4 in `.claude/DECISIONS.md`.

## Constraints

- The same rules as the lsp-bridge MAP apply:
  - `~/.claude/guides/RUST_STYLE.md` and `OPAQUE.md` — no raw bool, which is why `Save` is an owned
    enum;
  - `.claude/map/lsp-bridge/DISPATCH.md` for the comment policy and commit rules
    (`/commit-and-comment`);
  - the `/iced` skill for scrive-iced and the example.
- scrive-lsp stays headless and I/O-free (MAP_PLAN Constraints). `save` only builds a message; the
  example does the write.
- One `Client` per `CodeEditor` (D5), and lenient glue (D3).
- `Client::save`'s rustdoc must not intra-doc-link to `CodeEditor`. scrive-lsp doesn't depend on
  scrive-iced, and doc runs with `-D warnings`.
- Every commit must be green on:
  - clippy `-D warnings`, with default features and with `--all-features`;
  - `cargo test --workspace --all-features`;
  - doc;
  - the wasm32 all-features build, with the example native only.
- Conventional Commits with no attribution. Never `cargo fmt`.

## Out of scope

- **`willSave` and `willSaveWaitUntil`.** rust-analyzer doesn't need them.
- **Autosave.** That's host policy.
- **Dirty or unsaved tracking on `CodeEditor`.** A separate feature.
- **Hiding stale `rustc` diagnostics between saves.** They are the server's data.
- **Saves in the scripted `lsp` example.** Its server computes diagnostics from the text it's sent.
- **Advertising `window.workDoneProgress`.** A separate follow-up, already noted in the handoff.

## Phases

### Phase 1 — scrive-lsp: `didSave`

**Goal:** a host can tell the server a document was saved, and servers that ask for saves get them.

**Files:**
- `crates/scrive-lsp/src/client/capabilities.rs`
- `crates/scrive-lsp/src/client.rs`
- `crates/scrive-lsp/src/client/tests.rs`
- `.claude/DECISIONS.md` (D4, not committed, since `.claude/` is untracked)

**Steps:**
1. `capabilities.rs`:
   - Add the `Save` enum and `Server::save`, parsed from
     `TextDocumentSyncCapability::Options(o).save: Option<TextDocumentSyncSaveOptions>` using the
     mapping in the Target state. lsp-types 0.97's type is untagged `Supported(bool) | SaveOptions {
     include_text: Option<bool> }`.
   - Update the `Server::new` doc.
   - Advertise `did_save: Some(true)` in `synchronization`.
2. `client.rs`:
   - Add `pub fn save(&self, snapshot: &Snapshot) -> Output`, per the Target state.
   - Build it with `message::Notification::new::<notification::DidSaveTextDocument>(DidSaveTextDocumentParams {
     text_document: TextDocumentIdentifier { uri: key.uri().clone() }, text })`.
   - Add `save` to the `Client` struct doc.
3. `client/tests.rs`:
   - `save_notifies_when_the_server_asks`: rust-analyzer's `"save": {}`; no text.
   - `save_includes_text_only_when_asked`: `{"includeText": true}` sends the synced text.
   - `save_follows_every_save_option_shape`: a missing `save`, `save: false`, `save: true`, and a bare
     `Kind`. Only `true` sends.
   - `save_of_an_unsynced_snapshot_sends_nothing`: an edit is drained but not synced.
   - `save_before_open_or_after_shutdown_sends_nothing`, plus the `Initializing` case: open before the
     handshake, `save` sends nothing, and after `initialized` only the `didOpen` goes out.
   - A `("/params/capabilities/textDocument/synchronization/didSave", json!(true))` row in the pointer
     table of `initialize_advertises_encodings_versions_and_workspace_capabilities` (`tests.rs:198`).
4. Append D4 to `.claude/DECISIONS.md`. It records:
   - `didSave` comes into scope, with the flycheck evidence;
   - the `Save` enum and its mapping;
   - the revision gate;
   - `save_lsp`'s D3 leniency.

**Exit criteria:**
- the tests pass;
- clippy (both feature sets), test, doc and wasm are all green.

Commit: `feat(lsp): didSave notifications`.

### Phase 2 — scrive-iced: `CodeEditor::save_lsp`

**Goal:** a host saves with one call that syncs first, so the server holds the saved text.

**Files:**
- `crates/scrive-iced/src/code_editor/lsp.rs`
- `crates/scrive-lsp/README.md`

**Steps:**
1. Add `save_lsp` per the Target state. Update the module doc's list of methods.
2. In the glue tests, add `"save": {}` to the `ready()` fixture's `textDocumentSync`. It is
   rust-analyzer's shape, and no existing test reads it. Then add:
   - `save_lsp_syncs_then_sends_did_save`: an edit, then `save_lsp`. One batch comes back: `didChange`
     at version v, then `didSave` for the document's URI with no text.
   - `save_lsp_on_an_unregistered_editor_does_nothing`.
3. In the scrive-lsp README, change "five methods" to six, add `save_lsp`, and add a save line to "What
   it covers".

**Exit criteria:**
- the tests pass;
- both clippy runs, tests, doc and wasm are green.

Commit: `feat(iced): CodeEditor::save_lsp`.

### Phase 3 — rust_analyzer example: Ctrl+S saves

**Goal:** in the example, Ctrl+S writes the buffer, keeping its line endings, and re-runs `cargo
check`, so a fixed error clears.

**Files:**
- `crates/scrive-iced/examples/rust_analyzer.rs`
- `README.md`
- `crates/scrive-iced/README.md` (byte-identical to `README.md`)

**Steps:**
1. Add `file: PathBuf` to `App` and `Message::Save`.
2. Add `fn save_chord(event: keyboard::Event) -> Option<Message>`:
   - it matches `KeyPressed { key: Key::Character(c), modifiers, repeat: false, .. }`;
   - `c == "s"`, `modifiers.command()`, and neither `shift()` nor `alt()`.
   - Batch `keyboard::listen().filter_map(save_chord)` with the existing subscriptions.
3. On `Save`:
   - `std::fs::write(&self.file, doc.serialize(doc.buffer().eol_flavor()))`.
   - On success, send `editor.save_lsp(&mut client)` through the link (queued while connecting), and
     set the status per the Target state.
   - On error, put the error in the status bar.
4. Tests:
   - `save_chord_matches_only_plain_ctrl_s`: Ctrl+S matches. These don't: Ctrl+Shift+S, Alt, a repeat,
     plain `s`.
   - Extend and rename the ignored real-server test to
     `rust_analyzer_reports_the_scratch_crates_type_error_and_clears_it_on_save`, and update its `///`
     invariant. After the initial "mismatched" diagnostic:
     1. Apply a pinned fix through `editor.edit`, e.g. replace `doubled;` on line 11 with
        `doubled.to_string();`, or with the user's `let label = "doubled";` line.
     2. Write `serialize(eol_flavor)` to the file, then call `save_lsp`.
     3. Wait (with a timeout) for a publish at the post-edit version that has no "mismatched" entry.
     4. Require a quiet window of about 2 s in which no publish brings it back.

     Don't add the "sync without save still shows it" guard. rust-analyzer only republishes when its
     native diagnostics change, so that guard could hang.
5. READMEs: extend the rust_analyzer sentence to say `cargo check` diagnostics refresh on save (Ctrl+S).
   Check the two files are identical with `cmp`.

**Exit criteria:**
- the example builds and its tests pass;
- `cargo test -p scrive-iced --features lsp --example rust_analyzer -- --ignored` passes against real
  rust-analyzer;
- the wasm32 all-features build is green.

Commit: `feat(examples): save with Ctrl+S in the rust-analyzer example`.

## Critique log

**Round 1 — 2026-09-27 (Plan agent).** The diagnosis and the split held up. All findings were adopted:
- **HIGH:** saving would have rewritten CRLF files as LF. It now writes
  `serialize(buffer().eol_flavor())`.
- **MEDIUM:**
  - `App` lacked the path, so it now keeps `file: PathBuf`.
  - `mark_saved` isn't reachable from `CodeEditor`, so that step is dropped and dirty tracking is out of
    scope.
  - The real-server test could pass without proving anything. It now pins the edit and waits for the
    post-edit version plus a quiet window.
  - The glue fixture lacked `save`, which would have made the tests vacuous. `"save": {}` is added to
    `ready()`.
  - "Sync first" was a contract in docs only. `save` now takes `&Snapshot` and gates on the synced
    revision, and it's `&self`.
- **LOW-MEDIUM:**
  - The status bar went stale and lied when disconnected. It now shows "saved <file>", with the check
    note only while connected.
  - Doc drift: the scrive-lsp README "five methods", the glue module doc, the `Client` and
    `Server::new` docs, and no intra-doc link across crates.
- **LOW:**
  - Missing save-shape tests (none, `true`, `Initializing`), plus the pointer-table convention for the
    advertise check.
  - The chord details are pinned: `save_chord` with no Shift or Alt, `repeat: false`, `command()`, and
    a unit test.
  - The scope change is recorded as D4, and the owned `Save` enum replaces `Option<{include_text:
    bool}>`.
- **NIT:** `must_use` gets a reason, and `route(...)` replaces `.messages`.
