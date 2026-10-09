//! `CodeEditor` — the batteries-included editor tier.
//!
//! [`Editor`] is a *controlled* widget: it borrows a
//! [`Document`] immutably to draw and emits semantic [`Action`]s the host
//! applies. That gives maximum control at the cost of a large amount of
//! plumbing — driving highlighting, find, focus, and the intel controllers is
//! all on the host. [`CodeEditor`] is the other end of that trade: it **owns** a
//! `Document`, runs the plumbing internally, and reduces integration to three
//! wires — [`update`](CodeEditor::update), [`view`](CodeEditor::view),
//! [`subscription`](CodeEditor::subscription) — plus registering
//! [`required_fonts`](crate::required_fonts) at startup.
//!
//! # Mechanism vs policy
//!
//! The split that keeps this honest: **mechanism** (the highlight pump, find
//! rescan, focus rings, autoscroll, the intel drive loops) is owned here, hidden,
//! and always-correct — a host cannot forget a step it never had to take.
//! **Policy** (grammar, theme, providers, sizing) is a builder override with a
//! sensible default. The one genuine fork is ownership: `CodeEditor` owns the
//! `Document` and exposes reads plus *blessed* mutations that run the post-edit
//! tail — there is deliberately no raw `&mut Document`, because that is the
//! trapdoor that lets a caller mutate behind the tail's back (the exact shape of
//! the cold-load highlight bug this tier exists to prevent). A host that must own
//! the buffer itself drops to the [`Editor`] power tier.
//!
//! This module is built up across milestones; M1 is the synchronous core loop
//! (owned document, the three wires, and the cold-load highlight fix). Find,
//! the intel drive loops, the large-document parallel sweep, and the async
//! intel round-trip land on top in later milestones.

use std::ops::Range;

use iced::advanced::widget;
use iced::alignment::{Horizontal, Vertical};
use iced::widget::operation::{focus, focus_next, focus_previous, is_focused};
use iced::widget::{button, column, container, row, stack, text, text_input};
use iced::time::{Duration, Instant};
use iced::{Alignment, Color, Element, Font, Length, Shadow, Subscription, Task, Theme, Vector};

use scrive_core::intel::completion::Start;
use scrive_core::intel::hover::escape_markdown;
use scrive_core::intel::inlay;
use scrive_core::intel::ticket;
use scrive_core::{
    default_indent_size, is_completion_word_char, Bias, BufferRow, CompletionController, CompletionCx, CompletionItem,
    CompletionState, CompletionTrigger, Completions, DefinitionRequest, Diagnostic, DiagnosticsOutcome,
    Document, EditOp, FindQuery, FormatRequest, Grammar, Hover,
    HoverCx, HoverInfo, InsertText, Point, RenameRequest, Revision, Selection, SelectionId, SelectionSet, Severity,
    SignatureCx, SignatureHelp, SignatureInfo, Snippet, SnippetSession, TabOutcome, Ticket,
    TokenTheme, TransactionError, LOOKBACK_LINES,
};

use crate::editor::{Action, Editor, Wake};
#[cfg(feature = "syntect")]
use crate::highlight_pool::HighlightPool;

#[cfg(feature = "lsp")]
mod lsp;
mod pool;

/// The async request types a host pulls from a [`CodeEditor`]. They live in
/// scrive-core so a language-service client can use them without iced.
pub use scrive_core::{CompletionRequest, HoverRequest, SignatureRequest};

/// The default widget id the editor is addressable by (focus / future
/// multi-pane). Overridable with [`CodeEditor::id`].
const DEFAULT_ID: &str = "scrive-editor";

/// Focusable ids for the find bar's inputs. iced's `focus` operation focuses one
/// and unfocuses every other focusable, so moving focus between an input and the
/// editor is a proper single-focus model.
const FIND_INPUT: &str = "scrive-find-input";
const REPLACE_INPUT: &str = "scrive-replace-input";
/// The rename field's input. It shares the find bar's focus plumbing.
const RENAME_INPUT: &str = "scrive-rename-input";

/// The floating bars' input box: width, row height (pinned so the find bar's
/// chevron can span its rows exactly), and the gap and margin around the
/// in-box buttons. Shared so the rename field matches the find bar.
const BAR_INPUT_W: f32 = 264.0;
const BAR_ROW_H: f32 = 26.0;
const BAR_BOX_GAP: f32 = 3.0;
const BAR_BOX_MARGIN: f32 = 3.0;

/// How long typing must pause before hints are fetched for the new text:
/// short enough that hints follow the text, long enough not to fetch on every
/// keystroke (Zed waits 700 ms, Helix 250 ms).
const INLAY_EDIT_DELAY: Duration = Duration::from_millis(300);
/// How long scrolling must pause before hints are fetched for the rows it
/// reached…
const INLAY_SCROLL_DELAY: Duration = Duration::from_millis(75);
/// …and the longest a continuous scroll (a scrollbar drag) waits, so a drag
/// fetches as it goes instead of cancelling each request before it answers.
const INLAY_SCROLL_CAP: Duration = Duration::from_millis(300);
/// The fewest rows a hint request covers.
const INLAY_MIN_ROWS: u32 = 50;

/// The opaque message a [`CodeEditor`] emits and consumes. The host never
/// matches on it — it only maps it through the three wires
/// (`self.editor.update(e, now).map(Message::Editor)` and the same for `view` /
/// `subscription`). It is deliberately opaque: the internal set churns with
/// every refactor, so host-relevant signals come through the curated read
/// accessors and builder callbacks instead. A host that genuinely needs the raw
/// semantic vocabulary drops to the [`Editor`] power tier, where
/// [`Action`] is the stable, match-on-me enum.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum Event {
    /// A semantic action published by the underlying widget.
    Editor(Action),
    /// One frame tick of the highlight sweep — the internal pump that drives
    /// tokenization to convergence (and fixes cold load). Emitted by
    /// [`subscription`](CodeEditor::subscription) only while a dirty highlight
    /// frontier remains, so an idle document does zero per-frame work.
    HighlightSweep,
    // ── find bar chrome (internal; the host only maps these through) ─────────
    /// Open the find bar (Ctrl+F), seeding the query from a single-line selection.
    OpenFind,
    /// Open the find bar with the replace row expanded (Ctrl+H).
    OpenReplace,
    /// Close the open bar (Escape): the rename field if it is open, else the
    /// find bar. Focus returns to the editor.
    CloseFind,
    /// The query text changed.
    FindQuery(String),
    /// Toggle the case-sensitive (`Aa`) option.
    ToggleCase,
    /// Toggle the whole-word (`ab|`) option.
    ToggleWholeWord,
    /// Toggle the regex (`.*`) option.
    ToggleRegex,
    /// Toggle find-in-selection: capture the current selection as the scope.
    ToggleFindInSelection,
    /// Advance to the next match.
    FindNext,
    /// Step to the previous match.
    FindPrev,
    /// Turn every match into a caret (Alt+Enter).
    FindSelectAll,
    /// Toggle the replace row (the chevron).
    ToggleReplace,
    /// The replacement text changed.
    ReplaceText(String),
    /// Replace the active match and advance.
    ReplaceOne,
    /// Replace every match in one undo step.
    ReplaceAll,
    /// Toggle the preserve-case (`AB`) replace option.
    TogglePreserveCase,
    /// The rename field's text changed.
    RenameText(String),
    /// Submit the rename field (Enter): record a rename request and close it.
    SubmitRename,
    /// Tab / Shift+Tab moved focus between the bar's inputs and the editor.
    CycleFocus {
        /// Whether focus moved backwards (Shift+Tab).
        back: bool,
    },
    /// A left button press landed somewhere — re-assert single focus.
    PointerDown,
    /// A bar input gained or lost native focus; mirror it into the ring flags.
    Focused {
        /// Which input.
        field: Field,
        /// Whether it is now focused.
        on: bool,
    },
}

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

/// A batteries-included code editor: owns a [`Document`], renders the
/// [`Editor`] widget, and runs the editing/highlighting plumbing
/// internally. See the [module docs](self) for the mechanism-vs-policy split and
/// the ownership fork.
pub struct CodeEditor {
    /// The owned document — the single source of truth. Read through
    /// [`document`](CodeEditor::document); mutated only through the blessed
    /// tail-running methods, never a raw `&mut`.
    doc: Document,
    /// The one viewport fact (last range the widget reported). It aims the
    /// tokenize target and the highlight retention window from a single owner so
    /// they cannot drift.
    viewport: Range<u32>,
    /// The syntax theme (default [`scrive_dark_theme`](crate::scrive_dark_theme)).
    /// Retained (cheaply cloneable) so [`load`](CodeEditor::load) and
    /// [`set_theme`](CodeEditor::set_theme) can re-apply it across a reload or
    /// grammar swap.
    theme: TokenTheme,
    /// Whether a grammar has been attached — marks whether there is a highlight
    /// cache to pump.
    has_syntax: bool,
    /// The editor's widget id (focus addressing).
    id: widget::Id,
    /// Rendering policy.
    font: Font,
    text_size: f32,
    /// The first instant `update` was handed; find's debounce clock counts
    /// milliseconds from it.
    epoch: Option<Instant>,
    /// Milliseconds from `epoch` to the latest instant `update` was handed (0
    /// before the first). Paths outside `update`, like [`edit`](Self::edit),
    /// reuse it.
    now_ms: u64,
    /// Set by every committed edit; a host reads it for a dirty indicator via
    /// [`is_dirty`](CodeEditor::is_dirty) / [`take_dirty`](CodeEditor::take_dirty).
    dirty: bool,
    /// Whether the built-in find bar (Ctrl+F / Ctrl+H) is available. Default on;
    /// [`find`](CodeEditor::find) disables it.
    find_enabled: bool,
    /// Whether the find bar is currently open, and its live query text.
    find_open: bool,
    find_query: String,
    /// The `Aa` / `ab|` / `.*` options. Each is part of the QUERY, not chrome:
    /// flipping one re-scans exactly like a text change.
    find_case: bool,
    find_whole_word: bool,
    find_regex: bool,
    /// Whether the bar is expanded into find+replace (the chevron), and the live
    /// replacement text.
    replace_open: bool,
    replace_text: String,
    /// Which field wears the focus ring (a container border can't read its
    /// field's focus, so these mirror it).
    find_focused: bool,
    replace_focused: bool,
    /// The `AB` replace option — whether a replacement is re-cased to the match.
    replace_preserve_case: bool,
    // ── language intelligence (view-state + injected providers) ─────────────
    /// The completion controller (popup state) and the host-supplied provider.
    /// `None` provider ⇒ no completions. Driven on edits; its popup passes to the
    /// widget each frame.
    completion: CompletionController,
    comp_provider: Option<Box<dyn Completions>>,
    /// The active snippet tab-stop session, if any.
    snippet: Option<SnippetSession>,
    /// The signature-help provider and the current one-line box.
    sig_provider: Option<Box<dyn SignatureHelp>>,
    signature: Option<SignatureInfo>,
    /// The hover provider and the open hover popup.
    hover_provider: Option<Box<dyn Hover>>,
    hover: Option<HoverInfo>,
    /// A pending async completion request (recorded, with its ticket in
    /// `awaiting`, when the drive loop would query completions but no
    /// synchronous provider is set), for the host to pull via
    /// [`take_completion_request`](CodeEditor::take_completion_request).
    pending_completion_request: Option<CompletionRequest>,
    /// A pending async signature-help request (same pattern as completions).
    pending_signature_request: Option<SignatureRequest>,
    /// A pending async hover request (same pattern), recorded when the pointer
    /// rested over a word and no synchronous hover provider is set.
    pending_hover_request: Option<HoverRequest>,
    /// A pending goto-definition request (F12), for the host to pull via
    /// [`take_definition_request`](CodeEditor::take_definition_request).
    pending_definition_request: Option<DefinitionRequest>,
    /// A pending format request (Shift+Alt+F), for the host to pull via
    /// [`take_format_request`](CodeEditor::take_format_request).
    pending_format_request: Option<FormatRequest>,
    /// A pending rename request (the rename field was submitted), for the host
    /// to pull via [`take_rename_request`](CodeEditor::take_rename_request).
    pending_rename_request: Option<RenameRequest>,
    /// A pending inlay hint fetch, for the host to pull via
    /// [`take_inlay_request`](CodeEditor::take_inlay_request).
    pending_inlay_request: Option<inlay::Request>,
    /// A pending gesture on a hint (tooltip, jump or insert), for the host to
    /// pull via [`take_inlay_interaction`](CodeEditor::take_inlay_interaction).
    /// One slot: a newer gesture replaces it.
    pending_inlay_interaction: Option<inlay::Interaction>,
    /// The inlay hint scheduler.
    inlays: Inlays,
    /// The open inlay tooltip card. At most one card shows: this or `hover`.
    inlay_card: Option<InlayCard>,
    /// Whether F2 opens the rename field. Off by default, because a host with
    /// no rename provider would show a field that does nothing.
    rename_enabled: bool,
    /// The open rename field. Only one floating bar is open at a time.
    rename: Option<Rename>,
    /// Mints the ticket every async request carries. Per editor, so two
    /// requests at one revision still differ.
    tickets: ticket::Counter,
    /// The ticket each async service's reply must carry to land.
    awaiting: Awaiting,
    /// The caret the popup's current items were produced against. Accepting an
    /// item shifts its additional edits by how far the caret has moved since.
    items_caret: u32,
    /// The off-thread parallel highlight sweep — `Some` for a large document
    /// (see [`uses_pool`](Self::uses_pool)), `None` otherwise (the synchronous path). Owned
    /// here so a batteries-included host gets large-document highlighting for free.
    #[cfg(feature = "syntect")]
    hl_pool: Option<HighlightPool>,
    /// The language-server client the document is registered with. At most one, because the
    /// change log and the request slots each have a single consumer.
    #[cfg(feature = "lsp")]
    lsp_client: Option<scrive_lsp::client::Id>,
}

/// What an applied edit means for the completion controller — computed from the
/// [`Action`] before it is consumed, then threaded through the post-edit tail.
#[derive(Clone, Copy)]
enum CompletionEvent {
    /// A character was typed (opens/filters the popup, or is a trigger char).
    Typed(char),
    /// A deletion (refilters an open popup; closes at the word start).
    Deleting,
    /// Anything else — a caret move, paste, undo… — closes the popup.
    CaretOrClose,
}

/// The ticket each async service's reply must carry. A slot is set when a
/// request is recorded and cleared when the user abandons it;
/// [`CodeEditor::accepts`] reads it.
#[derive(Default)]
struct Awaiting {
    completion: Option<Ticket>,
    signature: Option<Ticket>,
    /// The hover ticket, plus the pointer offset and word it asked about: the
    /// word keeps a pointer move inside it from cancelling the request.
    hover: Option<(Ticket, u32, Range<u32>)>,
    /// The goto-definition ticket; a landed range is selected and revealed.
    definition: Option<Ticket>,
    /// The inlay fetch's ticket.
    inlays: Option<Ticket>,
    /// The inlay tooltip's ticket, and the hint and label part it describes.
    inlay_tooltip: Option<(Ticket, inlay::Key, u32)>,
    /// The inlay insert's ticket, and the hint and the offset it renders at.
    inlay_insert: Option<(Ticket, inlay::Key, u32)>,
}

/// Inlay hint fetching: whether hints are on, the wait a fetch is pending on,
/// and the rows the viewport may move within before the last fetch's window
/// needs replacing.
struct Inlays {
    enabled: bool,
    /// Bumped by every trigger, so the widget restarts its delay.
    generation: u64,
    /// The pending fetch: a delay and cap, never a deadline, because triggers
    /// arrive outside `update`, where the clock is stale.
    wait: Option<Wake>,
    /// The inner half of the last requested window, in buffer rows.
    window: Option<Range<u32>>,
    /// The last revision the scheduler saw; a different one is an edit.
    seen: Revision,
}

/// The inlay tooltip card: the hint and label part it describes, and its
/// markdown.
struct InlayCard {
    key: inlay::Key,
    part: u32,
    markdown: String,
}

/// The open rename field: the name being typed, the caret offset and revision
/// it opened at (the rename names the symbol there, so an edit underneath
/// closes it), and whether its input wears the focus ring.
struct Rename {
    text: String,
    offset: u32,
    revision: Revision,
    focused: bool,
}

/// Which awaited slot an `accepts` / `abandon` call addresses.
#[derive(Clone, Copy)]
enum Awaited {
    Completion,
    Signature,
    Hover,
    Definition,
    Inlays,
    InlayTooltip,
    InlayInsert,
}

impl CodeEditor {
    /// A new editor over `source`, with the default theme
    /// ([`scrive_dark_theme`](crate::scrive_dark_theme)) but **no grammar** —
    /// plain text until [`language`](CodeEditor::language) attaches one.
    ///
    /// # Panics
    /// Panics if `source` does not fit scrive's `u32` offset space (~4 GiB) —
    /// far past any editable document.
    #[must_use]
    pub fn new(source: impl Into<String>) -> Self {
        let source = source.into();
        let doc = Document::new(&source).expect("source fits the u32 offset space");
        let revision = doc.revision();
        Self {
            doc,
            viewport: 0..0,
            theme: crate::scrive_dark_theme(),
            has_syntax: false,
            id: widget::Id::new(DEFAULT_ID),
            font: crate::DEFAULT_FONT,
            text_size: 14.0,
            epoch: None,
            now_ms: 0,
            dirty: false,
            find_enabled: true,
            find_open: false,
            find_query: String::new(),
            find_case: false,
            find_whole_word: false,
            find_regex: false,
            replace_open: false,
            replace_text: String::new(),
            find_focused: false,
            replace_focused: false,
            replace_preserve_case: false,
            completion: CompletionController::new(),
            comp_provider: None,
            snippet: None,
            sig_provider: None,
            signature: None,
            hover_provider: None,
            hover: None,
            pending_completion_request: None,
            pending_signature_request: None,
            pending_hover_request: None,
            pending_definition_request: None,
            pending_format_request: None,
            pending_rename_request: None,
            pending_inlay_request: None,
            pending_inlay_interaction: None,
            inlays: Inlays { enabled: false, generation: 0, wait: None, window: None, seen: revision },
            inlay_card: None,
            rename_enabled: false,
            rename: None,
            tickets: ticket::Counter::new(),
            awaiting: Awaiting::default(),
            items_caret: 0,
            #[cfg(feature = "syntect")]
            hl_pool: None,
            #[cfg(feature = "lsp")]
            lsp_client: None,
        }
    }

    // ── builder (policy; every knob defaulted) ──────────────────────────────

    /// Attach a grammar, enabling syntax highlighting: a `scrive_core::SyntaxDef`
    /// under the `syntect` feature (on by default), or a
    /// `scrive_core::TreeSitterDef` under the `tree-sitter` feature. Uses the theme set by
    /// [`theme`](CodeEditor::theme) if one was staged, else the bundled default.
    /// Order-independent with `theme`. Seeds the visible rows' colors (the
    /// whole document before any viewport report) immediately, so the first
    /// paint is highlighted — the cold-load fix. A tree-sitter parse too long
    /// for one call's budget is the exception: it finishes over the next
    /// frames, through [`subscription`](CodeEditor::subscription).
    #[must_use]
    pub fn language(mut self, grammar: impl Into<Grammar>) -> Self {
        self.doc.set_syntax(grammar, self.theme.clone());
        self.has_syntax = true;
        self.seed_highlight();
        self
    }

    /// Override the syntax theme (default:
    /// [`scrive_dark_theme`](crate::scrive_dark_theme)). Order-independent with
    /// [`language`](CodeEditor::language): before a grammar is attached it is
    /// staged; after, it re-themes the live cache.
    #[must_use]
    pub fn theme(mut self, theme: TokenTheme) -> Self {
        if self.has_syntax {
            self.doc.set_theme(theme.clone());
        }
        self.theme = theme;
        self
    }

    /// Set the (monospace) font. Default [`DEFAULT_FONT`](crate::DEFAULT_FONT).
    #[must_use]
    pub fn font(mut self, font: Font) -> Self {
        self.font = font;
        self
    }

    /// Set the font size in logical pixels. Default `14.0`.
    #[must_use]
    pub fn text_size(mut self, px: f32) -> Self {
        self.text_size = px;
        self
    }

    /// Give the editor a widget id for focus addressing (default
    /// `"scrive-editor"`). Needed only if a multi-pane host targets focus at it.
    #[must_use]
    pub fn id(mut self, id: impl Into<widget::Id>) -> Self {
        self.id = id.into();
        self
    }

    /// Enable or disable the built-in find bar and its Ctrl+F / Ctrl+H bindings.
    /// Default on.
    #[must_use]
    pub fn find(mut self, enabled: bool) -> Self {
        self.find_enabled = enabled;
        self
    }

    /// Enable or disable the rename field that F2 opens. Default off: turn it on
    /// when the host answers [`take_rename_request`](CodeEditor::take_rename_request).
    #[must_use]
    pub fn rename(mut self, enabled: bool) -> Self {
        self.rename_enabled = enabled;
        self
    }

    /// Show inlay hints, the server's inline labels such as `: i32`. Default
    /// off: turn it on when the host answers
    /// [`take_inlay_request`](CodeEditor::take_inlay_request).
    #[must_use]
    pub fn inlay_hints(mut self, enabled: bool) -> Self {
        self.set_inlay_hints(enabled);
        self
    }

    /// Set the line-comment marker used by toggle-comment (e.g. `Some("//")`);
    /// `None` (the default) disables toggle-comment. It also drives comment-aware
    /// bracket matching, so brackets inside `// …` stop being matched / coloured /
    /// folded. Language config, like the grammar — the core ships none.
    #[must_use]
    pub fn line_comment(mut self, marker: Option<&str>) -> Self {
        self.doc.set_line_comment(marker);
        self
    }

    /// Configure comment/string-aware bracket matching's string and char-literal
    /// delimiters — brackets inside a string (e.g. `"{}"`) or char literal are not
    /// matched, coloured, folded, or indent-guided. (Line comments come from
    /// [`line_comment`](CodeEditor::line_comment).) `char_delim` is off by default:
    /// in some languages `'` is ambiguous (Rust lifetimes), so it is opt-in.
    #[must_use]
    pub fn bracket_lexing(mut self, string_delims: Vec<u8>, char_delim: Option<u8>) -> Self {
        self.doc.set_bracket_lexing(string_delims, char_delim);
        self
    }

    /// Supply a completion provider (default: none). The editor drives it on
    /// edits, filters and renders the popup, and applies an accepted item —
    /// including expanding a snippet insert into an interactive tab-stop session.
    #[must_use]
    pub fn completions(mut self, provider: impl Completions + 'static) -> Self {
        self.comp_provider = Some(Box::new(provider));
        self
    }

    /// Supply a hover provider (default: none). The editor queries it on the
    /// idle-hover action and renders the returned card above the word.
    #[must_use]
    pub fn hover(mut self, provider: impl Hover + 'static) -> Self {
        self.hover_provider = Some(Box::new(provider));
        self
    }

    /// Supply a signature-help provider (default: none). `(` opens the box;
    /// while open, edits/moves re-query and a `None` reply closes it.
    #[must_use]
    pub fn signature(mut self, provider: impl SignatureHelp + 'static) -> Self {
        self.sig_provider = Some(Box::new(provider));
        self
    }

    // ── reads (safe; no mutation) ───────────────────────────────────────────

    /// The owned document, for saving / diffing / inspection
    /// (`document().serialize(..)`, `.snapshot()`, `.revision()`). Read-only:
    /// mutations go through the blessed methods so the post-edit tail always runs.
    #[must_use]
    pub fn document(&self) -> &Document {
        &self.doc
    }

    /// Whether an edit has landed since the last [`take_dirty`](CodeEditor::take_dirty).
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Read and clear the dirty flag — for a host that marks a title bar or
    /// schedules a save on change.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// The primary caret position as (row, col).
    #[must_use]
    pub fn cursor(&self) -> Point {
        let head = self.doc.selections().newest().head();
        self.doc.buffer().offset_to_point(head)
    }

    /// The primary selection's byte range (empty at a bare caret).
    #[must_use]
    pub fn selection(&self) -> Range<u32> {
        let s = self.doc.selections().newest();
        s.start()..s.end()
    }

    // ── blessed mutations (run the post-edit tail) ──────────────────────────

    /// Apply a programmatic batch of edits as one transaction, then run the
    /// post-edit tail. The tail is why this exists instead of a raw `&mut
    /// Document`: it keeps highlighting, find, and the intel controllers current.
    /// A rejected batch is dropped silently; [`try_edit`](Self::try_edit) says why.
    pub fn edit(&mut self, ops: Vec<EditOp>) {
        let _ = self.try_edit(ops);
    }

    /// [`edit`](Self::edit), with a rejected batch returned instead of dropped.
    /// A transaction is all-or-nothing, so nothing is applied on `Err`.
    ///
    /// # Errors
    /// [`TransactionError::Overlap`] when two ops overlap in the pre-edit text,
    /// and [`TransactionError::WouldOverflow`] when the result would grow past
    /// the `u32` offset space.
    pub fn try_edit(&mut self, ops: Vec<EditOp>) -> Result<(), TransactionError> {
        let before = self.doc.revision();
        self.doc.edit(ops)?;
        self.after_edit(CompletionEvent::CaretOrClose);
        if self.doc.revision() != before {
            self.dirty = true;
        }
        Ok(())
    }

    /// Swap the whole buffer (load a new file), keeping the current grammar and
    /// theme unless `grammar` supplies a new language. The blessed whole-buffer
    /// swap: it runs as one transaction and re-tokenizes the visible document, so
    /// highlighting is correct immediately — never mutate the buffer behind the
    /// tail's back. (The swap is a normal transaction, so it is undoable.) As
    /// with [`language`](CodeEditor::language), a tree-sitter parse too long for
    /// one call's budget finishes over the next frames.
    pub fn load(&mut self, source: impl Into<String>, grammar: Option<Grammar>) {
        let source = source.into();
        // Replace the whole buffer as one transaction; the grammar, theme, and
        // highlight cache stay attached and re-tokenize via `on_commit`.
        let len = self.doc.buffer().len();
        let _ = self.doc.edit(vec![EditOp::new(0..len, &source)]);
        // The edit would keep the old hints at stale offsets in the new text.
        self.doc.clear_inlays();
        self.inlay_card = None;
        self.abandon(Awaited::Inlays);
        self.abandon(Awaited::InlayTooltip);
        self.abandon(Awaited::InlayInsert);
        self.inlays.seen = self.doc.revision();
        self.inlays.window = None;
        self.wait_inlays(Duration::ZERO, None);
        if let Some(g) = grammar {
            self.doc.set_syntax(g, self.theme.clone());
            self.has_syntax = true;
        }
        // Re-seed: the document may have grown/shrunk, a grammar swap left the
        // cache all-dirty, and a new grammar needs a fresh pool engine (fixing the
        // grammar-swap-strands-the-pool bug).
        self.seed_highlight();
        self.dirty = true;
    }

    /// Collapse every foldable region (the "Fold All" command). Folds are view
    /// state, so this records nothing on the undo stack and runs no post-edit tail.
    pub fn fold_all(&mut self) {
        for (open, ..) in self.doc.collapsible_pairs() {
            self.doc.toggle_fold_opener(open);
        }
    }

    /// Swap the syntax theme at runtime. The retained theme updates too, so a
    /// later [`load`](CodeEditor::load) keeps it.
    pub fn set_theme(&mut self, theme: TokenTheme) {
        self.doc.set_theme(theme.clone());
        self.theme = theme;
        self.doc.tokenize_highlight(self.viewport.end);
    }

    /// Publish a diagnostic set from the host's (debounced, off-thread) compile
    /// or language-server pass. Stamped by `revision`: a set computed against a
    /// snapshot the buffer has moved past is dropped (the previous squiggles keep
    /// riding edits). This is the ingest half of recompile-on-edit — pair it with
    /// an edit signal ([`take_dirty`](CodeEditor::take_dirty) or a revision
    /// compare) and [`document`](CodeEditor::document)`().snapshot()`.
    pub fn set_diagnostics(
        &mut self,
        revision: Revision,
        diags: Vec<Diagnostic>,
    ) -> DiagnosticsOutcome {
        self.doc.set_diagnostics(revision, diags)
    }

    /// Take the pending async completion request, if any. A host with an
    /// off-thread / language-server provider (rather than a synchronous
    /// [`completions`](CodeEditor::completions) one) polls this after `update`,
    /// runs its query, and returns the result through
    /// [`set_completions`](CodeEditor::set_completions) with the request's
    /// ticket.
    pub fn take_completion_request(&mut self) -> Option<CompletionRequest> {
        self.pending_completion_request.take()
    }

    /// Ingest completion items from an async source, stamped with the
    /// request's `ticket`. The items land only if the editor still awaits that
    /// ticket and the document has not moved since; otherwise they are dropped
    /// (a newer request, or none, is in charge). Landed items open or refilter
    /// the popup against the *live* word, and snippet-format items
    /// ([`InsertText::Snippet`](scrive_core::InsertText)) expand into a tab-stop
    /// session on accept exactly like a synchronous provider's. An empty list
    /// closes the popup; an Escape dismissal keeps it closed.
    pub fn set_completions(&mut self, ticket: Ticket, items: Vec<CompletionItem>) {
        if !self.accepts(Awaited::Completion, ticket) {
            return;
        }
        self.items_caret = self.doc.selections().newest().head();
        let word = self.completion_word_text();
        let anchor = self.completion_word().start;
        self.completion.set_items(items, &word, anchor);
    }

    /// Take the pending async signature-help request, if any — the signature
    /// twin of [`take_completion_request`](CodeEditor::take_completion_request).
    /// Answer it through [`set_signature`](CodeEditor::set_signature) with the
    /// request's ticket.
    pub fn take_signature_request(&mut self) -> Option<SignatureRequest> {
        self.pending_signature_request.take()
    }

    /// Ingest a signature-help result from an async source, stamped with the
    /// request's `ticket` (see [`set_completions`](Self::set_completions) for
    /// when it lands). `None` closes the box and stops re-querying.
    pub fn set_signature(&mut self, ticket: Ticket, info: Option<SignatureInfo>) {
        if !self.accepts(Awaited::Signature, ticket) {
            return;
        }
        if info.is_none() {
            self.abandon(Awaited::Signature);
        }
        self.signature = info;
    }

    /// Take the pending async hover request, if any — the hover twin of
    /// [`take_completion_request`](CodeEditor::take_completion_request).
    /// Answer it through [`set_hover`](CodeEditor::set_hover) with the
    /// request's ticket.
    pub fn take_hover_request(&mut self) -> Option<HoverRequest> {
        self.pending_hover_request.take()
    }

    /// Ingest hover docs from an async source, stamped with the request's
    /// `ticket` (see [`set_completions`](Self::set_completions) for when it
    /// lands). The card shows the diagnostics under the hovered offset first,
    /// then these docs; `None` leaves just the diagnostics, or no card.
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

    /// Take the pending goto-definition request, if any. Answer a target in
    /// this document through [`set_definition`](Self::set_definition).
    pub fn take_definition_request(&mut self) -> Option<DefinitionRequest> {
        self.pending_definition_request.take()
    }

    /// Land a goto-definition answer stamped with the request's `ticket`. A
    /// range in this document is selected and revealed; `None` (no definition,
    /// or one the host opens elsewhere) retires the request. Dropped if the
    /// caret moved or the text changed since the request.
    pub fn set_definition(&mut self, ticket: Ticket, target: Option<Range<u32>>) {
        if !self.accepts(Awaited::Definition, ticket) {
            return;
        }
        self.abandon(Awaited::Definition);
        if let Some(range) = target {
            self.select(range);
        }
    }

    /// Take the pending rename request, if any. Its answer is an edit batch
    /// for [`edit`](Self::edit), valid while the document is still at the
    /// ticket's revision.
    pub fn take_rename_request(&mut self) -> Option<RenameRequest> {
        self.pending_rename_request.take()
    }

    /// Take the pending format request, if any. Its answer is an edit batch
    /// for [`edit`](Self::edit), valid while the document is still at the
    /// ticket's revision.
    pub fn take_format_request(&mut self) -> Option<FormatRequest> {
        self.pending_format_request.take()
    }

    /// Select `range` and reveal it centered, unfolding whatever hides it: the
    /// programmatic jump for host navigation such as goto-definition. The range
    /// is clamped to the document and snapped to char boundaries. Like a caret
    /// move, it closes the completion popup and retires requests tied to the old
    /// caret.
    pub fn select(&mut self, range: Range<u32>) {
        self.doc.select_and_reveal(range);
        self.after_edit(CompletionEvent::CaretOrClose);
    }

    /// Turn inlay hints on or off at runtime. On asks for the visible rows'
    /// hints at once. Off removes the hints, closes their tooltip and forgets
    /// every hint request in flight.
    pub fn set_inlay_hints(&mut self, enabled: bool) {
        if enabled == self.inlays.enabled {
            return;
        }
        self.inlays.enabled = enabled;
        if enabled {
            self.wait_inlays(Duration::ZERO, None);
            return;
        }
        self.doc.clear_inlays();
        self.inlay_card = None;
        self.abandon(Awaited::Inlays);
        self.abandon(Awaited::InlayTooltip);
        self.abandon(Awaited::InlayInsert);
        self.pending_inlay_interaction = None;
        self.inlays.wait = None;
        self.inlays.window = None;
    }

    /// Take the pending inlay hint fetch, if any: a byte span to fetch hints
    /// for. Answer it through [`set_inlays`](Self::set_inlays) with the
    /// request's ticket.
    pub fn take_inlay_request(&mut self) -> Option<inlay::Request> {
        self.pending_inlay_request.take()
    }

    /// Land an inlay hint fetch stamped with the request's `ticket`: `Some`
    /// replaces the shown hints (an empty list clears them), and `None`, a
    /// failed fetch, keeps them. Dropped unless the editor still awaits
    /// `ticket` and the text has not changed since.
    pub fn set_inlays(&mut self, ticket: Ticket, hints: Option<Vec<inlay::Placed>>) {
        if !self.accepts(Awaited::Inlays, ticket) {
            return;
        }
        self.abandon(Awaited::Inlays);
        let Some(hints) = hints else { return };
        // A refetch keeps an unchanged hint's key, so its open card stays.
        if self.inlay_card.as_ref().is_some_and(|card| !hints.iter().any(|p| p.hint().key() == card.key)) {
            self.inlay_card = None;
        }
        let outcome = self.doc.set_inlays(ticket.revision(), hints);
        debug_assert!(matches!(outcome, inlay::Outcome::Applied { .. }), "`accepts` checked the revision");
    }

    /// The wait a hint fetch is pending on, if any: what [`view`](Self::view)
    /// hands the widget through [`Editor::wake_after`]. A host that runs its
    /// own timer, or a test, sends [`Action::Wake`] with its generation once
    /// the delay has passed.
    #[must_use]
    pub fn pending_wake(&self) -> Option<Wake> {
        self.inlays.wait
    }

    /// Take the pending gesture on a hint, if any: a tooltip to show, a label
    /// to jump through, or a hint to insert. Answer a tooltip through
    /// [`set_inlay_tooltip`](Self::set_inlay_tooltip), a jump as a definition,
    /// and an insert as an edit batch.
    pub fn take_inlay_interaction(&mut self) -> Option<inlay::Interaction> {
        self.pending_inlay_interaction.take()
    }

    /// Land a hint tooltip stamped with the gesture's `ticket`: `Some` shows
    /// the markdown as a card on the hovered label part, and `None` shows
    /// nothing. Dropped once the pointer has left the part or the text has
    /// changed.
    pub fn set_inlay_tooltip(&mut self, ticket: Ticket, markdown: Option<String>) {
        if !self.accepts(Awaited::InlayTooltip, ticket) {
            return;
        }
        let Some((_, key, part)) = self.awaiting.inlay_tooltip else { return };
        self.abandon(Awaited::InlayTooltip);
        self.inlay_card = markdown.map(|markdown| InlayCard { key, part, markdown });
        if self.inlay_card.is_some() {
            self.hover = None;
        }
    }

    /// Enable or disable the per-commit change log, for a host mirroring
    /// edits to a language server (`textDocument/didChange`). Off by default
    /// (zero overhead); turning it on starts a fresh chain at the current
    /// revision. Forwards to [`Document::observe_changes`]. Full-document sync
    /// hosts leave this off and re-read `document().snapshot()` instead.
    pub fn observe_changes(&mut self, on: bool) {
        self.doc.observe_changes(on);
    }

    /// Drain the change log: one entry per commit since the last drain (edits,
    /// undos and redos alike), each with the snapshot it applied to, ready to
    /// translate into LSP content changes. Empty unless
    /// [`observe_changes`](CodeEditor::observe_changes) is on. Forwards to
    /// [`Document::drain_changes`].
    #[must_use]
    pub fn drain_changes(&mut self) -> scrive_core::document::Changes {
        self.doc.drain_changes()
    }

    // ── the three wires ─────────────────────────────────────────────────────

    /// Fold one [`Event`] into the editor. `now` is when the event happened;
    /// pass the instant [`iced::application::timed()`] gives your `update`. Map
    /// the result back to your message type:
    /// `Message::Editor(e) => self.editor.update(e, now).map(Message::Editor)`.
    pub fn update(&mut self, event: Event, now: Instant) -> Task<Event> {
        let epoch = *self.epoch.get_or_insert(now);
        self.now_ms = now.saturating_duration_since(epoch).as_millis() as u64;
        match event {
            // The widget reported a new visible range (scroll / resize /
            // autoscroll). Aim the retention window there and tokenize down to
            // it. Not an edit — no history, no find rescan — so it bypasses
            // `apply`.
            Event::Editor(Action::ViewportChanged(rows)) => {
                let left_window = self.inlays.window.as_ref().is_some_and(|w| rows.start < w.start || rows.end > w.end);
                self.viewport = rows.clone();
                self.doc.set_highlight_window(rows.clone());
                if !self.pool_viewport(rows.clone()) {
                    self.doc.tokenize_highlight(rows.end);
                }
                self.hover = None; // scroll closes the hover…
                self.abandon(Awaited::Hover); // …and retires a pending one
                self.inlay_card = None;
                self.abandon(Awaited::InlayTooltip);
                if left_window {
                    self.wait_inlays(INLAY_SCROLL_DELAY, Some(INLAY_SCROLL_CAP));
                }
                Task::none()
            }
            // Escape with the bar open closes the BAR and keeps the selections
            // (matching mainstream editors) — this covers the editor-focused
            // press; the input-focused one arrives as `CloseFind` via the chord.
            Event::Editor(Action::Collapse) if self.find_open => {
                self.close_find();
                Task::none()
            }
            // Escape with no popup or box showing (the widget sends those as
            // PopupDismiss / SignatureClose) still retires an in-flight
            // signature request, before the post-edit tail would re-query it.
            Event::Editor(Action::Collapse) => {
                self.signature = None;
                self.abandon(Awaited::Signature);
                self.apply(Action::Collapse);
                Task::none()
            }
            // Folds are view state (no text change, no undo step, no rehighlight);
            // handled here rather than through the edit tail.
            Event::Editor(Action::ToggleFold { opener }) => {
                self.doc.toggle_fold_opener(opener);
                Task::none()
            }
            Event::Editor(Action::FoldAtCarets { unfold }) => {
                self.doc.fold_at_carets(unfold);
                Task::none()
            }
            // Completion popup navigation (captured by the widget while open) —
            // drive the controller; no document edit except on accept.
            Event::Editor(Action::PopupUp) => {
                self.completion.move_selection(false);
                Task::none()
            }
            Event::Editor(Action::PopupDown) => {
                self.completion.move_selection(true);
                Task::none()
            }
            Event::Editor(Action::PopupDismiss) => {
                self.completion.escape();
                self.abandon(Awaited::Completion);
                Task::none()
            }
            Event::Editor(Action::PopupAccept) => {
                self.accept_completion();
                Task::none()
            }
            Event::Editor(Action::PopupClickAccept(idx)) => {
                self.completion.set_selected(idx);
                self.accept_completion();
                Task::none()
            }
            // Handled here rather than in `apply`, whose post-edit tail would
            // treat it as a caret move and abandon the request it just made.
            Event::Editor(Action::TriggerCompletion) => {
                self.request_completions(CompletionTrigger::Manual);
                Task::none()
            }
            // Snippet tab-stop navigation (captured while a session is active).
            Event::Editor(Action::SnippetTab) => {
                self.snippet_tab(true);
                Task::none()
            }
            Event::Editor(Action::SnippetTabPrev) => {
                self.snippet_tab(false);
                Task::none()
            }
            Event::Editor(Action::SnippetCancel) => {
                if let Some(mut s) = self.snippet.take() {
                    s.cancel(self.doc.decorations_mut());
                }
                Task::none()
            }
            Event::Editor(Action::SignatureClose) => {
                self.signature = None;
                self.abandon(Awaited::Signature);
                Task::none()
            }
            Event::Editor(Action::HoverQuery(offset)) => {
                self.inlay_card = None;
                self.abandon(Awaited::InlayTooltip);
                let cx = self.build_hover_cx(offset);
                let has_word = cx.word.start != cx.word.end;
                let docs = has_word
                    .then(|| self.hover_provider.as_mut().and_then(|p| p.hover(&cx)))
                    .flatten();
                self.hover = self.hover_card(offset, docs);
                // With no synchronous provider, record an async request so the
                // host can supply docs; the diagnostics show meanwhile and the
                // docs join them when they land.
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
            Event::Editor(Action::HoverDismiss) => {
                self.hover = None;
                self.abandon(Awaited::Hover);
                self.inlay_card = None;
                self.abandon(Awaited::InlayTooltip);
                Task::none()
            }
            Event::Editor(Action::GotoDefinition) => {
                let head = self.doc.selections().newest().head();
                let ticket = self.tickets.issue(self.doc.revision());
                self.awaiting.definition = Some(ticket);
                self.pending_definition_request = Some(DefinitionRequest::new(ticket, head));
                Task::none()
            }
            // One bar at a time, so an open find bar closes.
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
            // Kept out of `apply`, whose tail would treat it as a caret move and
            // close the popup.
            Event::Editor(Action::Rename) => Task::none(),
            Event::Editor(Action::Format) => {
                let ticket = self.tickets.issue(self.doc.revision());
                self.pending_format_request = Some(FormatRequest::new(ticket, default_indent_size()));
                Task::none()
            }
            // The pending fetch's wait is over. Kept out of `apply`, whose tail
            // would close the completion popup after every typing pause.
            Event::Editor(Action::Wake(generation)) => {
                if self.inlays.wait.is_some_and(|w| w.generation == generation) {
                    self.inlays.wait = None;
                    let (rows, inner) = self.inlay_window();
                    let buffer = self.doc.buffer();
                    let start = buffer.point_to_offset(Point::new(rows.start, 0));
                    let end = if rows.end >= buffer.line_count() {
                        buffer.len()
                    } else {
                        buffer.point_to_offset(Point::new(rows.end, 0))
                    };
                    let ticket = self.tickets.issue(self.doc.revision());
                    self.awaiting.inlays = Some(ticket);
                    self.pending_inlay_request = Some(inlay::Request::new(ticket, start..end));
                    self.inlays.window = Some(inner);
                }
                Task::none()
            }
            // Gestures on hints: kept out of `apply`, whose tail would retire
            // the slots they fill.
            Event::Editor(Action::InlayHover { key, part }) => {
                self.hover = None;
                self.abandon(Awaited::Hover);
                self.inlay_card = None;
                self.abandon(Awaited::InlayTooltip);
                if self.inlays.enabled && self.inlays_current() {
                    let ticket = self.tickets.issue(self.doc.revision());
                    self.awaiting.inlay_tooltip = Some((ticket, key, part));
                    self.pending_inlay_interaction = Some(inlay::Interaction::tooltip(ticket, key, part));
                }
                Task::none()
            }
            Event::Editor(Action::InlayJump { key, part }) => {
                if self.inlays.enabled && self.inlays_current() {
                    let ticket = self.tickets.issue(self.doc.revision());
                    // The jump lands as a definition, so every target kind works.
                    self.awaiting.definition = Some(ticket);
                    self.pending_definition_request = None;
                    self.pending_inlay_interaction = Some(inlay::Interaction::jump(ticket, key, part));
                }
                Task::none()
            }
            Event::Editor(Action::InlayInsert { key, offset }) => {
                if self.inlays.enabled && self.inlays_current() {
                    let ticket = self.tickets.issue(self.doc.revision());
                    self.awaiting.inlay_insert = Some((ticket, key, offset));
                    self.pending_inlay_interaction = Some(inlay::Interaction::insert(ticket, key, offset));
                }
                Task::none()
            }
            Event::Editor(action) => {
                self.apply(action);
                Task::none()
            }
            // The idle sweep: one budgeted batch per frame toward convergence.
            // The subscription drops itself once the frontier is clean, so this
            // stops firing on an idle document.
            Event::HighlightSweep => {
                // A small document, or a grammar with no pool engine
                // (tree-sitter), drives the synchronous path.
                if !self.pool_sweep() {
                    let n = self.doc.buffer().line_count();
                    self.doc.tokenize_highlight(n);
                }
                Task::none()
            }

            // ── find bar ────────────────────────────────────────────────────
            Event::OpenFind if self.find_enabled => {
                self.rename = None;
                self.find_open = true;
                // Seed (or re-seed) the query from a non-empty, single-line
                // selection, as mainstream editors do; an empty selection leaves
                // the current query untouched.
                let sel = self.doc.selections().newest();
                let seed = (!sel.is_empty())
                    .then(|| self.doc.buffer().slice(sel.start()..sel.end()).into_owned())
                    .filter(|t| !t.contains('\n'));
                if let Some(text) = seed {
                    self.find_query = text;
                    self.push_find_query();
                }
                self.find_focused = true;
                self.replace_focused = false;
                focus(FIND_INPUT)
            }
            Event::OpenReplace if self.find_enabled => {
                // Ctrl+H is Ctrl+F with the replace row already out.
                self.replace_open = true;
                self.update(Event::OpenFind, now)
            }
            Event::CycleFocus { back } => {
                let moved = if back { focus_previous() } else { focus_next() };
                moved.chain(Self::sync_rings())
            }
            Event::PointerDown if self.find_open || self.rename.is_some() => {
                Task::batch([Self::resync_focus(), Self::sync_rings()])
            }
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
            // Escape arrives here through the global bar chord, whatever holds
            // focus. At most one bar is open.
            Event::CloseFind if self.rename.is_some() => {
                self.rename = None;
                focus(self.id.clone())
            }
            Event::CloseFind if self.find_open => {
                self.close_find();
                focus(self.id.clone())
            }
            Event::FindQuery(q) => {
                self.find_query = q;
                self.push_find_query();
                Task::none()
            }
            Event::ToggleCase => {
                self.find_case = !self.find_case;
                self.push_find_query();
                Task::none()
            }
            Event::ToggleWholeWord => {
                self.find_whole_word = !self.find_whole_word;
                self.push_find_query();
                Task::none()
            }
            Event::ToggleRegex => {
                self.find_regex = !self.find_regex;
                self.push_find_query();
                Task::none()
            }
            Event::ToggleFindInSelection if self.find_open => {
                // The scope lives in the document (it rides every edit), so read
                // the live one and set its opposite.
                let scope = if self.doc.find_scope().is_some() {
                    None
                } else {
                    let sel = self.doc.selections().newest();
                    (!sel.is_empty()).then(|| sel.start()..sel.end())
                };
                let now = self.now_ms;
                self.doc.set_find_scope(scope, now);
                Task::none()
            }
            Event::FindNext if self.find_open => {
                let now = self.now_ms;
                self.doc.find_next(now);
                Task::none()
            }
            Event::FindPrev if self.find_open => {
                let now = self.now_ms;
                self.doc.find_prev(now);
                Task::none()
            }
            Event::FindSelectAll if self.find_open => {
                // Every match becomes a caret; focus returns to the editor.
                if self.doc.select_find_matches() {
                    return focus(self.id.clone());
                }
                Task::none()
            }
            Event::ToggleReplace => {
                self.replace_open = !self.replace_open;
                self.replace_focused = self.replace_open;
                self.find_focused = !self.replace_open;
                focus(if self.replace_open { REPLACE_INPUT } else { FIND_INPUT })
            }
            Event::ReplaceText(t) => {
                self.replace_text = t;
                Task::none()
            }
            Event::ReplaceOne if self.find_open => {
                let now = self.now_ms;
                let before = self.doc.revision();
                self.doc.replace_next(&self.replace_text, self.replace_preserve_case, now);
                // The first press only NAVIGATES (shows the match before
                // overwriting it), which commits nothing — run the tail only if a
                // replacement actually landed.
                if self.doc.revision() != before {
                    self.after_edit(CompletionEvent::CaretOrClose);
                    self.dirty = true;
                }
                Task::none()
            }
            Event::ReplaceAll if self.find_open => {
                if self.doc.replace_all(&self.replace_text, self.replace_preserve_case) > 0 {
                    self.after_edit(CompletionEvent::CaretOrClose);
                    self.dirty = true;
                }
                Task::none()
            }
            Event::TogglePreserveCase => {
                self.replace_preserve_case = !self.replace_preserve_case;
                Task::none()
            }
            Event::RenameText(text) => {
                if let Some(rename) = &mut self.rename {
                    rename.text = text;
                }
                Task::none()
            }
            // A name typed over text that has since changed, or an empty one,
            // asks nothing.
            Event::SubmitRename => {
                let Some(rename) = self.rename.take() else { return Task::none() };
                if rename.revision == self.doc.revision() && !rename.text.is_empty() {
                    let ticket = self.tickets.issue(rename.revision);
                    self.pending_rename_request = Some(RenameRequest::new(ticket, rename.offset, rename.text));
                }
                focus(self.id.clone())
            }
            // Guarded find variants whose guard did not hold (bar closed, or find
            // disabled): ignore.
            Event::OpenFind
            | Event::OpenReplace
            | Event::CloseFind
            | Event::FindNext
            | Event::FindPrev
            | Event::FindSelectAll
            | Event::ToggleFindInSelection
            | Event::ReplaceOne
            | Event::ReplaceAll
            | Event::PointerDown => Task::none(),
        }
    }

    /// The editor element. Map it back to your message type:
    /// `self.editor.view().map(Message::Editor)`.
    #[must_use]
    pub fn view(&self) -> Element<'_, Event> {
        let popup = match self.completion.state() {
            CompletionState::Open(list) => Some(list),
            _ => None,
        };
        let editor = Editor::new(&self.doc, Event::Editor)
            .popup(popup)
            .snippet_active(self.snippet.is_some())
            .signature(self.signature.as_ref())
            .hover(self.hover.as_ref())
            // A request the document has moved past can no longer land, so it
            // must not stop the pointer from re-arming a fresh one.
            .hover_pending(
                self.awaiting
                    .hover
                    .as_ref()
                    .filter(|(ticket, ..)| ticket.revision() == self.doc.revision())
                    .map(|(_, _, word)| word.clone()),
            )
            .inlay_tooltip(self.inlay_card.as_ref().map(|card| (card.key, card.part, card.markdown.as_str())))
            .wake_after(self.inlays.wait)
            .font(self.font)
            .text_size(self.text_size)
            .id(self.id.clone());
        let bar = match &self.rename {
            Some(rename) => Some(rename_bar(rename)),
            None => self.find_open.then(|| self.find_bar()),
        };
        match bar {
            // Float the bar over the editor, top-right (where mainstream
            // editors place it). The right padding clears the scrollbar lane so
            // the bar never sits over it; the overlay is transparent except the
            // bar, so clicks pass through.
            Some(bar) => {
                let overlay = container(bar)
                    .width(Length::Fill)
                    .align_x(Horizontal::Right)
                    .padding(iced::Padding::new(8.0).right(8.0 + crate::SCROLLBAR_WIDTH));
                stack([editor.into(), overlay.into()]).into()
            }
            None => editor.into(),
        }
    }

    /// The find bar: a floating panel with a query row (input + option toggles +
    /// match count + prev/next/scope/close) and, once the chevron expands it, a
    /// replace row (input + preserve-case + replace/replace-all).
    fn find_bar(&self) -> Element<'_, Event> {
        let count = self.doc.find_match_count();
        // A half-typed regex (`(`, `[a-`) is a NORMAL state, not a failure — but
        // it must say so rather than read as "No results".
        let invalid = self.doc.find_pattern_error().is_some();
        let label = if invalid {
            "Invalid regex".to_string()
        } else {
            match self.doc.active_find_match() {
                Some(i) => format!("{} of {}", i + 1, count),
                None if count > 0 => format!("{count} matches"),
                None if self.find_query.is_empty() => String::new(),
                None => "No results".to_string(),
            }
        };
        let label_color = if invalid || label == "No results" {
            Color::from_rgb8(0xF4, 0x87, 0x71)
        } else {
            Color::from_rgb8(0xCC, 0xCC, 0xCC)
        };
        // Fixed so the nav buttons never shuffle as the match-count digits change.
        const COUNT_W: f32 = 78.0;
        const SPACING: f32 = 4.0;
        const BTN_W: f32 = 22.0;
        const IN_BTN: f32 = 20.0;
        // Flat icon buttons: transparent at rest, translucent gray on hover; `on`
        // latches the background + a focus-blue border so an engaged option reads
        // as pressed at rest, not only under the pointer.
        let sized_btn = |glyph: char, on: bool, msg, w: f32, h: f32| {
            button(
                text(glyph.to_string())
                    .font(crate::CODICON)
                    .size(15)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .align_x(Horizontal::Center)
                    .align_y(Vertical::Center),
            )
            .width(Length::Fixed(w))
            .height(Length::Fixed(h))
            .padding(0)
            .on_press(msg)
            .style(move |_theme, status| {
                let hover = matches!(status, button::Status::Hovered | button::Status::Pressed);
                button::Style {
                    background: (on || hover).then(|| Color::from_rgba8(90, 93, 94, 0.314).into()),
                    text_color: Color::from_rgb8(0xCC, 0xCC, 0xCC),
                    border: if on {
                        iced::border::rounded(5.0).width(1.0).color(Color::from_rgb8(0x00, 0x7F, 0xD4))
                    } else {
                        iced::border::rounded(5.0)
                    },
                    shadow: Shadow::default(),
                    snap: true,
                }
            })
        };
        let icon_btn = |glyph: char, on: bool, msg| sized_btn(glyph, on, msg, BTN_W, BTN_W);
        let btn = |glyph: char, msg| sized_btn(glyph, false, msg, BTN_W, BTN_W);
        let in_btn = |glyph: char, on: bool, msg| sized_btn(glyph, on, msg, IN_BTN, IN_BTN);
        let box_of =
            |field, buttons, focused| box_of(field, buttons, focused, BAR_INPUT_W, BAR_ROW_H, BAR_BOX_GAP, BAR_BOX_MARGIN);

        let find_box = box_of(
            text_input("Find", &self.find_query)
                .id(FIND_INPUT)
                .on_input(Event::FindQuery)
                .on_submit(Event::FindNext)
                .padding(iced::Padding::new(4.0).left(2.0))
                .size(13)
                .width(Length::Fill)
                .style(bar_input_style)
                .into(),
            vec![
                in_btn(crate::icon::CASE_SENSITIVE, self.find_case, Event::ToggleCase).into(),
                in_btn(crate::icon::WHOLE_WORD, self.find_whole_word, Event::ToggleWholeWord).into(),
                in_btn(crate::icon::REGEX, self.find_regex, Event::ToggleRegex).into(),
            ],
            self.find_focused,
        );
        let find_row = row![
            find_box,
            text(label)
                .size(12)
                .color(label_color)
                .width(Length::Fixed(COUNT_W))
                .align_x(Horizontal::Left),
            btn(crate::icon::ARROW_UP, Event::FindPrev),
            btn(crate::icon::ARROW_DOWN, Event::FindNext),
            icon_btn(crate::icon::SELECTION, self.scoped(), Event::ToggleFindInSelection),
            btn(crate::icon::CLOSE, Event::CloseFind),
        ]
        .spacing(SPACING)
        .height(Length::Fixed(BAR_ROW_H))
        .align_y(Alignment::Center);
        let rows = if self.replace_open {
            let replace_box = box_of(
                text_input("Replace", &self.replace_text)
                    .id(REPLACE_INPUT)
                    .on_input(Event::ReplaceText)
                    .on_submit(Event::ReplaceOne)
                    .padding(iced::Padding::new(4.0).left(2.0))
                    .size(13)
                    .width(Length::Fill)
                    .style(bar_input_style)
                    .into(),
                vec![in_btn(crate::icon::PRESERVE_CASE, self.replace_preserve_case, Event::TogglePreserveCase)
                    .into()],
                self.replace_focused,
            );
            let replace_row = row![
                replace_box,
                btn(crate::icon::REPLACE, Event::ReplaceOne),
                btn(crate::icon::REPLACE_ALL, Event::ReplaceAll),
            ]
            .spacing(SPACING)
            .height(Length::Fixed(BAR_ROW_H))
            .align_y(Alignment::Center);
            column![find_row, replace_row].spacing(SPACING)
        } else {
            column![find_row]
        };
        container(
            row![
                // The chevron spans both rows — its hit target grows with the bar
                // it toggles, VS Code's shape and the honest one (it acts on the
                // panel, not the query row it sits next to).
                sized_btn(
                    if self.replace_open { crate::icon::CHEVRON_DOWN } else { crate::icon::CHEVRON_RIGHT },
                    false,
                    Event::ToggleReplace,
                    BTN_W,
                    if self.replace_open { BAR_ROW_H * 2.0 + SPACING } else { BAR_ROW_H },
                ),
                rows,
            ]
            .spacing(SPACING)
            .align_y(Alignment::Center),
        )
        .padding([6, 8])
        .style(bar_panel_style)
        .into()
    }

    /// The editor's subscription — the frames-gated highlight sweep. It runs
    /// only while a dirty highlight frontier remains, and because
    /// [`language`](CodeEditor::language) / an edit leaves the frontier dirty it
    /// fires on the very next frame with no input required (this is what makes
    /// highlighting appear at load instead of after the first scroll). At
    /// convergence it returns [`Subscription::none`], so an idle document does
    /// zero per-frame work. Map it: `self.editor.subscription().map(Message::Editor)`.
    pub fn subscription(&self) -> Subscription<Event> {
        let sweeping = self.doc.highlight_frontier().is_some() || self.pool_active();
        let sweep = if sweeping {
            iced::window::frames().map(|_| Event::HighlightSweep)
        } else {
            Subscription::none()
        };
        // Find keys are chrome: caught here regardless of capture status, so
        // Escape closes the bar in one press even though the native input
        // captured it. `listen_with` filters, so non-find keys produce nothing
        // and the editor's own captured keystrokes still route through the widget.
        // The closure is a plain fn (non-capturing), so the `find_open` gating
        // happens in `update`. The same chords close the rename field, so they
        // are live while it is open even with find disabled.
        let keys = if self.find_enabled || self.rename.is_some() {
            iced::event::listen_with(|event, status, _window| match event {
                iced::Event::Keyboard(iced::keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                    find_chord(&key, modifiers, status)
                }
                // A press can move focus natively, behind the app's back, leaving
                // two widgets focused — watched here because the press is captured
                // by the input and never surfaces as a widget callback.
                iced::Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left)) => {
                    Some(Event::PointerDown)
                }
                _ => None,
            })
        } else {
            Subscription::none()
        };
        Subscription::batch([sweep, keys])
    }

    // ── internals ───────────────────────────────────────────────────────────

    /// Colour the document at load (or after a grammar swap / buffer load): the
    /// large-document parallel sweep for a big buffer, else a synchronous seed
    /// aimed at the reported viewport, or at the whole buffer before the widget
    /// has reported one. The widget reports only viewport changes, so a `load`
    /// while scrolled far down gets no fresh report to re-aim the window.
    /// The cold-load fix — the first paint is coloured
    /// with no viewport report required, and a huge buffer never blocks the UI
    /// thread (its pool sweeps in the background, aimed at the current viewport,
    /// which the first `ViewportChanged` reaims). Recreating the pool here also
    /// picks up a new grammar's engine after a `load` language swap.
    ///
    /// The synchronous seed is one `tokenize_highlight` call. For a tree-sitter
    /// grammar that call parses within a budget, so a big document's first call
    /// may only start the parse; the frontier stays dirty and
    /// [`HighlightSweep`](Event::HighlightSweep) resumes it each frame.
    fn seed_highlight(&mut self) {
        if !self.pool_seed() {
            let n = self.doc.buffer().line_count();
            let aim = if self.viewport.is_empty() || self.viewport.start >= n { 0..n } else { self.viewport.clone() };
            self.doc.set_highlight_window(aim);
            self.doc.tokenize_highlight(n);
        }
    }

    /// Push the live query text + its options into the document — the ONE place a
    /// [`FindQuery`] is built. Every input that changes what matches (the text
    /// and each option toggle) routes here, so the bar's controls cannot disagree
    /// about the live query. Empty text means "no query", never match-all.
    fn push_find_query(&mut self) {
        let query = (!self.find_query.is_empty()).then(|| {
            // `FindQuery` is `#[non_exhaustive]`: build via `new` + the fields.
            let mut q = FindQuery::new(self.find_query.clone());
            q.case_sensitive = self.find_case;
            q.whole_word = self.find_whole_word;
            q.regex = self.find_regex;
            q
        });
        let now = self.now_ms;
        self.doc.set_find_query(query, now);
    }

    /// Close the bar and drop its query. Also collapses the replace row so the
    /// next open starts from a clean shape (Ctrl+F find-only, Ctrl+H with replace).
    fn close_find(&mut self) {
        self.find_open = false;
        self.replace_open = false;
        self.find_focused = false;
        self.replace_focused = false;
        self.find_query.clear();
        let now = self.now_ms;
        self.doc.set_find_query(None, now);
    }

    /// Whether find is currently scoped to a selection (the find-in-selection
    /// toggle latches off the document's live scope, not an app-side copy).
    fn scoped(&self) -> bool {
        self.doc.find_scope().is_some()
    }

    /// Read the live widget focus back into the ring flags — for focus changes
    /// that can't be predicted here (a click, a Tab).
    fn sync_rings() -> Task<Event> {
        Task::batch([
            is_focused(FIND_INPUT).map(|on| Event::Focused { field: Field::Find, on }),
            is_focused(REPLACE_INPUT).map(|on| Event::Focused { field: Field::Replace, on }),
            is_focused(RENAME_INPUT).map(|on| Event::Focused { field: Field::Rename, on }),
        ])
    }

    /// Re-assert single focus after a press: a click inside an input focuses it
    /// natively and captures the event, so the editor's own press handler never
    /// runs to unfocus itself — focusing whichever bar input iced reports as
    /// focused is exactly the repair. When the press landed in the editor, no
    /// bar input is focused and every arm is a no-op.
    fn resync_focus() -> Task<Event> {
        Task::batch([
            is_focused(FIND_INPUT).then(|f| if f { focus(FIND_INPUT) } else { Task::none() }),
            is_focused(REPLACE_INPUT).then(|f| if f { focus(REPLACE_INPUT) } else { Task::none() }),
            is_focused(RENAME_INPUT).then(|f| if f { focus(RENAME_INPUT) } else { Task::none() }),
        ])
    }

    /// Apply one editing/selection [`Action`] to the document, then run the
    /// post-edit tail. The intel / popup / snippet / hover / viewport / fold
    /// actions are handled before `apply` (or are no-ops here) and land as the
    /// catch-all arm; they gain behavior as later milestones wire the controllers.
    fn apply(&mut self, action: Action) {
        // Capture what this action means for completion before the match consumes
        // `action`.
        let comp_event = match &action {
            Action::Type(c) => CompletionEvent::Typed(*c),
            Action::Backspace | Action::DeleteWordBack | Action::Delete | Action::DeleteWordForward => {
                CompletionEvent::Deleting
            }
            _ => CompletionEvent::CaretOrClose,
        };
        let rev_before = self.doc.revision();
        match action {
            Action::Type(ch) => self.doc.type_char(ch),
            Action::Backspace => self.doc.backspace(),
            Action::Delete => self.doc.delete_forward(),
            Action::DeleteWordBack => self.doc.delete_word_back(),
            Action::DeleteWordForward => self.doc.delete_word_forward(),
            Action::Enter => self.doc.enter(),
            Action::Tab => self.doc.tab(),
            Action::Outdent => self.doc.outdent(),
            Action::ToggleComment => self.doc.toggle_line_comment(),
            Action::DeleteLine => self.doc.delete_line(),
            Action::InsertLine { down } => self.doc.insert_line(down),
            Action::Cut => self.doc.cut(),
            Action::Paste { text, entire_line } => self.doc.paste(&text, entire_line),
            Action::Move { motion, extend } => self.doc.move_carets(motion, extend),
            Action::PlaceCaret(offset) => {
                let mut set = SelectionSet::new(0);
                set.set_single(Selection::caret(SelectionId(0), offset));
                self.doc.set_selections(set);
            }
            Action::DragSelect { granularity, origin, head } => {
                self.doc.drag_select(granularity, origin, head);
            }
            Action::AddCaret(offset) => self.doc.add_caret(offset),
            Action::AddNextOccurrence => self.doc.add_next_occurrence(),
            Action::RemoveNewestCaret => self.doc.remove_newest_selection(),
            Action::SelectAllOccurrences => self.doc.select_all_occurrences(),
            Action::AddCaretVertical { down } => self.doc.add_caret_vertical(down),
            Action::JumpToBracket => self.doc.jump_to_bracket(),
            Action::NextDiagnostic { forward } => {
                self.doc.next_diagnostic(forward);
            }
            Action::ExpandSelection => self.doc.expand_selection(),
            Action::ShrinkSelection => self.doc.shrink_selection(),
            Action::Collapse => self.doc.collapse_selections(),
            Action::SelectAll => self.doc.select_all(),
            Action::Undo => {
                self.doc.undo();
            }
            Action::Redo => {
                self.doc.redo();
            }
            Action::ColumnSelect(dir) => self.doc.column_select(dir),
            Action::ColumnDrag { anchor, active } => self.doc.column_drag(anchor, active),
            Action::MoveLine { down } => self.doc.move_line(down),
            Action::CopyLine { down } => self.doc.copy_line(down),
            // Handled in `update` (viewport, folds) or not yet wired (popup /
            // snippet / signature / hover — later milestones). Exhaustive no-op.
            Action::ViewportChanged(_)
            | Action::PopupUp
            | Action::PopupDown
            | Action::PopupAccept
            | Action::PopupClickAccept(_)
            | Action::PopupDismiss
            | Action::TriggerCompletion
            | Action::SnippetTab
            | Action::SnippetTabPrev
            | Action::SnippetCancel
            | Action::SignatureClose
            | Action::HoverQuery(_)
            | Action::HoverDismiss
            | Action::GotoDefinition
            | Action::Rename
            | Action::Format
            | Action::ToggleFold { .. }
            | Action::FoldAtCarets { .. }
            | Action::Wake(_)
            | Action::InlayHover { .. }
            | Action::InlayJump { .. }
            | Action::InlayInsert { .. } => {}
        }
        self.after_edit(comp_event);
        // Dirty only on an actual text change — a bare caret move / selection must
        // not schedule a host recompile or flip the save indicator.
        if self.doc.revision() != rev_before {
            self.dirty = true;
        }
    }

    /// Everything that must follow a document mutation — the ONE owner of the
    /// post-edit tail. Every edit entry point runs it, so no second entry point
    /// can silently do half of it (a path that skipped `tokenize_highlight` would
    /// paint stale colors; one that skipped `drive_completion` would strand the
    /// popup). The `on_edit` host signal joins it in a later milestone.
    fn after_edit(&mut self, comp_event: CompletionEvent) {
        // Bring the highlight cache current down to the reported viewport bottom
        // only — convergence stops at the edited lines for a normal edit, and the
        // viewport bound caps a state cascade to the screen.
        self.doc.tokenize_highlight(self.viewport.end);
        // Keep find fresh while editing: matches ride the edit via the decoration
        // mover; a debounced re-scan picks up appearing/disappearing matches.
        let now = self.now_ms;
        self.doc.maybe_rescan_find(now);
        // Drive completion (typing opens/filters, deleting refilters, else close),
        // signature help, and reconcile the snippet session; any edit closes hover.
        self.drive_completion(comp_event);
        self.drive_signature(comp_event);
        self.reconcile_snippet();
        self.hover = None;
        self.inlay_card = None;
        // A caret jump abandons a pending hover and definition. Typing keeps
        // them: their replies are then dropped by revision, and the pointer
        // re-arm asks for hover again.
        if matches!(comp_event, CompletionEvent::CaretOrClose) {
            self.abandon(Awaited::Hover);
            self.abandon(Awaited::Definition);
        }
        // The rename field names the symbol at the revision it opened on, so an
        // edit underneath (a host edit, an undo) makes it stale.
        if self.rename.as_ref().is_some_and(|r| r.revision != self.doc.revision()) {
            self.rename = None;
        }
        self.inlays_after_edit();
        // `dirty` is set by the callers on an actual text change (a bare caret
        // move runs the tail but must not dirty the document — see `apply`).
    }

    /// Drive the completion controller after an edit. The controller action is
    /// decided from the event here; whether it runs a synchronous provider or
    /// records an async request is [`request_completions`](Self::request_completions)'s
    /// job. No-op unless the host wired completions (sync provider or async pull).
    fn drive_completion(&mut self, event: CompletionEvent) {
        match event {
            CompletionEvent::Typed(c) => {
                let trigger = if is_completion_word_char(c) {
                    CompletionTrigger::Typed(c)
                } else if matches!(c, '(' | ',' | '=' | ':' | '.' | ' ') {
                    CompletionTrigger::TriggerChar(c)
                } else {
                    self.completion.on_boundary();
                    self.abandon(Awaited::Completion);
                    return;
                };
                self.request_completions(trigger);
            }
            // A deletion re-asks while a list is showing or on its way, so the
            // answer tracks the shorter word; emptying the word ends it.
            CompletionEvent::Deleting => {
                if self.completion.is_open() || self.awaiting.completion.is_some() {
                    let word = self.completion_word_text();
                    match word.chars().last() {
                        Some(c) => self.request_completions(CompletionTrigger::Typed(c)),
                        None => {
                            self.completion.close();
                            self.abandon(Awaited::Completion);
                        }
                    }
                }
            }
            CompletionEvent::CaretOrClose => {
                self.completion.close();
                self.abandon(Awaited::Completion);
            }
        }
    }

    /// Query completions for `trigger`: a synchronous provider fills the popup
    /// inline; with none set, record an async [`CompletionRequest`] under a
    /// fresh ticket for the host to pull via
    /// [`take_completion_request`](Self::take_completion_request) and answer
    /// through [`set_completions`](Self::set_completions). The async path keeps
    /// the provider path's behavior: an Escape-dismissed word asks for nothing,
    /// an open popup narrows immediately, and a trigger char or manual invoke
    /// overrides the dismissal.
    fn request_completions(&mut self, trigger: CompletionTrigger) {
        // Take the provider out so `self` is free for `build_cx`, then restore it
        // (avoids an is_some/unwrap dance and the whole-self borrow conflict).
        let Some(mut provider) = self.comp_provider.take() else {
            // Sampled before the refilter below can close the popup.
            let start = if self.completion.is_open() || self.awaiting.completion.is_some() {
                Start::Continuing
            } else {
                Start::Fresh
            };
            match trigger {
                CompletionTrigger::Typed(_) => {
                    if matches!(self.completion.state(), CompletionState::DismissedUntilBoundary) {
                        return;
                    }
                    // Narrow the open popup from the items it has instead of
                    // lagging a round trip behind the typing.
                    let word = self.completion_word_text();
                    self.completion.refilter(&word);
                }
                // A trigger char starts a new word; the open list described the
                // one it just ended. Closing also clears a dismissal.
                CompletionTrigger::TriggerChar(_) => self.completion.close(),
                // An open popup stays until the fresh list replaces it.
                CompletionTrigger::Manual => self.completion.on_boundary(),
            }
            let ticket = self.tickets.issue(self.doc.revision());
            self.awaiting.completion = Some(ticket);
            self.pending_completion_request =
                Some(CompletionRequest::new(ticket, self.completion_word(), trigger, start));
            return;
        };
        // A provider call replaces the list, unless a word char is refiltering
        // an open popup, which keeps the items and the caret they belong to.
        let fresh = !(self.completion.is_open() && matches!(trigger, CompletionTrigger::Typed(_)));
        let head = self.doc.selections().newest().head();
        let cx = self.build_cx(trigger);
        let word = self.completion_word_text();
        self.completion.on_input(&cx, &word, &mut *provider);
        self.comp_provider = Some(provider);
        if fresh {
            self.items_caret = head;
        }
    }

    /// Drive the signature-help box: `(` opens it; while it shows, or while a
    /// request for it is in flight, every edit or move re-queries, and a `None`
    /// reply closes it. Re-querying while awaited is what lets `foo(a` typed
    /// faster than the reply still open the box: the reply to the `(` request
    /// is stale by then, and only the newest request can land.
    fn drive_signature(&mut self, event: CompletionEvent) {
        let query = matches!(event, CompletionEvent::Typed('('))
            || self.signature.is_some()
            || self.awaiting.signature.is_some();
        if !query {
            return;
        }
        if let Some(mut provider) = self.sig_provider.take() {
            let cx = self.build_sig_cx();
            self.signature = provider.signature(&cx);
            self.sig_provider = Some(provider);
        } else {
            // No synchronous provider: record an async request for the host.
            let head = self.doc.selections().newest().head();
            let ticket = self.tickets.issue(self.doc.revision());
            let call = self.doc.brackets().innermost_open(head, b'(');
            self.awaiting.signature = Some(ticket);
            self.pending_signature_request =
                Some(SignatureRequest::new(ticket, self.doc.buffer().offset_to_point(head), call));
        }
    }

    /// Accept the popup's selected item as one edit: the main replacement and
    /// the item's additional edits (an auto-import, say) in a single batch, so
    /// one undo reverts all of it. A snippet expands and starts a tab-stop
    /// session at the first stop. Fires the retrigger and the signature-help
    /// follow-up if the item asks for them.
    ///
    /// The item's ranges were produced at `items_caret`. Since then the user
    /// may have typed on inside the word, so a replace range ending in the live
    /// word stretches to the caret, and additional edits at or past the popup
    /// anchor shift by the caret's movement. The snippet base and the caret come
    /// from where the patch put the main replacement, since an import above it
    /// moves it.
    fn accept_completion(&mut self) {
        let CompletionState::Open(list) = self.completion.state() else { return };
        let anchor = list.anchor;
        let Some(item) = self.completion.accept() else { return };
        // The accept retires the in-flight request; a retrigger below asks
        // afresh, so this must come first.
        self.abandon(Awaited::Completion);

        let caret = self.doc.selections().newest().head();
        let word = self.completion_word();
        let replace = match item.replace.clone() {
            None => word.clone(),
            Some(r) if (word.start..=word.end).contains(&r.end) => r.start..word.end,
            Some(r) => r,
        };
        let delta = i64::from(caret) - i64::from(self.items_caret);
        let shift = |o: u32| (i64::from(o) + delta).clamp(0, i64::from(u32::MAX)) as u32;
        let mut batch: Vec<EditOp> = item
            .additional
            .iter()
            .map(|op| {
                if op.range.start >= anchor {
                    EditOp::new(shift(op.range.start)..shift(op.range.end), op.text.clone())
                } else {
                    op.clone()
                }
            })
            // An edit touching the replaced word would fight the insertion; the
            // insertion wins.
            .filter(|op| op.range.end < replace.start || op.range.start > replace.end)
            .collect();

        // Read before the batch can move the line.
        let indent = self.line_indent(replace.start);
        let (text, expanded) = match &item.insert {
            InsertText::Plain(s) => (s.clone(), None),
            InsertText::Snippet(body) => match Snippet::parse(body) {
                Ok(snip) => {
                    let e = snip.for_insertion(&indent, default_indent_size() as usize);
                    (e.text.clone(), Some(e))
                }
                Err(_) => (body.clone(), None),
            },
        };
        let main = EditOp::new(replace.clone(), text.clone());
        batch.push(main.clone());
        batch.sort_by_key(|op| (op.range.start, op.range.end));
        let before = self.doc.revision();
        // A malformed set of additional edits must not cost the user the
        // completion itself.
        let committed = match self.doc.edit(batch) {
            Ok(c) => c,
            Err(_) => match self.doc.edit(vec![main]) {
                Ok(c) => c,
                Err(_) => return,
            },
        };
        let base = committed.patch().map_offset(replace.start, Bias::Left);

        if let Some(mut s) = self.snippet.take() {
            s.cancel(self.doc.decorations_mut());
        }
        match expanded {
            Some(e) => match SnippetSession::start(&e, base, self.doc.decorations_mut()) {
                Some((session, first)) => {
                    self.set_selection_range(first);
                    self.snippet = Some(session);
                }
                None => {
                    let fin = e.stops.last().map_or(e.text.len() as u32, |s| s.range.start);
                    self.set_caret(base + fin);
                }
            },
            None => self.set_caret(base + text.len() as u32),
        }
        self.doc.tokenize_highlight(self.viewport.end);
        let now = self.now_ms;
        self.doc.maybe_rescan_find(now);
        self.inlays_after_edit();
        if self.doc.revision() != before {
            self.dirty = true;
        }

        if item.retrigger && self.snippet.is_none() {
            self.request_completions(CompletionTrigger::Manual);
        }
        if item.signature_after {
            self.drive_signature(CompletionEvent::Typed('('));
        }
    }

    /// Whether a reply stamped `ticket` may land: it must be the request this
    /// editor still awaits for `kind`, and the document must not have moved
    /// since it was made. The one gate every async `set_*` goes through.
    fn accepts(&self, kind: Awaited, ticket: Ticket) -> bool {
        let awaited = match kind {
            Awaited::Completion => self.awaiting.completion,
            Awaited::Signature => self.awaiting.signature,
            Awaited::Hover => self.awaiting.hover.as_ref().map(|(t, ..)| *t),
            Awaited::Definition => self.awaiting.definition,
            Awaited::Inlays => self.awaiting.inlays,
            Awaited::InlayTooltip => self.awaiting.inlay_tooltip.as_ref().map(|(t, ..)| *t),
            Awaited::InlayInsert => self.awaiting.inlay_insert.as_ref().map(|(t, ..)| *t),
        };
        awaited == Some(ticket) && ticket.revision() == self.doc.revision()
    }

    /// Stop waiting for `kind`: drop the request the host hasn't pulled yet and
    /// the ticket a late reply would carry.
    fn abandon(&mut self, kind: Awaited) {
        match kind {
            Awaited::Completion => {
                self.awaiting.completion = None;
                self.pending_completion_request = None;
            }
            Awaited::Signature => {
                self.awaiting.signature = None;
                self.pending_signature_request = None;
            }
            Awaited::Hover => {
                self.awaiting.hover = None;
                self.pending_hover_request = None;
            }
            Awaited::Definition => {
                let ticket = self.awaiting.definition.take();
                self.pending_definition_request = None;
                self.drop_interaction(ticket);
            }
            Awaited::Inlays => {
                self.awaiting.inlays = None;
                self.pending_inlay_request = None;
            }
            Awaited::InlayTooltip => {
                let ticket = self.awaiting.inlay_tooltip.take().map(|(t, ..)| t);
                self.drop_interaction(ticket);
            }
            Awaited::InlayInsert => {
                let ticket = self.awaiting.inlay_insert.take().map(|(t, ..)| t);
                self.drop_interaction(ticket);
            }
        }
    }

    /// Forget the unpulled gesture made under `ticket`; a newer gesture's
    /// stays.
    fn drop_interaction(&mut self, ticket: Option<Ticket>) {
        if ticket.is_some() && self.pending_inlay_interaction.as_ref().map(inlay::Interaction::ticket) == ticket {
            self.pending_inlay_interaction = None;
        }
    }

    /// Ask for hints after `delay`, capped across a run of triggers by `cap`.
    /// Each call restarts the wait. Ignored while hints are off.
    fn wait_inlays(&mut self, delay: Duration, cap: Option<Duration>) {
        if !self.inlays.enabled {
            return;
        }
        self.inlays.generation += 1;
        self.inlays.wait = Some(Wake { generation: self.inlays.generation, delay, cap });
    }

    /// Note an edit for the hints: their tooltip and insert describe text that
    /// is gone, and the new text needs its own hints once typing pauses.
    fn inlays_after_edit(&mut self) {
        if self.doc.revision() == self.inlays.seen {
            return;
        }
        self.inlays.seen = self.doc.revision();
        self.inlay_card = None;
        self.abandon(Awaited::InlayTooltip);
        self.abandon(Awaited::InlayInsert);
        self.wait_inlays(INLAY_EDIT_DELAY, None);
    }

    /// The buffer rows to fetch hints for, and their inner half: the viewport
    /// padded by its height above and twice that below, at least
    /// `INLAY_MIN_ROWS`, within the document. The pads count display rows, so
    /// a block fold on screen doesn't widen them; a fold's hidden interior
    /// inside the window is requested too.
    fn inlay_window(&self) -> (Range<u32>, Range<u32>) {
        let lines = self.doc.buffer().line_count();
        let folds = self.doc.fold_map();
        let shown = folds.display_row_count();
        let to_display = |row: u32| if row >= lines { shown } else { folds.to_display_row(BufferRow(row)).index() };
        let first = to_display(self.viewport.start);
        let vis = first..to_display(self.viewport.end).max(first);
        let height = vis.end - vis.start;
        let mut start = vis.start.saturating_sub(height);
        let mut end = vis.end.saturating_add(2 * height).min(shown);
        if end - start < INLAY_MIN_ROWS {
            end = start.saturating_add(INLAY_MIN_ROWS).min(shown);
            start = end.saturating_sub(INLAY_MIN_ROWS);
        }
        let inner = start + (vis.start - start) / 2..end - (end - vis.end) / 2;
        let to_buffer = |d: u32| if d >= shown { lines } else { folds.to_buffer_row(folds.display_row_at(f64::from(d))).0 };
        (to_buffer(start)..to_buffer(end), to_buffer(inner.start)..to_buffer(inner.end))
    }

    /// Whether the hints on screen describe the current text: only then may a
    /// gesture on them be recorded.
    fn inlays_current(&self) -> bool {
        self.doc.inlays_revision() == Some(self.doc.revision())
    }

    /// Tab / Shift+Tab through the active snippet session.
    fn snippet_tab(&mut self, forward: bool) {
        let Some(mut session) = self.snippet.take() else { return };
        match session.tab(forward, self.doc.decorations_mut()) {
            TabOutcome::Move(range) => {
                self.set_selection_range(range);
                self.snippet = Some(session);
            }
            TabOutcome::Finish(offset) => self.set_caret(offset),
            TabOutcome::Stay => self.snippet = Some(session),
        }
    }

    /// Cancel the snippet session if the primary caret has left every stop.
    fn reconcile_snippet(&mut self) {
        if self.snippet.is_none() {
            return;
        }
        let head = self.doc.selections().newest().head();
        let escaped = self.snippet.as_ref().unwrap().edit_escapes(&(head..head), self.doc.decorations());
        if escaped {
            let mut s = self.snippet.take().unwrap();
            s.cancel(self.doc.decorations_mut());
        }
    }

    /// Move the primary caret to `offset`.
    fn set_caret(&mut self, offset: u32) {
        let mut set = SelectionSet::new(0);
        set.set_single(Selection::caret(SelectionId(0), offset));
        self.doc.set_selections(set);
    }

    /// Select `range` as the primary selection.
    fn set_selection_range(&mut self, range: Range<u32>) {
        let mut set = SelectionSet::new(0);
        set.set_single(Selection::from_anchor(SelectionId(0), range.start, range.end));
        self.doc.set_selections(set);
    }

    /// The completion-word byte range ending at the primary caret (empty at a
    /// boundary), computed with `is_completion_word_char`.
    fn completion_word(&self) -> Range<u32> {
        let head = self.doc.selections().newest().head();
        let p = self.doc.buffer().offset_to_point(head);
        let line_start = head - p.col;
        let prefix = &self.doc.buffer().line(p.row)[..p.col as usize];
        let word_start = prefix
            .char_indices()
            .rev()
            .take_while(|(_, c)| is_completion_word_char(*c))
            .last()
            .map_or(prefix.len(), |(i, _)| i);
        (line_start + word_start as u32)..head
    }

    /// The text of the completion word under the caret.
    fn completion_word_text(&self) -> String {
        let w = self.completion_word();
        self.doc.buffer().slice(w.start..w.end).into_owned()
    }

    /// The leading whitespace of the line containing byte `offset`.
    fn line_indent(&self, offset: u32) -> String {
        let row = self.doc.buffer().offset_to_point(offset).row;
        self.doc.buffer().line(row).chars().take_while(|c| *c == ' ' || *c == '\t').collect()
    }

    /// Build a completion request from the current document state. The lookback
    /// slice (up to `LOOKBACK_LINES`) is skipped when the popup is already open
    /// and a word char was typed — that path only refilters locally and never
    /// reads it, so the hot per-keystroke path avoids the slice copy.
    fn build_cx(&self, trigger: CompletionTrigger) -> CompletionCx {
        let head = self.doc.selections().newest().head();
        let position = self.doc.buffer().offset_to_point(head);
        let refilter_only =
            self.completion.is_open() && matches!(trigger, CompletionTrigger::Typed(_));
        let lookback = if refilter_only {
            String::new()
        } else {
            let start_row = position.row.saturating_sub(LOOKBACK_LINES - 1);
            let lb_start = self.doc.buffer().point_to_offset(Point::new(start_row, 0));
            self.doc.buffer().slice(lb_start..head).into_owned()
        };
        CompletionCx {
            doc: self.doc.buffer().doc_id(),
            revision: self.doc.revision().0,
            position,
            word: self.completion_word(),
            lookback,
            trigger,
        }
    }

    /// Build a signature request from the current document state.
    fn build_sig_cx(&self) -> SignatureCx {
        let head = self.doc.selections().newest().head();
        let position = self.doc.buffer().offset_to_point(head);
        let start_row = position.row.saturating_sub(LOOKBACK_LINES - 1);
        let lb_start = self.doc.buffer().point_to_offset(Point::new(start_row, 0));
        SignatureCx {
            doc: self.doc.buffer().doc_id(),
            revision: self.doc.revision().0,
            position,
            lookback: self.doc.buffer().slice(lb_start..head).into_owned(),
        }
    }

    /// The word range around `offset` (scanning both directions with
    /// `is_completion_word_char`) — the word under the hover pointer.
    fn word_around(&self, offset: u32) -> Range<u32> {
        let p = self.doc.buffer().offset_to_point(offset);
        let line = self.doc.buffer().line(p.row);
        let line_start = offset - p.col;
        let col = p.col as usize;
        let start = line[..col]
            .char_indices()
            .rev()
            .take_while(|(_, c)| is_completion_word_char(*c))
            .last()
            .map_or(col, |(i, _)| i);
        let mut end = col;
        for c in line[col..].chars().take_while(|c| is_completion_word_char(*c)) {
            end += c.len_utf8();
        }
        (line_start + start as u32)..(line_start + end as u32)
    }

    /// The hover card for `offset`: the diagnostics under it, escaped so a
    /// message's own `*` or backtick renders literally, then the docs after a
    /// blank line. The diagnostics never depend on the docs, so a `None` reply
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

    /// Build a hover request for the word under `offset`. The lookback runs
    /// through the word's END, so the provider reads the full word from its tail.
    fn build_hover_cx(&self, offset: u32) -> HoverCx {
        let word = self.word_around(offset);
        let position = self.doc.buffer().offset_to_point(offset);
        let start_row = position.row.saturating_sub(LOOKBACK_LINES - 1);
        let lb_start = self.doc.buffer().point_to_offset(Point::new(start_row, 0));
        HoverCx {
            doc: self.doc.buffer().doc_id(),
            revision: self.doc.revision().0,
            position,
            word: word.clone(),
            lookback: self.doc.buffer().slice(lb_start..word.end).into_owned(),
        }
    }
}

/// Place `[Fill field | buttons]` in a styled, fixed-width container — so both
/// find and replace boxes come out identical regardless of how many buttons each
/// holds. The focus ring lives here (a container can't read its field's focus),
/// driven by the caller-tracked `focused` flag.
fn box_of<'a>(
    field: Element<'a, Event>,
    buttons: Vec<Element<'a, Event>>,
    focused: bool,
    w: f32,
    h: f32,
    gap: f32,
    margin: f32,
) -> Element<'a, Event> {
    container(
        row![field, row(buttons).spacing(gap).align_y(Alignment::Center)]
            .spacing(gap)
            .align_y(Alignment::Center),
    )
    .width(Length::Fixed(w))
    .height(Length::Fixed(h))
    .padding(iced::Padding::from([0.0, margin]))
    .style(move |_theme: &Theme| container::Style {
        background: Some(Color::from_rgb8(0x3C, 0x3C, 0x3C).into()),
        border: iced::border::rounded(4.0).width(1.0).color(if focused {
            Color::from_rgb8(0x00, 0x7F, 0xD4) // focus ring
        } else {
            Color::TRANSPARENT
        }),
        ..container::Style::default()
    })
    .into()
}

/// The bars' text-input look. The field is transparent: the box and the focus
/// ring live on the `box_of` container around it, beside the in-box buttons as
/// a row sibling. A `Stack` overlay would early-return on capture and leave a
/// field stale-focused.
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
        shadow: Shadow {
            color: Color::from_rgba8(0, 0, 0, 0.36),
            offset: Vector::new(0.0, 2.0),
            blur_radius: 8.0,
        },
        ..container::Style::default()
    }
}

/// The rename field: one input seeded with the symbol under the caret, in the
/// find bar's panel. Enter submits; Escape (the shared bar chord) closes it.
fn rename_bar(rename: &Rename) -> Element<'_, Event> {
    let field = text_input("Rename symbol", &rename.text)
        .id(RENAME_INPUT)
        .on_input(Event::RenameText)
        .on_submit(Event::SubmitRename)
        .padding(iced::Padding::new(4.0).left(2.0))
        .size(13)
        .width(Length::Fill)
        .style(bar_input_style);
    container(box_of(
        field.into(),
        Vec::new(),
        rename.focused,
        BAR_INPUT_W,
        BAR_ROW_H,
        BAR_BOX_GAP,
        BAR_BOX_MARGIN,
    ))
    .padding([6, 8])
    .style(bar_panel_style)
    .into()
}

/// A severity's display name for the diagnostic hover.
fn severity_label(sev: Severity) -> &'static str {
    match sev {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
        Severity::Hint => "hint",
    }
}

/// The find bar's global chord table — a free fn so it is testable without a
/// window. `status` is what the widget tree did with the key BEFORE this saw it,
/// and is load-bearing for Tab: a focused editor captures Tab (it indents), so
/// gating on `Ignored` keeps indent working while the bar is open.
fn find_chord(
    key: &iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
    status: iced::event::Status,
) -> Option<Event> {
    use iced::keyboard::{key::Named, Key};
    let ctrl = modifiers.command() || modifiers.control();
    match key {
        Key::Character(c) if ctrl && c.as_str() == "f" => Some(Event::OpenFind),
        Key::Character(c) if ctrl && c.as_str() == "h" => Some(Event::OpenReplace),
        Key::Named(Named::Escape) => Some(Event::CloseFind),
        // Alt+Enter selects all matches — safe as a global chord because the
        // editor ignores Alt+Enter. Plain Enter is NOT here: in the input it
        // navigates via `on_submit`, in the editor it must only type a newline.
        Key::Named(Named::Enter) if modifiers.alt() => Some(Event::FindSelectAll),
        // Tab moves focus between the inputs — but only when nothing else took
        // the key (the editor captures Tab to indent).
        Key::Named(Named::Tab) if status == iced::event::Status::Ignored => {
            Some(Event::CycleFocus { back: modifiers.shift() })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "syntect")]
    use scrive_core::SyntaxDef;

    // A minimal grammar: one keyword-scoped rule over word runs. Enough to drive
    // the highlight cache without pulling the example's Rust grammar into the lib.
    #[cfg(feature = "syntect")]
    const GRAMMAR: &str = "%YAML 1.2\n---\nname: T\nscope: source.t\ncontexts:\n  main:\n    - match: '\\w+'\n      scope: keyword.t\n";

    /// The cold-load fix, as a fails-first regression: attaching a grammar
    /// tokenizes the visible document *at construction*, so the first paint is
    /// coloured with NO `update`/`ViewportChanged` ever called. Without the seed
    /// in [`CodeEditor::language`] the frontier stays fully dirty after
    /// `set_syntax` and this assertion fails — which was exactly the
    /// "no highlighting until one scroll tick" bug.
    #[cfg(feature = "syntect")]
    #[test]
    fn language_tokenizes_at_load_without_a_viewport_report() {
        let grammar = SyntaxDef::from_sublime_syntax(GRAMMAR).expect("grammar parses");
        let editor = CodeEditor::new("fn main() {}\nlet x = 1;\n").language(grammar);
        assert!(
            editor.document().highlight_frontier().is_none(),
            "grammar attach left the highlight frontier dirty — cold load would show uncoloured text until a scroll",
        );
    }

    /// Plain-text (no grammar) is a valid state: no highlight cache, nothing to
    /// pump, the sweep subscription stays idle.
    #[test]
    fn no_grammar_leaves_no_frontier_to_pump() {
        let editor = CodeEditor::new("plain text\n");
        assert!(editor.document().highlight_frontier().is_none());
    }

    /// Replace-all driven through the relocated find bar's [`Event`]s must land as
    /// one transaction: a single undo restores the document byte-for-byte. Proves
    /// the find/replace message path commits once, not once per match.
    #[test]
    fn replace_all_through_events_is_one_undo_step() {
        let mut ed = CodeEditor::new("foo foo foo\n");
        let _ = ed.update(Event::OpenFind, Instant::now());
        let _ = ed.update(Event::FindQuery("foo".into()), Instant::now());
        let _ = ed.update(Event::ReplaceText("bar".into()), Instant::now());
        let _ = ed.update(Event::ReplaceAll, Instant::now());
        assert_eq!(ed.document().text().into_owned(), "bar bar bar\n");
        let _ = ed.update(Event::Editor(Action::Undo), Instant::now());
        assert_eq!(ed.document().text().into_owned(), "foo foo foo\n");
    }

    /// A disabled find bar swallows its open chords — the editor stays find-less.
    #[test]
    fn find_disabled_ignores_open() {
        let mut ed = CodeEditor::new("abc\n").find(false);
        let _ = ed.update(Event::OpenFind, Instant::now());
        assert!(!ed.find_open, "find(false) must not open the bar");
    }

    /// A half-typed regex is a NORMAL invalid state (not "no results") — the bar
    /// reports the pattern error rather than implying the document lacks a match.
    #[test]
    fn half_typed_regex_reports_invalid() {
        let mut ed = CodeEditor::new("abc\n");
        let _ = ed.update(Event::OpenFind, Instant::now());
        let _ = ed.update(Event::ToggleRegex, Instant::now());
        let _ = ed.update(Event::FindQuery("(".into()), Instant::now()); // unbalanced while typing
        assert!(
            ed.document().find_pattern_error().is_some(),
            "a half-typed regex is a normal invalid state, surfaced as a pattern error",
        );
    }

    /// Find-in-selection scopes matches to the selection: the third `foo` outside
    /// the selected span is not counted.
    #[test]
    fn find_in_selection_scopes_the_matches() {
        let mut ed = CodeEditor::new("foo foo foo\n");
        let _ = ed.update(Event::OpenFind, Instant::now());
        let _ = ed.update(Event::Editor(Action::DragSelect {
            granularity: scrive_core::Granularity::Char,
            origin: 0,
            head: 7, // "foo foo"
        }), Instant::now());
        let _ = ed.update(Event::ToggleFindInSelection, Instant::now());
        assert!(ed.document().find_scope().is_some(), "the toggle sets the document scope");
        let _ = ed.update(Event::FindQuery("foo".into()), Instant::now());
        assert_eq!(ed.document().find_match_count(), 2, "matches are scoped to the selection");
    }

    /// The replace button NAVIGATES to a match before it overwrites: the first
    /// press selects, the second replaces (so you always see what you replace).
    #[test]
    fn replace_navigates_before_it_overwrites() {
        let mut ed = CodeEditor::new("foo foo\n");
        let _ = ed.update(Event::OpenFind, Instant::now());
        let _ = ed.update(Event::FindQuery("foo".into()), Instant::now());
        let _ = ed.update(Event::ReplaceText("bar".into()), Instant::now());
        let _ = ed.update(Event::ReplaceOne, Instant::now());
        assert_eq!(ed.document().text().into_owned(), "foo foo\n", "first press only navigates");
        let _ = ed.update(Event::ReplaceOne, Instant::now());
        assert_eq!(ed.document().text().into_owned(), "bar foo\n", "second press overwrites the match");
    }

    /// The find chord table maps the global keys (a pure fn, testable without a
    /// window).
    #[test]
    fn find_chords_map_the_global_keys() {
        use iced::keyboard::{key::Named, Key, Modifiers};
        let ignored = iced::event::Status::Ignored;
        assert!(matches!(
            find_chord(&Key::Character("f".into()), Modifiers::CTRL, ignored),
            Some(Event::OpenFind)
        ));
        assert!(matches!(
            find_chord(&Key::Character("h".into()), Modifiers::CTRL, ignored),
            Some(Event::OpenReplace)
        ));
        assert!(matches!(
            find_chord(&Key::Named(Named::Escape), Modifiers::empty(), ignored),
            Some(Event::CloseFind)
        ));
    }

    struct OneCompletion;
    impl Completions for OneCompletion {
        fn complete(&mut self, _cx: &CompletionCx) -> Vec<scrive_core::CompletionItem> {
            vec![scrive_core::CompletionItem::plain("hello", scrive_core::CompletionKind::Keyword)]
        }
    }

    /// End-to-end through the relocated intel loop: typing a word char opens the
    /// popup off the injected provider, and accepting it inserts the item as one
    /// edit. Proves `drive_completion` + `accept_completion` are wired to `update`.
    #[test]
    fn typing_opens_and_accepting_inserts_a_completion() {
        let mut ed = CodeEditor::new("").completions(OneCompletion);
        let _ = ed.update(Event::Editor(Action::Type('h')), Instant::now());
        assert!(
            matches!(ed.completion.state(), CompletionState::Open(_)),
            "typing a word char with a provider opens the popup",
        );
        let _ = ed.update(Event::Editor(Action::PopupAccept), Instant::now());
        assert_eq!(ed.document().text().into_owned(), "hello");
    }

    /// No provider ⇒ no popup, even on a word char (the drive loop no-ops).
    #[test]
    fn no_provider_never_opens_a_popup() {
        let mut ed = CodeEditor::new("");
        let _ = ed.update(Event::Editor(Action::Type('h')), Instant::now());
        assert!(matches!(ed.completion.state(), CompletionState::Closed));
    }

    /// The blessed whole-buffer swap replaces the text AND re-tokenizes the
    /// visible document at load — with no `ViewportChanged`. Fails-first against a
    /// `load` that swapped the buffer but left the (grammar-swapped) cache dirty.
    #[cfg(feature = "syntect")]
    #[test]
    fn load_swaps_the_buffer_and_retokenizes() {
        let grammar = SyntaxDef::from_sublime_syntax(GRAMMAR).expect("grammar parses");
        let mut ed = CodeEditor::new("old\n").language(grammar);
        let g2 = SyntaxDef::from_sublime_syntax(GRAMMAR).expect("grammar parses");
        ed.load("brand new content\nsecond line\n", Some(g2.into()));
        assert_eq!(ed.document().text().into_owned(), "brand new content\nsecond line\n");
        assert!(
            ed.document().highlight_frontier().is_none(),
            "load must re-tokenize the visible document",
        );
    }

    #[cfg(feature = "tree-sitter")]
    fn rust_tree_sitter() -> scrive_core::TreeSitterDef {
        scrive_core::TreeSitterDef::new(tree_sitter_rust::LANGUAGE, tree_sitter_rust::HIGHLIGHTS_QUERY)
            .expect("tree-sitter-rust's own query compiles")
    }

    /// The cold-load seed holds for a tree-sitter grammar: a small document
    /// parses within one call's budget, so it is coloured at construction.
    #[test]
    #[cfg(feature = "tree-sitter")]
    fn a_tree_sitter_language_tokenizes_at_load_without_a_viewport_report() {
        let editor = CodeEditor::new("fn main() {}\nlet x = 1;\n").language(rust_tree_sitter());
        assert!(editor.document().highlight_frontier().is_none(), "the seed left the frontier dirty");
        assert!(
            editor.document().highlight_line_spans(0).is_some_and(|spans| !spans.is_empty()),
            "`fn` is coloured",
        );
    }

    /// A tree-sitter document past the pool threshold stays off the pool, which
    /// has no engine for it, and converges through the per-frame sweep.
    #[test]
    #[cfg(feature = "tree-sitter")]
    fn a_large_tree_sitter_document_converges_through_the_sweep_without_a_pool() {
        let big = "fn f() {}\n".repeat(230_000); // ~2.3 MB, over PARALLEL_MIN_BYTES
        let mut ed = CodeEditor::new(big).language(rust_tree_sitter());
        #[cfg(feature = "syntect")]
        {
            if let Some(min) = crate::highlight_pool::PARALLEL_MIN_BYTES {
                assert!(ed.document().buffer().len() >= min, "the document crosses the pool threshold");
            }
            assert!(ed.hl_pool.is_none(), "no pool for a grammar without an engine");
        }
        assert!(ed.document().highlight_frontier().is_some(), "the first parse spans several calls");

        let mut sweeps = 0;
        while ed.document().highlight_frontier().is_some() {
            assert!(sweeps < 5_000, "the sweep did not converge");
            let _ = ed.update(Event::HighlightSweep, Instant::now());
            sweeps += 1;
        }
        #[cfg(feature = "syntect")]
        assert!(ed.hl_pool.is_none(), "the sweep never started a pool");
        assert!(ed.document().highlight_line_spans(0).is_some_and(|spans| !spans.is_empty()));
    }

    /// Loads a 120,000-row document while the widget reports rows
    /// 100,000..100,036, sweeps to convergence, and checks those rows are
    /// coloured. No fresh viewport report follows a load, so the seed must aim
    /// at the one already reported.
    #[cfg(any(feature = "syntect", feature = "tree-sitter"))]
    fn assert_load_colors_the_reported_viewport(grammar: Grammar) {
        let mut ed = CodeEditor::new("old\n");
        let _ = ed.update(Event::Editor(Action::ViewportChanged(100_000..100_036)), Instant::now());
        ed.load("fn f() {}\n".repeat(120_000), Some(grammar));
        let mut sweeps = 0;
        while ed.document().highlight_frontier().is_some() {
            assert!(sweeps < 5_000, "the sweep did not converge");
            let _ = ed.update(Event::HighlightSweep, Instant::now());
            sweeps += 1;
        }
        assert!(
            ed.document().highlight_line_spans(100_000).is_some_and(|spans| !spans.is_empty()),
            "the reported viewport is coloured",
        );
    }

    #[cfg(feature = "syntect")]
    #[test]
    fn load_colors_the_reported_viewport_with_syntect() {
        assert_load_colors_the_reported_viewport(SyntaxDef::from_sublime_syntax(GRAMMAR).expect("grammar parses").into());
    }

    #[test]
    #[cfg(feature = "tree-sitter")]
    fn load_colors_the_reported_viewport_with_tree_sitter() {
        assert_load_colors_the_reported_viewport(rust_tree_sitter().into());
    }

    /// `load` with a tree-sitter grammar swaps the backend out from under a
    /// syntect one and re-seeds it.
    #[test]
    #[cfg(all(feature = "syntect", feature = "tree-sitter"))]
    fn load_swaps_a_syntect_grammar_for_tree_sitter() {
        let grammar = SyntaxDef::from_sublime_syntax(GRAMMAR).expect("grammar parses");
        let mut ed = CodeEditor::new("old\n").language(grammar);
        assert!(ed.document().highlight_engine().is_some(), "syntect has a pool engine");

        ed.load("fn main() {}\n", Some(rust_tree_sitter().into()));
        assert!(ed.document().highlight_engine().is_none(), "the tree-sitter backend replaced syntect");
        assert!(ed.document().highlight_frontier().is_none(), "load re-tokenized the document");
        assert!(ed.document().highlight_line_spans(0).is_some_and(|spans| !spans.is_empty()));
    }

    /// The `bracket_lexing` builder wires through to the document's bracket
    /// matching: a bracket inside a string is not counted, so it is not coloured,
    /// folded, or indent-guided.
    #[cfg(feature = "syntect")]
    #[test]
    fn bracket_lexing_skips_in_string_brackets() {
        let grammar = SyntaxDef::from_sublime_syntax(GRAMMAR).expect("grammar parses");
        // `let s = "(";\nf(x);\n` — the ( at 9 is inside the string; f(x)'s ( ) code.
        let ed = CodeEditor::new("let s = \"(\";\nf(x);\n")
            .language(grammar)
            .bracket_lexing(vec![b'"'], None);
        let offs: Vec<u32> = ed.document().brackets().all().iter().map(|b| b.offset).collect();
        assert_eq!(offs, vec![14, 16], "the ( inside the string is skipped; f(x) counts");
    }

    /// A programmatic edit runs the tail (applies, marks dirty).
    #[test]
    fn edit_applies_and_marks_dirty() {
        let mut ed = CodeEditor::new("abc\n");
        ed.edit(vec![scrive_core::EditOp::new(0..0, "X")]);
        assert_eq!(ed.document().text().into_owned(), "Xabc\n");
        assert!(ed.is_dirty());
    }

    /// `try_edit` reports an overlapping batch instead of dropping it, and applies nothing.
    #[test]
    fn try_edit_reports_an_overlapping_batch() {
        let mut ed = CodeEditor::new("abcd\n");
        let result = ed.try_edit(vec![EditOp::new(0..3, "x"), EditOp::new(1..4, "y")]);
        assert!(matches!(result, Err(TransactionError::Overlap { .. })), "the overlap is reported");
        assert_eq!(ed.document().text().into_owned(), "abcd\n", "nothing was applied");
        assert!(!ed.is_dirty(), "a rejected batch doesn't dirty the document");
    }

    /// Async completions, end to end: with no sync provider, typing a word char
    /// records a request; the host fulfills it and ingests the result, which opens
    /// the popup; accepting inserts the item.
    #[test]
    fn async_completion_request_and_ingest_round_trip() {
        let mut ed = CodeEditor::new("");
        let _ = ed.update(Event::Editor(Action::Type('h')), Instant::now());
        let req = ed.take_completion_request().expect("a word char records an async request");
        ed.set_completions(
            req.ticket(),
            vec![CompletionItem::plain("hello", scrive_core::CompletionKind::Keyword)],
        );
        assert!(
            matches!(ed.completion.state(), CompletionState::Open(_)),
            "ingesting items at the current revision opens the popup",
        );
        let _ = ed.update(Event::Editor(Action::PopupAccept), Instant::now());
        assert_eq!(ed.document().text().into_owned(), "hello");
    }

    /// A reply is dropped once a newer request has superseded it and the
    /// buffer has moved past the revision it was computed for.
    #[test]
    fn stale_set_completions_is_dropped() {
        let mut ed = CodeEditor::new("");
        let _ = ed.update(Event::Editor(Action::Type('h')), Instant::now());
        let req = ed.take_completion_request().unwrap();
        // The buffer moves on before the async result arrives.
        let _ = ed.update(Event::Editor(Action::Type('i')), Instant::now());
        ed.set_completions(
            req.ticket(),
            vec![CompletionItem::plain("hello", scrive_core::CompletionKind::Keyword)],
        );
        assert!(
            matches!(ed.completion.state(), CompletionState::Closed),
            "a stale completion result must be dropped, not shown",
        );
    }

    /// Snippets ride completions with no async-specific work: an async-ingested
    /// snippet item, once accepted, starts an interactive tab-stop session.
    #[test]
    fn async_snippet_item_starts_a_session_on_accept() {
        let mut ed = CodeEditor::new("");
        let _ = ed.update(Event::Editor(Action::Type('i')), Instant::now());
        let req = ed.take_completion_request().unwrap();
        let snippet = CompletionItem::new(
            "iflet",
            scrive_core::CompletionKind::Keyword,
            InsertText::Snippet("if ${1:cond} {\n\t$0\n}".into()),
        );
        ed.set_completions(req.ticket(), vec![snippet]);
        assert!(matches!(ed.completion.state(), CompletionState::Open(_)));
        let _ = ed.update(Event::Editor(Action::PopupAccept), Instant::now());
        assert!(
            ed.snippet.is_some(),
            "accepting an async-ingested snippet item starts a tab-stop session",
        );
    }

    /// Typing `(` with no synchronous signature provider records an async
    /// request — the signature twin of the completion seam.
    #[test]
    fn typing_open_paren_records_async_signature_request() {
        let mut ed = CodeEditor::new("");
        let _ = ed.update(Event::Editor(Action::Type('(')), Instant::now());
        assert!(
            ed.take_signature_request().is_some(),
            "'(' with no provider records an async signature request",
        );
    }

    /// Hovering a word with no synchronous hover provider records an async
    /// request — the hover twin of the completion seam.
    #[test]
    fn hover_over_a_word_records_async_request() {
        let mut ed = CodeEditor::new("hello world\n");
        let _ = ed.update(Event::Editor(Action::HoverQuery(2)), Instant::now()); // inside "hello"
        assert!(
            ed.take_hover_request().is_some(),
            "hovering a word with no provider records an async hover request",
        );
    }

    /// A large document (≥ the parallel threshold) spins up the off-thread pool
    /// at load; a small one keeps the synchronous path. This is what stops a huge
    /// buffer from blocking the UI thread tokenizing synchronously.
    #[cfg(feature = "syntect")]
    #[test]
    #[cfg(not(target_arch = "wasm32"))] // no pool without threads
    fn large_document_uses_the_parallel_pool() {
        let grammar = SyntaxDef::from_sublime_syntax(GRAMMAR).expect("grammar parses");
        let big = "fn f() {}\n".repeat(230_000); // ~2.3 MB, over PARALLEL_MIN_BYTES
        let ed = CodeEditor::new(big).language(grammar);
        assert!(ed.hl_pool.is_some(), "a large document spins up the parallel highlight pool");

        let g2 = SyntaxDef::from_sublime_syntax(GRAMMAR).expect("grammar parses");
        let small = CodeEditor::new("fn f() {}\n").language(g2);
        assert!(small.hl_pool.is_none(), "a small document keeps the synchronous path");
    }

    /// `dirty` tracks actual text change: a bare caret move must not dirty the
    /// document (else a host would schedule a needless recompile / save), but
    /// typing does.
    #[test]
    fn caret_move_does_not_dirty_but_typing_does() {
        let mut ed = CodeEditor::new("abc\n");
        let _ = ed.update(Event::Editor(Action::PlaceCaret(1)), Instant::now());
        assert!(!ed.is_dirty(), "a caret move must not dirty the document");
        let _ = ed.update(Event::Editor(Action::Type('x')), Instant::now());
        assert!(ed.is_dirty(), "typing dirties the document");
    }

    /// The change log (LSP `didChange`) is off by default and, once enabled,
    /// logs one entry per commit and drains clean. Full-sync hosts never touch it.
    #[test]
    fn drain_changes_mirrors_edits_when_observing() {
        let mut ed = CodeEditor::new("hello\n");
        let off = ed.drain_changes();
        assert!(off.is_empty() && off.from().is_none(), "the change log is off by default");
        ed.observe_changes(true);
        let _ = ed.update(Event::Editor(Action::Type('X')), Instant::now()); // insert 'X' at the caret (offset 0)
        let changes = ed.drain_changes();
        assert_eq!(changes.len(), 1, "one keystroke logs one commit");
        assert_eq!(changes.doc_id(), ed.document().doc_id(), "the drain names its document");
        let entry = changes.iter().next().expect("one entry");
        assert_eq!(entry.ops()[0].text, "X", "the entry carries the typed text");
        assert_eq!(entry.before().text(), "hello\n", "the entry carries the pre-edit text");
        assert!(ed.drain_changes().is_empty(), "draining clears the log");
    }

    /// Feed one widget action through the real `update` path.
    fn act(ed: &mut CodeEditor, action: Action) {
        let _ = ed.update(Event::Editor(action), Instant::now());
    }

    /// A keyword item with `label` as its insertion.
    fn item(label: &str) -> CompletionItem {
        CompletionItem::plain(label, scrive_core::CompletionKind::Keyword)
    }

    /// The labels the open popup shows, in order (empty when closed).
    fn shown(ed: &CodeEditor) -> Vec<String> {
        match ed.completion.state() {
            CompletionState::Open(list) => {
                list.filtered.iter().map(|&i| list.items[i as usize].label.clone()).collect()
            }
            _ => Vec::new(),
        }
    }

    /// A one-parameter signature for the async signature tests.
    fn sig() -> SignatureInfo {
        SignatureInfo { label: "foo(a)".into(), params: vec![Range { start: 4, end: 5 }], active: 0, doc: None }
    }

    /// A hover card over `hello`.
    fn card() -> HoverInfo {
        HoverInfo { markdown: "doc".into(), range: 0..5 }
    }

    /// A click after a trigger char abandons its request, so the reply is
    /// dropped even though the revision never moved.
    #[test]
    fn a_click_after_a_trigger_char_drops_the_stale_completion() {
        let mut ed = CodeEditor::new("a\n");
        act(&mut ed, Action::PlaceCaret(1));
        act(&mut ed, Action::Type('.'));
        let req = ed.take_completion_request().expect("'.' is a trigger char");
        act(&mut ed, Action::PlaceCaret(0));
        ed.set_completions(req.ticket(), vec![item("len")]);
        assert!(matches!(ed.completion.state(), CompletionState::Closed), "a click abandons the '.' request");
    }

    /// The awaited ticket alone is not enough: a reply computed before the text
    /// moved is dropped.
    #[test]
    fn a_reply_whose_revision_moved_is_dropped_even_with_the_awaited_ticket() {
        let mut ed = CodeEditor::new("hello world\n");
        act(&mut ed, Action::HoverQuery(2));
        let req = ed.take_hover_request().expect("a word with no provider asks");
        act(&mut ed, Action::Type('x'));
        ed.set_hover(req.ticket, Some(card()));
        assert!(ed.hover.is_none(), "typing moved the revision past the request");

        act(&mut ed, Action::HoverQuery(2));
        let req = ed.take_hover_request().expect("the word asks again");
        ed.set_hover(req.ticket, Some(card()));
        assert!(ed.hover.is_some(), "an answer at the request's revision lands");
    }

    /// `HoverDismiss` retires the in-flight hover request.
    #[test]
    fn hover_dismiss_abandons_the_pending_hover() {
        let mut ed = CodeEditor::new("hello world\n");
        act(&mut ed, Action::HoverQuery(2));
        let req = ed.take_hover_request().expect("a word with no provider asks");
        act(&mut ed, Action::HoverDismiss);
        ed.set_hover(req.ticket, Some(card()));
        assert!(ed.hover.is_none(), "a dismissed hover's answer is dropped");
    }

    /// Emptying the word ends the session, so the next word starts fresh.
    #[test]
    fn deleting_back_to_an_empty_word_starts_the_next_request_fresh() {
        let mut ed = CodeEditor::new("");
        act(&mut ed, Action::Type('f'));
        let req = ed.take_completion_request().expect("a word char asks");
        assert_eq!(req.start(), Start::Fresh, "nothing was open or awaited");
        act(&mut ed, Action::Backspace);
        assert!(ed.take_completion_request().is_none(), "an emptied word asks nothing");
        act(&mut ed, Action::Type('g'));
        let req = ed.take_completion_request().expect("a word char asks");
        assert_eq!(req.start(), Start::Fresh, "the emptied word cleared the awaited slot");
    }

    /// A deletion while a completion is awaited (popup not yet open) asks
    /// again for the shorter word.
    #[test]
    fn a_deletion_while_awaited_re_requests_completion() {
        let mut ed = CodeEditor::new("");
        act(&mut ed, Action::Type('f'));
        act(&mut ed, Action::Type('o'));
        let _ = ed.take_completion_request().expect("a word char asks");
        act(&mut ed, Action::Backspace);
        let req = ed.take_completion_request().expect("the deletion re-asks while awaited");
        assert_eq!(req.trigger(), CompletionTrigger::Typed('f'), "keyed by the new last char");
        assert_eq!(req.start(), Start::Continuing, "a completion was awaited");
        assert_eq!(req.word(), 0..1, "the shorter word");
    }

    /// A click away while signature help is awaited re-queries, so the reply
    /// to the `(` request no longer lands.
    #[test]
    fn a_signature_reply_after_a_click_away_is_dropped() {
        let mut ed = CodeEditor::new("x\n");
        act(&mut ed, Action::Type('('));
        let paren = ed.take_signature_request().expect("'(' asks");
        act(&mut ed, Action::PlaceCaret(0));
        assert!(ed.take_signature_request().is_some(), "a move re-queries while awaited");
        ed.set_signature(paren.ticket(), Some(sig()));
        assert!(ed.signature.is_none(), "the reply for the old caret is dropped");
    }

    /// Typing on after `(` before its reply keeps re-querying, and the newest
    /// request's reply opens the box.
    #[test]
    fn typing_through_a_call_before_the_reply_still_opens_the_signature_box() {
        let mut ed = CodeEditor::new("");
        for c in ['f', 'o', 'o', '('] {
            act(&mut ed, Action::Type(c));
        }
        let paren = ed.take_signature_request().expect("'(' asks");
        act(&mut ed, Action::Type('a'));
        let latest = ed.take_signature_request().expect("typing re-queries while awaited");
        ed.set_signature(paren.ticket(), Some(sig()));
        assert!(ed.signature.is_none(), "the '(' reply is stale");
        ed.set_signature(latest.ticket(), Some(sig()));
        assert!(ed.signature.is_some(), "the newest reply opens the box");
    }

    /// Escape with no box showing retires an awaited signature request instead
    /// of re-querying it.
    #[test]
    fn escape_retires_an_awaited_signature_without_re_querying() {
        let mut ed = CodeEditor::new("");
        act(&mut ed, Action::Type('('));
        let paren = ed.take_signature_request().expect("'(' asks");
        act(&mut ed, Action::Collapse);
        assert!(ed.take_signature_request().is_none(), "Escape does not re-query");
        ed.set_signature(paren.ticket(), Some(sig()));
        assert!(ed.signature.is_none(), "the retired request's reply is dropped");
    }

    /// Word chars narrow an open async popup from the items it has, before any
    /// new reply lands, and the next request continues the list.
    #[test]
    fn typing_while_the_popup_is_open_refilters_before_the_reply_lands() {
        let mut ed = CodeEditor::new("");
        act(&mut ed, Action::Type('s'));
        let req = ed.take_completion_request().expect("a word char asks");
        ed.set_completions(req.ticket(), vec![item("send"), item("set")]);
        act(&mut ed, Action::Type('e'));
        act(&mut ed, Action::Type('t'));
        assert_eq!(shown(&ed), ["set"], "the popup narrows locally");
        let req = ed.take_completion_request().expect("typing still asks");
        assert_eq!(req.start(), Start::Continuing, "the popup was open");
    }

    /// An Escape-dismissed popup asks for nothing while the word grows; a
    /// trigger char clears the dismissal and its reply opens the popup.
    #[test]
    fn a_dismissed_popup_asks_for_nothing_until_a_trigger() {
        let mut ed = CodeEditor::new("");
        act(&mut ed, Action::Type('s'));
        let req = ed.take_completion_request().expect("a word char asks");
        ed.set_completions(req.ticket(), vec![item("send")]);
        act(&mut ed, Action::PopupDismiss);
        act(&mut ed, Action::Type('e'));
        assert!(ed.take_completion_request().is_none(), "a dismissed word asks nothing");
        act(&mut ed, Action::Type('.'));
        let req = ed.take_completion_request().expect("a trigger char asks");
        ed.set_completions(req.ticket(), vec![item("send")]);
        assert_eq!(shown(&ed), ["send"], "the trigger's reply opens the popup");
    }

    /// A retrigger item asks the async host for a fresh list after accepting,
    /// and that list opens the popup.
    #[test]
    fn accepting_a_retrigger_item_opens_the_popup_when_the_reply_lands() {
        let mut ed = CodeEditor::new("");
        act(&mut ed, Action::Type('s'));
        let req = ed.take_completion_request().expect("a word char asks");
        ed.set_completions(req.ticket(), vec![item("size=").with_retrigger(true)]);
        act(&mut ed, Action::PopupAccept);
        assert_eq!(ed.document().text(), "size=", "the item is inserted");
        let again = ed.take_completion_request().expect("the retrigger asks the async host");
        assert_eq!(again.trigger(), CompletionTrigger::Manual, "a retrigger is a manual invoke");
        ed.set_completions(again.ticket(), vec![item("8")]);
        assert_eq!(shown(&ed), ["8"], "the retrigger reply opens the popup");
    }

    /// An additional edit above a snippet lands in the same undo step, and the
    /// tab stops sit where the snippet text ended up, not where it would have
    /// been without the import.
    #[test]
    fn an_auto_import_above_a_snippet_keeps_the_tab_stops_on_the_placeholders() {
        let mut ed = CodeEditor::new("\n");
        act(&mut ed, Action::PlaceCaret(1));
        act(&mut ed, Action::Type('i'));
        let req = ed.take_completion_request().expect("a word char asks");
        let snippet = CompletionItem::new(
            "iflet",
            scrive_core::CompletionKind::Keyword,
            InsertText::Snippet("if ${1:cond} {\n\t$0\n}".into()),
        )
        .with_additional(vec![EditOp::insert(0, "use a;\n")]);
        ed.set_completions(req.ticket(), vec![snippet]);
        act(&mut ed, Action::PopupAccept);
        let text = ed.document().text().into_owned();
        assert!(text.starts_with("use a;\n"), "the import landed");
        let cond = text.find("cond").expect("the placeholder is inserted") as u32;
        assert_eq!(ed.selection(), cond..cond + 4, "the first stop sits on its placeholder");
        act(&mut ed, Action::Undo);
        assert_eq!(ed.document().text(), "\ni", "one undo reverts import and insertion together");
    }

    /// Accepting an item marked `signature_after` asks for signature help.
    #[test]
    fn accepting_a_signature_after_item_asks_for_signature_help() {
        let mut ed = CodeEditor::new("");
        act(&mut ed, Action::Type('f'));
        let req = ed.take_completion_request().expect("a word char asks");
        ed.set_completions(req.ticket(), vec![item("foo(").with_signature_after(true)]);
        assert!(ed.take_signature_request().is_none(), "nothing asked for signature help yet");
        act(&mut ed, Action::PopupAccept);
        assert!(ed.take_signature_request().is_some(), "the accept asks for signature help");
    }

    /// Two manual invokes at one revision get distinct tickets, and only the
    /// newer one's reply lands.
    #[test]
    fn two_manual_invokes_at_one_revision_accept_only_the_second() {
        let mut ed = CodeEditor::new("");
        act(&mut ed, Action::TriggerCompletion);
        let first = ed.take_completion_request().expect("Ctrl+Space asks");
        act(&mut ed, Action::TriggerCompletion);
        let second = ed.take_completion_request().expect("Ctrl+Space asks again");
        assert_eq!(first.ticket().revision(), second.ticket().revision(), "same revision");
        assert_eq!(second.trigger(), CompletionTrigger::Manual, "a manual invoke");
        assert_eq!((first.start(), second.start()), (Start::Fresh, Start::Continuing), "the second continues the first");
        ed.set_completions(first.ticket(), vec![item("stale")]);
        assert!(shown(&ed).is_empty(), "the superseded reply is dropped");
        ed.set_completions(second.ticket(), vec![item("fresh")]);
        assert_eq!(shown(&ed), ["fresh"], "the newest reply lands");
    }

    /// A signature request names the innermost `(` still open at the caret,
    /// not a call that has closed before it.
    #[test]
    fn signature_request_names_the_innermost_open_paren() {
        let mut ed = CodeEditor::new("");
        for c in ['f', '(', 'g', '(', 'x', ')', ','] {
            act(&mut ed, Action::Type(c));
        }
        let text = ed.document().text().into_owned();
        let outer = text.find('(').expect("the outer call") as u32;
        let req = ed.take_signature_request().expect("typing in a call re-queries while awaited");
        assert_eq!(req.call(), Some(outer), "the closed inner call does not count: {text:?}");
    }

    /// With string lexing on, a `(` inside a string literal on the caret's
    /// line is not the call.
    #[test]
    fn a_paren_inside_a_string_is_not_the_call() {
        let mut ed = CodeEditor::new("g\nf(\"(\", \n").bracket_lexing(vec![b'"'], None);
        act(&mut ed, Action::PlaceCaret(1));
        act(&mut ed, Action::Type('('));
        let _ = ed.take_signature_request();
        let text = ed.document().text().into_owned();
        let call = text.find("f(").expect("the call") as u32 + 1;
        let end = text[call as usize..].find('\n').expect("the call's line ends") as u32 + call;
        act(&mut ed, Action::PlaceCaret(end));
        let req = ed.take_signature_request().expect("a move re-queries while awaited");
        assert_eq!(req.call(), Some(call), "the paren inside the string is skipped");
    }

    /// `select` selects the range, requests a reveal and closes an open popup.
    #[test]
    fn select_selects_reveals_and_closes_the_popup() {
        let mut ed = CodeEditor::new("hello\n");
        act(&mut ed, Action::Type('h'));
        let req = ed.take_completion_request().expect("a word char asks");
        ed.set_completions(req.ticket(), vec![item("hello")]);
        assert!(ed.completion.is_open(), "the reply opens the popup");
        let seq = ed.document().reveal_seq();
        ed.select(0..1);
        assert!(matches!(ed.completion.state(), CompletionState::Closed), "selecting closes the popup");
        assert_eq!(ed.selection(), 0..1, "the range is selected");
        assert!(ed.document().reveal_seq() > seq, "the selection is revealed");
    }

    /// A definition answer for the request's caret selects the range and
    /// reveals it.
    #[test]
    fn goto_definition_selects_the_landed_range_and_reveals_it() {
        let mut ed = CodeEditor::new("fn foo() {}\nfoo();\n");
        act(&mut ed, Action::PlaceCaret(13));
        act(&mut ed, Action::GotoDefinition);
        let req = ed.take_definition_request().expect("F12 asks");
        assert_eq!(req.offset, 13, "asked at the caret");
        let seq = ed.document().reveal_seq();
        ed.set_definition(req.ticket, Some(3..6));
        assert_eq!(ed.selection(), 3..6, "the definition is selected");
        assert!(ed.document().reveal_seq() > seq, "the definition is revealed");
    }

    /// A click before the definition lands abandons the request.
    #[test]
    fn a_click_before_the_definition_lands_drops_it() {
        let mut ed = CodeEditor::new("fn foo() {}\nfoo();\n");
        act(&mut ed, Action::PlaceCaret(13));
        act(&mut ed, Action::GotoDefinition);
        let req = ed.take_definition_request().expect("F12 asks");
        act(&mut ed, Action::PlaceCaret(0));
        ed.set_definition(req.ticket, Some(3..6));
        assert_eq!(ed.selection(), 0..0, "the click abandoned the request");
    }

    /// Typing before the definition lands drops it by revision.
    #[test]
    fn typing_before_the_definition_lands_drops_it() {
        let mut ed = CodeEditor::new("fn foo() {}\nfoo();\n");
        act(&mut ed, Action::PlaceCaret(13));
        act(&mut ed, Action::GotoDefinition);
        let req = ed.take_definition_request().expect("F12 asks");
        act(&mut ed, Action::Type('x'));
        let caret = ed.selection();
        ed.set_definition(req.ticket, Some(3..6));
        assert_eq!(ed.selection(), caret, "the text moved past the request");
    }

    /// An accepted `None` retires the request, so a second answer under the
    /// same ticket is dropped.
    #[test]
    fn a_none_definition_retires_the_request() {
        let mut ed = CodeEditor::new("fn foo() {}\nfoo();\n");
        act(&mut ed, Action::PlaceCaret(13));
        act(&mut ed, Action::GotoDefinition);
        let req = ed.take_definition_request().expect("F12 asks");
        ed.set_definition(req.ticket, None);
        ed.set_definition(req.ticket, Some(3..6));
        assert_eq!(ed.selection(), 13..13, "the retired request's second answer is dropped");
    }

    /// Format asks at the current revision with scrive's indent width and
    /// leaves the text alone.
    #[test]
    fn format_records_a_request_with_the_indent_size() {
        let mut ed = CodeEditor::new("fn f(){}\n");
        act(&mut ed, Action::Format);
        let req = ed.take_format_request().expect("Shift+Alt+F asks");
        assert_eq!(req.tab_size, default_indent_size(), "the editor's indent width");
        assert_eq!(req.ticket.revision(), ed.document().revision(), "asked at the current revision");
        assert_eq!(ed.document().text(), "fn f(){}\n", "asking changes nothing");
    }

    /// The Escape the global bar chord produces, as a focused input sees it.
    fn escape_chord() -> Event {
        use iced::keyboard::{key::Named, Key, Modifiers};
        find_chord(&Key::Named(Named::Escape), Modifiers::empty(), iced::event::Status::Captured)
            .expect("Escape is a bar chord")
    }

    /// F2 opens the field on the symbol under the caret, and Enter asks to
    /// rename that symbol to the typed name, closing the field.
    #[test]
    fn rename_submits_the_typed_name_for_the_symbol_at_the_caret() {
        let mut ed = CodeEditor::new("let foo = 1;\n").rename(true);
        act(&mut ed, Action::PlaceCaret(5));
        act(&mut ed, Action::Rename);
        assert_eq!(ed.rename.as_ref().map(|r| r.text.as_str()), Some("foo"), "seeded with the symbol");
        let _ = ed.update(Event::RenameText("bar".into()), Instant::now());
        let _ = ed.update(Event::SubmitRename, Instant::now());
        let req = ed.take_rename_request().expect("Enter asks");
        assert_eq!((req.offset, req.new_name.as_str()), (5, "bar"), "the symbol at the caret, renamed");
        assert_eq!(req.ticket.revision(), ed.document().revision(), "asked at the current revision");
        assert!(ed.rename.is_none(), "submitting closes the field");

        act(&mut ed, Action::Rename);
        let _ = ed.update(Event::RenameText(String::new()), Instant::now());
        let _ = ed.update(Event::SubmitRename, Instant::now());
        assert!(ed.take_rename_request().is_none(), "an empty name asks nothing");
        assert!(ed.rename.is_none(), "submitting an empty name still closes the field");
    }

    /// Without the opt-in, F2 opens nothing and leaves an open popup alone.
    #[test]
    fn rename_is_ignored_unless_enabled() {
        let mut ed = CodeEditor::new("hello\n");
        act(&mut ed, Action::Type('h'));
        let req = ed.take_completion_request().expect("a word char asks");
        ed.set_completions(req.ticket(), vec![item("hello")]);
        act(&mut ed, Action::Rename);
        assert!(ed.rename.is_none(), "rename is off by default");
        assert!(ed.completion.is_open(), "F2 does not act as a caret move");
    }

    /// An edit underneath the open field closes it, so it can never submit a
    /// name for a stale offset.
    #[test]
    fn rename_closes_when_the_revision_moves() {
        let mut ed = CodeEditor::new("let foo = 1;\n").rename(true);
        act(&mut ed, Action::PlaceCaret(5));
        act(&mut ed, Action::Rename);
        ed.edit(vec![EditOp::insert(0, "x")]);
        assert!(ed.rename.is_none(), "a host edit closes the field");
        let _ = ed.update(Event::SubmitRename, Instant::now());
        assert!(ed.take_rename_request().is_none(), "a closed field asks nothing");
    }

    /// Only one bar is open, and Escape through the bar chord closes the
    /// rename field without reopening find.
    #[test]
    fn escape_through_the_bar_chord_closes_rename_not_find() {
        let mut ed = CodeEditor::new("let foo = 1;\n").rename(true);
        let _ = ed.update(Event::OpenFind, Instant::now());
        act(&mut ed, Action::Rename);
        assert!(!ed.find_open && ed.rename.is_some(), "one bar at a time");
        let _ = ed.update(escape_chord(), Instant::now());
        assert!(ed.rename.is_none(), "Escape closes the rename field");
        assert!(!ed.find_open, "find stays closed");

        let _ = ed.update(Event::OpenFind, Instant::now());
        let _ = ed.update(escape_chord(), Instant::now());
        assert!(!ed.find_open, "with no rename field, Escape closes find");
    }

    /// The bar chords stay live for the rename field when find is disabled.
    #[test]
    fn escape_closes_rename_even_with_find_disabled() {
        let mut ed = CodeEditor::new("let foo = 1;\n").find(false).rename(true);
        act(&mut ed, Action::Rename);
        assert!(ed.rename.is_some(), "F2 opens the field");
        let _ = ed.update(escape_chord(), Instant::now());
        assert!(ed.rename.is_none(), "Escape closes it");
    }

    /// Opening find closes the rename field.
    #[test]
    fn opening_find_closes_rename() {
        let mut ed = CodeEditor::new("let foo = 1;\n").rename(true);
        act(&mut ed, Action::Rename);
        let _ = ed.update(Event::OpenFind, Instant::now());
        assert!(ed.rename.is_none(), "one bar at a time");
        assert!(ed.find_open, "find opened");
    }

    /// An async hover that answers `None` keeps the diagnostics card, and one
    /// that answers docs puts them after the diagnostics.
    #[test]
    fn the_hover_card_keeps_its_diagnostics_when_the_docs_are_none() {
        let mut ed = CodeEditor::new("hello world\n");
        let rev = ed.document().revision();
        let _ = ed.set_diagnostics(rev, vec![Diagnostic::new(0..5, Severity::Error, "unknown name")]);
        act(&mut ed, Action::HoverQuery(2));
        let req = ed.take_hover_request().expect("a word with no provider asks");
        ed.set_hover(req.ticket, None);
        let shown = ed.hover.as_ref().expect("the diagnostics stay");
        assert_eq!(shown.markdown, "**error:** unknown name", "only the diagnostics show");

        act(&mut ed, Action::HoverQuery(2));
        let req = ed.take_hover_request().expect("the word asks again");
        ed.set_hover(req.ticket, Some(card()));
        let shown = ed.hover.as_ref().expect("a card shows");
        assert_eq!(shown.markdown, "**error:** unknown name\n\ndoc", "the docs follow the diagnostics");
    }

    /// A diagnostic's own markup characters are escaped in the card.
    #[test]
    fn diagnostic_messages_are_escaped_in_the_hover_card() {
        let mut ed = CodeEditor::new("hello world\n");
        let rev = ed.document().revision();
        let _ = ed.set_diagnostics(rev, vec![Diagnostic::new(0..5, Severity::Error, "expected *mut T")]);
        act(&mut ed, Action::HoverQuery(2));
        let card = ed.hover.as_ref().expect("the diagnostic shows at once");
        assert!(card.markdown.contains("expected \\*mut T"), "{}", card.markdown);
    }

    // ── inlay hints: scheduler, slots and toggle ──

    /// The pending wait's generation.
    fn wait_gen(ed: &CodeEditor) -> u64 {
        ed.inlays.wait.expect("a fetch is pending").generation
    }

    /// Fire the pending wait and return the request it recorded.
    fn fetch(ed: &mut CodeEditor) -> inlay::Request {
        act(ed, Action::Wake(wait_gen(ed)));
        ed.take_inlay_request().expect("the wake records a request")
    }

    /// Install `hints` in `ed` through a real fetch.
    fn install(ed: &mut CodeEditor, hints: Vec<inlay::Placed>) {
        ed.wait_inlays(INLAY_EDIT_DELAY, None);
        let req = fetch(ed);
        ed.set_inlays(req.ticket(), Some(hints));
    }

    /// An editor over `src` with hints on and `hints` installed.
    fn with_hints(src: &str, hints: Vec<inlay::Placed>) -> CodeEditor {
        let mut ed = CodeEditor::new(src).inlay_hints(true);
        install(&mut ed, hints);
        ed
    }

    /// The `: i32` hint after `x` in `let x = 1;`, keyed `k`: insertable, and
    /// its part 1 (`i32`) links.
    fn type_hint(k: u64) -> inlay::Placed {
        let label = vec![inlay::Part::new(": ", inlay::Link::None), inlay::Part::new("i32", inlay::Link::Jumps)];
        let hint = inlay::Hint::new(inlay::Kind::Type, label, inlay::Key::new(k))
            .expect("non-empty label")
            .insert(inlay::Insert::Available);
        inlay::Placed::new(5, hint)
    }

    /// How many hints the document shows.
    fn hint_count(ed: &CodeEditor) -> usize {
        ed.document().inlays_in(0..ed.document().buffer().len()).count()
    }

    /// The source the slot tests share.
    const LET_X: &str = "let x = 1;\n";

    /// The ticket of the pending gesture, taken.
    fn gesture_ticket(ed: &mut CodeEditor) -> Ticket {
        ed.take_inlay_interaction().expect("a gesture is recorded").ticket()
    }

    /// A new editor asks for no hints, and typing doesn't either.
    #[test]
    fn hints_are_off_by_default_and_ask_for_nothing() {
        let mut ed = CodeEditor::new("a\n");
        assert!(ed.inlays.wait.is_none(), "no wait while off");
        act(&mut ed, Action::Type('b'));
        assert!(ed.inlays.wait.is_none(), "typing schedules nothing while off");
        assert!(ed.take_inlay_request().is_none(), "nothing to fetch");
    }

    /// Turning hints on asks at once, and the wake records a request for the
    /// window at the current revision.
    #[test]
    fn enabling_hints_waits_zero_and_a_wake_records_a_request_for_the_window() {
        let src = format!("{}x", "x\n".repeat(9));
        let mut ed = CodeEditor::new(src.as_str()).inlay_hints(true);
        let wait = ed.inlays.wait.expect("enabling schedules a fetch");
        assert_eq!((wait.delay, wait.cap), (Duration::ZERO, None), "at once, uncapped");
        let req = fetch(&mut ed);
        assert_eq!(req.ticket().revision(), ed.document().revision(), "at the current revision");
        assert_eq!(req.span(), 0..src.len() as u32, "the whole ten-line document");
    }

    /// The window is the viewport padded by its height above and twice its
    /// height below, clipped to the document and at least 50 rows.
    #[test]
    fn the_request_window_pads_one_view_above_and_two_below() {
        let mut ed = CodeEditor::new("x\n".repeat(1000)).inlay_hints(true);
        act(&mut ed, Action::ViewportChanged(400..436));
        assert_eq!(fetch(&mut ed).span(), 2 * 364..2 * 508, "rows 364..508");
        act(&mut ed, Action::ViewportChanged(980..1001));
        assert_eq!(fetch(&mut ed).span(), 2 * 951..2000, "rows 951..1001, the last 50");
    }

    /// A wake for a superseded generation records nothing.
    #[test]
    fn a_wake_for_an_old_generation_records_nothing() {
        let mut ed = CodeEditor::new("a\n").inlay_hints(true);
        let g = wait_gen(&ed);
        act(&mut ed, Action::Type('b'));
        assert_eq!(wait_gen(&ed), g + 1, "the edit restarts the wait");
        act(&mut ed, Action::Wake(g));
        assert!(ed.take_inlay_request().is_none(), "the old generation asks nothing");
        act(&mut ed, Action::Wake(g + 1));
        assert!(ed.take_inlay_request().is_some(), "the current one asks");
    }

    /// An edit waits 300 ms, uncapped, and each edit restarts the wait; a
    /// caret move is no edit.
    #[test]
    fn an_edit_waits_three_hundred_ms_and_each_edit_restarts_it() {
        let mut ed = CodeEditor::new("a\n").inlay_hints(true);
        let _ = fetch(&mut ed);
        act(&mut ed, Action::Type('b'));
        let wait = ed.inlays.wait.expect("the edit schedules a fetch");
        assert_eq!((wait.delay, wait.cap), (INLAY_EDIT_DELAY, None), "300 ms, no cap");
        act(&mut ed, Action::Type('c'));
        assert_eq!(wait_gen(&ed), wait.generation + 1, "the next edit restarts it");
        act(&mut ed, Action::PlaceCaret(0));
        assert_eq!(wait_gen(&ed), wait.generation + 1, "a caret move changes nothing");
    }

    /// Accepting a completion edits outside the post-edit tail, and still
    /// schedules a fetch.
    #[test]
    fn accepting_a_completion_schedules_a_fetch() {
        let mut ed = CodeEditor::new("").completions(OneCompletion).inlay_hints(true);
        let _ = fetch(&mut ed);
        act(&mut ed, Action::Type('h'));
        let typed = wait_gen(&ed);
        act(&mut ed, Action::PopupAccept);
        assert_eq!(ed.document().text().into_owned(), "hello", "the completion landed");
        assert!(wait_gen(&ed) > typed, "the accept restarts the wait");
    }

    /// A trigger outside `update` (a server's refresh) leaves a delay with no
    /// clock in it, which the widget times from the frame that first sees it.
    #[test]
    fn a_refresh_wait_is_a_delay_not_a_deadline() {
        let mut ed = CodeEditor::new("a\n").inlay_hints(true);
        let _ = fetch(&mut ed);
        ed.wait_inlays(INLAY_EDIT_DELAY, None);
        let wait = ed.inlays.wait.expect("the refresh schedules a fetch");
        assert_eq!((wait.delay, wait.cap), (INLAY_EDIT_DELAY, None), "a plain 300 ms delay");
    }

    /// Scrolling within the inner half of the last window asks nothing.
    #[test]
    fn scrolling_inside_the_inner_window_asks_nothing() {
        let mut ed = CodeEditor::new("x\n".repeat(1000)).inlay_hints(true);
        act(&mut ed, Action::ViewportChanged(0..36));
        let _ = fetch(&mut ed);
        act(&mut ed, Action::ViewportChanged(10..46));
        assert!(ed.inlays.wait.is_none(), "still inside rows 0..72");
    }

    /// Scrolling out of the inner half waits 75 ms with a 300 ms cap, and the
    /// fetch covers the new window.
    #[test]
    fn scrolling_out_of_the_inner_window_waits_with_a_cap_and_re_requests() {
        let mut ed = CodeEditor::new("x\n".repeat(1000)).inlay_hints(true);
        act(&mut ed, Action::ViewportChanged(0..36));
        let _ = fetch(&mut ed);
        act(&mut ed, Action::ViewportChanged(40..76));
        let wait = ed.inlays.wait.expect("leaving the inner rows schedules a fetch");
        assert_eq!((wait.delay, wait.cap), (INLAY_SCROLL_DELAY, Some(INLAY_SCROLL_CAP)), "75 ms, capped at 300");
        assert_eq!(fetch(&mut ed).span().start, 2 * 4, "the window starts one view above, at row 4");
    }

    /// While hints are off no trigger schedules a fetch.
    #[test]
    fn triggers_are_ignored_while_disabled() {
        let mut ed = CodeEditor::new("x\n".repeat(100));
        act(&mut ed, Action::Type('a'));
        act(&mut ed, Action::ViewportChanged(60..90));
        ed.load("y\n", None);
        ed.wait_inlays(INLAY_EDIT_DELAY, None);
        assert!(ed.inlays.wait.is_none(), "nothing is pending");
    }

    /// An answer lands only under the ticket the editor still awaits, and
    /// only once.
    #[test]
    fn set_inlays_lands_only_under_the_awaited_ticket() {
        let mut ed = CodeEditor::new(LET_X).inlay_hints(true);
        let t1 = fetch(&mut ed).ticket();
        ed.wait_inlays(INLAY_EDIT_DELAY, None);
        let t2 = fetch(&mut ed).ticket();
        ed.set_inlays(t1, Some(vec![type_hint(1)]));
        assert_eq!(ed.document().inlays_revision(), None, "a superseded answer is dropped");
        ed.set_inlays(t2, Some(vec![type_hint(1)]));
        assert_eq!(ed.document().inlays_revision(), Some(ed.document().revision()), "the awaited answer installs");
        ed.set_inlays(t2, Some(Vec::new()));
        assert_eq!(hint_count(&ed), 1, "a settled ticket lands nothing");
    }

    /// A failed fetch settles its slot and keeps the hints on screen.
    #[test]
    fn a_failed_fetch_keeps_the_shown_hints() {
        let mut ed = with_hints(LET_X, vec![type_hint(1)]);
        ed.wait_inlays(INLAY_EDIT_DELAY, None);
        let t = fetch(&mut ed).ticket();
        ed.set_inlays(t, None);
        assert_eq!(hint_count(&ed), 1, "the hint stays");
        assert!(ed.awaiting.inlays.is_none(), "the slot is settled");
    }

    /// An empty answer clears the hints.
    #[test]
    fn an_empty_answer_clears_the_hints() {
        let mut ed = with_hints(LET_X, vec![type_hint(1)]);
        ed.wait_inlays(INLAY_EDIT_DELAY, None);
        let t = fetch(&mut ed).ticket();
        ed.set_inlays(t, Some(Vec::new()));
        assert_eq!(hint_count(&ed), 0, "no hints");
    }

    /// The wake bypasses the post-edit tail, so the completion popup stays
    /// open across a typing pause.
    #[test]
    fn a_wake_keeps_the_completion_popup_open() {
        let mut ed = CodeEditor::new("").completions(OneCompletion).inlay_hints(true);
        act(&mut ed, Action::Type('h'));
        assert!(matches!(ed.completion.state(), CompletionState::Open(_)), "typing opens the popup");
        let g = wait_gen(&ed);
        act(&mut ed, Action::Wake(g));
        assert!(matches!(ed.completion.state(), CompletionState::Open(_)), "the wake keeps it open");
        assert!(ed.take_inlay_request().is_some(), "and records the fetch");
    }

    /// No hint action runs the post-edit tail: the popup and every slot they
    /// fill survive.
    #[test]
    fn inlay_actions_never_reach_apply() {
        let mut ed = CodeEditor::new(LET_X).completions(OneCompletion).inlay_hints(true);
        install(&mut ed, vec![type_hint(1)]);
        act(&mut ed, Action::TriggerCompletion);
        assert!(matches!(ed.completion.state(), CompletionState::Open(_)), "Ctrl+Space opens the popup");
        let key = inlay::Key::new(1);
        act(&mut ed, Action::InlayJump { key, part: 1 });
        let jump = gesture_ticket(&mut ed);
        act(&mut ed, Action::InlayHover { key, part: 1 });
        let hover = gesture_ticket(&mut ed);
        act(&mut ed, Action::InlayInsert { key, offset: 5 });
        act(&mut ed, Action::Wake(0));
        assert!(matches!(ed.completion.state(), CompletionState::Open(_)), "the popup stays open");
        assert!(ed.accepts(Awaited::Definition, jump), "the jump still awaits its definition");
        assert!(ed.accepts(Awaited::InlayTooltip, hover), "the tooltip still awaits");
    }

    /// Hovering a hint asks for its part's tooltip, and the answer shows a
    /// card keyed to that part in place of the word hover.
    #[test]
    fn inlay_hover_records_a_tooltip_interaction_and_the_answer_shows_a_keyed_card() {
        let mut ed = with_hints(LET_X, vec![type_hint(1)]);
        let key = inlay::Key::new(1);
        act(&mut ed, Action::InlayHover { key, part: 1 });
        let gesture = ed.take_inlay_interaction().expect("the hover records a gesture");
        assert_eq!((gesture.key(), gesture.gesture()), (key, inlay::interaction::Gesture::Tooltip { part: 1 }), "part 1");
        ed.set_inlay_tooltip(gesture.ticket(), Some("**i32**".into()));
        let card = ed.inlay_card.as_ref().expect("the tooltip shows");
        assert_eq!((card.key, card.part), (key, 1), "keyed to the hovered part");
        assert!(ed.hover.is_none(), "one card at a time");

        act(&mut ed, Action::InlayHover { key, part: 1 });
        let t = gesture_ticket(&mut ed);
        ed.set_inlay_tooltip(t, None);
        assert!(ed.inlay_card.is_none(), "no tooltip, no card");
    }

    /// Show the `i32` tooltip card on `ed`.
    fn show_card(ed: &mut CodeEditor) {
        act(ed, Action::InlayHover { key: inlay::Key::new(1), part: 1 });
        let t = gesture_ticket(ed);
        ed.set_inlay_tooltip(t, Some("**i32**".into()));
        assert!(ed.inlay_card.is_some(), "the card shows");
    }

    /// A refetch that keeps the card's hint keeps the card.
    #[test]
    fn the_tooltip_card_survives_a_refetch_that_keeps_its_key() {
        let mut ed = with_hints(LET_X, vec![type_hint(1)]);
        show_card(&mut ed);
        install(&mut ed, vec![type_hint(1)]);
        assert!(ed.inlay_card.is_some(), "the hint is still there, so is its card");
    }

    /// A refetch without the card's hint closes the card.
    #[test]
    fn a_refetch_without_the_key_closes_the_tooltip_card() {
        let mut ed = with_hints(LET_X, vec![type_hint(1)]);
        show_card(&mut ed);
        install(&mut ed, vec![type_hint(2)]);
        assert!(ed.inlay_card.is_none(), "the card's hint is gone");
    }

    /// A label jump awaits through the definition slot, so its answer lands
    /// like a goto-definition.
    #[test]
    fn inlay_jump_awaits_through_the_definition_slot() {
        let mut ed = with_hints(LET_X, vec![type_hint(1)]);
        act(&mut ed, Action::InlayJump { key: inlay::Key::new(1), part: 1 });
        let gesture = ed.take_inlay_interaction().expect("the jump records a gesture");
        assert_eq!(gesture.gesture(), inlay::interaction::Gesture::Jump { part: 1 }, "a jump through part 1");
        assert!(ed.accepts(Awaited::Definition, gesture.ticket()), "awaited as a definition");
        ed.set_definition(gesture.ticket(), Some(0..3));
        assert_eq!(ed.selection(), 0..3, "the target is selected");
    }

    /// A double-click insert records its hint and offset.
    #[test]
    fn inlay_insert_records_an_insert_interaction() {
        let mut ed = with_hints(LET_X, vec![type_hint(1)]);
        let key = inlay::Key::new(1);
        act(&mut ed, Action::InlayInsert { key, offset: 5 });
        let gesture = ed.take_inlay_interaction().expect("the insert records a gesture");
        assert_eq!(gesture.gesture(), inlay::interaction::Gesture::Insert { offset: 5 }, "an insert at 5");
        assert_eq!(ed.awaiting.inlay_insert, Some((gesture.ticket(), key, 5)), "the slot holds the hint");
    }

    /// The gesture slot holds the newest gesture; the older one's own slot
    /// keeps waiting.
    #[test]
    fn a_newer_gesture_supersedes_the_pending_interaction() {
        let mut ed = with_hints(LET_X, vec![type_hint(1)]);
        let key = inlay::Key::new(1);
        act(&mut ed, Action::InlayHover { key, part: 1 });
        let hover = ed.awaiting.inlay_tooltip.map(|(t, ..)| t).expect("the tooltip awaits");
        act(&mut ed, Action::InlayInsert { key, offset: 5 });
        let gesture = ed.take_inlay_interaction().expect("a gesture is pending");
        assert_eq!(gesture.gesture(), inlay::interaction::Gesture::Insert { offset: 5 }, "the insert replaced the hover");
        assert!(ed.accepts(Awaited::InlayTooltip, hover), "the tooltip slot still awaits");
    }

    /// Once the text moved past the set, gestures on it record nothing.
    #[test]
    fn interactions_on_a_stale_set_record_nothing() {
        let mut ed = with_hints(LET_X, vec![type_hint(1)]);
        act(&mut ed, Action::PlaceCaret(LET_X.len() as u32));
        act(&mut ed, Action::Type('a'));
        let key = inlay::Key::new(1);
        for action in [Action::InlayHover { key, part: 1 }, Action::InlayJump { key, part: 1 }, Action::InlayInsert { key, offset: 5 }] {
            act(&mut ed, action);
        }
        assert!(ed.take_inlay_interaction().is_none(), "no gesture");
        assert!(ed.awaiting.inlay_tooltip.is_none() && ed.awaiting.inlay_insert.is_none(), "no inlay slot");
        assert!(ed.awaiting.definition.is_none(), "no jump");
    }

    /// Turning hints off clears the hints, the slots, the card and the wait.
    #[test]
    fn disabling_hints_clears_the_store_the_slots_the_card_and_the_wait() {
        let mut ed = with_hints(LET_X, vec![type_hint(1)]);
        show_card(&mut ed);
        act(&mut ed, Action::InlayInsert { key: inlay::Key::new(1), offset: 5 });
        act(&mut ed, Action::InlayHover { key: inlay::Key::new(1), part: 0 });
        ed.wait_inlays(INLAY_EDIT_DELAY, None);
        let _ = fetch(&mut ed);
        ed.wait_inlays(INLAY_EDIT_DELAY, None);
        ed.set_inlay_hints(false);
        assert_eq!(hint_count(&ed), 0, "no hints");
        assert!(ed.inlay_card.is_none(), "no card");
        assert!(ed.awaiting.inlays.is_none(), "no fetch awaited");
        assert!(ed.awaiting.inlay_tooltip.is_none() && ed.awaiting.inlay_insert.is_none(), "no gesture awaited");
        assert!(ed.inlays.wait.is_none(), "no wait");
        assert!(ed.take_inlay_interaction().is_none(), "no gesture to pull");
    }

    /// Leaving the hint retires its tooltip query.
    #[test]
    fn hover_dismiss_retires_the_inlay_tooltip() {
        let mut ed = with_hints(LET_X, vec![type_hint(1)]);
        act(&mut ed, Action::InlayHover { key: inlay::Key::new(1), part: 1 });
        let t = gesture_ticket(&mut ed);
        act(&mut ed, Action::HoverDismiss);
        ed.set_inlay_tooltip(t, Some("**i32**".into()));
        assert!(ed.inlay_card.is_none(), "the late answer shows nothing");
    }

    /// Scrolling retires a tooltip query and closes a shown card.
    #[test]
    fn scrolling_retires_the_inlay_tooltip() {
        let mut ed = with_hints(LET_X, vec![type_hint(1)]);
        act(&mut ed, Action::InlayHover { key: inlay::Key::new(1), part: 1 });
        let t = gesture_ticket(&mut ed);
        act(&mut ed, Action::ViewportChanged(0..2));
        ed.set_inlay_tooltip(t, Some("**i32**".into()));
        assert!(ed.inlay_card.is_none(), "the late answer shows nothing");
        show_card(&mut ed);
        act(&mut ed, Action::ViewportChanged(0..3));
        assert!(ed.inlay_card.is_none(), "scrolling closes the card");
    }

    /// An edit retires the tooltip query and the insert, and closes the card.
    #[test]
    fn an_edit_retires_the_inlay_tooltip_and_insert() {
        let mut ed = with_hints(LET_X, vec![type_hint(1)]);
        let key = inlay::Key::new(1);
        show_card(&mut ed);
        act(&mut ed, Action::InlayInsert { key, offset: 5 });
        let insert = gesture_ticket(&mut ed);
        act(&mut ed, Action::InlayHover { key, part: 0 });
        act(&mut ed, Action::Type('a'));
        assert!(ed.awaiting.inlay_tooltip.is_none(), "the tooltip query is retired");
        assert!(ed.awaiting.inlay_insert.is_none(), "the insert is retired");
        assert!(!ed.accepts(Awaited::InlayInsert, insert), "its answer can't land");

        let mut ed = with_hints(LET_X, vec![type_hint(1)]);
        show_card(&mut ed);
        act(&mut ed, Action::Type('a'));
        assert!(ed.inlay_card.is_none(), "the edit closes the card");
    }

    /// Loading a new buffer drops the old hints and asks at once.
    #[test]
    fn load_clears_the_hints_and_waits_zero() {
        let mut ed = with_hints(LET_X, vec![type_hint(1)]);
        ed.load("new\n", None);
        assert_eq!(hint_count(&ed), 0, "the old hints are gone");
        assert_eq!(ed.inlays.wait.map(|w| w.delay), Some(Duration::ZERO), "the new text is fetched at once");
    }

    /// `pending_wake` is the wait `view` hands the widget, and waking it
    /// records the fetch.
    #[test]
    fn pending_wake_is_the_wait_the_widget_is_handed() {
        assert_eq!(CodeEditor::new("a\n").pending_wake(), None, "off: no wait");
        let mut ed = CodeEditor::new("a\n").inlay_hints(true);
        let wake = ed.pending_wake().expect("on: a wait");
        assert_eq!((wake.delay, wake.cap), (Duration::ZERO, None), "at once");
        assert_eq!(Some(wake), ed.inlays.wait, "the scheduler's own wait");
        act(&mut ed, Action::Wake(wake.generation));
        assert!(ed.take_inlay_request().is_some(), "the wake records a request");
        assert_eq!(ed.pending_wake(), None, "and settles the wait");
    }

    /// With a block fold on screen, the pads count display rows: the window
    /// reaches past the fold by the visible height, not by its buffer rows.
    #[test]
    fn a_folded_viewport_pads_its_window_in_display_rows() {
        let mut lines = vec!["x"; 1001];
        lines[10] = "{";
        lines[500] = "}";
        let src = lines.join("\n");
        let mut ed = CodeEditor::new(src.as_str()).inlay_hints(true);
        let opener = src.find('{').expect("the block opens") as u32;
        act(&mut ed, Action::ToggleFold { opener });
        act(&mut ed, Action::ViewportChanged(0..520));
        let span = fetch(&mut ed).span();
        let row_start = |row: u32| ed.document().buffer().point_to_offset(Point::new(row, 0));
        assert_eq!(span, 0..row_start(580), "display rows 0..90 are buffer rows 0..580, the fold included");
        assert_eq!(ed.inlays.window, Some(0..550), "the inner window ends at display row 60");
    }
}
