# Phase 5 — Client core: multi-document sync, diagnostics, lifecycle, `uri::Key`, server requests

Read `MAP_PLAN.md` first. This doc implements the Phase 5 bullet list and decisions D4, D5, D6,
D9, D10, D20 and D21 (plus the `Stamp::Ticket` exception from Constraints). It does not change any
design decision; open details are marked **Decision:** with a one-line reason.

> **Tickets.** `update::Stamp::Ticket` arrives in this phase but no test here needs a ticket. Phase
> 2's `scrive_core::intel::ticket::Counter` (`new()`, `issue(Revision) -> Ticket`) is the only way to
> mint one (D11); Phase 6+ tests use it through one helper.

## 1. Prerequisites

Phases 1–4 are committed. Verify:

- `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` are green.
- Phase 4 files exist: `crates/scrive-lsp/src/{lib.rs, message.rs, encoding.rs}`, and
  `Encoding::{negotiate, kind, offset, position, span, range}` and
  `message::{Request::new, Notification::new, Response::ok, Response::error}` compile as described
  in MAP_PHASE_4.md.
- Phase 1's change log API. This doc assumes these names; grep and adapt call sites (not the
  design) if Phase 1 spelled them differently:

  ```rust
  // scrive_core::document
  impl Document { pub fn doc_id(&self) -> DocId; pub fn drain_changes(&mut self) -> document::Changes; }
  impl Changes {                                   // private fields: doc_id, from, entries
      pub fn doc_id(&self) -> DocId;
      pub fn from(&self) -> Option<Revision>;       // None = the log broke (cap hit)
      pub fn iter(&self) -> impl Iterator<Item = &Change>;
  }
  impl Change {                                    // private fields: before, ops
      pub fn before(&self) -> &Snapshot;
      pub fn ops(&self) -> &[EditOp];               // descending, ties in rope order
  }
  ```

  `grep -n "pub struct Changes\|pub struct Change\b\|pub fn from\|pub fn before\|pub fn ops" crates/scrive-core/src/document.rs`.
- Phase 2's `Ticket` is at `scrive_core::Ticket` (re-exported from `intel::ticket`) and is
  `Copy + Clone + PartialEq + Eq + Debug`. If it is not `Copy`, `update::Stamp` loses `Copy` and
  `stamp()` returns `&Stamp` — adapt, and note it in your report.

## 2. Goal and exit criteria

A `Client` that negotiates a server, keeps any number of documents in sync, gates and caches
diagnostics, runs the initialize/shutdown lifecycle, and answers server requests — all as a pure
function of its inputs.

Exit criteria:

1. `cargo test -p scrive-lsp` passes the Phase 4 tests plus these:
   - `uri.rs`: `ordinary_file_escapes_decode`, `localhost_authority_is_dropped`,
     `scheme_and_drive_letter_are_lowercased`, `reserved_and_invalid_utf8_escapes_are_kept`,
     `non_ascii_is_percent_encoded_in_upper_case`, `non_file_uris_pass_through`,
     `normalized_uris_round_trip_through_uri_from_str`.
   - `diagnostics.rs`: `missing_or_unknown_severity_maps_to_error`,
     `numeric_and_string_codes_become_strings`.
   - `client/tests.rs` (conversations):
     `initialize_advertises_encodings_versions_and_workspace_capabilities`,
     `handshake_sends_initialized_configuration_and_deferred_did_open_with_latest_text`,
     `sync_before_initialize_sends_nothing`,
     `utf16_server_receives_incremental_ranges_in_utf16`,
     `undo_of_a_typing_run_syncs_incrementally_in_one_did_change`,
     `multi_commit_drain_is_sent_as_one_did_change`,
     `broken_chain_falls_back_to_full_text`, `foreign_doc_id_changes_fall_back_to_full_text`,
     `capped_log_falls_back_to_full_text`, `full_sync_server_receives_whole_text`,
     `none_sync_sends_nothing_but_advances_the_synced_snapshot`,
     `unchanged_revision_sends_nothing`,
     `diagnostics_with_a_stale_version_are_dropped`,
     `diagnostics_without_a_version_apply_at_the_synced_revision`,
     `unopened_diagnostics_are_cached_and_applied_at_open`,
     `empty_publish_clears_the_cache`, `versioned_publish_for_an_unopened_uri_clears_the_cache`,
     `version_high_water_mark_survives_reopen`, `duplicate_uri_is_refused`,
     `close_sends_did_close`, `shutdown_sends_shutdown_then_exit_on_its_response`,
     `shutdown_error_response_still_sends_exit`, `shutdown_before_initialize_exits_silently`,
     `server_requests_after_shutdown_get_null`, `client_calls_decline_after_shutdown`,
     `failed_initialize_is_a_server_error_and_drops_deferred_opens`,
     `configuration_answers_dotted_sections`, `workspace_folders_answers_the_root`,
     `housekeeping_server_requests_get_null`, `apply_edit_is_declined`,
     `undecodable_configuration_params_answer_invalid_params`,
     `unknown_server_request_answers_method_not_found`,
     `unhandled_notifications_pass_through`, `undecodable_publish_is_a_decode_error`,
     `unknown_response_id_is_silent`.
2. Clippy and doc clean with `-D warnings`; the wasm32 build passes; the headless gate passes.

## 3. Design decisions implemented

- **D4.** Every entry point returns `#[must_use] Output { messages, updates }`; `receive` and `open`
  return `Result<Output, Error>`. No outbox, I/O, clock or threads. `receive` takes no snapshot:
  a publish is converted against that document's last *synced* snapshot. `update::Document` has
  private `doc_id`, `stamp`, `change` with accessors and `into_parts()` (the documented boundary
  exception). `Stamp` is `Ticket(Ticket)` or `Revision(Revision)` — both arrive now (the plan's one
  exception to "variants arrive when produced"; `Ticket` is produced from Phase 6).
  `Change::Diagnostics` is the only change variant this phase.
- **D5.** One `Client` per server; documents keyed by `DocId`, registered with a `uri::Key` and a
  language id. `Error::DuplicateUri` when two documents claim one URI. Before `initialized`, `open`
  and `sync` only store the snapshot; the deferred `didOpen` sends the latest text.
  `client::Id` is process-unique (Phase 9 stores it in the editor).
- **D6.** `uri::normalize(&Uri) -> uri::Key` (smart-constructor newtype) for `file:` URIs: lower
  the scheme and drive letter; decode every escape except those decoding to `/ % ? #`; drop
  `localhost`; re-encode with one fixed RFC 3986 set (non-ASCII percent-encoded, invalid-UTF-8
  escapes kept). Other schemes pass through. Every URI we send is a `Key`'s.
- **D9.** `didOpen` sends LF text at the next version. An unchanged revision sends nothing.
  Incremental sync when the server supports it *and* the chain check passes; each entry converted
  against its own `before`, all entries in one `didChange`. Full text for `Full` servers, a failed
  chain or a broken log. `NONE`, an omitted `change`, or `openClose: false` send nothing but still
  advance the synced snapshot; a bare `TextDocumentSyncKind` means `openClose: true`.
- **D10.** A per-`uri::Key` version high-water mark that survives `close`; every
  `didOpen`/`didChange` takes the next number. Advertise `versionSupport`. Diagnostics apply only if
  their `version`, when present, equals the last synced version; missing means "the last synced".
  An unversioned publish for a URI that isn't open is cached (latest set per URI; empty removes),
  converted and applied at `open`, unless a versioned publish arrives first.
- **D20.** Builder `.root(Uri)`, `.initialization_options(Value)`, `.configuration(Value)`,
  `.process_id(u32)`, `.build() -> (Client, initialize)`. Initialize params: `processId`, `rootUri`,
  `rootPath` (justified `#[allow(deprecated)]`), `workspaceFolders`, `clientInfo`,
  `initializationOptions`. Advertise `workspace.configuration`, `workspace.workspaceFolders`,
  `general.positionEncodings`, synchronization, `publishDiagnostics.versionSupport`. After
  `initialized`, send `workspace/didChangeConfiguration` if a configuration is set. A failed
  `initialize` returns `Err(Error::Server)` and drops the deferred opens. `close(doc_id)` sends
  `didClose`. `shutdown()` sends only `shutdown`; `exit` goes out when its response arrives (error
  responses included). Before initialization completes, `shutdown()` moves straight to `Exited`.
  After `shutdown()`, server requests get a `null` success and every client call declines.
- **D21.** Server requests are answered inside `Ok` per the table in §4.6. Unhandled notifications
  become `Update::Notification`. `Err` only for undecodable notification/response payloads, server
  errors (here: `initialize`) and `DuplicateUri`.

## 4. Step-by-step changes

Module layout after this phase:

```
crates/scrive-lsp/src/
├── lib.rs
├── client.rs             Client, client::{Builder, Id, Output, Error}
├── client/
│   ├── capabilities.rs   what we advertise; what the server offered
│   └── tests.rs          conversation tests (#[cfg(test)])
├── diagnostics.rs        lsp → scrive diagnostics (crate-private)
├── encoding.rs           (Phase 4)
├── message.rs            (Phase 4)
├── update.rs             Update, update::{Document, Stamp, Change}
└── uri.rs                uri::{Key, normalize}
```

**Decision:** the conversation tests live in `client/tests.rs` (declared `#[cfg(test)] mod tests;`
in `client.rs`). They are colocated with the client but keep `client.rs` readable; this adds one
file to the plan's Phase 5 file list.

### 4.1 `lib.rs`

```rust
//! (extend the Phase 4 crate doc with:)
//! - the client state machine → [`Client`] ([`client`])
//! - what the client hands back → [`Update`] ([`update`])
//! - URI identity → [`uri`]

pub mod client;
mod diagnostics;
pub mod encoding;
pub mod message;
pub mod update;
pub mod uri;

pub use client::{Client, Error, Output};
pub use encoding::Encoding;
pub use lsp_types;
pub use message::Message;
pub use update::Update;
```

Update `README.md` with one paragraph on `Client` (the host loop from MAP_PLAN.md "Target state",
as prose, without the editor glue).

### 4.2 `uri.rs`

```rust
//! URI identity: one normalized form per document, used to register, look up and send.
//!
//! Servers and hosts spell the same file differently (`file:///C%3A/x`, `file:///c:/x`,
//! `file://localhost/x`), and lsp-types' `Uri` compares raw strings, so every URI is normalized
//! once into a [`Key`] and compared only as a key.

use core::fmt;
use std::str::FromStr;

use lsp_types::Uri;

/// A normalized URI. Two spellings of one `file:` path produce equal keys.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key(Uri);

impl Key {
    /// The normalized URI, for sending to a server.
    #[must_use]
    pub fn uri(&self) -> &Uri { &self.0 }

    /// The normalized URI text.
    #[must_use]
    pub fn as_str(&self) -> &str { self.0.as_str() }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) }
}

/// Normalizes `uri` into a [`Key`]. For `file:` URIs: the scheme and a drive letter are lowered,
/// `localhost` is dropped, and the path is re-encoded with one fixed set — every escape is decoded
/// except those decoding to `/`, `%`, `?` or `#` (they would change the path's structure) and
/// bytes that are not UTF-8; non-ASCII is percent-encoded in upper-case hex. Other schemes pass
/// through unchanged.
#[must_use]
pub fn normalize(uri: &Uri) -> Key {
    let raw = uri.as_str();
    let Some((scheme, rest)) = raw.split_once(':') else { return Key(uri.clone()) };
    if !scheme.eq_ignore_ascii_case("file") {
        return Key(uri.clone());
    }
    // Query and fragment are rare on file URIs and pass through verbatim.
    let split = rest.find(['?', '#']).unwrap_or(rest.len());
    let (hier, tail) = rest.split_at(split);
    let mut out = String::from("file:");
    let path = match hier.strip_prefix("//") {
        Some(after) => {
            let (authority, path) = after.split_at(after.find('/').unwrap_or(after.len()));
            out.push_str("//");
            if !authority.eq_ignore_ascii_case("localhost") {
                out.push_str(authority);
            }
            path
        }
        None => hier,
    };
    out.push_str(&normalize_path(path));
    out.push_str(tail);
    // The output only ever contains unreserved, sub-delim, `:@/` characters and `%XX`, all of
    // which fluent-uri accepts; falling back keeps the function total without an `expect`.
    Uri::from_str(&out).map_or_else(|_| Key(uri.clone()), Key)
}
```

Path re-encoding (`raw` is ASCII, since it came out of a parsed `Uri`):

```rust
/// Decodes and re-encodes one path, then lowers a leading drive letter.
fn normalize_path(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    // Bytes decoded from a run of consecutive escapes; flushed as UTF-8 where valid.
    let mut run = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let Some(byte) = hex_pair(bytes.get(i + 1..i + 3)) {
                if matches!(byte, b'/' | b'%' | b'?' | b'#') {
                    flush(&mut run, &mut out);
                    push_escape(&mut out, byte);
                } else {
                    run.push(byte);
                }
                i += 3;
                continue;
            }
        }
        flush(&mut run, &mut out);
        push_byte(&mut out, bytes[i]);
        i += 1;
    }
    flush(&mut run, &mut out);
    lower_drive_letter(out)
}

/// Emits decoded bytes: valid UTF-8 as characters (re-encoded where the fixed set requires),
/// invalid sequences as their original escapes.
fn flush(run: &mut Vec<u8>, out: &mut String) {
    for chunk in run.utf8_chunks() {
        for c in chunk.valid().chars() {
            let mut buf = [0; 4];
            for &byte in c.encode_utf8(&mut buf).as_bytes() {
                push_byte(out, byte);
            }
        }
        for &byte in chunk.invalid() {
            push_escape(out, byte);
        }
    }
    run.clear();
}

/// The fixed RFC 3986 path set: unreserved, sub-delims, `:`, `@` and `/` stay literal; every other
/// byte (including all non-ASCII) is escaped.
fn push_byte(out: &mut String, byte: u8) {
    if byte.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/".contains(&byte) {
        out.push(byte as char);
    } else {
        push_escape(out, byte);
    }
}

fn push_escape(out: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    out.push('%');
    out.push(HEX[usize::from(byte >> 4)] as char);
    out.push(HEX[usize::from(byte & 0xF)] as char);
}

fn hex_pair(pair: Option<&[u8]>) -> Option<u8> {
    let pair = core::str::from_utf8(pair?).ok()?;
    u8::from_str_radix(pair, 16).ok()
}

/// `/C:/…` → `/c:/…`: Windows paths are case-insensitive in the drive letter, and VS Code sends
/// it lower-case.
fn lower_drive_letter(mut path: String) -> String {
    let bytes = path.as_bytes();
    if bytes.len() >= 3
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && bytes.get(3).map_or(true, |&b| b == b'/')
    {
        path[1..2].make_ascii_lowercase();
    }
    path
}
```

`<[u8]>::utf8_chunks` is stable since Rust 1.79 (toolchain here is 1.98). `hex_pair` must reject a
`+`/`-` sign that `from_str_radix` would accept: check both bytes with `is_ascii_hexdigit` first
(write it that way; the sketch above is abbreviated).

**Decision:** the authority other than `localhost` is kept verbatim (not lower-cased). The plan
names only `localhost`; UNC hosts are rare and case-folding them has no fixture.

### 4.3 `update.rs`

```rust
//! What the client hands back: document-bound changes, and notifications it passes through.

use scrive_core::{DocId, Revision, Ticket};
use scrive_core::Diagnostic;

use crate::message;

/// One thing a host must act on.
#[derive(Clone, Debug)]
pub enum Update {
    /// A change bound for one open document.
    Document(Document),
    /// A server notification the client does not consume (`window/logMessage`, `$/progress`, …).
    Notification(message::Notification),
}

/// A change for one document, stamped with what it is valid against.
#[derive(Clone, Debug)]
pub struct Document {
    doc_id: DocId,
    stamp: Stamp,
    change: Change,
}

/// What a [`Document`] update is valid against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stamp {
    /// The answer to the editor request that carried this ticket.
    Ticket(Ticket),
    /// Computed against this document revision (diagnostics, workspace edits).
    Revision(Revision),
}

/// The change itself.
#[derive(Clone, Debug)]
pub enum Change {
    /// The document's full diagnostic set, replacing the previous one.
    Diagnostics(Vec<Diagnostic>),
}

impl Document {
    pub(crate) fn new(doc_id: DocId, stamp: Stamp, change: Change) -> Self {
        Self { doc_id, stamp, change }
    }

    /// The document this change is for.
    #[must_use]
    pub fn doc_id(&self) -> DocId { self.doc_id }

    /// What the change is valid against.
    #[must_use]
    pub fn stamp(&self) -> Stamp { self.stamp }

    /// The change.
    #[must_use]
    pub fn change(&self) -> &Change { &self.change }

    /// All three parts, for hosts that route changes without the `CodeEditor` glue. This is the
    /// boundary where the bundle's guarantee ends: the caller now owns checking the stamp.
    #[must_use]
    pub fn into_parts(self) -> (DocId, Stamp, Change) { (self.doc_id, self.stamp, self.change) }
}
```

`Stamp::Ticket` is public and unconstructed until Phase 6; public items are never dead code.

### 4.4 `diagnostics.rs`

```rust
//! Server diagnostics converted to scrive's.

use lsp_types::{DiagnosticSeverity, NumberOrString};
use scrive_core::{Diagnostic, Severity, Snapshot};

use crate::Encoding;

/// Converts a published set against the snapshot the server saw.
pub(crate) fn convert(encoding: Encoding, snapshot: &Snapshot, diagnostics: &[lsp_types::Diagnostic]) -> Vec<Diagnostic> {
    diagnostics
        .iter()
        .map(|diagnostic| {
            let mut out = Diagnostic::new(
                encoding.span(snapshot, diagnostic.range),
                severity(diagnostic.severity),
                diagnostic.message.clone(),
            );
            out.code = diagnostic.code.as_ref().map(|code| match code {
                NumberOrString::Number(n) => n.to_string(),
                NumberOrString::String(s) => s.clone(),
            });
            out
        })
        .collect()
}

/// A missing severity is the client's call per the spec; an error is the safe reading, since
/// hiding a real error as a hint is worse than the reverse. Unknown values read the same way.
fn severity(severity: Option<DiagnosticSeverity>) -> Severity {
    match severity {
        Some(DiagnosticSeverity::WARNING) => Severity::Warning,
        Some(DiagnosticSeverity::INFORMATION) => Severity::Info,
        Some(DiagnosticSeverity::HINT) => Severity::Hint,
        _ => Severity::Error,
    }
}
```

`scrive_core::Diagnostic` is `#[non_exhaustive]`: build it with `Diagnostic::new` and assign the
public `code` field; a struct literal does not compile outside scrive-core. `DiagnosticSeverity`
derives `PartialEq + Eq`, so its associated consts work as patterns; the `_` arm is over a foreign
newtype, not an enum we own.

### 4.5 `client/capabilities.rs`

```rust
//! What the client advertises, and the subset of the server's capabilities it acts on.

use lsp_types::{
    ClientCapabilities, GeneralClientCapabilities, PublishDiagnosticsClientCapabilities,
    ServerCapabilities, TextDocumentClientCapabilities, TextDocumentSyncCapability,
    TextDocumentSyncClientCapabilities, TextDocumentSyncKind, WorkspaceClientCapabilities,
};

use crate::Encoding;

/// The capabilities sent in `initialize`.
pub(crate) fn client() -> ClientCapabilities {
    ClientCapabilities {
        workspace: Some(WorkspaceClientCapabilities {
            configuration: Some(true),
            workspace_folders: Some(true),
            ..WorkspaceClientCapabilities::default()
        }),
        text_document: Some(TextDocumentClientCapabilities {
            synchronization: Some(TextDocumentSyncClientCapabilities::default()),
            publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
                version_support: Some(true),
                ..PublishDiagnosticsClientCapabilities::default()
            }),
            ..TextDocumentClientCapabilities::default()
        }),
        general: Some(GeneralClientCapabilities {
            // Preference order: utf-8 needs no conversion walk at all.
            position_encodings: Some([Encoding::Utf8, Encoding::Utf16, Encoding::Utf32].map(Encoding::kind).to_vec()),
            ..GeneralClientCapabilities::default()
        }),
        ..ClientCapabilities::default()
    }
}

/// The server's capabilities, reduced to what the client acts on.
#[derive(Debug)]
pub(crate) struct Server {
    /// Whether `didOpen`/`didClose` are sent.
    pub(crate) open_close: bool,
    /// How `didChange` is sent; anything but FULL/INCREMENTAL means "not at all".
    pub(crate) change: TextDocumentSyncKind,
}

impl Server {
    pub(crate) fn new(capabilities: &ServerCapabilities) -> Self {
        let (open_close, change) = match &capabilities.text_document_sync {
            // A bare kind implies open/close notifications (the spec's shorthand).
            Some(TextDocumentSyncCapability::Kind(kind)) => (true, *kind),
            Some(TextDocumentSyncCapability::Options(options)) => (
                options.open_close.unwrap_or(false),
                options.change.unwrap_or(TextDocumentSyncKind::NONE),
            ),
            None => (false, TextDocumentSyncKind::NONE),
        };
        Self { open_close, change }
    }
}
```

**Decision:** an absent `textDocumentSync` means no open/close and no changes; an options object
without `openClose` means `false`. Both are the spec's defaults. `TextDocumentSyncKind` is `Copy`.

Phases 6–8 add fields to `Server` and to `client()`. Every field added must be read in its phase.

### 4.6 `client.rs`

Types:

```rust
//! The LSP client: a pure state machine from JSON-RPC messages and document snapshots to
//! messages to send and updates to apply.

mod capabilities;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use lsp_types::{
    ApplyWorkspaceEditResponse, ClientInfo, ConfigurationParams, DidChangeConfigurationParams,
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    InitializeParams, InitializeResult, InitializedParams, PublishDiagnosticsParams,
    TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem, TextDocumentSyncKind,
    Uri, VersionedTextDocumentIdentifier, WorkspaceFolder,
};
use scrive_core::{document, DocId, Revision, Snapshot};
use serde_json::Value;

use crate::message::{self, Message};
use crate::update::{self, Update};
use crate::{diagnostics, uri, Encoding};

/// JSON-RPC's "method not found": a server request this client does not implement.
const METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC's "invalid params": a server request whose params do not decode.
const INVALID_PARAMS: i64 = -32602;

/// One connection to one language server. See the crate docs for the host loop.
#[derive(Debug)]
pub struct Client {
    id: Id,
    state: State,
    encoding: Encoding,
    root: Option<Uri>,
    configuration: Option<Value>,
    /// Registered documents, in registration order (deterministic `didOpen` order at initialize).
    tracked: Vec<Tracked>,
    /// Per-URI version high-water marks. Survive `close`, so a reopened document continues its
    /// count and a late publish for the old incarnation can never match.
    versions: HashMap<uri::Key, i32>,
    /// The latest unversioned diagnostics for URIs that are not open.
    cached: HashMap<uri::Key, Vec<lsp_types::Diagnostic>>,
    next_request: i64,
}

/// A process-unique client identity, so an editor can tell which client it is registered with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Id(u64);

static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);

/// Configures and builds a [`Client`].
#[must_use]
#[derive(Debug, Default)]
pub struct Builder {
    root: Option<Uri>,
    initialization_options: Option<Value>,
    configuration: Option<Value>,
    process_id: Option<u32>,
}

/// What an entry point hands back: messages for the transport and updates for the documents.
#[must_use]
#[derive(Clone, Debug, Default)]
pub struct Output {
    /// Messages to send to the server, in order.
    pub messages: Vec<Message>,
    /// Changes and notifications for the host, in order.
    pub updates: Vec<Update>,
}

/// Why an entry point had nothing to send.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A notification or response payload did not decode as the method's type.
    #[error("`{method}` payload does not decode: {source}")]
    Decode {
        /// The method whose payload failed.
        method: String,
        /// The decoding error.
        source: serde_json::Error,
    },
    /// The server answered a request with an error.
    #[error("server failed `{method}`: {error}")]
    Server {
        /// The document the request was for; `None` for `initialize`.
        doc_id: Option<DocId>,
        /// The request's method.
        method: String,
        /// The server's error object.
        error: message::Error,
    },
    /// Another open document is already registered under this URI.
    #[error("`{uri}` is already open as another document")]
    DuplicateUri {
        /// The normalized URI.
        uri: uri::Key,
    },
}

/// The connection lifecycle.
#[derive(Debug)]
enum State {
    /// `initialize` is in flight.
    Initializing { request: message::Id },
    /// Initialized; the server's capabilities are known.
    Running(capabilities::Server),
    /// `shutdown` is in flight.
    ShuttingDown { request: message::Id },
    /// Nothing more goes out except answers to server requests.
    Exited,
}

/// One registered document.
#[derive(Debug)]
struct Tracked {
    doc_id: DocId,
    key: uri::Key,
    language: String,
    /// What the server has (or, before `didOpen`, will get).
    synced: Snapshot,
    /// The version last sent; `None` while the server has not been told about the document.
    version: Option<i32>,
}
```

**Decision:** `Error::Server.doc_id` is `Option<DocId>`. D12 gives it a `doc_id`, but D20's failed
`initialize` has no document; `None` covers that without a second variant.

**Decision:** `Error::Decode { method, source }` is the variant for "payloads that don't decode"
(D21). The plan names the case but not the variant.

Builder and lifecycle:

```rust
impl Client {
    /// A builder for a new client.
    pub fn builder() -> Builder { Builder::default() }

    /// This client's process-unique identity.
    #[must_use]
    pub fn id(&self) -> Id { self.id }

    /// The negotiated position encoding (utf-16 until `initialize` answers).
    #[must_use]
    pub fn encoding(&self) -> Encoding { self.encoding }
}

impl Builder {
    /// The workspace root, sent as `rootUri`, `rootPath` and the one workspace folder.
    pub fn root(mut self, root: Uri) -> Self { self.root = Some(root); self }
    /// Sent verbatim as `initializationOptions`.
    pub fn initialization_options(mut self, options: Value) -> Self { … }
    /// The settings object: pushed after `initialized` and read by `workspace/configuration`.
    pub fn configuration(mut self, configuration: Value) -> Self { … }
    /// The host's process id, sent as `processId` (the client cannot read it: wasm has none).
    pub fn process_id(mut self, process_id: u32) -> Self { … }

    /// The client and the `initialize` request to send first.
    #[must_use]
    pub fn build(self) -> (Client, Message) {
        let request = message::Id::Number(1);
        let initialize = message::Request::new::<lsp_types::request::Initialize>(request.clone(), self.initialize_params());
        let client = Client {
            id: Id(NEXT_CLIENT.fetch_add(1, Ordering::Relaxed)),
            state: State::Initializing { request },
            encoding: Encoding::default(),
            root: self.root,
            configuration: self.configuration,
            tracked: Vec::new(),
            versions: HashMap::new(),
            cached: HashMap::new(),
            next_request: 2,
        };
        (client, Message::Request(initialize))
    }

    // `rootUri` and `rootPath` are deprecated in favour of `workspaceFolders`, but servers still
    // read them (pyright reads `rootPath`), so all three are sent.
    #[allow(deprecated)]
    fn initialize_params(&self) -> InitializeParams {
        InitializeParams {
            process_id: self.process_id,
            root_uri: self.root.clone(),
            root_path: self.root.as_ref().and_then(root_path),
            initialization_options: self.initialization_options.clone(),
            capabilities: capabilities::client(),
            workspace_folders: self.root.as_ref().map(|root| vec![folder(root)]),
            client_info: Some(ClientInfo {
                name: "scrive-lsp".to_owned(),
                version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            }),
            ..InitializeParams::default()
        }
    }
}
```

`build` borrows `self` for `initialize_params` before moving fields out — order the code so it
compiles (compute the params first). `folder(root)` builds `WorkspaceFolder { uri, name }` with the
name set to the last non-empty path segment (or the whole URI text when there is none).
`root_path(root)` returns the percent-decoded path of a `file:` root, with the leading `/` dropped
before a drive letter (`/c:/x` → `c:/x`), and `None` for other schemes.

**Decision:** the initialize request always has id `1` and later requests count up from `2`. Tests
can then name the ids literally.

**Decision:** `build` returns `(Client, Message)` — the `initialize` in the plan's `(Client,
initialize)` is the ready-to-send message.

`next_request`:

```rust
impl Client {
    fn next_request(&mut self) -> message::Id {
        let id = self.next_request;
        self.next_request += 1;
        message::Id::Number(id)
    }
}

/// The next version for `key`, advancing its high-water mark. A free function so callers can hold
/// a `&mut Tracked` at the same time.
fn next_version(versions: &mut HashMap<uri::Key, i32>, key: &uri::Key) -> i32 {
    let version = versions.entry(key.clone()).or_insert(0);
    *version += 1;
    *version
}
```

`open`:

```rust
impl Client {
    /// Registers a document under `uri` and tells the server once it is initialized. Returns any
    /// cached diagnostics for the URI as an update.
    ///
    /// # Errors
    /// [`Error::DuplicateUri`] when another document is registered under the same normalized URI.
    pub fn open(&mut self, snapshot: &Snapshot, uri: &Uri, language: impl Into<String>) -> Result<Output, Error> {
        if matches!(self.state, State::ShuttingDown { .. } | State::Exited) {
            return Ok(Output::default());
        }
        let key = uri::normalize(uri);
        let doc_id = snapshot.doc_id();
        if self.tracked.iter().any(|t| t.key == key && t.doc_id != doc_id) {
            return Err(Error::DuplicateUri { uri: key });
        }
        // Re-registering a document (a new URI, or the same one again) is a close then an open.
        let mut output = self.close(doc_id);
        self.tracked.push(Tracked { doc_id, key: key.clone(), language: language.into(), synced: snapshot.clone(), version: None });
        if let State::Running(server) = &self.state {
            if server.open_close {
                let index = self.tracked.len() - 1;
                output.messages.push(self.did_open(index));
            }
        }
        if let Some(cached) = self.cached.remove(&key) {
            output.updates.push(Update::Document(update::Document::new(
                doc_id,
                update::Stamp::Revision(snapshot.revision()),
                update::Change::Diagnostics(diagnostics::convert(self.encoding, snapshot, &cached)),
            )));
        }
        Ok(output)
    }

    /// `didOpen` for `tracked[index]` at the next version, with its synced (latest) text.
    fn did_open(&mut self, index: usize) -> Message {
        let tracked = &mut self.tracked[index];
        let version = next_version(&mut self.versions, &tracked.key);
        tracked.version = Some(version);
        Message::Notification(message::Notification::new::<lsp_types::notification::DidOpenTextDocument>(
            DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: tracked.key.uri().clone(),
                    language_id: tracked.language.clone(),
                    version,
                    text: tracked.synced.text().into_owned(),
                },
            },
        ))
    }
}
```

`did_open` borrows `self.tracked` and `self.versions` disjointly; if the borrow checker objects,
destructure `let Self { tracked, versions, .. } = self;` first.

**Decision:** re-opening a `DocId` that is already registered closes it first (`didClose`, then
`didOpen`). The plan lists no error for it, and D5's "`load()` of a different file requires
`close_lsp`, then `open_lsp`" makes it a host slip, not a failure.

**Decision:** after `shutdown()`, `open` returns `Ok(Output::default())` and registers nothing —
"every client call declines".

`close`:

```rust
impl Client {
    /// Forgets a document and sends `didClose` if the server was told about it. The version
    /// high-water mark stays.
    pub fn close(&mut self, doc_id: DocId) -> Output {
        let Some(index) = self.tracked.iter().position(|t| t.doc_id == doc_id) else {
            return Output::default();
        };
        let tracked = self.tracked.remove(index);
        let mut output = Output::default();
        if let (State::Running(server), Some(_)) = (&self.state, tracked.version) {
            if server.open_close {
                output.messages.push(Message::Notification(message::Notification::new::<
                    lsp_types::notification::DidCloseTextDocument,
                >(DidCloseTextDocumentParams {
                    text_document: TextDocumentIdentifier { uri: tracked.key.uri().clone() },
                })));
            }
        }
        output
    }
}
```

`sync` — D9 with the chain check:

```rust
impl Client {
    /// Brings the server up to `snapshot`. `changes` is the document's drained change log; when it
    /// chains from what the server has, the edits go out incrementally, otherwise the whole text.
    pub fn sync(&mut self, snapshot: &Snapshot, changes: document::Changes) -> Output {
        let encoding = self.encoding;
        let Some(tracked) = self.tracked.iter_mut().find(|t| t.doc_id == snapshot.doc_id()) else {
            return Output::default();
        };
        // Revisions only grow; an older or equal snapshot has nothing new.
        if snapshot.revision() <= tracked.synced.revision() {
            return Output::default();
        }
        let server = match &self.state {
            State::Running(server) => server,
            // Before `initialized` the snapshot is stored; the deferred `didOpen` sends it.
            State::Initializing { .. } => {
                tracked.synced = snapshot.clone();
                return Output::default();
            }
            State::ShuttingDown { .. } | State::Exited => return Output::default(),
        };
        let content_changes = if !server.open_close || tracked.version.is_none() {
            None
        } else if server.change == TextDocumentSyncKind::INCREMENTAL {
            Some(incremental(encoding, &tracked.synced, snapshot, &changes).unwrap_or_else(|| full(snapshot)))
        } else if server.change == TextDocumentSyncKind::FULL {
            Some(full(snapshot))
        } else {
            None
        };
        // Advanced even when nothing is sent: diagnostics and requests convert against it.
        tracked.synced = snapshot.clone();
        let Some(content_changes) = content_changes else { return Output::default() };
        let version = next_version(&mut self.versions, &tracked.key);
        tracked.version = Some(version);
        Output {
            messages: vec![Message::Notification(message::Notification::new::<lsp_types::notification::DidChangeTextDocument>(
                DidChangeTextDocumentParams {
                    text_document: VersionedTextDocumentIdentifier { uri: tracked.key.uri().clone(), version },
                    content_changes,
                },
            ))],
            updates: Vec::new(),
        }
    }
}

/// The whole document as one content change.
fn full(snapshot: &Snapshot) -> Vec<TextDocumentContentChangeEvent> {
    vec![TextDocumentContentChangeEvent { range: None, range_length: None, text: snapshot.text().into_owned() }]
}

/// The logged edits as ranged content changes, or `None` when `changes` does not lead exactly from
/// `synced` to `snapshot`: another document's log, a log that broke (`from` is `None`), a gap, or
/// an end short of `snapshot`.
///
/// Each entry's ops are descending (ties in the rope's replay order), so each op's range is still
/// valid after the ops before it are applied: every earlier op lies at or after its end. That is
/// why every op converts against its entry's own `before`, and why the whole batch can go out in
/// one `didChange`, which the server applies in order.
fn incremental(
    encoding: Encoding,
    synced: &Snapshot,
    snapshot: &Snapshot,
    changes: &document::Changes,
) -> Option<Vec<TextDocumentContentChangeEvent>> {
    if changes.doc_id() != snapshot.doc_id() || changes.from() != Some(synced.revision()) {
        return None;
    }
    let mut cursor = synced.revision();
    let mut events = Vec::new();
    for change in changes.iter() {
        let before = change.before();
        if before.revision() != cursor {
            return None;
        }
        events.extend(change.ops().iter().map(|op| TextDocumentContentChangeEvent {
            range: Some(encoding.range(before, op.range.clone())),
            range_length: None,
            text: op.text.clone(),
        }));
        cursor = Revision(cursor.0 + 1);
    }
    (cursor == snapshot.revision()).then_some(events)
}
```

`self.state` is read while `tracked` (from `self.tracked`) is mutably borrowed; both are fields of
`self`, so destructure `let Self { tracked, state, versions, encoding, .. } = self;` at the top to
make the borrows disjoint.

**Decision:** `sync` ignores a snapshot whose revision is not newer than the synced one. D9 says an
unchanged revision sends nothing; an *older* one can only come from a host bug and would otherwise
send stale text.

**Decision:** `sync` for a `DocId` that isn't registered returns an empty `Output`.

`shutdown`:

```rust
impl Client {
    /// Starts an orderly shutdown: sends `shutdown`, and `exit` when its response arrives. Before
    /// initialization completes there is nothing to shut down, so the client just exits.
    pub fn shutdown(&mut self) -> Output {
        match self.state {
            State::Initializing { .. } => {
                self.state = State::Exited;
                Output::default()
            }
            State::Running(_) => {
                let request = self.next_request();
                self.state = State::ShuttingDown { request: request.clone() };
                Output {
                    messages: vec![Message::Request(message::Request::new::<lsp_types::request::Shutdown>(request, ()))],
                    updates: Vec::new(),
                }
            }
            State::ShuttingDown { .. } | State::Exited => Output::default(),
        }
    }
}
```

`receive`:

```rust
impl Client {
    /// Folds one message from the server into the client.
    ///
    /// # Errors
    /// [`Error::Decode`] for a payload that does not decode, [`Error::Server`] when `initialize`
    /// fails. Everything else — including answers to server requests — is `Ok`.
    pub fn receive(&mut self, message: Message) -> Result<Output, Error> {
        match message {
            Message::Request(request) => Ok(self.answer(request)),
            Message::Notification(notification) => self.notified(notification),
            Message::Response(response) => self.responded(response),
        }
    }

    fn responded(&mut self, response: message::Response) -> Result<Output, Error> {
        let Some(id) = response.id else { return Ok(Output::default()) };
        match &self.state {
            State::Initializing { request } if *request == id => self.initialized(response.result),
            State::ShuttingDown { request } if *request == id => {
                // Any answer, an error included, completes the handshake.
                self.state = State::Exited;
                Ok(Output {
                    messages: vec![Message::Notification(message::Notification::new::<lsp_types::notification::Exit>(()))],
                    updates: Vec::new(),
                })
            }
            // Phase 6 routes pending request ids here. An id nothing is waiting for is silent.
            _ => Ok(Output::default()),
        }
    }

    fn initialized(&mut self, result: Result<Value, message::Error>) -> Result<Output, Error> {
        let method = "initialize";
        let result = match result {
            Ok(value) => serde_json::from_value::<InitializeResult>(value)
                .map_err(|source| Error::Decode { method: method.to_owned(), source }),
            Err(error) => Err(Error::Server { doc_id: None, method: method.to_owned(), error }),
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                // Nothing can be sent to a server that did not initialize; deferred opens go.
                self.state = State::Exited;
                self.tracked.clear();
                return Err(error);
            }
        };
        self.encoding = Encoding::negotiate(result.capabilities.position_encoding.as_ref());
        let server = capabilities::Server::new(&result.capabilities);
        let mut output = Output::default();
        output.messages.push(Message::Notification(message::Notification::new::<lsp_types::notification::Initialized>(InitializedParams {})));
        if let Some(settings) = &self.configuration {
            output.messages.push(Message::Notification(message::Notification::new::<
                lsp_types::notification::DidChangeConfiguration,
            >(DidChangeConfigurationParams { settings: settings.clone() })));
        }
        let open_close = server.open_close;
        self.state = State::Running(server);
        if open_close {
            for index in 0..self.tracked.len() {
                output.messages.push(self.did_open(index));
            }
        }
        Ok(output)
    }
}
```

**Decision:** an `initialize` result that doesn't decode is treated like a failed `initialize`
(state `Exited`, deferred opens dropped) and returns `Error::Decode`. A client with unknown
capabilities cannot sync.

Notifications (D10):

```rust
impl Client {
    fn notified(&mut self, notification: message::Notification) -> Result<Output, Error> {
        if notification.method != "textDocument/publishDiagnostics" {
            return Ok(Output { messages: Vec::new(), updates: vec![Update::Notification(notification)] });
        }
        let params: PublishDiagnosticsParams = decode(&notification.method, notification.params)?;
        let key = uri::normalize(&params.uri);
        let Some(tracked) = self.tracked.iter().find(|t| t.key == key) else {
            // Unversioned sets for closed files are kept for `open`; a versioned publish (or an
            // empty set) supersedes whatever was cached.
            if params.version.is_some() || params.diagnostics.is_empty() {
                self.cached.remove(&key);
            } else {
                self.cached.insert(key, params.diagnostics);
            }
            return Ok(Output::default());
        };
        if params.version.is_some() && params.version != tracked.version {
            return Ok(Output::default());
        }
        Ok(Output {
            messages: Vec::new(),
            updates: vec![Update::Document(update::Document::new(
                tracked.doc_id,
                update::Stamp::Revision(tracked.synced.revision()),
                update::Change::Diagnostics(diagnostics::convert(self.encoding, &tracked.synced, &params.diagnostics)),
            ))],
        })
    }
}

/// Decodes a payload, naming the method on failure. Absent params decode from `null`.
fn decode<T: serde::de::DeserializeOwned>(method: &str, params: Option<Value>) -> Result<T, Error> {
    serde_json::from_value(params.unwrap_or(Value::Null)).map_err(|source| Error::Decode { method: method.to_owned(), source })
}
```

**Decision:** "unless a versioned publish arrives first" (D10) is read as: a versioned publish for
a URI that isn't open removes that URI's cached set. It cannot be applied (nothing is synced for
it), and it says the server has moved past the unversioned set.

Server requests (D21):

```rust
impl Client {
    fn answer(&self, request: message::Request) -> Output {
        let result = match self.state {
            // After `shutdown()` the client makes no promises; `null` keeps the server unblocked.
            State::ShuttingDown { .. } | State::Exited => Ok(Value::Null),
            State::Initializing { .. } | State::Running(_) => self.respond(&request),
        };
        Output {
            messages: vec![Message::Response(message::Response { id: Some(request.id), result })],
            updates: Vec::new(),
        }
    }

    fn respond(&self, request: &message::Request) -> Result<Value, message::Error> {
        match request.method.as_str() {
            "workspace/configuration" => {
                let params: ConfigurationParams = serde_json::from_value(request.params.clone().unwrap_or(Value::Null))
                    .map_err(|error| message::Error { code: INVALID_PARAMS, message: error.to_string(), data: None })?;
                Ok(Value::Array(params.items.iter().map(|item| self.section(item.section.as_deref())).collect()))
            }
            "workspace/workspaceFolders" => Ok(match &self.root {
                Some(root) => serde_json::json!([folder(root)]),
                None => Value::Null,
            }),
            "client/registerCapability"
            | "client/unregisterCapability"
            | "window/workDoneProgress/create"
            | "window/showMessageRequest" => Ok(Value::Null),
            "workspace/applyEdit" => Ok(serde_json::json!(ApplyWorkspaceEditResponse {
                applied: false,
                failure_reason: Some("scrive-lsp does not apply server-initiated edits".to_owned()),
                failed_change: None,
            })),
            method if method.starts_with("workspace/") && method.ends_with("/refresh") => Ok(Value::Null),
            method => Err(message::Error { code: METHOD_NOT_FOUND, message: format!("`{method}` is not supported"), data: None }),
        }
    }

    /// The dotted `section` of the configuration: empty or absent is the whole value, a missing
    /// path is `null`.
    fn section(&self, section: Option<&str>) -> Value {
        let Some(configuration) = &self.configuration else { return Value::Null };
        match section.filter(|s| !s.is_empty()) {
            None => configuration.clone(),
            Some(path) => path.split('.').try_fold(configuration, |value, key| value.get(key)).cloned().unwrap_or(Value::Null),
        }
    }
}
```

Match on literal method strings, not `<X as Request>::METHOD` — trait associated consts cannot be
patterns, and importing `lsp_types::request::Request` would clash with `message::Request`. The
conversation tests pin every literal.

**Decision:** only `workspace/configuration` decodes its params (it is the only answer that reads
them), so it is the only source of `InvalidParams`.

## 5. Files changed

| File | Change |
|---|---|
| `crates/scrive-lsp/src/lib.rs` | modules, re-exports, crate doc |
| `crates/scrive-lsp/README.md` | one paragraph on `Client` |
| `crates/scrive-lsp/src/client.rs` | new |
| `crates/scrive-lsp/src/client/capabilities.rs` | new |
| `crates/scrive-lsp/src/client/tests.rs` | new (Decision in §4) |
| `crates/scrive-lsp/src/update.rs` | new |
| `crates/scrive-lsp/src/uri.rs` | new, with tests |
| `crates/scrive-lsp/src/diagnostics.rs` | new, with tests |

## 6. Tests

All test bodies use real `scrive_core::Document`s: `Document::new`, `observe_changes(true)`,
`edit`/`edit_grouped`/`undo`, `snapshot()`, `drain_changes()`. Messages are compared as JSON
(`serde_json::to_value`) against `serde_json::json!` fixtures — member order does not matter.

### 6.1 Helpers (`client/tests.rs`)

```rust
use std::str::FromStr;

use scrive_core::{Document, EditOp, GroupingHint, OpClass};
use serde_json::{json, Value};

use super::*;

fn uri(text: &str) -> Uri { Uri::from_str(text).expect("fixture URI parses") }

fn document(text: &str) -> Document {
    let mut doc = Document::new(text).expect("fixture loads");
    doc.observe_changes(true);
    doc
}

fn wire(messages: &[Message]) -> Vec<Value> {
    messages.iter().map(|m| serde_json::to_value(m).expect("serializes")).collect()
}

fn from_server(value: Value) -> Message { serde_json::from_value(value).expect("fixture parses") }

/// A client whose `initialize` has been answered with `capabilities`; returns what the answer sent.
fn running(builder: Builder, capabilities: Value) -> (Client, Output) {
    let (mut client, _) = builder.build();
    let output = client
        .receive(from_server(json!({"jsonrpc": "2.0", "id": 1, "result": {"capabilities": capabilities}})))
        .expect("initialize answer is accepted");
    (client, output)
}

/// The one place tests read a `Change`. Later phases add variants; only this helper changes.
fn diagnostics(update: &Update) -> (DocId, update::Stamp, Vec<scrive_core::Diagnostic>) {
    let Update::Document(document) = update else { panic!("expected a document update, got {update:?}") };
    match document.change() {
        update::Change::Diagnostics(set) => (document.doc_id(), document.stamp(), set.clone()),
    }
}
```

`Change` has one variant in this phase. The `match` above has no wildcard arm on purpose: a `_` arm
would be `unreachable_patterns` under `-D warnings` now, and a `let … else` would be an
`irrefutable_let_patterns` warning. When Phase 6 adds `Completions`, this match stops compiling and
gets its `other => panic!(…)` arm — in this helper only.

`Update` has two variants now (`Document`, `Notification`), so `let … else` on it is fine.

### 6.2 Conversation tests

Server capability fixtures used below:

- `INCREMENTAL`: `json!({"positionEncoding": "utf-16", "textDocumentSync": {"openClose": true, "change": 2}})`
- `FULL`: `json!({"textDocumentSync": 1})`
- `NONE`: `json!({"textDocumentSync": 0})`

| Test | Assertion | Sketch |
|---|---|---|
| `initialize_advertises_encodings_versions_and_workspace_capabilities` | the `initialize` params contain `general.positionEncodings == ["utf-8","utf-16","utf-32"]`, `textDocument.publishDiagnostics.versionSupport == true`, `workspace.configuration == true`, `workspace.workspaceFolders == true`, `processId == 42`, `rootUri`, `rootPath`, `workspaceFolders[0].uri`, `clientInfo.name == "scrive-lsp"`, `initializationOptions` | `Client::builder().root(uri("file:///work/proj")).process_id(42).initialization_options(json!({"a": 1})).build()`; read the message JSON with `pointer("/params/...")` |
| `handshake_sends_initialized_configuration_and_deferred_did_open_with_latest_text` | answer sends, in order: `initialized` with `params: {}`, `workspace/didChangeConfiguration` with the settings, `didOpen` version 1 with the **edited** text | build with `.configuration(json!({"x": 1}))`; `open` a doc before the answer (no messages); edit it; `sync` (no messages); answer initialize with `INCREMENTAL` |
| `sync_before_initialize_sends_nothing` | `open` and `sync` return empty `Output` before the answer | as above, assert both outputs |
| `utf16_server_receives_incremental_ranges_in_utf16` | `didChange` version 2 with `range (0,3)-(0,3)`, `text "b"` | doc `"😀a"`; open; answer `INCREMENTAL`; `edit(vec![EditOp::insert(5, "b")])`; `sync(&doc.snapshot(), doc.drain_changes())` |
| `undo_of_a_typing_run_syncs_incrementally_in_one_did_change` | after undo, one `didChange` with two content changes: delete `(0,3)-(0,4)` then delete `(0,2)-(0,3)`, both `text ""` | doc `"ab"`; answer; two `edit_grouped(vec![EditOp::insert(n, c)], GroupingHint::mergeable(OpClass::Type))` at 2 and 3; drain + sync; `doc.undo()`; drain + sync |
| `multi_commit_drain_is_sent_as_one_did_change` | one `didChange` with the events of both commits, in commit order | two discrete `edit`s, one drain, one sync |
| `broken_chain_falls_back_to_full_text` | a `didChange` whose one event has no `range` and the full text | edit + drain (discarded, not synced); edit + drain; sync with the second drain (its `from` is past `synced`) |
| `foreign_doc_id_changes_fall_back_to_full_text` | full text | two documents; sync doc A's snapshot with doc B's drained `Changes` |
| `capped_log_falls_back_to_full_text` | full text | 1025 discrete edits without draining (the log breaks: `from()` is `None`), then drain + sync |
| `full_sync_server_receives_whole_text` | full text even though the chain passes | answer with `FULL` |
| `none_sync_sends_nothing_but_advances_the_synced_snapshot` | `sync` returns no messages; a later unversioned publish with range `(0,4)-(0,5)` lands at span `4..5` of the *edited* text and `Stamp::Revision(edited revision)` | answer with `NONE` (bare kind ⇒ `didOpen` still sent at answer); edit `"abc"` → `"xyzabc"`; sync; publish |
| `unchanged_revision_sends_nothing` | empty output | sync twice with the same snapshot |
| `diagnostics_with_a_stale_version_are_dropped` | no updates | open (v1); edit + sync (v2); publish `version: 1` |
| `diagnostics_without_a_version_apply_at_the_synced_revision` | one update, `Stamp::Revision(synced revision)`, spans converted against the synced text | publish without `version` |
| `unopened_diagnostics_are_cached_and_applied_at_open` | publish for an unopened URI → no update; later `open` → `Output.updates` holds the converted set, stamped with the opened snapshot's revision; a second `open` of another doc on that URI after close has no cache | |
| `empty_publish_clears_the_cache` | open gets no update | publish non-empty, then empty, then open |
| `versioned_publish_for_an_unopened_uri_clears_the_cache` | open gets no update | publish unversioned, then versioned, then open |
| `version_high_water_mark_survives_reopen` | second `didOpen` has version 3 | open (v1), edit + sync (v2), `close`, open a fresh `Document` on the same URI |
| `duplicate_uri_is_refused` | `Err(Error::DuplicateUri { .. })` and the second doc is not registered | open doc A on `file:///C%3A/a.rs`, doc B on `file:///c:/a.rs` |
| `close_sends_did_close` | `textDocument/didClose` with the normalized URI | |
| `shutdown_sends_shutdown_then_exit_on_its_response` | `shutdown()` → `{"id":2,"method":"shutdown"}` (no params); response `{"id":2,"result":null}` → `exit` (no params) | |
| `shutdown_error_response_still_sends_exit` | error response → `exit` | |
| `shutdown_before_initialize_exits_silently` | no messages; a late initialize answer is silent; `sync` declines | |
| `server_requests_after_shutdown_get_null` | `workspace/configuration` after `shutdown()` → `result: null` | |
| `client_calls_decline_after_shutdown` | `open`, `sync`, `close` return empty `Output` | |
| `failed_initialize_is_a_server_error_and_drops_deferred_opens` | `Err(Error::Server { doc_id: None, method: "initialize", .. })`; afterwards `sync` sends nothing | |
| `configuration_answers_dotted_sections` | request items `["rust-analyzer.check", "", "missing.path", null]` → `[{"command":"clippy"}, whole, null, whole]` | `.configuration(json!({"rust-analyzer": {"check": {"command": "clippy"}}}))` |
| `workspace_folders_answers_the_root` | `[{"uri": "file:///work/proj", "name": "proj"}]`; without a root, `null` | |
| `housekeeping_server_requests_get_null` | `client/registerCapability`, `client/unregisterCapability`, `window/workDoneProgress/create`, `window/showMessageRequest`, `workspace/semanticTokens/refresh` → `result: null` | table loop |
| `apply_edit_is_declined` | `result.applied == false` | |
| `undecodable_configuration_params_answer_invalid_params` | `error.code == -32602` | params `{"items": 3}` |
| `unknown_server_request_answers_method_not_found` | `error.code == -32601` | method `custom/thing` |
| `unhandled_notifications_pass_through` | `Update::Notification` equal to the input (`window/logMessage`, `$/progress`) | |
| `undecodable_publish_is_a_decode_error` | `Err(Error::Decode { method: "textDocument/publishDiagnostics", .. })` | params `{"uri": 3}` |
| `unknown_response_id_is_silent` | `Ok` with empty output | response id 99 |

Body sketch for the undo test:

```rust
/// Undoing a typing run replays several steps; each step's log entry converts against its own
/// `before`, so the server receives exact incremental deletes instead of the whole text.
#[test]
fn undo_of_a_typing_run_syncs_incrementally_in_one_did_change() {
    let mut doc = document("ab");
    let (mut client, _) = running(Client::builder(), json!({"textDocumentSync": 2}));
    let _ = client.open(&doc.snapshot(), &uri("file:///a.rs"), "rust").expect("opens");
    for (at, c) in [(2, "c"), (3, "d")] {
        doc.edit_grouped(vec![EditOp::insert(at, c)], GroupingHint::mergeable(OpClass::Type)).expect("types");
    }
    let _ = client.sync(&doc.snapshot(), doc.drain_changes());
    assert!(doc.undo(), "the typing run undoes");
    let output = client.sync(&doc.snapshot(), doc.drain_changes());
    assert_eq!(
        wire(&output.messages),
        vec![json!({"jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
            "textDocument": {"uri": "file:///a.rs", "version": 3},
            "contentChanges": [
                {"range": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 4}}, "text": ""},
                {"range": {"start": {"line": 0, "character": 2}, "end": {"line": 0, "character": 3}}, "text": ""},
            ],
        }})],
        "undo of a two-step typing run is one didChange with two ranged deletes, newest first",
    );
}
```

A bare `2` for `textDocumentSync` implies `openClose: true`, so `open` after the answer sends
`didOpen` v1 immediately; the first sync is v2 and the undo is v3.

### 6.3 `uri.rs` tests

| Test | Input → normalized |
|---|---|
| `ordinary_file_escapes_decode` | `file:///C%3A/Users/a%40b/x%2By.rs` → `file:///c:/Users/a@b/x+y.rs`; `file:///tmp/a%20b.rs` → unchanged (space stays escaped) |
| `localhost_authority_is_dropped` | `file://localhost/tmp/a.rs` → `file:///tmp/a.rs`; `file://LOCALHOST/tmp/a.rs` → same |
| `scheme_and_drive_letter_are_lowercased` | `FILE:///C:/x.rs` → `file:///c:/x.rs` |
| `reserved_and_invalid_utf8_escapes_are_kept` | `file:///tmp/a%2fb%25c%3Fd%23e` → `file:///tmp/a%2Fb%25c%3Fd%23e`; `file:///tmp/%FF.rs` → unchanged |
| `non_ascii_is_percent_encoded_in_upper_case` | `file:///tmp/%c3%a9.rs` → `file:///tmp/%C3%A9.rs` |
| `non_file_uris_pass_through` | `untitled:Untitled-1`, `https://Example.com/A%3a` → unchanged |
| `normalized_uris_round_trip_through_uri_from_str` | for every normalized output above: `Uri::from_str(key.as_str())` is `Ok`, and `normalize(&that) == key` (idempotent) |

### 6.4 `diagnostics.rs` tests

- `missing_or_unknown_severity_maps_to_error` — severities absent, `1`, `2`, `3`, `4`, `9` (decode
  from JSON) → `Error, Error, Warning, Info, Hint, Error`.
- `numeric_and_string_codes_become_strings` — `"code": 42` → `Some("42")`, `"code": "E0308"` →
  `Some("E0308")`, absent → `None`.

## 7. Verification

```
cargo test -p scrive-lsp
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
cargo build --workspace --all-targets --target wasm32-unknown-unknown
for c in scrive-core scrive-lsp; do out=$(cargo tree -p $c -e normal --prefix none) || exit 1; if grep -qE '^(iced|winit|wgpu)' <<<"$out"; then echo "$c LEAK"; exit 1; fi; done
```

## 8. Spot-check tables

Sync decision (server `change` × chain × state):

| State | open_close | change | Chain | Sent | `synced` advances |
|---|---|---|---|---|---|
| Initializing | — | — | — | nothing | yes |
| Running | true | INCREMENTAL | passes | ranged `didChange` | yes |
| Running | true | INCREMENTAL | fails / broken / foreign | full `didChange` | yes |
| Running | true | FULL | any | full `didChange` | yes |
| Running | true | NONE | any | nothing | yes |
| Running | false | any | any | nothing | yes |
| ShuttingDown / Exited | — | — | — | nothing | no |
| any | — | — | revision ≤ synced | nothing | no |

Version / diagnostics gate:

| Tracked version | Publish version | Result |
|---|---|---|
| 2 | 2 | applied, `Stamp::Revision(synced)` |
| 2 | 1 | dropped |
| 2 | absent | applied at synced |
| `None` (not told) | 1 | dropped |
| not open | absent, non-empty | cached |
| not open | absent, empty | cache entry removed |
| not open | present | cache entry removed |

Lifecycle:

| State | `shutdown()` | Shutdown response | Server request |
|---|---|---|---|
| Initializing | → Exited, nothing sent | — | answered normally |
| Running | sends `shutdown` → ShuttingDown | — | answered normally |
| ShuttingDown | nothing | sends `exit` → Exited | `null` |
| Exited | nothing | silent | `null` |

## 9. What NOT to change

- scrive-core and scrive-iced — nothing. If a Phase 1 accessor is missing, stop and report.
- `message.rs`/`encoding.rs` behavior. Adding a helper there is allowed only if this phase calls it.
- No pending-request table, no `$/cancelRequest`, no completion/signature/hover code (Phases 6–7).
- No `Change` variants beyond `Diagnostics`; no `Update::FileEdits`; no `Error::{StaleEdit,
  Unsupported}` (Phase 8).
- No `workspaceEdit` capability (Phase 8), no `completion`/`hover`/`signatureHelp` capabilities.
- No `Cargo.toml` changes.

## 10. Pitfalls

- **Deprecated lsp-types fields.** `InitializeParams::root_uri` and `root_path` are `#[deprecated]`
  in 0.97. Put `#[allow(deprecated)]` on the one function that builds the params, with the comment
  above it saying why (servers still read them; pyright reads `rootPath`). Do not spread the allow
  wider. `..InitializeParams::default()` itself does not warn.
- **Dead code under `-D warnings`.** `capabilities::Server` fields, `Tracked` fields and private
  helpers must all be read in this phase. `Tracked::language` is read by `did_open`; do not add
  fields later phases need (`session`, `pending`) now.
- **Single-variant `Change` in tests.** See §6.1: one helper, no wildcard, no `let … else`.
- **fluent-uri strictness.** `Uri::from_str` rejects spaces, non-ASCII and bare `%`. Test fixtures
  must be percent-encoded. `normalize` only emits characters fluent-uri accepts; the round-trip
  test proves it.
- **`HashMap` iteration order** is random. Never iterate `versions` or `cached` to build output;
  `tracked` is a `Vec` precisely so `didOpen` order at initialize is deterministic.
- **Borrow splitting.** `sync`, `did_open` and `notified` need a `&mut Tracked` and other fields at
  once; destructure `self` rather than cloning snapshots or keys to dodge the borrow checker.
- **`lsp_types::Range` vs `core::ops::Range`.** Always spell the LSP one in full.
- **wasm.** `AtomicU64` is fine (scrive-core's `DocId` uses it on wasm32). No `std::time`, threads,
  `std::process::id()` (that is why `process_id` is a builder input), or I/O.
- **Method names as literals.** A typo in `"workspace/configuration"` silently turns into
  MethodNotFound; the conversation tests are what catch it — keep one test per literal.
- **`Output` is `#[must_use]`.** Internal calls that discard one (for example `self.close(doc_id)`
  inside `open`) must use it, as the sketch does, or bind it with `let _ =` and a reason.
