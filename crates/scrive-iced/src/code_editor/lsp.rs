//! A [`CodeEditor`]'s side of a [`scrive_lsp::Client`].
//!
//! A host calls [`open_lsp`](CodeEditor::open_lsp) once per document and
//! [`sync_lsp`](CodeEditor::sync_lsp) after every [`update`](CodeEditor::update). Each call
//! returns the messages to send; the transport stays the host's.

use scrive_core::DiagnosticsOutcome;
use scrive_lsp::lsp_types::Uri;
use scrive_lsp::update::{self, Change, Refusal, Stamp, Target};
use scrive_lsp::{Client, Error, Message, Output, Update};

use super::{Awaited, CodeEditor};

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
        // The client stamps diagnostics and rename edits with a revision, and every other
        // change with a ticket; any other pairing is treated as stale.
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
    use scrive_lsp::{Client, Error, Message};

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
            "textDocumentSync": { "openClose": true, "change": 2 },
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

    /// The number of diagnostics on `editor`.
    fn diagnostics(editor: &CodeEditor) -> usize {
        let len = editor.document().buffer().len();
        editor.document().diagnostics_in(0..len).count()
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

    /// A `documentChanges` rename: `greet` becomes `new_name` in the caller `A` (`old` bytes
    /// A rename edits the background document too and mirrors it at once, so a second rename,
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

    /// A late local definition, after a click moved the caret, is refused as stale and doesn't
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
