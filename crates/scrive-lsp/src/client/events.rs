//! The stream of everything that reaches a client: the server's messages, the client's own
//! traces, and its stops.

use core::pin::Pin;
use std::task::{Context, Poll};

use futures_core::Stream;

use super::{event, Event, Id};
use crate::transport;

/// Everything that reaches a client, as a stream the host runs, e.g. with
/// `Task::run(events, Message::Lsp)`. Each item goes to
/// [`Client::receive`](super::Client::receive). It yields the client's own items (outgoing
/// traces, its stops) before the bridge's. Until the host runs it, no reply reaches the client.
///
/// It ends when the client is dropped, after `Stopped(Shutdown)`, and after any stop of a
/// bridge that cannot restart. A server process that is lost and not started again leaves the
/// stream running.
#[must_use = "a client whose Events nobody runs never sees a reply"]
pub struct Events {
    client: Id,
    local: futures_channel::mpsc::UnboundedReceiver<Event>,
    inbound: transport::Inbound,
    ended: bool,
}

impl Stream for Events {
    type Item = Event;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Event>> {
        let this = self.get_mut();
        if this.ended {
            return Poll::Ready(None);
        }
        match Pin::new(&mut this.local).poll_next(cx) {
            Poll::Ready(Some(event)) => {
                this.ended = event.stops();
                return Poll::Ready(Some(event));
            }
            // The client was dropped: nobody is left to receive.
            Poll::Ready(None) => {
                this.ended = true;
                return Poll::Ready(None);
            }
            Poll::Pending => {}
        }
        match this.inbound.poll_next(cx) {
            Poll::Ready(event) => {
                let event = Event::new(this.client, event::Payload::Transport(event));
                this.ended = event.stops();
                Poll::Ready(Some(event))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Events {
    pub(crate) fn new(
        client: Id,
        local: futures_channel::mpsc::UnboundedReceiver<Event>,
        inbound: transport::Inbound,
    ) -> Self {
        Self {
            client,
            local,
            inbound,
            ended: false,
        }
    }
}
