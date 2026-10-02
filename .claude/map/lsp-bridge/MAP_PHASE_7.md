# Phase 7 — Signature help (call continuation) and hover, `markdown::to_hover`

Read `MAP_PLAN.md` first, then MAP_PHASE_6.md §4.4 (the pending machinery this phase extends).
This doc implements the Phase 7 bullet list and decisions D15 and D16 (client side), on top of
D12. It does not change any design decision; open details are marked **Decision:** with a one-line
reason.

## 1. Prerequisites

Phases 1–6 are committed. Verify:

- Green baseline: `cargo test --workspace`, clippy `-D warnings`, the wasm32 build.
- Phase 6's machinery exists in `crates/scrive-lsp/src/client.rs`: `Pending`, `Kind`, `Query`
  (with `kind()` and `request()`), `send`, `cancel`, `settled`, `reissue`, `failed`, `resolved`,
  `Output::{answer, append}`; and `markdown.rs` with `lines`, `Line`, `link`, `to_plain`,
  `documentation`. The test helpers `request` (mints through a per-test
  `scrive_core::intel::ticket::Counter`), `running`, `document`, `from_server`, `wire`,
  `completions` and `completion_capabilities` exist in `client/tests.rs`.
- Phase 3's API. Grep and adapt call sites (not the design) if names differ:

  ```rust
  // scrive_core
  impl SignatureRequest {                 // private fields
      pub fn new(ticket: Ticket, position: Point, call: Option<u32>) -> Self;
      pub fn ticket(&self) -> Ticket;
      pub fn position(&self) -> Point;
      pub fn call(&self) -> Option<u32>;  // offset of the innermost `(` around the caret
  }
  #[non_exhaustive]
  pub struct HoverRequest {               // plain data: public fields (Phase 2), built with `new`
      pub ticket: Ticket,
      pub offset: u32,
      pub word: Range<u32>,
  }
  impl HoverRequest { pub fn new(ticket: Ticket, offset: u32, word: Range<u32>) -> Self; }
  impl intel::ticket::Counter { pub fn new() -> Self; pub fn issue(&mut self, revision: Revision) -> Ticket; }
  pub fn intel::hover::escape_markdown(text: &str) -> String;
  pub struct SignatureInfo { pub label: String, pub params: Vec<Range<u32>>, pub active: u32, pub doc: Option<String> }
  pub struct HoverInfo { pub markdown: String, pub range: Range<u32> }
  ```

  `grep -n "pub fn escape_markdown\|pub fn call\|pub word" crates/scrive-core/src/intel/*.rs`.
- **The markdown grammar.** Read the grammar documented on `HoverInfo::markdown` (Phase 3). This
  phase relies on one property: `` "`" + escape_markdown(s) + "`" `` renders as the literal code
  `s` (escapes are honoured inside code spans, `**` is not bold there). Check it against Phase 3's
  escape table or tests. If the grammar does not honour escapes inside code, stop and report —
  `to_hover`'s code-line construction depends on it.
- If `SignatureInfo` or `HoverInfo` became `#[non_exhaustive]` in Phase 3, build them through
  whatever constructor Phase 3 added instead of the struct literals below.

## 2. Goal and exit criteria

`Client::signature_help` and `Client::hover` answer the editor's `SignatureRequest` and
`HoverRequest`. Signature help continues an in-flight request while the caret stays in the same
call and re-issues once the reply lands if the caret has moved; hover supersedes. Replies convert to
`Change::Signature(Option<SignatureInfo>)` and `Change::Hover(Option<HoverInfo>)` under the latest
ticket. `markdown::to_hover` lowers server markdown to scrive's hover subset.

Exit criteria:

1. `cargo test -p scrive-lsp` passes everything from Phases 4–6 plus:
   - `signature.rs`: `empty_signatures_convert_to_none`, `active_signature_is_clamped`,
     `per_signature_active_parameter_wins`, `active_parameter_is_clamped_to_the_last_parameter`,
     `simple_labels_are_found_after_the_open_paren`,
     `repeated_simple_labels_are_found_in_order`,
     `missing_simple_label_is_an_empty_range_at_the_cursor`, `label_offsets_are_utf16`,
     `label_offsets_snap_clamp_and_collapse`, `signature_documentation_is_plain_text`.
   - `hover.rs`: `plaintext_contents_are_escaped`, `markdown_contents_go_through_to_hover`,
     `marked_string_array_is_joined_with_blank_lines`, `language_string_becomes_code_lines`,
     `empty_contents_convert_to_none`, `server_range_containing_the_offset_is_kept`,
     `server_range_missing_the_offset_falls_back_to_the_word`,
     `missing_range_falls_back_to_the_word`.
   - `markdown.rs`: `fences_become_code_lines`,
     `tilde_and_unterminated_fences_become_code_lines`, `code_lines_escape_their_text`,
     `thematic_breaks_are_stripped_from_hover`, `links_become_their_text_in_hover`,
     `bold_and_inline_code_pass_through`, `unclosed_backtick_is_escaped`,
     `foreign_escapes_resolve_and_ours_are_kept`, `headings_become_bold`,
     `lone_backslash_is_escaped`.
   - `client/tests.rs`: `signature_and_hover_capabilities_are_advertised`,
     `signature_request_carries_the_caret_position`,
     `signature_declines_before_initialize_and_without_a_provider`,
     `same_call_adopts_the_in_flight_signature_request`,
     `signature_reply_after_the_caret_moved_is_delivered_and_reissued`,
     `reissued_signature_reply_at_the_latest_caret_ends_the_continuation`,
     `different_call_supersedes_with_a_cancel`, `request_outside_any_call_never_continues`,
     `failed_signature_request_answers_none`,
     `content_modified_reissues_signature_help_once`,
     `hover_request_carries_the_offset_position`,
     `hover_declines_before_initialize_and_without_a_provider`,
     `new_hover_supersedes_the_previous_one`, `hover_reply_answers_with_the_converted_card`,
     `null_hover_reply_answers_none`, `failed_hover_request_answers_none`,
     `stale_signature_and_hover_requests_are_ignored`.
2. Clippy and doc clean with `-D warnings`; the wasm32 build passes.

## 3. Design decisions implemented

- **D15 — signature help.** `call` is the offset of the innermost `(` around the caret (computed by
  the editor; the client never scans text). A request with the same `call` updates the in-flight
  entry instead of superseding it, and the reply is stamped with `latest_ticket`. If
  `latest_caret` moved, the reply is delivered and the request re-issued at the synced snapshot —
  this terminates when typing stops. Conversion: empty `signatures` → `None`; the per-signature
  `activeParameter` wins, clamped per `SignatureInfo`; `Simple` labels are searched after the
  previous parameter, starting after the first `(`; `LabelOffsets` are **UTF-16**, clamped and
  snapped. Advertised: `documentationFormat`, `labelOffsetSupport`, `activeParameterSupport`.
- **D16 — hover.** `escape_markdown` owns the grammar (Phase 3). `MarkupKind::PlainText` is
  escaped. Markdown goes through `to_hover`: fences → code lines, `---` stripped, links → their
  text. A `MarkedString` array is joined with blank lines; a `LanguageString` becomes code lines.
  The range is the server's if it contains the offset, and the word otherwise. Advertised:
  `contentFormat: [markdown, plaintext]`.
- **D12 (reused).** One pending entry per (document, kind), supersede with `$/cancelRequest`,
  silent cancellations, one ContentModified re-issue per ticket, the drop in `receive`, stale
  requests ignored, local declines answer the ticket: `Signature(None)` and `Hover(None)` when the
  client isn't ready or the server has no provider. A failed signature request becomes
  `Signature(None)`.

## 4. Step-by-step changes

Module layout additions:

```
crates/scrive-lsp/src/
├── signature.rs   signature::{Query, convert} (crate-private)
└── hover.rs       hover::{Query, convert} (crate-private)
```

### 4.1 `lib.rs`

`mod hover; mod signature;`. Extend the crate doc's list with signature help and hover.

### 4.2 `update.rs`

```rust
pub enum Change {
    Diagnostics(Vec<Diagnostic>),
    Completions(Vec<scrive_core::CompletionItem>),
    /// The signature for the ticket's request; `None` closes the box.
    Signature(Option<scrive_core::SignatureInfo>),
    /// The hover card for the ticket's request; `None` clears it.
    Hover(Option<scrive_core::HoverInfo>),
}
```

### 4.3 `client/capabilities.rs`

In `client()`, add to `TextDocumentClientCapabilities`:

```rust
signature_help: Some(SignatureHelpClientCapabilities {
    signature_information: Some(SignatureInformationSettings {
        // The box renders documentation as plain text.
        documentation_format: Some(vec![MarkupKind::PlainText]),
        parameter_information: Some(ParameterInformationSettings { label_offset_support: Some(true) }),
        active_parameter_support: Some(true),
    }),
    ..SignatureHelpClientCapabilities::default()
}),
hover: Some(HoverClientCapabilities {
    content_format: Some(vec![MarkupKind::Markdown, MarkupKind::PlainText]),
    ..HoverClientCapabilities::default()
}),
```

`SignatureInformationSettings` has exactly those three fields in lsp-types 0.97; if the struct
literal fails, add `..SignatureInformationSettings::default()`.

**Decision:** `documentationFormat: [plaintext]`. D15 says the field is advertised but not its
value; the box shows plain text, and `markdown::documentation` lowers markdown anyway.

**Decision:** `contextSupport` is not advertised and requests carry `context: None`. D15's
advertised list omits it, and continuation already covers what retrigger context would.

In `Server`:

```rust
/// Whether the server answers `textDocument/signatureHelp`.
pub(crate) signature: bool,
/// Whether the server answers `textDocument/hover`.
pub(crate) hover: bool,
```

```rust
signature: capabilities.signature_help_provider.is_some(),
hover: matches!(
    capabilities.hover_provider,
    Some(HoverProviderCapability::Simple(true) | HoverProviderCapability::Options(_))
),
```

`HoverProviderCapability` is a multi-variant lsp-types enum, so `matches!` is fine here.

### 4.4 `signature.rs`

```rust
//! Signature help converted to scrive's one-line signature box.

use core::ops::Range;

use lsp_types::{ParameterInformation, ParameterLabel, SignatureHelp};
use scrive_core::SignatureInfo;

use crate::{markdown, Encoding};

/// What a pending signature request asked.
#[derive(Clone, Debug)]
pub(crate) struct Query {
    /// The editor's call identity: the offset of the innermost `(` around the caret.
    pub(crate) call: Option<u32>,
    /// The caret the request's position was computed from.
    pub(crate) caret: u32,
}

/// The active signature, or `None` when the server offers none.
pub(crate) fn convert(help: SignatureHelp) -> Option<SignatureInfo> {
    let last = help.signatures.len().checked_sub(1)?;
    let index = (help.active_signature.unwrap_or(0) as usize).min(last);
    let fallback = help.active_parameter;
    let signature = help.signatures.into_iter().nth(index)?;
    let params = parameters(&signature.label, signature.parameters.as_deref().unwrap_or_default());
    // The per-signature field (LSP 3.16) is the precise one; the top-level field is shared by all
    // signatures and only a fallback.
    let active = signature
        .active_parameter
        .or(fallback)
        .unwrap_or(0)
        .min(params.len().saturating_sub(1) as u32);
    Some(SignatureInfo {
        label: signature.label,
        params,
        active,
        doc: signature.documentation.as_ref().map(markdown::documentation),
    })
}

/// Byte ranges of each parameter within `label`, one per parameter so indices stay aligned with
/// the active parameter.
fn parameters(label: &str, parameters: &[ParameterInformation]) -> Vec<Range<u32>> {
    // Searching starts after the first `(`, so a parameter named like the function (`x(x)`) is
    // found in the parameter list, not in the name.
    let mut cursor = label.find('(').map_or(0, |open| open + 1);
    parameters
        .iter()
        .map(|parameter| {
            let range = match &parameter.label {
                ParameterLabel::Simple(text) if !text.is_empty() => match label[cursor..].find(text.as_str()) {
                    Some(at) => cursor + at..cursor + at + text.len(),
                    None => cursor..cursor,
                },
                ParameterLabel::Simple(_) => cursor..cursor,
                // Offsets are UTF-16 code units per the spec, whatever encoding was negotiated.
                ParameterLabel::LabelOffsets([start, end]) => {
                    let end = Encoding::Utf16.bytes([label], *end) as usize;
                    let start = (Encoding::Utf16.bytes([label], *start) as usize).min(end);
                    start..end
                }
            };
            cursor = range.end;
            range.start as u32..range.end as u32
        })
        .collect()
}
```

`Encoding::bytes` clamps a count past the end to the label's length and snaps a count inside a
character (half a surrogate pair) to its start — the "clamped and snapped" rule. `.min(end)` is
the inverted-offset rule: `[7, 5]` collapses to the end.

**Decision:** a `Simple` label not found in the rest of the signature yields an empty range at the
search cursor. The parameter list keeps one entry per parameter (so `active` still indexes it) and
nothing is highlighted.

**Decision:** a `LabelOffsets` parameter also advances the search cursor to its end. The plan says
`Simple` labels are searched "after the previous parameter"; this holds whichever form the previous
one used.

### 4.5 `hover.rs`

```rust
//! Hover replies converted to scrive's hover card.

use core::ops::Range;

use lsp_types::{HoverContents, LanguageString, MarkedString, MarkupContent, MarkupKind};
use scrive_core::intel::hover::escape_markdown;
use scrive_core::{HoverInfo, Snapshot};

use crate::{markdown, Encoding};

/// What a pending hover request asked.
#[derive(Clone, Debug)]
pub(crate) struct Query {
    /// The byte the pointer rested over.
    pub(crate) offset: u32,
    /// The word under the pointer — the card's range when the server gives none that fits.
    pub(crate) word: Range<u32>,
}

/// The card, or `None` when the server said nothing.
pub(crate) fn convert(encoding: Encoding, snapshot: &Snapshot, query: &Query, hover: lsp_types::Hover) -> Option<HoverInfo> {
    let markdown = contents(hover.contents);
    if markdown.trim().is_empty() {
        return None;
    }
    // The widget re-tests pointer containment against this range to dismiss the card, so it must
    // contain the offset the pointer rested on.
    let range = hover
        .range
        .map(|range| encoding.span(snapshot, range))
        .filter(|span| span.start <= query.offset && query.offset < span.end)
        .unwrap_or_else(|| query.word.clone());
    Some(HoverInfo { markdown, range })
}

fn contents(contents: HoverContents) -> String {
    match contents {
        HoverContents::Scalar(marked) => marked_string(marked),
        HoverContents::Array(list) => list
            .into_iter()
            .map(marked_string)
            .filter(|part| !part.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        HoverContents::Markup(MarkupContent { kind: MarkupKind::Markdown, value }) => markdown::to_hover(&value),
        HoverContents::Markup(MarkupContent { kind: MarkupKind::PlainText, value }) => escape_markdown(&value),
    }
}

fn marked_string(marked: MarkedString) -> String {
    match marked {
        MarkedString::String(markdown) => markdown::to_hover(&markdown),
        MarkedString::LanguageString(LanguageString { value, .. }) => markdown::code_lines(&value),
    }
}
```

**Decision:** "contains" is half-open (`start <= offset < end`): `offset` is the byte under the
pointer, which lies inside a word, not at its end. An empty server range therefore never contains
it.

**Decision:** empty parts of a `MarkedString` array are dropped before joining, so `["", "x"]` does
not start with a blank paragraph.

### 4.6 `markdown.rs` — `to_hover`

Phase 6's `strip_heading(&str) -> &str` becomes `heading(&str) -> Option<&str>` (the title when
the line is a heading); `to_plain` uses `heading(text).unwrap_or(text)`. Then add:

```rust
use scrive_core::intel::hover::escape_markdown;

/// Server markdown in scrive's hover subset (see `HoverInfo::markdown`): fenced blocks become code
/// lines, thematic breaks are dropped, links become their text, headings become bold. Inline code
/// and `**bold**` pass through. Backslash escapes outside scrive's set resolve to their character;
/// scrive's own (`\*`, `` \` ``, `\\`) are kept.
pub(crate) fn to_hover(markdown: &str) -> String {
    lines(markdown)
        .map(|line| match line {
            Line::Code(code) => code_line(code),
            Line::Text(text) => match heading(text) {
                Some(title) => format!("**{}**", hover_inline(title)),
                None => hover_inline(text),
            },
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `code` as code lines, one per source line.
pub(crate) fn code_lines(code: &str) -> String {
    code.lines().map(code_line).collect::<Vec<_>>().join("\n")
}

/// One line as a code span. `escape_markdown` keeps a backtick or backslash in the code from
/// ending the span; an empty line stays empty rather than becoming an empty span.
fn code_line(line: &str) -> String {
    if line.is_empty() {
        String::new()
    } else {
        format!("`{}`", escape_markdown(line))
    }
}

fn hover_inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if let Some((label, after)) = link(rest) {
            out.push_str(&hover_inline(label));
            rest = after;
            continue;
        }
        match c {
            '`' => match rest[1..].find('`') {
                // A server code span: its content is literal, so it is re-escaped for scrive.
                Some(end) => {
                    out.push('`');
                    out.push_str(&escape_markdown(&rest[1..1 + end]));
                    out.push('`');
                    rest = &rest[end + 2..];
                }
                // An unclosed backtick is literal; unescaped it would code-format the rest.
                None => {
                    out.push_str("\\`");
                    rest = &rest[1..];
                }
            },
            '\\' => match rest[1..].chars().next() {
                Some(escaped @ ('*' | '`' | '\\')) => {
                    out.push('\\');
                    out.push(escaped);
                    rest = &rest[2..];
                }
                Some(escaped) if escaped.is_ascii_punctuation() => {
                    out.push(escaped);
                    rest = &rest[1 + escaped.len_utf8()..];
                }
                // A backslash that escapes nothing is itself literal.
                _ => {
                    out.push_str("\\\\");
                    rest = &rest[1..];
                }
            },
            c => {
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    out
}
```

`escape_markdown` and the grammar are Phase 3's; do not re-implement escaping here. `to_hover` is
the only producer of scrive hover markdown in this crate besides `escape_markdown` for plaintext.

**Decision:** headings become `**title**`. The plan lists fences, `---` and links; a `# ` left in
the card reads as noise, and bold is the only emphasis scrive renders.

**Decision:** single-backtick spans only. A double-backtick span (``` `` a`b `` ```) is rare in
hover text and lowers imperfectly (as an empty span plus text); it cannot break the card.

### 4.7 `client.rs`

Types:

```rust
enum Kind { Completion, Signature, Hover }

enum Query {
    Completion(completion::Query),
    Signature(signature::Query),
    Hover(hover::Query),
}
```

`Query::kind` gains the two arms. `Query::request` gains:

```rust
Query::Signature(query) => message::Request::new::<lsp_types::request::SignatureHelpRequest>(id, SignatureHelpParams {
    context: None,
    text_document_position_params: TextDocumentPositionParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        position: encoding.position(snapshot, query.caret),
    },
    work_done_progress_params: Default::default(),
}),
Query::Hover(query) => message::Request::new::<lsp_types::request::HoverRequest>(id, HoverParams {
    text_document_position_params: TextDocumentPositionParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        position: encoding.position(snapshot, query.offset),
    },
    work_done_progress_params: Default::default(),
}),
```

`lsp_types::request::HoverRequest` is named by its full path; scrive-core's `HoverRequest` is the
one imported.

`reissue`, `failed` and `resolved` gain arms:

```rust
// reissue
Query::Signature(query) => self.request_signature(entry.doc_id, entry.latest_ticket, query.call, entry.latest_caret, Some(entry.latest_ticket)),
Query::Hover(query) => self.request_hover(entry.doc_id, entry.latest_ticket, query, Some(entry.latest_ticket)),

// failed — settles the editor's slot
Query::Signature(_) => Output::answer(entry.doc_id, entry.latest_ticket, update::Change::Signature(None)),
Query::Hover(_) => Output::answer(entry.doc_id, entry.latest_ticket, update::Change::Hover(None)),

// resolved
Query::Signature(query) => { let query = query.clone(); self.signed(&entry, &query, value) }
Query::Hover(query) => { let query = query.clone(); self.hovered(&entry, &query, value) }
```

**Decision:** a failed hover request answers `Hover(None)`, like signature help (D12 names only the
signature case). It clears the awaited slot instead of leaving it hanging.

Entry points:

```rust
impl Client {
    /// Answers the editor's signature request. While the caret stays in the same call, a request
    /// already in flight is adopted instead of superseded.
    pub fn signature_help(&mut self, snapshot: &Snapshot, request: &SignatureRequest) -> Output {
        let doc_id = snapshot.doc_id();
        let ticket = request.ticket();
        let Some(tracked) = self.tracked.iter().find(|t| t.doc_id == doc_id) else {
            return Output::default();
        };
        if ticket.revision() != snapshot.revision() || snapshot.revision() != tracked.synced.revision() {
            return Output::default();
        }
        if !matches!(&self.state, State::Running(server) if server.signature) {
            return Output::answer(doc_id, ticket, update::Change::Signature(None));
        }
        let caret = snapshot.clip_offset(snapshot.point_to_offset(request.position()), Bias::Left);
        if let Some(call) = request.call() {
            let in_flight = self.pending.iter_mut().find(|p| {
                p.doc_id == doc_id && matches!(&p.query, Query::Signature(query) if query.call == Some(call))
            });
            if let Some(entry) = in_flight {
                entry.latest_ticket = ticket;
                entry.latest_caret = caret;
                return Output::default();
            }
        }
        self.request_signature(doc_id, ticket, request.call(), caret, None)
    }

    /// Sends a signature request at the synced snapshot (the editor's revision; callers checked).
    fn request_signature(&mut self, doc_id: DocId, ticket: Ticket, call: Option<u32>, caret: u32, reissued_for: Option<Ticket>) -> Output {
        let Some(snapshot) = self.tracked.iter().find(|t| t.doc_id == doc_id).map(|t| t.synced.clone()) else {
            return Output::default();
        };
        self.send(doc_id, &snapshot, ticket, caret, Query::Signature(signature::Query { call, caret }), reissued_for)
    }

    fn signed(&mut self, entry: &Pending, query: &signature::Query, value: Value) -> Result<Output, Error> {
        let help: Option<lsp_types::SignatureHelp> = serde_json::from_value(value)
            .map_err(|source| Error::Decode { method: "textDocument/signatureHelp".to_owned(), source })?;
        let mut output = Output::answer(entry.doc_id, entry.latest_ticket, update::Change::Signature(help.and_then(signature::convert)));
        // The caret moved while the request was out: the answer may describe the old caret's
        // parameter. Deliver it, then ask again at the synced caret; once typing stops the carets
        // agree and this ends.
        if entry.latest_caret != query.caret {
            output.append(self.request_signature(entry.doc_id, entry.latest_ticket, query.call, entry.latest_caret, entry.reissued_for));
        }
        Ok(output)
    }

    /// Answers the editor's hover request; a new hover always supersedes the previous one.
    pub fn hover(&mut self, snapshot: &Snapshot, request: &HoverRequest) -> Output {
        let doc_id = snapshot.doc_id();
        let ticket = request.ticket;
        let Some(tracked) = self.tracked.iter().find(|t| t.doc_id == doc_id) else {
            return Output::default();
        };
        if ticket.revision() != snapshot.revision() || snapshot.revision() != tracked.synced.revision() {
            return Output::default();
        }
        if !matches!(&self.state, State::Running(server) if server.hover) {
            return Output::answer(doc_id, ticket, update::Change::Hover(None));
        }
        let query = hover::Query { offset: request.offset, word: request.word.clone() };
        self.request_hover(doc_id, ticket, query, None)
    }

    fn request_hover(&mut self, doc_id: DocId, ticket: Ticket, query: hover::Query, reissued_for: Option<Ticket>) -> Output {
        let Some(snapshot) = self.tracked.iter().find(|t| t.doc_id == doc_id).map(|t| t.synced.clone()) else {
            return Output::default();
        };
        let offset = query.offset;
        self.send(doc_id, &snapshot, ticket, offset, Query::Hover(query), reissued_for)
    }

    fn hovered(&mut self, entry: &Pending, query: &hover::Query, value: Value) -> Result<Output, Error> {
        let reply: Option<lsp_types::Hover> = serde_json::from_value(value)
            .map_err(|source| Error::Decode { method: "textDocument/hover".to_owned(), source })?;
        let card = reply.and_then(|reply| hover::convert(self.encoding, &entry.request_snapshot, query, reply));
        Ok(Output::answer(entry.doc_id, entry.latest_ticket, update::Change::Hover(card)))
    }
}
```

`matches!` over `Query` is fine now: `Query` has three variants, so the macro's `_ => false` arm is
reachable. `request_signature` takes five parameters; if clippy's `too_many_arguments` default (7)
is not hit it is acceptable, but if it reads badly, pass a `signature::Query` instead of
`(call, caret)`.

**Decision:** a request with `call: None` never adopts an in-flight entry. D15 keys continuation on
"the same `call`"; two requests outside any call have no call to share, so the newer supersedes.

**Decision:** the signature re-issue passes on `entry.reissued_for` unchanged. It is a
continuation re-issue, not a ContentModified one, so it must not consume the once-per-ticket
ContentModified budget.

Hover never adopts: D16 defines no continuation for it, and the editor already keeps a request
alive while the pointer stays in its word (`hover_pending`, Phase 2).

## 5. Files changed

| File | Change |
|---|---|
| `crates/scrive-lsp/src/lib.rs` | `mod hover; mod signature;`, crate doc |
| `crates/scrive-lsp/src/client.rs` | `Kind`/`Query` arms; `signature_help`, `request_signature`, `signed`, `hover`, `request_hover`, `hovered`; `reissue`/`failed`/`resolved` arms |
| `crates/scrive-lsp/src/client/capabilities.rs` | signature and hover capabilities; `Server::{signature, hover}` |
| `crates/scrive-lsp/src/client/tests.rs` | conversations; `signature(..)`, `hover(..)` and `signature_hover_capabilities()` helpers |
| `crates/scrive-lsp/src/update.rs` | `Change::{Signature, Hover}` |
| `crates/scrive-lsp/src/signature.rs` | new, with tests |
| `crates/scrive-lsp/src/hover.rs` | new, with tests |
| `crates/scrive-lsp/src/markdown.rs` | `heading`, `to_hover`, `code_lines`, `code_line`, `hover_inline`, with tests |

## 6. Tests

### 6.1 Helpers (`client/tests.rs`)

```rust
use scrive_core::{HoverInfo, HoverRequest, Point, SignatureInfo, SignatureRequest};

/// The signature and hover capabilities fixture below.
fn signature_hover_capabilities() -> Value { /* the fixture JSON */ }

fn signature(update: &Update) -> (update::Stamp, Option<SignatureInfo>) {
    let Update::Document(document) = update else { panic!("expected a document update, got {update:?}") };
    match document.change() {
        update::Change::Signature(info) => (document.stamp(), info.clone()),
        other => panic!("expected a signature, got {other:?}"),
    }
}

fn hover(update: &Update) -> (update::Stamp, Option<HoverInfo>) { /* same shape */ }
```

Fixture:

- capabilities (`signature_hover_capabilities()`): `json!({"textDocumentSync": 2, "signatureHelpProvider": {"triggerCharacters": ["(", ","]}, "hoverProvider": true})`
- signature document `"foo("`: caret 4, `Point::new(0, 4)`, `call: Some(3)`.
- signature reply `SIG`:
  `json!({"signatures": [{"label": "foo(a: i32, b: i32)", "parameters": [{"label": "a: i32"}, {"label": "b: i32"}]}], "activeParameter": 0})`
- hover document `"let value = 1;"`: `HoverRequest::new(tickets.issue(doc.revision()), 5, 4..9)`.
- Tickets come from one `Counter` per test (`let mut tickets = Counter::new();`); keep each
  request and compare stamps against its ticket. "Ticket N" below means the N-th request's.

### 6.2 Conversations

| Test | Assertion |
|---|---|
| `signature_and_hover_capabilities_are_advertised` | `initialize` params: `signatureHelp.signatureInformation.documentationFormat == ["plaintext"]`, `.parameterInformation.labelOffsetSupport == true`, `.activeParameterSupport == true`; `hover.contentFormat == ["markdown","plaintext"]` |
| `signature_request_carries_the_caret_position` | `{"id":2,"method":"textDocument/signatureHelp","params":{"textDocument":{…},"position":{"line":0,"character":4}}}` (no `context`) |
| `signature_declines_before_initialize_and_without_a_provider` | `Signature(None)` with the ticket, no messages, in both cases |
| `same_call_adopts_the_in_flight_signature_request` | request (id 2); type `a` + sync; request at `(0,5)`, `call Some(3)` → empty `Output` |
| `signature_reply_after_the_caret_moved_is_delivered_and_reissued` | then `SIG` for id 2 → one update `Signature(Some(..))` stamped with ticket 2 **and** a request id 3 at `(0,5)` |
| `reissued_signature_reply_at_the_latest_caret_ends_the_continuation` | then `SIG` for id 3 → one update, no messages |
| `different_call_supersedes_with_a_cancel` | doc `"foo(bar("`: request `call Some(3)` (id 2), then at the same revision `call Some(7)` → `[$/cancelRequest {id:2}, request id 3]` |
| `request_outside_any_call_never_continues` | two requests with `call None` at one revision → the second cancels the first |
| `failed_signature_request_answers_none` | error −32603 → `Signature(None)` under the ticket |
| `content_modified_reissues_signature_help_once` | −32801 on id 2 → request id 3; −32801 on id 3 → empty |
| `hover_request_carries_the_offset_position` | `{"id":2,"method":"textDocument/hover","params":{…,"position":{"line":0,"character":5}}}` |
| `hover_declines_before_initialize_and_without_a_provider` | `Hover(None)`; also with `"hoverProvider": false` |
| `new_hover_supersedes_the_previous_one` | second hover at the same revision → cancel + request id 3 |
| `hover_reply_answers_with_the_converted_card` | reply `{"contents":{"kind":"markdown","value":"```rust\nlet value: i32\n```"},"range":(0,4)-(0,9)}` → `Hover(Some(HoverInfo { markdown: "`let value: i32`", range: 4..9 }))` |
| `null_hover_reply_answers_none` | `result: null` → `Hover(None)` |
| `failed_hover_request_answers_none` | −32603 → `Hover(None)` |
| `stale_signature_and_hover_requests_are_ignored` | tickets at an older revision → empty `Output` for both |

Body sketch:

```rust
/// Typing inside one call adopts the in-flight request; when its reply lands with the caret
/// elsewhere, the answer is delivered and the request re-issued at the new caret.
#[test]
fn signature_reply_after_the_caret_moved_is_delivered_and_reissued() {
    let mut tickets = Counter::new();
    let mut doc = document("foo(");
    let (mut client, _) = running(Client::builder(), signature_hover_capabilities());
    let _ = client.open(&doc.snapshot(), &uri("file:///a.rs"), "rust").expect("opens");
    let first = SignatureRequest::new(tickets.issue(doc.revision()), Point::new(0, 4), Some(3));
    let _ = client.signature_help(&doc.snapshot(), &first);

    doc.edit_grouped(vec![EditOp::insert(4, "a")], GroupingHint::mergeable(OpClass::Type)).expect("types");
    let _ = client.sync(&doc.snapshot(), doc.drain_changes());
    let second = SignatureRequest::new(tickets.issue(doc.revision()), Point::new(0, 5), Some(3));
    let adopted = client.signature_help(&doc.snapshot(), &second);
    assert!(adopted.messages.is_empty(), "the same call adopts the in-flight request");

    let output = client.receive(from_server(json!({"jsonrpc": "2.0", "id": 2, "result": sig()}))).expect("reply");
    let (stamp, info) = signature(&output.updates[0]);
    assert_eq!(stamp, update::Stamp::Ticket(second.ticket()), "the reply answers the latest ticket");
    assert!(info.is_some(), "the signature is delivered");
    assert_eq!(
        wire(&output.messages)[0].pointer("/params/position"),
        Some(&json!({"line": 0, "character": 5})),
        "the request is re-issued at the moved caret",
    );
}
```

### 6.3 `signature.rs` unit tests

Decode `SignatureHelp` from `json!` fixtures and call `convert`.

| Test | Fixture | Expected |
|---|---|---|
| `empty_signatures_convert_to_none` | `{"signatures": []}` | `None` |
| `active_signature_is_clamped` | two signatures, `"activeSignature": 7` | the second |
| `per_signature_active_parameter_wins` | top-level `activeParameter: 0`, signature's `activeParameter: 1` | `active == 1` |
| `active_parameter_is_clamped_to_the_last_parameter` | two params, `activeParameter: 5` / no params, `activeParameter: 3` | `1` / `0` |
| `simple_labels_are_found_after_the_open_paren` | label `x(x)`, params `["x"]` | `[2..3]` |
| `repeated_simple_labels_are_found_in_order` | label `max(x, x)`, params `["x", "x"]` | `[4..5, 7..8]` |
| `missing_simple_label_is_an_empty_range_at_the_cursor` | label `f(a, b)`, params `["a", "zz", "b"]` | `[2..3, 3..3, 5..6]` |
| `label_offsets_are_utf16` | label `f(é: 😀)`, params `[[5, 7]]` | `[6..10]` |
| `label_offsets_snap_clamp_and_collapse` | same label, `[[6, 7]]`, `[[5, 99]]`, `[[7, 5]]` | `[6..10]`, `[6..11]`, `[6..6]` |
| `signature_documentation_is_plain_text` | `"documentation": {"kind": "markdown", "value": "**x**"}` | `doc == Some("x")` |

(Label `f(é: 😀)` bytes: `f` 0, `(` 1, `é` 2..4, `:` 4, space 5, `😀` 6..10, `)` 10, length 11.
UTF-16 units: `f` 0, `(` 1, `é` 2, `:` 3, space 4, `😀` 5..7, `)` 7.)

### 6.4 `hover.rs` unit tests

Snapshot `"let value = 1;"`, `Query { offset: 5, word: 4..9 }`, encoding utf-16.

| Test | `contents` / `range` | Expected card |
|---|---|---|
| `plaintext_contents_are_escaped` | `{"kind":"plaintext","value":"a*b `c`"}` | `markdown == escape_markdown("a*b `c`")` |
| `markdown_contents_go_through_to_hover` | `{"kind":"markdown","value":"[Vec](u) is **big**"}` | `"Vec is **big**"` |
| `marked_string_array_is_joined_with_blank_lines` | `["one", {"language":"rust","value":"fn f()"}, ""]` | `"one\n\n`fn f()`"` |
| `language_string_becomes_code_lines` | `{"language":"rust","value":"a\n\nb"}` | `` "`a`\n\n`b`" `` |
| `empty_contents_convert_to_none` | `""`, `[]`, `{"kind":"markdown","value":"  "}` | `None` |
| `server_range_containing_the_offset_is_kept` | range `(0,4)-(0,9)` | `4..9` |
| `server_range_missing_the_offset_falls_back_to_the_word` | range `(0,10)-(0,11)`; empty range `(0,5)-(0,5)` | `4..9` both |
| `missing_range_falls_back_to_the_word` | no range | `4..9` |

### 6.5 `markdown.rs` — `to_hover` tests

| Test | Input | `to_hover` |
|---|---|---|
| `fences_become_code_lines` | "```rust\nfn f() -> u8\n```" | `` "`fn f() -> u8`" `` |
| `tilde_and_unterminated_fences_become_code_lines` | `"~~~\nx\n~~~"` / "```\ny" | `` "`x`" `` / `` "`y`" `` |
| `code_lines_escape_their_text` | "```\na`b*c\\d\n```" | `` "`" + escape_markdown("a`b*c\\d") + "`" `` |
| `thematic_breaks_are_stripped_from_hover` | `"a\n---\nb"` | `"a\nb"` |
| `links_become_their_text_in_hover` | `"see [Vec](https://doc) and ![i](x.png)"` | `"see Vec and i"` |
| `bold_and_inline_code_pass_through` | "**bold** and `code`" | `"**bold** and `code`"` (code content via `escape_markdown`, unchanged here) |
| `unclosed_backtick_is_escaped` | "a ` b" | "a \\` b" |
| `foreign_escapes_resolve_and_ours_are_kept` | `"a\\_b \\*c\\*"` | `"a_b \\*c\\*"` |
| `headings_become_bold` | `"# Title"` | `"**Title**"` |
| `lone_backslash_is_escaped` | `"C:\\path"` (one backslash before `p`) | `"C:\\\\path"` (two) |

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

Signature request decision (after the stale check):

| Client/provider | `call` | In-flight signature entry | Result |
|---|---|---|---|
| not Running / no provider | any | — | `Signature(None)` |
| ok | `Some(c)` | same `call` | adopt: update `latest_ticket`/`latest_caret`, send nothing |
| ok | `Some(c)` | other `call` or none | request (supersedes with cancel) |
| ok | `None` | any | request (supersedes with cancel) |

Signature reply:

| `latest_caret == query.caret` | Output |
|---|---|
| yes | `Signature(convert(reply))` under `latest_ticket` |
| no | the same update **and** a request at `latest_caret` against the synced snapshot |

Signature label resolution (label `foo(a: i32, b: i32)` unless noted):

| Parameters | Ranges |
|---|---|
| `Simple "a: i32"`, `Simple "b: i32"` | `4..10`, `12..18` |
| label `x(x)`, `Simple "x"` | `2..3` |
| label `max(x, x)`, `Simple "x"` ×2 | `4..5`, `7..8` |
| label `f(a, b)`, `Simple "a"`, `"zz"`, `"b"` | `2..3`, `3..3`, `5..6` |
| label `f(é: 😀)`, `LabelOffsets [5,7]` | `6..10` |
| same, `[6,7]` (mid-surrogate) | `6..10` |
| same, `[5,99]` | `6..11` |
| same, `[7,5]` (inverted) | `6..6` |
| label `a b` (no paren), `Simple "b"` | `2..3` |

Active parameter:

| Signature's | Top-level | Params | `active` |
|---|---|---|---|
| 1 | 0 | 2 | 1 |
| absent | 1 | 2 | 1 |
| absent | absent | 2 | 0 |
| 5 | — | 2 | 1 |
| 3 | — | 0 | 0 |

Hover contents lowering:

| Contents | Card markdown |
|---|---|
| `PlainText "a*b"` | `a\*b` |
| `Markdown "```\nx\n```"` | `` `x` `` |
| `Markdown "a\n---\nb"` | `a` / `b` |
| `Markdown "[t](u)"` | `t` |
| `Markdown "# H"` | `**H**` |
| `MarkedString "x"` | `x` |
| `LanguageString { value: "a\nb" }` | `` `a` `` / `` `b` `` |
| `["x", "y"]` | `x`, blank line, `y` |
| `""` / `[]` | `None` |

## 9. What NOT to change

- scrive-core and scrive-iced — in particular not `escape_markdown` or the widget parser. If the
  grammar does not fit (§1), stop and report.
- Completion behavior and its tests (Phase 6), except the shared helpers' signatures if a borrow
  requires it.
- No definition, rename or format; no `Pending::versions`; no `Change::{Edits, Definition}` (Phase 8).
- Do not advertise signature `contextSupport`, hover `dynamicRegistration`, or markdown for
  signature documentation.
- The client never scans document text for signature help; `call` comes from the request.

## 10. Pitfalls

- **Name clashes.** `lsp_types::request::HoverRequest` vs `scrive_core::HoverRequest`;
  `lsp_types::Hover` vs scrive-core's `Hover` trait; `lsp_types::SignatureHelp` vs scrive-core's
  `SignatureHelp` trait. Import the scrive-core types you use and spell the lsp-types ones in full.
  No aliased imports.
- **`matches!` on `Query`** is safe from this phase on (three variants), but `Kind` comparisons
  stay `==`.
- **Dead code.** `signature::Query::caret` is read by `signed` and `Query::request`;
  `hover::Query::word` by `hover::convert`; `markdown::code_lines` by `hover.rs`. Every new
  `Server` field is read in `signature_help`/`hover`.
- **`to_plain` must not change.** Refactoring `strip_heading` into `heading` must keep every Phase 6
  markdown test green.
- **UTF-16 label offsets** use `Encoding::Utf16` explicitly, not the negotiated encoding (Risk 2).
- **Slicing by bytes** in `hover_inline` and `parameters`: every index comes from `find` on the
  same string or from `len_utf8`, so it is a char boundary; keep it that way.
- **Stale continuation.** The drop in `settled` compares `latest_ticket.revision()` with the synced
  revision *before* `signed` runs, so a re-issue never goes out for a document the user has left.
- **Single-variant enums in tests.** None remain: `Change` has four variants and the helpers carry
  `other => panic!` arms.
- **wasm.** No clock (hover dwell is the editor's), no threads.
