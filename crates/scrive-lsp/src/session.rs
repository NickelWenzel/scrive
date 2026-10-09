//! The LSP state machine: from JSON-RPC messages and document snapshots to messages to send and
//! updates to apply. It does no I/O, reads no clock and spawns nothing.

pub(crate) mod capabilities;
#[cfg(test)]
mod tests;

use core::ops::Range;
use std::collections::HashMap;
use std::sync::Arc;

use lsp_types::error_codes::{CONTENT_MODIFIED, REQUEST_CANCELLED, SERVER_CANCELLED};
use lsp_types::{
    ApplyWorkspaceEditResponse, CompletionContext, CompletionParams,
    CompletionTriggerKind, ConfigurationParams, DidChangeConfigurationParams,
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams,
    DocumentFormattingParams, FormattingOptions, GotoDefinitionParams, HoverParams,
    InitializeParams, InitializeResult, InitializedParams, InlayHintParams,
    PublishDiagnosticsParams, RenameParams, SignatureHelpParams, TextDocumentContentChangeEvent,
    TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams, TextDocumentSyncKind,
    Uri, VersionedTextDocumentIdentifier,
};
use scrive_core::{
    document, intel, Bias, CompletionRequest, CompletionTrigger, DefinitionRequest, DocId,
    FormatRequest, HoverRequest, RenameRequest, Revision, SignatureRequest, Snapshot, Ticket,
};
use serde_json::Value;

use crate::client::{error, Error};
use crate::message::{self, Message};
use crate::update::{self, jump, Update};
use crate::{
    completion, diagnostics, edits, hover, inlay, log, signature, uri, workspace, Encoding,
};

/// JSON-RPC's "method not found": a server request this client does not implement.
const METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC's "invalid params": a server request whose params do not decode.
const INVALID_PARAMS: i64 = -32602;

/// One server conversation: the documents it knows, the requests in flight and the negotiated
/// capabilities. Every entry point returns what to send and what to apply.
#[derive(Debug)]
pub(crate) struct Session {
    state: State,
    encoding: Encoding,
    /// The `initialize` params as sent; `workspace/workspaceFolders` is answered from them.
    initialize: InitializeParams,
    configuration: Option<Value>,
    /// In registration order, so the deferred `didOpen`s go out in a deterministic order.
    tracked: Vec<Tracked>,
    /// Version high-water marks. They survive `close`, so a reopened document continues its
    /// count and a late publish for the old incarnation never matches the new one.
    versions: HashMap<uri::Key, i32>,
    /// The latest unversioned diagnostics for URIs that are not open, applied at `open`.
    cached: HashMap<uri::Key, Vec<lsp_types::Diagnostic>>,
    /// Requests in flight, at most one per document and [`Kind`].
    pending: Vec<Pending>,
    next_request: i64,
    /// The last inlay hint key minted; keys are unique per session.
    next_inlay: u64,
}

/// What an entry point hands back: messages for the transport and updates for the documents.
#[must_use]
#[derive(Debug, Default)]
pub(crate) struct Output {
    /// Messages to send to the server, in order.
    pub(crate) messages: Vec<Message>,
    /// Changes and notifications for the host, in order.
    pub(crate) updates: Vec<Update>,
}

/// The connection lifecycle.
#[derive(Debug)]
enum State {
    /// `initialize` is in flight.
    Initializing { request: message::Id },
    /// Initialized; the server's capabilities are known.
    Running(capabilities::Server),
    /// The server is gone or going: nothing goes out but `null` answers to its requests, and
    /// every request declines.
    Disconnected,
}

/// One registered document.
#[derive(Debug)]
struct Tracked {
    doc_id: DocId,
    key: uri::Key,
    language: String,
    /// What the server has, or will get with the deferred `didOpen`.
    synced: Snapshot,
    /// The version last sent; `None` while the server has not been told about the document.
    version: Option<i32>,
    /// The completion session of the document's latest completion request, which the pending
    /// completion entry, if any, answers.
    session: Option<completion::Session>,
    /// The document's last inlay hint answer, which hint gestures are answered from.
    inlays: Option<inlay::Set>,
}

/// One request in flight.
#[derive(Debug)]
struct Pending {
    id: message::Id,
    doc_id: DocId,
    /// What the request's positions were computed against; the reply converts against it.
    request_snapshot: Snapshot,
    /// The newest editor ticket the reply answers. A continuing request moves it forward.
    latest_ticket: Ticket,
    /// The caret that goes with `latest_ticket`.
    latest_caret: u32,
    query: Query,
    /// The ticket this request was re-issued for after `ContentModified`. Requests that follow
    /// from it inherit it, so each ticket is re-issued at most once.
    reissued_for: Option<Ticket>,
    /// For requests whose results name other documents: the synced revision of every open
    /// document when the request went out. A result pointing into a document that has moved
    /// since, or opened since, is stale.
    revisions: Vec<(uri::Key, Revision)>,
}

/// The kind of a pending request; with the document, it keys the pending table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Completion,
    Signature,
    Hover,
    Definition,
    Rename,
    Format,
    Inlays,
    Tooltip,
}

/// What a pending request asked, with what its reply needs.
#[derive(Debug)]
enum Query {
    Completion(completion::Query),
    Signature(signature::Query),
    Hover(hover::Query),
    /// The definition of the symbol at `offset`.
    Definition { offset: u32 },
    /// A rename of the symbol at `offset` to `new_name`.
    Rename { offset: u32, new_name: String },
    /// Formatting of the whole document at this indent width.
    Format { tab_size: u32 },
    /// The inlay hints over `span`.
    Inlays { span: Range<u32> },
    /// `inlayHint/resolve` of the hint under `key`, for the tooltip of `part`. `hint` is the raw
    /// hint as the server sent it.
    Resolve {
        key: intel::inlay::Key,
        part: u32,
        hint: Value,
    },
    /// `textDocument/hover` at a label part's location, in any document, for a hint tooltip.
    LocationHover {
        uri: Uri,
        position: lsp_types::Position,
    },
}

impl Session {
    /// A session waiting for its `initialize` reply, and that request: id `1`, then every later
    /// request counts up from `2`.
    pub(crate) fn new(initialize: InitializeParams, configuration: Option<Value>) -> (Self, Message) {
        let request = message::Id::Number(1);
        let message = message::Request::new::<lsp_types::request::Initialize>(
            request.clone(),
            initialize.clone(),
        );
        let session = Self {
            state: State::Initializing { request },
            encoding: Encoding::default(),
            initialize,
            configuration,
            tracked: Vec::new(),
            versions: HashMap::new(),
            cached: HashMap::new(),
            pending: Vec::new(),
            next_request: 2,
            next_inlay: 0,
        };
        (session, Message::Request(message))
    }

    /// Whether `initialize` is still unanswered.
    pub(crate) fn initializing(&self) -> bool {
        matches!(self.state, State::Initializing { .. })
    }

    /// Whether the handshake completed and the server is still there.
    pub(crate) fn running(&self) -> bool {
        matches!(self.state, State::Running(_))
    }

    /// The method of the request `id` answers: `initialize`, or a request in flight. `None` for
    /// anything else, such as a cancelled or superseded request.
    pub(crate) fn method(&self, id: &message::Id) -> Option<&'static str> {
        use lsp_types::request::Request;
        if let State::Initializing { request } = &self.state {
            if request == id {
                return Some(lsp_types::request::Initialize::METHOD);
            }
        }
        self.pending
            .iter()
            .find(|p| p.id == *id)
            .map(|p| p.query.method())
    }

    /// Registers the document of `snapshot` under `uri`, and tells the server once it is
    /// initialized. Opening a document that is already registered re-registers it, closing it
    /// first. While the server is not running the document is only registered; the handshake
    /// sends its `didOpen`.
    ///
    /// Diagnostics the server published for `uri` while it was not open arrive in the output's
    /// updates, stamped with the snapshot's revision.
    ///
    /// # Errors
    /// [`Error::DuplicateUri`] when another document is registered under the same normalized URI.
    pub(crate) fn open(
        &mut self,
        snapshot: &Snapshot,
        uri: &Uri,
        language: impl Into<String>,
    ) -> Result<Output, Error> {
        let key = uri::Key::new(uri);
        let doc_id = snapshot.doc_id();
        if self
            .tracked
            .iter()
            .any(|t| t.key == key && t.doc_id != doc_id)
        {
            return Err(Error::DuplicateUri { uri: key });
        }
        let mut output = self.close(doc_id);
        if let Some(cached) = self.cached.remove(&key) {
            output.updates.push(Update::Document(update::Document::new(
                doc_id,
                update::Stamp::Revision(snapshot.revision()),
                update::Change::Diagnostics(diagnostics::convert(self.encoding, snapshot, &cached)),
            )));
        }
        self.tracked.push(Tracked {
            doc_id,
            key,
            language: language.into(),
            synced: snapshot.clone(),
            version: None,
            session: None,
            inlays: None,
        });
        if self.opens_and_closes() {
            let index = self.tracked.len() - 1;
            output.messages.push(self.did_open(index));
        }
        Ok(output)
    }

    /// Forgets a document, cancelling its requests in flight, and sends `didClose` if the server
    /// was told about it. Its version high-water mark stays, so a reopen continues the count.
    pub(crate) fn close(&mut self, doc_id: DocId) -> Output {
        let Some(index) = self.tracked.iter().position(|t| t.doc_id == doc_id) else {
            return Output::default();
        };
        let tracked = self.tracked.remove(index);
        let mut output = Output::default();
        self.pending.retain(|pending| {
            let keep = pending.doc_id != doc_id;
            if !keep {
                output.messages.push(cancel(pending.id.clone()));
            }
            keep
        });
        if tracked.version.is_some() && self.opens_and_closes() {
            output
                .messages
                .push(Message::Notification(message::Notification::new::<
                    lsp_types::notification::DidCloseTextDocument,
                >(
                    DidCloseTextDocumentParams {
                        text_document: TextDocumentIdentifier {
                            uri: tracked.key.uri().clone(),
                        },
                    },
                )));
        }
        output
    }

    /// Answers the editor's completion request: locally from the list of the request it
    /// continues, by adopting it into that request while it is in flight, or with a new
    /// `textDocument/completion` that supersedes the document's previous one.
    ///
    /// A request continues the previous one only while the user types at its caret: a
    /// [`Typed`](CompletionTrigger::Typed) request that is
    /// [`Continuing`](scrive_core::intel::completion::Start::Continuing), at the same word
    /// start, with the text before the old caret unchanged and every byte since typed at the
    /// caret, at most 32 bytes past it, and only while the previous request is in flight or its
    /// list has arrived. A list the server marked incomplete is asked for again.
    ///
    /// When the server cannot be asked (it is not running, has no completion provider, or did not
    /// register the trigger the text before the caret ends with) the request is declined with an
    /// empty [`update::Change::Completions`] under its ticket, so the editor stops waiting. A
    /// request for a document that is not registered gets nothing, and so does one from a
    /// revision other than `snapshot`'s or the last synced one while the server runs: the editor
    /// has moved on. Otherwise a request at or past the synced revision is declined.
    pub(crate) fn complete(&mut self, snapshot: &Snapshot, request: &CompletionRequest) -> Output {
        let doc_id = snapshot.doc_id();
        let ticket = request.ticket();
        let Some(tracked) = self.tracked.iter().find(|t| t.doc_id == doc_id) else {
            return Output::default();
        };
        if !self.answerable(tracked, ticket, snapshot) {
            return Output::default();
        }
        let decline = || Output::answer(doc_id, ticket, update::Change::Completions(Vec::new()));
        let State::Running(server) = &self.state else {
            return decline();
        };
        let Some(triggers) = &server.completion else {
            return decline();
        };
        let word = request.word();
        let mut context = match request.trigger() {
            CompletionTrigger::TriggerChar(_) => {
                match matched_trigger(snapshot, word.end, triggers) {
                    Some(trigger) => CompletionContext {
                        trigger_kind: CompletionTriggerKind::TRIGGER_CHARACTER,
                        trigger_character: Some(trigger),
                    },
                    None => return decline(),
                }
            }
            CompletionTrigger::Typed(_) | CompletionTrigger::Manual => CompletionContext {
                trigger_kind: CompletionTriggerKind::INVOKED,
                trigger_character: None,
            },
        };
        if let Some(session) = tracked
            .session
            .as_ref()
            .filter(|session| session.continues(snapshot, request))
        {
            if let Some(entry) = self
                .pending
                .iter_mut()
                .find(|p| p.doc_id == doc_id && p.query.kind() == Kind::Completion)
            {
                entry.latest_ticket = ticket;
                entry.latest_caret = word.end;
                return Output::default();
            }
            // A session whose request was dropped before its list arrived has nothing to
            // refine, so the request starts afresh with its own context.
            if session.filled() {
                if !session.incomplete() {
                    let items = session.answer(snapshot, word.end);
                    return Output::answer(doc_id, ticket, update::Change::Completions(items));
                }
                context = for_incomplete();
            }
        }
        self.request_completion(doc_id, ticket, word, context, None)
    }

    /// Answers the editor's signature request with a `textDocument/signatureHelp` at its caret.
    ///
    /// While the caret stays in one call (the same [`SignatureRequest::call`]), a request already
    /// in flight for that call is adopted instead of superseded: its reply answers the newest
    /// ticket, and if the caret moved meanwhile the request is sent again at the new caret. A
    /// request in another call, or outside any call, supersedes the document's previous one.
    ///
    /// When the server is not running or has no signature provider, the request is declined with
    /// [`update::Change::Signature`]`(None)` under its ticket. A request for a document that is
    /// not registered gets nothing, and so does one from a revision other than `snapshot`'s or
    /// the last synced one while the server runs; otherwise a request at or past the synced
    /// revision is declined.
    pub(crate) fn signature_help(&mut self, snapshot: &Snapshot, request: &SignatureRequest) -> Output {
        let doc_id = snapshot.doc_id();
        let ticket = request.ticket();
        let Some(tracked) = self.tracked.iter().find(|t| t.doc_id == doc_id) else {
            return Output::default();
        };
        if !self.answerable(tracked, ticket, snapshot) {
            return Output::default();
        }
        if !matches!(&self.state, State::Running(server) if server.signature) {
            return Output::answer(doc_id, ticket, update::Change::Signature(None));
        }
        let caret = snapshot.clip_offset(snapshot.point_to_offset(request.position()), Bias::Left);
        let call = request.call();
        // Two requests outside any call share no call, so only a known call continues.
        if call.is_some() {
            let same_call = |p: &&mut Pending| {
                p.doc_id == doc_id
                    && matches!(&p.query, Query::Signature(query) if query.call == call)
            };
            if let Some(entry) = self.pending.iter_mut().find(same_call) {
                entry.latest_ticket = ticket;
                entry.latest_caret = caret;
                return Output::default();
            }
        }
        self.send(
            doc_id,
            ticket,
            Query::Signature(signature::Query { call, caret }),
            None,
        )
    }

    /// Answers the editor's hover request with a `textDocument/hover` at its offset, superseding
    /// the document's previous hover request.
    ///
    /// When the server is not running or has no hover provider, the request is declined with
    /// [`update::Change::Hover`]`(None)` under its ticket. A request for a document that is not
    /// registered gets nothing, and so does one from a revision other than `snapshot`'s or the
    /// last synced one while the server runs; otherwise a request at or past the synced revision
    /// is declined.
    pub(crate) fn hover(&mut self, snapshot: &Snapshot, request: &HoverRequest) -> Output {
        let doc_id = snapshot.doc_id();
        let ticket = request.ticket;
        let Some(tracked) = self.tracked.iter().find(|t| t.doc_id == doc_id) else {
            return Output::default();
        };
        if !self.answerable(tracked, ticket, snapshot) {
            return Output::default();
        }
        if !matches!(&self.state, State::Running(server) if server.hover) {
            return Output::answer(doc_id, ticket, update::Change::Hover(None));
        }
        let query = hover::Query {
            offset: request.offset,
            word: request.word.clone(),
        };
        self.send(doc_id, ticket, Query::Hover(query), None)
    }

    /// Asks the server where the symbol at the request's offset is defined. The answer is an
    /// [`update::Change::Definition`] under the request's ticket.
    ///
    /// When the server is not running or has no definition provider, the request is declined
    /// with [`update::Change::Definition`]`(None)` under its ticket. A request for a document
    /// that is not registered gets nothing, and so does one from a revision other than
    /// `snapshot`'s or the last synced one while the server runs; otherwise a request at or past
    /// the synced revision is declined.
    pub(crate) fn definition(&mut self, snapshot: &Snapshot, request: &DefinitionRequest) -> Output {
        let doc_id = snapshot.doc_id();
        let ticket = request.ticket;
        let Some(tracked) = self.tracked.iter().find(|t| t.doc_id == doc_id) else {
            return Output::default();
        };
        if !self.answerable(tracked, ticket, snapshot) {
            return Output::default();
        }
        if !matches!(&self.state, State::Running(server) if server.definition) {
            return Output::answer(doc_id, ticket, update::Change::Definition(None));
        }
        let query = Query::Definition {
            offset: request.offset,
        };
        self.send(doc_id, ticket, query, None)
    }

    /// Asks the server to rename the symbol at the request's offset to its new name. The answer
    /// is edits: an [`update::Change::Edits`] stamped with the synced revision for each open
    /// document, and an [`Update::FileEdits`] for each file that is not open. Either the whole
    /// rename arrives, or `receive` reports why none of it can: [`Error::StaleEdit`] or
    /// [`Error::Unsupported`]. A reply that arrives after the requesting document moved is
    /// dropped.
    ///
    /// When the server is not running or has no rename provider, nothing is sent. A request from
    /// a revision other than `snapshot`'s or the last synced one, or for a document that is not
    /// registered, gets nothing either.
    pub(crate) fn rename(&mut self, snapshot: &Snapshot, request: &RenameRequest) -> Output {
        let doc_id = snapshot.doc_id();
        let ticket = request.ticket;
        let Some(tracked) = self.tracked.iter().find(|t| t.doc_id == doc_id) else {
            return Output::default();
        };
        if ticket.revision() != snapshot.revision()
            || snapshot.revision() != tracked.synced.revision()
        {
            return Output::default();
        }
        if !matches!(&self.state, State::Running(server) if server.rename) {
            return Output::default();
        }
        let query = Query::Rename {
            offset: request.offset,
            new_name: request.new_name.clone(),
        };
        self.send(doc_id, ticket, query, None)
    }

    /// Asks the server to format the whole document, indenting with spaces. The answer is a
    /// [`update::Change::Edits`] under the request's ticket; a result that changes nothing
    /// answers nothing.
    ///
    /// When the server is not running or has no formatting provider, nothing is sent. A request
    /// from a revision other than `snapshot`'s or the last synced one, or for a document that is
    /// not registered, gets nothing either.
    pub(crate) fn format(&mut self, snapshot: &Snapshot, request: &FormatRequest) -> Output {
        let doc_id = snapshot.doc_id();
        let ticket = request.ticket;
        let Some(tracked) = self.tracked.iter().find(|t| t.doc_id == doc_id) else {
            return Output::default();
        };
        if ticket.revision() != snapshot.revision()
            || snapshot.revision() != tracked.synced.revision()
        {
            return Output::default();
        }
        if !matches!(&self.state, State::Running(server) if server.formatting) {
            return Output::default();
        }
        let query = Query::Format {
            tab_size: request.tab_size,
        };
        self.send(doc_id, ticket, query, None)
    }

    /// Asks the server for the inlay hints over the request's byte span, clamped to the
    /// document. The answer is an [`update::Change::Inlays`] under the request's ticket,
    /// replacing the hints the editor shows; a request already in flight for the document is
    /// cancelled.
    ///
    /// When the server is not running or has no inlay hint provider, the request is declined
    /// with an empty [`update::Change::Inlays`] under its ticket. A request for a document that
    /// is not registered gets nothing, and so does one from a revision other than `snapshot`'s
    /// or the last synced one while the server runs; otherwise a request at or past the synced
    /// revision is declined.
    pub(crate) fn inlays(&mut self, snapshot: &Snapshot, request: &intel::inlay::Request) -> Output {
        let doc_id = snapshot.doc_id();
        let ticket = request.ticket();
        let Some(tracked) = self.tracked.iter().find(|t| t.doc_id == doc_id) else {
            return Output::default();
        };
        if !self.answerable(tracked, ticket, snapshot) {
            return Output::default();
        }
        if !matches!(&self.state, State::Running(server) if server.inlay.is_some()) {
            return Output::answer(doc_id, ticket, update::Change::Inlays(Some(Vec::new())));
        }
        // rust-analyzer fails a range that ends past the last line instead of clamping it.
        let span = request.span();
        let end = span.end.min(snapshot.len());
        let query = Query::Inlays {
            span: span.start.min(end)..end,
        };
        self.send(doc_id, ticket, query, None)
    }

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
    pub(crate) fn interact(
        &mut self,
        snapshot: &Snapshot,
        interaction: &intel::inlay::Interaction,
    ) -> Output {
        let doc_id = snapshot.doc_id();
        let ticket = interaction.ticket();
        let key = interaction.key();
        let Some(tracked) = self.tracked.iter().find(|t| t.doc_id == doc_id) else {
            return Output::default();
        };
        let current = ticket.revision() == snapshot.revision()
            && snapshot.revision() == tracked.synced.revision();
        let set = tracked
            .inlays
            .as_ref()
            .filter(|set| current && set.revision() == ticket.revision());
        let (Some(set), State::Running(server)) = (set, &self.state) else {
            return Output::answer(doc_id, ticket, declined(interaction));
        };
        let Some(stored) = set.get(key) else {
            return Output::answer(doc_id, ticket, declined(interaction));
        };
        match interaction.gesture() {
            intel::inlay::interaction::Gesture::Tooltip { part } => {
                if let Some(markdown) = stored.tooltip(part) {
                    return Output::answer(
                        doc_id,
                        ticket,
                        update::Change::InlayTooltip(Some(markdown)),
                    );
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
                    self.target(
                        doc_id,
                        set.snapshot(),
                        set.revisions(),
                        uri::Key::new(&location.uri),
                        location.range,
                    )
                });
                Output::answer(doc_id, ticket, update::Change::Definition(target))
            }
            intel::inlay::interaction::Gesture::Insert { .. } => {
                let ops = edits::hygiene(
                    edits::Text::Snapshot(set.snapshot()),
                    self.encoding,
                    stored.text_edits(),
                );
                Output::answer(doc_id, ticket, update::Change::Edits(ops))
            }
        }
    }

    /// Brings the server up to `snapshot`. `changes` is the document's drained change log: when
    /// it leads exactly from what the server has to `snapshot`, and the server syncs
    /// incrementally, the edits go out as ranges; otherwise the whole text goes out. Servers that
    /// take no changes are sent nothing. Either way the snapshot becomes the one server
    /// positions are converted against.
    ///
    /// A snapshot no newer than the synced one and a document that is not registered send
    /// nothing. While the server is not running, the snapshot is stored and nothing is sent.
    pub(crate) fn sync(&mut self, snapshot: &Snapshot, changes: document::Changes) -> Output {
        let Self {
            state,
            encoding,
            tracked,
            versions,
            ..
        } = self;
        let Some(tracked) = tracked.iter_mut().find(|t| t.doc_id == snapshot.doc_id()) else {
            return Output::default();
        };
        if snapshot.revision() <= tracked.synced.revision() {
            return Output::default();
        }
        let server = match state {
            State::Running(server) => server,
            // Nothing goes out, but the text is kept: requests decline against it, and the next
            // `didOpen` carries it.
            State::Initializing { .. } | State::Disconnected => {
                tracked.synced = snapshot.clone();
                return Output::default();
            }
        };
        let content_changes = if !server.open_close || tracked.version.is_none() {
            None
        } else if server.change == TextDocumentSyncKind::INCREMENTAL {
            Some(
                incremental(*encoding, &tracked.synced, snapshot, &changes)
                    .unwrap_or_else(|| full(snapshot)),
            )
        } else if server.change == TextDocumentSyncKind::FULL {
            Some(full(snapshot))
        } else {
            None
        };
        tracked.synced = snapshot.clone();
        let Some(content_changes) = content_changes else {
            return Output::default();
        };
        let version = next_version(versions, &tracked.key);
        tracked.version = Some(version);
        Output {
            messages: vec![Message::Notification(message::Notification::new::<
                lsp_types::notification::DidChangeTextDocument,
            >(
                DidChangeTextDocumentParams {
                    text_document: VersionedTextDocumentIdentifier {
                        uri: tracked.key.uri().clone(),
                        version,
                    },
                    content_changes,
                },
            ))],
            updates: Vec::new(),
        }
    }

    /// Tells the server that the document of `snapshot` was written to disk, with the text when
    /// the server asks for it. The host writes the file and calls [`sync`](Self::sync) first:
    /// `snapshot` must be the synced one, so the saved text is the text the server has.
    ///
    /// Nothing is sent for a snapshot other than the synced one, a document that is not
    /// registered or not yet open on the server, a server that asks for no saves, or while the
    /// server is not running. A save before initialization needs no replay: the server reads the
    /// file from disk when it starts.
    pub(crate) fn save(&self, snapshot: &Snapshot) -> Output {
        let State::Running(server) = &self.state else {
            return Output::default();
        };
        let Some(tracked) = self.tracked.iter().find(|t| t.doc_id == snapshot.doc_id()) else {
            return Output::default();
        };
        if tracked.version.is_none() || snapshot.revision() != tracked.synced.revision() {
            return Output::default();
        }
        let text = match server.save {
            capabilities::Save::Never => return Output::default(),
            capabilities::Save::Notify => None,
            capabilities::Save::WithText => Some(tracked.synced.text().into_owned()),
        };
        Output {
            messages: vec![Message::Notification(message::Notification::new::<
                lsp_types::notification::DidSaveTextDocument,
            >(DidSaveTextDocumentParams {
                text_document: TextDocumentIdentifier {
                    uri: tracked.key.uri().clone(),
                },
                text,
            }))],
            updates: Vec::new(),
        }
    }

    /// Settles every request in flight with its empty answer and declines from now on. The
    /// `shutdown`/`exit` exchange belongs to the transport side.
    pub(crate) fn shutdown(&mut self) -> Output {
        let output = self.settle_pending();
        self.state = State::Disconnected;
        output
    }

    /// The server is gone. Settles every request in flight with its empty answer, and clears
    /// what the server told: an empty diagnostic set for every registered document, the
    /// unopened-file cache, inlay hints and completion sessions. Forgets the versions, so nothing
    /// saves until a reopen, and goes back to utf-16 until a handshake negotiates again.
    pub(crate) fn disconnected(&mut self) -> Output {
        let mut output = self.settle_pending();
        for tracked in &mut self.tracked {
            output.updates.push(Update::Document(update::Document::new(
                tracked.doc_id,
                update::Stamp::Revision(tracked.synced.revision()),
                update::Change::Diagnostics(Vec::new()),
            )));
            tracked.version = None;
            tracked.session = None;
            tracked.inlays = None;
        }
        self.cached.clear();
        self.encoding = Encoding::default();
        self.state = State::Disconnected;
        output
    }

    /// A fresh `initialize` for a new connection, from the stored parameters. Its reply reopens
    /// every registered document from its synced text, with versions counting on, and pushes
    /// the settings last set by `configure`.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn reinitialize(&mut self) -> Output {
        debug_assert!(
            matches!(self.state, State::Disconnected),
            "a new connection follows the loss of the old one"
        );
        let request = self.next_request();
        self.state = State::Initializing {
            request: request.clone(),
        };
        Output {
            messages: vec![Message::Request(message::Request::new::<
                lsp_types::request::Initialize,
            >(request, self.initialize.clone()))],
            updates: Vec::new(),
        }
    }

    /// Replaces the settings that `workspace/configuration` answers from. While running, the
    /// server is also told with `workspace/didChangeConfiguration`; before that, the handshake
    /// pushes them.
    pub(crate) fn configure(&mut self, configuration: Value) -> Output {
        self.configuration = Some(configuration);
        match self.state {
            State::Running(_) => Output {
                messages: self.configuration_push().into_iter().collect(),
                updates: Vec::new(),
            },
            State::Initializing { .. } | State::Disconnected => Output::default(),
        }
    }

    /// Folds one message from the server into the session. Server requests are always answered.
    ///
    /// # Errors
    /// [`Error::Decode`] for a payload that does not decode (a `publishDiagnostics`, or the
    /// `initialize` result), and [`Error::Server`] when `initialize` fails. A failed or
    /// undecodable `initialize` leaves the session disconnected, with its registered documents
    /// kept.
    pub(crate) fn receive(&mut self, message: Message) -> Result<Output, Error> {
        match message {
            Message::Request(request) => Ok(self.answer(request)),
            Message::Notification(notification) => self.notified(notification),
            Message::Response(response) => self.responded(response),
        }
    }

    /// Logs become [`Update::Log`], a server's `$/cancelRequest` is swallowed, and anything else
    /// but `publishDiagnostics` passes through.
    ///
    /// `publishDiagnostics` for an open document applies only if its version, when present, is
    /// the last one sent. A missing version is read as the last one sent, since servers that
    /// ignore `versionSupport` would otherwise never land a diagnostic.
    fn notified(&mut self, notification: message::Notification) -> Result<Output, Error> {
        use lsp_types::notification::Notification;
        let method = notification.method.as_str();
        if method == lsp_types::notification::LogMessage::METHOD {
            let params: lsp_types::LogMessageParams = decode(method, notification.params)?;
            return Ok(Output::update(Update::Log(log::Entry::logged(params))));
        }
        if method == lsp_types::notification::ShowMessage::METHOD {
            let params: lsp_types::ShowMessageParams = decode(method, notification.params)?;
            return Ok(Output::update(Update::Log(log::Entry::shown(params))));
        }
        // Server requests are answered as they arrive, so there is nothing to cancel.
        if method == lsp_types::notification::Cancel::METHOD {
            return Ok(Output::default());
        }
        if method != lsp_types::notification::PublishDiagnostics::METHOD {
            return Ok(Output::update(Update::Notification(
                update::Notification::new(notification),
            )));
        }
        let params: PublishDiagnosticsParams = decode(method, notification.params)?;
        let key = uri::Key::new(&params.uri);
        let Some(tracked) = self.tracked.iter().find(|t| t.key == key) else {
            // A versioned publish cannot apply to a closed URI, but it does supersede the
            // unversioned set cached for it.
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
                update::Change::Diagnostics(diagnostics::convert(
                    self.encoding,
                    &tracked.synced,
                    &params.diagnostics,
                )),
            ))],
        })
    }

    fn responded(&mut self, response: message::Response) -> Result<Output, Error> {
        let Some(id) = response.id else {
            return Ok(Output::default());
        };
        match &self.state {
            State::Initializing { request } if *request == id => self.initialized(response.result),
            State::Initializing { .. } | State::Running(_) | State::Disconnected => {
                self.settled(&id, response.result)
            }
        }
    }

    /// Routes the reply to a pending request. Replies to requests the client no longer waits
    /// for, and cancellations, are silent. A failed or undecodable reply to an intel request
    /// settles the editor's slot with the request's empty answer; one to a command is an error,
    /// since a user asked for it. A request the server dropped as content-modified is sent
    /// again, at most once per ticket.
    fn settled(
        &mut self,
        id: &message::Id,
        result: Result<Value, message::Error>,
    ) -> Result<Output, Error> {
        let Some(index) = self.pending.iter().position(|p| p.id == *id) else {
            return Ok(Output::default());
        };
        let entry = self.pending.swap_remove(index);
        if !self.awaited(&entry) {
            return Ok(Output::default());
        }
        match result {
            Err(error) if error.code == REQUEST_CANCELLED || error.code == SERVER_CANCELLED => {
                Ok(Output::default())
            }
            // The drop above guarantees the ticket is still current, so the server's text has
            // caught up with the editor's and asking again can succeed.
            Err(error) if error.code == CONTENT_MODIFIED => Ok(self.reissue(entry)),
            Err(error) if entry.query.kind().is_command() => Err(Error::Server {
                doc_id: Some(entry.doc_id),
                method: entry.query.method().to_owned(),
                error: error::Server::new(error),
            }),
            Err(_) => Ok(entry.empty()),
            Ok(value) => self.resolved(&entry, value),
        }
    }

    /// Whether `entry`'s ticket is still the one its editor awaits: the document is open and
    /// synced at the ticket's revision. Dropped here and never in `sync`: a host syncs before it
    /// hands the editor's requests over, so a continuing request moves `latest_ticket` forward
    /// only after the sync. Only when the reply arrives is it certain that no newer request
    /// adopted this entry.
    fn awaited(&self, entry: &Pending) -> bool {
        self.tracked
            .iter()
            .find(|t| t.doc_id == entry.doc_id)
            .is_some_and(|t| t.synced.revision() == entry.latest_ticket.revision())
    }

    /// Settles every request in flight with its empty answer. A request whose editor moved past
    /// it is dropped, as its reply would be.
    fn settle_pending(&mut self) -> Output {
        let mut output = Output::default();
        for entry in std::mem::take(&mut self.pending) {
            if self.awaited(&entry) {
                output.append(entry.empty());
            }
        }
        output
    }

    /// Sends `entry`'s query again at its latest caret, unless it already was for that ticket.
    fn reissue(&mut self, entry: Pending) -> Output {
        if entry.reissued_for == Some(entry.latest_ticket) {
            return Output::default();
        }
        match entry.query {
            Query::Completion(query) => self.request_completion(
                entry.doc_id,
                entry.latest_ticket,
                query.word.start..entry.latest_caret,
                query.at(entry.latest_caret),
                Some(entry.latest_ticket),
            ),
            Query::Signature(query) => self.send(
                entry.doc_id,
                entry.latest_ticket,
                Query::Signature(signature::Query {
                    call: query.call,
                    caret: entry.latest_caret,
                }),
                Some(entry.latest_ticket),
            ),
            query @ (Query::Hover(_)
            | Query::Definition { .. }
            | Query::Rename { .. }
            | Query::Format { .. }
            | Query::Inlays { .. }
            | Query::Resolve { .. }
            | Query::LocationHover { .. }) => self.send(
                entry.doc_id,
                entry.latest_ticket,
                query,
                Some(entry.latest_ticket),
            ),
        }
    }

    fn resolved(&mut self, entry: &Pending, value: Value) -> Result<Output, Error> {
        match &entry.query {
            Query::Completion(query) => Ok(self.completed(entry, query, value)),
            Query::Signature(query) => Ok(self.signed(entry, query, value)),
            Query::Hover(query) => Ok(self.hovered(entry, query, value)),
            Query::Definition { .. } => self.defined(entry, value),
            Query::Rename { .. } => self.renamed(entry, value),
            Query::Format { .. } => self.formatted(entry, value),
            Query::Inlays { span } => Ok(self.inlaid(entry, span, value)),
            Query::Resolve { key, part, .. } => {
                Ok(self.tooltip_resolved(entry, *key, *part, value))
            }
            Query::LocationHover { .. } => Ok(self.location_hovered(entry, value)),
        }
    }

    fn initialized(&mut self, result: Result<Value, message::Error>) -> Result<Output, Error> {
        const METHOD: &str = "initialize";
        let result = result
            .map_err(|error| Error::Server {
                doc_id: None,
                method: METHOD.to_owned(),
                error: error::Server::new(error),
            })
            .and_then(|value| decode::<InitializeResult>(METHOD, Some(value)));
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                // Without known capabilities nothing can be synced, so the connection is over;
                // the documents stay registered, declining every request.
                self.state = State::Disconnected;
                return Err(error);
            }
        };
        self.encoding = Encoding::negotiate(result.capabilities.position_encoding.as_ref());
        self.state = State::Running(capabilities::Server::new(&result.capabilities));
        let mut output = Output::default();
        output
            .messages
            .push(Message::Notification(message::Notification::new::<
                lsp_types::notification::Initialized,
            >(InitializedParams {})));
        output.messages.extend(self.configuration_push());
        if self.opens_and_closes() {
            for index in 0..self.tracked.len() {
                output.messages.push(self.did_open(index));
            }
        }
        // Requests from before the handshake were declined; this re-arms every editor, which
        // also covers servers that never send `workspace/inlayHint/refresh`.
        if matches!(&self.state, State::Running(server) if server.inlay.is_some()) {
            output.updates.extend(self.refreshes());
        }
        Ok(output)
    }

    /// `workspace/didChangeConfiguration` with the stored settings, if there are any.
    fn configuration_push(&self) -> Option<Message> {
        let settings = self.configuration.as_ref()?;
        Some(Message::Notification(message::Notification::new::<
            lsp_types::notification::DidChangeConfiguration,
        >(DidChangeConfigurationParams {
            settings: settings.clone(),
        })))
    }

    /// Whether a request under `ticket` at `snapshot` gets an answer. Its ticket must be from
    /// `snapshot`. While running, `snapshot` must be the synced text, which the request's
    /// positions convert against. Otherwise nothing is sent, so any text at or past the synced
    /// one is answered with the request's decline, and the editor's slot doesn't hang.
    fn answerable(&self, tracked: &Tracked, ticket: Ticket, snapshot: &Snapshot) -> bool {
        ticket.revision() == snapshot.revision()
            && match self.state {
                State::Running(_) => snapshot.revision() == tracked.synced.revision(),
                State::Initializing { .. } | State::Disconnected => {
                    snapshot.revision() >= tracked.synced.revision()
                }
            }
    }

    /// Whether the server is running and wants `didOpen`/`didClose`.
    fn opens_and_closes(&self) -> bool {
        matches!(&self.state, State::Running(server) if server.open_close)
    }

    /// `didOpen` for `tracked[index]` at the next version, with its synced text.
    fn did_open(&mut self, index: usize) -> Message {
        let Self {
            tracked, versions, ..
        } = self;
        let tracked = &mut tracked[index];
        let version = next_version(versions, &tracked.key);
        tracked.version = Some(version);
        Message::Notification(message::Notification::new::<
            lsp_types::notification::DidOpenTextDocument,
        >(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: tracked.key.uri().clone(),
                language_id: tracked.language.clone(),
                version,
                text: tracked.synced.text().into_owned(),
            },
        }))
    }

    fn next_request(&mut self) -> message::Id {
        let id = self.next_request;
        self.next_request += 1;
        message::Id::Number(id)
    }

    /// Starts a new completion session at `word` and sends its request at the synced snapshot,
    /// whose revision every caller has checked against the ticket's.
    fn request_completion(
        &mut self,
        doc_id: DocId,
        ticket: Ticket,
        word: Range<u32>,
        context: CompletionContext,
        reissued_for: Option<Ticket>,
    ) -> Output {
        let Some(tracked) = self.tracked.iter_mut().find(|t| t.doc_id == doc_id) else {
            return Output::default();
        };
        tracked.session = Some(completion::Session::new(&tracked.synced, word.clone()));
        self.send(
            doc_id,
            ticket,
            Query::Completion(completion::Query { word, context }),
            reissued_for,
        )
    }

    /// Sends `query` for `doc_id` at its synced snapshot, cancelling the request of the same
    /// kind it supersedes.
    fn send(
        &mut self,
        doc_id: DocId,
        ticket: Ticket,
        query: Query,
        reissued_for: Option<Ticket>,
    ) -> Output {
        let mut output = Output::default();
        if let Some(index) = self
            .pending
            .iter()
            .position(|p| p.doc_id == doc_id && p.query.kind() == query.kind())
        {
            output
                .messages
                .push(cancel(self.pending.swap_remove(index).id));
        }
        let Some(index) = self.tracked.iter().position(|t| t.doc_id == doc_id) else {
            return output;
        };
        let revisions = match query.kind() {
            Kind::Definition | Kind::Rename | Kind::Inlays => self
                .tracked
                .iter()
                .map(|t| (t.key.clone(), t.synced.revision()))
                .collect(),
            Kind::Completion | Kind::Signature | Kind::Hover | Kind::Format | Kind::Tooltip => {
                Vec::new()
            }
        };
        let id = self.next_request();
        let tracked = &self.tracked[index];
        output.messages.push(Message::Request(query.request(
            id.clone(),
            tracked.key.uri(),
            self.encoding,
            &tracked.synced,
        )));
        self.pending.push(Pending {
            id,
            doc_id,
            request_snapshot: tracked.synced.clone(),
            latest_ticket: ticket,
            latest_caret: query.caret(),
            query,
            reissued_for,
            revisions,
        });
        output
    }

    /// Stores the reply in the session and answers the latest ticket from it. An incomplete
    /// list whose caret has moved since the request is also asked for again at the new caret.
    fn completed(&mut self, entry: &Pending, query: &completion::Query, value: Value) -> Output {
        let Some(reply) = completion::Reply::decode(value) else {
            return entry.empty();
        };
        let incomplete = reply.incomplete();
        let candidates = completion::convert(
            self.encoding,
            &entry.request_snapshot,
            query.word.clone(),
            reply,
        );
        let Some(tracked) = self.tracked.iter_mut().find(|t| t.doc_id == entry.doc_id) else {
            return Output::default();
        };
        let Some(session) = tracked.session.as_mut() else {
            return Output::default();
        };
        session.fill(candidates, incomplete);
        let items = session.answer(&tracked.synced, entry.latest_caret);
        let mut output = Output::answer(
            entry.doc_id,
            entry.latest_ticket,
            update::Change::Completions(items),
        );
        if incomplete && entry.latest_caret != query.word.end {
            output.append(self.request_completion(
                entry.doc_id,
                entry.latest_ticket,
                query.word.start..entry.latest_caret,
                for_incomplete(),
                entry.reissued_for,
            ));
        }
        output
    }

    /// Answers the latest ticket with the reply's signature. When the caret moved while the
    /// request was out, the answer may describe the old caret's parameter, so the request is also
    /// sent again at the new caret; once typing stops the carets agree and this ends.
    fn signed(&mut self, entry: &Pending, query: &signature::Query, value: Value) -> Output {
        let Ok(help) = serde_json::from_value::<Option<lsp_types::SignatureHelp>>(value) else {
            return entry.empty();
        };
        let mut output = Output::answer(
            entry.doc_id,
            entry.latest_ticket,
            update::Change::Signature(help.and_then(signature::convert)),
        );
        if entry.latest_caret != query.caret {
            output.append(self.send(
                entry.doc_id,
                entry.latest_ticket,
                Query::Signature(signature::Query {
                    call: query.call,
                    caret: entry.latest_caret,
                }),
                entry.reissued_for,
            ));
        }
        output
    }

    /// Answers the ticket with the reply's card.
    fn hovered(&self, entry: &Pending, query: &hover::Query, value: Value) -> Output {
        let Ok(reply) = serde_json::from_value::<Option<lsp_types::Hover>>(value) else {
            return entry.empty();
        };
        let card = reply
            .and_then(|reply| hover::convert(self.encoding, &entry.request_snapshot, query, reply));
        Output::answer(entry.doc_id, entry.latest_ticket, update::Change::Hover(card))
    }

    /// Answers the ticket with the reply's hints, and keeps them as the document's set. `null`
    /// is an empty answer, which clears the editor's hints.
    fn inlaid(&mut self, entry: &Pending, span: &Range<u32>, value: Value) -> Output {
        let Ok(entries) = serde_json::from_value::<Option<Vec<Value>>>(value) else {
            return entry.empty();
        };
        let snapshot = &entry.request_snapshot;
        let fetched = inlay::decode(
            self.encoding,
            snapshot,
            span.clone(),
            entries.unwrap_or_default(),
        );
        let Self {
            tracked,
            next_inlay,
            ..
        } = self;
        let Some(tracked) = tracked.iter_mut().find(|t| t.doc_id == entry.doc_id) else {
            return Output::default();
        };
        let previous = tracked
            .inlays
            .take()
            .filter(|set| set.revision() == snapshot.revision());
        let (set, placed) = inlay::Set::install(
            snapshot,
            entry.revisions.clone(),
            fetched,
            previous,
            next_inlay,
        );
        tracked.inlays = Some(set);
        Output::answer(
            entry.doc_id,
            entry.latest_ticket,
            update::Change::Inlays(Some(placed)),
        )
    }

    /// Adds the resolved hint's tooltips to the stored hint, then answers from it, or falls back
    /// to the hover at the part's location.
    fn tooltip_resolved(
        &mut self,
        entry: &Pending,
        key: intel::inlay::Key,
        part: u32,
        value: Value,
    ) -> Output {
        let Ok(resolved) = serde_json::from_value::<lsp_types::InlayHint>(value) else {
            return entry.empty();
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
            return Output::answer(
                entry.doc_id,
                entry.latest_ticket,
                update::Change::InlayTooltip(None),
            );
        };
        stored.absorb(resolved);
        if let Some(markdown) = stored.tooltip(part) {
            return Output::answer(
                entry.doc_id,
                entry.latest_ticket,
                update::Change::InlayTooltip(Some(markdown)),
            );
        }
        let location = stored.location(part).cloned();
        self.hover_location(
            entry.doc_id,
            entry.latest_ticket,
            location,
            entry.reissued_for,
        )
    }

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
        let Some(location) = location else {
            return none();
        };
        if !matches!(&self.state, State::Running(server) if server.hover) {
            return none();
        }
        let Some(set) = self
            .tracked
            .iter()
            .find(|t| t.doc_id == doc_id)
            .and_then(|t| t.inlays.as_ref())
        else {
            return none();
        };
        let key = uri::Key::new(&location.uri);
        // The requesting document needs no check: its set is at the synced revision.
        let moved = self
            .tracked
            .iter()
            .find(|t| t.key == key && t.doc_id != doc_id)
            .is_some_and(|other| {
                revision_of(set.revisions(), &key) != Some(other.synced.revision())
            });
        if moved {
            return none();
        }
        let query = Query::LocationHover {
            uri: location.uri,
            position: location.range.start,
        };
        self.send(doc_id, ticket, query, reissued_for)
    }

    /// Answers the ticket with the hover at a label part's location, as tooltip markdown.
    fn location_hovered(&self, entry: &Pending, value: Value) -> Output {
        let Ok(reply) = serde_json::from_value::<Option<lsp_types::Hover>>(value) else {
            return entry.empty();
        };
        let markdown = reply
            .map(|reply| hover::contents(reply.contents))
            .filter(|markdown| !markdown.trim().is_empty());
        Output::answer(
            entry.doc_id,
            entry.latest_ticket,
            update::Change::InlayTooltip(markdown),
        )
    }

    /// Answers the ticket with the first location in the reply.
    ///
    /// # Errors
    /// [`Error::Decode`] when the result is not a location, a list of them, or `null`.
    fn defined(&self, entry: &Pending, value: Value) -> Result<Output, Error> {
        let locations: Option<workspace::Locations> = decode(entry.query.method(), Some(value))?;
        let target = locations
            .and_then(workspace::Locations::first)
            .and_then(|(key, range)| {
                self.target(
                    entry.doc_id,
                    &entry.request_snapshot,
                    &entry.revisions,
                    key,
                    range,
                )
            });
        Ok(Output::answer(
            entry.doc_id,
            entry.latest_ticket,
            update::Change::Definition(target),
        ))
    }

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
            return Some(update::Target::Unopened(jump::Unopened::new(
                key,
                range,
                self.encoding,
            )));
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

    /// The reply's workspace edit as updates, once every document it touches is checked.
    ///
    /// # Errors
    /// [`Error::StaleEdit`] when a touched document is not the text the server edited;
    /// [`Error::Unsupported`] for a file operation; [`Error::Decode`] for a malformed edit.
    fn renamed(&self, entry: &Pending, value: Value) -> Result<Output, Error> {
        let Some(edit) = workspace::Edit::decode(value)? else {
            return Ok(Output::default());
        };
        for file in edit.files() {
            let recorded = revision_of(&entry.revisions, file.key());
            let stale = match self.tracked.iter().find(|t| t.key == *file.key()) {
                Some(tracked) => {
                    recorded != Some(tracked.synced.revision())
                        || file
                            .version()
                            .is_some_and(|version| Some(version) != tracked.version)
                }
                // Open at the request and closed since: its unsaved text is gone, and the disk
                // holds something other than what the server edited.
                None => recorded.is_some(),
            };
            if stale {
                return Err(Error::StaleEdit {
                    uri: file.key().clone(),
                });
            }
        }
        let mut output = Output::default();
        for file in edit.into_files() {
            let (key, text_edits) = file.into_parts();
            match self.tracked.iter().find(|t| t.key == key) {
                Some(tracked) => {
                    let ops = edits::hygiene(
                        edits::Text::Snapshot(&tracked.synced),
                        self.encoding,
                        &text_edits,
                    );
                    if !ops.is_empty() {
                        output.updates.push(Update::Document(update::Document::new(
                            tracked.doc_id,
                            update::Stamp::Revision(tracked.synced.revision()),
                            update::Change::Edits(ops),
                        )));
                    }
                }
                None => output.updates.push(Update::FileEdits(update::FileEdits::new(
                    key,
                    text_edits,
                    self.encoding,
                ))),
            }
        }
        Ok(output)
    }

    /// Answers the ticket with the reply's edits, converted against the request's text, which
    /// is still the synced text once `settled` let the reply through.
    ///
    /// # Errors
    /// [`Error::Decode`] when the result is not a `TextEdit[]` or `null`.
    fn formatted(&self, entry: &Pending, value: Value) -> Result<Output, Error> {
        let text_edits: Option<Vec<lsp_types::TextEdit>> =
            decode(entry.query.method(), Some(value))?;
        let ops = edits::hygiene(
            edits::Text::Snapshot(&entry.request_snapshot),
            self.encoding,
            &text_edits.unwrap_or_default(),
        );
        if ops.is_empty() {
            return Ok(Output::default());
        }
        Ok(Output::answer(
            entry.doc_id,
            entry.latest_ticket,
            update::Change::Edits(ops),
        ))
    }

    fn answer(&self, request: message::Request) -> Output {
        use lsp_types::request::Request;
        let refresh = request.method == lsp_types::request::InlayHintRefreshRequest::METHOD;
        let (response, updates) = match self.state {
            // A disconnected session promises nothing; `null` keeps the server unblocked.
            State::Disconnected => (message::Response::ok(request.id, ()), Vec::new()),
            State::Initializing { .. } => (self.respond(request), Vec::new()),
            State::Running(_) => {
                let updates = if refresh {
                    self.refreshes()
                } else {
                    Vec::new()
                };
                (self.respond(request), updates)
            }
        };
        Output {
            messages: vec![Message::Response(response)],
            updates,
        }
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

    fn respond(&self, request: message::Request) -> message::Response {
        use lsp_types::request::Request;
        let message::Request { id, method, params } = request;
        match method.as_str() {
            "workspace/configuration" => {
                match serde_json::from_value::<ConfigurationParams>(params.unwrap_or(Value::Null)) {
                    Ok(params) => message::Response::ok(
                        id,
                        params
                            .items
                            .iter()
                            .map(|item| self.section(item.section.as_deref()))
                            .collect::<Vec<_>>(),
                    ),
                    Err(error) => message::Response::error(
                        Some(id),
                        message::Error {
                            code: INVALID_PARAMS,
                            message: error.to_string(),
                            data: None,
                        },
                    ),
                }
            }
            "workspace/workspaceFolders" => {
                message::Response::ok(id, self.initialize.workspace_folders.clone())
            }
            "client/registerCapability"
            | "client/unregisterCapability"
            | "window/workDoneProgress/create"
            | "window/showMessageRequest" => message::Response::ok(id, ()),
            "workspace/applyEdit" => message::Response::ok(
                id,
                ApplyWorkspaceEditResponse {
                    applied: false,
                    failure_reason: Some(
                        "scrive-lsp does not apply server-initiated edits".to_owned(),
                    ),
                    failed_change: None,
                },
            ),
            // `answer` also tells each open document to refetch.
            method if method == lsp_types::request::InlayHintRefreshRequest::METHOD => {
                message::Response::ok(id, ())
            }
            // `workspace/semanticTokens/refresh`, …: nothing is cached that a refresh would
            // invalidate.
            method if method.starts_with("workspace/") && method.ends_with("/refresh") => {
                message::Response::ok(id, ())
            }
            method => message::Response::error(
                Some(id),
                message::Error {
                    code: METHOD_NOT_FOUND,
                    message: format!("`{method}` is not supported"),
                    data: None,
                },
            ),
        }
    }

    /// The dotted `section` of the configuration. An empty or absent section is the whole value,
    /// and a path that does not exist is `null`.
    fn section(&self, section: Option<&str>) -> Value {
        let Some(configuration) = &self.configuration else {
            return Value::Null;
        };
        match section.filter(|s| !s.is_empty()) {
            None => configuration.clone(),
            Some(path) => path
                .split('.')
                .try_fold(configuration, |value, key| value.get(key))
                .cloned()
                .unwrap_or(Value::Null),
        }
    }
}

impl Output {
    /// One update, with nothing to send.
    fn update(update: Update) -> Self {
        Self {
            messages: Vec::new(),
            updates: vec![update],
        }
    }

    /// A ticket-stamped change for one document, with nothing to send.
    fn answer(doc_id: DocId, ticket: Ticket, change: update::Change) -> Self {
        Self {
            messages: Vec::new(),
            updates: vec![Update::Document(update::Document::new(
                doc_id,
                update::Stamp::Ticket(ticket),
                change,
            ))],
        }
    }

    /// Appends `other`'s messages and updates after this output's.
    fn append(&mut self, other: Output) {
        self.messages.extend(other.messages);
        self.updates.extend(other.updates);
    }
}

impl Pending {
    /// The empty answer that settles the editor's slot for this request: after the server failed
    /// it, or when no reply can come. Rename and format have no slot.
    fn empty(&self) -> Output {
        match self.query {
            Query::Completion(_) => Output::answer(
                self.doc_id,
                self.latest_ticket,
                update::Change::Completions(Vec::new()),
            ),
            Query::Signature(_) => Output::answer(
                self.doc_id,
                self.latest_ticket,
                update::Change::Signature(None),
            ),
            Query::Hover(_) => {
                Output::answer(self.doc_id, self.latest_ticket, update::Change::Hover(None))
            }
            Query::Inlays { .. } => Output::answer(
                self.doc_id,
                self.latest_ticket,
                update::Change::Inlays(None),
            ),
            Query::Resolve { .. } | Query::LocationHover { .. } => Output::answer(
                self.doc_id,
                self.latest_ticket,
                update::Change::InlayTooltip(None),
            ),
            Query::Definition { .. } => Output::answer(
                self.doc_id,
                self.latest_ticket,
                update::Change::Definition(None),
            ),
            Query::Rename { .. } | Query::Format { .. } => Output::default(),
        }
    }
}

impl Kind {
    /// Whether the user asked for this request, so its failure is reported rather than settled.
    fn is_command(self) -> bool {
        match self {
            Kind::Completion | Kind::Signature | Kind::Hover | Kind::Inlays | Kind::Tooltip => {
                false
            }
            Kind::Definition | Kind::Rename | Kind::Format => true,
        }
    }
}

impl Query {
    fn kind(&self) -> Kind {
        match self {
            Query::Completion(_) => Kind::Completion,
            Query::Signature(_) => Kind::Signature,
            Query::Hover(_) => Kind::Hover,
            Query::Definition { .. } => Kind::Definition,
            Query::Rename { .. } => Kind::Rename,
            Query::Format { .. } => Kind::Format,
            Query::Inlays { .. } => Kind::Inlays,
            Query::Resolve { .. } | Query::LocationHover { .. } => Kind::Tooltip,
        }
    }

    /// The request's LSP method.
    fn method(&self) -> &'static str {
        use lsp_types::request::Request;
        match self {
            Query::Completion(_) => lsp_types::request::Completion::METHOD,
            Query::Signature(_) => lsp_types::request::SignatureHelpRequest::METHOD,
            Query::Hover(_) => lsp_types::request::HoverRequest::METHOD,
            Query::Definition { .. } => lsp_types::request::GotoDefinition::METHOD,
            Query::Rename { .. } => lsp_types::request::Rename::METHOD,
            Query::Format { .. } => lsp_types::request::Formatting::METHOD,
            Query::Inlays { .. } => lsp_types::request::InlayHintRequest::METHOD,
            Query::Resolve { .. } => lsp_types::request::InlayHintResolveRequest::METHOD,
            Query::LocationHover { .. } => lsp_types::request::HoverRequest::METHOD,
        }
    }

    /// The caret the query was made at.
    fn caret(&self) -> u32 {
        match self {
            Query::Completion(query) => query.word.end,
            Query::Signature(query) => query.caret,
            Query::Hover(query) => query.offset,
            Query::Definition { offset } | Query::Rename { offset, .. } => *offset,
            // Formatting, resolves and location hovers ask at no caret in the requesting
            // document.
            Query::Format { .. } | Query::Resolve { .. } | Query::LocationHover { .. } => 0,
            // A fetch covers a span; its start stands in for the caret.
            Query::Inlays { span } => span.start,
        }
    }

    /// The request message for this query at `snapshot`.
    fn request(
        &self,
        id: message::Id,
        uri: &Uri,
        encoding: Encoding,
        snapshot: &Snapshot,
    ) -> message::Request {
        use lsp_types::request::Request;
        match self {
            Query::Completion(query) => message::Request::new::<lsp_types::request::Completion>(
                id,
                CompletionParams {
                    text_document_position: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position: encoding.position(snapshot, query.word.end),
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                    context: Some(query.context.clone()),
                },
            ),
            Query::Signature(query) => {
                message::Request::new::<lsp_types::request::SignatureHelpRequest>(
                    id,
                    SignatureHelpParams {
                        context: None,
                        text_document_position_params: TextDocumentPositionParams {
                            text_document: TextDocumentIdentifier { uri: uri.clone() },
                            position: encoding.position(snapshot, query.caret),
                        },
                        work_done_progress_params: Default::default(),
                    },
                )
            }
            Query::Hover(query) => message::Request::new::<lsp_types::request::HoverRequest>(
                id,
                HoverParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position: encoding.position(snapshot, query.offset),
                    },
                    work_done_progress_params: Default::default(),
                },
            ),
            Query::Definition { offset } => {
                message::Request::new::<lsp_types::request::GotoDefinition>(
                    id,
                    GotoDefinitionParams {
                        text_document_position_params: TextDocumentPositionParams {
                            text_document: TextDocumentIdentifier { uri: uri.clone() },
                            position: encoding.position(snapshot, *offset),
                        },
                        work_done_progress_params: Default::default(),
                        partial_result_params: Default::default(),
                    },
                )
            }
            Query::Rename { offset, new_name } => {
                message::Request::new::<lsp_types::request::Rename>(
                    id,
                    RenameParams {
                        text_document_position: TextDocumentPositionParams {
                            text_document: TextDocumentIdentifier { uri: uri.clone() },
                            position: encoding.position(snapshot, *offset),
                        },
                        new_name: new_name.clone(),
                        work_done_progress_params: Default::default(),
                    },
                )
            }
            Query::Format { tab_size } => message::Request::new::<lsp_types::request::Formatting>(
                id,
                DocumentFormattingParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    // scrive indents with spaces, so the formatter must too.
                    options: FormattingOptions {
                        tab_size: *tab_size,
                        insert_spaces: true,
                        ..FormattingOptions::default()
                    },
                    work_done_progress_params: Default::default(),
                },
            ),
            Query::Inlays { span } => message::Request::new::<lsp_types::request::InlayHintRequest>(
                id,
                InlayHintParams {
                    work_done_progress_params: Default::default(),
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    range: encoding.range(snapshot, span.clone()),
                },
            ),
            Query::Resolve { hint, .. } => message::Request {
                id,
                method: lsp_types::request::InlayHintResolveRequest::METHOD.to_owned(),
                params: Some(hint.clone()),
            },
            // The location is the server's own, in its own document and encoding, so it goes
            // out unconverted.
            Query::LocationHover {
                uri: target,
                position,
            } => message::Request::new::<lsp_types::request::HoverRequest>(
                id,
                HoverParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier {
                            uri: target.clone(),
                        },
                        position: *position,
                    },
                    work_done_progress_params: Default::default(),
                },
            ),
        }
    }
}

/// The context for asking again for a list the server marked incomplete.
fn for_incomplete() -> CompletionContext {
    CompletionContext {
        trigger_kind: CompletionTriggerKind::TRIGGER_FOR_INCOMPLETE_COMPLETIONS,
        trigger_character: None,
    }
}

/// `$/cancelRequest` for `id`, built by hand because lsp-types' `CancelParams` holds an `i32` id,
/// narrower than [`message::Id`].
fn cancel(id: message::Id) -> Message {
    Message::Notification(message::Notification {
        method: "$/cancelRequest".to_owned(),
        params: Some(serde_json::json!({ "id": id })),
    })
}

/// The empty answer to a gesture that cannot be served.
fn declined(interaction: &intel::inlay::Interaction) -> update::Change {
    match interaction.gesture() {
        intel::inlay::interaction::Gesture::Tooltip { .. } => update::Change::InlayTooltip(None),
        intel::inlay::interaction::Gesture::Jump { .. } => update::Change::Definition(None),
        intel::inlay::interaction::Gesture::Insert { .. } => update::Change::Edits(Vec::new()),
    }
}

/// The synced revision `key` was at when `revisions` were recorded, if it was open then.
fn revision_of(revisions: &[(uri::Key, Revision)], key: &uri::Key) -> Option<Revision> {
    revisions
        .iter()
        .find(|(k, _)| k == key)
        .map(|&(_, revision)| revision)
}

/// The longest registered trigger that the text before `caret` ends with. Triggers may be
/// longer than one character (`::`).
fn matched_trigger(snapshot: &Snapshot, caret: u32, triggers: &[String]) -> Option<String> {
    let reach = triggers.iter().map(|t| t.len() as u32).max()?;
    let start = snapshot.clip_offset(caret.saturating_sub(reach), Bias::Left);
    let before = snapshot.slice(start..caret);
    triggers
        .iter()
        .filter(|t| !t.is_empty() && before.ends_with(t.as_str()))
        .max_by_key(|t| t.len())
        .cloned()
}

/// The next version for `key`, advancing its high-water mark.
// `Key` hashes and compares by its URI text, which fluent-uri's internal `Cell` never changes.
#[allow(clippy::mutable_key_type)]
fn next_version(versions: &mut HashMap<uri::Key, i32>, key: &uri::Key) -> i32 {
    let version = versions.entry(key.clone()).or_insert(0);
    *version += 1;
    *version
}

/// The whole document as one content change.
fn full(snapshot: &Snapshot) -> Vec<TextDocumentContentChangeEvent> {
    vec![TextDocumentContentChangeEvent {
        range: None,
        range_length: None,
        text: snapshot.text().into_owned(),
    }]
}

/// The logged edits as ranged content changes, or `None` when `changes` does not lead exactly
/// from `synced` to `snapshot`: another document's log, a broken log, a gap, or an end short of
/// `snapshot`.
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
        // The server applies content changes in order (LSP §textDocument_didChange). A
        // commit's ops are descending, so every op lies before the ones already applied and
        // its range is the same in `before` as in the text the server holds by then.
        events.extend(
            change
                .ops()
                .iter()
                .map(|op| TextDocumentContentChangeEvent {
                    range: Some(encoding.range(before, op.range.clone())),
                    range_length: None,
                    text: op.text.clone(),
                }),
        );
        cursor = Revision(cursor.0 + 1);
    }
    (cursor == snapshot.revision()).then_some(events)
}

/// Decodes a payload, naming the method on failure. Absent params decode from `null`.
fn decode<T: serde::de::DeserializeOwned>(method: &str, params: Option<Value>) -> Result<T, Error> {
    serde_json::from_value(params.unwrap_or(Value::Null)).map_err(|source| Error::Decode {
        method: method.to_owned(),
        source: Arc::new(source),
    })
}
