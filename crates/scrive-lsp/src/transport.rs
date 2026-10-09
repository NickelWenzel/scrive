//! The bridges' common parts: a connection's outgoing queue, what a bridge reports, and how the
//! client reaches a bridge's worker.

#[cfg(not(target_family = "wasm"))]
pub(crate) mod frame;
#[cfg(not(target_family = "wasm"))]
pub(crate) mod lifecycle;
pub(crate) mod memory;
#[cfg(not(target_family = "wasm"))]
pub(crate) mod stdio;

#[cfg(not(target_family = "wasm"))]
use core::pin::Pin;
#[cfg(not(target_family = "wasm"))]
use std::io;
use std::sync::Arc;
use std::task::{Context, Poll};

#[cfg(not(target_family = "wasm"))]
pub(crate) use lifecycle::{Handshake, Lifecycle};

use crate::client;
#[cfg(not(target_family = "wasm"))]
use crate::log;

/// The `id` of the `shutdown` request the client's side sends. Session ids are numbers, so its
/// reply falls through the session's pending table unrouted.
pub(crate) const SHUTDOWN_ID: &str = "scrive-lsp/shutdown";

/// One connection's outgoing queue. Sending never blocks; a message for a connection that has
/// gone is dropped.
#[derive(Clone, Debug)]
pub(crate) enum Link {
    /// The memory bridge.
    Memory(memory::Link),
    /// The stdio bridge's writer thread.
    #[cfg(not(target_family = "wasm"))]
    Stdio(stdio::Writer),
}

/// What a bridge reports.
#[derive(Clone, Debug)]
pub(crate) enum Event {
    /// One JSON-RPC message from the server, as received.
    Message(Arc<[u8]>),
    /// The connection is over.
    Stopped(client::Reason),
    /// Lines the server wrote outside the protocol: one read's stderr lines, or the stdout noise
    /// between two frames.
    #[cfg(not(target_family = "wasm"))]
    Log(Arc<[log::Entry]>),
    /// A frame the bridge dropped.
    #[cfg(not(target_family = "wasm"))]
    Error(client::Error),
}

/// A bridge's incoming side, polled by [`client::Events`].
pub(crate) enum Inbound {
    /// The memory bridge.
    Memory(memory::Inbox),
    /// The worker bridges' events, fed by their threads.
    #[cfg(not(target_family = "wasm"))]
    Channel(futures_channel::mpsc::UnboundedReceiver<Event>),
}

/// How the client reaches the worker behind its connection.
#[derive(Debug)]
pub(crate) enum Control {
    /// No worker: the client ends the connection itself.
    Memory,
    /// The stdio bridge's supervisor.
    #[cfg(not(target_family = "wasm"))]
    Stdio(stdio::Control),
}

impl Link {
    /// Queues `body`, one serialized JSON-RPC message, for the server.
    pub(crate) fn send(&self, body: Arc<[u8]>) {
        match self {
            Link::Memory(link) => link.send(&body),
            #[cfg(not(target_family = "wasm"))]
            Link::Stdio(writer) => writer.send(body),
        }
    }

    /// Closes the server's input once everything queued before this call is written. Memory's
    /// end closes when its sender drops instead.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn close(&self) {
        match self {
            Link::Memory(_) => {}
            Link::Stdio(writer) => writer.close(),
        }
    }
}

impl Inbound {
    /// The bridge's next event. A bridge never ends without a [`Event::Stopped`].
    pub(crate) fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Event> {
        match self {
            Inbound::Memory(inbox) => inbox.poll_next(cx),
            #[cfg(not(target_family = "wasm"))]
            Inbound::Channel(receiver) => {
                match futures_core::Stream::poll_next(Pin::new(receiver), cx) {
                    Poll::Ready(Some(event)) => Poll::Ready(event),
                    // Every worker sends `Stopped` before it ends; this covers a worker that
                    // died without one.
                    Poll::Ready(None) => Poll::Ready(Event::Stopped(client::Reason::Failed(
                        Arc::new(io::Error::other("the bridge's worker ended")),
                    ))),
                    Poll::Pending => Poll::Pending,
                }
            }
        }
    }
}

impl Control {
    /// Tells the worker what the client decided. Memory has no worker, so it ignores it.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn send(&self, lifecycle: Lifecycle) {
        match self {
            Self::Memory => {}
            Self::Stdio(control) => control.send(lifecycle),
        }
    }
}
