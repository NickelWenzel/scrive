//! The bridges' common parts: a connection's outgoing queue, and what a bridge reports.

pub(crate) mod memory;

use std::sync::Arc;
use std::task::{Context, Poll};

use crate::client;

/// The `id` of the `shutdown` request the client's side sends. Session ids are numbers, so its
/// reply falls through the session's pending table unrouted.
pub(crate) const SHUTDOWN_ID: &str = "scrive-lsp/shutdown";

/// One connection's outgoing queue. Sending never blocks; a message for a connection that has
/// gone is dropped.
#[derive(Clone, Debug)]
pub(crate) enum Link {
    /// The memory bridge.
    Memory(memory::Link),
}

/// What a bridge reports.
#[derive(Clone, Debug)]
pub(crate) enum Event {
    /// One JSON-RPC message from the server, as received.
    Message(Arc<[u8]>),
    /// The connection is over.
    Stopped(client::Reason),
}

/// A bridge's incoming side, polled by [`client::Events`].
pub(crate) enum Inbound {
    /// The memory bridge.
    Memory(memory::Inbox),
}

impl Link {
    /// Queues `body`, one serialized JSON-RPC message, for the server.
    pub(crate) fn send(&self, body: Arc<[u8]>) {
        match self {
            Link::Memory(link) => link.send(&body),
        }
    }
}

impl Inbound {
    /// The bridge's next event. A bridge never ends without a [`Event::Stopped`].
    pub(crate) fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Event> {
        match self {
            Inbound::Memory(inbox) => inbox.poll_next(cx),
        }
    }
}
