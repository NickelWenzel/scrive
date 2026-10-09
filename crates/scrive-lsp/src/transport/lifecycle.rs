//! What a bridge's worker learns from the client about the protocol conversation, and the client
//! half of the LSP shutdown handshake it runs.

use std::time::Duration;
#[cfg(not(target_family = "wasm"))]
use std::time::Instant;

#[cfg(all(feature = "websocket", target_arch = "wasm32", target_os = "unknown"))]
use wasmtimer::std::Instant;

use super::Generation;
use crate::{client, message, transport};

/// A control message from the client to a bridge's worker. Each names the connection it is
/// about, and the worker ignores one that no longer applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lifecycle {
    /// The client is done with the server: run the shutdown sequence, or abort a backoff, and
    /// stop. `handshake` describes connection `generation`; any other connection is treated as
    /// `Pending`.
    Shutdown {
        generation: Generation,
        handshake: Handshake,
    },
    /// Connection `generation` completed `initialize`: cancel its deadline, reset the backoff.
    Handshaken(Generation),
    /// Bring up connection `generation` after the backoff; answers the loss that announced it.
    Reconnect(Generation),
    /// Don't reconnect: tear connection `generation` down if it is up, and wait for `Restart`
    /// or `Shutdown`. The worker moves on to the next generation.
    Stop(Generation),
    /// Tear down whatever is up and bring up connection `generation` now, with no backoff.
    Restart(Generation),
}

/// Whether the server answered `initialize`, which decides how it is shut down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Handshake {
    Done,
    Pending,
}

/// The client half of the LSP shutdown handshake (LSP 3.17 §shutdown, §exit). It owns the
/// deadlines; the worker owns the process or socket and acts on [`Next`].
#[derive(Debug)]
pub(crate) struct Sequence {
    step: Step,
}

#[derive(Debug)]
enum Step {
    /// `shutdown` is out; waiting for its reply until `until`.
    Reply { until: Instant },
    /// `exit` is out and the write half is closed; waiting for the server to go until `until`.
    Exit { until: Instant },
}

/// What the worker does after a step of the sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Next {
    /// Wait for the server until [`Sequence::until`].
    Wait,
    /// The server had its time: kill it.
    Kill,
}

#[derive(serde::Deserialize)]
struct Probe {
    id: Option<serde_json::Value>,
    method: Option<serde::de::IgnoredAny>,
}

impl Sequence {
    /// Sends `shutdown` after a handshake, or `exit` and the close without one: a server whose
    /// `initialize` was never answered treats any other request as a protocol error.
    pub(crate) fn begin(
        link: &transport::Link,
        handshake: Handshake,
        grace: Duration,
        now: Instant,
    ) -> Self {
        let step = match handshake {
            Handshake::Done => {
                link.send(shutdown());
                Step::Reply { until: now + grace }
            }
            Handshake::Pending => leave(link, grace, now),
        };
        Self { step }
    }

    /// The reply arrived: `exit`, close, and wait for the server to go.
    pub(crate) fn replied(
        &mut self,
        link: &transport::Link,
        grace: Duration,
        now: Instant,
    ) -> Next {
        if let Step::Reply { .. } = self.step {
            self.step = leave(link, grace, now);
        }
        Next::Wait
    }

    /// A deadline passed: a missing reply moves on to `exit`, a server that won't go is killed.
    pub(crate) fn expired(
        &mut self,
        link: &transport::Link,
        grace: Duration,
        now: Instant,
    ) -> Next {
        match self.step {
            Step::Reply { .. } => {
                self.step = leave(link, grace, now);
                Next::Wait
            }
            Step::Exit { .. } => Next::Kill,
        }
    }

    /// When the current step's deadline passes.
    pub(crate) fn until(&self) -> Instant {
        match self.step {
            Step::Reply { until } | Step::Exit { until } => until,
        }
    }
}

/// Whether `body` is the reply to the request with the reserved
/// [`SHUTDOWN_ID`](transport::SHUTDOWN_ID). Parses only `id` and `method`.
pub(crate) fn is_shutdown_reply(body: &[u8]) -> bool {
    serde_json::from_slice::<Probe>(body).is_ok_and(|probe| {
        probe.method.is_none()
            && probe.id.as_ref().and_then(serde_json::Value::as_str) == Some(transport::SHUTDOWN_ID)
    })
}

/// Sends `exit`, closes the server's input behind it, and waits for the server to go.
fn leave(link: &transport::Link, grace: Duration, now: Instant) -> Step {
    let notification = message::Notification::new::<lsp_types::notification::Exit>(());
    link.send(client::serialize(&message::Message::Notification(notification)));
    link.close();
    Step::Exit { until: now + grace }
}

fn shutdown() -> std::sync::Arc<[u8]> {
    let request = message::Request::new::<lsp_types::request::Shutdown>(
        message::Id::String(transport::SHUTDOWN_ID.to_owned()),
        (),
    );
    client::serialize(&message::Message::Request(request))
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::transport::tap;

    const GRACE: Duration = Duration::from_secs(2);

    /// The methods queued on `tap` since the last call, `close` for the close.
    fn queued(tap: &tap::Queue) -> Vec<String> {
        tap.items()
            .into_iter()
            .map(|item| match item {
                Some(body) => body["method"].as_str().unwrap_or("reply").to_owned(),
                None => "close".to_owned(),
            })
            .collect()
    }

    /// The reply to the reserved id is recognised, whatever its result.
    #[test]
    fn shutdown_reply_is_recognised_by_its_reserved_id() {
        assert!(
            is_shutdown_reply(br#"{"jsonrpc":"2.0","id":"scrive-lsp/shutdown","result":null}"#),
            "a result"
        );
        assert!(
            is_shutdown_reply(
                br#"{"id":"scrive-lsp/shutdown","error":{"code":-32600,"message":"no"}}"#
            ),
            "an error"
        );
    }

    /// Other ids are the session's business.
    #[test]
    fn numeric_and_other_string_ids_are_not_the_shutdown_reply() {
        assert!(!is_shutdown_reply(br#"{"id":1,"result":null}"#), "a number");
        assert!(!is_shutdown_reply(br#"{"id":"other","result":null}"#), "another string");
        assert!(!is_shutdown_reply(b"not json"), "garbage");
    }

    /// A server request that happens to use the reserved id is not a reply.
    #[test]
    fn a_request_with_the_reserved_id_is_not_a_reply() {
        let request = json!({"id": transport::SHUTDOWN_ID, "method": "workspace/configuration"});
        assert!(!is_shutdown_reply(request.to_string().as_bytes()), "it has a method");
    }

    /// After the handshake, `shutdown` goes out first; its reply sends `exit` and the close.
    #[test]
    fn sequence_sends_exit_after_the_reply() {
        let (link, tap) = tap::Queue::link(Generation::FIRST);
        let now = Instant::now();
        let mut sequence = Sequence::begin(&link, Handshake::Done, GRACE, now);
        let items = tap.items();
        let [Some(shutdown)] = items.as_slice() else {
            panic!("expected the shutdown request, got {items:?}")
        };
        assert_eq!(shutdown["method"], "shutdown", "the request");
        assert_eq!(shutdown["id"], transport::SHUTDOWN_ID, "with the reserved id");
        assert_eq!(sequence.until(), now + GRACE, "the reply has one grace period");
        let later = now + Duration::from_millis(10);
        assert_eq!(sequence.replied(&link, GRACE, later), Next::Wait, "then the exit wait");
        assert_eq!(queued(&tap), ["exit", "close"], "exit, then the close");
        assert_eq!(sequence.until(), later + GRACE, "the exit has its own grace period");
    }

    /// A reply that doesn't come in time is given up on, and `exit` goes out anyway.
    #[test]
    fn sequence_moves_on_to_exit_when_the_reply_is_late() {
        let (link, tap) = tap::Queue::link(Generation::FIRST);
        let now = Instant::now();
        let mut sequence = Sequence::begin(&link, Handshake::Done, GRACE, now);
        let _ = tap.items();
        assert_eq!(
            sequence.expired(&link, GRACE, now + GRACE),
            Next::Wait,
            "the exit wait follows"
        );
        assert_eq!(queued(&tap), ["exit", "close"], "exit, then the close");
        assert_eq!(
            sequence.replied(&link, GRACE, now + GRACE),
            Next::Wait,
            "a late reply changes nothing"
        );
        assert!(tap.items().is_empty(), "nothing is sent twice");
    }

    /// A server still there after the exit grace is killed.
    #[test]
    fn sequence_kills_after_the_exit_grace() {
        let (link, _tap) = tap::Queue::link(Generation::FIRST);
        let now = Instant::now();
        let mut sequence = Sequence::begin(&link, Handshake::Done, GRACE, now);
        let _ = sequence.expired(&link, GRACE, now + GRACE);
        assert_eq!(
            sequence.expired(&link, GRACE, now + 2 * GRACE),
            Next::Kill,
            "the second deadline kills"
        );
    }

    /// Before the handshake, only `exit` and the close go out.
    #[test]
    fn pending_handshake_skips_the_shutdown_request() {
        let (link, tap) = tap::Queue::link(Generation::FIRST);
        let mut sequence = Sequence::begin(&link, Handshake::Pending, GRACE, Instant::now());
        assert_eq!(queued(&tap), ["exit", "close"], "no shutdown request");
        assert_eq!(
            sequence.expired(&link, GRACE, sequence.until()),
            Next::Kill,
            "one grace period, then the kill"
        );
    }

    /// The close is queued behind `exit`'s body, so the server reads `exit` before EOF.
    #[test]
    fn the_close_follows_exit() {
        let (link, tap) = tap::Queue::link(Generation::FIRST);
        let _ = Sequence::begin(&link, Handshake::Pending, GRACE, Instant::now());
        let items = tap.items();
        let [Some(exit), None] = items.as_slice() else {
            panic!("expected exit then the close, got {items:?}")
        };
        assert_eq!(exit, &json!({"jsonrpc": "2.0", "method": "exit"}), "the exit body");
    }
}
