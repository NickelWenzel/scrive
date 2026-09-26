use std::str::FromStr;

use scrive_core::Document;
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
    let doc = document("fn main() {}");
    let (mut client, _) = Client::builder().configuration(json!({"x": 1})).build();
    let opened = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    assert!(
        opened.messages.is_empty(),
        "nothing goes out before initialize is answered"
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
            did_open("file:///a.rs", 1, "fn main() {}"),
        ],
        "the handshake finishes, then the deferred document opens",
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
/// late answer is ignored.
#[test]
fn shutdown_before_initialize_exits_silently() {
    let doc = document("a");
    let (mut client, _) = Client::builder().build();
    let _ = client
        .open(&doc.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    assert!(
        client.shutdown().messages.is_empty(),
        "shutdown sends nothing"
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

/// After `shutdown()`, registering and closing documents send nothing.
#[test]
fn client_calls_decline_after_shutdown() {
    let (a, b) = (document("a"), document("b"));
    let (mut client, _) = running(Client::builder(), incremental());
    let _ = client
        .open(&a.snapshot(), &uri("file:///a.rs"), "rust")
        .expect("opens");
    let _ = client.shutdown();
    let opened = client
        .open(&b.snapshot(), &uri("file:///b.rs"), "rust")
        .expect("open declines without an error");
    assert!(opened.messages.is_empty(), "open declines after shutdown");
    assert!(
        client.close(a.doc_id()).messages.is_empty(),
        "close declines after shutdown"
    );
}

/// A server that fails `initialize` cannot be synced, so the error surfaces and the documents
/// waiting for their `didOpen` are dropped.
#[test]
fn failed_initialize_is_a_server_error_and_drops_deferred_opens() {
    let doc = document("a");
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
