# Phase 3 — editor commands, call identity, widget hardening, safe hover

Read `MAP_PLAN.md` first (D11, D15's `call`, D16's markdown ownership, D17, Constraints). This
doc specifies Phase 3 only. Line numbers are HEAD `6cf4f2c` numbering; Phases 1 and 2 moved
code in code_editor.rs, editor.rs and the intel modules, so **locate each site by the function
or arm named**, not by the number.

## 1. Prerequisites

- **Phases 1 and 2 are committed.** Verify:
  - `Document::select_and_reveal` and `Document::doc_id` exist
    (`grep -n "pub fn select_and_reveal\|pub fn doc_id" crates/scrive-core/src/document.rs`);
  - `crates/scrive-core/src/intel/ticket.rs` exists with `Ticket` and `Counter`;
  - `SignatureRequest` lives in crates/scrive-core/src/intel/signature.rs with private fields
    and `new(ticket, position)`;
  - code_editor.rs has `struct Awaiting`, `enum Awaited`, `fn accepts`, `fn abandon`,
    `tickets: ticket::Counter`, and editor.rs has `Action::TriggerCompletion`,
    `State::hover_queried`, `Editor::hover_pending`, and the test helpers `headless_renderer`
    and `pump`;
  - `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` are
    green.
- Confirm the gaps: F2 and F12 are unbound (`interpret_key`), Shift+Alt+F falls through to
  typing, there is no `impl … diff` for `Editor` (editor.rs:1173-1180 has only `tag`/`state`),
  `parse_md_runs` (editor.rs:3631) has no escapes, and the diagnostic hover line is
  `format!("**{}:** {msg}", …)` (code_editor.rs:697).

## 2. Goal and exit criteria

**Goal.** The editor grows the three user commands an LSP client serves — goto-definition
(F12), rename (F2, opt-in field) and format (Shift+Alt+F) — as ticketed requests; signature
requests name the call they are in; the widget resets its per-document state when handed a
different document; and the hover card's markdown has one safe grammar with an escape function.

**When this phase is done:**

1. The command state machines work.
   - `goto_definition_selects_the_landed_range_and_reveals_it`
   - `a_click_before_the_definition_lands_drops_it`
   - `a_none_definition_retires_the_request`
   - `format_records_a_request_with_the_indent_size`
   - `rename_submits_the_typed_name_for_the_symbol_at_the_caret`
   - `rename_is_ignored_unless_enabled`
   - `rename_closes_when_the_revision_moves`
   - `select_selects_reveals_and_closes_the_popup`
   - `f12_and_f2_are_the_language_commands` (editor.rs, `interpret_key`)
   - `shift_alt_f_formats_by_physical_key` (editor.rs, widget harness)
2. Rename works through `find_chord(Escape)`, including precedence over find.
   - `escape_through_the_bar_chord_closes_rename_not_find`
   - `escape_closes_rename_even_with_find_disabled`
   - `opening_find_closes_rename`
3. `innermost_open` works with no closing paren and with parens inside a string on the same line.
   - `innermost_open_finds_the_unclosed_call` (bracket.rs)
   - `innermost_open_skips_a_paren_inside_a_string` (bracket.rs)
   - `signature_request_names_the_innermost_open_paren` (code_editor.rs)
   - `a_paren_inside_a_string_is_not_the_call` (code_editor.rs)
4. Swapping documents resets the widget and reveals.
   - `rendering_another_document_resets_the_view_state_and_reveals` (editor.rs)
5. The markdown escape tables pass.
   - `escape_markdown_backslashes_exactly_the_markup_chars` (hover.rs)
   - `parse_md_runs_honors_escapes_and_literal_code` (editor.rs)
   - `escaped_text_renders_verbatim` (editor.rs, round trip)
   - the existing `parse_md_runs_splits_bold_and_code` stays green
6. The hover card keeps its diagnostics on `None`.
   - `the_hover_card_keeps_its_diagnostics_when_the_docs_are_none`
   - `diagnostic_messages_are_escaped_in_the_hover_card`
7. The whole workspace is clippy/doc clean and every earlier test is green.

## 3. Design decisions implemented here

- **D17 — commands.**
  - Bindings on the focused widget, each with a dedicated `CodeEditor::update` arm:
    F12 → `Action::GotoDefinition`, F2 → `Action::Rename`, Shift+Alt+F → `Action::Format`
    (matched on the **physical** `KeyF` in the `KeyPressed` handler before `interpret_key`;
    `physical_key` is in scope there).
  - Plain-data requests, one module each, all with `new`: `intel::definition::DefinitionRequest
    { ticket, offset }`, `intel::rename::RenameRequest { ticket, offset, new_name }`,
    `intel::format::FormatRequest { ticket, tab_size }` (the client always sends
    `insertSpaces: true`).
  - `take_definition_request`, `take_rename_request`, `take_format_request`, and
    `set_definition(ticket, Option<Range<u32>>)` through `accepts` (D11: the awaited slots gain
    `definition`; CaretOrClose clears it; an accepted result — `None` included — retires it).
  - **Rename field:** opt-in via `CodeEditor::rename(bool)` (builder toggle, exempt from the
    no-bool rule). It shares the find bar's plumbing: Escape through the global `find_chord`
    closes rename if it is open and find otherwise; only one bar is open at a time; the chord
    subscription runs when `find_enabled || rename.is_some()`; PointerDown and the focus helpers
    cover `RENAME_INPUT`; `Event::Focused { field, on }` replaces `Focused { replace, on }`. It
    closes when the revision moves.
  - `CodeEditor::select(range)` calls `Document::select_and_reveal` then
    `after_edit(CaretOrClose)`.
  - **Widget `diff`:** when the rendered `DocId` changes, `State` is rebuilt from its default,
    keeping focus, metrics, font and modifiers; the widget then autoscrolls, centered if the
    document ever revealed.
- **D15 (Phase 3 part) — call identity.** `SignatureRequest` gains `call: Option<u32>`, the
  offset of the innermost `(` enclosing the caret, from a new
  `Brackets::innermost_open(offset, b'(')` implemented as
  `enclosing_openers(offset).iter().rev().find(|e| e.ch == b'(')`. Unmatched openers count; it is
  exactly as string/comment-aware as bracket colouring (line-local, needs the grammar's
  `BracketConfig`).
- **D16 (Phase 3 part) — safe hover.** `scrive_core::intel::hover::escape_markdown` owns the
  grammar, documented on `HoverInfo::markdown`: escapes for `\*`, `` \` `` and `\\`, and no
  bold inside code. The widget parser decodes that grammar. `hover_card(offset, docs)` merges
  escaped diagnostics and never drops them on `None`; `set_hover` takes its offset from the
  awaited slot.

**Decisions this doc makes where the plan is silent:**

- Decision: the three command requests are `#[non_exhaustive]` structs with public fields and
  `new`, like `HoverRequest` — the plan calls them "plain data" and they carry no invariant
  beyond their (opaque) `Ticket`.
- Decision: `FormatRequest::tab_size` is `default_indent_size()` (4): scrive indents with that
  many spaces, which is what `insertSpaces: true` + `tabSize` must describe.
- Decision: only the three request types that *moved* in Phase 2 are re-exported from
  `scrive_iced::code_editor`; the new command requests are reached as `scrive_core::…Request`
  (they never lived in scrive-iced, so there is nothing to keep compatible).
- Decision: backslash escapes are honored in **every** style, code included (unlike
  CommonMark). That keeps one grammar: a code line is `` ` `` + `escape_markdown(line)` +
  `` ` `` (Phase 7's `to_hover`), and a backtick inside code can be shown. The parser "uses"
  `escape_markdown` in the sense that it is its exact inverse; the round-trip test pins that.
- Decision: the escape set is exactly `\`, `*`, `` ` ``. Every `*` is escaped (not just pairs) so
  the rule is context-free; a lone `\*` decodes to `*` like a lone `*` would.
- Decision: the rename field is seeded with the completion-word under the caret
  (`word_around(head)`), and `RenameRequest::offset` is the caret at open. Submitting an empty
  name, or after the revision moved, records nothing.
- Decision: the rename field floats where the find bar does (top-right) and reuses its input and
  panel styles; the two style closures in `find_bar` are hoisted into private free fns
  (`bar_input_style`, `bar_panel_style`) because two bars now use them. The find bar's look and
  behavior are unchanged.
- Decision: `Widget::state()` records the document's `DocId`, so the first `diff` after tree
  creation is a no-op; a fresh `State`'s `last_reveal_seq` of 0 is what makes a document that
  ever revealed take the jump path (its recorded `RevealMode`, `Center` after
  `select_and_reveal`/find/F8).

## 4. Step-by-step changes

### Step 1 — `Brackets::innermost_open` (scrive-core)

1a. crates/scrive-core/src/bracket_tree.rs — current private helper (bracket_tree.rs:146-153):

```rust
/// The open brackets enclosing byte `offset` — the opener stack of the shape of
/// everything strictly before it, innermost (top of stack) last, with absolute
/// offsets. ...
fn enclosing_openers(tree: &SumTree<BracketItem>, offset: u32) -> Vec<Entry> {
    tree.summary_before(&ByteDim(offset)).stack
}
```

Add after it (keep `enclosing_openers` and `Entry` private):

```rust
/// The innermost opener `ch` still open at `offset` — matched or not — as its
/// absolute offset. Walks the enclosing opener stack top-down, so it costs
/// O(log + depth). An unmatched opener counts: `foo(a, b` with no `)` yet is
/// still the call being typed.
pub(crate) fn innermost_open(tree: &SumTree<BracketItem>, offset: u32, ch: u8) -> Option<u32> {
    enclosing_openers(tree, offset).iter().rev().find(|e| e.ch == ch).map(|e| e.off)
}
```

1b. crates/scrive-core/src/bracket.rs — add to `impl Brackets` after `innermost_enclosing_where`
(bracket.rs:283-293):

```rust
    /// The offset of the innermost opener `ch` (e.g. `b'('`) still open at
    /// `offset` — strictly before it, matched or not. Unlike
    /// [`Self::enclosing_pair`] an unmatched opener counts, so the call being
    /// typed (`foo(a, b` with no `)` yet) is found. It skips brackets exactly
    /// as the matching does: inside strings and comments when the document's
    /// [`BracketConfig`] says so, line-locally. O(log + depth).
    #[must_use]
    pub fn innermost_open(&self, offset: u32, ch: u8) -> Option<u32> {
        bracket_tree::innermost_open(&self.tree, offset, ch)
    }
```

### Step 2 — the command request modules (scrive-core, new files)

crates/scrive-core/src/intel/definition.rs:

```rust
//! Goto-definition — the request an editor records for F12. The host answers
//! with a range in the same document (the editor's `set_definition`), or opens
//! the target itself when it lies elsewhere.

use crate::intel::ticket::Ticket;

/// A request for the definition of the symbol at `offset`.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct DefinitionRequest {
    /// The ticket the answer must carry to land.
    pub ticket: Ticket,
    /// The caret offset the request was made at.
    pub offset: u32,
}

impl DefinitionRequest {
    /// A definition request at `offset`, made under `ticket`.
    #[must_use]
    pub fn new(ticket: Ticket, offset: u32) -> Self {
        Self { ticket, offset }
    }
}
```

crates/scrive-core/src/intel/rename.rs:

```rust
//! Rename — the request an editor records when the user submits a new name for
//! the symbol under the caret. The answer is a set of edits; the editor's own
//! document gets them as an ordinary edit.

use crate::intel::ticket::Ticket;

/// A request to rename the symbol at `offset` to `new_name`.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct RenameRequest {
    /// The ticket naming the revision the rename was asked at.
    pub ticket: Ticket,
    /// The caret offset of the symbol to rename.
    pub offset: u32,
    /// The name the user typed (never empty).
    pub new_name: String,
}

impl RenameRequest {
    /// A rename of the symbol at `offset` to `new_name`, made under `ticket`.
    #[must_use]
    pub fn new(ticket: Ticket, offset: u32, new_name: impl Into<String>) -> Self {
        Self { ticket, offset, new_name: new_name.into() }
    }
}
```

crates/scrive-core/src/intel/format.rs:

```rust
//! Formatting — the request an editor records for Shift+Alt+F. The answer is a
//! set of edits against the revision named by the ticket.

use crate::intel::ticket::Ticket;

/// A request to format the whole document.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct FormatRequest {
    /// The ticket naming the revision the format was asked at.
    pub ticket: Ticket,
    /// The indent width in spaces — scrive indents with spaces, so a formatter
    /// is told to insert spaces at this width.
    pub tab_size: u32,
}

impl FormatRequest {
    /// A format request at indent width `tab_size`, made under `ticket`.
    #[must_use]
    pub fn new(ticket: Ticket, tab_size: u32) -> Self {
        Self { ticket, tab_size }
    }
}
```

crates/scrive-core/src/intel.rs — add `pub mod definition;`, `pub mod format;`,
`pub mod rename;` (alphabetical, among the existing `pub mod` lines) and change the module
doc's first line to "Language services — completion, signature help, hover, and the
definition / rename / format commands." The commands have no provider trait; mention in one
sentence that they are request-only ("The commands have no provider: the editor always records
a request.").

### Step 3 — `SignatureRequest::call` (crates/scrive-core/src/intel/signature.rs)

Phase 2's struct:

```rust
pub struct SignatureRequest {
    ticket: Ticket,
    position: Point,
}
```

becomes (with `new` and a new accessor):

```rust
pub struct SignatureRequest {
    ticket: Ticket,
    position: Point,
    call: Option<u32>,
}

impl SignatureRequest {
    /// A request for signature help at `position` (the caret), inside the call
    /// whose `(` sits at `call`, made under `ticket`.
    #[must_use]
    pub fn new(ticket: Ticket, position: Point, call: Option<u32>) -> Self {
        Self { ticket, position, call }
    }

    // ticket() and position() unchanged

    /// The offset of the innermost `(` still open at the caret, if any — the
    /// call's identity. Two requests with the same `call` are about the same
    /// call, so a client can keep one in flight while the user types its
    /// arguments. As string- and comment-aware as bracket colouring (line-local).
    #[must_use]
    pub fn call(&self) -> Option<u32> {
        self.call
    }
}
```

### Step 4 — `escape_markdown` and the grammar (crates/scrive-core/src/intel/hover.rs)

4a. `HoverInfo::markdown` doc (hover.rs:44-49). Current:

```rust
    /// Markdown body (a minimal block/inline subset; richer degrades to plain
    /// text at render).
    pub markdown: String,
```

New:

```rust
    /// The card's text, in the small markdown grammar the hover card renders:
    /// - `**` toggles bold, outside code; a single `*` is literal;
    /// - `` ` `` toggles inline code; inside code `**` is literal;
    /// - `\*`, `` \` `` and `\\` are the literal characters, in every style,
    ///   code included; any other `\` is literal;
    /// - each line renders as one line; nothing else is markup.
    ///
    /// Build text that must show verbatim (a diagnostic message, plain-text
    /// docs, a code line's content) with [`escape_markdown`].
    pub markdown: String,
```

4b. Append the function (after `HoverRequest`):

```rust
/// Escape `text` so the hover card shows it verbatim: every `\`, `*` and `` ` ``
/// gets a backslash (see [`HoverInfo::markdown`] for the grammar). The one
/// escape rule — the card's parser is its inverse — so a message like
/// `expected *mut T` can never switch the card to bold.
#[must_use]
pub fn escape_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '*' | '`') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
```

hover.rs has no test module today; add one (`#[cfg(test)] mod tests { use super::*; … }`).

### Step 5 — crate-root exports (crates/scrive-core/src/lib.rs)

After the `pub use intel::completion::…` line add:

```rust
pub use intel::definition::DefinitionRequest;
pub use intel::format::FormatRequest;
pub use intel::rename::RenameRequest;
```

`escape_markdown` stays at `scrive_core::intel::hover::escape_markdown` (a verb reads better
with its module; the root holds types and a few established helpers).

### Step 6 — the widget (crates/scrive-iced/src/editor.rs)

6a. Imports (editor.rs:36-40): add `DocId` to the `scrive_core::{…}` list, and add
`use scrive_core::intel::hover::escape_markdown;` only inside `mod tests` (the parser does not
call it).

6b. `Action` — add after `NextDiagnostic { … }` (editor.rs:352-356):

```rust
    /// Go to the definition of the symbol at the caret (F12). The host decides
    /// what answers; the editor records a ticketed request.
    GotoDefinition,
    /// Rename the symbol at the caret (F2).
    Rename,
    /// Format the document (Shift+Alt+F).
    Format,
```

`moves_caret` (editor.rs:393-432): add `| Action::GotoDefinition | Action::Rename |
Action::Format` to the excluded list (a landed definition reveals through
`select_and_reveal`'s own `request_reveal`).

6c. `interpret_key`, after the F8 arm (editor.rs:3512-3513):

```rust
        // F12 goes to the definition, F2 renames — language commands the host
        // answers (or doesn't).
        Key::Named(Named::F12) => Some(Action::GotoDefinition),
        Key::Named(Named::F2) => Some(Action::Rename),
```

6d. `KeyPressed` handler, immediately before `if let Some(action) = interpret_key(…)`
(editor.rs:2435):

```rust
                // Shift+Alt+F formats. Matched on the PHYSICAL key: with Alt held
                // the logical char differs by layout (macOS yields `Ï`), and the
                // fallthrough in `interpret_key` would type it.
                if modifiers.shift()
                    && modifiers.alt()
                    && !modifiers.control()
                    && !modifiers.logo()
                    && matches!(
                        physical_key,
                        iced::keyboard::key::Physical::Code(iced::keyboard::key::Code::KeyF)
                    )
                {
                    state.ping();
                    shell.publish((self.on_action)(Action::Format));
                    shell.capture_event();
                    return;
                }
```

6e. `State` gains the rendered document (after `hover_chip` at editor.rs:537):

```rust
    /// The document this state was built for. The widget tree keeps state by
    /// position, so a host that renders another document in this place (a tab
    /// switch) hands its scroll, drags and hover to the wrong text; `diff`
    /// compares this to the incoming document and starts over on a change.
    doc: Option<DocId>,
```

`impl Default for State` adds `doc: None,`.

6f. `Widget` impl, editor.rs:1173-1180. Current:

```rust
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<State>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(State::default())
    }
```

New:

```rust
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<State>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(State { doc: Some(self.doc.doc_id()), ..State::default() })
    }

    /// Start the view state over when a different document is rendered here.
    /// Scroll, drags, hover and fold previews describe the old document's text;
    /// focus, the measured metrics (and the font they were measured for) and
    /// the held modifiers describe the widget, so they carry over. The fresh
    /// state autoscrolls once; its `last_reveal_seq` of 0 sends a document that
    /// ever requested a reveal down the jump path, centered where its last jump
    /// left it.
    fn diff(&mut self, tree: &mut widget::Tree) {
        let state = tree.state.downcast_mut::<State>();
        let doc = self.doc.doc_id();
        if state.doc == Some(doc) {
            return;
        }
        *state = State {
            focus: state.focus,
            metrics: state.metrics,
            measured_font: state.measured_font,
            modifiers: state.modifiers,
            doc: Some(doc),
            autoscroll: true,
            ..State::default()
        };
    }
```

(`Focus`, `Metrics`, `Option<Font>` and `Modifiers` are all `Copy`. The editor has no child
widgets, so there is no `tree.children` to reconcile.)

6g. `parse_md_runs`, editor.rs:3628-3659. Current:

```rust
/// Split one markdown line into styled runs, consuming the `**bold**` and
/// `` `code` `` markers. A minimal inline subset (no nesting) — enough for the
/// hover's spec-derived docs; unmatched markers just toggle back at line end.
fn parse_md_runs(line: &str) -> Vec<(String, MdStyle)> {
    let mut runs = Vec::new();
    let mut cur = String::new();
    let mut style = MdStyle::Plain;
    let mut push = |cur: &mut String, style: MdStyle| {
        if !cur.is_empty() {
            runs.push((std::mem::take(cur), style));
        }
    };
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' if chars.peek() == Some(&'*') => {
                chars.next(); // second '*'
                push(&mut cur, style);
                style = if style == MdStyle::Bold { MdStyle::Plain } else { MdStyle::Bold };
            }
            '`' => {
                push(&mut cur, style);
                style = if style == MdStyle::Code { MdStyle::Plain } else { MdStyle::Code };
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        runs.push((cur, style));
    }
    runs
}
```

New (doc + two arms change; the rest is identical):

```rust
/// Split one markdown line into styled runs — the decode side of the grammar
/// documented on `HoverInfo::markdown` and produced by `escape_markdown`:
/// `**` toggles bold outside code, `` ` `` toggles code, and `\*`, `` \` ``,
/// `\\` are literal characters in every style. No nesting; unmatched markers
/// just toggle back at line end.
fn parse_md_runs(line: &str) -> Vec<(String, MdStyle)> {
    let mut runs = Vec::new();
    let mut cur = String::new();
    let mut style = MdStyle::Plain;
    let mut push = |cur: &mut String, style: MdStyle| {
        if !cur.is_empty() {
            runs.push((std::mem::take(cur), style));
        }
    };
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // An escape is its character, whatever the style — so escaped text
            // (a diagnostic, a code line) renders exactly as written.
            '\\' => match chars.next_if(|&n| matches!(n, '\\' | '*' | '`')) {
                Some(escaped) => cur.push(escaped),
                None => cur.push('\\'),
            },
            // Inside code, `**` is text: code never turns bold.
            '*' if style != MdStyle::Code && chars.peek() == Some(&'*') => {
                chars.next(); // second '*'
                push(&mut cur, style);
                style = if style == MdStyle::Bold { MdStyle::Plain } else { MdStyle::Bold };
            }
            '`' => {
                push(&mut cur, style);
                style = if style == MdStyle::Code { MdStyle::Plain } else { MdStyle::Code };
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        runs.push((cur, style));
    }
    runs
}
```

### Step 7 — `CodeEditor` (crates/scrive-iced/src/code_editor.rs)

7a. Imports: add `DefinitionRequest, FormatRequest, RenameRequest` to the `scrive_core::{…}`
list, and `use scrive_core::intel::hover::escape_markdown;`.

7b. Constants (after `REPLACE_INPUT`, code_editor.rs:59-60):

```rust
/// The rename field's input. It shares the find bar's focus plumbing.
const RENAME_INPUT: &str = "scrive-rename-input";
```

7c. `Event` (code_editor.rs:72-127):
- `CloseFind` doc (code_editor.rs:85-86) becomes: "Close the open bar (Escape): the rename
  field if it is open, else the find bar — returning focus to the editor."
- Add after `TogglePreserveCase`:

  ```rust
      /// The rename field's text changed.
      RenameText(String),
      /// Submit the rename field (Enter): record a rename request and close it.
      SubmitRename,
  ```

- Replace `Focused` (code_editor.rs:120-126). Current:

  ```rust
      /// A bar input gained or lost native focus; mirror it into the ring flags.
      Focused {
          /// Whether this is the replace input (else the find input).
          replace: bool,
          /// Whether it is now focused.
          on: bool,
      },
  ```

  New:

  ```rust
      /// A bar input gained or lost native focus; mirror it into the ring flags.
      Focused {
          /// Which input.
          field: Field,
          /// Whether it is now focused.
          on: bool,
      },
  ```

- After `enum Event`, add the public payload type (it appears in the public `Event`, so it
  must be public; hosts never construct it):

  ```rust
  /// One of the floating bars' text inputs, for [`Event::Focused`].
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum Field {
      /// The find bar's query input.
      Find,
      /// The find bar's replacement input.
      Replace,
      /// The rename field.
      Rename,
  }
  ```

7d. Private state for the rename field, next to `Awaiting`:

```rust
/// The open rename field: the name being typed, the caret offset and revision
/// it was opened at (a rename names the symbol there, so an edit underneath
/// closes it), and whether its input wears the focus ring.
struct Rename {
    text: String,
    offset: u32,
    revision: Revision,
    focused: bool,
}
```

`Awaiting` gains `definition: Option<Ticket>,` (doc: "The goto-definition ticket; a landed range
selects and reveals."). `Awaited` gains `Definition`. `accepts` gains
`Awaited::Definition => self.awaiting.definition,`; `abandon` gains

```rust
            Awaited::Definition => {
                self.awaiting.definition = None;
                self.pending_definition_request = None;
            }
```

7e. Fields, after `pending_hover_request`:

```rust
    /// A pending goto-definition request (F12), for the host to pull via
    /// [`take_definition_request`](CodeEditor::take_definition_request).
    pending_definition_request: Option<DefinitionRequest>,
    /// A pending rename request (the rename field submitted).
    pending_rename_request: Option<RenameRequest>,
    /// A pending format request (Shift+Alt+F).
    pending_format_request: Option<FormatRequest>,
    /// Whether F2 opens the rename field. Off by default: a host without a
    /// rename provider would otherwise show a field that does nothing.
    rename_enabled: bool,
    /// The open rename field, if any. Only one floating bar is open at a time.
    rename: Option<Rename>,
```

Initialize them in `new`: `None, None, None, false, None`.

7f. Builder, after `find` (code_editor.rs:369-375):

```rust
    /// Enable or disable the rename field that F2 opens. Default off: turn it on
    /// when the host answers [`take_rename_request`](CodeEditor::take_rename_request).
    #[must_use]
    pub fn rename(mut self, enabled: bool) -> Self {
        self.rename_enabled = enabled;
        self
    }
```

7g. Public API, after `take_hover_request`/`set_hover`:

```rust
    /// Take the pending goto-definition request, if any. Answer a target in
    /// this document through [`set_definition`](Self::set_definition).
    pub fn take_definition_request(&mut self) -> Option<DefinitionRequest> {
        self.pending_definition_request.take()
    }

    /// Land a goto-definition answer stamped with the request's `ticket`: a
    /// range in this document is selected and revealed; `None` (no definition,
    /// or one the host opens elsewhere) just retires the request. Dropped if
    /// the user moved the caret or edited since.
    pub fn set_definition(&mut self, ticket: Ticket, target: Option<Range<u32>>) {
        if !self.accepts(Awaited::Definition, ticket) {
            return;
        }
        self.abandon(Awaited::Definition);
        if let Some(range) = target {
            self.select(range);
        }
    }

    /// Take the pending rename request, if any.
    pub fn take_rename_request(&mut self) -> Option<RenameRequest> {
        self.pending_rename_request.take()
    }

    /// Take the pending format request, if any.
    pub fn take_format_request(&mut self) -> Option<FormatRequest> {
        self.pending_format_request.take()
    }

    /// Select `range` and reveal it centered, unfolding what hides it — the
    /// blessed programmatic jump (clamped and snapped to char boundaries). Runs
    /// the post-edit tail as a caret move: the popup closes and pending
    /// requests tied to the old caret are retired.
    pub fn select(&mut self, range: Range<u32>) {
        self.doc.select_and_reveal(range);
        self.after_edit(CompletionEvent::CaretOrClose);
    }
```

7h. `set_hover` (Phase 2 version) becomes:

```rust
    /// Ingest a hover card from an async source, stamped with the request's
    /// `ticket`. The card shows the diagnostics under the hovered offset first,
    /// then these docs; `None` leaves just the diagnostics (or nothing).
    pub fn set_hover(&mut self, ticket: Ticket, info: Option<HoverInfo>) {
        if !self.accepts(Awaited::Hover, ticket) {
            return;
        }
        let Some(offset) = self.awaiting.hover.as_ref().map(|(_, offset, _)| *offset) else {
            return;
        };
        if info.is_none() {
            self.abandon(Awaited::Hover);
        }
        self.hover = self.hover_card(offset, info);
    }
```

7i. `hover_card` — new private method, near `build_hover_cx`:

```rust
    /// The hover card for `offset`: the diagnostics under it — escaped, so a
    /// message's own `*` or backtick renders literally — then the docs after a
    /// blank line. The diagnostics never depend on the docs: a `None` reply
    /// still leaves them showing.
    fn hover_card(&self, offset: u32, docs: Option<HoverInfo>) -> Option<HoverInfo> {
        let diags: Vec<(Range<u32>, String)> = self
            .doc
            .diagnostics_in(offset..offset + 1)
            .map(|(r, sev, msg)| (r, format!("**{}:** {}", severity_label(sev), escape_markdown(&msg))))
            .collect();
        let Some(range) = diags.first().map(|(r, _)| r.clone()) else {
            return docs;
        };
        let mut md: Vec<String> = diags.into_iter().map(|(_, m)| m).collect();
        if let Some(docs) = docs {
            md.push(String::new());
            md.push(docs.markdown);
        }
        Some(HoverInfo { markdown: md.join("\n"), range })
    }
```

7j. `update` arms (locate by pattern):

- `HoverQuery` — replace the inline merge (HEAD code_editor.rs:694-713) so the arm reads:

  ```rust
              Event::Editor(Action::HoverQuery(offset)) => {
                  let cx = self.build_hover_cx(offset);
                  let has_word = cx.word.start != cx.word.end;
                  let docs = has_word
                      .then(|| self.hover_provider.as_mut().and_then(|p| p.hover(&cx)))
                      .flatten();
                  // Diagnostics show at once; async docs join them when they land.
                  self.hover = self.hover_card(offset, docs);
                  if self.hover_provider.is_none() && has_word {
                      let ticket = self.tickets.issue(self.doc.revision());
                      self.awaiting.hover = Some((ticket, offset, cx.word.clone()));
                      self.pending_hover_request = Some(HoverRequest::new(ticket, offset, cx.word.clone()));
                  } else {
                      // Nothing asked here, so a late reply to an earlier query
                      // would be for a spot the pointer has left.
                      self.abandon(Awaited::Hover);
                  }
                  Task::none()
              }
  ```

- New command arms, before the catch-all `Event::Editor(action) => { self.apply(action); … }`:

  ```rust
              // F12: ask where the symbol at the caret is defined.
              Event::Editor(Action::GotoDefinition) => {
                  let head = self.doc.selections().newest().head();
                  let ticket = self.tickets.issue(self.doc.revision());
                  self.awaiting.definition = Some(ticket);
                  self.pending_definition_request = Some(DefinitionRequest::new(ticket, head));
                  Task::none()
              }
              // F2: open the rename field on the symbol at the caret — one bar at
              // a time, so an open find bar closes.
              Event::Editor(Action::Rename) if self.rename_enabled => {
                  if self.find_open {
                      self.close_find();
                  }
                  let head = self.doc.selections().newest().head();
                  let word = self.word_around(head);
                  self.rename = Some(Rename {
                      text: self.doc.buffer().slice(word).into_owned(),
                      offset: head,
                      revision: self.doc.revision(),
                      focused: true,
                  });
                  focus(RENAME_INPUT)
              }
              // Rename not enabled: F2 does nothing (and must not reach `apply`,
              // whose tail would close the popup as if the caret moved).
              Event::Editor(Action::Rename) => Task::none(),
              // Shift+Alt+F: ask for the whole document formatted.
              Event::Editor(Action::Format) => {
                  let ticket = self.tickets.issue(self.doc.revision());
                  self.pending_format_request = Some(FormatRequest::new(ticket, default_indent_size()));
                  Task::none()
              }
  ```

- `OpenFind` arm: first statement `self.rename = None; // one bar at a time`.
- `PointerDown` arm guard: `Event::PointerDown if self.find_open || self.rename.is_some() =>`.
- `Focused` arm (HEAD code_editor.rs:802-809) becomes:

  ```rust
              Event::Focused { field, on } => {
                  match field {
                      Field::Find => self.find_focused = on,
                      Field::Replace => self.replace_focused = on,
                      Field::Rename => {
                          if let Some(rename) = &mut self.rename {
                              rename.focused = on;
                          }
                      }
                  }
                  Task::none()
              }
  ```

- Escape handling. Current (HEAD code_editor.rs:810-813):

  ```rust
              Event::CloseFind if self.find_open => {
                  self.close_find();
                  focus(self.id.clone())
              }
  ```

  New — the rename arm goes first:

  ```rust
              // Escape arrives here from the global bar chord, whatever holds
              // focus. Only one bar is open at a time; the rename field wins.
              Event::CloseFind if self.rename.is_some() => {
                  self.rename = None;
                  focus(self.id.clone())
              }
              Event::CloseFind if self.find_open => {
                  self.close_find();
                  focus(self.id.clone())
              }
  ```

- Rename field arms, after `TogglePreserveCase`:

  ```rust
              Event::RenameText(text) => {
                  if let Some(rename) = &mut self.rename {
                      rename.text = text;
                  }
                  Task::none()
              }
              // Enter: ask for the rename and close the field. A name typed over
              // text that has since changed, or an empty one, asks nothing.
              Event::SubmitRename => {
                  let Some(rename) = self.rename.take() else { return Task::none() };
                  if rename.revision == self.doc.revision() && !rename.text.is_empty() {
                      let ticket = self.tickets.issue(rename.revision);
                      self.pending_rename_request = Some(RenameRequest::new(ticket, rename.offset, rename.text));
                  }
                  focus(self.id.clone())
              }
  ```

  The trailing "guards that did not hold" arm already lists `CloseFind` and `PointerDown`;
  nothing new needs adding there.

7k. `view` (HEAD code_editor.rs:929-941). Current tail:

```rust
        if self.find_open {
            // Float the find bar over the editor, top-right ...
            let overlay = container(self.find_bar())
                .width(Length::Fill)
                .align_x(Horizontal::Right)
                .padding(iced::Padding::new(8.0).right(8.0 + crate::SCROLLBAR_WIDTH));
            stack([editor.into(), overlay.into()]).into()
        } else {
            editor.into()
        }
```

New:

```rust
        // One floating bar at a time: the rename field while it is open, else
        // the find bar. Top-right, where mainstream editors put it; the right
        // padding clears the scrollbar lane, and the overlay is transparent
        // except the bar, so clicks pass through.
        let bar = match &self.rename {
            Some(rename) => Some(rename_bar(rename)),
            None => self.find_open.then(|| self.find_bar()),
        };
        match bar {
            Some(bar) => {
                let overlay = container(bar)
                    .width(Length::Fill)
                    .align_x(Horizontal::Right)
                    .padding(iced::Padding::new(8.0).right(8.0 + crate::SCROLLBAR_WIDTH));
                stack([editor.into(), overlay.into()]).into()
            }
            None => editor.into(),
        }
```

7l. Shared bar styles and the rename bar — private free fns next to `box_of`:

```rust
/// The bars' text-input look: transparent, sitting inside a `box_of` container
/// that draws the box and the focus ring.
fn bar_input_style(_theme: &Theme, _status: text_input::Status) -> text_input::Style {
    text_input::Style {
        background: Color::TRANSPARENT.into(),
        border: iced::border::rounded(0.0),
        placeholder: Color::from_rgb8(0xA6, 0xA6, 0xA6),
        value: Color::from_rgb8(0xCC, 0xCC, 0xCC),
        selection: Color::from_rgb8(0x26, 0x4F, 0x78),
    }
}

/// The floating bars' panel: dark fill, hairline border, soft drop shadow.
fn bar_panel_style(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Color::from_rgb8(0x25, 0x25, 0x26).into()),
        border: iced::border::rounded(8.0).color(Color::from_rgb8(0x45, 0x45, 0x45)).width(1.0),
        shadow: Shadow { color: Color::from_rgba8(0, 0, 0, 0.36), offset: Vector::new(0.0, 2.0), blur_radius: 8.0 },
        ..container::Style::default()
    }
}

/// The rename field: one input seeded with the symbol under the caret, in the
/// find bar's panel. Enter submits; Escape (the shared bar chord) closes.
fn rename_bar(rename: &Rename) -> Element<'_, Event> {
    // The find bar's input width and row height, so the two bars match.
    const INPUT_W: f32 = 264.0;
    const ROW_H: f32 = 26.0;
    let field = text_input("Rename symbol", &rename.text)
        .id(RENAME_INPUT)
        .on_input(Event::RenameText)
        .on_submit(Event::SubmitRename)
        .padding(iced::Padding::new(4.0).left(2.0))
        .size(13)
        .width(Length::Fill)
        .style(bar_input_style);
    container(box_of(field.into(), Vec::new(), rename.focused, INPUT_W, ROW_H, 3.0, 3.0))
        .padding([6, 8])
        .style(bar_panel_style)
        .into()
}
```

In `find_bar`, delete the local `input_style` closure (HEAD code_editor.rs:978-984) and use
`.style(bar_input_style)` on both inputs; replace the panel's inline `.style(|_theme: &Theme|
container::Style { … })` (HEAD code_editor.rs:1104-1113) with `.style(bar_panel_style)`. The
values are copied verbatim, so the find bar is pixel-identical.

7m. `subscription` (HEAD code_editor.rs:1132-1153). Change the guard
`let keys = if self.find_enabled {` to `let keys = if self.find_enabled || self.rename.is_some() {`
and add one sentence to the comment above it: "The same chords close the rename field, so they
are live while it is open even with find disabled."

7n. `sync_rings` / `resync_focus` (HEAD code_editor.rs:1218-1237). New bodies:

```rust
    fn sync_rings() -> Task<Event> {
        Task::batch([
            is_focused(FIND_INPUT).map(|on| Event::Focused { field: Field::Find, on }),
            is_focused(REPLACE_INPUT).map(|on| Event::Focused { field: Field::Replace, on }),
            is_focused(RENAME_INPUT).map(|on| Event::Focused { field: Field::Rename, on }),
        ])
    }

    fn resync_focus() -> Task<Event> {
        Task::batch([
            is_focused(FIND_INPUT).then(|f| if f { focus(FIND_INPUT) } else { Task::none() }),
            is_focused(REPLACE_INPUT).then(|f| if f { focus(REPLACE_INPUT) } else { Task::none() }),
            is_focused(RENAME_INPUT).then(|f| if f { focus(RENAME_INPUT) } else { Task::none() }),
        ])
    }
```

(keep their doc comments; say "a bar input" instead of "an input" where they name the find bar).

7o. `apply`'s exhaustive no-op arm: add `| Action::GotoDefinition | Action::Rename |
Action::Format`.

7p. `after_edit` — the Phase 2 block

```rust
        if matches!(comp_event, CompletionEvent::CaretOrClose) {
            self.abandon(Awaited::Hover);
        }
```

becomes

```rust
        // A caret jump abandons a pending hover and definition; typing keeps
        // them (their replies are then dropped by revision).
        if matches!(comp_event, CompletionEvent::CaretOrClose) {
            self.abandon(Awaited::Hover);
            self.abandon(Awaited::Definition);
        }
        // The rename field names the symbol at the revision it opened on; an
        // edit underneath (a host edit, an undo) makes that stale.
        if self.rename.as_ref().is_some_and(|r| r.revision != self.doc.revision()) {
            self.rename = None;
        }
```

7q. `drive_signature` async branch (Phase 2 version):

```rust
            let head = self.doc.selections().newest().head();
            let ticket = self.tickets.issue(self.doc.revision());
            self.awaiting.signature = Some(ticket);
            self.pending_signature_request =
                Some(SignatureRequest::new(ticket, self.doc.buffer().offset_to_point(head)));
```

becomes

```rust
            let head = self.doc.selections().newest().head();
            let ticket = self.tickets.issue(self.doc.revision());
            // The call's `(` names which call this is, so a client can keep one
            // request in flight while the arguments are typed.
            let call = self.doc.brackets().innermost_open(head, b'(');
            self.awaiting.signature = Some(ticket);
            self.pending_signature_request =
                Some(SignatureRequest::new(ticket, self.doc.buffer().offset_to_point(head), call));
```

## 5. Files that change

| File | Change |
|---|---|
| crates/scrive-core/src/bracket_tree.rs | `pub(crate) fn innermost_open` over the private `enclosing_openers` |
| crates/scrive-core/src/bracket.rs | `Brackets::innermost_open`; tests |
| crates/scrive-core/src/intel/definition.rs | **new**: `DefinitionRequest` |
| crates/scrive-core/src/intel/rename.rs | **new**: `RenameRequest` |
| crates/scrive-core/src/intel/format.rs | **new**: `FormatRequest` |
| crates/scrive-core/src/intel.rs | three `pub mod`s; doc line |
| crates/scrive-core/src/intel/signature.rs | `SignatureRequest.call`, `new(ticket, position, call)`, `call()` |
| crates/scrive-core/src/intel/hover.rs | `escape_markdown`; `HoverInfo::markdown` grammar doc; tests |
| crates/scrive-core/src/lib.rs | root re-exports of the three command requests |
| crates/scrive-iced/src/editor.rs | `Action::{GotoDefinition, Rename, Format}`, `moves_caret`, F12/F2, physical Shift+Alt+F, `State::doc`, `state()`/`diff`, safe `parse_md_runs`; tests |
| crates/scrive-iced/src/code_editor.rs | `Field`, `Event::{RenameText, SubmitRename}`, `Focused { field, on }`; `Rename`; definition slot; command requests/`take_*`/`set_definition`/`select`/`rename(bool)`; `hover_card`; rename bar + shared bar styles; subscription/focus plumbing; `drive_signature` call; tests |

## 6. Tests to add

### bracket.rs (`mod tests`; `cfg()` at bracket.rs:359 has `//` comments and `"` strings)

- **`innermost_open_finds_the_unclosed_call`** — table over `Brackets::match_text`:
  ```rust
  for (text, at, want) in [
      ("foo(a, b", 8, Some(3)),     // no closing paren yet
      ("f(g(x), y", 9, Some(1)),    // the closed inner call doesn't count
      ("f(g(x", 5, Some(3)),        // two open: the innermost wins
      ("f(x)", 4, None),            // closed before the caret
      ("f(x)", 2, Some(1)),
      ("(", 0, None),               // strictly before the offset
      ("x = [1, 2", 9, None),       // other openers are ignored
      ("a[f(b]", 6, Some(3)),       // a mismatched closer drops, the ( stays open
  ] {
      let b = Brackets::match_text(text);
      assert_eq!(b.innermost_open(at, b'('), want, "{text:?} at {at}");
  }
  ```
  Verify the `a[f(b]` row against the shape rules before relying on it (a `]` against an open
  `(` on top is a mismatch and is dropped, so `(` at 3 stays open); drop the row if the
  implementation disagrees and note it — it is a characterization, not a requirement.
- **`innermost_open_skips_a_paren_inside_a_string`** — `let text = "f(\"(\", x";` with
  `Brackets::match_text_with(text, &cfg())`: `innermost_open(8, b'(') == Some(1)`; with the
  structural `match_text(text)`: `Some(3)` (the string's paren counts without lexing). Also a
  comment: `"f( // (\n"` under `cfg()` at offset 7 → `Some(1)`.

### intel/hover.rs (new `mod tests`)

- **`escape_markdown_backslashes_exactly_the_markup_chars`**
  ```rust
  for (raw, escaped) in [
      ("plain", "plain"),
      ("a*b", "a\\*b"),
      ("**x**", "\\*\\*x\\*\\*"),
      ("`c`", "\\`c\\`"),
      ("a\\b", "a\\\\b"),
      ("expected *mut T, found `&T`", "expected \\*mut T, found \\`&T\\`"),
      ("", ""),
  ] {
      assert_eq!(escape_markdown(raw), escaped, "{raw:?}");
  }
  ```

### editor.rs (`mod tests`)

- **`parse_md_runs_honors_escapes_and_literal_code`** (`use MdStyle::*`):

  | Input | Runs |
  |---|---|
  | `a \*\* b` | `[("a ** b", Plain)]` |
  | `` \`x\` `` | `[("`x`", Plain)]` |
  | `a\\b` | `[("a\b", Plain)]` |
  | `\q` | `[("\q", Plain)]` (unknown escape stays) |
  | `a\` | `[("a\", Plain)]` (trailing backslash) |
  | `` `a**b` `` | `[("a**b", Code)]` (no bold inside code) |
  | `` `a\`b` `` | `[("a`b", Code)]` |
  | `**x** \*` | `[("x", Bold), (" *", Plain)]` |

  Write the inputs as Rust string literals (`"a \\*\\* b"`, `"`a\\`b`"`, …).
- **`escaped_text_renders_verbatim`** — for each of `"a*b"`, `"**x**"`, `"`c`"`, `"a\\b"`,
  `"\\*"`, `"mixed * ` \\ **"`: `parse_md_runs(&escape_markdown(s)) == vec![(s.to_string(),
  Plain)]`; and `parse_md_runs(&format!("`{}`", escape_markdown("a`b**c"))) ==
  vec![("a`b**c".to_string(), Code)]`.
- **`f12_and_f2_are_the_language_commands`** — `interpret_key(&Key::Named(Named::F12), None,
  Modifiers::default()) == Some(Action::GotoDefinition)`; F2 → `Some(Action::Rename)`.
- **`shift_alt_f_formats_by_physical_key`** — with Phase 2's `headless_renderer`/`pump` and
  `doc = Document::new("x\n")`, pump one event:
  ```rust
  use iced::keyboard::{key::{Code, Physical}, Location};
  let press = iced::Event::Keyboard(Keyboard::KeyPressed {
      key: Key::Character("Ï".into()),
      modified_key: Key::Character("Ï".into()),
      physical_key: Physical::Code(Code::KeyF),
      location: Location::Standard,
      modifiers: Modifiers::SHIFT | Modifiers::ALT,
      text: Some("Ï".into()),
      repeat: false,
  });
  ```
  Assert the actions contain `Action::Format` and no `Action::Type(_)`.
- **`rendering_another_document_resets_the_view_state_and_reveals`**
  ```rust
  use iced::advanced::widget::Tree;
  let a = Document::new("one\n").unwrap();
  let mut b = Document::new(&"line\n".repeat(400)).unwrap();
  b.select_and_reveal(1000..1004);
  let mut on_a = Editor::new(&a, |x: Action| x);
  let mut tree = Tree::new(&on_a as &dyn Widget<Action, iced::Theme, iced::Renderer>);
  {
      let st = tree.state.downcast_mut::<State>();
      st.scroll = ScrollAnchor::from_rows(120.0, 19.0);
      st.hover_scroll = 38.0;
      st.unfocus();
  }
  Widget::<Action, iced::Theme, iced::Renderer>::diff(&mut on_a, &mut tree);
  assert_eq!(tree.state.downcast_ref::<State>().hover_scroll, 38.0, "the same document keeps its view state");
  let mut on_b = Editor::new(&b, |x: Action| x);
  Widget::<Action, iced::Theme, iced::Renderer>::diff(&mut on_b, &mut tree);
  let st = tree.state.downcast_ref::<State>();
  assert_eq!(st.scroll, ScrollAnchor::TOP, "a new document starts unscrolled");
  assert_eq!(st.hover_scroll, 0.0, "hover state belongs to the old document");
  assert!(st.autoscroll, "the new document is revealed once");
  assert_eq!(st.last_reveal_seq, 0, "a document that revealed takes the jump path (centered)");
  assert!(!st.is_focused(), "focus belongs to the widget and survives");
  assert_eq!(st.doc, Some(b.doc_id()));
  ```

### code_editor.rs (`mod tests`; Phase 2's `act`, `item`, `shown`, `sig` helpers)

- **`goto_definition_selects_the_landed_range_and_reveals_it`**
  ```rust
  let mut ed = CodeEditor::new("fn foo() {}\nfoo();\n");
  act(&mut ed, Action::PlaceCaret(13));
  act(&mut ed, Action::GotoDefinition);
  let req = ed.take_definition_request().expect("F12 asks");
  assert_eq!(req.offset, 13);
  let seq = ed.document().reveal_seq();
  ed.set_definition(req.ticket, Some(3..6));
  assert_eq!(ed.selection(), 3..6, "the definition is selected");
  assert!(ed.document().reveal_seq() > seq, "…and revealed");
  ```
- **`a_click_before_the_definition_lands_drops_it`** — F12, `PlaceCaret(0)`, then
  `set_definition(ticket, Some(3..6))`: selection stays `0..0`.
- **`a_none_definition_retires_the_request`** — F12, `set_definition(t, None)`, then
  `set_definition(t, Some(3..6))` is dropped.
- **`format_records_a_request_with_the_indent_size`** — `Format` → `take_format_request()`:
  `tab_size == default_indent_size()`, `ticket.revision() == ed.document().revision()`; the
  document is unchanged.
- **`rename_submits_the_typed_name_for_the_symbol_at_the_caret`**
  ```rust
  let mut ed = CodeEditor::new("let foo = 1;\n").rename(true);
  act(&mut ed, Action::PlaceCaret(5));
  act(&mut ed, Action::Rename);
  assert_eq!(ed.rename.as_ref().map(|r| r.text.as_str()), Some("foo"), "seeded with the symbol");
  let _ = ed.update(Event::RenameText("bar".into()), Instant::now());
  let _ = ed.update(Event::SubmitRename, Instant::now());
  let req = ed.take_rename_request().expect("Enter asks");
  assert_eq!((req.offset, req.new_name.as_str()), (5, "bar"));
  assert!(ed.rename.is_none(), "submitting closes the field");
  ```
  Plus: submitting an empty name records nothing.
- **`rename_is_ignored_unless_enabled`** — without `.rename(true)`: `Rename` leaves
  `ed.rename` `None`, and an open popup (land one first) stays open.
- **`rename_closes_when_the_revision_moves`** — open it, `ed.edit(vec![EditOp::insert(0,
  "x")])` → `ed.rename.is_none()`; `SubmitRename` afterwards records nothing.
- **`escape_through_the_bar_chord_closes_rename_not_find`**
  ```rust
  use iced::keyboard::{key::Named, Key, Modifiers};
  let esc = find_chord(&Key::Named(Named::Escape), Modifiers::empty(), iced::event::Status::Captured)
      .expect("Escape is a bar chord");
  let mut ed = CodeEditor::new("let foo = 1;\n").rename(true);
  let _ = ed.update(Event::OpenFind, Instant::now());
  act(&mut ed, Action::Rename);
  assert!(!ed.find_open && ed.rename.is_some(), "one bar at a time");
  let _ = ed.update(esc, Instant::now());
  assert!(ed.rename.is_none(), "Escape closes the rename field");
  assert!(!ed.find_open, "find stays closed");
  ```
- **`escape_closes_rename_even_with_find_disabled`** — `.find(false).rename(true)`, open rename,
  send the chord's event: closed.
- **`opening_find_closes_rename`** — open rename, `Event::OpenFind`: `rename.is_none()` and
  `find_open`.
- **`select_selects_reveals_and_closes_the_popup`** — land a popup (`Type('h')` + reply), then
  `ed.select(0..1)`: popup `Closed`, `selection() == 0..1`, `reveal_seq` bumped.
- **`signature_request_names_the_innermost_open_paren`** — `CodeEditor::new("")`, type
  `f ( g ( x ) ,` (auto-close may insert `)`s; the offsets below do not depend on it): the last
  `take_signature_request().call() == Some(1)`.
- **`a_paren_inside_a_string_is_not_the_call`**
  ```rust
  let mut ed = CodeEditor::new("g\nf(\"(\", \n").bracket_lexing(vec![b'"'], None);
  act(&mut ed, Action::PlaceCaret(1));
  act(&mut ed, Action::Type('(')); // awaits signature help
  let _ = ed.take_signature_request();
  let text = ed.document().text().into_owned();
  let call = text.find("f(").unwrap() as u32 + 1;
  let end = text[call as usize..].find('\n').unwrap() as u32 + call;
  act(&mut ed, Action::PlaceCaret(end)); // re-queries while awaited
  let req = ed.take_signature_request().expect("a move re-queries while awaited");
  assert_eq!(req.call(), Some(call), "the paren inside the string is skipped");
  ```
- **`the_hover_card_keeps_its_diagnostics_when_the_docs_are_none`**
  ```rust
  let mut ed = CodeEditor::new("hello world\n");
  let rev = ed.document().revision();
  let _ = ed.set_diagnostics(rev, vec![Diagnostic::new(0..5, Severity::Error, "unknown name")]);
  act(&mut ed, Action::HoverQuery(2));
  let req = ed.take_hover_request().expect("a word with no provider asks");
  ed.set_hover(req.ticket, None);
  let card = ed.hover.as_ref().expect("the diagnostics stay");
  assert!(card.markdown.starts_with("**error:** unknown name"), "{}", card.markdown);
  ```
  And with docs: a fresh query answered with `Some(HoverInfo { markdown: "docs".into(), range:
  0..5 })` gives `"**error:** unknown name\n\ndocs"`.
- **`diagnostic_messages_are_escaped_in_the_hover_card`** — diagnostic message
  `"expected *mut T"` → the card contains `"expected \\*mut T"`.

## 7. Verification

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
cargo build --workspace --all-targets --target wasm32-unknown-unknown
out=$(cargo tree -p scrive-core -e normal --prefix none) || exit 1; if grep -qE '^(iced|winit|wgpu)' <<<"$out"; then echo LEAK; exit 1; fi
cargo test -p scrive-core -- innermost_open escape_markdown
cargo test -p scrive-iced -- rename definition format select hover_card parse_md escaped_text rendering_another shift_alt_f f12
grep -n "Focused { replace" -r crates/          # no hits
```

## 8. Spot checks

`Brackets::innermost_open(offset, b'(')`:

| Text | Offset | Config | Result |
|---|---|---|---|
| `foo(a, b` | 8 | none | `Some(3)` |
| `f(g(x), y` | 9 | none | `Some(1)` |
| `f(g(x` | 5 | none | `Some(3)` |
| `f(x)` | 4 | none | `None` |
| `(` | 0 | none | `None` (strictly before) |
| `f("(", x` | 8 | `"` strings | `Some(1)` |
| `f("(", x` | 8 | none | `Some(3)` |
| `x = [1, 2` | 9 | none | `None` |

`escape_markdown` / `parse_md_runs`:

| Raw | Escaped | Rendered (after parse) |
|---|---|---|
| `a*b` | `a\*b` | `a*b` plain |
| `**x**` | `\*\*x\*\*` | `**x**` plain, not bold |
| `` `c` `` | `` \`c\` `` | `` `c` `` plain, not code |
| `a\b` | `a\\b` | `a\b` |
| code line `` a`b**c `` | `` `a\`b\*\*c` `` | `` a`b**c `` as code |
| (markup) `` `a**b` `` | — | `a**b` as code, no bold |
| (markup) `\q` | — | `\q` |

Rename / Escape precedence (`find_chord(Escape)` → `Event::CloseFind`):

| State before | After Escape |
|---|---|
| rename open (find closed, by construction) | rename closed, focus → editor |
| find open, rename closed | find closed |
| both closed | no-op (guarded arm falls through) |
| `.find(false)`, rename open | rename closed (the subscription runs because `rename.is_some()`) |

Command landings:

| Sequence | `set_definition(t, Some(3..6))` |
|---|---|
| F12 → answer | selects `3..6`, reveals |
| F12 → click → answer | dropped |
| F12 → type → answer | dropped (revision) |
| F12 → answer `None` → answer `Some` | second dropped |

## 9. What NOT to change

- No scrive-lsp code; no LSP types in scrive-core.
- Do not change find behavior: the find bar's look (styles move, values stay), its chords, its
  debounce, replace, scope, or `find_chord`'s table (Escape still maps to `CloseFind`; the
  existing `find_chords_map_the_global_keys` test must pass unchanged).
- Do not rename existing public types or `Event` variants other than the planned
  `Focused { replace, on }` → `Focused { field, on }`.
- Do not make `enclosing_openers`, `Entry` or `request_reveal` public.
- Do not add `jump`, `try_edit` or any `lsp` feature (Phase 9).
- Do not re-export the new command requests from `code_editor`.
- Never run `cargo fmt`.

## 10. Known pitfalls

- **`Action` semver.** Three more variants on the public exhaustive `Action`; expected for 0.4.0.
  Add each to `moves_caret`'s excluded list and to `apply`'s no-op arm, or Ctrl+… autoscrolls /
  the match is non-exhaustive.
- **Arm order in `update`.** `Event::Editor(Action::Rename) if self.rename_enabled` must come
  before the unguarded `Event::Editor(Action::Rename)`, and all command arms before the catch-all
  `Event::Editor(action)`. `CloseFind if self.rename.is_some()` must come before
  `CloseFind if self.find_open`.
- **`Event::Focused`'s payload must be public.** `Field` appears in a public enum's variant, so a
  private `Field` is a hard error ("private type in public interface"). It lives in
  `scrive_iced::code_editor`, is not re-exported at the crate root, and hosts never build it.
- **The subscription closure is a non-capturing `fn`.** Keep the `find_enabled ||
  rename.is_some()` test *outside* the `listen_with` closure; do not capture `self`.
- **Lifetimes in `rename_bar`.** Written as a free fn taking `&Rename`, the returned
  `Element<'_, Event>` borrows the rename state; a `&self` method taking a separate `&Rename`
  would tie the output to `self` and reject `&rename.text` (E0621).
- **`Widget::diff` replaces the default**, which clears `tree.children`; the editor has none, so
  nothing else is needed. Do not call `Tree::diff` from inside it (infinite recursion).
- **`SignatureRequest::new` gains a parameter.** Phase 2 code and any test that builds one must
  pass `call`.
- **`#[non_exhaustive]` across crates.** `DefinitionRequest`, `RenameRequest`, `FormatRequest`
  and `HoverRequest` must be built with `new` in scrive-iced; tests read their public fields.
- **Doc links inside scrive-core** must not name `CodeEditor`; write editor methods as code
  spans. In editor.rs, `parse_md_runs` is private — refer to `HoverInfo::markdown` and
  `escape_markdown` in code spans, not intra-doc links.
- **`next_if` on `Peekable<Chars>`** hands the closure a `&char`; bind it by value
  (`|&n| matches!(n, '\\' | '*' | '`')`) — `matches!` on the reference against `char`
  patterns does not compile.
- **Auto-close in tests.** Typing `(` or `"` through `Action::Type` may insert a closing
  partner. Assert offsets found with `text.find(...)`, never hard-coded positions after typed
  brackets.
- **clippy:** `Field` and the new `Awaited::Definition` are plain enums with several variants; no
  single-variant lint applies. `bar_input_style`/`bar_panel_style` take unused `_theme`/`_status`
  parameters by the iced style-fn signature — the leading underscore keeps clippy quiet.
