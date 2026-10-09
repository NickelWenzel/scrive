//! One item of a client's [`Events`](super::Events) stream.

use core::fmt;

use super::{Id, Status};
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
    /// A status the client decided outside `receive`: a restart, or memory's own stops.
    Status(Status),
}

/// A summary only: iced's `debug` feature formats every message, and a payload can be megabytes.
impl fmt::Debug for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut event = f.debug_struct("Event");
        event.field("client", &self.client);
        match &self.payload {
            Payload::Transport(transport::Event::Message { generation, body }) => event
                .field("direction", &trace::Direction::Incoming)
                .field("generation", generation)
                .field("bytes", &body.len()),
            Payload::Sent(entry) => event
                .field("direction", &trace::Direction::Outgoing)
                .field("method", &entry.method())
                .field("bytes", &entry.json().len()),
            Payload::Transport(transport::Event::Stopped(reason)) => event.field("stopped", reason),
            Payload::Status(status) => event.field("status", status),
            #[cfg(not(target_family = "wasm"))]
            Payload::Transport(transport::Event::Log(entries)) => event
                .field("log", &entries.first().map(log::Entry::source))
                .field("lines", &entries.len()),
            #[cfg(not(target_family = "wasm"))]
            Payload::Transport(transport::Event::Error { generation, error }) => event
                .field("generation", generation)
                .field("error", error),
            #[cfg(not(target_family = "wasm"))]
            Payload::Transport(transport::Event::Lost { generation, reason }) => event
                .field("lost", reason)
                .field("generation", generation),
            #[cfg(not(target_family = "wasm"))]
            Payload::Transport(transport::Event::Attempting {
                generation,
                failure,
            }) => event
                .field("attempt_failed", &failure.kind())
                .field("generation", generation),
            #[cfg(not(target_family = "wasm"))]
            Payload::Transport(transport::Event::Reconnected { generation, .. }) => {
                event.field("reconnected", generation)
            }
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

    /// Whether the stream ends after this event: a bridge's own `Stopped`, or a stop the client
    /// queued for a bridge that cannot restart. A stop the client decides on a loss is returned
    /// by `receive` instead, and the stream runs on for
    /// [`Client::restart`](super::Client::restart).
    pub(crate) fn stops(&self) -> bool {
        match &self.payload {
            Payload::Transport(transport::Event::Stopped(_))
            | Payload::Status(Status::Stopped(_)) => true,
            Payload::Transport(transport::Event::Message { .. })
            | Payload::Sent(_)
            | Payload::Status(Status::Starting | Status::Running | Status::Restarting { .. }) => {
                false
            }
            #[cfg(not(target_family = "wasm"))]
            Payload::Transport(
                transport::Event::Log(_)
                | transport::Event::Error { .. }
                | transport::Event::Lost { .. }
                | transport::Event::Attempting { .. }
                | transport::Event::Reconnected { .. },
            ) => false,
        }
    }
}
