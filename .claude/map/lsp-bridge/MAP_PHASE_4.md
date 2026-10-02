# Phase 4 — scrive-lsp: crate, JSON-RPC envelope, position encoding, CI gate

Read `MAP_PLAN.md` first. This doc implements the Phase 4 bullet list and decisions D1, D2, D3 and D7.
It does not change any design decision; where the plan leaves a detail open, the choice is marked
**Decision:** with a one-line reason.

## 1. Prerequisites

Phases 1, 2 and 3 are committed. Verify before writing code:

- `git log --oneline -5` shows the Phase 3 commit on top of `6cf4f2c`'s descendants, and
  `git status` is clean apart from `.claude/`.
- `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` are green.
- Phase 1's `Snapshot` API exists in `crates/scrive-core/src/buffer.rs`. This doc assumes the
  signatures below; grep for them and adapt the call sites (not the design) if Phase 1 named the
  parameters differently:

  ```rust
  impl Snapshot {
      pub fn offset_to_point(&self, offset: u32) -> Point;           // clamps past-the-end
      pub fn point_to_offset(&self, point: Point) -> u32;            // clamps the row, clamps col to the row length
      pub fn chunks(&self, range: Range<u32>) -> impl Iterator<Item = &str>; // no allocation
      pub fn clip_offset(&self, offset: u32, bias: Bias) -> u32;     // clamps to len, snaps to a char boundary
  }
  ```

  `grep -n "pub fn chunks\|pub fn clip_offset\|pub fn offset_to_point\|pub fn point_to_offset" crates/scrive-core/src/buffer.rs`
  must print four lines. If `chunks` takes no range or `clip_offset` takes no `Bias`, stop and
  report: the encoding core depends on both.
- lsp-types 0.97.0 is in the registry cache:
  `ls ~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/lsp-types-0.97.0/`.
  Its transitive deps (fluent-uri 0.1.4, serde_repr, bitflags 1.3.2) are cached too.
- `.github/workflows/ci.yml` still matches the three-job layout quoted in §4.6.

## 2. Goal and exit criteria

A new workspace member `crates/scrive-lsp` that builds for native and wasm32, with two public
modules:

- `message` — the tolerant JSON-RPC envelope (D2).
- `encoding` — the negotiated position encoding and the allocation-free conversions between LSP
  positions and scrive byte offsets (D7).

Internal crates move to `[workspace.dependencies]` (D1). CI gains a headless-dependency gate and
runs the scrive-lsp lib tests on wasip1.

Exit criteria (all must hold):

1. `cargo build --workspace --all-targets --target wasm32-unknown-unknown` passes.
2. `cargo clippy --workspace --all-targets -- -D warnings` and
   `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` are clean.
3. `cargo test -p scrive-lsp` passes these tests (all in `crates/scrive-lsp/src/`):
   - `message.rs`: `request_parses_without_a_jsonrpc_member`,
     `string_and_integer_ids_parse`, `zero_fraction_float_id_parses_as_an_integer`,
     `fractional_id_is_rejected`, `boolean_id_is_rejected`,
     `error_response_with_null_id_parses`, `error_wins_when_result_and_error_are_both_present`,
     `null_result_is_a_successful_reply`, `message_without_method_result_or_error_is_rejected`,
     `unknown_members_are_ignored`, `params_may_be_an_object_an_array_or_absent`,
     `null_params_read_as_absent`, `method_with_null_id_is_a_notification`,
     `null_params_are_omitted_on_output`, `response_always_carries_result_even_when_null`,
     `error_response_carries_error_and_no_result`, `messages_round_trip_through_json`,
     `message_is_clone_debug_and_send`.
   - `encoding.rs`: `utf8_positions_convert_and_snap_left`,
     `utf16_positions_count_surrogate_pairs`, `utf32_positions_count_scalar_values`,
     `mid_surrogate_position_snaps_left`, `line_at_or_past_line_count_clamps_to_document_end`,
     `maximal_position_clamps_to_document_end`, `character_past_line_end_clamps_to_line_end`,
     `inverted_range_collapses_to_its_end`, `offsets_convert_back_to_positions`,
     `mid_character_offset_snaps_left_before_conversion`,
     `conversion_walks_across_rope_chunk_boundaries`, `text_offsets_treat_crlf_as_one_line_break`,
     `text_line_past_the_end_clamps_to_text_end`, `absent_or_unknown_encoding_negotiates_utf16`,
     `advertised_kinds_negotiate_back_to_themselves`.
4. The headless gate loop from §7 prints nothing and exits 0.
5. The existing workspace tests stay green.

## 3. Design decisions implemented

- **D1 (crate layout).** `scrive-lsp` depends on scrive-core only among internal crates. Internal
  crates move to `[workspace.dependencies]` now, so the Phase 10 version bump edits only the root
  manifest. scrive-iced's `lsp` feature is Phase 9 — do not add it here.
- **D2 (tolerant envelope).** `Message` is `Request | Response | Notification`, serde, and
  `Clone + Debug + Send`. On input: unknown members ignored; `jsonrpc` optional; ids may be
  integers, zero-fraction floats, strings or `null`; when `result` and `error` are both present the
  error wins; `params` may be an object, an array or absent. On output: params that serialize to
  `null` are omitted; a success response always carries `"result"`, even `null`.
- **D3.** lsp-types 0.97 carries the payloads and is re-exported as `scrive_lsp::lsp_types`.
- **D7 (position encoding).** Advertise utf-8, utf-16, utf-32 (Phase 5 builds that list from
  `Encoding::kind`). An absent or unknown negotiated kind means utf-16. `Encoding` is public.
  Clamping: a character past the line end → the line end; a line at or past `line_count` → the
  document end (checked explicitly — the rope's row clamp would land on the last line instead); a
  position inside a character snaps left; an inverted range collapses to its end. The core walks an
  iterator of `&str` chunks without allocating, has an ASCII fast path, and serves both `Snapshot`
  (via `Snapshot::chunks`) and plain disk text (for Phase 8's `FileEdits` and `Jump::Unopened`).
  utf-8 needs no walk.

Constraints that bite here: `#![deny(missing_docs)]`, `#![forbid(unsafe_code)]`, no `mod.rs`, no
aliased imports, no `unwrap()` in library code, module-path names (`message::Request`, not
`JsonRpcRequest`), comments explain why and never narrate the plan, never run `cargo fmt` (you may
`rustfmt --edition 2021` the files you create).

## 4. Step-by-step changes

### 4.1 Root `Cargo.toml`

Add the member and the internal-crate table. Keep everything else byte-identical.

```diff
 [workspace]
 resolver = "2"
-members = ["crates/scrive-core", "crates/scrive-iced"]
+members = ["crates/scrive-core", "crates/scrive-lsp", "crates/scrive-iced"]

 # Shared metadata.
 [workspace.package]
 version = "0.3.0"
 edition = "2021"
 license = "MIT"
 readme = "README.md"
 repository = "https://github.com/robbym/scrive"

+# The internal crates, declared once. A release bump edits the `version`s here
+# and nowhere else; each member opts in with `<crate>.workspace = true`.
+[workspace.dependencies]
+scrive-core = { path = "crates/scrive-core", version = "0.3.0" }
+scrive-lsp = { path = "crates/scrive-lsp", version = "0.3.0" }
+
 # Track iced's git master. ...
```

`scrive-lsp` in the table is unused until Phase 9. Cargo does not warn about unused workspace
dependencies; it is listed now so Phase 10 touches one file (D1).

### 4.2 `crates/scrive-iced/Cargo.toml`

```diff
 [dependencies]
-scrive-core = { path = "../scrive-core", version = "0.3.0" }
+scrive-core.workspace = true
```

Nothing else changes in this file this phase.

### 4.3 `crates/scrive-lsp/Cargo.toml` (new, full)

```toml
[package]
name = "scrive-lsp"
version.workspace = true
edition.workspace = true
license.workspace = true
readme = "README.md"
repository.workspace = true
description = "A headless, I/O-free Language Server Protocol client for scrive: a pure state machine over JSON-RPC messages that syncs scrive-core documents and turns server replies into editor updates."
keywords = ["lsp", "language-server", "editor", "headless"]
categories = ["text-editors", "development-tools"]

# HARD STRUCTURAL RULE: scrive-lsp does no I/O and depends on no GUI crate. The
# host owns the transport and hands this crate one JSON-RPC message at a time;
# every entry point returns the messages to send and the updates to apply. It
# builds for wasm32, and CI runs `cargo tree -p scrive-lsp` and fails if
# `iced`/`winit`/`wgpu` appears in its normal dependency graph.
#
# Dependency budget — each entry earns its place with a consumer:
#   scrive-core — the document model the client syncs: `Snapshot` for position
#                 conversion, `document::Changes` for incremental sync, and the
#                 intel request/result types it answers with.
#   lsp-types   — the LSP payload types, re-exported as `scrive_lsp::lsp_types`
#                 so hosts and the client agree on one version. Pure data plus
#                 serde; wasm-safe. Its `Uri` (fluent-uri) is strict, which is
#                 why `uri::normalize` exists.
#   serde       — the envelope's hand-written (De)Serialize impls, and derives on
#                 the envelope's error object.
#   serde_json  — `Value` for params and results that are decoded lazily, one
#                 payload (or one completion item) at a time.
#   thiserror   — the crate's error enum.
[dependencies]
scrive-core.workspace = true
lsp-types = "0.97"   # LSP payloads (re-exported; never wrapped)
serde = { version = "1", features = ["derive"] }
serde_json = "1"     # envelope params/results as `Value`
thiserror = "2"      # client::Error (Phase 5)

[package.metadata.docs.rs]
all-features = true
```

`thiserror` has no consumer until Phase 5. An unused dependency is not a compiler warning, and the
plan's budget lists it for this crate, so it goes in now with the rest of the manifest.
**Decision:** add it in Phase 4 — the manifest and its budget comment are written once, and Phase 5
does not need to touch `Cargo.toml`.

If the build cannot reach the network, `cargo build --offline` resolves everything from the cache
(lsp-types, fluent-uri, serde_repr and bitflags 1.3.2 are cached; serde/serde_json/thiserror 2 are
already in `Cargo.lock`). Commit the resulting `Cargo.lock` change.

### 4.4 `crates/scrive-lsp/README.md` (new)

Short, present tense, no plan narration. Suggested content:

```markdown
# scrive-lsp

A Language Server Protocol client for the scrive editor that does no I/O.

The host owns the transport (a child process's stdio, a WebSocket, a web worker…) and passes each
incoming JSON-RPC message to the client. The client returns the messages to send back and the
updates to apply to scrive-core documents. It keeps no clock, spawns no threads and builds for
wasm32.

- `message` — a tolerant JSON-RPC 2.0 envelope (`Message`), serde-ready.
- `encoding` — the negotiated position encoding and conversions between LSP positions and scrive
  byte offsets.

Payload types come from `lsp-types`, re-exported as `scrive_lsp::lsp_types`.
```

Phase 5 extends the README when the client lands.

### 4.5 `crates/scrive-lsp/src/lib.rs` (new)

```rust
//! `scrive-lsp` — a Language Server Protocol client for scrive that does no I/O.
//!
//! The host owns the transport. It hands each incoming JSON-RPC [`Message`] to the client and
//! sends whatever the client returns; nothing here reads a socket, a clock or a thread. That keeps
//! the crate a pure state machine that tests as data in, data out, and builds for wasm32.
//!
//! - the JSON-RPC envelope → [`message`]
//! - LSP positions ↔ scrive byte offsets → [`encoding`]
//!
//! Payloads are [`lsp_types`], re-exported so a host and this crate always agree on one version.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod encoding;
pub mod message;

pub use encoding::Encoding;
pub use lsp_types;
pub use message::Message;
```

`pub use lsp_types;` is a re-export, not an alias; it satisfies D3.

### 4.6 `crates/scrive-lsp/src/message.rs` (new)

Module layout: one primary type (`Message`) with the types that exist only to serve it
(`Request`, `Response`, `Notification`, `Id`, `Error`).

```rust
//! The JSON-RPC 2.0 envelope every LSP message travels in.
//!
//! Parsing is tolerant because real servers are: `jsonrpc` may be missing, ids arrive as
//! `1.0`, error replies to unparseable requests carry `"id": null`, and some servers send both
//! `result` and `error`. Output is strict: `jsonrpc: "2.0"` always, params omitted rather than
//! `null`, and a success reply always carries `result`.

use core::fmt;

use serde::de::{self, Deserializer};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One JSON-RPC message, in either direction.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    /// A call that expects a [`Response`] with the same id.
    Request(Request),
    /// The answer to a [`Request`].
    Response(Response),
    /// A one-way message; nothing answers it.
    Notification(Notification),
}

/// A call that expects a response.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    /// Correlates the response with this request.
    pub id: Id,
    /// The LSP method, e.g. `textDocument/completion`.
    pub method: String,
    /// The params object or array; `None` when absent (or `null` on the wire).
    pub params: Option<Value>,
}

/// The answer to a request.
#[derive(Clone, Debug, PartialEq)]
pub struct Response {
    /// The id of the request this answers. `None` for an error reply to a request whose id the
    /// peer could not read (`"id": null`).
    pub id: Option<Id>,
    /// The `result` member, or the `error` member when present (the error wins over a result).
    pub result: Result<Value, Error>,
}

/// A one-way message.
#[derive(Clone, Debug, PartialEq)]
pub struct Notification {
    /// The LSP method, e.g. `textDocument/publishDiagnostics`.
    pub method: String,
    /// The params object or array; `None` when absent (or `null` on the wire).
    pub params: Option<Value>,
}

/// A request id. Floats with a zero fraction read as integers, so `1.0` and `1` match.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(untagged)]
pub enum Id {
    /// An integer id.
    Number(i64),
    /// A string id.
    String(String),
}

/// The `error` member of a failed response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Error {
    /// The JSON-RPC or LSP error code, e.g. `-32601` (method not found) or `-32801` (content
    /// modified).
    pub code: i64,
    /// A human-readable description. Missing on the wire reads as empty.
    #[serde(default)]
    pub message: String,
    /// Extra data the peer attached, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}
```

Display for `Error` (Phase 5's `client::Error::Server` formats it):

```rust
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}
```

Serialize — hand-written, because the three shapes share members and the output rules (params
omitted when null, `result` always present on success) are not expressible with derives:

```rust
impl Serialize for Message {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("jsonrpc", "2.0")?;
        match self {
            Message::Request(request) => {
                map.serialize_entry("id", &request.id)?;
                map.serialize_entry("method", &request.method)?;
                if let Some(params) = request.params.as_ref().filter(|p| !p.is_null()) {
                    map.serialize_entry("params", params)?;
                }
            }
            Message::Notification(notification) => {
                map.serialize_entry("method", &notification.method)?;
                if let Some(params) = notification.params.as_ref().filter(|p| !p.is_null()) {
                    map.serialize_entry("params", params)?;
                }
            }
            Message::Response(response) => {
                // `None` serializes as `"id": null`, which is what an error reply to an
                // unreadable request must carry.
                map.serialize_entry("id", &response.id)?;
                match &response.result {
                    Ok(result) => map.serialize_entry("result", result)?,
                    Err(error) => map.serialize_entry("error", error)?,
                }
            }
        }
        map.end()
    }
}
```

Deserialize — the presence-detecting `result`. `Option<Value>` with a plain `#[serde(default)]`
reads `"result": null` as `None`, the same as an absent member, which would make a successful void
reply (`shutdown`'s `null`) indistinguishable from a malformed message. `present` wraps every value
that *is* on the wire, including `null`, in `Some`; the `default` only fires when the member is
missing.

```rust
/// The wire shape before classification.
#[derive(Deserialize)]
struct Raw {
    // `"id": null` and a missing id both read as `None`: neither identifies a request.
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    method: Option<String>,
    // `"params": null` reads as absent, which is how it is re-serialized.
    #[serde(default)]
    params: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    result: Option<Value>,
    #[serde(default)]
    error: Option<Error>,
}

/// Wraps any on-the-wire value, `null` included, in `Some`; only a missing member stays `None`.
fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

impl<'de> Deserialize<'de> for Message {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Raw::deserialize(deserializer)?;
        let id = raw.id.map(Id::parse).transpose().map_err(de::Error::custom)?;
        match (raw.method, id) {
            (Some(method), Some(id)) => Ok(Message::Request(Request { id, method, params: raw.params })),
            (Some(method), None) => Ok(Message::Notification(Notification { method, params: raw.params })),
            (None, id) => {
                let result = match (raw.error, raw.result) {
                    (Some(error), _) => Err(error),
                    (None, Some(result)) => Ok(result),
                    (None, None) => {
                        return Err(de::Error::custom("message has no `method`, `result` or `error`"))
                    }
                };
                Ok(Message::Response(Response { id, result }))
            }
        }
    }
}

impl Id {
    /// Reads a wire id. Integers and zero-fraction floats up to 2^53 become `Number`.
    fn parse(value: Value) -> Result<Self, &'static str> {
        const EXACT: f64 = 9_007_199_254_740_992.0; // 2^53: every integer below is exact in f64
        match value {
            Value::String(id) => Ok(Id::String(id)),
            Value::Number(number) => number
                .as_i64()
                .or_else(|| number.as_f64().filter(|f| f.fract() == 0.0 && f.abs() <= EXACT).map(|f| f as i64))
                .map(Id::Number)
                .ok_or("id is not an integer"),
            _ => Err("id is neither a number nor a string"),
        }
    }
}
```

**Decision:** a message with a `method` and `"id": null` is a notification. A null id cannot be
answered, so treating it as a request would only produce a reply the peer cannot match.

**Decision:** a message with no `method`, no `result` and no `error` is rejected. That is what
makes the presence detection load-bearing: `{"id":1,"result":null}` is a success,
`{"id":1}` is not a message.

Constructors. These are what Phase 5+ use to build outgoing traffic; they are public (hosts and the
Phase 10 scripted server use them too), so they are never dead code.

```rust
impl Request {
    /// A request for the lsp-types method `R`. Params that serialize to `null` (`()` for
    /// `shutdown`) are left out.
    #[must_use]
    pub fn new<R: lsp_types::request::Request>(id: Id, params: R::Params) -> Self {
        Self { id, method: R::METHOD.to_owned(), params: to_params(&params) }
    }
}

impl Notification {
    /// A notification for the lsp-types method `N`. Params that serialize to `null` (`()` for
    /// `exit`) are left out.
    #[must_use]
    pub fn new<N: lsp_types::notification::Notification>(params: N::Params) -> Self {
        Self { method: N::METHOD.to_owned(), params: to_params(&params) }
    }
}

impl Response {
    /// A success reply. `()` becomes `"result": null`.
    #[must_use]
    pub fn ok(id: Id, result: impl Serialize) -> Self {
        let result = serde_json::to_value(result).expect("lsp-types results always serialize to JSON");
        Self { id: Some(id), result: Ok(result) }
    }

    /// An error reply.
    #[must_use]
    pub fn error(id: Option<Id>, error: Error) -> Self {
        Self { id, result: Err(error) }
    }
}

/// Params as a `Value`, with `null` meaning "absent".
fn to_params(params: &impl Serialize) -> Option<Value> {
    // lsp-types params are plain derived structs with string map keys; serializing them to a
    // `Value` has no failure path.
    match serde_json::to_value(params).expect("lsp-types params always serialize to JSON") {
        Value::Null => None,
        params => Some(params),
    }
}
```

`expect` with a reason is allowed by RUST_STYLE for invariant violations. `serde_json::to_value`
fails only for maps with non-string keys or a custom `Serialize` error; neither exists in lsp-types
params.

`lsp_types::request::Request` and `lsp_types::notification::Notification` are named by full path in
the bounds, so neither trait is imported and nothing clashes with `message::Request`/`Notification`.

### 4.7 `crates/scrive-lsp/src/encoding.rs` (new)

```rust
//! The negotiated position encoding and the conversions between LSP positions and scrive byte
//! offsets.
//!
//! LSP counts a position's `character` in the negotiated unit (utf-8 bytes, utf-16 code units or
//! utf-32 scalar values); scrive counts bytes. Conversions walk one line's text as `&str` chunks —
//! a rope snapshot's own chunks, or a slice of plain text — so nothing is materialized, and an
//! all-ASCII chunk is counted by its length.

use core::ops::Range;

use lsp_types::{Position, PositionEncodingKind};
use scrive_core::{Bias, Point, Snapshot};

/// How LSP positions count characters on a line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Encoding {
    /// utf-8 code units — bytes, scrive's own unit.
    Utf8,
    /// utf-16 code units. The LSP default when nothing was negotiated.
    #[default]
    Utf16,
    /// utf-32 code units — Unicode scalar values.
    Utf32,
}
```

Negotiation:

```rust
impl Encoding {
    /// The encoding a server chose in `capabilities.positionEncoding`. Absent or unknown means
    /// utf-16, the protocol default.
    #[must_use]
    pub fn negotiate(chosen: Option<&PositionEncodingKind>) -> Self {
        match chosen {
            Some(kind) if *kind == PositionEncodingKind::UTF8 => Encoding::Utf8,
            Some(kind) if *kind == PositionEncodingKind::UTF32 => Encoding::Utf32,
            _ => Encoding::Utf16,
        }
    }

    /// The protocol name of this encoding, for `general.positionEncodings`.
    #[must_use]
    pub fn kind(self) -> PositionEncodingKind {
        match self {
            Encoding::Utf8 => PositionEncodingKind::UTF8,
            Encoding::Utf16 => PositionEncodingKind::UTF16,
            Encoding::Utf32 => PositionEncodingKind::UTF32,
        }
    }
}
```

`PositionEncodingKind` wraps a `Cow<'static, str>`, so it cannot be a match pattern; compare with
`==` in guards as above.

The chunk core. `pub(crate)` because Phase 6 memoizes conversions over one materialized line
through it; in this phase the public methods below call it, so it is not dead code.

```rust
impl Encoding {
    /// Code units `c` occupies in this encoding.
    fn width(self, c: char) -> u32 {
        match self {
            Encoding::Utf8 => c.len_utf8() as u32,
            Encoding::Utf16 => c.len_utf16() as u32,
            Encoding::Utf32 => 1,
        }
    }

    /// Bytes spanned by the first `units` code units of `chunks`. A count that ends inside a
    /// character (half a surrogate pair, part of a utf-8 sequence) snaps left to that character's
    /// start; a count past the end stops at the end.
    pub(crate) fn bytes<'a>(self, chunks: impl IntoIterator<Item = &'a str>, units: u32) -> u32 {
        let mut bytes = 0;
        let mut left = units;
        for chunk in chunks {
            if left == 0 {
                break;
            }
            // Every ASCII character is one unit in all three encodings.
            if chunk.is_ascii() {
                let take = left.min(chunk.len() as u32);
                bytes += take;
                left -= take;
                continue;
            }
            for c in chunk.chars() {
                let width = self.width(c);
                if width > left {
                    return bytes;
                }
                bytes += c.len_utf8() as u32;
                left -= width;
                if left == 0 {
                    return bytes;
                }
            }
        }
        bytes
    }

    /// Code units in `chunks`.
    pub(crate) fn units<'a>(self, chunks: impl IntoIterator<Item = &'a str>) -> u32 {
        chunks
            .into_iter()
            .map(|chunk| match self {
                _ if chunk.is_ascii() => chunk.len() as u32,
                Encoding::Utf8 => chunk.len() as u32,
                Encoding::Utf16 => chunk.encode_utf16().count() as u32,
                Encoding::Utf32 => chunk.chars().count() as u32,
            })
            .sum()
    }
}
```

Snapshot conversions (public — hosts that skip the glue need them, and Phase 5 onward uses them):

```rust
impl Encoding {
    /// The byte offset of `position` in `snapshot`, clamped: a line at or past `line_count` is
    /// the document end, a character past the line end is the line end, and a position inside a
    /// character snaps to its start.
    #[must_use]
    pub fn offset(self, snapshot: &Snapshot, position: Position) -> u32 {
        // Checked here: `point_to_offset` clamps the row to the last line, which would put a
        // position past the end at the start of the last line instead of the document end.
        if position.line >= snapshot.line_count() {
            return snapshot.len();
        }
        let start = snapshot.point_to_offset(Point::new(position.line, 0));
        // `point_to_offset` clamps the column to the row's length, so `u32::MAX` is the line end.
        let end = snapshot.point_to_offset(Point::new(position.line, u32::MAX));
        match self {
            Encoding::Utf8 => snapshot.clip_offset(start + position.character.min(end - start), Bias::Left),
            Encoding::Utf16 | Encoding::Utf32 => start + self.bytes(snapshot.chunks(start..end), position.character),
        }
    }

    /// The LSP position of byte `offset` in `snapshot`. An offset past the end clamps to the end;
    /// one inside a character snaps to its start.
    #[must_use]
    pub fn position(self, snapshot: &Snapshot, offset: u32) -> Position {
        let offset = snapshot.clip_offset(offset, Bias::Left);
        let point = snapshot.offset_to_point(offset);
        let character = match self {
            Encoding::Utf8 => point.col,
            Encoding::Utf16 | Encoding::Utf32 => self.units(snapshot.chunks(offset - point.col..offset)),
        };
        Position::new(point.row, character)
    }

    /// The byte span of an LSP range in `snapshot`. Each end clamps as in
    /// [`offset`](Self::offset); an inverted range collapses to its end.
    #[must_use]
    pub fn span(self, snapshot: &Snapshot, range: lsp_types::Range) -> Range<u32> {
        let start = self.offset(snapshot, range.start);
        let end = self.offset(snapshot, range.end);
        start.min(end)..end
    }

    /// The LSP range of byte span `span` in `snapshot`.
    #[must_use]
    pub fn range(self, snapshot: &Snapshot, span: Range<u32>) -> lsp_types::Range {
        lsp_types::Range::new(self.position(snapshot, span.start), self.position(snapshot, span.end))
    }
}
```

`start.min(end)..end` is the inverted-range rule: when `start > end` the result is `end..end`.

Text conversions (for disk text a document isn't open for — Phase 8's `FileEdits::apply` and
`Jump::Unopened::span`). Disk text may carry CRLF; lines split at `\n`, and a `\r` before the `\n`
is not part of the line's content.

```rust
impl Encoding {
    /// The byte offset of `position` in plain `text`, clamped like [`offset`](Self::offset). A
    /// `\r\n` counts as one line break, so the end of a CRLF line is before its `\r`.
    #[must_use]
    pub fn text_offset(self, text: &str, position: Position) -> usize {
        let Some(line) = line_bounds(text, position.line) else {
            return text.len();
        };
        let content = &text[line.clone()];
        let bytes = match self {
            Encoding::Utf8 => floor_char_boundary(content, (position.character as usize).min(content.len())),
            Encoding::Utf16 | Encoding::Utf32 => self.bytes([content], position.character) as usize,
        };
        line.start + bytes
    }

    /// The byte span of an LSP range in plain `text`; an inverted range collapses to its end.
    #[must_use]
    pub fn text_span(self, text: &str, range: lsp_types::Range) -> Range<usize> {
        let start = self.text_offset(text, range.start);
        let end = self.text_offset(text, range.end);
        start.min(end)..end
    }
}

/// The content bytes of line `line` in `text` (without its `\n` or a `\r\n`), or `None` when the
/// text has fewer lines.
fn line_bounds(text: &str, line: u32) -> Option<Range<usize>> {
    let mut start = 0;
    for _ in 0..line {
        start += text[start..].find('\n')? + 1;
    }
    let end = text[start..].find('\n').map_or(text.len(), |at| start + at);
    let end = if text[start..end].ends_with('\r') { end - 1 } else { end };
    Some(start..end)
}

/// The largest char boundary in `text` at or below `at`.
fn floor_char_boundary(text: &str, mut at: usize) -> usize {
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}
```

**Decision:** the text form returns `usize` offsets. Disk text is sliced with `usize` in Phase 8 and
never enters a scrive buffer, so the `u32` offset space does not apply to it.

**Decision:** a lone `\r` is not a line break in the text form. scrive itself is LF-only, CRLF is
the only other ending seen on disk in practice, and treating `\r` as a break would need a second
line scanner for a case no test fixture has.

`str::floor_char_boundary` is unstable, hence the local helper.

## 5. Files changed

| File | Change |
|---|---|
| `Cargo.toml` | add member; add `[workspace.dependencies]` for scrive-core and scrive-lsp |
| `Cargo.lock` | lsp-types 0.97.0, fluent-uri 0.1.4 and the scrive-lsp package |
| `crates/scrive-iced/Cargo.toml` | `scrive-core.workspace = true` |
| `crates/scrive-lsp/Cargo.toml` | new (§4.3) |
| `crates/scrive-lsp/README.md` | new (§4.4) |
| `crates/scrive-lsp/src/lib.rs` | new (§4.5) |
| `crates/scrive-lsp/src/message.rs` | new (§4.6) with tests |
| `crates/scrive-lsp/src/encoding.rs` | new (§4.7) with tests |
| `.github/workflows/ci.yml` | headless gate + scrive-lsp wasip1 tests (§4.8) |

### 4.8 `.github/workflows/ci.yml`

Current file: three jobs, `test`, `wasm` and `lints`. Add two steps.

In the `wasm` job, after `Test scrive-core on wasm32-wasip1`:

```yaml
      # The LSP client is headless too; its unit tests run as wasm the same way.
      - name: Test scrive-lsp on wasm32-wasip1
        run: cargo test -p scrive-lsp --lib --target wasm32-wasip1
        env:
          CARGO_TARGET_WASM32_WASIP1_RUNNER: wasmtime
```

In the `lints` job, after `Docs build clean`:

```yaml
      # The headless crates must never pull a GUI crate into their normal
      # dependency graph. `cargo tree` failing is itself a failure.
      - name: Headless crates depend on no GUI crate
        shell: bash
        run: |
          for c in scrive-core scrive-lsp; do
            out=$(cargo tree -p "$c" -e normal --prefix none) || exit 1
            if grep -qE '^(iced|winit|wgpu)' <<<"$out"; then
              echo "$c depends on a GUI crate"
              exit 1
            fi
          done
```

`shell: bash` is explicit because the here-string needs bash; ubuntu's default shell already is
bash, but the step should not depend on that. scrive-core's manifest comment already claims this
gate exists; with this step it does.

Do not add `--all-features` runs here — that is Phase 9.

## 6. Tests

Conventions: colocated `#[cfg(test)] mod tests` at the bottom of each file; names read as
sentences; each test has a `///` doc stating its invariant; every `assert!`/`assert_eq!` has a
string message; table tests put the row in the message. Tests may `unwrap()`.

### 6.1 `message.rs`

Helpers:

```rust
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn parse(value: Value) -> Message {
        serde_json::from_value(value).expect("fixture parses")
    }

    fn rejects(value: Value) -> bool {
        serde_json::from_value::<Message>(value).is_err()
    }

    fn wire(message: &Message) -> Value {
        serde_json::to_value(message).expect("message serializes")
    }
}
```

| Test | Asserts | Fixture |
|---|---|---|
| `request_parses_without_a_jsonrpc_member` | `Request { id: Number(1), method: "initialize", params: Some({}) }` | `{"id":1,"method":"initialize","params":{}}` |
| `string_and_integer_ids_parse` | `Id::String("a")`, `Id::Number(7)` | `{"jsonrpc":"2.0","id":"a","method":"m"}`, `{"id":7,"method":"m"}` |
| `zero_fraction_float_id_parses_as_an_integer` | `Response { id: Some(Number(1)), result: Ok(Null) }` | `{"id":1.0,"result":null}` |
| `fractional_id_is_rejected` | `rejects` | `{"id":1.5,"result":null}` |
| `boolean_id_is_rejected` | `rejects` | `{"id":true,"method":"m"}` |
| `error_response_with_null_id_parses` | `Response { id: None, result: Err(code -32700) }` | `{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}` |
| `error_wins_when_result_and_error_are_both_present` | `result` is `Err` with code -32603 | `{"id":2,"result":{"x":1},"error":{"code":-32603,"message":"boom"}}` |
| `null_result_is_a_successful_reply` | `Ok(Value::Null)` | `{"id":3,"result":null}` |
| `message_without_method_result_or_error_is_rejected` | `rejects` | `{"id":4}` and `{}` |
| `unknown_members_are_ignored` | parses as `Notification { method: "n", params: None }` | `{"method":"n","extra":true,"jsonrpc":"1.0"}` |
| `params_may_be_an_object_an_array_or_absent` | params `Some(object)`, `Some(array)`, `None` | three notifications |
| `null_params_read_as_absent` | `params: None` | `{"method":"n","params":null}` |
| `method_with_null_id_is_a_notification` | `Message::Notification` | `{"id":null,"method":"n"}` |
| `null_params_are_omitted_on_output` | wire equals fixture | `Request::new::<lsp_types::request::Shutdown>(Id::Number(2), ())` → `{"jsonrpc":"2.0","id":2,"method":"shutdown"}`; `Notification::new::<lsp_types::notification::Exit>(())` → `{"jsonrpc":"2.0","method":"exit"}` |
| `response_always_carries_result_even_when_null` | wire equals fixture | `Response::ok(Id::Number(3), ())` → `{"jsonrpc":"2.0","id":3,"result":null}` |
| `error_response_carries_error_and_no_result` | wire equals fixture; `wire.get("result").is_none()` | `Response::error(None, Error { code: -32601, message: "x".into(), data: None })` → `{"jsonrpc":"2.0","id":null,"error":{"code":-32601,"message":"x"}}` |
| `messages_round_trip_through_json` | `parse(wire(m)) == m` for one of each shape, including a string id and an error with `data` | built in the test |
| `message_is_clone_debug_and_send` | compiles | `fn assert_traits<T: Clone + core::fmt::Debug + Send>() {} assert_traits::<Message>();` |

Body sketch for one row:

```rust
/// `result` and `error` together are a server bug; the error is the truthful part, so it wins.
#[test]
fn error_wins_when_result_and_error_are_both_present() {
    let message = parse(json!({"id": 2, "result": {"x": 1}, "error": {"code": -32603, "message": "boom"}}));
    let Message::Response(response) = message else { panic!("a message with no method is a response") };
    assert_eq!(response.id, Some(Id::Number(2)), "the id survives");
    assert_eq!(response.result.map_err(|e| e.code), Err(-32603), "the error wins over the result");
}
```

### 6.2 `encoding.rs`

Helpers:

```rust
#[cfg(test)]
mod tests {
    use scrive_core::Document;

    use super::*;

    fn snapshot(text: &str) -> Snapshot {
        Document::new(text).expect("fixture loads").snapshot()
    }

    fn at(line: u32, character: u32) -> Position {
        Position::new(line, character)
    }
}
```

Use real `Document::new(...).snapshot()`; do not construct a `Snapshot` any other way.

The main fixture is `"aé€😀b"` (bytes: `a`=0, `é`=1..3, `€`=3..6, `😀`=6..10, `b`=10, end=11).

| Test | Rows (position → offset unless noted) |
|---|---|
| `utf8_positions_convert_and_snap_left` | utf-8: (0,0)→0, (0,1)→1, (0,2)→1, (0,3)→3, (0,6)→6, (0,7)→6, (0,10)→10, (0,11)→11, (0,99)→11 |
| `utf16_positions_count_surrogate_pairs` | utf-16: (0,1)→1, (0,2)→3, (0,3)→6, (0,5)→10, (0,6)→11 |
| `utf32_positions_count_scalar_values` | utf-32: (0,1)→1, (0,2)→3, (0,3)→6, (0,4)→10, (0,5)→11 |
| `mid_surrogate_position_snaps_left` | utf-16 (0,4)→6 |
| `line_at_or_past_line_count_clamps_to_document_end` | utf-16 on `""`: (1,0)→0; on `"\n\n"`: (1,0)→1, (2,0)→2, (3,0)→2; on `"a\nbc"`: (5,0)→4 |
| `maximal_position_clamps_to_document_end` | every encoding on `"a\nbc"`: (u32::MAX, u32::MAX)→4 |
| `character_past_line_end_clamps_to_line_end` | every encoding on `"a\nbc"`: (0,9)→1 (before the `\n`), (1,9)→4 |
| `inverted_range_collapses_to_its_end` | utf-16 `span` on `"abcd"` of (0,3)..(0,1) → `1..1` |
| `offsets_convert_back_to_positions` | offset → position on the main fixture: utf-16 10→(0,5), 6→(0,3), 11→(0,6); utf-32 10→(0,4); utf-8 10→(0,10); on `"a\nbc"` utf-16 3→(1,1) |
| `mid_character_offset_snaps_left_before_conversion` | utf-16 7→(0,3), utf-8 2→(0,1) |
| `conversion_walks_across_rope_chunk_boundaries` | `"é".repeat(200) + "x"` (400 bytes, several 128-byte chunks): utf-16 (0,200)→400, (0,201)→401, and 400→(0,200); `"a".repeat(300)`: utf-16 (0,250)→250 (ASCII fast path across chunks) |
| `text_offsets_treat_crlf_as_one_line_break` | `text_offset` on `"ab\r\ncd"`: utf-16 (0,9)→2, (1,1)→5; `text_span` of (0,0)..(1,2) → `0..6` |
| `text_line_past_the_end_clamps_to_text_end` | `text_offset` on `"a\n"`: (1,0)→2, (2,0)→2 |
| `absent_or_unknown_encoding_negotiates_utf16` | `negotiate(None)`, `negotiate(Some(&"utf-7".into()))` → `Utf16` |
| `advertised_kinds_negotiate_back_to_themselves` | for each of the three: `negotiate(Some(&e.kind())) == e` |

Table body sketch:

```rust
/// utf-16 counts a non-BMP character as two units, so offsets after the emoji shift by two.
#[test]
fn utf16_positions_count_surrogate_pairs() {
    let snap = snapshot("aé€😀b");
    for (character, offset) in [(1, 1), (2, 3), (3, 6), (5, 10), (6, 11)] {
        assert_eq!(
            Encoding::Utf16.offset(&snap, at(0, character)),
            offset,
            "utf-16 (0,{character}) should be byte {offset}",
        );
    }
}
```

## 7. Verification

```
cargo build -p scrive-lsp
cargo test -p scrive-lsp
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
cargo build --workspace --all-targets --target wasm32-unknown-unknown
for c in scrive-core scrive-lsp; do out=$(cargo tree -p $c -e normal --prefix none) || exit 1; if grep -qE '^(iced|winit|wgpu)' <<<"$out"; then echo "$c LEAK"; exit 1; fi; done
```

The wasip1 test step needs wasmtime and runs in CI only. If wasmtime is installed locally you may
also run `CARGO_TARGET_WASM32_WASIP1_RUNNER=wasmtime cargo test -p scrive-lsp --lib --target wasm32-wasip1`.

## 8. Spot-check tables

Envelope parsing:

| Input | Parsed as |
|---|---|
| `{"id":1,"method":"m"}` | Request, id 1, params `None` |
| `{"id":"x","method":"m","params":[1]}` | Request, id `"x"`, params array |
| `{"method":"m"}` | Notification |
| `{"id":null,"method":"m"}` | Notification (Decision) |
| `{"id":1.0,"result":null}` | Response, id 1, `Ok(null)` |
| `{"id":1.5,"result":null}` | error |
| `{"id":null,"error":{"code":-32700,"message":"p"}}` | Response, id `None`, `Err` |
| `{"id":1,"result":1,"error":{"code":1,"message":""}}` | Response, `Err` (error wins) |
| `{"id":1}` | error (Decision) |
| `{"id":1,"error":{"code":1}}` | Response, `Err` with empty message |

Envelope output:

| Built | Wire |
|---|---|
| `Request::new::<Shutdown>(Id::Number(2), ())` | `{"jsonrpc":"2.0","id":2,"method":"shutdown"}` |
| `Notification::new::<Exit>(())` | `{"jsonrpc":"2.0","method":"exit"}` |
| `Notification::new::<Initialized>(InitializedParams {})` | `{"jsonrpc":"2.0","method":"initialized","params":{}}` |
| `Response::ok(Id::Number(3), ())` | `{"jsonrpc":"2.0","id":3,"result":null}` |
| `Response::error(None, …)` | `{"jsonrpc":"2.0","id":null,"error":{…}}` |

Encoding (plan's rows):

| Text | Encoding | Position | Offset |
|---|---|---|---|
| `"aé€😀b"` | utf-8 / utf-16 / utf-32 | (0,3) | 3 / 6 / 6 |
| `"aé€😀b"` | utf-16 | (0,4) mid-surrogate | 6 |
| `"aé€😀b"` | utf-16 / utf-32 | end (0,6) / (0,5) | 11 |
| `""` | any | (1,0) | 0 |
| `"\n\n"` | any | (3,0) | 2 |
| `"a\nbc"` | any | (5,0) | 4 |
| `"a\nbc"` | any | (u32::MAX, u32::MAX) | 4 |
| `"abcd"` | any | range (0,3)..(0,1) | `1..1` |

## 9. What NOT to change

- Any file under `crates/scrive-core/` or `crates/scrive-iced/src/`. If Phase 1's `Snapshot` API
  is missing or shaped differently, stop and report — do not add it here.
- The iced pin, `[patch.crates-io]`, the workspace `version`.
- CI jobs other than the two added steps; no `--all-features` runs (Phase 9).
- No `client`, `update`, `uri` or other module — Phase 5.
- Do not add `thiserror` usage, a crate `Error` type, or any speculative constructor
  (`Request::untyped`, `Id::next`, …).
- Do not run `cargo fmt` or `cargo update`.

## 10. Pitfalls

- **Name clash with `core::ops::Range`.** lsp-types also exports `Range`. Import `core::ops::Range`
  and always write `lsp_types::Range` in full. Never `use lsp_types::Range as LspRange` — aliased
  imports are forbidden.
- **`pub(crate)` items and `-D warnings`.** `Encoding::bytes`/`units` are `pub(crate)`; they are
  used by the public methods, so they are live. Do not add any `pub(crate)` helper that nothing in
  this phase calls — `dead_code` fails the clippy step, and `#[allow(dead_code)]` is forbidden.
- **`PositionEncodingKind` in patterns.** It wraps a `Cow`, so `match kind { PositionEncodingKind::UTF8 => … }`
  does not compile. Use guards with `==`.
- **The row clamp.** `Snapshot::point_to_offset` clamps an out-of-range row to the last line. The
  explicit `line >= line_count` check must come first, or `"a\nbc" (5,0)` returns 2 instead of 4.
- **`params: null` vs absent.** `Option<Value>` with `#[serde(default)]` already reads `null` as
  `None`; only `result` needs the `present` helper. Do not add `deserialize_with = "present"` to
  `params` or `id`, or `{"id":null,"method":"m"}` becomes a request with an unreadable id.
- **Serialize `Id` untagged.** `#[serde(untagged)]` on `Id` makes `Number(2)` serialize as `2`, not
  `{"Number":2}`.
- **wasm.** No `std::time`, threads, `std::process::id`, or file I/O anywhere in scrive-lsp — the
  crate builds for wasm32-unknown-unknown and its tests run on wasip1. `serde_json` and lsp-types
  are wasm-safe.
- **fluent-uri strictness** does not matter yet (no URIs are parsed in this phase), but note it for
  Phase 5: `Uri::from_str` rejects unencoded spaces and non-ASCII.
- **lsp-types deprecated fields.** None are touched in this phase. `InitializeParams::root_uri` and
  `root_path` are `#[deprecated]` in 0.97; Phase 5 handles them.
- **rustfmt.** Only `rustfmt --edition 2021 crates/scrive-lsp/src/*.rs`; never `cargo fmt`.
