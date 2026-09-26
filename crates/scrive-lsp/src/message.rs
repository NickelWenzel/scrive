//! The JSON-RPC 2.0 envelope every LSP message travels in.
//!
//! Parsing is tolerant because real servers are: `jsonrpc` may be missing, ids arrive as
//! `1.0`, error replies to unparseable requests carry `"id": null`, and some servers send both
//! `result` and `error`. Output is strict: `jsonrpc: "2.0"` always, params omitted rather than
//! `null`, and a success reply always carries `result`.

use core::fmt;

use serde::de::{self, Deserializer};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One JSON-RPC message, in either direction.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    /// A call that expects a [`Response`] with the same id.
    Request(Request),
    /// The answer to a [`Request`].
    Response(Response),
    /// A one-way message; nothing answers it.
    Notification(Notification),
}

/// A call that expects a response.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    /// Correlates the response with this request.
    pub id: Id,
    /// The LSP method, e.g. `textDocument/completion`.
    pub method: String,
    /// The params object or array; `None` when absent (or `null` on the wire).
    pub params: Option<Value>,
}

/// The answer to a request.
#[derive(Clone, Debug, PartialEq)]
pub struct Response {
    /// The id of the request this answers. `None` for an error reply to a request whose id the
    /// peer could not read (`"id": null`).
    pub id: Option<Id>,
    /// The `result` member, or the `error` member when present (the error wins over a result).
    pub result: Result<Value, Error>,
}

/// A one-way message.
#[derive(Clone, Debug, PartialEq)]
pub struct Notification {
    /// The LSP method, e.g. `textDocument/publishDiagnostics`.
    pub method: String,
    /// The params object or array; `None` when absent (or `null` on the wire).
    pub params: Option<Value>,
}

/// A request id. Floats with a zero fraction read as integers, so `1.0` and `1` match.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(untagged)]
pub enum Id {
    /// An integer id.
    Number(i64),
    /// A string id.
    String(String),
}

/// The `error` member of a failed response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Error {
    /// The JSON-RPC or LSP error code, e.g. `-32601` (method not found) or `-32801` (content
    /// modified).
    pub code: i64,
    /// A human-readable description. Missing on the wire reads as empty.
    #[serde(default)]
    pub message: String,
    /// Extra data the peer attached, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// The wire shape before classification.
#[derive(Deserialize)]
struct Raw {
    // `"id": null` and a missing id both read as `None`: neither identifies a request.
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
    // A plain `Option` reads `"result": null` as absent, but `null` is a successful void reply
    // (`shutdown`), so presence has to be detected separately.
    #[serde(default, deserialize_with = "present")]
    result: Option<Value>,
    #[serde(default)]
    error: Option<Error>,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl Serialize for Message {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("jsonrpc", "2.0")?;
        match self {
            Message::Request(request) => {
                map.serialize_entry("id", &request.id)?;
                map.serialize_entry("method", &request.method)?;
                if let Some(params) = request.params.as_ref().filter(|p| !p.is_null()) {
                    map.serialize_entry("params", params)?;
                }
            }
            Message::Notification(notification) => {
                map.serialize_entry("method", &notification.method)?;
                if let Some(params) = notification.params.as_ref().filter(|p| !p.is_null()) {
                    map.serialize_entry("params", params)?;
                }
            }
            Message::Response(response) => {
                // JSON-RPC 2.0 §5: an error reply to a request whose id could not be read
                // carries `"id": null`, which is what `None` serializes as.
                map.serialize_entry("id", &response.id)?;
                match &response.result {
                    Ok(result) => map.serialize_entry("result", result)?,
                    Err(error) => map.serialize_entry("error", error)?,
                }
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Message {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Raw::deserialize(deserializer)?;
        let id = raw
            .id
            .map(Id::parse)
            .transpose()
            .map_err(de::Error::custom)?;
        match (raw.method, id) {
            (Some(method), Some(id)) => Ok(Message::Request(Request {
                id,
                method,
                params: raw.params,
            })),
            // A null id cannot be answered, so a call carrying one is a notification.
            (Some(method), None) => Ok(Message::Notification(Notification {
                method,
                params: raw.params,
            })),
            (None, id) => {
                let result = match (raw.error, raw.result) {
                    (Some(error), _) => Err(error),
                    (None, Some(result)) => Ok(result),
                    (None, None) => {
                        return Err(de::Error::custom(
                            "message has no `method`, `result` or `error`",
                        ))
                    }
                };
                Ok(Message::Response(Response { id, result }))
            }
        }
    }
}

impl Request {
    /// A request for the lsp-types method `R`. Params that serialize to `null` (`()` for
    /// `shutdown`) are left out.
    #[must_use]
    pub fn new<R: lsp_types::request::Request>(id: Id, params: R::Params) -> Self {
        Self {
            id,
            method: R::METHOD.to_owned(),
            params: to_params(&params),
        }
    }
}

impl Notification {
    /// A notification for the lsp-types method `N`. Params that serialize to `null` (`()` for
    /// `exit`) are left out.
    #[must_use]
    pub fn new<N: lsp_types::notification::Notification>(params: N::Params) -> Self {
        Self {
            method: N::METHOD.to_owned(),
            params: to_params(&params),
        }
    }
}

impl Response {
    /// A success reply. `()` becomes `"result": null`.
    #[must_use]
    pub fn ok(id: Id, result: impl Serialize) -> Self {
        let result =
            serde_json::to_value(result).expect("lsp-types results always serialize to JSON");
        Self {
            id: Some(id),
            result: Ok(result),
        }
    }

    /// An error reply. `id` is `None` when the request's id could not be read.
    #[must_use]
    pub fn error(id: Option<Id>, error: Error) -> Self {
        Self {
            id,
            result: Err(error),
        }
    }
}

impl Id {
    /// Reads a wire id. Integers and zero-fraction floats up to 2^53 become `Number`.
    fn parse(value: Value) -> Result<Self, &'static str> {
        // 2^53: every integer at or below it is exact in an f64.
        const EXACT: f64 = 9_007_199_254_740_992.0;
        match value {
            Value::String(id) => Ok(Id::String(id)),
            Value::Number(number) => number
                .as_i64()
                .or_else(|| {
                    number
                        .as_f64()
                        .filter(|f| f.fract() == 0.0 && f.abs() <= EXACT)
                        .map(|f| f as i64)
                })
                .map(Id::Number)
                .ok_or("id is not an integer"),
            _ => Err("id is neither a number nor a string"),
        }
    }
}

/// Wraps any on-the-wire value, `null` included, in `Some`; only a missing member stays `None`.
fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

/// Params as a `Value`, with `null` meaning "absent".
fn to_params(params: &impl Serialize) -> Option<Value> {
    // lsp-types params are derived structs with string map keys, so conversion cannot fail.
    match serde_json::to_value(params).expect("lsp-types params always serialize to JSON") {
        Value::Null => None,
        params => Some(params),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn parse(value: Value) -> Message {
        serde_json::from_value(value).expect("fixture parses")
    }

    fn rejects(value: Value) -> bool {
        serde_json::from_value::<Message>(value).is_err()
    }

    fn wire(message: &Message) -> Value {
        serde_json::to_value(message).expect("message serializes")
    }

    fn response(message: Message) -> Response {
        let Message::Response(response) = message else {
            panic!("a message with no method is a response, got {message:?}")
        };
        response
    }

    /// Servers that omit `jsonrpc` still produce readable requests.
    #[test]
    fn request_parses_without_a_jsonrpc_member() {
        assert_eq!(
            parse(json!({"id": 1, "method": "initialize", "params": {}})),
            Message::Request(Request {
                id: Id::Number(1),
                method: "initialize".into(),
                params: Some(json!({})),
            }),
            "a request without `jsonrpc` parses",
        );
    }

    /// Both id kinds the spec allows parse to their own variant.
    #[test]
    fn string_and_integer_ids_parse() {
        let Message::Request(string) = parse(json!({"jsonrpc": "2.0", "id": "a", "method": "m"}))
        else {
            panic!("a method with an id is a request")
        };
        assert_eq!(
            string.id,
            Id::String("a".into()),
            "a string id stays a string"
        );
        let Message::Request(integer) = parse(json!({"id": 7, "method": "m"})) else {
            panic!("a method with an id is a request")
        };
        assert_eq!(integer.id, Id::Number(7), "an integer id stays a number");
    }

    /// `1.0` names the same request as `1`.
    #[test]
    fn zero_fraction_float_id_parses_as_an_integer() {
        assert_eq!(
            parse(json!({"id": 1.0, "result": null})),
            Message::Response(Response {
                id: Some(Id::Number(1)),
                result: Ok(Value::Null),
            }),
            "a zero-fraction float id reads as an integer",
        );
    }

    /// A fractional id names no request we could have sent.
    #[test]
    fn fractional_id_is_rejected() {
        assert!(
            rejects(json!({"id": 1.5, "result": null})),
            "id 1.5 is rejected"
        );
    }

    /// Ids are numbers or strings; anything else is malformed.
    #[test]
    fn boolean_id_is_rejected() {
        assert!(
            rejects(json!({"id": true, "method": "m"})),
            "a boolean id is rejected"
        );
    }

    /// An error reply to an unreadable request carries `"id": null` and still parses.
    #[test]
    fn error_response_with_null_id_parses() {
        let reply = response(parse(json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": {"code": -32700, "message": "Parse error"},
        })));
        assert_eq!(reply.id, None, "a null id reads as no id");
        assert_eq!(
            reply.result.map_err(|e| e.code),
            Err(-32700),
            "the error survives"
        );
    }

    /// `result` and `error` together are a server bug; the error is the truthful part, so it wins.
    #[test]
    fn error_wins_when_result_and_error_are_both_present() {
        let reply = response(parse(json!({
            "id": 2,
            "result": {"x": 1},
            "error": {"code": -32603, "message": "boom"},
        })));
        assert_eq!(reply.id, Some(Id::Number(2)), "the id survives");
        assert_eq!(
            reply.result.map_err(|e| e.code),
            Err(-32603),
            "the error wins over the result"
        );
    }

    /// `"result": null` is a successful void reply, not a missing result.
    #[test]
    fn null_result_is_a_successful_reply() {
        let reply = response(parse(json!({"id": 3, "result": null})));
        assert_eq!(reply.result, Ok(Value::Null), "a null result is a success");
    }

    /// Without a method, a result or an error there is nothing to classify.
    #[test]
    fn message_without_method_result_or_error_is_rejected() {
        assert!(rejects(json!({"id": 4})), "an id alone is rejected");
        assert!(rejects(json!({})), "an empty object is rejected");
    }

    /// Extra members and an unexpected `jsonrpc` value do not stop a parse.
    #[test]
    fn unknown_members_are_ignored() {
        assert_eq!(
            parse(json!({"method": "n", "extra": true, "jsonrpc": "1.0"})),
            Message::Notification(Notification {
                method: "n".into(),
                params: None,
            }),
            "unknown members are ignored",
        );
    }

    /// JSON-RPC allows structured params by name or by position, or none at all.
    #[test]
    fn params_may_be_an_object_an_array_or_absent() {
        for (fixture, params) in [
            (
                json!({"method": "n", "params": {"a": 1}}),
                Some(json!({"a": 1})),
            ),
            (
                json!({"method": "n", "params": [1, 2]}),
                Some(json!([1, 2])),
            ),
            (json!({"method": "n"}), None),
        ] {
            assert_eq!(
                parse(fixture.clone()),
                Message::Notification(Notification {
                    method: "n".into(),
                    params,
                }),
                "{fixture} parses with its params",
            );
        }
    }

    /// `"params": null` means the same as no params.
    #[test]
    fn null_params_read_as_absent() {
        assert_eq!(
            parse(json!({"method": "n", "params": null})),
            Message::Notification(Notification {
                method: "n".into(),
                params: None,
            }),
            "null params read as absent",
        );
    }

    /// A call with a null id cannot be answered, so it is a notification.
    #[test]
    fn method_with_null_id_is_a_notification() {
        assert!(
            matches!(
                parse(json!({"id": null, "method": "n"})),
                Message::Notification(_)
            ),
            "a method with a null id is a notification",
        );
    }

    /// `()` params (`shutdown`, `exit`) leave `params` off the wire.
    #[test]
    fn null_params_are_omitted_on_output() {
        let shutdown = Message::Request(Request::new::<lsp_types::request::Shutdown>(
            Id::Number(2),
            (),
        ));
        assert_eq!(
            wire(&shutdown),
            json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown"}),
            "shutdown carries no params",
        );
        let exit = Message::Notification(Notification::new::<lsp_types::notification::Exit>(()));
        assert_eq!(
            wire(&exit),
            json!({"jsonrpc": "2.0", "method": "exit"}),
            "exit carries no params",
        );
    }

    /// A success reply must carry `result`, so a void one sends `"result": null`.
    #[test]
    fn response_always_carries_result_even_when_null() {
        let reply = Message::Response(Response::ok(Id::Number(3), ()));
        assert_eq!(
            wire(&reply),
            json!({"jsonrpc": "2.0", "id": 3, "result": null}),
            "a void success carries a null result",
        );
    }

    /// An error reply carries `error`, never `result`, and a missing id as `null`.
    #[test]
    fn error_response_carries_error_and_no_result() {
        let reply = Message::Response(Response::error(
            None,
            Error {
                code: -32601,
                message: "x".into(),
                data: None,
            },
        ));
        let wire = wire(&reply);
        assert_eq!(
            wire,
            json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32601, "message": "x"}}),
            "an error reply carries the error and a null id",
        );
        assert!(wire.get("result").is_none(), "an error reply has no result");
    }

    /// Everything the envelope writes, it reads back unchanged.
    #[test]
    fn messages_round_trip_through_json() {
        let messages = [
            Message::Request(Request {
                id: Id::String("req-1".into()),
                method: "textDocument/hover".into(),
                params: Some(json!({"position": {"line": 1, "character": 2}})),
            }),
            Message::Request(Request::new::<lsp_types::request::Shutdown>(
                Id::Number(9),
                (),
            )),
            Message::Notification(Notification::new::<lsp_types::notification::Initialized>(
                lsp_types::InitializedParams {},
            )),
            Message::Response(Response::ok(Id::Number(4), json!({"items": []}))),
            Message::Response(Response::error(
                Some(Id::Number(5)),
                Error {
                    code: -32801,
                    message: "content modified".into(),
                    data: Some(json!({"retry": true})),
                },
            )),
        ];
        for message in messages {
            assert_eq!(parse(wire(&message)), message, "{message:?} round-trips");
        }
    }

    /// Hosts move messages across threads and keep copies for logging.
    #[test]
    fn message_is_clone_debug_and_send() {
        fn assert_traits<T: Clone + fmt::Debug + Send>() {}
        assert_traits::<Message>();
    }
}
