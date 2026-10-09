use std::str::FromStr;

use scrive_core::intel::completion::Start;
use scrive_core::intel::inlay;
use scrive_core::intel::ticket::Counter;
use scrive_core::{
    CompletionItem, DefinitionRequest, Document, EditOp, FormatRequest, GroupingHint, HoverInfo,
    HoverRequest, OpClass, Point, RenameRequest, SignatureInfo,
};
use serde_json::{json, Value};

use super::*;
use crate::client::Builder;

/// Server capabilities for a utf-16 server with incremental sync and open/close notifications.
fn incremental() -> Value {
    json!({"positionEncoding": "utf-16", "textDocumentSync": {"openClose": true, "change": 2}})
}

fn uri(text: &str) -> Uri {
    Uri::from_str(text).expect("fixture URI parses")
}

fn document(text: &str) -> Document {
    let mut doc = Document::new(text).expect("fixture loads");
    doc.observe_changes(true);
    doc
}

fn wire(messages: &[Message]) -> Vec<Value> {
    messages
        .iter()
        .map(|m| serde_json::to_value(m).expect("serializes"))
        .collect()
}

fn from_server(value: Value) -> Message {
    serde_json::from_value(value).expect("fixture parses")
}

/// A client whose `initialize` has been answered with `capabilities`, and what the answer sent.
fn running(builder: Builder, capabilities: Value) -> (Session, Output) {
    let (mut client, _) = builder.session(None);
    let output = client
        .receive(from_server(
            json!({"jsonrpc": "2.0", "id": 1, "result": {"capabilities": capabilities}}),
        ))
        .expect("initialize answer is accepted");
    (client, output)
}

/// The one response `client` sends to a server request for `method` with `params`.
fn answer(client: &mut Session, method: &str, params: Value) -> Value {
    let output = client
        .receive(from_server(
            json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params}),
        ))
        .expect("server requests are always answered");
    assert!(
        output.updates.is_empty(),
        "a server request updates nothing"
    );
    let [response] = wire(&output.messages)
        .try_into()
        .expect("a server request gets exactly one response");
    assert_eq!(response["id"], 7, "the response answers the request's id");
    response
}

fn did_open(uri: &str, version: i32, text: &str) -> Value {
    json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {"textDocument": {
        "uri": uri, "languageId": "rust", "version": version, "text": text,
    }}})
}

fn did_change(uri: &str, version: i32, changes: Vec<Value>) -> Value {
    json!({"jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
        "textDocument": {"uri": uri, "version": version},
        "contentChanges": changes,
    }})
}

fn ranged(start: (u32, u32), end: (u32, u32), text: &str) -> Value {
    json!({"range": {
        "start": {"line": start.0, "character": start.1},
        "end": {"line": end.0, "character": end.1},
    }, "text": text})
}

fn whole(text: &str) -> Value {
    json!({"text": text})
}

/// With the other per-variant helpers ([`completions`], [`signature`], [`hover`], [`edits`],
/// [`definition`]),
/// the only place tests read a `Change`, so a new variant changes only these helpers.
fn diagnostics(update: &Update) -> (DocId, update::Stamp, Vec<scrive_core::Diagnostic>) {
    let Update::Document(document) = update else {
        panic!("expected a document update, got {update:?}")
    };
    match document.change() {
        update::Change::Diagnostics(set) => (document.doc_id(), document.stamp(), set.clone()),
        other => panic!("expected diagnostics, got {other:?}"),
    }
}

/// The stamp and items of a completion update.
fn completions(update: &Update) -> (update::Stamp, Vec<CompletionItem>) {
    let Update::Document(document) = update else {
        panic!("expected a document update, got {update:?}")
    };
    match document.change() {
        update::Change::Completions(items) => (document.stamp(), items.clone()),
        other => panic!("expected completions, got {other:?}"),
    }
}

/// The stamp and signature of a signature update.
fn signature(update: &Update) -> (update::Stamp, Option<SignatureInfo>) {
    let Update::Document(document) = update else {
        panic!("expected a document update, got {update:?}")
    };
    match document.change() {
        update::Change::Signature(info) => (document.stamp(), info.clone()),
        other => panic!("expected a signature, got {other:?}"),
    }
}

/// The stamp and card of a hover update.
fn hover(update: &Update) -> (update::Stamp, Option<HoverInfo>) {
    let Update::Document(document) = update else {
        panic!("expected a document update, got {update:?}")
    };
    match document.change() {
        update::Change::Hover(info) => (document.stamp(), info.clone()),
        other => panic!("expected a hover, got {other:?}"),
    }
}

/// The stamp and ops of an edits update.
fn edits(update: &Update) -> (update::Stamp, Vec<EditOp>) {
    let Update::Document(document) = update else {
        panic!("expected a document update, got {update:?}")
    };
    match document.change() {
        update::Change::Edits(ops) => (document.stamp(), ops.clone()),
        other => panic!("expected edits, got {other:?}"),
    }
}

/// The stamp and target of a definition update.
fn definition(update: &Update) -> (update::Stamp, Option<update::Target>) {
    let Update::Document(document) = update else {
        panic!("expected a document update, got {update:?}")
    };
    match document.change() {
        update::Change::Definition(target) => (document.stamp(), target.clone()),
        other => panic!("expected a definition, got {other:?}"),
    }
}

/// A `publishDiagnostics` with one error over `start..end` on line 0.
fn publish(uri: &str, version: Option<i32>, span: Option<(u32, u32)>) -> Message {
    let diagnostics: Vec<Value> = span
        .into_iter()
        .map(|(start, end)| {
            json!({"range": {
                "start": {"line": 0, "character": start},
                "end": {"line": 0, "character": end},
            }, "severity": 1, "message": "bad"})
        })
        .collect();
    let mut params = json!({"uri": uri, "diagnostics": diagnostics});
    if let Some(version) = version {
        params["version"] = json!(version);
    }
    from_server(
        json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": params}),
    )
}

/// The byte spans (start, end) of the one diagnostic set in `output`, with its document and stamp.
fn landed(output: &Output) -> (DocId, update::Stamp, Vec<(u32, u32)>) {
    let [update] = output.updates.as_slice() else {
        panic!("expected one update, got {:?}", output.updates)
    };
    let (doc_id, stamp, set) = diagnostics(update);
    (
        doc_id,
        stamp,
        set.into_iter()
            .map(|d| (d.span.start, d.span.end))
            .collect(),
    )
}

/// The `initialize` request advertises what the client handles, and carries the builder's
/// inputs.
#[test]
fn initialize_advertises_encodings_versions_and_workspace_capabilities() {
    let (_, initialize) = Builder::default()
        .root(uri("file:///work/proj"))
        .initialization_options(json!({"a": 1}))
        .session(Some(42));
    let initialize = serde_json::to_value(&initialize).expect("serializes");
    assert_eq!(initialize["id"], 1, "initialize is request 1");
    assert_eq!(
        initialize["method"], "initialize",
        "the first request initializes"
    );
    for (pointer, expected) in [
        (
            "/params/capabilities/general/positionEncodings",
            json!(["utf-8", "utf-16", "utf-32"]),
        ),
        (
            "/params/capabilities/textDocument/publishDiagnostics/versionSupport",
            json!(true),
        ),
        (
            "/params/capabilities/textDocument/synchronization/didSave",
            json!(true),
        ),
        ("/params/capabilities/workspace/configuration", json!(true)),
        (
            "/params/capabilities/workspace/workspaceFolders",
            json!(true),
        ),
        ("/params/processId", json!(42)),
        ("/params/capabilities/window/workDoneProgress", json!(true)),
        ("/params/rootUri", json!("file:///work/proj")),
        ("/params/rootPath", json!("/work/proj")),
        ("/params/workspaceFolders/0/uri", json!("file:///work/proj")),
        ("/params/workspaceFolders/0/name", json!("proj")),
        ("/params/clientInfo/name", json!("scrive-lsp")),
        ("/params/initializationOptions", json!({"a": 1})),
    ] {
        assert_eq!(
            initialize.pointer(pointer),
            Some(&expected),
            "{pointer} is advertised"
        );
    }
}

/// A document opened before `initialize` is answered goes out as a `didOpen` right after
/// `initialized` and the configuration push.
#[test]
fn handshake_sends_initialized_configuration_and_deferred_did_open_with_latest_text() {
    let mut doc = document("fn main() {}");
    let (mut client, _) = Builder::default().configuration(json!({"x": 1})).session(None);
    let opened = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    assert!(
        opened.messages.is_empty(),
        "nothing goes out before initialize is answered"
    );
    doc.edit(vec![EditOp::insert(0, "pub ")]).expect("edits");
    let synced = client.sync(&doc.snapshot(), doc.drain_changes());
    assert!(
        synced.messages.is_empty(),
        "sync before initialize sends nothing"
    );
    let output = client
        .receive(from_server(
            json!({"jsonrpc": "2.0", "id": 1, "result": {"capabilities": incremental()}}),
        ))
        .expect("initialize answer is accepted");
    assert_eq!(
        wire(&output.messages),
        vec![
            json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
            json!({"jsonrpc": "2.0", "method": "workspace/didChangeConfiguration",
                "params": {"settings": {"x": 1}}}),
            did_open("file:///a.rs", 1, "pub fn main() {}"),
        ],
        "the handshake finishes, then the deferred document opens with its edited text",
    );
}

/// Before `initialize` is answered, registering and syncing only store the snapshot.
#[test]
fn sync_before_initialize_sends_nothing() {
    let mut doc = document("a");
    let (mut client, _) = Builder::default().session(None);
    let opened = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    assert!(
        opened.messages.is_empty() && opened.updates.is_empty(),
        "open before initialize sends nothing",
    );
    doc.edit(vec![EditOp::insert(1, "b")]).expect("edits");
    let synced = client.sync(&doc.snapshot(), doc.drain_changes());
    assert!(
        synced.messages.is_empty() && synced.updates.is_empty(),
        "sync before initialize sends nothing",
    );
}

/// Ranges go out in the negotiated unit: after an astral character, utf-16 counts two units.
#[test]
fn utf16_server_receives_incremental_ranges_in_utf16() {
    let mut doc = document("😀a");
    let (mut client, _) = running(Builder::default(), incremental());
    let _ = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    doc.edit(vec![EditOp::insert(5, "b")]).expect("edits");
    assert_eq!(
        wire(&client.sync(&doc.snapshot(), doc.drain_changes()).messages),
        vec![did_change(
            "file:///a.rs",
            2,
            vec![ranged((0, 3), (0, 3), "b")]
        )],
        "the insert after `😀a` is at utf-16 column 3",
    );
}

/// Undoing a typing run replays several steps. Each step's log entry converts against its own
/// `before`, so the server receives exact incremental deletes instead of the whole text.
#[test]
fn undo_of_a_typing_run_syncs_incrementally_in_one_did_change() {
    let mut doc = document("ab");
    let (mut client, _) = running(Builder::default(), json!({"textDocumentSync": 2}));
    let _ = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    for (at, c) in [(2, "c"), (3, "d")] {
        doc.edit_grouped(
            vec![EditOp::insert(at, c)],
            GroupingHint::mergeable(OpClass::Type),
        )
        .expect("types");
    }
    let _ = client.sync(&doc.snapshot(), doc.drain_changes());
    assert!(doc.undo(), "the typing run undoes");
    assert_eq!(
        doc.snapshot().text(),
        "ab",
        "the whole run is one undo step"
    );
    assert_eq!(
        wire(&client.sync(&doc.snapshot(), doc.drain_changes()).messages),
        vec![did_change(
            "file:///a.rs",
            3,
            vec![ranged((0, 3), (0, 4), ""), ranged((0, 2), (0, 3), "")],
        )],
        "undo of a two-step typing run is one didChange with two ranged deletes, newest first",
    );
}

/// Several commits drained at once go out as one `didChange`, in commit order.
#[test]
fn multi_commit_drain_is_sent_as_one_did_change() {
    let mut doc = document("abc");
    let (mut client, _) = running(Builder::default(), incremental());
    let _ = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    doc.edit(vec![EditOp::insert(0, "x")]).expect("edits");
    doc.edit(vec![EditOp::insert(4, "y")]).expect("edits");
    assert_eq!(
        wire(&client.sync(&doc.snapshot(), doc.drain_changes()).messages),
        vec![did_change(
            "file:///a.rs",
            2,
            vec![ranged((0, 0), (0, 0), "x"), ranged((0, 4), (0, 4), "y")],
        )],
        "both commits go out in one didChange, oldest first",
    );
}

/// A log that starts past what the server has leaves a gap, so the whole text goes out.
#[test]
fn broken_chain_falls_back_to_full_text() {
    let mut doc = document("abc");
    let (mut client, _) = running(Builder::default(), incremental());
    let _ = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    doc.edit(vec![EditOp::insert(0, "x")]).expect("edits");
    let _lost = doc.drain_changes();
    doc.edit(vec![EditOp::insert(0, "y")]).expect("edits");
    assert_eq!(
        wire(&client.sync(&doc.snapshot(), doc.drain_changes()).messages),
        vec![did_change("file:///a.rs", 2, vec![whole("yxabc")])],
        "a gap in the chain sends the whole text",
    );
}

/// Another document's log never describes this document's edits.
#[test]
fn foreign_doc_id_changes_fall_back_to_full_text() {
    let (mut a, mut b) = (document("abc"), document("def"));
    let (mut client, _) = running(Builder::default(), incremental());
    let _ = client
        .open(&a.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens a");
    let _ = client
        .open(&b.snapshot(), &uri("file:///b.rs"), "rust")
        .expect("opens b");
    a.edit(vec![EditOp::insert(0, "x")]).expect("edits a");
    b.edit(vec![EditOp::insert(0, "y")]).expect("edits b");
    let _ = a.drain_changes();
    assert_eq!(
        wire(&client.sync(&a.snapshot(), b.drain_changes()).messages),
        vec![did_change("file:///a.rs", 2, vec![whole("xabc")])],
        "a's snapshot with b's log sends a's whole text",
    );
}

/// A log that overflowed its cap has no chain, so the whole text goes out.
#[test]
fn capped_log_falls_back_to_full_text() {
    let mut doc = document("");
    let (mut client, _) = running(Builder::default(), incremental());
    let _ = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    for at in 0..1025 {
        doc.edit(vec![EditOp::insert(at, "a")]).expect("edits");
    }
    let changes = doc.drain_changes();
    assert_eq!(changes.from(), None, "the log broke at its cap");
    assert_eq!(
        wire(&client.sync(&doc.snapshot(), changes).messages),
        vec![did_change(
            "file:///a.rs",
            2,
            vec![whole(&"a".repeat(1025))]
        )],
        "a broken log sends the whole text",
    );
}

/// A server that only takes full syncs gets the whole text even when the chain would pass.
#[test]
fn full_sync_server_receives_whole_text() {
    let mut doc = document("abc");
    let (mut client, _) = running(Builder::default(), json!({"textDocumentSync": 1}));
    let _ = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    doc.edit(vec![EditOp::insert(3, "d")]).expect("edits");
    assert_eq!(
        wire(&client.sync(&doc.snapshot(), doc.drain_changes()).messages),
        vec![did_change("file:///a.rs", 2, vec![whole("abcd")])],
        "a FULL server gets the whole text",
    );
}

/// Syncing a snapshot the server already has sends nothing.
#[test]
fn unchanged_revision_sends_nothing() {
    let mut doc = document("abc");
    let (mut client, _) = running(Builder::default(), incremental());
    let _ = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    doc.edit(vec![EditOp::insert(3, "d")]).expect("edits");
    let _ = client.sync(&doc.snapshot(), doc.drain_changes());
    let again = client.sync(&doc.snapshot(), doc.drain_changes());
    assert!(
        again.messages.is_empty() && again.updates.is_empty(),
        "a second sync at the same revision sends nothing",
    );
}

/// A reopened URI continues its version count, so versions never repeat for one URI.
#[test]
fn version_high_water_mark_survives_reopen() {
    let (mut first, second) = (document("a"), document("b"));
    let (mut client, _) = running(Builder::default(), incremental());
    let _ = client
        .open(&first.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    first.edit(vec![EditOp::insert(1, "x")]).expect("edits");
    let _ = client.sync(&first.snapshot(), first.drain_changes());
    let _ = client.close(first.doc_id());
    let reopened = client
        .open(&second.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("reopens");
    assert_eq!(
        wire(&reopened.messages),
        vec![did_open("file:///a.rs", 3, "b")],
        "the reopen takes version 3, after didOpen 1 and didChange 2",
    );
}

/// A NONE server is told nothing about edits, but its diagnostics still convert against the
/// edited text.
#[test]
fn none_sync_sends_nothing_but_advances_the_synced_snapshot() {
    let mut doc = document("abc");
    let (mut client, _) = running(Builder::default(), json!({"textDocumentSync": 0}));
    let opened = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    assert_eq!(
        wire(&opened.messages),
        vec![did_open("file:///a.rs", 1, "abc")],
        "a bare NONE kind still wants didOpen",
    );
    doc.edit(vec![EditOp::insert(0, "xyz")]).expect("edits");
    let synced = client.sync(&doc.snapshot(), doc.drain_changes());
    assert!(
        synced.messages.is_empty(),
        "a NONE server gets no didChange"
    );
    let output = client
        .receive(publish("file:///a.rs", None, Some((4, 5))))
        .expect("the publish is accepted");
    assert_eq!(
        landed(&output),
        (
            doc.doc_id(),
            update::Stamp::Revision(doc.snapshot().revision()),
            vec![(4, 5)]
        ),
        "the diagnostic lands on the edited text at the edited revision",
    );
}

/// A publish for a version the server has already been sent past is dropped; one for the
/// current version applies.
#[test]
fn diagnostics_with_a_stale_version_are_dropped() {
    let mut doc = document("abc");
    let (mut client, _) = running(Builder::default(), incremental());
    let _ = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    doc.edit(vec![EditOp::insert(3, "d")]).expect("edits");
    let _ = client.sync(&doc.snapshot(), doc.drain_changes());
    let stale = client
        .receive(publish("file:///a.rs", Some(1), Some((0, 1))))
        .expect("the publish is accepted");
    assert!(
        stale.updates.is_empty(),
        "version 1 is stale after didChange 2"
    );
    let current = client
        .receive(publish("file:///a.rs", Some(2), Some((0, 1))))
        .expect("the publish is accepted");
    assert_eq!(
        landed(&current).2,
        vec![(0, 1)],
        "version 2 is the current version and applies",
    );
}

/// An unversioned publish is taken to be for the last synced text, and converts against it.
#[test]
fn diagnostics_without_a_version_apply_at_the_synced_revision() {
    let mut doc = document("😀b");
    let (mut client, _) = running(Builder::default(), incremental());
    let _ = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    doc.edit(vec![EditOp::insert(0, "a")]).expect("edits");
    let _ = client.sync(&doc.snapshot(), doc.drain_changes());
    let output = client
        .receive(publish("file:///a.rs", None, Some((3, 4))))
        .expect("the publish is accepted");
    assert_eq!(
        landed(&output),
        (
            doc.doc_id(),
            update::Stamp::Revision(doc.snapshot().revision()),
            vec![(5, 6)]
        ),
        "utf-16 columns 3..4 of `a😀b` are bytes 5..6, at the synced revision",
    );
}

/// Diagnostics for a file that is not open wait for its `open`, under any spelling of its URI,
/// and are handed out once.
#[test]
fn unopened_diagnostics_are_cached_and_applied_at_open() {
    let (first, second) = (document("abc"), document("abc"));
    let (mut client, _) = running(Builder::default(), incremental());
    let published = client
        .receive(publish("file:///C%3A/b.rs", None, Some((1, 2))))
        .expect("the publish is accepted");
    assert!(
        published.updates.is_empty(),
        "nothing is open to apply it to"
    );
    let opened = client
        .open(&first.snapshot(), &uri("file:///c:/b.rs"), "rust")
        .expect("opens");
    assert_eq!(
        landed(&opened),
        (
            first.doc_id(),
            update::Stamp::Revision(first.snapshot().revision()),
            vec![(1, 2)]
        ),
        "open hands out the cached set against the opened snapshot",
    );
    let _ = client.close(first.doc_id());
    let reopened = client
        .open(&second.snapshot(), &uri("file:///c:/b.rs"), "rust")
        .expect("reopens");
    assert!(
        reopened.updates.is_empty(),
        "the cache was consumed by the first open"
    );
}

/// An empty publish for a closed file means it has no diagnostics any more.
#[test]
fn empty_publish_clears_the_cache() {
    let doc = document("abc");
    let (mut client, _) = running(Builder::default(), incremental());
    for message in [
        publish("file:///b.rs", None, Some((0, 1))),
        publish("file:///b.rs", None, None),
    ] {
        let _ = client.receive(message).expect("the publish is accepted");
    }
    let opened = client
        .open(&doc.snapshot(), &uri("file:///b.rs"), "rust")
        .expect("opens");
    assert!(
        opened.updates.is_empty(),
        "the empty publish removed the cached set"
    );
}

/// A versioned publish for a closed file supersedes the unversioned set cached for it.
#[test]
fn versioned_publish_for_an_unopened_uri_clears_the_cache() {
    let doc = document("abc");
    let (mut client, _) = running(Builder::default(), incremental());
    for message in [
        publish("file:///b.rs", None, Some((0, 1))),
        publish("file:///b.rs", Some(4), Some((1, 2))),
    ] {
        let _ = client.receive(message).expect("the publish is accepted");
    }
    let opened = client
        .open(&doc.snapshot(), &uri("file:///b.rs"), "rust")
        .expect("opens");
    assert!(
        opened.updates.is_empty(),
        "the versioned publish removed the cached set"
    );
}

/// A `publishDiagnostics` whose params do not decode is an error: there is nothing to answer.
#[test]
fn undecodable_publish_is_a_decode_error() {
    let (mut client, _) = running(Builder::default(), incremental());
    let failed = client.receive(from_server(
        json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {"uri": 3}}),
    ));
    assert!(
        matches!(&failed, Err(Error::Decode { method, .. }) if method == "textDocument/publishDiagnostics"),
        "an undecodable publish is a decode error, got {failed:?}",
    );
}

/// Two spellings of one file are one URI, and the second document claiming it is refused and
/// left unregistered.
#[test]
fn duplicate_uri_is_refused() {
    let (a, b) = (document("a"), document("b"));
    let (mut client, _) = running(Builder::default(), incremental());
    let _ = client
        .open(&a.snapshot(), &uri("file:///C%3A/a.rs"), "rust")
        .expect("the first document opens");
    let refused = client.open(&b.snapshot(), &uri("file:///c:/a.rs"), "rust");
    assert!(
        matches!(refused, Err(Error::DuplicateUri { ref uri }) if uri.as_str() == "file:///c:/a.rs"),
        "the second document is refused, got {refused:?}",
    );
    assert!(
        client.close(b.doc_id()).messages.is_empty(),
        "the refused document was never registered",
    );
}

/// Closing sends `didClose` under the normalized URI.
#[test]
fn close_sends_did_close() {
    let doc = document("a");
    let (mut client, _) = running(Builder::default(), incremental());
    let _ = client
        .open(&doc.snapshot(), &uri("file:///C%3A/a.rs"), "rust")
        .expect("opens");
    assert_eq!(
        wire(&client.close(doc.doc_id()).messages),
        vec![json!({"jsonrpc": "2.0", "method": "textDocument/didClose",
            "params": {"textDocument": {"uri": "file:///c:/a.rs"}}})],
        "close sends didClose for the normalized URI",
    );
}

/// Incremental sync with open/close notifications and `save` set to `save`.
fn saving(save: Value) -> Value {
    json!({"positionEncoding": "utf-16",
        "textDocumentSync": {"openClose": true, "change": 2, "save": save}})
}

fn did_save(uri: &str, text: Option<&str>) -> Value {
    let mut params = json!({"textDocument": {"uri": uri}});
    if let Some(text) = text {
        params["text"] = json!(text);
    }
    json!({"jsonrpc": "2.0", "method": "textDocument/didSave", "params": params})
}

/// A server that asks for saves without text, as rust-analyzer does, gets a bare `didSave`.
#[test]
fn save_notifies_when_the_server_asks() {
    let doc = document("a");
    let (mut client, _) = running(Builder::default(), saving(json!({})));
    let _ = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    let saved = client.save(&doc.snapshot());
    assert_eq!(
        wire(&saved.messages),
        vec![did_save("file:///a.rs", None)],
        "save sends didSave without the text",
    );
    assert!(saved.updates.is_empty(), "save updates nothing");
}

/// `includeText: true` sends the synced text with the save, and `includeText: false` does not.
#[test]
fn save_includes_text_only_when_asked() {
    for (include, text) in [(true, Some("ab")), (false, None)] {
        let mut doc = document("a");
        let (mut client, _) = running(Builder::default(), saving(json!({"includeText": include})));
        let _ = client
            .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
            .expect("opens");
        doc.edit(vec![EditOp::insert(1, "b")]).expect("edits");
        let _ = client.sync(&doc.snapshot(), doc.drain_changes());
        assert_eq!(
            wire(&client.save(&doc.snapshot()).messages),
            vec![did_save("file:///a.rs", text)],
            "includeText {include} decides whether the synced text goes out",
        );
    }
}

/// Only `save: true` among the other shapes asks for saves: a missing `save`, `save: false`
/// and a bare sync kind send nothing.
#[test]
fn save_follows_every_save_option_shape() {
    for (capabilities, sends) in [
        (incremental(), false),
        (saving(json!(false)), false),
        (saving(json!(true)), true),
        (json!({"positionEncoding": "utf-16", "textDocumentSync": 2}), false),
    ] {
        let doc = document("a");
        let (mut client, _) = running(Builder::default(), capabilities.clone());
        let _ = client
            .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
            .expect("opens");
        let expected = if sends {
            vec![did_save("file:///a.rs", None)]
        } else {
            Vec::new()
        };
        assert_eq!(
            wire(&client.save(&doc.snapshot()).messages),
            expected,
            "save under {capabilities}",
        );
    }
}

/// A snapshot the server has not been synced to is not saved: the saved text would not be the
/// text the server holds.
#[test]
fn save_of_an_unsynced_snapshot_sends_nothing() {
    let mut doc = document("a");
    let (mut client, _) = running(Builder::default(), saving(json!({})));
    let _ = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    doc.edit(vec![EditOp::insert(1, "b")]).expect("edits");
    let _ = doc.drain_changes();
    let saved = client.save(&doc.snapshot());
    assert!(
        saved.messages.is_empty() && saved.updates.is_empty(),
        "an unsynced snapshot sends nothing",
    );
}

/// Before the handshake, for an unregistered document, and after shutdown, save sends nothing;
/// a save before the handshake is not replayed after it.
#[test]
fn save_before_open_or_after_shutdown_sends_nothing() {
    let doc = document("a");
    let (mut client, _) = running(Builder::default(), saving(json!({})));
    assert!(
        client.save(&doc.snapshot()).messages.is_empty(),
        "save of an unregistered document sends nothing",
    );
    let _ = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    let _ = client.shutdown();
    assert!(
        client.save(&doc.snapshot()).messages.is_empty(),
        "save after shutdown sends nothing",
    );

    let (mut client, _) = Builder::default().session(None);
    let _ = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    assert!(
        client.save(&doc.snapshot()).messages.is_empty(),
        "save before initialize sends nothing",
    );
    let output = client
        .receive(from_server(
            json!({"jsonrpc": "2.0", "id": 1, "result": {"capabilities": saving(json!({}))}}),
        ))
        .expect("initialize answer is accepted");
    assert_eq!(
        wire(&output.messages),
        vec![
            json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
            did_open("file:///a.rs", 1, "a"),
        ],
        "only the deferred didOpen follows initialized",
    );
}

/// Before `initialize` is answered there is nothing to shut down: nothing goes out, and the
/// late answer is ignored, and so is every later sync.
#[test]
fn shutdown_before_initialize_sends_nothing_and_ignores_the_late_answer() {
    let mut doc = document("a");
    let (mut client, _) = Builder::default().session(None);
    let _ = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    assert!(
        client.shutdown().messages.is_empty(),
        "shutdown sends nothing"
    );
    doc.edit(vec![EditOp::insert(1, "b")]).expect("edits");
    assert!(
        client
            .sync(&doc.snapshot(), doc.drain_changes())
            .messages
            .is_empty(),
        "sync declines after shutdown",
    );
    let late = client
        .receive(from_server(
            json!({"jsonrpc": "2.0", "id": 1, "result": {"capabilities": incremental()}}),
        ))
        .expect("a late answer is silent");
    assert!(
        late.messages.is_empty() && late.updates.is_empty(),
        "the late initialize answer sends nothing",
    );
}

/// Once shutdown has started, server requests are answered with `null` so the server never
/// blocks on the client.
#[test]
fn server_requests_after_shutdown_get_null() {
    let (mut client, _) = running(
        Builder::default().configuration(json!({"x": 1})),
        incremental(),
    );
    let _ = client.shutdown();
    let response = answer(
        &mut client,
        "workspace/configuration",
        json!({"items": [{"section": "x"}]}),
    );
    assert_eq!(
        response["result"],
        Value::Null,
        "the configuration request gets null"
    );
}

/// After `shutdown()`, registering, syncing and closing documents send nothing.
#[test]
fn client_calls_decline_after_shutdown() {
    let (mut a, b) = (document("a"), document("b"));
    let (mut client, _) = running(Builder::default(), incremental());
    let _ = client
        .open(&a.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    let _ = client.shutdown();
    let opened = client
        .open(&b.snapshot(), &uri("file:///b.rs"), "rust")
        .expect("open declines without an error");
    assert!(opened.messages.is_empty(), "open declines after shutdown");
    a.edit(vec![EditOp::insert(1, "b")]).expect("edits");
    assert!(
        client
            .sync(&a.snapshot(), a.drain_changes())
            .messages
            .is_empty(),
        "sync declines after shutdown",
    );
    assert!(
        client.close(a.doc_id()).messages.is_empty(),
        "close declines after shutdown"
    );
}

/// A server that fails `initialize` cannot be synced, so the error surfaces; the documents stay
/// registered, and their requests decline.
#[test]
fn failed_initialize_is_a_server_error_and_keeps_the_documents() {
    let mut tickets = Counter::new();
    let mut doc = document("let value = 1;");
    let (mut client, _) = Builder::default().session(None);
    let _ = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    let failed = client.receive(from_server(
        json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32603, "message": "no"}}),
    ));
    assert!(
        matches!(&failed, Err(Error::Server { doc_id: None, method, .. }) if method == "initialize"),
        "a failed initialize is a server error, got {failed:?}",
    );
    doc.edit(vec![EditOp::insert(14, " ")]).expect("edits");
    assert!(
        client
            .sync(&doc.snapshot(), doc.drain_changes())
            .messages
            .is_empty(),
        "nothing syncs after a failed initialize",
    );
    let request = hover_request(&mut tickets, &doc);
    let (stamp, card) = hovered(&client.hover(&doc.snapshot(), &request));
    assert_eq!(
        stamp,
        update::Stamp::Ticket(request.ticket),
        "a hover at the new revision answers under its ticket",
    );
    assert!(card.is_none(), "a hover at the new revision declines");
    let other = document("b");
    assert!(
        matches!(
            client.open(&other.snapshot(), &uri("file:///a.rs"), "rust"),
            Err(Error::DuplicateUri { .. })
        ),
        "the document is still registered under its URI",
    );
}

/// After `shutdown()` an edit is still synced locally, and every request with an awaited editor
/// slot declines with its empty answer; rename and format send nothing.
#[test]
fn requests_decline_with_their_empty_answers_after_shutdown() {
    let mut tickets = Counter::new();
    let mut doc = document("let value = 1;");
    let (mut client, _) = running(Builder::default(), incremental());
    let _ = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    let _ = client.shutdown();
    doc.edit(vec![EditOp::insert(14, " ")]).expect("edits");
    let synced = client.sync(&doc.snapshot(), doc.drain_changes());
    assert!(synced.messages.is_empty(), "sync sends nothing after shutdown");
    let snapshot = doc.snapshot();

    let request = request(&mut tickets, &doc, 4..9, CompletionTrigger::Manual, Start::Fresh);
    let (stamp, items) = answered(&client.complete(&snapshot, &request));
    assert_eq!(stamp, update::Stamp::Ticket(request.ticket()), "completion answers its ticket");
    assert!(items.is_empty(), "completion declines");
    let request = signature_request(&mut tickets, &doc, 5, None);
    let (stamp, info) = signature(only(&client.signature_help(&snapshot, &request)));
    assert_eq!(stamp, update::Stamp::Ticket(request.ticket()), "signature answers its ticket");
    assert!(info.is_none(), "signature help declines");
    let request = hover_request(&mut tickets, &doc);
    let (stamp, card) = hovered(&client.hover(&snapshot, &request));
    assert_eq!(stamp, update::Stamp::Ticket(request.ticket), "hover answers its ticket");
    assert!(card.is_none(), "hover declines");
    let request = DefinitionRequest::new(tickets.issue(doc.revision()), 5);
    assert_eq!(
        definition(only(&client.definition(&snapshot, &request))),
        (update::Stamp::Ticket(request.ticket), None),
        "definition declines",
    );
    let request = inlay_request(&mut tickets, &doc);
    let (stamp, hints) = inlays(only(&client.inlays(&snapshot, &request)));
    assert_eq!(stamp, update::Stamp::Ticket(request.ticket()), "inlays answer their ticket");
    assert!(hints.is_some_and(|hints| hints.is_empty()), "inlays decline");
    let request = RenameRequest::new(tickets.issue(doc.revision()), 5, "other");
    assert_silent(&client.rename(&snapshot, &request), "rename sends nothing");
    let request = FormatRequest::new(tickets.issue(doc.revision()), 4);
    assert_silent(&client.format(&snapshot, &request), "format sends nothing");
    assert_eq!(
        client.tracked[0].synced.revision(),
        doc.revision(),
        "the edit is the synced text",
    );
}

/// Before the handshake and after shutdown, `sync` keeps the snapshot it is handed.
#[test]
fn sync_before_initialize_and_after_shutdown_stores_the_snapshot() {
    let mut doc = document("a");
    let (mut client, _) = Builder::default().session(None);
    let _ = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    doc.edit(vec![EditOp::insert(1, "b")]).expect("edits");
    let _ = client.sync(&doc.snapshot(), doc.drain_changes());
    assert_eq!(
        client.tracked[0].synced.revision(),
        doc.revision(),
        "sync before initialize stores the snapshot",
    );

    let (mut client, _) = running(Builder::default(), incremental());
    let _ = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    let _ = client.shutdown();
    doc.edit(vec![EditOp::insert(2, "c")]).expect("edits");
    let _ = client.sync(&doc.snapshot(), doc.drain_changes());
    assert_eq!(
        client.tracked[0].synced.revision(),
        doc.revision(),
        "sync after shutdown stores the snapshot",
    );
}

/// `workspace/configuration` reads dotted sections; an empty or absent section is the whole
/// value and a missing one is `null`.
#[test]
fn configuration_answers_dotted_sections() {
    let configuration = json!({"rust-analyzer": {"check": {"command": "clippy"}}});
    let (mut client, _) = running(
        Builder::default().configuration(configuration.clone()),
        incremental(),
    );
    let response = answer(
        &mut client,
        "workspace/configuration",
        json!({"items": [
            {"section": "rust-analyzer.check"}, {"section": ""}, {"section": "missing.path"}, {},
        ]}),
    );
    assert_eq!(
        response["result"],
        json!([{"command": "clippy"}, configuration, null, configuration]),
        "each item gets its section",
    );
}

/// `workspace/workspaceFolders` answers the root folder, or `null` without a root.
#[test]
fn workspace_folders_answers_the_root() {
    let (mut rooted, _) = running(
        Builder::default().root(uri("file:///work/proj")),
        incremental(),
    );
    assert_eq!(
        answer(&mut rooted, "workspace/workspaceFolders", Value::Null)["result"],
        json!([{"uri": "file:///work/proj", "name": "proj"}]),
        "the root is the one folder",
    );
    let (mut rootless, _) = running(Builder::default(), incremental());
    assert_eq!(
        answer(&mut rootless, "workspace/workspaceFolders", Value::Null)["result"],
        Value::Null,
        "without a root there are no folders",
    );
}

/// Registration, progress, message and refresh requests need no work and get `null`.
#[test]
fn housekeeping_server_requests_get_null() {
    let (mut client, _) = running(Builder::default(), incremental());
    for method in [
        "client/registerCapability",
        "client/unregisterCapability",
        "window/workDoneProgress/create",
        "window/showMessageRequest",
        "workspace/semanticTokens/refresh",
    ] {
        let response = answer(&mut client, method, json!({}));
        assert_eq!(
            response.get("result"),
            Some(&Value::Null),
            "{method} gets null"
        );
    }
}

/// The client does not apply server-initiated edits, and says so.
#[test]
fn apply_edit_is_declined() {
    let (mut client, _) = running(Builder::default(), incremental());
    let response = answer(&mut client, "workspace/applyEdit", json!({"edit": {}}));
    assert_eq!(
        response["result"]["applied"], false,
        "the edit is not applied"
    );
}

/// A configuration request whose params do not decode is answered with `InvalidParams`.
#[test]
fn undecodable_configuration_params_answer_invalid_params() {
    let (mut client, _) = running(Builder::default(), incremental());
    let response = answer(&mut client, "workspace/configuration", json!({"items": 3}));
    assert_eq!(
        response["error"]["code"], -32602,
        "bad params are InvalidParams"
    );
}

/// A request the client does not implement is answered with `MethodNotFound`.
#[test]
fn unknown_server_request_answers_method_not_found() {
    let (mut client, _) = running(Builder::default(), incremental());
    let response = answer(&mut client, "custom/thing", json!({}));
    assert_eq!(
        response["error"]["code"], -32601,
        "unknown methods are MethodNotFound"
    );
}

/// Notifications the client does not consume reach the host unchanged.
#[test]
fn unhandled_notifications_pass_through() {
    let (mut client, _) = running(Builder::default(), incremental());
    let params = json!({"token": 1, "value": {"kind": "begin", "title": "Indexing"}});
    let output = client
        .receive(from_server(
            json!({"jsonrpc": "2.0", "method": "$/progress", "params": params}),
        ))
        .expect("notifications are accepted");
    match output.updates.as_slice() {
        [Update::Notification(passed)] => {
            assert_eq!(passed.method(), "$/progress", "the method passes through");
            assert_eq!(passed.params(), Some(&params), "the params pass through");
        }
        other => panic!("expected one passed-through notification, got {other:?}"),
    }
}

/// `window/logMessage` and `window/showMessage` become log entries from the server; only the
/// second asks to be shown.
#[test]
fn log_and_show_messages_become_log_entries() {
    let (mut client, _) = running(Builder::default(), incremental());
    for (method, shown) in [("window/logMessage", false), ("window/showMessage", true)] {
        let output = client
            .receive(from_server(json!({"jsonrpc": "2.0", "method": method,
                "params": {"type": 3, "message": "hi"}})))
            .expect("log notifications are accepted");
        let [Update::Log(entry)] = output.updates.as_slice() else {
            panic!("expected one log entry, got {:?}", output.updates)
        };
        assert_eq!(entry.is_shown(), shown, "{method} shown");
        assert_eq!(entry.level(), lsp_types::MessageType::INFO, "{method} level");
        assert_eq!(entry.text(), "hi", "{method} text");
        assert_eq!(entry.source(), log::Source::Server, "{method} source");
        assert!(output.messages.is_empty(), "{method} sends nothing");
    }
}

/// A log message whose params don't decode is reported like any other payload.
#[test]
fn undecodable_log_message_is_a_decode_error() {
    let (mut client, _) = running(Builder::default(), incremental());
    let output = client.receive(from_server(
        json!({"jsonrpc": "2.0", "method": "window/logMessage", "params": {"type": "loud"}}),
    ));
    assert!(
        matches!(&output, Err(Error::Decode { method, .. }) if method == "window/logMessage"),
        "an undecodable log message is a decode error, got {output:?}",
    );
}

/// The client answers server requests at once, so a server's cancellation has nothing to
/// cancel and is not passed on.
#[test]
fn a_server_cancel_request_is_swallowed() {
    let (mut client, _) = running(Builder::default(), incremental());
    let output = client
        .receive(from_server(
            json!({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": 7}}),
        ))
        .expect("cancellations are accepted");
    assert_silent(&output, "a server's cancellation");
}

/// A response nothing is waiting for is ignored.
#[test]
fn unknown_response_id_is_silent() {
    let (mut client, _) = running(Builder::default(), incremental());
    let output = client
        .receive(from_server(
            json!({"jsonrpc": "2.0", "id": 99, "result": {}}),
        ))
        .expect("an unknown id is not an error");
    assert!(
        output.messages.is_empty() && output.updates.is_empty(),
        "an unknown id sends and updates nothing",
    );
}

/// Server capabilities with incremental sync and completion triggered by `.` and `::`.
fn completion_capabilities() -> Value {
    json!({"textDocumentSync": 2, "completionProvider": {"triggerCharacters": [".", "::"]}})
}

/// A completion request for `word` in `doc` at its current revision, under a fresh ticket from
/// the test's `tickets`.
fn request(
    tickets: &mut Counter,
    doc: &Document,
    word: core::ops::Range<u32>,
    trigger: CompletionTrigger,
    start: Start,
) -> CompletionRequest {
    CompletionRequest::new(tickets.issue(doc.revision()), word, trigger, start)
}

/// A running client with `text` open as `file:///a.rs` (`didOpen` already sent).
fn completing(text: &str) -> (Session, Document) {
    let doc = document(text);
    let (mut client, _) = running(Builder::default(), completion_capabilities());
    let _ = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    (client, doc)
}

/// Labels, for compact assertions.
fn labels(items: &[CompletionItem]) -> Vec<&str> {
    items.iter().map(|item| item.label.as_str()).collect()
}

/// The reply to request `id` for `let v = pr` with the caret at 10: `print` edits exactly the
/// word, `println` is a snippet without an edit, `= prim` edits `6..10` and filters on `prim`,
/// and `other` matches nothing.
fn list(id: i64, incomplete: bool) -> Message {
    from_server(
        json!({"jsonrpc": "2.0", "id": id, "result": {"isIncomplete": incomplete, "items": [
            {"label": "print", "textEdit": {"range": {"start": {"line": 0, "character": 8}, "end": {"line": 0, "character": 10}}, "newText": "print"}},
            {"label": "println", "insertText": "println!($0)", "insertTextFormat": 2},
            {"label": "= prim", "filterText": "prim", "textEdit": {"range": {"start": {"line": 0, "character": 6}, "end": {"line": 0, "character": 10}}, "newText": "= prim"}},
            {"label": "other"},
        ]}}),
    )
}

/// An error reply to request `id`.
fn failure(id: i64, code: i64) -> Message {
    from_server(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": "no"}}))
}

fn completion_request(id: i64, position: (u32, u32), context: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "textDocument/completion", "params": {
        "textDocument": {"uri": "file:///a.rs"},
        "position": {"line": position.0, "character": position.1},
        "context": context,
    }})
}

fn cancel_request(id: i64) -> Value {
    json!({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": id}})
}

fn assert_silent(output: &Output, why: &str) {
    assert!(
        output.messages.is_empty() && output.updates.is_empty(),
        "{why}: got {output:?}"
    );
}

/// The one completion update in `output`, which sends nothing.
fn answered(output: &Output) -> (update::Stamp, Vec<CompletionItem>) {
    assert!(
        output.messages.is_empty(),
        "an answer sends nothing: {output:?}"
    );
    let [update] = output.updates.as_slice() else {
        panic!("expected one update, got {:?}", output.updates)
    };
    completions(update)
}

/// A request goes out at the caret, with the context the trigger implies.
#[test]
fn completion_request_carries_position_and_context() {
    let mut tickets = Counter::new();
    let (mut client, doc) = completing("let v = pr");
    let first = request(
        &mut tickets,
        &doc,
        8..10,
        CompletionTrigger::Manual,
        Start::Fresh,
    );
    let output = client.complete(&doc.snapshot(), &first);
    assert_eq!(
        wire(&output.messages),
        vec![completion_request(2, (0, 10), json!({"triggerKind": 1}))],
        "a manual request is invoked at the caret",
    );
    assert!(output.updates.is_empty(), "nothing is answered yet");
}

/// `initialize` advertises the completion features the conversion handles, and none it does
/// not.
#[test]
fn completion_advertises_snippets_plaintext_docs_and_context() {
    let (_, initialize) = Builder::default().session(None);
    let initialize = serde_json::to_value(&initialize).expect("serializes");
    let completion = &initialize["params"]["capabilities"]["textDocument"]["completion"];
    assert_eq!(
        completion["completionItem"]["snippetSupport"],
        json!(true),
        "snippets are advertised"
    );
    assert_eq!(
        completion["completionItem"]["documentationFormat"],
        json!(["plaintext"]),
        "documentation is plain text"
    );
    assert_eq!(
        completion["contextSupport"],
        json!(true),
        "context is advertised"
    );
    for absent in [
        "/completionItem/insertReplaceSupport",
        "/completionItem/labelDetailsSupport",
        "/completionList",
    ] {
        assert_eq!(
            completion.pointer(absent),
            None,
            "{absent} is not advertised"
        );
    }
}

/// A new request for the same document cancels the one in flight.
#[test]
fn second_request_supersedes_the_first_with_a_cancel() {
    let mut tickets = Counter::new();
    let (mut client, doc) = completing("let v = pr");
    let first = request(
        &mut tickets,
        &doc,
        8..10,
        CompletionTrigger::Manual,
        Start::Fresh,
    );
    let _ = client.complete(&doc.snapshot(), &first);
    let second = request(
        &mut tickets,
        &doc,
        8..10,
        CompletionTrigger::Manual,
        Start::Fresh,
    );
    assert_eq!(
        wire(&client.complete(&doc.snapshot(), &second).messages),
        vec![
            cancel_request(2),
            completion_request(3, (0, 10), json!({"triggerKind": 1}))
        ],
        "the first request is cancelled before the second goes out",
    );
}

/// Once superseded, a request's reply answers nothing.
#[test]
fn reply_to_a_superseded_request_is_silent() {
    let mut tickets = Counter::new();
    let (mut client, doc) = completing("let v = pr");
    let first = request(
        &mut tickets,
        &doc,
        8..10,
        CompletionTrigger::Manual,
        Start::Fresh,
    );
    let _ = client.complete(&doc.snapshot(), &first);
    let second = request(
        &mut tickets,
        &doc,
        8..10,
        CompletionTrigger::Manual,
        Start::Fresh,
    );
    let _ = client.complete(&doc.snapshot(), &second);
    let output = client
        .receive(list(2, false))
        .expect("the reply is accepted");
    assert_silent(&output, "a superseded reply is dropped");
}

/// A cancellation, the client's or the server's own, leaves nothing waiting for the reply.
#[test]
fn request_cancelled_and_server_cancelled_replies_are_silent() {
    for code in [-32800, -32802] {
        let mut tickets = Counter::new();
        let (mut client, doc) = completing("let v = pr");
        let first = request(
            &mut tickets,
            &doc,
            8..10,
            CompletionTrigger::Manual,
            Start::Fresh,
        );
        let _ = client.complete(&doc.snapshot(), &first);
        let output = client
            .receive(failure(2, code))
            .expect("a cancellation is not an error");
        assert_silent(&output, &format!("a {code} reply answers nothing"));
    }
}

/// A content-modified request whose ticket is still current is sent again, but only once.
#[test]
fn content_modified_is_reissued_once_per_ticket() {
    let mut tickets = Counter::new();
    let (mut client, doc) = completing("std::");
    let first = request(
        &mut tickets,
        &doc,
        5..5,
        CompletionTrigger::TriggerChar(':'),
        Start::Fresh,
    );
    let _ = client.complete(&doc.snapshot(), &first);
    let context = json!({"triggerKind": 2, "triggerCharacter": "::"});
    let reissued = client
        .receive(failure(2, -32801))
        .expect("content modified is not an error");
    assert_eq!(
        wire(&reissued.messages),
        vec![completion_request(3, (0, 5), context)],
        "the request goes out again with its position and context",
    );
    assert!(reissued.updates.is_empty(), "nothing is answered yet");
    let output = client
        .receive(failure(3, -32801))
        .expect("content modified is not an error");
    assert_silent(
        &output,
        "a second content-modified reply for one ticket is final",
    );
}

/// A re-issue follows the continued caret; the trigger character belonged to the original
/// keystroke, so it asks as invoked.
#[test]
fn content_modified_after_a_continuation_reissues_at_the_latest_caret() {
    let mut tickets = Counter::new();
    let (mut client, mut doc) = completing("let v = pr");
    let first = request(
        &mut tickets,
        &doc,
        8..10,
        CompletionTrigger::Typed('r'),
        Start::Fresh,
    );
    let _ = client.complete(&doc.snapshot(), &first);
    type_ops(&mut client, &mut doc, vec![EditOp::insert(10, "i")]);
    let second = request(
        &mut tickets,
        &doc,
        8..11,
        CompletionTrigger::Typed('i'),
        Start::Continuing,
    );
    let _ = client.complete(&doc.snapshot(), &second);
    let reissued = client
        .receive(failure(2, -32801))
        .expect("content modified is not an error");
    assert_eq!(
        wire(&reissued.messages),
        vec![completion_request(3, (0, 11), json!({"triggerKind": 1}))],
        "the re-issue asks at the latest caret",
    );
    let (stamp, items) = answered(
        &client
            .receive(list(3, false))
            .expect("the reply is accepted"),
    );
    assert_eq!(
        stamp,
        update::Stamp::Ticket(second.ticket()),
        "the re-issue answers the latest ticket"
    );
    assert!(!items.is_empty(), "the re-issued reply lands");
}

/// A failed completion request still settles the editor's slot.
#[test]
fn other_server_errors_answer_an_empty_list() {
    let mut tickets = Counter::new();
    let (mut client, doc) = completing("let v = pr");
    let first = request(
        &mut tickets,
        &doc,
        8..10,
        CompletionTrigger::Manual,
        Start::Fresh,
    );
    let _ = client.complete(&doc.snapshot(), &first);
    let output = client
        .receive(failure(2, -32603))
        .expect("a completion failure is not an error");
    let (stamp, items) = answered(&output);
    assert_eq!(
        stamp,
        update::Stamp::Ticket(first.ticket()),
        "stamped with the request's ticket"
    );
    assert!(items.is_empty(), "the failure answers an empty list");
}

/// Before `initialize` is answered there is no server to ask; the editor stops waiting.
#[test]
fn completion_declines_before_initialize() {
    let mut tickets = Counter::new();
    let doc = document("let v = pr");
    let (mut client, _) = Builder::default().session(None);
    let _ = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    let first = request(
        &mut tickets,
        &doc,
        8..10,
        CompletionTrigger::Manual,
        Start::Fresh,
    );
    let (stamp, items) = answered(&client.complete(&doc.snapshot(), &first));
    assert_eq!(
        stamp,
        update::Stamp::Ticket(first.ticket()),
        "the decline answers the ticket"
    );
    assert!(items.is_empty(), "the decline is an empty list");
}

/// A server without a completion provider is never asked.
#[test]
fn completion_declines_without_a_provider() {
    let mut tickets = Counter::new();
    let doc = document("let v = pr");
    let (mut client, _) = running(Builder::default(), json!({"textDocumentSync": 2}));
    let _ = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    let first = request(
        &mut tickets,
        &doc,
        8..10,
        CompletionTrigger::Manual,
        Start::Fresh,
    );
    let (stamp, items) = answered(&client.complete(&doc.snapshot(), &first));
    assert_eq!(
        stamp,
        update::Stamp::Ticket(first.ticket()),
        "the decline answers the ticket"
    );
    assert!(items.is_empty(), "the decline is an empty list");
}

/// The editor's trigger set is fixed; a trigger the server did not register asks nothing.
#[test]
fn unregistered_trigger_character_declines() {
    let mut tickets = Counter::new();
    let (mut client, doc) = completing("let v = ");
    let first = request(
        &mut tickets,
        &doc,
        8..8,
        CompletionTrigger::TriggerChar(' '),
        Start::Fresh,
    );
    let (stamp, items) = answered(&client.complete(&doc.snapshot(), &first));
    assert_eq!(
        stamp,
        update::Stamp::Ticket(first.ticket()),
        "the decline answers the ticket"
    );
    assert!(items.is_empty(), "the decline is an empty list");
}

/// A trigger longer than the typed character matches the text before the caret.
#[test]
fn multi_character_trigger_matches_by_suffix() {
    let mut tickets = Counter::new();
    let (mut client, doc) = completing("std::");
    let first = request(
        &mut tickets,
        &doc,
        5..5,
        CompletionTrigger::TriggerChar(':'),
        Start::Fresh,
    );
    assert_eq!(
        wire(&client.complete(&doc.snapshot(), &first).messages),
        vec![completion_request(
            2,
            (0, 5),
            json!({"triggerKind": 2, "triggerCharacter": "::"})
        )],
        "the registered `::` is the trigger character",
    );
}

/// A request from an older revision is neither sent nor answered.
#[test]
fn stale_completion_request_is_ignored() {
    let mut tickets = Counter::new();
    let (mut client, mut doc) = completing("let v = pr");
    doc.edit(vec![EditOp::insert(10, "i")]).expect("edits");
    let _ = client.sync(&doc.snapshot(), doc.drain_changes());
    let stale = CompletionRequest::new(
        tickets.issue(Revision(0)),
        8..10,
        CompletionTrigger::Manual,
        Start::Fresh,
    );
    assert_silent(
        &client.complete(&doc.snapshot(), &stale),
        "a stale request is ignored",
    );
}

/// A reply answers the request's ticket with the items matching the word, as shown.
#[test]
fn reply_answers_the_ticket_with_the_matching_items() {
    let mut tickets = Counter::new();
    let (mut client, doc) = completing("let v = pr");
    let first = request(
        &mut tickets,
        &doc,
        8..10,
        CompletionTrigger::Typed('r'),
        Start::Fresh,
    );
    let _ = client.complete(&doc.snapshot(), &first);
    let output = client
        .receive(list(2, false))
        .expect("the reply is accepted");
    let (stamp, items) = answered(&output);
    assert_eq!(
        stamp,
        update::Stamp::Ticket(first.ticket()),
        "the reply answers the ticket"
    );
    assert_eq!(
        labels(&items),
        ["print", "println", "= prim"],
        "`other` does not match `pr`"
    );
    let replaces: Vec<_> = items.iter().map(|item| item.replace.clone()).collect();
    assert_eq!(
        replaces,
        [None, None, Some(6..10)],
        "only a range other than the word is kept"
    );
}

/// A result that is not a completion result settles the editor's slot like a server error.
#[test]
fn undecodable_completion_result_settles_with_an_empty_list() {
    let mut tickets = Counter::new();
    let (mut client, doc) = completing("let v = pr");
    let first = request(
        &mut tickets,
        &doc,
        8..10,
        CompletionTrigger::Manual,
        Start::Fresh,
    );
    let _ = client.complete(&doc.snapshot(), &first);
    let output = client
        .receive(from_server(
            json!({"jsonrpc": "2.0", "id": 2, "result": "x"}),
        ))
        .expect("an undecodable completion result is not an error");
    let (stamp, items) = answered(&output);
    assert_eq!(
        stamp,
        update::Stamp::Ticket(first.ticket()),
        "stamped with the request's ticket"
    );
    assert!(
        items.is_empty(),
        "the undecodable result answers an empty list"
    );
}

/// A reply computed for a revision the document has left is dropped when it arrives.
#[test]
fn reply_behind_the_synced_revision_is_dropped_in_receive() {
    let mut tickets = Counter::new();
    let (mut client, mut doc) = completing("let v = pr");
    let first = request(
        &mut tickets,
        &doc,
        8..10,
        CompletionTrigger::Manual,
        Start::Fresh,
    );
    let _ = client.complete(&doc.snapshot(), &first);
    doc.edit(vec![EditOp::insert(0, "x")]).expect("edits");
    let _ = client.sync(&doc.snapshot(), doc.drain_changes());
    let output = client
        .receive(list(2, false))
        .expect("the reply is accepted");
    assert_silent(&output, "a reply behind the synced revision is dropped");
}

/// Closing a document cancels its requests before `didClose`.
#[test]
fn close_cancels_pending_requests() {
    let mut tickets = Counter::new();
    let (mut client, doc) = completing("let v = pr");
    let first = request(
        &mut tickets,
        &doc,
        8..10,
        CompletionTrigger::Manual,
        Start::Fresh,
    );
    let _ = client.complete(&doc.snapshot(), &first);
    assert_eq!(
        wire(&client.close(doc.doc_id()).messages),
        vec![
            cancel_request(2),
            json!({"jsonrpc": "2.0", "method": "textDocument/didClose",
                "params": {"textDocument": {"uri": "file:///a.rs"}}}),
        ],
        "the request is cancelled, then the document closed",
    );
}

/// `shutdown` sends nothing itself, settles the request in flight with its empty answer, and the
/// late reply is silent.
#[test]
fn shutdown_settles_pending_requests_with_their_empty_answers() {
    let mut tickets = Counter::new();
    let (mut client, doc) = completing("let v = pr");
    let first = request(
        &mut tickets,
        &doc,
        8..10,
        CompletionTrigger::Manual,
        Start::Fresh,
    );
    let _ = client.complete(&doc.snapshot(), &first);
    let (stamp, items) = answered(&client.shutdown());
    assert_eq!(
        stamp,
        update::Stamp::Ticket(first.ticket()),
        "the settle answers the request's ticket"
    );
    assert!(items.is_empty(), "the settle is the empty answer");
    let output = client
        .receive(list(2, false))
        .expect("the reply is accepted");
    assert_silent(&output, "a forgotten request's reply is dropped");
}

/// Applies `ops` as one typing commit and syncs it.
fn type_ops(client: &mut Session, doc: &mut Document, ops: Vec<EditOp>) {
    doc.edit_grouped(ops, GroupingHint::mergeable(OpClass::Type))
        .expect("types");
    let _ = client.sync(&doc.snapshot(), doc.drain_changes());
}

/// A client whose first request, at `pr` in `let v = pr`, has been answered with [`list`].
fn answered_session(tickets: &mut Counter, incomplete: bool) -> (Session, Document) {
    let (mut client, doc) = completing("let v = pr");
    let first = request(
        tickets,
        &doc,
        8..10,
        CompletionTrigger::Typed('r'),
        Start::Fresh,
    );
    let _ = client.complete(&doc.snapshot(), &first);
    let _ = client
        .receive(list(2, incomplete))
        .expect("the reply is accepted");
    (client, doc)
}

/// Typing into a complete list answers locally, with ranges moved by the typed byte.
#[test]
fn complete_list_is_reused_with_shifted_ranges() {
    let mut tickets = Counter::new();
    let (mut client, mut doc) = answered_session(&mut tickets, false);
    type_ops(&mut client, &mut doc, vec![EditOp::insert(10, "i")]);
    let second = request(
        &mut tickets,
        &doc,
        8..11,
        CompletionTrigger::Typed('i'),
        Start::Continuing,
    );
    let (stamp, items) = answered(&client.complete(&doc.snapshot(), &second));
    assert_eq!(
        stamp,
        update::Stamp::Ticket(second.ticket()),
        "the reuse answers the new ticket"
    );
    assert_eq!(
        labels(&items),
        ["print", "println", "= prim"],
        "items are filtered by `pri`"
    );
    let print = items.iter().find(|i| i.label == "print").expect("kept");
    assert_eq!(print.replace, None, "the word range stays live");
    let prim = items.iter().find(|i| i.label == "= prim").expect("kept");
    assert_eq!(
        prim.replace,
        Some(6..11),
        "a range containing the caret grows"
    );
}

/// An incomplete list is not reused; the server is asked to refine it.
#[test]
fn incomplete_list_re_requests_with_trigger_for_incomplete() {
    let mut tickets = Counter::new();
    let (mut client, mut doc) = answered_session(&mut tickets, true);
    type_ops(&mut client, &mut doc, vec![EditOp::insert(10, "i")]);
    let second = request(
        &mut tickets,
        &doc,
        8..11,
        CompletionTrigger::Typed('i'),
        Start::Continuing,
    );
    let output = client.complete(&doc.snapshot(), &second);
    assert_eq!(
        wire(&output.messages),
        vec![completion_request(3, (0, 11), json!({"triggerKind": 3}))],
        "the list is asked for again at the new caret",
    );
    assert!(output.updates.is_empty(), "nothing is answered locally");
}

/// While a request is in flight, typing that extends the word adopts it: no cancel, no second
/// request, and the reply answers the newest ticket against the newest word.
#[test]
fn continuation_answers_the_latest_ticket_filtered_to_the_latest_word() {
    let mut tickets = Counter::new();
    let (mut client, mut doc) = completing("let v = pr");
    let first = request(
        &mut tickets,
        &doc,
        8..10,
        CompletionTrigger::Typed('r'),
        Start::Fresh,
    );
    let sent = client.complete(&doc.snapshot(), &first);
    assert_eq!(sent.messages.len(), 1, "the first request goes out");

    type_ops(&mut client, &mut doc, vec![EditOp::insert(10, "i")]);
    let second = request(
        &mut tickets,
        &doc,
        8..11,
        CompletionTrigger::Typed('i'),
        Start::Continuing,
    );
    assert_silent(
        &client.complete(&doc.snapshot(), &second),
        "a continuation sends nothing",
    );

    let output = client
        .receive(list(2, false))
        .expect("the reply is accepted");
    let (stamp, items) = answered(&output);
    assert_eq!(
        stamp,
        update::Stamp::Ticket(second.ticket()),
        "the reply answers the latest ticket"
    );
    assert_eq!(
        labels(&items),
        ["print", "println", "= prim"],
        "items are filtered by `pri`"
    );
    let prim = items.iter().find(|i| i.label == "= prim").expect("kept");
    assert_eq!(
        prim.replace,
        Some(6..11),
        "a range containing the caret grows by the typed byte"
    );
}

/// An incomplete reply for a caret that has since moved answers, and asks again at the caret.
#[test]
fn incomplete_reply_after_the_caret_moved_answers_and_re_requests() {
    let mut tickets = Counter::new();
    let (mut client, mut doc) = completing("let v = pr");
    let first = request(
        &mut tickets,
        &doc,
        8..10,
        CompletionTrigger::Typed('r'),
        Start::Fresh,
    );
    let _ = client.complete(&doc.snapshot(), &first);
    type_ops(&mut client, &mut doc, vec![EditOp::insert(10, "i")]);
    let second = request(
        &mut tickets,
        &doc,
        8..11,
        CompletionTrigger::Typed('i'),
        Start::Continuing,
    );
    let _ = client.complete(&doc.snapshot(), &second);

    let output = client
        .receive(list(2, true))
        .expect("the reply is accepted");
    let [update] = output.updates.as_slice() else {
        panic!("expected one update, got {:?}", output.updates)
    };
    let (stamp, items) = completions(update);
    assert_eq!(
        stamp,
        update::Stamp::Ticket(second.ticket()),
        "the reply answers the latest ticket"
    );
    assert_eq!(
        labels(&items),
        ["print", "println", "= prim"],
        "items are filtered by `pri`"
    );
    assert_eq!(
        wire(&output.messages),
        vec![completion_request(3, (0, 11), json!({"triggerKind": 3}))],
        "the incomplete list is asked for again at the moved caret",
    );
}

/// Deleting after the caret changes the text without moving it; the session ends.
#[test]
fn forward_delete_ends_the_session() {
    let mut tickets = Counter::new();
    let (mut client, mut doc) = completing("let v = prx");
    let first = request(
        &mut tickets,
        &doc,
        8..10,
        CompletionTrigger::Typed('r'),
        Start::Fresh,
    );
    let _ = client.complete(&doc.snapshot(), &first);
    let _ = client
        .receive(list(2, false))
        .expect("the reply is accepted");
    type_ops(&mut client, &mut doc, vec![EditOp::delete(10..11)]);
    let second = request(
        &mut tickets,
        &doc,
        8..10,
        CompletionTrigger::Typed('r'),
        Start::Continuing,
    );
    assert_eq!(
        wire(&client.complete(&doc.snapshot(), &second).messages),
        vec![completion_request(3, (0, 10), json!({"triggerKind": 1}))],
        "the list is not reused after a forward delete",
    );
}

/// A list is reused for up to 32 typed bytes; one more asks the server again.
#[test]
fn typing_past_thirty_two_bytes_supersedes() {
    let mut tickets = Counter::new();
    let (mut client, mut doc) = answered_session(&mut tickets, false);
    type_ops(
        &mut client,
        &mut doc,
        vec![EditOp::insert(10, "i".repeat(32))],
    );
    let within = request(
        &mut tickets,
        &doc,
        8..42,
        CompletionTrigger::Typed('i'),
        Start::Continuing,
    );
    let output = client.complete(&doc.snapshot(), &within);
    assert!(
        output.messages.is_empty(),
        "32 bytes past the caret reuses the list"
    );
    assert_eq!(output.updates.len(), 1, "the reuse answers");

    type_ops(&mut client, &mut doc, vec![EditOp::insert(42, "i")]);
    let beyond = request(
        &mut tickets,
        &doc,
        8..43,
        CompletionTrigger::Typed('i'),
        Start::Continuing,
    );
    assert_eq!(
        wire(&client.complete(&doc.snapshot(), &beyond).messages),
        vec![completion_request(3, (0, 43), json!({"triggerKind": 1}))],
        "33 bytes past the caret asks again",
    );
}

/// A backspace moves the caret left of the session's; the session ends.
#[test]
fn caret_left_supersedes() {
    let mut tickets = Counter::new();
    let (mut client, mut doc) = answered_session(&mut tickets, false);
    type_ops(&mut client, &mut doc, vec![EditOp::delete(9..10)]);
    let second = request(
        &mut tickets,
        &doc,
        8..9,
        CompletionTrigger::Typed('p'),
        Start::Continuing,
    );
    assert_eq!(
        wire(&client.complete(&doc.snapshot(), &second).messages),
        vec![completion_request(3, (0, 9), json!({"triggerKind": 1}))],
        "a caret left of the session's asks again",
    );
}

/// An edit inside the word keeps its length arithmetic but changes its text; the session ends.
#[test]
fn changed_prefix_supersedes() {
    let mut tickets = Counter::new();
    let (mut client, mut doc) = answered_session(&mut tickets, false);
    type_ops(
        &mut client,
        &mut doc,
        vec![EditOp::new(8..9, "q"), EditOp::insert(10, "i")],
    );
    let second = request(
        &mut tickets,
        &doc,
        8..11,
        CompletionTrigger::Typed('i'),
        Start::Continuing,
    );
    assert_eq!(
        wire(&client.complete(&doc.snapshot(), &second).messages),
        vec![completion_request(3, (0, 11), json!({"triggerKind": 1}))],
        "a changed prefix asks again",
    );
}

/// Ctrl+Space always asks the server, even at an unchanged revision.
#[test]
fn manual_request_never_continues() {
    let mut tickets = Counter::new();
    let (mut client, doc) = completing("let v = pr");
    let first = request(
        &mut tickets,
        &doc,
        8..10,
        CompletionTrigger::Manual,
        Start::Continuing,
    );
    let _ = client.complete(&doc.snapshot(), &first);
    let second = request(
        &mut tickets,
        &doc,
        8..10,
        CompletionTrigger::Manual,
        Start::Continuing,
    );
    assert_eq!(
        wire(&client.complete(&doc.snapshot(), &second).messages),
        vec![
            cancel_request(2),
            completion_request(3, (0, 10), json!({"triggerKind": 1}))
        ],
        "the second manual request supersedes the first",
    );
}

/// A first request dropped before its list arrived leaves nothing to refine: the continuing
/// request that follows is sent as a fresh, invoked one.
#[test]
fn continuing_request_after_a_dropped_first_request_starts_fresh() {
    let mut tickets = Counter::new();
    let (mut client, mut doc) = completing("let v = pr");
    let first = request(
        &mut tickets,
        &doc,
        8..10,
        CompletionTrigger::Typed('r'),
        Start::Fresh,
    );
    let _ = client.complete(&doc.snapshot(), &first);
    type_ops(&mut client, &mut doc, vec![EditOp::insert(10, "i")]);
    let dropped = client
        .receive(list(2, false))
        .expect("the reply is accepted");
    assert_silent(&dropped, "the first reply is behind the synced revision");
    let second = request(
        &mut tickets,
        &doc,
        8..11,
        CompletionTrigger::Typed('i'),
        Start::Continuing,
    );
    assert_eq!(
        wire(&client.complete(&doc.snapshot(), &second).messages),
        vec![completion_request(3, (0, 11), json!({"triggerKind": 1}))],
        "the request is invoked, not a refinement of a list never received",
    );
}

/// Server capabilities with incremental sync, hover, and signature help triggered by `(` and `,`.
fn signature_hover_capabilities() -> Value {
    json!({
        "textDocumentSync": 2,
        "signatureHelpProvider": {"triggerCharacters": ["(", ","]},
        "hoverProvider": true,
    })
}

/// A running client with `text` open as `file:///a.rs`, whose server offers signature help and
/// hover.
fn signing(text: &str) -> (Session, Document) {
    let doc = document(text);
    let (mut client, _) = running(Builder::default(), signature_hover_capabilities());
    let _ = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    (client, doc)
}

/// A signature request at `column` on line 0 of `doc`, inside the call whose `(` is at `call`,
/// under a fresh ticket.
fn signature_request(
    tickets: &mut Counter,
    doc: &Document,
    column: u32,
    call: Option<u32>,
) -> SignatureRequest {
    SignatureRequest::new(tickets.issue(doc.revision()), Point::new(0, column), call)
}

fn signature_help_request(id: i64, column: u32) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "textDocument/signatureHelp", "params": {
        "textDocument": {"uri": "file:///a.rs"},
        "position": {"line": 0, "character": column},
    }})
}

/// The reply to request `id`: one signature `foo(a: i32, b: i32)` with the first parameter
/// active.
fn signature_reply(id: i64) -> Message {
    from_server(json!({"jsonrpc": "2.0", "id": id, "result": {
        "signatures": [{"label": "foo(a: i32, b: i32)", "parameters": [{"label": "a: i32"}, {"label": "b: i32"}]}],
        "activeParameter": 0,
    }}))
}

/// The one signature update in `output`, which sends nothing.
fn signed(output: &Output) -> (update::Stamp, Option<SignatureInfo>) {
    assert!(
        output.messages.is_empty(),
        "an answer sends nothing: {output:?}"
    );
    let [update] = output.updates.as_slice() else {
        panic!("expected one update, got {:?}", output.updates)
    };
    signature(update)
}

/// `initialize` advertises plain-text signature documentation, label offsets and per-signature
/// active parameters, no signature context, and markdown hover contents.
#[test]
fn signature_and_hover_capabilities_are_advertised() {
    let (_, initialize) = Builder::default().session(None);
    let initialize = serde_json::to_value(&initialize).expect("serializes");
    let signature = &initialize["params"]["capabilities"]["textDocument"]["signatureHelp"];
    let information = &signature["signatureInformation"];
    assert_eq!(
        information["documentationFormat"],
        json!(["plaintext"]),
        "documentation is plain text"
    );
    assert_eq!(
        information["parameterInformation"]["labelOffsetSupport"],
        json!(true),
        "label offsets are advertised"
    );
    assert_eq!(
        information["activeParameterSupport"],
        json!(true),
        "per-signature active parameters are advertised"
    );
    assert_eq!(
        signature.get("contextSupport"),
        None,
        "context is not advertised"
    );
    assert_eq!(
        initialize["params"]["capabilities"]["textDocument"]["hover"]["contentFormat"],
        json!(["markdown", "plaintext"]),
        "markdown hover is preferred"
    );
}

/// A request goes out at the caret, without a context.
#[test]
fn signature_request_carries_the_caret_position() {
    let mut tickets = Counter::new();
    let (mut client, doc) = signing("foo(");
    let first = signature_request(&mut tickets, &doc, 4, Some(3));
    let output = client.signature_help(&doc.snapshot(), &first);
    assert_eq!(
        wire(&output.messages),
        vec![signature_help_request(2, 4)],
        "the request asks at the caret"
    );
    assert!(output.updates.is_empty(), "nothing is answered yet");
}

/// With no server to ask, or one without a provider, the editor stops waiting at once.
#[test]
fn signature_declines_before_initialize_and_without_a_provider() {
    let doc = document("foo(");
    let (initializing, _) = Builder::default().session(None);
    let (running, _) = running(Builder::default(), json!({"textDocumentSync": 2}));
    for mut client in [initializing, running] {
        let mut tickets = Counter::new();
        let _ = client
            .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
            .expect("opens");
        let first = signature_request(&mut tickets, &doc, 4, Some(3));
        let (stamp, info) = signed(&client.signature_help(&doc.snapshot(), &first));
        assert_eq!(
            stamp,
            update::Stamp::Ticket(first.ticket()),
            "the decline answers the ticket"
        );
        assert!(info.is_none(), "the decline closes the box");
    }
}

/// Typing inside the call of the request in flight sends nothing new.
#[test]
fn same_call_adopts_the_in_flight_signature_request() {
    let mut tickets = Counter::new();
    let (mut client, mut doc) = signing("foo(");
    let first = signature_request(&mut tickets, &doc, 4, Some(3));
    let _ = client.signature_help(&doc.snapshot(), &first);
    type_ops(&mut client, &mut doc, vec![EditOp::insert(4, "a")]);
    let second = signature_request(&mut tickets, &doc, 5, Some(3));
    assert_silent(
        &client.signature_help(&doc.snapshot(), &second),
        "the same call adopts the in-flight request",
    );
}

/// When the adopted request's reply lands with the caret elsewhere, the answer is delivered and
/// the request re-issued at the new caret.
#[test]
fn signature_reply_after_the_caret_moved_is_delivered_and_reissued() {
    let mut tickets = Counter::new();
    let (mut client, mut doc) = signing("foo(");
    let first = signature_request(&mut tickets, &doc, 4, Some(3));
    let _ = client.signature_help(&doc.snapshot(), &first);
    type_ops(&mut client, &mut doc, vec![EditOp::insert(4, "a")]);
    let second = signature_request(&mut tickets, &doc, 5, Some(3));
    let _ = client.signature_help(&doc.snapshot(), &second);
    let output = client
        .receive(signature_reply(2))
        .expect("the reply is accepted");
    let [update] = output.updates.as_slice() else {
        panic!("expected one update, got {:?}", output.updates)
    };
    let (stamp, info) = signature(update);
    assert_eq!(
        stamp,
        update::Stamp::Ticket(second.ticket()),
        "the reply answers the latest ticket"
    );
    let info = info.expect("the signature is delivered");
    assert_eq!(info.params, vec![4..10, 12..18], "the parameters are located");
    assert_eq!(
        wire(&output.messages),
        vec![signature_help_request(3, 5)],
        "the request is re-issued at the moved caret"
    );
}

/// A reply at the caret the editor last asked about ends the continuation.
#[test]
fn reissued_signature_reply_at_the_latest_caret_ends_the_continuation() {
    let mut tickets = Counter::new();
    let (mut client, mut doc) = signing("foo(");
    let first = signature_request(&mut tickets, &doc, 4, Some(3));
    let _ = client.signature_help(&doc.snapshot(), &first);
    type_ops(&mut client, &mut doc, vec![EditOp::insert(4, "a")]);
    let second = signature_request(&mut tickets, &doc, 5, Some(3));
    let _ = client.signature_help(&doc.snapshot(), &second);
    let _ = client
        .receive(signature_reply(2))
        .expect("the reply is accepted");
    let (stamp, info) = signed(
        &client
            .receive(signature_reply(3))
            .expect("the reply is accepted"),
    );
    assert_eq!(
        stamp,
        update::Stamp::Ticket(second.ticket()),
        "the re-issue answers the latest ticket"
    );
    assert!(info.is_some(), "the signature is delivered");
}

/// A request in another call replaces the one in flight.
#[test]
fn different_call_supersedes_with_a_cancel() {
    let mut tickets = Counter::new();
    let (mut client, doc) = signing("foo(bar(");
    let outer = signature_request(&mut tickets, &doc, 8, Some(3));
    let _ = client.signature_help(&doc.snapshot(), &outer);
    let inner = signature_request(&mut tickets, &doc, 8, Some(7));
    assert_eq!(
        wire(&client.signature_help(&doc.snapshot(), &inner).messages),
        vec![cancel_request(2), signature_help_request(3, 8)],
        "the outer call's request is cancelled before the inner one goes out"
    );
}

/// Requests outside any call share no call, so the newer one supersedes.
#[test]
fn request_outside_any_call_never_continues() {
    let mut tickets = Counter::new();
    let (mut client, doc) = signing("foo");
    let first = signature_request(&mut tickets, &doc, 3, None);
    let _ = client.signature_help(&doc.snapshot(), &first);
    let second = signature_request(&mut tickets, &doc, 3, None);
    assert_eq!(
        wire(&client.signature_help(&doc.snapshot(), &second).messages),
        vec![cancel_request(2), signature_help_request(3, 3)],
        "the second request cancels the first"
    );
}

/// A failed or undecodable signature reply closes the box instead of leaving it waiting.
#[test]
fn failed_signature_request_answers_none() {
    for reply in [
        failure(2, -32603),
        from_server(json!({"jsonrpc": "2.0", "id": 2, "result": "x"})),
    ] {
        let mut tickets = Counter::new();
        let (mut client, doc) = signing("foo(");
        let first = signature_request(&mut tickets, &doc, 4, Some(3));
        let _ = client.signature_help(&doc.snapshot(), &first);
        let (stamp, info) = signed(
            &client
                .receive(reply)
                .expect("a signature failure is not an error"),
        );
        assert_eq!(
            stamp,
            update::Stamp::Ticket(first.ticket()),
            "stamped with the request's ticket"
        );
        assert!(info.is_none(), "the failure closes the box");
    }
}

/// A content-modified signature request is sent again once, then given up.
#[test]
fn content_modified_reissues_signature_help_once() {
    let mut tickets = Counter::new();
    let (mut client, doc) = signing("foo(");
    let first = signature_request(&mut tickets, &doc, 4, Some(3));
    let _ = client.signature_help(&doc.snapshot(), &first);
    let reissued = client
        .receive(failure(2, -32801))
        .expect("content modified is not an error");
    assert_eq!(
        wire(&reissued.messages),
        vec![signature_help_request(3, 4)],
        "the request goes out again at its caret"
    );
    assert!(reissued.updates.is_empty(), "nothing is answered yet");
    assert_silent(
        &client
            .receive(failure(3, -32801))
            .expect("content modified is not an error"),
        "a second content-modified reply for one ticket is final",
    );
}

/// Requests from an older revision are neither sent nor answered.
#[test]
fn stale_signature_and_hover_requests_are_ignored() {
    let mut tickets = Counter::new();
    let (mut client, mut doc) = signing("foo(");
    type_ops(&mut client, &mut doc, vec![EditOp::insert(4, "a")]);
    let stale = SignatureRequest::new(tickets.issue(Revision(0)), Point::new(0, 4), Some(3));
    assert_silent(
        &client.signature_help(&doc.snapshot(), &stale),
        "a stale signature request is ignored",
    );
    let stale = HoverRequest::new(tickets.issue(Revision(0)), 1, 0..3);
    assert_silent(
        &client.hover(&doc.snapshot(), &stale),
        "a stale hover request is ignored",
    );
}

/// A hover request for `value` in `doc` (`let value = 1;`), under a fresh ticket.
fn hover_request(tickets: &mut Counter, doc: &Document) -> HoverRequest {
    HoverRequest::new(tickets.issue(doc.revision()), 5, 4..9)
}

fn hover_wire(id: i64) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "textDocument/hover", "params": {
        "textDocument": {"uri": "file:///a.rs"},
        "position": {"line": 0, "character": 5},
    }})
}

/// The one hover update in `output`, which sends nothing.
fn hovered(output: &Output) -> (update::Stamp, Option<HoverInfo>) {
    assert!(
        output.messages.is_empty(),
        "an answer sends nothing: {output:?}"
    );
    let [update] = output.updates.as_slice() else {
        panic!("expected one update, got {:?}", output.updates)
    };
    hover(update)
}

/// A hover request goes out at the offset under the pointer.
#[test]
fn hover_request_carries_the_offset_position() {
    let mut tickets = Counter::new();
    let (mut client, doc) = signing("let value = 1;");
    let output = client.hover(&doc.snapshot(), &hover_request(&mut tickets, &doc));
    assert_eq!(
        wire(&output.messages),
        vec![hover_wire(2)],
        "the request asks at the pointer"
    );
    assert!(output.updates.is_empty(), "nothing is answered yet");
}

/// With no server to ask, or one without a provider, the card gets no docs at once.
#[test]
fn hover_declines_before_initialize_and_without_a_provider() {
    let doc = document("let value = 1;");
    let (initializing, _) = Builder::default().session(None);
    let (without, _) = running(Builder::default(), json!({"textDocumentSync": 2}));
    let (disabled, _) = running(
        Builder::default(),
        json!({"textDocumentSync": 2, "hoverProvider": false}),
    );
    for mut client in [initializing, without, disabled] {
        let mut tickets = Counter::new();
        let _ = client
            .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
            .expect("opens");
        let first = hover_request(&mut tickets, &doc);
        let (stamp, info) = hovered(&client.hover(&doc.snapshot(), &first));
        assert_eq!(
            stamp,
            update::Stamp::Ticket(first.ticket),
            "the decline answers the ticket"
        );
        assert!(info.is_none(), "the decline has no docs");
    }
}

/// A new hover cancels the one in flight.
#[test]
fn new_hover_supersedes_the_previous_one() {
    let mut tickets = Counter::new();
    let (mut client, doc) = signing("let value = 1;");
    let _ = client.hover(&doc.snapshot(), &hover_request(&mut tickets, &doc));
    assert_eq!(
        wire(&client.hover(&doc.snapshot(), &hover_request(&mut tickets, &doc)).messages),
        vec![cancel_request(2), hover_wire(3)],
        "the first hover is cancelled before the second goes out"
    );
}

/// A reply becomes a card in the editor's markdown subset over the server's range.
#[test]
fn hover_reply_answers_with_the_converted_card() {
    let mut tickets = Counter::new();
    let (mut client, doc) = signing("let value = 1;");
    let first = hover_request(&mut tickets, &doc);
    let _ = client.hover(&doc.snapshot(), &first);
    let (stamp, info) = hovered(
        &client
            .receive(from_server(json!({"jsonrpc": "2.0", "id": 2, "result": {
                "contents": {"kind": "markdown", "value": "```rust\nlet value: i32\n```"},
                "range": {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 9}},
            }})))
            .expect("the reply is accepted"),
    );
    assert_eq!(
        stamp,
        update::Stamp::Ticket(first.ticket),
        "the card answers the ticket"
    );
    let info = info.expect("a card");
    assert_eq!(info.markdown, "`let value: i32`", "the fence is a code line");
    assert_eq!(info.range, 4..9, "the server's range is kept");
}

/// A `null` reply means no docs.
#[test]
fn null_hover_reply_answers_none() {
    let mut tickets = Counter::new();
    let (mut client, doc) = signing("let value = 1;");
    let first = hover_request(&mut tickets, &doc);
    let _ = client.hover(&doc.snapshot(), &first);
    let (_, info) = hovered(
        &client
            .receive(from_server(
                json!({"jsonrpc": "2.0", "id": 2, "result": null}),
            ))
            .expect("the reply is accepted"),
    );
    assert!(info.is_none(), "null has no docs");
}

/// A failed or undecodable hover reply settles the card instead of leaving it waiting.
#[test]
fn failed_hover_request_answers_none() {
    for reply in [
        failure(2, -32603),
        from_server(json!({"jsonrpc": "2.0", "id": 2, "result": "x"})),
    ] {
        let mut tickets = Counter::new();
        let (mut client, doc) = signing("let value = 1;");
        let first = hover_request(&mut tickets, &doc);
        let _ = client.hover(&doc.snapshot(), &first);
        let (stamp, info) = hovered(
            &client
                .receive(reply)
                .expect("a hover failure is not an error"),
        );
        assert_eq!(
            stamp,
            update::Stamp::Ticket(first.ticket),
            "stamped with the request's ticket"
        );
        assert!(info.is_none(), "the failure has no docs");
    }
}

/// Server capabilities with incremental sync, utf-8 positions (fixture columns are bytes) and
/// formatting.
fn command_capabilities() -> Value {
    json!({
        "positionEncoding": "utf-8",
        "textDocumentSync": 2,
        "definitionProvider": true,
        "renameProvider": true,
        "documentFormattingProvider": true,
    })
}

/// A running client with `text` open as `file:///a.rs`, whose server offers the commands.
fn commanding(text: &str) -> (Session, Document) {
    let doc = document(text);
    let (mut client, _) = running(Builder::default(), command_capabilities());
    let _ = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    (client, doc)
}

/// The success reply to request `id`.
fn reply(id: i64, result: Value) -> Message {
    from_server(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// A `TextEdit` over `start..end` (line, character).
fn text_edit(start: (u32, u32), end: (u32, u32), text: &str) -> Value {
    json!({"range": {
        "start": {"line": start.0, "character": start.1},
        "end": {"line": end.0, "character": end.1},
    }, "newText": text})
}

/// `initialize` advertises formatting.
#[test]
fn initialize_advertises_formatting() {
    let (_, initialize) = Builder::default().session(None);
    let initialize = serde_json::to_value(&initialize).expect("serializes");
    assert_eq!(
        initialize.pointer("/params/capabilities/textDocument/formatting"),
        Some(&json!({})),
        "formatting is advertised",
    );
}

/// A format request asks for the whole document, indented with spaces at the request's width.
#[test]
fn format_request_asks_for_spaces_at_the_tab_size() {
    let mut tickets = Counter::new();
    let (mut client, doc) = commanding("fn a() {}  \n");
    let request = FormatRequest::new(tickets.issue(doc.revision()), 4);
    assert_eq!(
        wire(&client.format(&doc.snapshot(), &request).messages),
        vec![json!({"jsonrpc": "2.0", "id": 2, "method": "textDocument/formatting", "params": {
            "textDocument": {"uri": "file:///a.rs"},
            "options": {"tabSize": 4, "insertSpaces": true},
        }})],
        "the request goes out with spaces",
    );
}

/// The formatted edits answer the request's ticket, trimmed and in LF.
#[test]
fn format_reply_is_stamped_with_the_request_ticket() {
    let mut tickets = Counter::new();
    let (mut client, doc) = commanding("fn a() {}  \n");
    let request = FormatRequest::new(tickets.issue(doc.revision()), 4);
    let _ = client.format(&doc.snapshot(), &request);
    let output = client
        .receive(reply(2, json!([text_edit((0, 9), (0, 11), "")])))
        .expect("the reply is accepted");
    assert!(output.messages.is_empty(), "the answer sends nothing");
    let [update] = output.updates.as_slice() else {
        panic!("expected one update, got {:?}", output.updates)
    };
    let (stamp, ops) = edits(update);
    assert_eq!(
        stamp,
        update::Stamp::Ticket(request.ticket),
        "stamped with the request's ticket"
    );
    assert_eq!(
        ops,
        vec![EditOp::delete(9..11)],
        "the trailing blanks go"
    );
}

/// A reply to a format asked before the latest edit would rewrite text that has moved.
#[test]
fn format_reply_after_an_edit_is_dropped() {
    let mut tickets = Counter::new();
    let (mut client, mut doc) = commanding("fn a() {}  \n");
    let request = FormatRequest::new(tickets.issue(doc.revision()), 4);
    let _ = client.format(&doc.snapshot(), &request);
    type_ops(&mut client, &mut doc, vec![EditOp::insert(0, "x")]);
    assert_silent(
        &client
            .receive(reply(2, json!([text_edit((0, 9), (0, 11), "")])))
            .expect("a stale reply is not an error"),
        "a stale format reply is dropped",
    );
}

/// A result with nothing to change answers nothing: formatting has no slot to settle.
#[test]
fn format_reply_without_changes_answers_nothing() {
    for result in [json!(null), json!([]), json!([text_edit((0, 0), (0, 2), "fn")])] {
        let mut tickets = Counter::new();
        let (mut client, doc) = commanding("fn a() {}\n");
        let _ = client.format(
            &doc.snapshot(),
            &FormatRequest::new(tickets.issue(doc.revision()), 4),
        );
        assert_silent(
            &client.receive(reply(2, result)).expect("the reply is accepted"),
            "nothing to change",
        );
    }
}

/// A user asked for the format, so a failed or undecodable reply is reported, not swallowed.
#[test]
fn format_failures_are_errors() {
    let mut tickets = Counter::new();
    let (mut client, doc) = commanding("fn a() {}\n");
    let request = FormatRequest::new(tickets.issue(doc.revision()), 4);
    let _ = client.format(&doc.snapshot(), &request);
    let error = client
        .receive(failure(2, -32603))
        .expect_err("a server error is reported");
    assert!(
        matches!(&error, Error::Server { doc_id: Some(id), method, .. }
            if *id == doc.doc_id() && method == "textDocument/formatting"),
        "the error names the document and the method: {error:?}",
    );
    let _ = client.format(&doc.snapshot(), &request);
    let error = client
        .receive(reply(3, json!("x")))
        .expect_err("an undecodable result is reported");
    assert!(
        matches!(&error, Error::Decode { method, .. } if method == "textDocument/formatting"),
        "the error names the method: {error:?}",
    );
}

/// With no server to ask, or one without a provider, a format sends nothing and answers nothing.
#[test]
fn format_declines_before_initialize_and_without_a_provider() {
    let doc = document("fn a() {}\n");
    let (initializing, _) = Builder::default().session(None);
    let (without, _) = running(Builder::default(), json!({"textDocumentSync": 2}));
    for mut client in [initializing, without] {
        let mut tickets = Counter::new();
        let _ = client
            .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
            .expect("opens");
        assert_silent(
            &client.format(
                &doc.snapshot(),
                &FormatRequest::new(tickets.issue(doc.revision()), 4),
            ),
            "a declined format sends nothing",
        );
    }
}

/// `a.rs` calls `greet`, which `b.rs` defines at `3..8`.
const CALLER: &str = "fn a() {}\ngreet();\n";
const CALLEE: &str = "fn greet() {}\n";

/// Opens `doc` on `client` as `uri`.
fn open_as(client: &mut Session, doc: &Document, uri_text: &str) {
    let _ = client
        .open(&doc.snapshot(), &uri(uri_text), "rust")
        .expect("opens");
}

/// A range on one line.
fn span_on(line: u32, start: u32, end: u32) -> Value {
    json!({"start": {"line": line, "character": start}, "end": {"line": line, "character": end}})
}

/// The one definition update in `output`, which sends nothing.
fn defined(output: &Output) -> (update::Stamp, Option<update::Target>) {
    assert!(
        output.messages.is_empty(),
        "an answer sends nothing: {output:?}"
    );
    let [update] = output.updates.as_slice() else {
        panic!("expected one update, got {:?}", output.updates)
    };
    definition(update)
}

/// `initialize` advertises goto definition with location links.
#[test]
fn initialize_advertises_definition_links() {
    let (_, initialize) = Builder::default().session(None);
    let initialize = serde_json::to_value(&initialize).expect("serializes");
    assert_eq!(
        initialize.pointer("/params/capabilities/textDocument/definition/linkSupport"),
        Some(&json!(true)),
        "location links are accepted",
    );
}

/// A definition in the requesting document selects its span there, under the request's ticket.
#[test]
fn definition_in_the_same_document_is_a_local_target() {
    let mut tickets = Counter::new();
    let (mut client, doc) = commanding("fn f() {}\nf();\n");
    let request = DefinitionRequest::new(tickets.issue(doc.revision()), 10);
    assert_eq!(
        wire(&client.definition(&doc.snapshot(), &request).messages),
        vec![json!({"jsonrpc": "2.0", "id": 2, "method": "textDocument/definition", "params": {
            "textDocument": {"uri": "file:///a.rs"},
            "position": {"line": 1, "character": 0},
        }})],
        "the request asks at the offset",
    );
    let (stamp, target) = defined(
        &client
            .receive(reply(2, json!({"uri": "file:///a.rs", "range": span_on(0, 3, 4)})))
            .expect("the reply is accepted"),
    );
    assert_eq!(
        stamp,
        update::Stamp::Ticket(request.ticket),
        "stamped with the request's ticket"
    );
    assert_eq!(target, Some(update::Target::Local(3..4)), "the local span");
}

/// A definition in another open document is converted against that document's synced text.
#[test]
fn definition_in_another_open_document_is_an_open_target() {
    let mut tickets = Counter::new();
    let (mut client, doc) = commanding(CALLER);
    let callee = document(CALLEE);
    open_as(&mut client, &callee, "file:///b.rs");
    let request = DefinitionRequest::new(tickets.issue(doc.revision()), 10);
    let _ = client.definition(&doc.snapshot(), &request);
    let (_, target) = defined(
        &client
            .receive(reply(2, json!([{"uri": "file:///b.rs", "range": span_on(0, 3, 8)}])))
            .expect("the reply is accepted"),
    );
    let Some(update::Target::Open(open)) = target else {
        panic!("expected an open target, got {target:?}")
    };
    assert_eq!(open.doc_id(), callee.doc_id(), "the callee's document");
    assert_eq!(open.revision(), callee.revision(), "at its synced revision");
    assert_eq!(open.span(), 3..8, "over `greet`");
}

/// A definition in an open document that moved since the request points at text the server
/// never saw, so it is dropped.
#[test]
fn definition_in_an_open_document_that_moved_is_dropped() {
    let mut tickets = Counter::new();
    let (mut client, doc) = commanding(CALLER);
    let mut callee = document(CALLEE);
    open_as(&mut client, &callee, "file:///b.rs");
    let request = DefinitionRequest::new(tickets.issue(doc.revision()), 10);
    let _ = client.definition(&doc.snapshot(), &request);
    type_ops(&mut client, &mut callee, vec![EditOp::insert(0, "\n")]);
    let (stamp, target) = defined(
        &client
            .receive(reply(2, json!({"uri": "file:///b.rs", "range": span_on(0, 3, 8)})))
            .expect("the reply is accepted"),
    );
    assert_eq!(
        stamp,
        update::Stamp::Ticket(request.ticket),
        "the requester still settles"
    );
    assert_eq!(target, None, "the moved target is dropped");
}

/// A document opened after the request has no recorded revision, so a definition in it is
/// dropped.
#[test]
fn definition_in_a_document_opened_after_the_request_is_dropped() {
    let mut tickets = Counter::new();
    let (mut client, doc) = commanding(CALLER);
    let request = DefinitionRequest::new(tickets.issue(doc.revision()), 10);
    let _ = client.definition(&doc.snapshot(), &request);
    open_as(&mut client, &document(CALLEE), "file:///b.rs");
    let (_, target) = defined(
        &client
            .receive(reply(2, json!({"uri": "file:///b.rs", "range": span_on(0, 3, 8)})))
            .expect("the reply is accepted"),
    );
    assert_eq!(target, None, "the late-opened target is dropped");
}

/// A definition in a file nobody opened keeps the server's range for the host to convert.
#[test]
fn definition_in_an_unopened_document_is_an_unopened_target() {
    let mut tickets = Counter::new();
    let (mut client, doc) = commanding(CALLER);
    let request = DefinitionRequest::new(tickets.issue(doc.revision()), 10);
    let _ = client.definition(&doc.snapshot(), &request);
    let (_, target) = defined(
        &client
            .receive(reply(2, json!({"uri": "file:///w/c.rs", "range": span_on(1, 3, 4)})))
            .expect("the reply is accepted"),
    );
    let Some(update::Target::Unopened(unopened)) = target else {
        panic!("expected an unopened target, got {target:?}")
    };
    assert_eq!(unopened.uri().as_str(), "file:///w/c.rs", "the file");
    assert_eq!(unopened.span("x\nfn c() {}\n"), 5..6, "converted against the file's text");
}

/// A `LocationLink` selects its name, not the whole definition.
#[test]
fn location_link_definition_uses_the_target_selection_range() {
    let mut tickets = Counter::new();
    let (mut client, doc) = commanding(CALLER);
    let callee = document(CALLEE);
    open_as(&mut client, &callee, "file:///b.rs");
    let request = DefinitionRequest::new(tickets.issue(doc.revision()), 10);
    let _ = client.definition(&doc.snapshot(), &request);
    let (_, target) = defined(
        &client
            .receive(reply(2, json!([{
                "targetUri": "file:///b.rs",
                "targetRange": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 13}},
                "targetSelectionRange": span_on(0, 3, 8),
            }])))
            .expect("the reply is accepted"),
    );
    let Some(update::Target::Open(open)) = target else {
        panic!("expected an open target, got {target:?}")
    };
    assert_eq!(open.span(), 3..8, "the selection range");
}

/// With no server to ask, or one without a provider, the editor stops waiting at once.
#[test]
fn definition_without_a_provider_declines_with_none() {
    let doc = document(CALLER);
    let (initializing, _) = Builder::default().session(None);
    let (without, _) = running(Builder::default(), json!({"textDocumentSync": 2}));
    for mut client in [initializing, without] {
        let mut tickets = Counter::new();
        open_as(&mut client, &doc, "file:///a.rs");
        let request = DefinitionRequest::new(tickets.issue(doc.revision()), 10);
        let (stamp, target) = defined(&client.definition(&doc.snapshot(), &request));
        assert_eq!(
            stamp,
            update::Stamp::Ticket(request.ticket),
            "the decline answers the ticket"
        );
        assert_eq!(target, None, "the decline has no target");
    }
}

/// A user asked for the definition, so a failed or undecodable reply is reported.
#[test]
fn definition_server_error_is_a_server_error() {
    let mut tickets = Counter::new();
    let (mut client, doc) = commanding(CALLER);
    let request = DefinitionRequest::new(tickets.issue(doc.revision()), 10);
    let _ = client.definition(&doc.snapshot(), &request);
    let error = client
        .receive(failure(2, -32603))
        .expect_err("a server error is reported");
    assert!(
        matches!(&error, Error::Server { method, .. } if method == "textDocument/definition"),
        "the error names the method: {error:?}",
    );
    let _ = client.definition(&doc.snapshot(), &request);
    let error = client
        .receive(reply(3, json!("x")))
        .expect_err("an undecodable result is reported");
    assert!(
        matches!(&error, Error::Decode { method, .. } if method == "textDocument/definition"),
        "the error names the method: {error:?}",
    );
}

/// A running client with [`CALLER`] open as `a.rs` and [`CALLEE`] as `b.rs`, and a rename of
/// `greet` in the caller sent as request 2.
fn renaming(tickets: &mut Counter) -> (Session, Document, Document) {
    let (mut client, caller) = commanding(CALLER);
    let callee = document(CALLEE);
    open_as(&mut client, &callee, "file:///b.rs");
    let request = RenameRequest::new(tickets.issue(caller.revision()), 10, "hail");
    let _ = client.rename(&caller.snapshot(), &request);
    (client, caller, callee)
}

/// A `TextDocumentEdit` renaming `greet` in the caller (`file:///a.rs`) or the callee.
fn rename_in(uri: &str, version: Value) -> Value {
    let range = if uri == "file:///a.rs" {
        span_on(1, 0, 5)
    } else {
        span_on(0, 3, 8)
    };
    json!({"textDocument": {"uri": uri, "version": version},
        "edits": [{"range": range, "newText": "hail"}]})
}

/// The documents, stamps and ops of `output`'s document updates.
fn renamed_documents(output: &Output) -> Vec<(DocId, update::Stamp, Vec<EditOp>)> {
    output
        .updates
        .iter()
        .map(|update| {
            let Update::Document(document) = update else {
                panic!("expected a document update, got {update:?}")
            };
            let (stamp, ops) = edits(update);
            (document.doc_id(), stamp, ops)
        })
        .collect()
}

/// The stale document of a rename reply that `receive` refused.
fn stale(result: Result<Output, Error>) -> String {
    match result {
        Err(Error::StaleEdit { uri }) => uri.as_str().to_owned(),
        other => panic!("expected a stale edit, got {other:?}"),
    }
}

/// `initialize` advertises rename without prepare, and versioned, transactional workspace edits
/// without file operations.
#[test]
fn initialize_advertises_rename_and_transactional_edits() {
    let (_, initialize) = Builder::default().session(None);
    let initialize = serde_json::to_value(&initialize).expect("serializes");
    for (pointer, expected) in [
        ("/params/capabilities/textDocument/rename", Some(json!({}))),
        (
            "/params/capabilities/workspace/workspaceEdit/documentChanges",
            Some(json!(true)),
        ),
        (
            "/params/capabilities/workspace/workspaceEdit/failureHandling",
            Some(json!("transactional")),
        ),
        (
            "/params/capabilities/workspace/workspaceEdit/resourceOperations",
            None,
        ),
    ] {
        assert_eq!(
            initialize.pointer(pointer),
            expected.as_ref(),
            "{pointer} is advertised as expected"
        );
    }
}

/// A rename request asks at the offset for the new name.
#[test]
fn rename_request_carries_the_position_and_new_name() {
    let mut tickets = Counter::new();
    let (mut client, doc) = commanding(CALLER);
    let request = RenameRequest::new(tickets.issue(doc.revision()), 10, "hail");
    assert_eq!(
        wire(&client.rename(&doc.snapshot(), &request).messages),
        vec![json!({"jsonrpc": "2.0", "id": 2, "method": "textDocument/rename", "params": {
            "textDocument": {"uri": "file:///a.rs"},
            "position": {"line": 1, "character": 0},
            "newName": "hail",
        }})],
        "the request goes out",
    );
}

/// `documentChanges` for two open documents land as one revision-stamped batch each.
#[test]
fn rename_via_document_changes_edits_two_open_documents() {
    let mut tickets = Counter::new();
    let (mut client, caller, callee) = renaming(&mut tickets);
    let output = client
        .receive(reply(2, json!({"documentChanges": [
            rename_in("file:///a.rs", json!(1)),
            rename_in("file:///b.rs", json!(1)),
        ]})))
        .expect("the rename is accepted");
    assert!(output.messages.is_empty(), "the answer sends nothing");
    assert_eq!(
        renamed_documents(&output),
        vec![
            (
                caller.doc_id(),
                update::Stamp::Revision(caller.revision()),
                vec![EditOp::new(10..15, "hail")]
            ),
            (
                callee.doc_id(),
                update::Stamp::Revision(callee.revision()),
                vec![EditOp::new(3..8, "hail")]
            ),
        ],
        "both documents are renamed at their synced revisions",
    );
}

/// The `changes` map lands in URI order.
#[test]
fn rename_via_the_changes_map_edits_two_open_documents() {
    let mut tickets = Counter::new();
    let (mut client, caller, callee) = renaming(&mut tickets);
    let output = client
        .receive(reply(2, json!({"changes": {
            "file:///b.rs": [{"range": span_on(0, 3, 8), "newText": "hail"}],
            "file:///a.rs": [{"range": span_on(1, 0, 5), "newText": "hail"}],
        }})))
        .expect("the rename is accepted");
    let documents: Vec<DocId> = renamed_documents(&output)
        .into_iter()
        .map(|(doc_id, _, _)| doc_id)
        .collect();
    assert_eq!(
        documents,
        vec![caller.doc_id(), callee.doc_id()],
        "a.rs before b.rs"
    );
}

/// A touched document that moved since the request refuses the whole rename.
#[test]
fn rename_rejects_a_document_that_moved_since_the_request() {
    let mut tickets = Counter::new();
    let (mut client, _, mut callee) = renaming(&mut tickets);
    type_ops(&mut client, &mut callee, vec![EditOp::insert(0, "\n")]);
    assert_eq!(
        stale(client.receive(reply(2, json!({"documentChanges": [
            rename_in("file:///a.rs", json!(1)),
            rename_in("file:///b.rs", json!(null)),
        ]})))),
        "file:///b.rs",
        "the moved callee is stale",
    );
}

/// A document opened after the request was never seen by the rename.
#[test]
fn rename_rejects_a_document_opened_after_the_request() {
    let mut tickets = Counter::new();
    let (mut client, caller) = commanding(CALLER);
    let request = RenameRequest::new(tickets.issue(caller.revision()), 10, "hail");
    let _ = client.rename(&caller.snapshot(), &request);
    open_as(&mut client, &document(CALLEE), "file:///b.rs");
    assert_eq!(
        stale(client.receive(reply(2, json!({"documentChanges": [
            rename_in("file:///a.rs", json!(1)),
            rename_in("file:///b.rs", json!(null)),
        ]})))),
        "file:///b.rs",
        "the late-opened callee is stale",
    );
}

/// A document closed since the request lost the text the server edited.
#[test]
fn rename_rejects_a_document_closed_since_the_request() {
    let mut tickets = Counter::new();
    let (mut client, _, callee) = renaming(&mut tickets);
    let _ = client.close(callee.doc_id());
    assert_eq!(
        stale(client.receive(reply(2, json!({"documentChanges": [
            rename_in("file:///a.rs", json!(1)),
            rename_in("file:///b.rs", json!(null)),
        ]})))),
        "file:///b.rs",
        "the closed callee is stale",
    );
}

/// An edit naming a version other than the one last sent was computed for other text.
#[test]
fn rename_rejects_a_version_the_server_did_not_see() {
    let mut tickets = Counter::new();
    let (mut client, _, _) = renaming(&mut tickets);
    assert_eq!(
        stale(client.receive(reply(2, json!({"documentChanges": [
            rename_in("file:///a.rs", json!(1)),
            rename_in("file:///b.rs", json!(8)),
        ]})))),
        "file:///b.rs",
        "the callee's named version is stale",
    );
}

/// A file operation refuses the whole rename.
#[test]
fn rename_rejects_resource_operations() {
    let mut tickets = Counter::new();
    let (mut client, _, _) = renaming(&mut tickets);
    let result = client.receive(reply(2, json!({"documentChanges": [
        rename_in("file:///a.rs", json!(1)),
        {"kind": "create", "uri": "file:///w/n.rs"},
    ]})));
    assert!(
        matches!(&result, Err(Error::Unsupported { operation }) if operation == "create"),
        "a create is unsupported: {result:?}",
    );
}

/// Edits for a file nobody opened come back for the host to apply on disk.
#[test]
fn rename_of_an_unopened_document_yields_file_edits() {
    let mut tickets = Counter::new();
    let (mut client, caller, _) = renaming(&mut tickets);
    let output = client
        .receive(reply(2, json!({"documentChanges": [
            rename_in("file:///a.rs", json!(1)),
            rename_in("file:///w/c.rs", json!(null)),
        ]})))
        .expect("the rename is accepted");
    let [Update::Document(document), Update::FileEdits(file_edits)] = output.updates.as_slice()
    else {
        panic!("expected a document and a file, got {:?}", output.updates)
    };
    assert_eq!(document.doc_id(), caller.doc_id(), "the caller is renamed");
    assert_eq!(file_edits.uri().as_str(), "file:///w/c.rs", "the unopened file");
    assert_eq!(
        file_edits.apply(CALLEE),
        "fn hail() {}\n",
        "the file's edits apply to its text"
    );
}

/// A rename whose requesting document moved is dropped; the user can ask again.
#[test]
fn rename_whose_requester_moved_is_dropped_silently() {
    let mut tickets = Counter::new();
    let (mut client, mut caller, _) = renaming(&mut tickets);
    type_ops(&mut client, &mut caller, vec![EditOp::insert(0, "\n")]);
    assert_silent(
        &client
            .receive(reply(2, json!({"documentChanges": [rename_in("file:///a.rs", json!(1))]})))
            .expect("a stale reply is not an error"),
        "the stale rename is dropped",
    );
}

/// A `null` result renames nothing.
#[test]
fn null_rename_result_changes_nothing() {
    let mut tickets = Counter::new();
    let (mut client, _, _) = renaming(&mut tickets);
    assert_silent(
        &client
            .receive(reply(2, json!(null)))
            .expect("null is accepted"),
        "nothing to rename",
    );
}

/// A user asked for the rename, so a failed or malformed reply is reported.
#[test]
fn rename_failures_are_errors() {
    let mut tickets = Counter::new();
    let (mut client, caller, _) = renaming(&mut tickets);
    let error = client
        .receive(failure(2, -32603))
        .expect_err("a server error is reported");
    assert!(
        matches!(&error, Error::Server { method, .. } if method == "textDocument/rename"),
        "the error names the method: {error:?}",
    );
    let request = RenameRequest::new(tickets.issue(caller.revision()), 10, "hail");
    let _ = client.rename(&caller.snapshot(), &request);
    let error = client
        .receive(reply(3, json!({"documentChanges": "x"})))
        .expect_err("a malformed edit is reported");
    assert!(
        matches!(&error, Error::Decode { method, .. } if method == "textDocument/rename"),
        "the error names the method: {error:?}",
    );
}

/// With no server to ask, or one without a provider, a rename sends nothing and answers nothing.
#[test]
fn rename_declines_before_initialize_and_without_a_provider() {
    let doc = document(CALLER);
    let (initializing, _) = Builder::default().session(None);
    let (without, _) = running(Builder::default(), json!({"textDocumentSync": 2}));
    for mut client in [initializing, without] {
        let mut tickets = Counter::new();
        open_as(&mut client, &doc, "file:///a.rs");
        let request = RenameRequest::new(tickets.issue(doc.revision()), 10, "hail");
        assert_silent(
            &client.rename(&doc.snapshot(), &request),
            "a declined rename sends nothing",
        );
    }
}

/// Server capabilities with utf-16 positions (the default), incremental sync, hover, and inlay
/// hints whose tooltips resolve lazily.
fn inlay_capabilities() -> Value {
    json!({
        "textDocumentSync": 2,
        "hoverProvider": true,
        "inlayHintProvider": {"resolveProvider": true},
    })
}

/// `let a = f(1);` / `let b = a;`: `a` ends at 5, `1` starts at 10, `b` ends at 19; 25 bytes, 3
/// lines.
const INLAY_TEXT: &str = "let a = f(1);\nlet b = a;\n";

/// `: i32` after `a`; its `i32` part links into the unopened `core.rs`.
fn type_hint() -> Value {
    json!({"position": {"line": 0, "character": 5}, "kind": 1, "label": [
        {"value": ": "},
        {"value": "i32", "location": {"uri": "file:///w/core.rs", "range": span_on(3, 4, 7)}},
    ], "paddingLeft": false, "paddingRight": false, "data": {"id": 1}})
}

/// `x:` before `1`, padded on the right.
fn parameter_hint() -> Value {
    json!({"position": {"line": 0, "character": 10}, "kind": 2, "label": "x:",
        "paddingLeft": false, "paddingRight": true, "data": {"id": 2}})
}

/// `: i32` after `b`, insertable: the edit rewrites `b` as `b: i32`, which hygiene trims to an
/// insert at 19.
fn insertable_hint() -> Value {
    json!({"position": {"line": 1, "character": 5}, "kind": 1, "label": ": i32",
        "textEdits": [text_edit((1, 4), (1, 5), "b: i32")], "data": {"id": 3}})
}

/// A hint at `(line, character)` with a string `label` and optional `kind`.
fn plain_hint(line: u32, character: u32, label: &str, kind: Option<i32>) -> Value {
    let mut hint = json!({"position": {"line": line, "character": character}, "label": label});
    if let Some(kind) = kind {
        hint["kind"] = json!(kind);
    }
    hint
}

/// A running client with `text` open as `file:///a.rs`.
fn hinting(text: &str, capabilities: Value) -> (Session, Document) {
    let doc = document(text);
    let (mut client, _) = running(Builder::default(), capabilities);
    open_as(&mut client, &doc, "file:///a.rs");
    (client, doc)
}

/// An inlay request over the whole document at its revision.
fn inlay_request(tickets: &mut Counter, doc: &Document) -> inlay::Request {
    inlay::Request::new(tickets.issue(doc.revision()), 0..doc.snapshot().len())
}

/// The fetch request with `id` for `file:///a.rs` over `start..end` (line, character).
fn inlay_wire(id: i64, start: (u32, u32), end: (u32, u32)) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "textDocument/inlayHint", "params": {
        "textDocument": {"uri": "file:///a.rs"},
        "range": {
            "start": {"line": start.0, "character": start.1},
            "end": {"line": end.0, "character": end.1},
        },
    }})
}

/// Requests the hints of `doc`, answers request `id` with `hints`, and returns the installed
/// hints.
fn fetch(
    client: &mut Session,
    tickets: &mut Counter,
    doc: &Document,
    id: i64,
    hints: Value,
) -> Vec<inlay::Placed> {
    let _ = client.inlays(&doc.snapshot(), &inlay_request(tickets, doc));
    let output = client.receive(reply(id, hints)).expect("the reply is accepted");
    inlays(only(&output)).1.expect("an answer, not a failure")
}

/// The stamp and hints of an inlay update.
fn inlays(update: &Update) -> (update::Stamp, Option<Vec<inlay::Placed>>) {
    let Update::Document(document) = update else {
        panic!("expected a document update, got {update:?}")
    };
    match document.change() {
        update::Change::Inlays(hints) => (document.stamp(), hints.clone()),
        other => panic!("expected inlay hints, got {other:?}"),
    }
}

/// The one update in `output`, which sends nothing.
fn only(output: &Output) -> &Update {
    assert!(
        output.messages.is_empty(),
        "an answer sends nothing: {output:?}"
    );
    let [update] = output.updates.as_slice() else {
        panic!("expected one update, got {:?}", output.updates)
    };
    update
}

/// The offsets of `hints`.
fn offsets(hints: &[inlay::Placed]) -> Vec<u32> {
    hints.iter().map(inlay::Placed::offset).collect()
}

/// The label of each hint, its parts' texts joined.
fn label_texts(hints: &[inlay::Placed]) -> Vec<String> {
    hints
        .iter()
        .map(|placed| placed.hint().parts().iter().map(inlay::Part::text).collect())
        .collect()
}

/// A one-part hint without a link, as the client should have built it.
fn expected_hint(
    kind: inlay::Kind,
    label: &str,
    key: inlay::Key,
    padding: (bool, bool),
    placement: inlay::Placement,
) -> inlay::Hint {
    inlay::Hint::new(kind, vec![inlay::Part::new(label, inlay::Link::None)], key)
        .expect("a visible label")
        .padding(inlay::Padding {
            left: padding.0,
            right: padding.1,
        })
        .placement(placement)
}

/// `initialize` advertises inlay hints that resolve only their tooltips, and refreshes.
#[test]
fn initialize_advertises_inlay_hints_with_lazy_tooltips_and_refresh() {
    let (_, initialize) = Builder::default().session(None);
    let initialize = serde_json::to_value(&initialize).expect("serializes");
    assert_eq!(
        initialize.pointer("/params/capabilities/textDocument/inlayHint"),
        Some(&json!({
            "dynamicRegistration": false,
            "resolveSupport": {"properties": ["tooltip", "label.tooltip"]},
        })),
        "locations and edits come inline; only tooltips resolve",
    );
    assert_eq!(
        initialize.pointer("/params/capabilities/workspace/inlayHint"),
        Some(&json!({"refreshSupport": true})),
        "the server may ask for a refetch",
    );
}

/// Every shape of `inlayHintProvider` is read, with whether the server resolves.
#[test]
fn inlay_provider_is_read_from_every_shape() {
    let cases = [
        (None, None),
        (Some(json!(false)), None),
        (Some(json!(true)), Some(capabilities::Resolve::Unsupported)),
        (Some(json!({})), Some(capabilities::Resolve::Unsupported)),
        (
            Some(json!({"resolveProvider": false})),
            Some(capabilities::Resolve::Unsupported),
        ),
        (
            Some(json!({"resolveProvider": true})),
            Some(capabilities::Resolve::Supported),
        ),
        (
            Some(json!({"resolveProvider": true, "documentSelector": null, "id": "inlays"})),
            Some(capabilities::Resolve::Supported),
        ),
    ];
    for (provider, expected) in cases {
        let mut capabilities = json!({"textDocumentSync": 2});
        if let Some(provider) = &provider {
            capabilities["inlayHintProvider"] = provider.clone();
        }
        let (client, _) = running(Builder::default(), capabilities);
        assert!(
            matches!(&client.state, State::Running(server) if server.inlay == expected),
            "{provider:?} reads as {expected:?}",
        );
    }
}

/// A fetch asks for the request's span as an LSP range.
#[test]
fn inlay_request_carries_the_span_as_a_range() {
    let mut tickets = Counter::new();
    let (mut client, doc) = hinting(INLAY_TEXT, inlay_capabilities());
    let output = client.inlays(&doc.snapshot(), &inlay_request(&mut tickets, &doc));
    assert_eq!(
        wire(&output.messages),
        vec![inlay_wire(2, (0, 0), (2, 0))],
        "the whole document, up to the start of its last, empty line",
    );
    assert!(output.updates.is_empty(), "nothing is answered yet");
}

/// A span past the end of the document is clamped to it: rust-analyzer fails a range that ends
/// past the last line.
#[test]
fn inlay_request_past_the_last_line_ends_at_the_buffer_end() {
    let mut tickets = Counter::new();
    let (mut client, doc) = hinting("let a = 1;", inlay_capabilities());
    let request = inlay::Request::new(tickets.issue(doc.revision()), 4..1000);
    assert_eq!(
        wire(&client.inlays(&doc.snapshot(), &request).messages),
        vec![inlay_wire(2, (0, 4), (0, 10))],
        "the range ends at the last character",
    );
}

/// With no server to ask, or one without a provider, the editor's hints clear at once.
#[test]
fn inlays_decline_before_initialize_and_without_a_provider() {
    let doc = document(INLAY_TEXT);
    let (initializing, _) = Builder::default().session(None);
    let (without, _) = running(Builder::default(), json!({"textDocumentSync": 2}));
    let (disabled, _) = running(
        Builder::default(),
        json!({"textDocumentSync": 2, "inlayHintProvider": false}),
    );
    for mut client in [initializing, without, disabled] {
        let mut tickets = Counter::new();
        open_as(&mut client, &doc, "file:///a.rs");
        let request = inlay_request(&mut tickets, &doc);
        let (stamp, hints) = inlays(only(&client.inlays(&doc.snapshot(), &request)));
        assert_eq!(
            stamp,
            update::Stamp::Ticket(request.ticket()),
            "the decline answers the ticket"
        );
        assert_eq!(hints.map(|h| h.len()), Some(0), "an empty set, not a failure");
    }
}

/// A request from a revision the client has not synced, or that moved on, gets nothing.
#[test]
fn stale_inlay_request_is_ignored() {
    let mut tickets = Counter::new();
    let (mut client, mut doc) = hinting(INLAY_TEXT, inlay_capabilities());
    let request = inlay_request(&mut tickets, &doc);
    type_ops(&mut client, &mut doc, vec![EditOp::insert(0, "x")]);
    assert_silent(
        &client.inlays(&doc.snapshot(), &request),
        "a request from an old revision",
    );
}

/// A second fetch cancels the one in flight.
#[test]
fn new_inlay_request_supersedes_the_previous_one() {
    let mut tickets = Counter::new();
    let (mut client, doc) = hinting(INLAY_TEXT, inlay_capabilities());
    let _ = client.inlays(&doc.snapshot(), &inlay_request(&mut tickets, &doc));
    let output = client.inlays(&doc.snapshot(), &inlay_request(&mut tickets, &doc));
    assert_eq!(
        wire(&output.messages),
        vec![cancel_request(2), inlay_wire(3, (0, 0), (2, 0))],
        "the old request is cancelled before the new one goes out",
    );
}

/// The reply answers the ticket with hints converted to scrive's model.
#[test]
fn inlay_reply_answers_the_ticket_with_converted_hints() {
    let mut tickets = Counter::new();
    let (mut client, doc) = hinting(INLAY_TEXT, inlay_capabilities());
    let request = inlay_request(&mut tickets, &doc);
    let _ = client.inlays(&doc.snapshot(), &request);
    let output = client
        .receive(reply(
            2,
            json!([type_hint(), parameter_hint(), insertable_hint()]),
        ))
        .expect("the reply is accepted");
    let (stamp, hints) = inlays(only(&output));
    assert_eq!(
        stamp,
        update::Stamp::Ticket(request.ticket()),
        "stamped with the request's ticket"
    );
    let hints = hints.expect("an answer");
    assert_eq!(offsets(&hints), vec![5, 10, 19], "offsets in the request snapshot");
    let key = |i: usize| hints[i].hint().key();
    let typed = inlay::Hint::new(
        inlay::Kind::Type,
        vec![
            inlay::Part::new(": ", inlay::Link::None),
            inlay::Part::new("i32", inlay::Link::Jumps),
        ],
        key(0),
    )
    .expect("a visible label");
    assert_eq!(hints[0].hint(), &typed, "a type suffix whose located part jumps");
    assert_eq!(
        hints[1].hint(),
        &expected_hint(
            inlay::Kind::Parameter,
            "x:",
            key(1),
            (false, true),
            inlay::Placement::Prefix
        ),
        "a parameter prefix padded on the right",
    );
    assert_eq!(
        hints[2].hint(),
        &expected_hint(
            inlay::Kind::Type,
            ": i32",
            key(2),
            (false, false),
            inlay::Placement::Suffix
        )
        .insert(inlay::Insert::Available),
        "a hint with text edits is insertable",
    );
}

/// utf-16 positions convert against the request snapshot; one inside a surrogate pair snaps to
/// the character's start.
#[test]
fn inlay_positions_convert_from_utf16_against_the_request_snapshot() {
    let mut tickets = Counter::new();
    let (mut client, doc) = hinting("let 😀 = f(a);\n", inlay_capabilities());
    let hints = fetch(
        &mut client,
        &mut tickets,
        &doc,
        2,
        json!([
            plain_hint(0, 6, "a", None),
            plain_hint(0, 11, "b", None),
            plain_hint(0, 5, "c", None),
        ]),
    );
    assert_eq!(offsets(&hints), vec![8, 13, 4], "bytes, in server order");
}

/// A hint on a line the document does not have is dropped, not clamped to the end.
#[test]
fn inlay_hints_on_lines_past_the_end_are_dropped() {
    let mut tickets = Counter::new();
    let (mut client, doc) = hinting("a\n", inlay_capabilities());
    let hints = fetch(
        &mut client,
        &mut tickets,
        &doc,
        2,
        json!([plain_hint(1, 0, "x", None), plain_hint(2, 0, "y", None)]),
    );
    assert_eq!(offsets(&hints), vec![2], "only the hint on the last, empty line");
}

/// Hints more than one line outside the requested span are dropped.
#[test]
fn inlay_hints_outside_the_request_span_are_clipped() {
    let mut tickets = Counter::new();
    let (mut client, doc) = hinting("a\nb\nc\nd\ne\n", inlay_capabilities());
    let request = inlay::Request::new(tickets.issue(doc.revision()), 4..5);
    assert_eq!(
        wire(&client.inlays(&doc.snapshot(), &request).messages),
        vec![inlay_wire(2, (2, 0), (2, 1))],
        "the span is line 2",
    );
    let hints = (0..5)
        .map(|line| plain_hint(line, 0, &line.to_string(), None))
        .collect::<Vec<_>>();
    let output = client
        .receive(reply(2, Value::Array(hints)))
        .expect("the reply is accepted");
    let hints = inlays(only(&output)).1.expect("an answer");
    assert_eq!(label_texts(&hints), vec!["1", "2", "3"], "lines 1 to 3 survive");
}

/// The client neither sorts nor groups: hints keep the server's order, ties included.
#[test]
fn inlay_answer_keeps_the_server_order_including_ties() {
    let mut tickets = Counter::new();
    let (mut client, doc) = hinting(INLAY_TEXT, inlay_capabilities());
    let hints = fetch(
        &mut client,
        &mut tickets,
        &doc,
        2,
        json!([
            parameter_hint(),
            plain_hint(0, 5, ": a", Some(1)),
            plain_hint(0, 5, ": b", Some(1)),
            plain_hint(0, 5, ": c", Some(1)),
        ]),
    );
    assert_eq!(offsets(&hints), vec![10, 5, 5, 5], "server order");
    assert_eq!(
        label_texts(&hints),
        vec!["x:", ": a", ": b", ": c"],
        "ties keep their order"
    );
}

/// Control characters become spaces in core, and a label with no visible text drops its hint.
#[test]
fn inlay_labels_are_sanitised_by_core_and_empty_labels_dropped() {
    let mut tickets = Counter::new();
    let (mut client, doc) = hinting(INLAY_TEXT, inlay_capabilities());
    let at = |label: Value| json!({"position": {"line": 0, "character": 5}, "label": label});
    let hints = fetch(
        &mut client,
        &mut tickets,
        &doc,
        2,
        json!([
            at(json!("a\tb")),
            at(json!("")),
            at(json!([])),
            at(json!([{"value": "c"}])),
        ]),
    );
    assert_eq!(label_texts(&hints), vec!["a b", "c"], "two hints survive");
}

/// A hint of no known kind takes its side from the server's padding: padding faces away from
/// what it annotates, and symmetric padding leaves the side to the text.
#[test]
fn other_inlay_hints_take_their_placement_from_padding() {
    let mut tickets = Counter::new();
    let (mut client, doc) = hinting(INLAY_TEXT, inlay_capabilities());
    let cases = [
        (None, Some((false, true)), inlay::Placement::Prefix),
        (None, Some((true, false)), inlay::Placement::Suffix),
        (None, Some((false, false)), inlay::Placement::Auto),
        (None, Some((true, true)), inlay::Placement::Auto),
        (None, None, inlay::Placement::Auto),
        (Some(3), Some((false, true)), inlay::Placement::Prefix),
    ];
    let reply_hints = cases
        .iter()
        .map(|&(kind, padding, _)| {
            let mut hint = plain_hint(0, 5, "y", kind);
            if let Some((left, right)) = padding {
                hint["paddingLeft"] = json!(left);
                hint["paddingRight"] = json!(right);
            }
            hint
        })
        .collect::<Vec<_>>();
    let hints = fetch(&mut client, &mut tickets, &doc, 2, Value::Array(reply_hints));
    assert_eq!(hints.len(), cases.len(), "every hint converts");
    for (placed, (kind, padding, placement)) in hints.iter().zip(cases) {
        let padding = padding.unwrap_or_default();
        assert_eq!(
            placed.hint(),
            &expected_hint(
                inlay::Kind::Other,
                "y",
                placed.hint().key(),
                padding,
                placement
            ),
            "kind {kind:?} with padding {padding:?}",
        );
    }
}

/// A padding flag is dropped where the label already carries the space; the side still comes
/// from the server's flags.
#[test]
fn inlay_padding_collapses_when_the_label_has_the_space() {
    let mut tickets = Counter::new();
    let (mut client, doc) = hinting(INLAY_TEXT, inlay_capabilities());
    let cases = [
        (Some(1), (false, false), json!(": i32"), (false, false), inlay::Placement::Suffix),
        (Some(1), (true, false), json!(": i32"), (true, false), inlay::Placement::Suffix),
        (Some(2), (false, true), json!("x:"), (false, true), inlay::Placement::Prefix),
        (Some(2), (false, true), json!("x: "), (false, false), inlay::Placement::Prefix),
        (None, (false, true), json!("<'_>"), (false, true), inlay::Placement::Prefix),
        (None, (true, false), json!(" = usize"), (false, false), inlay::Placement::Suffix),
        (None, (false, false), json!("&*"), (false, false), inlay::Placement::Auto),
        (
            None,
            (true, true),
            json!([{"value": " a"}, {"value": "b "}]),
            (false, false),
            inlay::Placement::Auto,
        ),
    ];
    let reply_hints = cases
        .iter()
        .map(|(kind, (left, right), label, ..)| {
            let mut hint = json!({"position": {"line": 0, "character": 5}, "label": label,
                "paddingLeft": left, "paddingRight": right});
            if let Some(kind) = kind {
                hint["kind"] = json!(kind);
            }
            hint
        })
        .collect::<Vec<_>>();
    let hints = fetch(&mut client, &mut tickets, &doc, 2, Value::Array(reply_hints));
    assert_eq!(hints.len(), cases.len(), "every hint converts");
    for (placed, (kind, raw, label, padding, placement)) in hints.iter().zip(cases) {
        let parts = match label {
            Value::String(text) => vec![inlay::Part::new(text, inlay::Link::None)],
            Value::Array(parts) => parts
                .iter()
                .map(|part| {
                    inlay::Part::new(part["value"].as_str().unwrap_or_default(), inlay::Link::None)
                })
                .collect(),
            other => panic!("unexpected label {other}"),
        };
        let kind = match kind {
            Some(1) => inlay::Kind::Type,
            Some(2) => inlay::Kind::Parameter,
            _ => inlay::Kind::Other,
        };
        let expected = inlay::Hint::new(kind, parts, placed.hint().key())
            .expect("a visible label")
            .padding(inlay::Padding {
                left: padding.0,
                right: padding.1,
            })
            .placement(placement);
        assert_eq!(placed.hint(), &expected, "{kind:?} padded {raw:?}");
    }
}

/// One malformed entry is skipped alone; the rest of the answer lands.
#[test]
fn one_malformed_inlay_hint_does_not_lose_the_set() {
    let mut tickets = Counter::new();
    let (mut client, doc) = hinting(INLAY_TEXT, inlay_capabilities());
    let hints = fetch(
        &mut client,
        &mut tickets,
        &doc,
        2,
        json!([
            {"position": {"line": 0, "character": 5}, "label": ": i32", "kind": 1},
            {"position": "nowhere", "label": "x"},
            {"position": {"line": 0, "character": 10}, "label": [
                {"value": "T",
                    "location": {"uri": "file:///bad path.rs", "range": span_on(0, 0, 1)}},
            ]},
            {"label": "no position"},
            7,
            {"position": {"line": 1, "character": 5}, "label": ": i32", "kind": 1},
        ]),
    );
    assert_eq!(offsets(&hints), vec![5, 19], "the two well-formed hints");
}

/// `null` is an empty answer, which clears the editor's hints.
#[test]
fn null_inlay_result_clears_the_hints() {
    let mut tickets = Counter::new();
    let (mut client, doc) = hinting(INLAY_TEXT, inlay_capabilities());
    let hints = fetch(&mut client, &mut tickets, &doc, 2, Value::Null);
    assert!(hints.is_empty(), "an empty set");
}

/// A failed fetch, or a result that is not an array, settles the ticket with `None`, so the
/// editor keeps its hints.
#[test]
fn failed_or_undecodable_inlay_result_answers_none() {
    let mut tickets = Counter::new();
    let (mut client, doc) = hinting(INLAY_TEXT, inlay_capabilities());
    for (id, message) in [(2, failure(2, -32603)), (3, reply(3, json!("x")))] {
        let request = inlay_request(&mut tickets, &doc);
        let _ = client.inlays(&doc.snapshot(), &request);
        let output = client.receive(message).expect("an intel failure is no error");
        let (stamp, hints) = inlays(only(&output));
        assert_eq!(
            stamp,
            update::Stamp::Ticket(request.ticket()),
            "request {id} settles its ticket"
        );
        assert!(hints.is_none(), "request {id} failed");
    }
}

/// ContentModified, which ends rust-analyzer's indexing, re-sends the fetch once.
#[test]
fn content_modified_reissues_the_inlay_request_once() {
    let mut tickets = Counter::new();
    let (mut client, doc) = hinting(INLAY_TEXT, inlay_capabilities());
    let _ = client.inlays(&doc.snapshot(), &inlay_request(&mut tickets, &doc));
    let reissued = client
        .receive(failure(2, -32801))
        .expect("content modified is not an error");
    assert_eq!(
        wire(&reissued.messages),
        vec![inlay_wire(3, (0, 0), (2, 0))],
        "the same span goes out again",
    );
    assert!(reissued.updates.is_empty(), "nothing is answered yet");
    assert_silent(
        &client
            .receive(failure(3, -32801))
            .expect("content modified is not an error"),
        "a second content-modified reply for one ticket is final",
    );
}

/// A refetch at the same revision keeps the keys of unchanged hints, matched in order, and mints
/// new ones for the rest.
#[test]
fn inlay_keys_are_stable_across_a_refetch_at_the_same_revision() {
    let mut tickets = Counter::new();
    let (mut client, doc) = hinting(INLAY_TEXT, inlay_capabilities());
    let a = || plain_hint(0, 5, ": i32", Some(1));
    let c = || plain_hint(1, 5, ": i32", Some(1));
    let d = || plain_hint(0, 5, ": i32", Some(2));
    let keys = |hints: &[inlay::Placed]| hints.iter().map(|p| p.hint().key()).collect::<Vec<_>>();
    let first = keys(&fetch(&mut client, &mut tickets, &doc, 2, json!([a(), a(), c()])));
    let second = keys(&fetch(&mut client, &mut tickets, &doc, 3, json!([c(), a(), a(), d()])));
    assert_eq!(
        second[..3],
        [first[2], first[0], first[1]],
        "unchanged hints keep their keys, repeated ones in order",
    );
    assert!(!first.contains(&second[3]), "a different kind is a new hint");
}

/// After the text moves, every hint gets a new key.
#[test]
fn inlay_keys_are_fresh_after_the_revision_moves() {
    let mut tickets = Counter::new();
    let (mut client, mut doc) = hinting(INLAY_TEXT, inlay_capabilities());
    let a = || json!([plain_hint(0, 5, ": i32", Some(1))]);
    let first = fetch(&mut client, &mut tickets, &doc, 2, a());
    type_ops(&mut client, &mut doc, vec![EditOp::insert(25, "x")]);
    let second = fetch(&mut client, &mut tickets, &doc, 3, a());
    assert_ne!(
        first[0].hint().key(),
        second[0].hint().key(),
        "keys match only within one revision"
    );
}

/// The document and stamp of an inlay refresh.
fn refreshed(update: &Update) -> (DocId, update::Stamp) {
    let Update::Document(document) = update else {
        panic!("expected a document update, got {update:?}")
    };
    match document.change() {
        update::Change::InlayRefresh => (document.doc_id(), document.stamp()),
        other => panic!("expected an inlay refresh, got {other:?}"),
    }
}

/// `workspace/inlayHint/refresh` with id 7.
fn inlay_refresh() -> Message {
    from_server(json!({"jsonrpc": "2.0", "id": 7, "method": "workspace/inlayHint/refresh"}))
}

/// A refresh is answered `null` and tells every open document, at its synced revision, to
/// refetch.
#[test]
fn inlay_refresh_answers_null_and_reaches_every_open_document() {
    let (mut client, a) = hinting(INLAY_TEXT, inlay_capabilities());
    let mut b = document(CALLEE);
    open_as(&mut client, &b, "file:///b.rs");
    type_ops(&mut client, &mut b, vec![EditOp::insert(0, "\n")]);
    let output = client.receive(inlay_refresh()).expect("server requests are answered");
    assert_eq!(
        wire(&output.messages),
        vec![json!({"jsonrpc": "2.0", "id": 7, "result": null})],
        "the refresh is acknowledged",
    );
    assert_eq!(
        output.updates.iter().map(refreshed).collect::<Vec<_>>(),
        vec![
            (a.doc_id(), update::Stamp::Revision(a.revision())),
            (b.doc_id(), update::Stamp::Revision(b.revision())),
        ],
        "one refresh per open document, in registration order",
    );
}

/// Other refreshes are acknowledged and reach no document.
#[test]
fn other_refreshes_answer_null_without_updates() {
    let (mut client, _doc) = hinting(INLAY_TEXT, inlay_capabilities());
    let response = answer(&mut client, "workspace/semanticTokens/refresh", json!(null));
    assert_eq!(response.get("result"), Some(&Value::Null), "acknowledged");
}

/// Before the handshake a refresh is only acknowledged: `initialized` re-arms the editors.
#[test]
fn inlay_refresh_before_initialize_is_only_acknowledged() {
    let doc = document(INLAY_TEXT);
    let (mut client, _) = Builder::default().session(None);
    open_as(&mut client, &doc, "file:///a.rs");
    let output = client.receive(inlay_refresh()).expect("server requests are answered");
    assert_eq!(output.messages.len(), 1, "the refresh is acknowledged");
    assert!(output.updates.is_empty(), "no document refetches yet");
}

/// Requests made before the handshake were declined, so `initialized` asks every open document
/// to refetch, even from a server that never sends refreshes.
#[test]
fn initialized_refreshes_inlays_for_a_server_without_refresh_support() {
    let doc = document(INLAY_TEXT);
    let (mut client, _) = Builder::default().session(None);
    open_as(&mut client, &doc, "file:///a.rs");
    let output = client
        .receive(from_server(json!({"jsonrpc": "2.0", "id": 1, "result": {"capabilities":
            {"textDocumentSync": 2, "inlayHintProvider": true}}})))
        .expect("initialize answer is accepted");
    assert_eq!(
        output.updates.iter().map(refreshed).collect::<Vec<_>>(),
        vec![(doc.doc_id(), update::Stamp::Revision(doc.revision()))],
        "the open document refetches",
    );
}

/// Without an inlay provider the handshake asks for no refetch.
#[test]
fn initialized_sends_no_inlay_refresh_without_a_provider() {
    let doc = document(INLAY_TEXT);
    let (mut client, _) = Builder::default().session(None);
    open_as(&mut client, &doc, "file:///a.rs");
    let output = client
        .receive(from_server(json!({"jsonrpc": "2.0", "id": 1, "result": {"capabilities":
            {"textDocumentSync": 2}}})))
        .expect("initialize answer is accepted");
    assert!(output.updates.is_empty(), "nothing to refetch");
}

/// The stamp and markdown of an inlay tooltip update.
fn tooltip(update: &Update) -> (update::Stamp, Option<String>) {
    let Update::Document(document) = update else {
        panic!("expected a document update, got {update:?}")
    };
    match document.change() {
        update::Change::InlayTooltip(markdown) => (document.stamp(), markdown.clone()),
        other => panic!("expected an inlay tooltip, got {other:?}"),
    }
}

/// A running client with [`INLAY_TEXT`] open, its three fixture hints fetched by request 2.
fn hinted(capabilities: Value) -> (Session, Document, Counter, Vec<inlay::Placed>) {
    let mut tickets = Counter::new();
    let (mut client, doc) = hinting(INLAY_TEXT, capabilities);
    let hints = fetch(
        &mut client,
        &mut tickets,
        &doc,
        2,
        json!([type_hint(), parameter_hint(), insertable_hint()]),
    );
    (client, doc, tickets, hints)
}

/// Capabilities without lazy tooltips: hover and inlay hints, nothing to resolve.
fn resolveless_capabilities() -> Value {
    json!({"textDocumentSync": 2, "hoverProvider": true, "inlayHintProvider": true})
}

/// `inlayHint/resolve` with `id` for the raw `hint`.
fn resolve_wire(id: i64, hint: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "inlayHint/resolve", "params": hint})
}

/// The hover with `id` at `type_hint`'s `i32` location in `core.rs`.
fn location_hover_wire(id: i64) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "textDocument/hover", "params": {
        "textDocument": {"uri": "file:///w/core.rs"},
        "position": {"line": 3, "character": 4},
    }})
}

/// The hover reply for `i32`.
fn i32_hover(id: i64) -> Message {
    reply(
        id,
        json!({"contents": {"kind": "markdown", "value": "```rust\nstruct i32\n```"}}),
    )
}

/// A one-part type hint after `a` whose part links to `range` in `uri`.
fn linked_hint(uri: &str, range: Value) -> Value {
    json!({"position": {"line": 0, "character": 5}, "kind": 1, "label": [
        {"value": "T", "location": {"uri": uri, "range": range}},
    ]})
}

/// A tooltip resolves the hint once; the resolved tooltip answers, and later hovers are
/// answered from the stored hint without asking again.
#[test]
fn inlay_tooltip_resolves_then_answers_from_the_stored_hint() {
    let (mut client, doc, mut tickets, hints) = hinted(inlay_capabilities());
    let key = hints[0].hint().key();
    let gesture = inlay::Interaction::tooltip(tickets.issue(doc.revision()), key, 0);
    assert_eq!(
        wire(&client.interact(&doc.snapshot(), &gesture).messages),
        vec![resolve_wire(3, type_hint())],
        "the hint goes back verbatim",
    );
    let mut resolved = type_hint();
    resolved["tooltip"] = json!({"kind": "markdown", "value": "**i32** is 32 bits"});
    let output = client.receive(reply(3, resolved)).expect("the reply is accepted");
    let (stamp, markdown) = tooltip(only(&output));
    assert_eq!(
        stamp,
        update::Stamp::Ticket(gesture.ticket()),
        "the tooltip answers the gesture"
    );
    assert_eq!(
        markdown.as_deref(),
        Some("**i32** is 32 bits"),
        "the resolved tooltip, in the card's subset"
    );
    let again = inlay::Interaction::tooltip(tickets.issue(doc.revision()), key, 0);
    let output = client.interact(&doc.snapshot(), &again);
    assert!(output.messages.is_empty(), "a resolved hint is not resolved again");
    assert_eq!(
        tooltip(only(&output)).1.as_deref(),
        Some("**i32** is 32 bits"),
        "answered from the stored hint"
    );
}

/// A resolve over the same label adds its part tooltips, and the part's own tooltip answers.
#[test]
fn resolve_with_the_same_label_adds_part_tooltips() {
    let (mut client, doc, mut tickets, hints) = hinted(inlay_capabilities());
    let gesture =
        inlay::Interaction::tooltip(tickets.issue(doc.revision()), hints[0].hint().key(), 1);
    let _ = client.interact(&doc.snapshot(), &gesture);
    let mut resolved = type_hint();
    resolved["label"][1]["tooltip"] = json!("the type");
    let output = client.receive(reply(3, resolved)).expect("the reply is accepted");
    assert_eq!(
        tooltip(only(&output)).1.as_deref(),
        Some("the type"),
        "the part's own tooltip"
    );
}

/// A resolve that reshapes the label contributes only its hint-level tooltip: the parts, their
/// links included, stay as fetched.
#[test]
fn resolve_that_restructures_the_label_keeps_the_fetched_parts() {
    let (mut client, doc, mut tickets, hints) = hinted(inlay_capabilities());
    let key = hints[0].hint().key();
    let gesture = inlay::Interaction::tooltip(tickets.issue(doc.revision()), key, 1);
    let _ = client.interact(&doc.snapshot(), &gesture);
    let mut resolved = type_hint();
    resolved["label"] =
        json!([{"value": ": "}, {"value": "i"}, {"value": "32", "tooltip": "part"}]);
    resolved["tooltip"] = json!("whole");
    let output = client.receive(reply(3, resolved)).expect("the reply is accepted");
    assert_eq!(
        tooltip(only(&output)).1.as_deref(),
        Some("whole"),
        "the hint's tooltip, not the reshaped part's"
    );
    let jump = inlay::Interaction::jump(tickets.issue(doc.revision()), key, 1);
    let (_, target) = defined(&client.interact(&doc.snapshot(), &jump));
    let Some(update::Target::Unopened(unopened)) = target else {
        panic!("expected an unopened target, got {target:?}")
    };
    assert_eq!(unopened.uri().as_str(), "file:///w/core.rs", "the fetched part's link");
}

/// A resolve that answers after an edit is dropped, and the moved set answers nothing.
#[test]
fn resolve_reply_after_an_edit_is_dropped() {
    let (mut client, mut doc, mut tickets, hints) = hinted(inlay_capabilities());
    let key = hints[0].hint().key();
    let gesture = inlay::Interaction::tooltip(tickets.issue(doc.revision()), key, 0);
    let _ = client.interact(&doc.snapshot(), &gesture);
    type_ops(&mut client, &mut doc, vec![EditOp::insert(25, "x")]);
    let mut resolved = type_hint();
    resolved["tooltip"] = json!("late");
    assert_silent(
        &client.receive(reply(3, resolved)).expect("the reply is accepted"),
        "a reply behind the synced revision",
    );
    let later = inlay::Interaction::tooltip(tickets.issue(doc.revision()), key, 0);
    let output = client.interact(&doc.snapshot(), &later);
    assert_eq!(tooltip(only(&output)).1, None, "the set is from an old revision");
}

/// A resolve that brings no tooltip falls through to the hover at the part's location.
#[test]
fn resolve_without_a_tooltip_falls_back_to_the_location_hover() {
    let (mut client, doc, mut tickets, hints) = hinted(inlay_capabilities());
    let gesture =
        inlay::Interaction::tooltip(tickets.issue(doc.revision()), hints[0].hint().key(), 1);
    let _ = client.interact(&doc.snapshot(), &gesture);
    let output = client.receive(reply(3, type_hint())).expect("the reply is accepted");
    assert_eq!(
        wire(&output.messages),
        vec![location_hover_wire(4)],
        "the hover at the part's location"
    );
    assert!(output.updates.is_empty(), "nothing is answered yet");
    let output = client.receive(i32_hover(4)).expect("the reply is accepted");
    let (stamp, markdown) = tooltip(only(&output));
    assert_eq!(
        stamp,
        update::Stamp::Ticket(gesture.ticket()),
        "the hover answers the gesture"
    );
    assert_eq!(markdown.as_deref(), Some("`struct i32`"), "the hover's card");
}

/// Without lazy tooltips a part's location is hovered at once, even in a file nobody opened.
#[test]
fn inlay_tooltip_hovers_at_a_part_location_in_an_unopened_file() {
    let (mut client, doc, mut tickets, hints) = hinted(resolveless_capabilities());
    let gesture =
        inlay::Interaction::tooltip(tickets.issue(doc.revision()), hints[0].hint().key(), 1);
    assert_eq!(
        wire(&client.interact(&doc.snapshot(), &gesture).messages),
        vec![location_hover_wire(3)],
        "the location goes out as the server sent it",
    );
    let output = client.receive(i32_hover(3)).expect("the reply is accepted");
    assert_eq!(
        tooltip(only(&output)).1.as_deref(),
        Some("`struct i32`"),
        "the hover's card"
    );
}

/// With no tooltip, nothing to resolve and no location, the answer is `None` at once.
#[test]
fn inlay_tooltip_without_any_source_answers_none_at_once() {
    let (mut client, doc, mut tickets, hints) = hinted(resolveless_capabilities());
    let gesture =
        inlay::Interaction::tooltip(tickets.issue(doc.revision()), hints[1].hint().key(), 0);
    let output = client.interact(&doc.snapshot(), &gesture);
    assert_eq!(tooltip(only(&output)).1, None, "nothing to show");
}

/// A server without hover gets no location hover.
#[test]
fn no_location_hover_without_a_hover_provider() {
    let (mut client, doc, mut tickets, hints) =
        hinted(json!({"textDocumentSync": 2, "inlayHintProvider": true}));
    let gesture =
        inlay::Interaction::tooltip(tickets.issue(doc.revision()), hints[0].hint().key(), 1);
    let output = client.interact(&doc.snapshot(), &gesture);
    assert_eq!(tooltip(only(&output)).1, None, "nothing to ask");
}

/// A location in another open document that moved since the fetch is not hovered.
#[test]
fn location_hover_into_a_moved_open_document_declines() {
    let mut tickets = Counter::new();
    let (mut client, doc) = hinting(INLAY_TEXT, resolveless_capabilities());
    let mut callee = document(CALLEE);
    open_as(&mut client, &callee, "file:///b.rs");
    let hints = fetch(
        &mut client,
        &mut tickets,
        &doc,
        2,
        json!([linked_hint("file:///b.rs", span_on(0, 3, 8))]),
    );
    type_ops(&mut client, &mut callee, vec![EditOp::insert(0, "\n")]);
    let gesture =
        inlay::Interaction::tooltip(tickets.issue(doc.revision()), hints[0].hint().key(), 0);
    let output = client.interact(&doc.snapshot(), &gesture);
    assert_eq!(tooltip(only(&output)).1, None, "the location moved");
}

/// A failed resolve answers no tooltip and leaves the hint resolvable for the next hover.
#[test]
fn failed_resolve_answers_no_tooltip_and_retries_on_the_next_hover() {
    let (mut client, doc, mut tickets, hints) = hinted(inlay_capabilities());
    let key = hints[0].hint().key();
    let gesture = inlay::Interaction::tooltip(tickets.issue(doc.revision()), key, 0);
    let _ = client.interact(&doc.snapshot(), &gesture);
    let output = client.receive(failure(3, -32603)).expect("an intel failure is no error");
    assert_eq!(tooltip(only(&output)).1, None, "the failure settles the gesture");
    let again = inlay::Interaction::tooltip(tickets.issue(doc.revision()), key, 0);
    assert_eq!(
        wire(&client.interact(&doc.snapshot(), &again).messages),
        vec![resolve_wire(4, type_hint())],
        "the hint is resolved again",
    );
}

/// A new tooltip gesture cancels the resolve in flight.
#[test]
fn new_inlay_tooltip_supersedes_the_one_in_flight() {
    let (mut client, doc, mut tickets, hints) = hinted(inlay_capabilities());
    let key = hints[0].hint().key();
    let first = inlay::Interaction::tooltip(tickets.issue(doc.revision()), key, 0);
    let _ = client.interact(&doc.snapshot(), &first);
    let second = inlay::Interaction::tooltip(tickets.issue(doc.revision()), key, 0);
    assert_eq!(
        wire(&client.interact(&doc.snapshot(), &second).messages),
        vec![cancel_request(3), resolve_wire(4, type_hint())],
        "the old resolve is cancelled before the new one goes out",
    );
}

/// A label part linking into the requesting document jumps there, without asking the server.
#[test]
fn label_jump_into_the_same_document_is_a_local_target() {
    let mut tickets = Counter::new();
    let (mut client, doc) = hinting(INLAY_TEXT, inlay_capabilities());
    let hints = fetch(
        &mut client,
        &mut tickets,
        &doc,
        2,
        json!([linked_hint("file:///a.rs", span_on(1, 4, 5))]),
    );
    let gesture = inlay::Interaction::jump(tickets.issue(doc.revision()), hints[0].hint().key(), 0);
    let (stamp, target) = defined(&client.interact(&doc.snapshot(), &gesture));
    assert_eq!(
        stamp,
        update::Stamp::Ticket(gesture.ticket()),
        "stamped with the gesture's ticket"
    );
    assert_eq!(target, Some(update::Target::Local(18..19)), "over `b`");
}

/// A label part linking into another open document jumps there at its synced revision.
#[test]
fn label_jump_into_another_open_document_is_an_open_target() {
    let mut tickets = Counter::new();
    let (mut client, doc) = hinting(INLAY_TEXT, inlay_capabilities());
    let callee = document(CALLEE);
    open_as(&mut client, &callee, "file:///b.rs");
    let hints = fetch(
        &mut client,
        &mut tickets,
        &doc,
        2,
        json!([linked_hint("file:///b.rs", span_on(0, 3, 8))]),
    );
    let gesture = inlay::Interaction::jump(tickets.issue(doc.revision()), hints[0].hint().key(), 0);
    let (_, target) = defined(&client.interact(&doc.snapshot(), &gesture));
    let Some(update::Target::Open(open)) = target else {
        panic!("expected an open target, got {target:?}")
    };
    assert_eq!(open.doc_id(), callee.doc_id(), "the callee's document");
    assert_eq!(open.revision(), callee.revision(), "at its synced revision");
    assert_eq!(open.span(), 3..8, "over `greet`");
}

/// A link into an open document that moved since the fetch points at text the server never
/// saw, so the jump is dropped.
#[test]
fn label_jump_into_an_open_document_that_moved_is_dropped() {
    let mut tickets = Counter::new();
    let (mut client, doc) = hinting(INLAY_TEXT, inlay_capabilities());
    let mut callee = document(CALLEE);
    open_as(&mut client, &callee, "file:///b.rs");
    let hints = fetch(
        &mut client,
        &mut tickets,
        &doc,
        2,
        json!([linked_hint("file:///b.rs", span_on(0, 3, 8))]),
    );
    type_ops(&mut client, &mut callee, vec![EditOp::insert(0, "\n")]);
    let gesture = inlay::Interaction::jump(tickets.issue(doc.revision()), hints[0].hint().key(), 0);
    let (_, target) = defined(&client.interact(&doc.snapshot(), &gesture));
    assert_eq!(target, None, "the moved target is dropped");
}

/// A document opened after the fetch has no recorded revision, so a jump into it is dropped.
#[test]
fn label_jump_into_a_document_opened_after_the_fetch_is_dropped() {
    let mut tickets = Counter::new();
    let (mut client, doc) = hinting(INLAY_TEXT, inlay_capabilities());
    let hints = fetch(
        &mut client,
        &mut tickets,
        &doc,
        2,
        json!([linked_hint("file:///b.rs", span_on(0, 3, 8))]),
    );
    open_as(&mut client, &document(CALLEE), "file:///b.rs");
    let gesture = inlay::Interaction::jump(tickets.issue(doc.revision()), hints[0].hint().key(), 0);
    let (_, target) = defined(&client.interact(&doc.snapshot(), &gesture));
    assert_eq!(target, None, "the late-opened target is dropped");
}

/// A link into a file nobody opened keeps the server's range for the host to convert.
#[test]
fn label_jump_into_an_unopened_file_is_an_unopened_target() {
    let (mut client, doc, mut tickets, hints) = hinted(inlay_capabilities());
    let gesture = inlay::Interaction::jump(tickets.issue(doc.revision()), hints[0].hint().key(), 1);
    let (_, target) = defined(&client.interact(&doc.snapshot(), &gesture));
    let Some(update::Target::Unopened(unopened)) = target else {
        panic!("expected an unopened target, got {target:?}")
    };
    assert_eq!(unopened.uri().as_str(), "file:///w/core.rs", "the file");
    assert_eq!(
        unopened.span("a\nb\nc\nlet i32\n"),
        10..13,
        "converted against the file's text"
    );
}

/// A part without a location jumps nowhere.
#[test]
fn jump_on_a_part_without_a_location_answers_none() {
    let (mut client, doc, mut tickets, hints) = hinted(inlay_capabilities());
    let gesture = inlay::Interaction::jump(tickets.issue(doc.revision()), hints[0].hint().key(), 0);
    let (_, target) = defined(&client.interact(&doc.snapshot(), &gesture));
    assert_eq!(target, None, "`: ` has no location");
}

/// An insert answers the hint's text edits, trimmed by hygiene, without asking the server.
#[test]
fn inlay_insert_answers_the_hint_edits_through_hygiene() {
    let (mut client, doc, mut tickets, hints) = hinted(inlay_capabilities());
    let gesture =
        inlay::Interaction::insert(tickets.issue(doc.revision()), hints[2].hint().key(), 19);
    let output = client.interact(&doc.snapshot(), &gesture);
    let (stamp, ops) = edits(only(&output));
    assert_eq!(
        stamp,
        update::Stamp::Ticket(gesture.ticket()),
        "stamped with the gesture's ticket"
    );
    assert_eq!(ops, vec![EditOp::insert(19, ": i32")], "the trimmed insert after `b`");
}

/// An insert on a hint without edits answers an empty batch, which settles the gesture.
#[test]
fn inlay_insert_without_edits_declines_with_no_edits() {
    let (mut client, doc, mut tickets, hints) = hinted(inlay_capabilities());
    let gesture =
        inlay::Interaction::insert(tickets.issue(doc.revision()), hints[1].hint().key(), 10);
    let output = client.interact(&doc.snapshot(), &gesture);
    assert!(edits(only(&output)).1.is_empty(), "no edits");
}

/// Once the text moved past the set, every gesture gets its empty answer without asking.
#[test]
fn interactions_on_a_moved_set_decline() {
    let (mut client, mut doc, mut tickets, hints) = hinted(inlay_capabilities());
    type_ops(&mut client, &mut doc, vec![EditOp::insert(25, "x")]);
    let mut ticket = || tickets.issue(doc.revision());
    let tooltip_gesture = inlay::Interaction::tooltip(ticket(), hints[0].hint().key(), 0);
    let jump = inlay::Interaction::jump(ticket(), hints[0].hint().key(), 1);
    let insert = inlay::Interaction::insert(ticket(), hints[2].hint().key(), 19);
    assert_eq!(
        tooltip(only(&client.interact(&doc.snapshot(), &tooltip_gesture))).1,
        None,
        "no tooltip"
    );
    assert_eq!(
        defined(&client.interact(&doc.snapshot(), &jump)).1,
        None,
        "no target"
    );
    assert!(
        edits(only(&client.interact(&doc.snapshot(), &insert))).1.is_empty(),
        "no edits"
    );
}

/// A key no fetch minted gets the empty answer.
#[test]
fn interaction_with_an_unknown_key_declines() {
    let (mut client, doc, mut tickets, _) = hinted(inlay_capabilities());
    let gesture =
        inlay::Interaction::tooltip(tickets.issue(doc.revision()), inlay::Key::new(u64::MAX), 0);
    let output = client.interact(&doc.snapshot(), &gesture);
    assert_eq!(tooltip(only(&output)).1, None, "an unknown hint");
}

/// Once the server is shutting down, gestures get their empty answer.
#[test]
fn interactions_decline_once_the_server_is_shutting_down() {
    let (mut client, doc, mut tickets, hints) = hinted(inlay_capabilities());
    let _ = client.shutdown();
    let gesture = inlay::Interaction::jump(tickets.issue(doc.revision()), hints[0].hint().key(), 1);
    assert_eq!(
        defined(&client.interact(&doc.snapshot(), &gesture)).1,
        None,
        "no target"
    );
}

/// Closing a document forgets its hints: a reopened document answers no gesture on them.
#[test]
fn close_forgets_the_inlay_set() {
    let (mut client, doc, mut tickets, hints) = hinted(inlay_capabilities());
    let _ = client.close(doc.doc_id());
    open_as(&mut client, &doc, "file:///a.rs");
    let gesture = inlay::Interaction::jump(tickets.issue(doc.revision()), hints[0].hint().key(), 1);
    assert_eq!(
        defined(&client.interact(&doc.snapshot(), &gesture)).1,
        None,
        "the set went with the close"
    );
}

/// A server offering everything the disconnect tests touch, with utf-8 positions.
fn everything() -> Value {
    json!({
        "positionEncoding": "utf-8",
        "textDocumentSync": {"openClose": true, "change": 2, "save": true},
        "completionProvider": {},
        "signatureHelpProvider": {},
        "hoverProvider": true,
        "definitionProvider": true,
        "renameProvider": true,
        "documentFormattingProvider": true,
        "inlayHintProvider": true,
    })
}

/// A running session with `text` open as `file:///a.rs`, on a server that offers everything.
fn connected(text: &str) -> (Session, Document) {
    let doc = document(text);
    let (mut client, _) = running(Builder::default(), everything());
    open_as(&mut client, &doc, "file:///a.rs");
    (client, doc)
}

/// A lost server settles a request its editor still awaits with the empty answer, and drops
/// one whose document moved past it, as its reply would be.
#[test]
fn disconnected_settles_pending_requests_through_the_staleness_filter() {
    let mut tickets = Counter::new();
    let (mut client, a) = connected("let value = 1;");
    let mut b = document("let v = pr");
    open_as(&mut client, &b, "file:///b.rs");
    let hovering = hover_request(&mut tickets, &a);
    assert!(
        !client.hover(&a.snapshot(), &hovering).messages.is_empty(),
        "the hover goes out"
    );
    let completion = request(
        &mut tickets,
        &b,
        8..10,
        CompletionTrigger::Manual,
        Start::Fresh,
    );
    assert!(
        !client
            .complete(&b.snapshot(), &completion)
            .messages
            .is_empty(),
        "the completion goes out"
    );
    b.edit(vec![EditOp::insert(10, "i")]).expect("edits");
    let _ = client.sync(&b.snapshot(), b.drain_changes());

    let output = client.disconnected();
    assert!(output.messages.is_empty(), "a disconnect sends nothing");
    let (stamp, card) = hover(&output.updates[0]);
    assert_eq!(
        stamp,
        update::Stamp::Ticket(hovering.ticket),
        "the hover settles under its ticket"
    );
    assert!(card.is_none(), "with no card");
    assert!(
        output.updates[1..]
            .iter()
            .all(|update| matches!(update, Update::Document(d) if matches!(d.change(), update::Change::Diagnostics(_)))),
        "the moved completion is dropped, not refused: {:?}",
        output.updates,
    );
}

/// A definition in flight settles with no target, so the editor's slot clears.
#[test]
fn disconnected_settles_a_pending_definition_with_none() {
    let mut tickets = Counter::new();
    let (mut client, doc) = connected("let value = 1;");
    let request = DefinitionRequest::new(tickets.issue(doc.revision()), 5);
    let _ = client.definition(&doc.snapshot(), &request);
    let output = client.disconnected();
    let (stamp, target) = definition(&output.updates[0]);
    assert_eq!(
        stamp,
        update::Stamp::Ticket(request.ticket),
        "the definition's ticket"
    );
    assert!(target.is_none(), "no target");
}

/// A lost server's knowledge goes: diagnostics clear, the unopened cache, hints, completion
/// sessions and versions are forgotten, and positions fall back to utf-16.
#[test]
fn disconnected_clears_diagnostics_caches_hints_and_versions() {
    let mut tickets = Counter::new();
    let (mut client, doc) = connected("let v = pr");
    let _ = client
        .receive(publish("file:///c.rs", None, Some((0, 1))))
        .expect("the publish is accepted");
    assert!(!client.cached.is_empty(), "the unopened publish is cached");
    let _ = fetch(&mut client, &mut tickets, &doc, 2, json!([]));
    let completion = request(
        &mut tickets,
        &doc,
        8..10,
        CompletionTrigger::Manual,
        Start::Fresh,
    );
    let _ = client.complete(&doc.snapshot(), &completion);
    assert_eq!(client.encoding, Encoding::Utf8, "utf-8 was negotiated");

    let output = client.disconnected();
    let cleared: Vec<_> = output.updates.iter().filter(|update| {
        matches!(update, Update::Document(d) if matches!(d.change(), update::Change::Diagnostics(set) if set.is_empty()))
    }).collect();
    assert_eq!(cleared.len(), 1, "one empty diagnostic set per document");
    let (doc_id, stamp, _) = diagnostics(cleared[0]);
    assert_eq!(doc_id, doc.doc_id(), "for the open document");
    assert_eq!(
        stamp,
        update::Stamp::Revision(doc.revision()),
        "at its synced revision"
    );
    assert!(client.cached.is_empty(), "the cache is cleared");
    let tracked = &client.tracked[0];
    assert!(tracked.inlays.is_none(), "the hints are forgotten");
    assert!(
        tracked.session.is_none(),
        "the completion session is forgotten"
    );
    assert!(tracked.version.is_none(), "the version is forgotten");
    assert_eq!(
        client.encoding,
        Encoding::Utf16,
        "positions fall back to utf-16"
    );
    assert!(
        client.save(&doc.snapshot()).messages.is_empty(),
        "nothing saves"
    );
}

/// After a disconnect, an edit is synced locally and every request with an awaited slot
/// declines; rename and format send nothing.
#[test]
fn requests_decline_after_disconnected() {
    let mut tickets = Counter::new();
    let (mut client, mut doc) = connected("let value = 1;");
    let _ = client.disconnected();
    doc.edit(vec![EditOp::insert(14, " ")]).expect("edits");
    assert!(
        client
            .sync(&doc.snapshot(), doc.drain_changes())
            .messages
            .is_empty(),
        "sync sends nothing"
    );
    let snapshot = doc.snapshot();
    let request = request(
        &mut tickets,
        &doc,
        4..9,
        CompletionTrigger::Manual,
        Start::Fresh,
    );
    assert!(
        answered(&client.complete(&snapshot, &request)).1.is_empty(),
        "completion declines"
    );
    let request = signature_request(&mut tickets, &doc, 5, None);
    assert!(
        signature(only(&client.signature_help(&snapshot, &request)))
            .1
            .is_none(),
        "signature declines"
    );
    let request = hover_request(&mut tickets, &doc);
    assert!(
        hovered(&client.hover(&snapshot, &request)).1.is_none(),
        "hover declines"
    );
    let request = DefinitionRequest::new(tickets.issue(doc.revision()), 5);
    assert!(
        definition(only(&client.definition(&snapshot, &request)))
            .1
            .is_none(),
        "definition declines"
    );
    let request = inlay_request(&mut tickets, &doc);
    assert!(
        inlays(only(&client.inlays(&snapshot, &request)))
            .1
            .is_some_and(|hints| hints.is_empty()),
        "inlays decline"
    );
    let request = RenameRequest::new(tickets.issue(doc.revision()), 5, "other");
    assert_silent(&client.rename(&snapshot, &request), "rename sends nothing");
    let request = FormatRequest::new(tickets.issue(doc.revision()), 4);
    assert_silent(&client.format(&snapshot, &request), "format sends nothing");
}

/// While running, new settings go out at once, and later configuration requests read them.
#[test]
fn configure_while_running_pushes_did_change_configuration() {
    let (mut client, _) = running(
        Builder::default().configuration(json!({"x": 1})),
        incremental(),
    );
    let output = client.configure(json!({"x": 2}));
    assert_eq!(
        wire(&output.messages),
        vec![
            json!({"jsonrpc": "2.0", "method": "workspace/didChangeConfiguration",
            "params": {"settings": {"x": 2}}})
        ],
        "the new settings go out",
    );
    let response = answer(
        &mut client,
        "workspace/configuration",
        json!({"items": [{"section": "x"}]}),
    );
    assert_eq!(
        response["result"],
        json!([2]),
        "configuration requests read the new settings"
    );
}

/// Before the handshake, settings are only stored, and the handshake pushes them once.
#[test]
fn configure_before_initialize_only_stores() {
    let (mut client, _) = Builder::default()
        .configuration(json!({"x": 1}))
        .session(None);
    assert!(
        client.configure(json!({"x": 2})).messages.is_empty(),
        "nothing goes out yet"
    );
    let output = client
        .receive(from_server(
            json!({"jsonrpc": "2.0", "id": 1, "result": {"capabilities": incremental()}}),
        ))
        .expect("initialize answer is accepted");
    let pushes: Vec<_> = wire(&output.messages)
        .into_iter()
        .filter(|message| message["method"] == "workspace/didChangeConfiguration")
        .collect();
    assert_eq!(
        pushes,
        vec![
            json!({"jsonrpc": "2.0", "method": "workspace/didChangeConfiguration",
            "params": {"settings": {"x": 2}}})
        ],
        "the handshake pushes the new settings, once",
    );
}

/// A reply's method is known for `initialize` and for requests in flight, and for nothing else.
#[test]
fn method_names_initialize_and_pending_requests_only() {
    let (client, _) = Builder::default().session(None);
    assert_eq!(
        client.method(&message::Id::Number(1)),
        Some("initialize"),
        "initialize"
    );
    let mut tickets = Counter::new();
    let (mut client, doc) = connected("let value = 1;");
    let _ = client.hover(&doc.snapshot(), &hover_request(&mut tickets, &doc));
    assert_eq!(
        client.method(&message::Id::Number(2)),
        Some("textDocument/hover"),
        "a hover"
    );
    assert_eq!(
        client.method(&message::Id::Number(9)),
        None,
        "an unknown id"
    );
    assert_eq!(
        client.method(&message::Id::Number(1)),
        None,
        "initialize, once answered"
    );
}
