//! What a bridge's worker learns from the client about the protocol conversation.

use std::sync::Arc;

use crate::{client, message, transport};

/// A control message from the client to a bridge's worker.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Lifecycle {
    /// The client is done with the server. A server whose `initialize` was never answered gets no
    /// `shutdown` request (lsp-server treats any request before `initialize` as a protocol
    /// error), only `exit`.
    Shutdown { handshake: Handshake },
}

/// Whether the server answered `initialize`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Handshake {
    Done,
    Pending,
}

/// The serialized `shutdown` request, with the id `transport::SHUTDOWN_ID`.
pub(crate) fn shutdown() -> Arc<[u8]> {
    let request = message::Request::new::<lsp_types::request::Shutdown>(
        message::Id::String(transport::SHUTDOWN_ID.to_owned()),
        (),
    );
    client::serialize(&message::Message::Request(request))
}

/// The serialized `exit` notification.
pub(crate) fn exit() -> Arc<[u8]> {
    let notification = message::Notification::new::<lsp_types::notification::Exit>(());
    client::serialize(&message::Message::Notification(notification))
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    fn parsed(body: &[u8]) -> Value {
        serde_json::from_slice(body).expect("the body is JSON")
    }

    /// `shutdown` carries the reserved string id, so its reply is never routed; `exit` has none.
    #[test]
    fn the_shutdown_body_carries_the_reserved_string_id() {
        let shutdown = parsed(&shutdown());
        assert_eq!(shutdown["id"], transport::SHUTDOWN_ID, "the reserved id");
        assert_eq!(shutdown["method"], "shutdown", "the method");
        let exit = parsed(&exit());
        assert_eq!(exit["method"], "exit", "the method");
        assert!(exit.get("id").is_none(), "a notification has no id");
    }
}
