use std::str::FromStr;

use scrive_core::intel::completion::Start;
use scrive_core::intel::ticket::Counter;
use scrive_core::{
    CompletionItem, Document, EditOp, FormatRequest, GroupingHint, HoverInfo, HoverRequest,
    OpClass, Point, SignatureInfo,
};
use serde_json::{json, Value};

use super::*;

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
fn running(builder: Builder, capabilities: Value) -> (Client, Output) {
    let (mut client, _) = builder.build();
    let output = client
        .receive(from_server(
            json!({"jsonrpc": "2.0", "id": 1, "result": {"capabilities": capabilities}}),
        ))
        .expect("initialize answer is accepted");
    (client, output)
}

/// The one response `client` sends to a server request for `method` with `params`.
fn answer(client: &mut Client, method: &str, params: Value) -> Value {
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

/// With the other per-variant helpers ([`completions`], [`signature`], [`hover`], [`edits`]),
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
    let (_, initialize) = Client::builder()
        .root(uri("file:///work/proj"))
        .process_id(42)
        .initialization_options(json!({"a": 1}))
        .build();
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
        ("/params/capabilities/workspace/configuration", json!(true)),
        (
            "/params/capabilities/workspace/workspaceFolders",
            json!(true),
        ),
        ("/params/processId", json!(42)),
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
    let (mut client, _) = Client::builder().configuration(json!({"x": 1})).build();
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
    let (mut client, _) = Client::builder().build();
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
    let (mut client, _) = running(Client::builder(), incremental());
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
    let (mut client, _) = running(Client::builder(), json!({"textDocumentSync": 2}));
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
    let (mut client, _) = running(Client::builder(), incremental());
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
    let (mut client, _) = running(Client::builder(), incremental());
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
    let (mut client, _) = running(Client::builder(), incremental());
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
    let (mut client, _) = running(Client::builder(), incremental());
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
    let (mut client, _) = running(Client::builder(), json!({"textDocumentSync": 1}));
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
    let (mut client, _) = running(Client::builder(), incremental());
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
    let (mut client, _) = running(Client::builder(), incremental());
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
    let (mut client, _) = running(Client::builder(), json!({"textDocumentSync": 0}));
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
    let (mut client, _) = running(Client::builder(), incremental());
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
    let (mut client, _) = running(Client::builder(), incremental());
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
    let (mut client, _) = running(Client::builder(), incremental());
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
    let (mut client, _) = running(Client::builder(), incremental());
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
    let (mut client, _) = running(Client::builder(), incremental());
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
    let (mut client, _) = running(Client::builder(), incremental());
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
    let (mut client, _) = running(Client::builder(), incremental());
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
    let (mut client, _) = running(Client::builder(), incremental());
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

/// `shutdown` goes out alone; `exit` follows only once the server has answered it.
#[test]
fn shutdown_sends_shutdown_then_exit_on_its_response() {
    let (mut client, _) = running(Client::builder(), incremental());
    assert_eq!(
        wire(&client.shutdown().messages),
        vec![json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown"})],
        "shutdown is request 2, without params",
    );
    let output = client
        .receive(from_server(
            json!({"jsonrpc": "2.0", "id": 2, "result": null}),
        ))
        .expect("the shutdown answer is accepted");
    assert_eq!(
        wire(&output.messages),
        vec![json!({"jsonrpc": "2.0", "method": "exit"})],
        "the shutdown answer sends exit",
    );
}

/// A server that fails `shutdown` still gets its `exit`.
#[test]
fn shutdown_error_response_still_sends_exit() {
    let (mut client, _) = running(Client::builder(), incremental());
    let _ = client.shutdown();
    let output = client
        .receive(from_server(
            json!({"jsonrpc": "2.0", "id": 2, "error": {"code": -32603, "message": "busy"}}),
        ))
        .expect("the shutdown error is accepted");
    assert_eq!(
        wire(&output.messages),
        vec![json!({"jsonrpc": "2.0", "method": "exit"})],
        "an error answer to shutdown still sends exit",
    );
}

/// Before `initialize` is answered there is nothing to shut down: nothing goes out, and the
/// late answer is ignored, and so is every later sync.
#[test]
fn shutdown_before_initialize_exits_silently() {
    let mut doc = document("a");
    let (mut client, _) = Client::builder().build();
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
        Client::builder().configuration(json!({"x": 1})),
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
    let (mut client, _) = running(Client::builder(), incremental());
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

/// A server that fails `initialize` cannot be synced, so the error surfaces and the documents
/// waiting for their `didOpen` are dropped.
#[test]
fn failed_initialize_is_a_server_error_and_drops_deferred_opens() {
    let mut doc = document("a");
    let (mut client, _) = Client::builder().build();
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
    doc.edit(vec![EditOp::insert(1, "b")]).expect("edits");
    assert!(
        client
            .sync(&doc.snapshot(), doc.drain_changes())
            .messages
            .is_empty(),
        "nothing syncs after a failed initialize",
    );
    assert!(
        client.close(doc.doc_id()).messages.is_empty(),
        "the deferred document is gone",
    );
}

/// `workspace/configuration` reads dotted sections; an empty or absent section is the whole
/// value and a missing one is `null`.
#[test]
fn configuration_answers_dotted_sections() {
    let configuration = json!({"rust-analyzer": {"check": {"command": "clippy"}}});
    let (mut client, _) = running(
        Client::builder().configuration(configuration.clone()),
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
        Client::builder().root(uri("file:///work/proj")),
        incremental(),
    );
    assert_eq!(
        answer(&mut rooted, "workspace/workspaceFolders", Value::Null)["result"],
        json!([{"uri": "file:///work/proj", "name": "proj"}]),
        "the root is the one folder",
    );
    let (mut rootless, _) = running(Client::builder(), incremental());
    assert_eq!(
        answer(&mut rootless, "workspace/workspaceFolders", Value::Null)["result"],
        Value::Null,
        "without a root there are no folders",
    );
}

/// Registration, progress, message and refresh requests need no work and get `null`.
#[test]
fn housekeeping_server_requests_get_null() {
    let (mut client, _) = running(Client::builder(), incremental());
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
    let (mut client, _) = running(Client::builder(), incremental());
    let response = answer(&mut client, "workspace/applyEdit", json!({"edit": {}}));
    assert_eq!(
        response["result"]["applied"], false,
        "the edit is not applied"
    );
}

/// A configuration request whose params do not decode is answered with `InvalidParams`.
#[test]
fn undecodable_configuration_params_answer_invalid_params() {
    let (mut client, _) = running(Client::builder(), incremental());
    let response = answer(&mut client, "workspace/configuration", json!({"items": 3}));
    assert_eq!(
        response["error"]["code"], -32602,
        "bad params are InvalidParams"
    );
}

/// A request the client does not implement is answered with `MethodNotFound`.
#[test]
fn unknown_server_request_answers_method_not_found() {
    let (mut client, _) = running(Client::builder(), incremental());
    let response = answer(&mut client, "custom/thing", json!({}));
    assert_eq!(
        response["error"]["code"], -32601,
        "unknown methods are MethodNotFound"
    );
}

/// Notifications the client does not consume reach the host unchanged.
#[test]
fn unhandled_notifications_pass_through() {
    let (mut client, _) = running(Client::builder(), incremental());
    for fixture in [
        json!({"jsonrpc": "2.0", "method": "window/logMessage", "params": {"type": 3, "message": "hi"}}),
        json!({"jsonrpc": "2.0", "method": "$/progress", "params": {"token": 1, "value": {}}}),
    ] {
        let sent = from_server(fixture);
        let output = client
            .receive(sent.clone())
            .expect("notifications are accepted");
        let Message::Notification(sent) = sent else {
            panic!("the fixture is a notification")
        };
        match output.updates.as_slice() {
            [Update::Notification(passed)] => {
                assert_eq!(*passed, sent, "{} passes through unchanged", sent.method);
            }
            other => panic!("expected one passed-through notification, got {other:?}"),
        }
    }
}

/// A response nothing is waiting for is ignored.
#[test]
fn unknown_response_id_is_silent() {
    let (mut client, _) = running(Client::builder(), incremental());
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
fn completing(text: &str) -> (Client, Document) {
    let doc = document(text);
    let (mut client, _) = running(Client::builder(), completion_capabilities());
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
    let (_, initialize) = Client::builder().build();
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
    let (mut client, _) = Client::builder().build();
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
    let (mut client, _) = running(Client::builder(), json!({"textDocumentSync": 2}));
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

/// `shutdown` goes out alone, and the forgotten request's reply is silent.
#[test]
fn shutdown_forgets_pending_requests() {
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
        wire(&client.shutdown().messages),
        vec![json!({"jsonrpc": "2.0", "id": 3, "method": "shutdown"})],
        "only shutdown goes out",
    );
    let output = client
        .receive(list(2, false))
        .expect("the reply is accepted");
    assert_silent(&output, "a forgotten request's reply is dropped");
}

/// Applies `ops` as one typing commit and syncs it.
fn type_ops(client: &mut Client, doc: &mut Document, ops: Vec<EditOp>) {
    doc.edit_grouped(ops, GroupingHint::mergeable(OpClass::Type))
        .expect("types");
    let _ = client.sync(&doc.snapshot(), doc.drain_changes());
}

/// A client whose first request, at `pr` in `let v = pr`, has been answered with [`list`].
fn answered_session(tickets: &mut Counter, incomplete: bool) -> (Client, Document) {
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
fn signing(text: &str) -> (Client, Document) {
    let doc = document(text);
    let (mut client, _) = running(Client::builder(), signature_hover_capabilities());
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
    let (_, initialize) = Client::builder().build();
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
    let (initializing, _) = Client::builder().build();
    let (running, _) = running(Client::builder(), json!({"textDocumentSync": 2}));
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
    let (initializing, _) = Client::builder().build();
    let (without, _) = running(Client::builder(), json!({"textDocumentSync": 2}));
    let (disabled, _) = running(
        Client::builder(),
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
        "documentFormattingProvider": true,
    })
}

/// A running client with `text` open as `file:///a.rs`, whose server offers the commands.
fn commanding(text: &str) -> (Client, Document) {
    let doc = document(text);
    let (mut client, _) = running(Client::builder(), command_capabilities());
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
    let (_, initialize) = Client::builder().build();
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
    let (initializing, _) = Client::builder().build();
    let (without, _) = running(Client::builder(), json!({"textDocumentSync": 2}));
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
