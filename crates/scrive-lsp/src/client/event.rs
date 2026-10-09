//! One item of a client's [`Events`](super::Events) stream.

use core::fmt;

use super::{Id, Reason};
use crate::{trace, transport};
#[cfg(not(target_family = "wasm"))]
use crate::log;

/// One item of a client's [`Events`](super::Events): hand it to
/// [`Client::receive`](super::Client::receive). Cheap to clone, and `Send` on every target, so it
/// can be an iced message.
#[derive(Clone)]
pub struct Event {
    client: Id,
    payload: Payload,
}

/// What an [`Event`] carries.
#[derive(Clone)]
pub(crate) enum Payload {
    /// What the bridge reported.
    Transport(transport::Event),
    /// A message the client sent, traced.
    Sent(trace::Entry),
    /// The client ended the connection itself: shutdown, or a failed `initialize`.
    Stopped(Reason),
}

/// A summary only: iced's `debug` feature formats every message, and a payload can be megabytes.
impl fmt::Debug for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut event = f.debug_struct("Event");
        event.field("client", &self.client);
        match &self.payload {
            Payload::Transport(transport::Event::Message(body)) => event
                .field("direction", &trace::Direction::Incoming)
                .field("bytes", &body.len()),
            Payload::Sent(entry) => event
                .field("direction", &trace::Direction::Outgoing)
                .field("method", &entry.method())
                .field("bytes", &entry.json().len()),
            Payload::Transport(transport::Event::Stopped(reason)) | Payload::Stopped(reason) => {
                event.field("stopped", reason)
            }
            #[cfg(not(target_family = "wasm"))]
            Payload::Transport(transport::Event::Log(entries)) => event
                .field("log", &entries.first().map(log::Entry::source))
                .field("lines", &entries.len()),
            #[cfg(not(target_family = "wasm"))]
            Payload::Transport(transport::Event::Error(error)) => event.field("error", error),
        };
        event.finish()
    }
}

impl Event {
    pub(crate) fn new(client: Id, payload: Payload) -> Self {
        Self { client, payload }
    }

    pub(crate) fn into_parts(self) -> (Id, Payload) {
        (self.client, self.payload)
    }

    /// Whether the stream ends after this event: after every stop.
    pub(crate) fn stops(&self) -> bool {
        match &self.payload {
            Payload::Transport(transport::Event::Stopped(_)) | Payload::Stopped(_) => true,
            Payload::Transport(transport::Event::Message(_)) | Payload::Sent(_) => false,
            #[cfg(not(target_family = "wasm"))]
            Payload::Transport(transport::Event::Log(_) | transport::Event::Error(_)) => false,
        }
    }
}
