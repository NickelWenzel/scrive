//! A [`CodeEditor`]'s side of a [`scrive_lsp::Client`].
//!
//! A host calls [`open_lsp`](CodeEditor::open_lsp) once per document,
//! [`sync_lsp`](CodeEditor::sync_lsp) after every [`update`](CodeEditor::update), and
//! [`apply_lsp`](CodeEditor::apply_lsp) for each `Update::Document` the client returns, and
//! [`save_lsp`](CodeEditor::save_lsp) after writing the document to disk. Each call returns the
//! messages to send; the transport stays the host's.

use scrive_core::DiagnosticsOutcome;
use scrive_lsp::lsp_types::Uri;
use scrive_lsp::update::{self, Change, Refusal, Stamp, Target};
use scrive_lsp::{Client, Error, Message, Output, Update};

use super::{Awaited, CodeEditor, INLAY_EDIT_DELAY};

impl CodeEditor {
    /// Register this editor's document with `client` under `uri`, in language `language`, and
    /// start mirroring its edits. Returns the messages to send: a `didOpen`, once the client is
    /// initialized.
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
    ) -> Result<Vec<Message>, Error> {
        debug_assert!(
            self.lsp_client.is_none_or(|id| id == client.id()),
            "a CodeEditor talks to one Client: close_lsp before opening with another",
        );
        let snapshot = self.doc.snapshot();
        let output = client.open(&snapshot, uri, language)?;
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
        Ok(self.route(output))
    }

    /// Mirror every edit since the last sync to `client`, then send each request the editor
    /// recorded: completion, signature help, hover, definition, rename and format. Call it
    /// after every [`update`](CodeEditor::update); with nothing new it returns no messages.
    /// Answers the client gives without asking the server, such as declines and reused
    /// completion lists, land before it returns.
    ///
    /// On an editor that is not registered, before [`open_lsp`](CodeEditor::open_lsp) or after
    /// [`close_lsp`](CodeEditor::close_lsp), it does nothing and returns no messages. Its
    /// `client` must be the one the editor was opened with.
    #[must_use = "the messages must be sent to the server"]
    pub fn sync_lsp(&mut self, client: &mut Client) -> Vec<Message> {
        let Some(registered) = self.lsp_client else {
            return Vec::new();
        };
        debug_assert_eq!(
            registered,
            client.id(),
            "sync_lsp needs the Client this editor was opened with",
        );
        let snapshot = self.doc.snapshot();
        // The client ignores a request from a revision it has not been synced to.
        let mut outputs = vec![client.sync(&snapshot, self.doc.drain_changes())];
        if let Some(request) = self.take_completion_request() {
            outputs.push(client.complete(&snapshot, &request));
        }
        if let Some(request) = self.take_signature_request() {
            outputs.push(client.signature_help(&snapshot, &request));
        }
        if let Some(request) = self.take_hover_request() {
            outputs.push(client.hover(&snapshot, &request));
        }
        if let Some(request) = self.take_definition_request() {
            outputs.push(client.definition(&snapshot, &request));
        }
        if let Some(request) = self.take_rename_request() {
            outputs.push(client.rename(&snapshot, &request));
        }
        if let Some(request) = self.take_format_request() {
            outputs.push(client.format(&snapshot, &request));
        }
        let mut messages = Vec::new();
        for output in outputs {
            messages.extend(self.route(output));
        }
        messages
    }

    /// Tell `client` that the document was saved, syncing first so the server holds the saved
    /// text. Call it after writing the document to disk. Returns the messages to send: the
    /// sync's, then a `didSave` when the server asks for saves.
    ///
    /// On an editor that is not registered it does nothing and returns no messages.
    #[must_use = "the didChange and didSave must be sent to the server"]
    pub fn save_lsp(&mut self, client: &mut Client) -> Vec<Message> {
        if self.lsp_client.is_none() {
            return Vec::new();
        }
        let mut messages = self.sync_lsp(client);
        let output = client.save(&self.doc.snapshot());
        messages.extend(self.route(output));
        messages
    }

    /// Apply one document update from [`Client::receive`], then sync. The update is refused
    /// when it is for another document, stale, or an edit batch that does not apply. A
    /// definition in another document comes back as [`Applied::jump`](update::Applied::jump)
    /// for the host to route.
    #[must_use = "Applied.messages must be sent, and Applied.jump routed"]
    pub fn apply_lsp(
        &mut self,
        client: &mut Client,
        document: update::Document,
    ) -> update::Applied {
        let mut applied = self.land(document);
        applied.messages = self.sync_lsp(client);
        applied
    }

    /// Select a definition that another editor's [`apply_lsp`](CodeEditor::apply_lsp) returned
    /// as [`Jump::Open`](update::Jump::Open), then sync. Returns the messages to send.
    ///
    /// # Errors
    /// [`Refusal::Foreign`] when `open` is in another document, and [`Refusal::Stale`] when
    /// this one has changed since the server answered. Nothing is selected either way.
    pub fn jump(
        &mut self,
        client: &mut Client,
        open: update::jump::Open,
    ) -> Result<Vec<Message>, Refusal> {
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
    /// forgotten, and the server's diagnostics are cleared.
    /// Returns the messages to send: the `didClose`, and cancellations for requests in flight.
    /// F2 stays as [`rename`](CodeEditor::rename) set it, for a later
    /// [`open_lsp`](CodeEditor::open_lsp).
    #[must_use = "the didClose must be sent to the server"]
    pub fn close_lsp(&mut self, client: &mut Client) -> Vec<Message> {
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
        let _ = self.set_diagnostics(self.doc.revision(), Vec::new());
        self.lsp_client = None;
        let output = client.close(self.doc.doc_id());
        debug_assert!(
            output.updates.is_empty(),
            "close answers with messages only"
        );
        output.messages
    }

    /// Land `output`'s updates and return its messages. `open` and the request methods answer
    /// only the document they were given, under its current ticket or revision.
    fn route(&mut self, output: Output) -> Vec<Message> {
        let Output { messages, updates } = output;
        debug_assert!(
            updates
                .iter()
                .all(|update| matches!(update, Update::Document(_))),
            "open and requests answer with document updates only",
        );
        for update in updates {
            if let Update::Document(document) = update {
                let applied = self.land(document);
                debug_assert!(
                    applied.jump.is_none(),
                    "a local answer never points into another document",
                );
            }
        }
        messages
    }

    /// Check `document`'s identity and stamp against this editor, then apply its change. It
    /// never syncs, so `route` can land through it from inside `sync_lsp`.
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

/// An update that was not applied, and why.
fn refused(refusal: Refusal) -> update::Applied {
    update::Applied {
        refused: Some(refusal),
        ..update::Applied::default()
    }
}

#[cfg(test)]
mod tests {
    use iced::time::Instant;
    use scrive_core::CompletionState;
    use serde_json::{json, Value};

    use scrive_lsp::lsp_types::Uri;
    use scrive_lsp::update::{self, Refusal};
    use scrive_lsp::{Client, Error, Message, Output, Update};

    use crate::editor::Action;
    use crate::{CodeEditor, Event};

    const A: &str = "file:///w/a.rs";
    const B: &str = "file:///w/b.rs";

    fn uri(text: &str) -> Uri {
        text.parse().expect("test URIs parse")
    }

    /// A message as the JSON it serializes to: how the tests read requests.
    fn wire(message: &Message) -> Value {
        serde_json::to_value(message).expect("envelopes serialize")
    }

    /// A JSON fixture as an envelope.
    fn envelope(value: Value) -> Message {
        serde_json::from_value(value).expect("fixtures are envelopes")
    }

    /// The outgoing messages with `method`, as JSON.
    fn all_sent(messages: &[Message], method: &str) -> Vec<Value> {
        messages
            .iter()
            .map(wire)
            .filter(|message| message["method"] == method)
            .collect()
    }

    /// The first outgoing message with `method`, as JSON.
    fn sent(messages: &[Message], method: &str) -> Value {
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
    fn reply(request: &Value, result: Value) -> Message {
        envelope(json!({ "jsonrpc": "2.0", "id": request["id"], "result": result }))
    }

    /// An LSP range on line `line`, from column `start` to `end`.
    fn on_line(line: u32, start: u32, end: u32) -> Value {
        json!({
            "start": { "line": line, "character": start },
            "end": { "line": line, "character": end },
        })
    }

    /// A `publishDiagnostics` for `uri` at `version` with one error at `range`.
    fn publish(uri: &str, version: &Value, range: Value) -> Message {
        envelope(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": uri,
                "version": version,
                "diagnostics": [{ "range": range, "severity": 1, "message": "unused" }],
            },
        }))
    }

    /// A client past the handshake: every provider, incremental sync, and byte columns (utf-8),
    /// so fixture positions are byte offsets.
    fn ready() -> Client {
        let (mut client, initialize) = Client::builder().root(uri("file:///w/")).build();
        let result = json!({ "capabilities": {
            "positionEncoding": "utf-8",
            "textDocumentSync": { "openClose": true, "change": 2, "save": {} },
            "completionProvider": { "triggerCharacters": ["."] },
            "signatureHelpProvider": { "triggerCharacters": ["("] },
            "hoverProvider": true,
            "definitionProvider": true,
            "renameProvider": true,
            "documentFormattingProvider": true,
        }});
        let output = client
            .receive(reply(&wire(&initialize), result))
            .expect("the initialize result decodes");
        assert_eq!(
            all_sent(&output.messages, "initialized").len(),
            1,
            "the handshake completes",
        );
        client
    }

    /// An editor over `text`, opened on `client` under `path`, with the rename field enabled.
    fn opened(client: &mut Client, path: &str, text: &str) -> (CodeEditor, Vec<Message>) {
        let mut editor = CodeEditor::new(text).rename(true);
        let messages = editor
            .open_lsp(client, &uri(path), "rust")
            .expect("the URI is free");
        (editor, messages)
    }

    /// Feed `event` through the editor's real update path, then sync.
    fn drive(editor: &mut CodeEditor, client: &mut Client, event: Event) -> Vec<Message> {
        let _ = editor.update(event, Instant::now());
        editor.sync_lsp(client)
    }

    /// The document updates in `output`, one per touched document, in order.
    fn documents(output: Output) -> Vec<update::Document> {
        output
            .updates
            .into_iter()
            .map(|update| match update {
                Update::Document(document) => document,
                other => panic!("expected document updates, got {other:?}"),
            })
            .collect()
    }

    /// The single document update the server's `message` produces.
    fn one_document(client: &mut Client, message: Message) -> update::Document {
        let output = client.receive(message).expect("the fixture is accepted");
        let [document] = documents(output)
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
        let mut client = ready();
        let (mut ed, messages) = opened(&mut client, A, "let x = 1;\n");
        let version = version(&sent(&messages, "textDocument/didOpen"));
        let document = one_document(&mut client, publish(A, &version, on_line(0, 4, 5)));
        let applied = ed.apply_lsp(&mut client, document);
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
        let mut client = ready();
        let (mut ed, _) = opened(&mut client, A, "\n");
        let messages = drive(&mut ed, &mut client, Event::Editor(Action::Type('g')));
        let request = sent(&messages, "textDocument/completion");
        let result = json!({ "isIncomplete": false, "items": [{ "label": "greet" }] });
        let document = one_document(&mut client, reply(&request, result));
        let applied = ed.apply_lsp(&mut client, document);
        assert_eq!(applied.refused, None, "the reply is awaited");
        assert!(
            matches!(ed.completion.state(), CompletionState::Open(_)),
            "the popup opens",
        );
    }

    /// A signature help reply opens the signature box.
    #[test]
    fn signature_reply_opens_the_box() {
        let mut client = ready();
        let (mut ed, _) = opened(&mut client, A, "\n");
        let typed = drive(&mut ed, &mut client, Event::Editor(Action::Type('f')));
        assert!(
            all_sent(&typed, "textDocument/signatureHelp").is_empty(),
            "no call is open yet",
        );
        let messages = drive(&mut ed, &mut client, Event::Editor(Action::Type('(')));
        let request = sent(&messages, "textDocument/signatureHelp");
        let result =
            json!({ "signatures": [{ "label": "f(a)", "parameters": [{ "label": "a" }] }] });
        let document = one_document(&mut client, reply(&request, result));
        let applied = ed.apply_lsp(&mut client, document);
        assert_eq!(applied.refused, None, "the reply is awaited");
        assert!(ed.signature.is_some(), "the box opens");
    }

    /// A hover reply shows the card for the hovered word.
    #[test]
    fn hover_reply_shows_the_card() {
        let mut client = ready();
        let (mut ed, _) = opened(&mut client, A, "hello\n");
        let messages = drive(&mut ed, &mut client, Event::Editor(Action::HoverQuery(2)));
        let request = sent(&messages, "textDocument/hover");
        let result = json!({ "contents": { "kind": "markdown", "value": "**hi**" } });
        let document = one_document(&mut client, reply(&request, result));
        let applied = ed.apply_lsp(&mut client, document);
        assert_eq!(applied.refused, None, "the reply is awaited");
        assert!(ed.hover.is_some(), "the card shows");
    }

    /// A definition in the same document selects it, with nothing for the host to route.
    #[test]
    fn local_definition_selects_the_target() {
        let mut client = ready();
        let (mut ed, _) = opened(&mut client, A, "fn f() {}\nf();\n");
        let _ = drive(&mut ed, &mut client, Event::Editor(Action::PlaceCaret(10)));
        let messages = drive(&mut ed, &mut client, Event::Editor(Action::GotoDefinition));
        let request = sent(&messages, "textDocument/definition");
        let result = json!({ "uri": A, "range": on_line(0, 3, 4) });
        let document = one_document(&mut client, reply(&request, result));
        let applied = ed.apply_lsp(&mut client, document);
        assert_eq!(applied.refused, None, "the reply is awaited");
        assert_eq!(applied.jump, None, "a local target needs no routing");
        assert_eq!(ed.selection(), 3..4, "the definition is selected");
    }

    /// Format edits apply as one transaction, and the same call mirrors them to the server.
    #[test]
    fn format_reply_edits_the_document_and_syncs_it() {
        let mut client = ready();
        let (mut ed, _) = opened(&mut client, A, "a  \n");
        let messages = drive(&mut ed, &mut client, Event::Editor(Action::Format));
        let request = sent(&messages, "textDocument/formatting");
        let result = json!([{ "range": on_line(0, 1, 3), "newText": "" }]);
        let document = one_document(&mut client, reply(&request, result));
        let applied = ed.apply_lsp(&mut client, document);
        assert_eq!(applied.refused, None, "the edits are current");
        assert_eq!(ed.document().text(), "a\n", "the trailing spaces are gone");
        let change = sent(&applied.messages, "textDocument/didChange");
        assert_eq!(
            change["params"]["contentChanges"],
            json!([{ "range": on_line(0, 1, 3), "text": "" }]),
            "the server sees the deletion",
        );
    }

    /// Two editors on one client each mirror only their own document, at their own versions.
    #[test]
    fn two_editors_on_one_client_sync_independently() {
        let mut client = ready();
        let (mut a, opened_a) = opened(&mut client, A, "a\n");
        let (mut b, opened_b) = opened(&mut client, B, "b\n");
        let (version_a, version_b) = (
            version(&sent(&opened_a, "textDocument/didOpen")),
            version(&sent(&opened_b, "textDocument/didOpen")),
        );
        for (editor, path, opened_at) in [(&mut a, A, version_a), (&mut b, B, version_b)] {
            let messages = drive(editor, &mut client, Event::Editor(Action::Type('x')));
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
    fn rename(editor: &mut CodeEditor, client: &mut Client, new_name: &str) -> Value {
        let _ = drive(editor, client, Event::Editor(Action::PlaceCaret(1)));
        let _ = drive(editor, client, Event::Editor(Action::Rename));
        let _ = drive(editor, client, Event::RenameText(new_name.to_owned()));
        let messages = drive(editor, client, Event::SubmitRename);
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

    /// Apply each document of `output` to the editor holding it; returns what each sent.
    fn apply_both(
        a: &mut CodeEditor,
        b: &mut CodeEditor,
        client: &mut Client,
        output: Output,
    ) -> (Vec<Message>, Vec<Message>) {
        let (mut sent_a, mut sent_b) = (Vec::new(), Vec::new());
        for document in documents(output) {
            let (editor, sent) = if document.doc_id() == a.document().doc_id() {
                (&mut *a, &mut sent_a)
            } else {
                (&mut *b, &mut sent_b)
            };
            let applied = editor.apply_lsp(client, document);
            assert_eq!(applied.refused, None, "each document's edits are current");
            sent.extend(applied.messages);
        }
        (sent_a, sent_b)
    }

    /// A rename edits the background document too and mirrors it at once, so a second rename,
    /// computed at the new versions, lands in both.
    #[test]
    fn background_rename_emits_its_did_change_then_a_second_rename_lands_in_both() {
        let mut client = ready();
        let (mut a, opened_a) = opened(&mut client, A, "greet();\n");
        let (mut b, opened_b) = opened(&mut client, B, "fn greet() {}\n");
        let request = rename(&mut a, &mut client, "welcome");
        let result = renamed(
            5,
            "welcome",
            &version(&sent(&opened_a, "textDocument/didOpen")),
            &version(&sent(&opened_b, "textDocument/didOpen")),
        );
        let output = client
            .receive(reply(&request, result))
            .expect("the rename is current");
        let (sent_a, sent_b) = apply_both(&mut a, &mut b, &mut client, output);
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

        let request = rename(&mut a, &mut client, "hail");
        let result = renamed(
            7,
            "hail",
            &version(&sent(&sent_a, "textDocument/didChange")),
            &version(&changed_b),
        );
        let output = client
            .receive(reply(&request, result))
            .expect("the second rename is current");
        let _ = apply_both(&mut a, &mut b, &mut client, output);
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
        let mut client = ready();
        let mut ed = CodeEditor::new("\n");
        ed.observe_changes(true);
        let _ = ed.update(Event::Editor(Action::Type('x')), Instant::now());
        let _opened = ed
            .open_lsp(&mut client, &uri(A), "rust")
            .expect("the URI is free");
        let messages = drive(&mut ed, &mut client, Event::Editor(Action::Type('y')));
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
        let mut client = ready();
        let (_a, _) = opened(&mut client, A, "a\n");
        let mut b = CodeEditor::new("b\n");
        b.observe_changes(true);
        let _ = b.update(Event::Editor(Action::Type('x')), Instant::now());
        let result = b.open_lsp(&mut client, &uri(A), "rust");
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
        let mut client = ready();
        let (mut a, _) = opened(&mut client, A, "a\n");
        let (_b, opened_b) = opened(&mut client, B, "b\n");
        let version = version(&sent(&opened_b, "textDocument/didOpen"));
        let for_b = one_document(&mut client, publish(B, &version, on_line(0, 0, 1)));
        let applied = a.apply_lsp(&mut client, for_b);
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
        let mut client = ready();
        let (mut ed, _) = opened(&mut client, A, "\n");
        let first = drive(&mut ed, &mut client, Event::Editor(Action::Type('g')));
        let request = sent(&first, "textDocument/completion");
        let second = drive(&mut ed, &mut client, Event::Editor(Action::Type('r')));
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
        let document = one_document(&mut client, reply(&request, result));
        let applied = ed.apply_lsp(&mut client, document);
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
        let mut client = ready();
        let (mut ed, _) = opened(&mut client, A, "fn f() {}\nf();\n");
        let _ = drive(&mut ed, &mut client, Event::Editor(Action::PlaceCaret(10)));
        let messages = drive(&mut ed, &mut client, Event::Editor(Action::GotoDefinition));
        let request = sent(&messages, "textDocument/definition");
        let _ = drive(&mut ed, &mut client, Event::Editor(Action::PlaceCaret(5)));
        let result = json!({ "uri": A, "range": on_line(0, 3, 4) });
        let document = one_document(&mut client, reply(&request, result));
        let applied = ed.apply_lsp(&mut client, document);
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
        let mut client = ready();
        let (mut a, _) = opened(&mut client, A, "f();\n");
        let (_b, _) = opened(&mut client, B, "fn f() {}\n");
        let messages = drive(&mut a, &mut client, Event::Editor(Action::GotoDefinition));
        let request = sent(&messages, "textDocument/definition");
        let _ = drive(&mut a, &mut client, Event::Editor(Action::PlaceCaret(2)));
        let result = json!({ "uri": B, "range": on_line(0, 3, 4) });
        let document = one_document(&mut client, reply(&request, result));
        let applied = a.apply_lsp(&mut client, document);
        assert_eq!(applied.jump, None, "no jump is handed to the host");
        assert_eq!(
            applied.refused,
            Some(Refusal::Stale),
            "the click abandoned the request"
        );
    }

    /// F12 on a call in A, answered in B, and the jump lands in B's editor.
    fn jump_into_b(a: &mut CodeEditor, client: &mut Client) -> update::jump::Open {
        let messages = drive(a, client, Event::Editor(Action::GotoDefinition));
        let request = sent(&messages, "textDocument/definition");
        let result = json!({ "uri": B, "range": on_line(0, 3, 4) });
        let document = one_document(client, reply(&request, result));
        let applied = a.apply_lsp(client, document);
        assert_eq!(applied.refused, None, "the reply is awaited");
        match applied.jump {
            Some(update::Jump::Open(open)) => open,
            other => panic!("expected a jump into B, got {other:?}"),
        }
    }

    /// A definition in another open document selects the span in that document's editor.
    #[test]
    fn a_cross_document_definition_selects_in_the_target_editor() {
        let mut client = ready();
        let (mut a, _) = opened(&mut client, A, "f();\n");
        let (mut b, _) = opened(&mut client, B, "fn f() {}\n");
        let open = jump_into_b(&mut a, &mut client);
        let messages = b.jump(&mut client, open).expect("B has not moved");
        assert!(messages.is_empty(), "a selection sends nothing");
        assert_eq!(b.selection(), 3..4, "the definition is selected in B");
    }

    /// A jump into a document edited since the server answered is refused and selects nothing.
    #[test]
    fn jump_refuses_a_target_whose_document_moved() {
        let mut client = ready();
        let (mut a, _) = opened(&mut client, A, "f();\n");
        let (mut b, _) = opened(&mut client, B, "fn f() {}\n");
        let open = jump_into_b(&mut a, &mut client);
        let _ = b.update(Event::Editor(Action::Type('z')), Instant::now());
        let before = b.selection();
        assert_eq!(
            b.jump(&mut client, open),
            Err(Refusal::Stale),
            "B moved since the answer",
        );
        assert_eq!(b.selection(), before, "nothing is selected");
    }

    /// Before the handshake the client declines requests itself, and `sync_lsp` lands the
    /// declines: nothing goes out, and the definition request is retired.
    #[test]
    fn sync_lsp_lands_local_declines() {
        let (mut client, _initialize) = Client::builder().root(uri("file:///w/")).build();
        let (mut ed, messages) = opened(&mut client, A, "abc\n");
        assert!(messages.is_empty(), "the didOpen waits for the handshake");
        let completion = drive(
            &mut ed,
            &mut client,
            Event::Editor(Action::TriggerCompletion),
        );
        let definition = drive(&mut ed, &mut client, Event::Editor(Action::GotoDefinition));
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
        let mut client = ready();
        let (mut ed, _) = opened(&mut client, A, "abcd\n");
        let messages = drive(&mut ed, &mut client, Event::Editor(Action::Format));
        let request = sent(&messages, "textDocument/formatting");
        let result = json!([
            { "range": on_line(0, 0, 3), "newText": "x" },
            { "range": on_line(0, 1, 4), "newText": "y" },
        ]);
        let document = one_document(&mut client, reply(&request, result));
        let applied = ed.apply_lsp(&mut client, document);
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
        let mut client = ready();
        let early = client
            .receive(publish(A, &Value::Null, on_line(0, 4, 5)))
            .expect("the publish decodes");
        assert!(early.updates.is_empty(), "A is not open yet");
        let (mut ed, _) = opened(&mut client, A, "let x = 1;\n");
        assert_eq!(diagnostics(&ed), 1, "the open lands the cached squiggle");
        let _ = ed.update(Event::Editor(Action::HoverQuery(4)), Instant::now());
        assert!(ed.hover.is_some(), "the squiggle's card shows");
        let closed = ed.close_lsp(&mut client);
        let close = sent(&closed, "textDocument/didClose");
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
        assert!(
            ed.sync_lsp(&mut client).is_empty(),
            "a closed editor syncs nothing"
        );
    }

    /// `close_lsp` closes the completion popup and the signature box, and the box stops
    /// re-asking: only a new `(` asks for signature help again.
    #[test]
    fn close_lsp_closes_the_popups_and_stops_signature_requests() {
        let mut client = ready();
        let (mut ed, _) = opened(&mut client, A, "\n");
        let _ = drive(&mut ed, &mut client, Event::Editor(Action::Type('f')));
        let messages = drive(&mut ed, &mut client, Event::Editor(Action::Type('(')));
        let request = sent(&messages, "textDocument/signatureHelp");
        let result =
            json!({ "signatures": [{ "label": "f(a)", "parameters": [{ "label": "a" }] }] });
        let document = one_document(&mut client, reply(&request, result));
        assert_eq!(
            ed.apply_lsp(&mut client, document).refused,
            None,
            "the box's reply lands"
        );
        let messages = drive(&mut ed, &mut client, Event::Editor(Action::Type('g')));
        let request = sent(&messages, "textDocument/completion");
        let result = json!({ "isIncomplete": false, "items": [{ "label": "greet" }] });
        let document = one_document(&mut client, reply(&request, result));
        assert_eq!(
            ed.apply_lsp(&mut client, document).refused,
            None,
            "the list lands"
        );
        assert!(
            ed.signature.is_some() && matches!(ed.completion.state(), CompletionState::Open(_)),
            "the box and the popup are open before the close",
        );
        let _closed = ed.close_lsp(&mut client);
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
        let mut client = ready();
        let (mut ed, messages) = opened(&mut client, A, "\n");
        let opened_at = version(&sent(&messages, "textDocument/didOpen"));
        let _ = ed.update(Event::Editor(Action::Type(';')), Instant::now());
        let messages: Vec<Value> = ed.save_lsp(&mut client).iter().map(wire).collect();
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
        let mut client = ready();
        let mut ed = CodeEditor::new("\n");
        let _ = ed.update(Event::Editor(Action::Type(';')), Instant::now());
        assert!(ed.save_lsp(&mut client).is_empty(), "nothing is sent");
    }

    /// `sync_lsp` on an editor that was never opened sends nothing and keeps its requests.
    #[test]
    fn sync_lsp_on_an_unregistered_editor_does_nothing() {
        let mut client = ready();
        let mut ed = CodeEditor::new("\n");
        let _ = ed.update(Event::Editor(Action::Type('g')), Instant::now());
        assert!(ed.sync_lsp(&mut client).is_empty(), "nothing is sent");
        assert!(
            ed.take_completion_request().is_some(),
            "the recorded request is left for the host",
        );
    }
}
