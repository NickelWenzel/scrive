//! A [`CodeEditor`]'s side of a [`scrive_lsp::Client`].
//!
//! A host calls [`open_lsp`](CodeEditor::open_lsp) once per document,
//! [`sync_lsp`](CodeEditor::sync_lsp) after every [`update`](CodeEditor::update),
//! [`apply_lsp`](CodeEditor::apply_lsp) for each `Update::Document` the client returns, and
//! [`save_lsp`](CodeEditor::save_lsp) after writing the document to disk. The client sends what
//! each call produces. `sync_lsp`, `save_lsp`, `apply_lsp` and [`jump`](CodeEditor::jump) return
//! a jump into another document for the host to route.

use iced::time::Duration;
use scrive_core::DiagnosticsOutcome;
use scrive_lsp::lsp_types::Uri;
use scrive_lsp::update::{self, Change, Refusal, Stamp, Target};
use scrive_lsp::{Client, Error};

use super::{Awaited, CodeEditor, INLAY_EDIT_DELAY};

impl CodeEditor {
    /// Register this editor's document with `client` under `uri`, in language `language`, and
    /// start mirroring its edits. The client sends the `didOpen` once it is initialized.
    ///
    /// Call it once per document, and again after [`close_lsp`](CodeEditor::close_lsp) when
    /// the editor [`load`](CodeEditor::load)s another file or the server restarts. An editor
    /// talks to one client.
    ///
    /// # Errors
    /// [`Error::DuplicateUri`] when another document already claims `uri`. The editor is then
    /// left as it was: its change log is kept, and no client is recorded.
    pub fn open_lsp(
        &mut self,
        client: &mut Client,
        uri: &Uri,
        language: &str,
    ) -> Result<(), Error> {
        debug_assert!(
            self.lsp_client.is_none_or(|id| id == client.id()),
            "a CodeEditor talks to one Client: close_lsp before opening with another",
        );
        let snapshot = self.doc.snapshot();
        let answer = client.open(&snapshot, uri, language)?;
        // Restart the log at the registered text: older entries would replay edits the server
        // already has.
        self.doc.observe_changes(false);
        self.doc.observe_changes(true);
        debug_assert_eq!(
            self.doc.revision(),
            snapshot.revision(),
            "the log starts at the registered snapshot",
        );
        self.lsp_client = Some(client.id());
        self.wait_inlays(Duration::ZERO, None);
        if let Some(document) = answer {
            let landed = self.land(document);
            debug_assert!(landed.jump.is_none(), "an open lands cached diagnostics only");
        }
        Ok(())
    }

    /// Mirror every edit since the last sync to `client`, then send each request the editor
    /// recorded: completion, signature help, hover, definition, rename, format, an inlay hint
    /// fetch and a gesture on a hint. Call it after every [`update`](CodeEditor::update); with
    /// nothing new it sends nothing.
    ///
    /// Answers the client gives without asking the server land before it returns: declines,
    /// reused completion lists, and a hint's label jump or text edits. An edit that lands this
    /// way is synced in the same call. A label jump into another document comes back as
    /// [`Applied::jump`](update::Applied::jump) for the host to route; `refused` is always
    /// `None`.
    ///
    /// On an editor that is not registered, before [`open_lsp`](CodeEditor::open_lsp) or after
    /// [`close_lsp`](CodeEditor::close_lsp), it does nothing. Its `client` must be the one the
    /// editor was opened with.
    #[must_use = "Applied.jump must be routed"]
    pub fn sync_lsp(&mut self, client: &mut Client) -> update::Applied {
        let mut synced = update::Applied::default();
        let Some(registered) = self.lsp_client else {
            return synced;
        };
        debug_assert_eq!(
            registered,
            client.id(),
            "sync_lsp needs the Client this editor was opened with",
        );
        // An inserted hint's edit lands inside a pass, so its didChange and the requests it
        // recorded go out on the next. That pass lands no edit: only a gesture's interaction
        // edits, and the hint fetch the edit scheduled waits for a wake.
        let mut passes = 0;
        loop {
            passes += 1;
            debug_assert!(passes <= 2, "only the first pass lands an edit");
            let revision = self.doc.revision();
            absorb(&mut synced, self.sync_pass(client));
            if self.doc.revision() == revision {
                return synced;
            }
        }
    }

    /// Sync the edits, send every recorded request against one snapshot, then land the
    /// client's local answers in order.
    fn sync_pass(&mut self, client: &mut Client) -> update::Applied {
        let snapshot = self.doc.snapshot();
        // The client ignores a request from a revision it has not been synced to.
        client.sync(&snapshot, self.doc.drain_changes());
        let mut answers = Vec::new();
        if let Some(request) = self.take_completion_request() {
            answers.extend(client.complete(&snapshot, &request));
        }
        if let Some(request) = self.take_signature_request() {
            answers.extend(client.signature_help(&snapshot, &request));
        }
        if let Some(request) = self.take_hover_request() {
            answers.extend(client.hover(&snapshot, &request));
        }
        if let Some(request) = self.take_definition_request() {
            answers.extend(client.definition(&snapshot, &request));
        }
        if let Some(request) = self.take_rename_request() {
            answers.extend(client.rename(&snapshot, &request));
        }
        if let Some(request) = self.take_format_request() {
            answers.extend(client.format(&snapshot, &request));
        }
        if let Some(request) = self.take_inlay_request() {
            answers.extend(client.inlays(&snapshot, &request));
        }
        // Last, so an insert's edit lands after the answers made at the revision it moves.
        if let Some(interaction) = self.take_inlay_interaction() {
            answers.extend(client.interact(&snapshot, &interaction));
        }
        let mut passed = update::Applied::default();
        // A refused local answer is dropped, as a late reply would be.
        for document in answers {
            absorb(&mut passed, self.land(document));
        }
        passed
    }

    /// Tell `client` that the document was saved, syncing first so the server holds the saved
    /// text. Call it after writing the document to disk. The client sends the sync, then a
    /// `didSave` when the server asks for saves; the sync's jump comes back.
    ///
    /// On an editor that is not registered it does nothing.
    #[must_use = "Applied.jump must be routed"]
    pub fn save_lsp(&mut self, client: &mut Client) -> update::Applied {
        if self.lsp_client.is_none() {
            return update::Applied::default();
        }
        let saved = self.sync_lsp(client);
        client.save(&self.doc.snapshot());
        saved
    }

    /// Apply one document update from [`Client::receive`], then sync. The update is refused
    /// when it is for another document, stale, or an edit batch that does not apply. A
    /// definition in another document comes back as [`Applied::jump`](update::Applied::jump)
    /// for the host to route, as does a hint's label jump that the sync landed; the update's
    /// own jump wins over the sync's.
    #[must_use = "Applied.jump must be routed"]
    pub fn apply_lsp(
        &mut self,
        client: &mut Client,
        document: update::Document,
    ) -> update::Applied {
        let landed = self.land(document);
        let synced = self.sync_lsp(client);
        update::Applied {
            // The host is waiting on the answer it passed in.
            jump: landed.jump.or(synced.jump),
            refused: landed.refused,
        }
    }

    /// Select a definition that another editor's [`apply_lsp`](CodeEditor::apply_lsp) returned
    /// as [`Jump::Open`](update::Jump::Open), then sync. Returns what the sync after the
    /// selection produced.
    ///
    /// # Errors
    /// [`Refusal::Foreign`] when `open` is in another document, and [`Refusal::Stale`] when
    /// this one has changed since the server answered. Nothing is selected either way.
    pub fn jump(
        &mut self,
        client: &mut Client,
        open: update::jump::Open,
    ) -> Result<update::Applied, Refusal> {
        if open.doc_id() != self.doc.doc_id() {
            return Err(Refusal::Foreign);
        }
        if open.revision() != self.doc.revision() {
            return Err(Refusal::Stale);
        }
        self.select(open.span());
        Ok(self.sync_lsp(client))
    }

    /// Unregister this editor's document from `client`. Mirroring stops, the rename field, the
    /// completion popup, the signature box and the hover card close, requests in flight are
    /// forgotten, the server's diagnostics are cleared, the inlay hints and their tooltip are
    /// cleared, and a scheduled hint fetch is dropped. The client sends the `didClose`, and
    /// cancellations for requests in flight. F2 stays as [`rename`](CodeEditor::rename) set it,
    /// for a later [`open_lsp`](CodeEditor::open_lsp).
    pub fn close_lsp(&mut self, client: &mut Client) {
        self.doc.observe_changes(false);
        self.rename = None;
        // No reply can land after the close, so nothing may stay open waiting for one.
        self.completion.close();
        self.signature = None;
        self.hover = None;
        self.abandon(Awaited::Completion);
        self.abandon(Awaited::Signature);
        self.abandon(Awaited::Hover);
        self.abandon(Awaited::Definition);
        self.pending_rename_request = None;
        self.pending_format_request = None;
        self.inlay_card = None;
        self.abandon(Awaited::Inlays);
        self.abandon(Awaited::InlayTooltip);
        self.abandon(Awaited::InlayInsert);
        self.pending_inlay_interaction = None;
        self.inlays.wait = None;
        self.inlays.window = None;
        self.doc.clear_inlays();
        let _ = self.set_diagnostics(self.doc.revision(), Vec::new());
        self.lsp_client = None;
        client.close(self.doc.doc_id());
    }

    /// Check `document`'s identity and stamp against this editor, then apply its change. It
    /// never syncs, so `sync_pass` can land through it.
    fn land(&mut self, document: update::Document) -> update::Applied {
        if document.doc_id() != self.doc.doc_id() {
            return refused(Refusal::Foreign);
        }
        let (_, stamp, change) = document.into_parts();
        // The client stamps diagnostics, rename edits and inlay refreshes with a revision, and
        // every other change with a ticket; any other pairing is treated as stale.
        match change {
            Change::Diagnostics(diagnostics) => {
                let Stamp::Revision(revision) = stamp else {
                    return refused(Refusal::Stale);
                };
                match self.set_diagnostics(revision, diagnostics) {
                    DiagnosticsOutcome::Applied { .. } => update::Applied::default(),
                    DiagnosticsOutcome::Stale { .. } => refused(Refusal::Stale),
                }
            }
            Change::Completions(items) => {
                let Stamp::Ticket(ticket) = stamp else {
                    return refused(Refusal::Stale);
                };
                // `set_*` may retire the slot it lands in, so the verdict is read first.
                let accepted = self.accepts(Awaited::Completion, ticket);
                self.set_completions(ticket, items);
                if accepted {
                    update::Applied::default()
                } else {
                    refused(Refusal::Stale)
                }
            }
            Change::Signature(info) => {
                let Stamp::Ticket(ticket) = stamp else {
                    return refused(Refusal::Stale);
                };
                let accepted = self.accepts(Awaited::Signature, ticket);
                self.set_signature(ticket, info);
                if accepted {
                    update::Applied::default()
                } else {
                    refused(Refusal::Stale)
                }
            }
            Change::Hover(info) => {
                let Stamp::Ticket(ticket) = stamp else {
                    return refused(Refusal::Stale);
                };
                let accepted = self.accepts(Awaited::Hover, ticket);
                self.set_hover(ticket, info);
                if accepted {
                    update::Applied::default()
                } else {
                    refused(Refusal::Stale)
                }
            }
            Change::Definition(target) => {
                let Stamp::Ticket(ticket) = stamp else {
                    return refused(Refusal::Stale);
                };
                if !self.accepts(Awaited::Definition, ticket) {
                    return refused(Refusal::Stale);
                }
                let (span, jump) = match target {
                    Some(Target::Local(span)) => (Some(span), None),
                    Some(Target::Open(open)) => (None, Some(update::Jump::Open(open))),
                    Some(Target::Unopened(unopened)) => {
                        (None, Some(update::Jump::Unopened(unopened)))
                    }
                    None => (None, None),
                };
                self.set_definition(ticket, span);
                update::Applied {
                    jump,
                    ..update::Applied::default()
                }
            }
            Change::Edits(ops) => {
                let revision = match stamp {
                    Stamp::Ticket(ticket) => ticket.revision(),
                    Stamp::Revision(revision) => revision,
                };
                if revision != self.doc.revision() {
                    return refused(Refusal::Stale);
                }
                let inserting = match stamp {
                    Stamp::Ticket(ticket) => self.accepts(Awaited::InlayInsert, ticket),
                    Stamp::Revision(_) => false,
                };
                if inserting && ops.is_empty() {
                    // A declined insert: `try_edit`'s tail would close the popup and the hover
                    // card for no edit.
                    self.abandon(Awaited::InlayInsert);
                    return update::Applied::default();
                }
                if inserting {
                    // The edits spell out the hint's label at its offset, so the hint would
                    // render beside its own text; `try_edit`'s tail clears the slot naming it.
                    if let Some((_, key, offset)) = self.awaiting.inlay_insert.take() {
                        let _ = self.doc.remove_inlay(key, offset);
                    }
                }
                match self.try_edit(ops) {
                    Ok(()) => update::Applied::default(),
                    Err(_) => refused(Refusal::Overlap),
                }
            }
            Change::Inlays(placed) => {
                let Stamp::Ticket(ticket) = stamp else {
                    return refused(Refusal::Stale);
                };
                let accepted = self.accepts(Awaited::Inlays, ticket);
                self.set_inlays(ticket, placed);
                if accepted {
                    update::Applied::default()
                } else {
                    refused(Refusal::Stale)
                }
            }
            Change::InlayRefresh => {
                // The server's hints changed whatever text it saw, so a refresh is never stale.
                self.wait_inlays(INLAY_EDIT_DELAY, None);
                update::Applied::default()
            }
            Change::InlayTooltip(markdown) => {
                let Stamp::Ticket(ticket) = stamp else {
                    return refused(Refusal::Stale);
                };
                let accepted = self.accepts(Awaited::InlayTooltip, ticket);
                self.set_inlay_tooltip(ticket, markdown);
                if accepted {
                    update::Applied::default()
                } else {
                    refused(Refusal::Stale)
                }
            }
        }
    }
}

/// Append `routed`'s jump to `into`, which keeps an earlier one.
fn absorb(into: &mut update::Applied, routed: update::Applied) {
    if into.jump.is_none() {
        into.jump = routed.jump;
    }
}

/// An update that was not applied, and why.
fn refused(refusal: Refusal) -> update::Applied {
    update::Applied {
        refused: Some(refusal),
        ..update::Applied::default()
    }
}

#[cfg(test)]
mod tests {
    use iced::futures::{FutureExt, StreamExt};
    use iced::time::Instant;
    use scrive_core::intel::inlay::Key;
    use scrive_core::CompletionState;
    use serde_json::{json, Value};

    use scrive_lsp::lsp_types::Uri;
    use scrive_lsp::update::{self, Refusal};
    use scrive_lsp::{client, lsp_server, Client, Error, Update};

    use crate::code_editor::INLAY_EDIT_DELAY;
    use crate::editor::Action;
    use crate::{CodeEditor, Event};

    const A: &str = "file:///w/a.rs";
    const B: &str = "file:///w/b.rs";

    fn uri(text: &str) -> Uri {
        text.parse().expect("test URIs parse")
    }

    /// A client on the memory bridge, with the server end held by the test.
    struct Wire {
        client: Client,
        events: client::Events,
        server: lsp_server::Connection,
        /// The `initialize` request the client sent when it was built.
        initialize: Value,
    }

    impl Wire {
        fn new() -> Self {
            let (near, server) = lsp_server::Connection::memory();
            let (client, events) = Client::builder().root(uri("file:///w/")).memory(near);
            let mut wire = Self {
                client,
                events,
                server,
                initialize: Value::Null,
            };
            let [initialize] = wire
                .sent()
                .try_into()
                .expect("initialize goes out first, alone");
            wire.initialize = initialize;
            wire
        }

        /// What the client sent since the last call, as JSON.
        fn sent(&self) -> Vec<Value> {
            self.server
                .receiver
                .try_iter()
                .map(|message| {
                    serde_json::to_value(message).expect("lsp-server messages serialize")
                })
                .collect()
        }

        /// Sends `message` as the server, then folds in every event the stream holds.
        fn deliver(&mut self, message: Value) -> Vec<Update> {
            let message =
                serde_json::from_value(message).expect("fixtures are lsp-server messages");
            self.server
                .sender
                .send(message)
                .expect("the client end is open");
            let mut updates = Vec::new();
            while let Some(Some(event)) = self.events.next().now_or_never() {
                updates.extend(self.client.receive(event));
            }
            updates
        }
    }

    /// The outgoing messages with `method`.
    fn all_sent(messages: &[Value], method: &str) -> Vec<Value> {
        messages
            .iter()
            .filter(|message| message["method"] == method)
            .cloned()
            .collect()
    }

    /// The first outgoing message with `method`.
    fn sent(messages: &[Value], method: &str) -> Value {
        all_sent(messages, method)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("no {method} was sent"))
    }

    /// The document version a `didOpen` or `didChange` carries.
    fn version(notification: &Value) -> Value {
        notification["params"]["textDocument"]["version"].clone()
    }

    /// A success response to `request`.
    fn reply(request: &Value, result: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": request["id"], "result": result })
    }

    /// An LSP range on line `line`, from column `start` to `end`.
    fn on_line(line: u32, start: u32, end: u32) -> Value {
        json!({
            "start": { "line": line, "character": start },
            "end": { "line": line, "character": end },
        })
    }

    /// A `publishDiagnostics` for `uri` at `version` with one error at `range`.
    fn publish(uri: &str, version: &Value, range: Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": uri,
                "version": version,
                "diagnostics": [{ "range": range, "severity": 1, "message": "unused" }],
            },
        })
    }

    /// A client past the handshake: every provider, incremental sync, and byte columns (utf-8),
    /// so fixture positions are byte offsets.
    fn ready() -> Wire {
        handshake(json!({
            "positionEncoding": "utf-8",
            "textDocumentSync": { "openClose": true, "change": 2, "save": {} },
            "completionProvider": { "triggerCharacters": ["."] },
            "signatureHelpProvider": { "triggerCharacters": ["("] },
            "hoverProvider": true,
            "definitionProvider": true,
            "renameProvider": true,
            "documentFormattingProvider": true,
        }))
    }

    /// The capabilities `ready` negotiates, plus an inlay hint provider that resolves tooltips.
    fn ready_with_hints() -> Wire {
        handshake(json!({
            "positionEncoding": "utf-8",
            "textDocumentSync": { "openClose": true, "change": 2, "save": {} },
            "completionProvider": { "triggerCharacters": ["."] },
            "signatureHelpProvider": { "triggerCharacters": ["("] },
            "hoverProvider": true,
            "definitionProvider": true,
            "renameProvider": true,
            "documentFormattingProvider": true,
            "inlayHintProvider": { "resolveProvider": true },
        }))
    }

    /// A client whose server answered `initialize` with `capabilities`.
    fn handshake(capabilities: Value) -> Wire {
        let mut wire = Wire::new();
        let result = json!({ "capabilities": capabilities });
        let updates = wire.deliver(reply(&wire.initialize.clone(), result));
        assert!(
            matches!(
                updates.last(),
                Some(Update::Status(client::Status::Running))
            ),
            "the handshake runs the client, got {updates:?}",
        );
        assert_eq!(
            all_sent(&wire.sent(), "initialized").len(),
            1,
            "the handshake completes",
        );
        wire
    }

    /// An editor over `text`, opened on `wire`'s client under `path`, with the rename field
    /// enabled, and what the open sent.
    fn opened(wire: &mut Wire, path: &str, text: &str) -> (CodeEditor, Vec<Value>) {
        let mut editor = CodeEditor::new(text).rename(true);
        editor
            .open_lsp(&mut wire.client, &uri(path), "rust")
            .expect("the URI is free");
        (editor, wire.sent())
    }

    /// Feed `event` through the editor's real update path, then sync; returns what went out.
    fn drive(editor: &mut CodeEditor, wire: &mut Wire, event: Event) -> Vec<Value> {
        let _ = editor.update(event, Instant::now());
        let _ = editor.sync_lsp(&mut wire.client);
        wire.sent()
    }

    /// Feed `action` through the editor's update path, then sync, keeping the jump; returns
    /// what went out too.
    fn gesture(
        editor: &mut CodeEditor,
        wire: &mut Wire,
        action: Action,
    ) -> (update::Applied, Vec<Value>) {
        let _ = editor.update(Event::Editor(action), Instant::now());
        let applied = editor.sync_lsp(&mut wire.client);
        (applied, wire.sent())
    }

    /// Fire the editor's scheduled hint fetch, as its widget does once the delay passes, then
    /// sync. Returns what went out.
    fn fetch_inlays(editor: &mut CodeEditor, wire: &mut Wire) -> Vec<Value> {
        let generation = editor.pending_wake().expect("a fetch is scheduled").generation;
        drive(editor, wire, Event::Editor(Action::Wake(generation)))
    }

    /// The hints `editor` shows, as `(render offset, key)`.
    fn shown(editor: &CodeEditor) -> Vec<(u32, Key)> {
        let len = editor.document().buffer().len();
        editor
            .document()
            .inlays_in(0..len)
            .map(|hint| (hint.offset(), hint.key()))
            .collect()
    }

    /// The document updates among `updates`, one per touched document, in order.
    fn documents(updates: Vec<Update>) -> Vec<update::Document> {
        updates
            .into_iter()
            .map(|update| match update {
                Update::Document(document) => document,
                other => panic!("expected document updates, got {other:?}"),
            })
            .collect()
    }

    /// The single document update the server's `message` produces.
    fn one_document(wire: &mut Wire, message: Value) -> update::Document {
        let [document] = documents(wire.deliver(message))
            .try_into()
            .expect("the message touches one document");
        document
    }

    /// The number of diagnostics on `editor`.
    fn diagnostics(editor: &CodeEditor) -> usize {
        let len = editor.document().buffer().len();
        editor.document().diagnostics_in(0..len).count()
    }

    /// A server's diagnostics for the editor's synced version land as squiggles.
    #[test]
    fn diagnostics_publish_lands_in_the_editor() {
        let mut wire = ready();
        let (mut ed, messages) = opened(&mut wire, A, "let x = 1;\n");
        let version = version(&sent(&messages, "textDocument/didOpen"));
        let document = one_document(&mut wire, publish(A, &version, on_line(0, 4, 5)));
        let applied = ed.apply_lsp(&mut wire.client, document);
        assert_eq!(applied.refused, None, "the publish is current");
        assert_eq!(
            ed.document().diagnostics_in(0..11).count(),
            1,
            "one squiggle lands"
        );
    }

    /// A completion reply opens the popup it answers.
    #[test]
    fn completion_reply_opens_the_popup() {
        let mut wire = ready();
        let (mut ed, _) = opened(&mut wire, A, "\n");
        let messages = drive(&mut ed, &mut wire, Event::Editor(Action::Type('g')));
        let request = sent(&messages, "textDocument/completion");
        let result = json!({ "isIncomplete": false, "items": [{ "label": "greet" }] });
        let document = one_document(&mut wire, reply(&request, result));
        let applied = ed.apply_lsp(&mut wire.client, document);
        assert_eq!(applied.refused, None, "the reply is awaited");
        assert!(
            matches!(ed.completion.state(), CompletionState::Open(_)),
            "the popup opens",
        );
    }

    /// A signature help reply opens the signature box.
    #[test]
    fn signature_reply_opens_the_box() {
        let mut wire = ready();
        let (mut ed, _) = opened(&mut wire, A, "\n");
        let typed = drive(&mut ed, &mut wire, Event::Editor(Action::Type('f')));
        assert!(
            all_sent(&typed, "textDocument/signatureHelp").is_empty(),
            "no call is open yet",
        );
        let messages = drive(&mut ed, &mut wire, Event::Editor(Action::Type('(')));
        let request = sent(&messages, "textDocument/signatureHelp");
        let result =
            json!({ "signatures": [{ "label": "f(a)", "parameters": [{ "label": "a" }] }] });
        let document = one_document(&mut wire, reply(&request, result));
        let applied = ed.apply_lsp(&mut wire.client, document);
        assert_eq!(applied.refused, None, "the reply is awaited");
        assert!(ed.signature.is_some(), "the box opens");
    }

    /// A hover reply shows the card for the hovered word.
    #[test]
    fn hover_reply_shows_the_card() {
        let mut wire = ready();
        let (mut ed, _) = opened(&mut wire, A, "hello\n");
        let messages = drive(&mut ed, &mut wire, Event::Editor(Action::HoverQuery(2)));
        let request = sent(&messages, "textDocument/hover");
        let result = json!({ "contents": { "kind": "markdown", "value": "**hi**" } });
        let document = one_document(&mut wire, reply(&request, result));
        let applied = ed.apply_lsp(&mut wire.client, document);
        assert_eq!(applied.refused, None, "the reply is awaited");
        assert!(ed.hover.is_some(), "the card shows");
    }

    /// A definition in the same document selects it, with nothing for the host to route.
    #[test]
    fn local_definition_selects_the_target() {
        let mut wire = ready();
        let (mut ed, _) = opened(&mut wire, A, "fn f() {}\nf();\n");
        let _ = drive(&mut ed, &mut wire, Event::Editor(Action::PlaceCaret(10)));
        let messages = drive(&mut ed, &mut wire, Event::Editor(Action::GotoDefinition));
        let request = sent(&messages, "textDocument/definition");
        let result = json!({ "uri": A, "range": on_line(0, 3, 4) });
        let document = one_document(&mut wire, reply(&request, result));
        let applied = ed.apply_lsp(&mut wire.client, document);
        assert_eq!(applied.refused, None, "the reply is awaited");
        assert_eq!(applied.jump, None, "a local target needs no routing");
        assert_eq!(ed.selection(), 3..4, "the definition is selected");
    }

    /// Format edits apply as one transaction, and the same call mirrors them to the server.
    #[test]
    fn format_reply_edits_the_document_and_syncs_it() {
        let mut wire = ready();
        let (mut ed, _) = opened(&mut wire, A, "a  \n");
        let messages = drive(&mut ed, &mut wire, Event::Editor(Action::Format));
        let request = sent(&messages, "textDocument/formatting");
        let result = json!([{ "range": on_line(0, 1, 3), "newText": "" }]);
        let document = one_document(&mut wire, reply(&request, result));
        let applied = ed.apply_lsp(&mut wire.client, document);
        assert_eq!(applied.refused, None, "the edits are current");
        assert_eq!(ed.document().text(), "a\n", "the trailing spaces are gone");
        let change = sent(&wire.sent(), "textDocument/didChange");
        assert_eq!(
            change["params"]["contentChanges"],
            json!([{ "range": on_line(0, 1, 3), "text": "" }]),
            "the server sees the deletion",
        );
    }

    /// Two editors on one client each mirror only their own document, at their own versions.
    #[test]
    fn two_editors_on_one_client_sync_independently() {
        let mut wire = ready();
        let (mut a, opened_a) = opened(&mut wire, A, "a\n");
        let (mut b, opened_b) = opened(&mut wire, B, "b\n");
        let (version_a, version_b) = (
            version(&sent(&opened_a, "textDocument/didOpen")),
            version(&sent(&opened_b, "textDocument/didOpen")),
        );
        for (editor, path, opened_at) in [(&mut a, A, version_a), (&mut b, B, version_b)] {
            let messages = drive(editor, &mut wire, Event::Editor(Action::Type('x')));
            let changes = all_sent(&messages, "textDocument/didChange");
            assert_eq!(changes.len(), 1, "one didChange per typed character");
            assert_eq!(
                changes[0]["params"]["textDocument"]["uri"], path,
                "the change names the typed-in document",
            );
            assert_eq!(
                version(&changes[0]),
                json!(opened_at.as_i64().expect("versions are integers") + 1),
                "the document's own version goes up by one",
            );
        }
    }

    /// Open F2 in `editor`, type `new_name` into the field and submit it; returns the request.
    fn rename(editor: &mut CodeEditor, wire: &mut Wire, new_name: &str) -> Value {
        let _ = drive(editor, wire, Event::Editor(Action::PlaceCaret(1)));
        let _ = drive(editor, wire, Event::Editor(Action::Rename));
        let _ = drive(editor, wire, Event::RenameText(new_name.to_owned()));
        let messages = drive(editor, wire, Event::SubmitRename);
        sent(&messages, "textDocument/rename")
    }

    /// A `documentChanges` rename: `greet` becomes `new_name` in the caller `A` (`old` bytes
    /// at 0) and the callee `B` (after `fn `), at the versions the server was last sent.
    fn renamed(old: u32, new_name: &str, version_a: &Value, version_b: &Value) -> Value {
        json!({ "documentChanges": [
            { "textDocument": { "uri": A, "version": version_a },
              "edits": [{ "range": on_line(0, 0, old), "newText": new_name }] },
            { "textDocument": { "uri": B, "version": version_b },
              "edits": [{ "range": on_line(0, 3, 3 + old), "newText": new_name }] },
        ]})
    }

    /// Apply each document of `updates` to the editor holding it; returns what each sent.
    fn apply_both(
        a: &mut CodeEditor,
        b: &mut CodeEditor,
        wire: &mut Wire,
        updates: Vec<Update>,
    ) -> (Vec<Value>, Vec<Value>) {
        let (mut sent_a, mut sent_b) = (Vec::new(), Vec::new());
        for document in documents(updates) {
            let (editor, sent) = if document.doc_id() == a.document().doc_id() {
                (&mut *a, &mut sent_a)
            } else {
                (&mut *b, &mut sent_b)
            };
            let applied = editor.apply_lsp(&mut wire.client, document);
            assert_eq!(applied.refused, None, "each document's edits are current");
            sent.extend(wire.sent());
        }
        (sent_a, sent_b)
    }

    /// A rename edits the background document too and mirrors it at once, so a second rename,
    /// computed at the new versions, lands in both.
    #[test]
    fn background_rename_emits_its_did_change_then_a_second_rename_lands_in_both() {
        let mut wire = ready();
        let (mut a, opened_a) = opened(&mut wire, A, "greet();\n");
        let (mut b, opened_b) = opened(&mut wire, B, "fn greet() {}\n");
        let request = rename(&mut a, &mut wire, "welcome");
        let result = renamed(
            5,
            "welcome",
            &version(&sent(&opened_a, "textDocument/didOpen")),
            &version(&sent(&opened_b, "textDocument/didOpen")),
        );
        let updates = wire.deliver(reply(&request, result));
        let (sent_a, sent_b) = apply_both(&mut a, &mut b, &mut wire, updates);
        assert_eq!(a.document().text(), "welcome();\n", "the caller is renamed");
        assert_eq!(
            b.document().text(),
            "fn welcome() {}\n",
            "the callee is renamed"
        );
        let changed_b = sent(&sent_b, "textDocument/didChange");
        assert_eq!(
            changed_b["params"]["textDocument"]["uri"], B,
            "the background document's edit reaches the server",
        );

        let request = rename(&mut a, &mut wire, "hail");
        let result = renamed(
            7,
            "hail",
            &version(&sent(&sent_a, "textDocument/didChange")),
            &version(&changed_b),
        );
        let updates = wire.deliver(reply(&request, result));
        let _ = apply_both(&mut a, &mut b, &mut wire, updates);
        assert_eq!(
            a.document().text(),
            "hail();\n",
            "the caller is renamed again"
        );
        assert_eq!(
            b.document().text(),
            "fn hail() {}\n",
            "the callee is renamed again"
        );
    }

    /// Edits logged before `open_lsp` are already in the registered text, so they are never
    /// replayed.
    #[test]
    fn open_lsp_drops_a_stale_change_log() {
        let mut wire = ready();
        let mut ed = CodeEditor::new("\n");
        ed.observe_changes(true);
        let _ = ed.update(Event::Editor(Action::Type('x')), Instant::now());
        ed.open_lsp(&mut wire.client, &uri(A), "rust")
            .expect("the URI is free");
        let _ = wire.sent();
        let messages = drive(&mut ed, &mut wire, Event::Editor(Action::Type('y')));
        let change = sent(&messages, "textDocument/didChange");
        let content = change["params"]["contentChanges"]
            .as_array()
            .expect("contentChanges is a list");
        assert_eq!(content.len(), 1, "only the edit after the open is sent");
        assert_eq!(content[0]["text"], "y", "it is the typed character");
    }

    /// An `open_lsp` the client refuses changes nothing on the editor.
    #[test]
    fn failed_open_lsp_leaves_the_editor_untouched() {
        let mut wire = ready();
        let (_a, _) = opened(&mut wire, A, "a\n");
        let mut b = CodeEditor::new("b\n");
        b.observe_changes(true);
        let _ = b.update(Event::Editor(Action::Type('x')), Instant::now());
        let result = b.open_lsp(&mut wire.client, &uri(A), "rust");
        assert!(
            matches!(result, Err(Error::DuplicateUri { .. })),
            "the URI is taken",
        );
        assert!(b.lsp_client.is_none(), "no client is recorded");
        assert_eq!(b.drain_changes().len(), 1, "the change log is kept");
    }

    /// An update for another document is refused and changes nothing.
    #[test]
    fn apply_lsp_refuses_a_foreign_document() {
        let mut wire = ready();
        let (mut a, _) = opened(&mut wire, A, "a\n");
        let (_b, opened_b) = opened(&mut wire, B, "b\n");
        let version = version(&sent(&opened_b, "textDocument/didOpen"));
        let for_b = one_document(&mut wire, publish(B, &version, on_line(0, 0, 1)));
        let applied = a.apply_lsp(&mut wire.client, for_b);
        assert_eq!(
            applied.refused,
            Some(Refusal::Foreign),
            "B's update is refused by A"
        );
        assert_eq!(diagnostics(&a), 0, "A has no squiggles");
    }

    /// Typing on inside the word adopts the request in flight, whose reply then lands.
    #[test]
    fn typing_twice_before_the_reply_continues_the_completion() {
        let mut wire = ready();
        let (mut ed, _) = opened(&mut wire, A, "\n");
        let first = drive(&mut ed, &mut wire, Event::Editor(Action::Type('g')));
        let request = sent(&first, "textDocument/completion");
        let second = drive(&mut ed, &mut wire, Event::Editor(Action::Type('r')));
        assert!(
            all_sent(&second, "textDocument/completion").is_empty(),
            "the second keystroke asks nothing new",
        );
        assert!(
            all_sent(&second, "$/cancelRequest").is_empty(),
            "the first request is kept",
        );
        let result =
            json!({ "isIncomplete": false, "items": [{ "label": "greet" }, { "label": "grow" }] });
        let document = one_document(&mut wire, reply(&request, result));
        let applied = ed.apply_lsp(&mut wire.client, document);
        assert_eq!(
            applied.refused, None,
            "the first reply answers the second keystroke"
        );
        assert!(
            matches!(ed.completion.state(), CompletionState::Open(_)),
            "the popup opens",
        );
    }

    /// A late local definition, after a click moved the caret, is refused as stale and doesn't
    /// move the selection.
    #[test]
    fn a_click_after_f12_drops_the_late_local_definition() {
        let mut wire = ready();
        let (mut ed, _) = opened(&mut wire, A, "fn f() {}\nf();\n");
        let _ = drive(&mut ed, &mut wire, Event::Editor(Action::PlaceCaret(10)));
        let messages = drive(&mut ed, &mut wire, Event::Editor(Action::GotoDefinition));
        let request = sent(&messages, "textDocument/definition");
        let _ = drive(&mut ed, &mut wire, Event::Editor(Action::PlaceCaret(5)));
        let result = json!({ "uri": A, "range": on_line(0, 3, 4) });
        let document = one_document(&mut wire, reply(&request, result));
        let applied = ed.apply_lsp(&mut wire.client, document);
        assert_eq!(
            applied.refused,
            Some(Refusal::Stale),
            "the click abandoned the request"
        );
        assert_eq!(ed.selection(), 5..5, "the clicked caret stays");
    }

    /// A late definition in another document, after a click, gives the host nothing to route.
    #[test]
    fn a_click_after_f12_drops_the_late_cross_document_jump() {
        let mut wire = ready();
        let (mut a, _) = opened(&mut wire, A, "f();\n");
        let (_b, _) = opened(&mut wire, B, "fn f() {}\n");
        let messages = drive(&mut a, &mut wire, Event::Editor(Action::GotoDefinition));
        let request = sent(&messages, "textDocument/definition");
        let _ = drive(&mut a, &mut wire, Event::Editor(Action::PlaceCaret(2)));
        let result = json!({ "uri": B, "range": on_line(0, 3, 4) });
        let document = one_document(&mut wire, reply(&request, result));
        let applied = a.apply_lsp(&mut wire.client, document);
        assert_eq!(applied.jump, None, "no jump is handed to the host");
        assert_eq!(
            applied.refused,
            Some(Refusal::Stale),
            "the click abandoned the request"
        );
    }

    /// F12 on a call in A, answered in B, and the jump lands in B's editor.
    fn jump_into_b(a: &mut CodeEditor, wire: &mut Wire) -> update::jump::Open {
        let messages = drive(a, wire, Event::Editor(Action::GotoDefinition));
        let request = sent(&messages, "textDocument/definition");
        let result = json!({ "uri": B, "range": on_line(0, 3, 4) });
        let document = one_document(wire, reply(&request, result));
        let applied = a.apply_lsp(&mut wire.client, document);
        assert_eq!(applied.refused, None, "the reply is awaited");
        match applied.jump {
            Some(update::Jump::Open(open)) => open,
            other => panic!("expected a jump into B, got {other:?}"),
        }
    }

    /// A definition in another open document selects the span in that document's editor.
    #[test]
    fn a_cross_document_definition_selects_in_the_target_editor() {
        let mut wire = ready();
        let (mut a, _) = opened(&mut wire, A, "f();\n");
        let (mut b, _) = opened(&mut wire, B, "fn f() {}\n");
        let open = jump_into_b(&mut a, &mut wire);
        let applied = b.jump(&mut wire.client, open).expect("B has not moved");
        assert!(wire.sent().is_empty(), "a selection sends nothing");
        assert!(applied.jump.is_none(), "a selection jumps nowhere else");
        assert_eq!(b.selection(), 3..4, "the definition is selected in B");
    }

    /// A jump into a document edited since the server answered is refused and selects nothing.
    #[test]
    fn jump_refuses_a_target_whose_document_moved() {
        let mut wire = ready();
        let (mut a, _) = opened(&mut wire, A, "f();\n");
        let (mut b, _) = opened(&mut wire, B, "fn f() {}\n");
        let open = jump_into_b(&mut a, &mut wire);
        let _ = b.update(Event::Editor(Action::Type('z')), Instant::now());
        let before = b.selection();
        assert!(
            matches!(b.jump(&mut wire.client, open), Err(Refusal::Stale)),
            "B moved since the answer",
        );
        assert_eq!(b.selection(), before, "nothing is selected");
    }

    /// Before the handshake the client declines requests itself, and `sync_lsp` lands the
    /// declines: nothing goes out, and the definition request is retired.
    #[test]
    fn sync_lsp_lands_local_declines() {
        let mut wire = Wire::new();
        let (mut ed, messages) = opened(&mut wire, A, "abc\n");
        assert!(messages.is_empty(), "the didOpen waits for the handshake");
        let completion = drive(
            &mut ed,
            &mut wire,
            Event::Editor(Action::TriggerCompletion),
        );
        let definition = drive(&mut ed, &mut wire, Event::Editor(Action::GotoDefinition));
        assert!(
            completion.is_empty() && definition.is_empty(),
            "nothing is sent"
        );
        assert!(
            ed.awaiting.definition.is_none(),
            "the declined definition is retired"
        );
        assert!(
            !matches!(ed.completion.state(), CompletionState::Open(_)),
            "the empty list leaves the popup closed",
        );
    }

    /// Format edits that overlap are refused and change nothing.
    #[test]
    fn overlapping_format_edits_are_refused_as_overlap() {
        let mut wire = ready();
        let (mut ed, _) = opened(&mut wire, A, "abcd\n");
        let messages = drive(&mut ed, &mut wire, Event::Editor(Action::Format));
        let request = sent(&messages, "textDocument/formatting");
        let result = json!([
            { "range": on_line(0, 0, 3), "newText": "x" },
            { "range": on_line(0, 1, 4), "newText": "y" },
        ]);
        let document = one_document(&mut wire, reply(&request, result));
        let applied = ed.apply_lsp(&mut wire.client, document);
        assert_eq!(
            applied.refused,
            Some(Refusal::Overlap),
            "the batch is rejected"
        );
        assert_eq!(ed.document().text(), "abcd\n", "nothing is applied");
    }

    /// `open_lsp` lands the diagnostics published before the open; `close_lsp` sends the
    /// `didClose`, clears the squiggles and the hover card over them, forgets the pending
    /// hover request and stops mirroring.
    #[test]
    fn close_lsp_clears_diagnostics_and_sends_did_close() {
        let mut wire = ready();
        let early = wire.deliver(publish(A, &Value::Null, on_line(0, 4, 5)));
        assert!(early.is_empty(), "A is not open yet");
        let (mut ed, _) = opened(&mut wire, A, "let x = 1;\n");
        assert_eq!(diagnostics(&ed), 1, "the open lands the cached squiggle");
        let _ = ed.update(Event::Editor(Action::HoverQuery(4)), Instant::now());
        assert!(ed.hover.is_some(), "the squiggle's card shows");
        ed.close_lsp(&mut wire.client);
        let close = sent(&wire.sent(), "textDocument/didClose");
        assert_eq!(close["params"]["textDocument"]["uri"], A, "A is closed");
        assert_eq!(diagnostics(&ed), 0, "the squiggles are cleared");
        assert!(ed.hover.is_none(), "the hover card closes");
        assert!(
            ed.take_hover_request().is_none(),
            "the hover request is forgotten"
        );
        assert!(ed.lsp_client.is_none(), "no client is recorded");
        let _ = ed.update(Event::Editor(Action::Type('q')), Instant::now());
        assert!(ed.drain_changes().is_empty(), "edits are no longer logged");
        let _ = ed.sync_lsp(&mut wire.client);
        assert!(wire.sent().is_empty(), "a closed editor syncs nothing");
    }

    /// `close_lsp` closes the completion popup and the signature box, and the box stops
    /// re-asking: only a new `(` asks for signature help again.
    #[test]
    fn close_lsp_closes_the_popups_and_stops_signature_requests() {
        let mut wire = ready();
        let (mut ed, _) = opened(&mut wire, A, "\n");
        let _ = drive(&mut ed, &mut wire, Event::Editor(Action::Type('f')));
        let messages = drive(&mut ed, &mut wire, Event::Editor(Action::Type('(')));
        let request = sent(&messages, "textDocument/signatureHelp");
        let result =
            json!({ "signatures": [{ "label": "f(a)", "parameters": [{ "label": "a" }] }] });
        let document = one_document(&mut wire, reply(&request, result));
        assert_eq!(
            ed.apply_lsp(&mut wire.client, document).refused,
            None,
            "the box's reply lands"
        );
        let messages = drive(&mut ed, &mut wire, Event::Editor(Action::Type('g')));
        let request = sent(&messages, "textDocument/completion");
        let result = json!({ "isIncomplete": false, "items": [{ "label": "greet" }] });
        let document = one_document(&mut wire, reply(&request, result));
        assert_eq!(
            ed.apply_lsp(&mut wire.client, document).refused,
            None,
            "the list lands"
        );
        assert!(
            ed.signature.is_some() && matches!(ed.completion.state(), CompletionState::Open(_)),
            "the box and the popup are open before the close",
        );
        ed.close_lsp(&mut wire.client);
        assert!(ed.signature.is_none(), "the signature box closes");
        assert!(
            !matches!(ed.completion.state(), CompletionState::Open(_)),
            "the popup closes",
        );
        let _ = ed.update(Event::Editor(Action::Type('x')), Instant::now());
        assert!(
            ed.take_signature_request().is_none() && ed.awaiting.signature.is_none(),
            "typing inside the call asks for no signature help",
        );
    }

    /// `save_lsp` mirrors the unsynced edit, then saves: the `didSave` follows the `didChange`
    /// in one batch, without the text.
    #[test]
    fn save_lsp_syncs_then_sends_did_save() {
        let mut wire = ready();
        let (mut ed, messages) = opened(&mut wire, A, "\n");
        let opened_at = version(&sent(&messages, "textDocument/didOpen"));
        let _ = ed.update(Event::Editor(Action::Type(';')), Instant::now());
        let _ = ed.save_lsp(&mut wire.client);
        let messages = wire.sent();
        let methods: Vec<&Value> = messages.iter().map(|message| &message["method"]).collect();
        assert_eq!(
            methods,
            ["textDocument/didChange", "textDocument/didSave"],
            "the edit syncs before the save",
        );
        assert_eq!(
            version(&messages[0]),
            json!(opened_at.as_i64().expect("versions are numbers") + 1),
            "the didChange carries the next version",
        );
        assert_eq!(
            messages[1]["params"],
            json!({ "textDocument": { "uri": A } }),
            "the didSave names A and carries no text",
        );
    }

    /// `save_lsp` on an editor that was never opened sends nothing.
    #[test]
    fn save_lsp_on_an_unregistered_editor_does_nothing() {
        let mut wire = ready();
        let mut ed = CodeEditor::new("\n");
        let _ = ed.update(Event::Editor(Action::Type(';')), Instant::now());
        let _ = ed.save_lsp(&mut wire.client);
        assert!(wire.sent().is_empty(), "nothing is sent");
    }

    /// `sync_lsp` on an editor that was never opened sends nothing and keeps its requests.
    #[test]
    fn sync_lsp_on_an_unregistered_editor_does_nothing() {
        let mut wire = ready();
        let mut ed = CodeEditor::new("\n");
        let _ = ed.update(Event::Editor(Action::Type('g')), Instant::now());
        let _ = ed.sync_lsp(&mut wire.client);
        assert!(wire.sent().is_empty(), "nothing is sent");
        assert!(
            ed.take_completion_request().is_some(),
            "the recorded request is left for the host",
        );
    }

    /// A `let` whose `x` the `hinted` answer annotates.
    const LET_X: &str = "let x = f();\n";

    /// `: Foo` after `x`: insertable, and `Foo` links into a file no editor opened.
    fn hinted() -> Value {
        json!([{
            "position": { "line": 0, "character": 5 },
            "kind": 1,
            "label": [
                { "value": ": " },
                {
                    "value": "Foo",
                    "location": { "uri": "file:///w/foo.rs", "range": on_line(0, 7, 10) },
                },
            ],
            "textEdits": [{ "range": on_line(0, 5, 5), "newText": ": Foo" }],
        }])
    }

    /// An editor over `LET_X` with hints on, opened on a hint-aware client, its hints fetched
    /// and landed. Returns the editor, the `didOpen`'s version and the hint's key.
    fn with_hint(wire: &mut Wire) -> (CodeEditor, Value, Key) {
        let mut ed = CodeEditor::new(LET_X).inlay_hints(true);
        ed.open_lsp(&mut wire.client, &uri(A), "rust")
            .expect("the URI is free");
        let opened = wire.sent();
        let request = sent(&fetch_inlays(&mut ed, wire), "textDocument/inlayHint");
        let document = one_document(wire, reply(&request, hinted()));
        let applied = ed.apply_lsp(&mut wire.client, document);
        assert_eq!(applied.refused, None, "the hints are current");
        let [(offset, key)] = shown(&ed)[..] else {
            panic!("one hint shows, got {:?}", shown(&ed));
        };
        assert_eq!(offset, 5, "the hint renders after `x`");
        (ed, version(&sent(&opened, "textDocument/didOpen")), key)
    }

    /// A Ctrl+click on a label part that points into a file nobody opened comes back from
    /// `sync_lsp` itself, with no request to the server.
    #[test]
    fn sync_lsp_returns_an_inlay_label_jump_into_an_unopened_file() {
        let mut wire = ready_with_hints();
        let (mut ed, _, key) = with_hint(&mut wire);
        let (applied, messages) = gesture(&mut ed, &mut wire, Action::InlayJump { key, part: 1 });
        match &applied.jump {
            Some(update::Jump::Unopened(unopened)) => assert_eq!(
                unopened.uri().as_str(),
                "file:///w/foo.rs",
                "the jump names the part's file",
            ),
            other => panic!("expected a jump into an unopened file, got {other:?}"),
        }
        assert_eq!(applied.refused, None, "a sync refuses nothing");
        assert!(
            all_sent(&messages, "textDocument/definition").is_empty(),
            "the client answers from the hint, without asking the server",
        );
    }

    /// A double-clicked hint inserts its text once: the hint goes before the text lands, and the
    /// edit's didChange leaves in the same `sync_lsp` call.
    #[test]
    fn an_inlay_insert_lands_once_and_syncs_in_the_same_call() {
        let mut wire = ready_with_hints();
        let (mut ed, opened_at, key) = with_hint(&mut wire);
        let (applied, messages) =
            gesture(&mut ed, &mut wire, Action::InlayInsert { key, offset: 5 });
        assert_eq!(ed.document().text(), "let x: Foo = f();\n", "the hint's text is inserted");
        assert!(shown(&ed).is_empty(), "the inserted hint no longer shows");
        let change = sent(&messages, "textDocument/didChange");
        assert_eq!(
            version(&change),
            json!(opened_at.as_i64().expect("versions are integers") + 1),
            "the insert is synced at the next version",
        );
        assert!(applied.jump.is_none(), "an insert jumps nowhere");
    }

    /// `close_lsp` drops the shown hints and every inlay slot, so nothing waits on a reply that
    /// can no longer land.
    #[test]
    fn close_lsp_clears_the_hints_and_their_slots() {
        let mut wire = ready_with_hints();
        let (mut ed, _, key) = with_hint(&mut wire);
        let _ = ed.update(
            Event::Editor(Action::InlayHover { key, part: 1 }),
            Instant::now(),
        );
        ed.wait_inlays(INLAY_EDIT_DELAY, None);
        assert!(
            ed.awaiting.inlay_tooltip.is_some() && ed.pending_wake().is_some(),
            "a tooltip and a fetch are pending before the close",
        );
        ed.close_lsp(&mut wire.client);
        assert!(shown(&ed).is_empty(), "the hints are cleared");
        assert!(ed.take_inlay_interaction().is_none(), "the hover gesture is forgotten");
        assert!(ed.take_inlay_request().is_none(), "no fetch is left to pull");
        assert!(ed.pending_wake().is_none(), "the scheduled fetch is dropped");
        assert!(
            ed.awaiting.inlays.is_none()
                && ed.awaiting.inlay_tooltip.is_none()
                && ed.awaiting.inlay_insert.is_none(),
            "every inlay slot is settled",
        );
    }
}
