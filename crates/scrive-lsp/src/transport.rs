//! The bridges' common parts: a connection's outgoing queue, what a bridge reports, and how the
//! client reaches a bridge's worker.

#[cfg(not(target_family = "wasm"))]
pub(crate) mod frame;
mod generation;
#[cfg(any(
    not(target_family = "wasm"),
    all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
))]
pub(crate) mod lifecycle;
pub(crate) mod memory;
#[cfg(not(target_family = "wasm"))]
mod reader;
#[cfg(any(
    not(target_family = "wasm"),
    all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
))]
mod settings;
#[cfg(not(target_family = "wasm"))]
mod socket;
#[cfg(not(target_family = "wasm"))]
pub(crate) mod stdio;
#[cfg(all(test, not(target_family = "wasm")))]
pub(crate) mod tap;
#[cfg(not(target_family = "wasm"))]
pub(crate) mod tcp;
#[cfg(all(
    feature = "websocket",
    any(
        not(target_family = "wasm"),
        all(target_arch = "wasm32", target_os = "unknown")
    )
))]
pub(crate) mod websocket;
#[cfg(not(target_family = "wasm"))]
mod writer;

#[cfg(any(
    not(target_family = "wasm"),
    all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
))]
use core::pin::Pin;
#[cfg(any(
    not(target_family = "wasm"),
    all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
))]
use std::io;
#[cfg(not(target_family = "wasm"))]
use std::sync::mpsc;
use std::sync::Arc;
#[cfg(not(target_family = "wasm"))]
use std::thread;
use std::task::{Context, Poll};

pub(crate) use generation::Generation;
#[cfg(any(
    not(target_family = "wasm"),
    all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
))]
pub(crate) use lifecycle::{Handshake, Lifecycle};
#[cfg(any(
    not(target_family = "wasm"),
    all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
))]
pub(crate) use settings::Settings;
#[cfg(not(target_family = "wasm"))]
pub(crate) use writer::Writer;

use crate::client;
#[cfg(any(
    not(target_family = "wasm"),
    all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
))]
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
    /// A worker bridge's writer thread, on a pipe or a socket.
    #[cfg(not(target_family = "wasm"))]
    Stream(Writer),
    /// A WebSocket connection's I/O thread, which `waker` wakes.
    #[cfg(all(feature = "websocket", not(target_family = "wasm")))]
    Websocket {
        writer: Writer,
        waker: Arc<mio::Waker>,
    },
    /// A browser WebSocket's queue, which a task drains once the socket is open.
    #[cfg(all(feature = "websocket", target_arch = "wasm32", target_os = "unknown"))]
    Browser(futures_channel::mpsc::UnboundedSender<Arc<[u8]>>),
}

/// What a bridge reports. Protocol traffic names the connection it came from, so the client can
/// drop what a dead connection still delivers.
#[derive(Clone, Debug)]
pub(crate) enum Event {
    /// One JSON-RPC message from the server, as received on connection `generation`.
    Message {
        generation: Generation,
        body: Arc<[u8]>,
    },
    /// The bridge is done for good.
    Stopped(client::Reason),
    /// Lines the server wrote outside the protocol: one read's stderr lines, or the noise between
    /// two frames on stdout or a socket. Never dropped: a dead server's last words are what a log is for.
    #[cfg(any(
        not(target_family = "wasm"),
        all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
    ))]
    Log(Arc<[log::Entry]>),
    /// A frame connection `generation` dropped.
    #[cfg(not(target_family = "wasm"))]
    Error {
        generation: Generation,
        error: client::Error,
    },
    /// The connection before `generation` ended; `generation` is the one a reconnect creates.
    #[cfg(any(
        not(target_family = "wasm"),
        all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
    ))]
    Lost {
        generation: Generation,
        reason: client::Reason,
    },
    /// An attempt to bring up connection `generation` failed; the next follows after a backoff.
    #[cfg(any(
        not(target_family = "wasm"),
        all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
    ))]
    Attempting {
        generation: Generation,
        failure: Arc<io::Error>,
    },
    /// Connection `generation` is up, and `link` is its outgoing queue.
    #[cfg(any(
        not(target_family = "wasm"),
        all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
    ))]
    Reconnected { generation: Generation, link: Link },
}

/// A bridge's incoming side, polled by [`client::Events`].
pub(crate) enum Inbound {
    /// The memory bridge.
    Memory(memory::Inbox),
    /// The worker bridges' events, fed by their threads or, in a browser, their callbacks.
    #[cfg(any(
        not(target_family = "wasm"),
        all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
    ))]
    Channel(futures_channel::mpsc::UnboundedReceiver<Event>),
}

/// How the client reaches the worker behind its connection.
#[derive(Debug)]
pub(crate) enum Control {
    /// No worker: the client ends the connection itself.
    Memory,
    /// The stdio bridge's supervisor.
    #[cfg(not(target_family = "wasm"))]
    Stdio(Handle),
    /// The worker of a TCP bridge that dials its server.
    #[cfg(not(target_family = "wasm"))]
    Tcp(Handle),
    /// The worker of a TCP bridge that accepted its server once.
    #[cfg(not(target_family = "wasm"))]
    Listen(Handle),
    /// The worker of a WebSocket bridge.
    #[cfg(all(feature = "websocket", not(target_family = "wasm")))]
    Websocket(Handle),
    /// The task of a browser WebSocket bridge.
    #[cfg(all(feature = "websocket", target_arch = "wasm32", target_os = "unknown"))]
    Browser(websocket::Control),
}

/// The channel a worker bridge's threads report on.
#[cfg(any(
    not(target_family = "wasm"),
    all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
))]
pub(crate) type Feed = futures_channel::mpsc::UnboundedSender<Event>;

/// The client's line to a worker bridge's worker.
#[cfg(not(target_family = "wasm"))]
#[derive(Debug)]
pub(crate) struct Handle {
    notices: mpsc::Sender<Notice>,
}

/// What a worker is told: by the client, and by the threads of one connection.
#[cfg(not(target_family = "wasm"))]
#[derive(Debug)]
pub(crate) enum Notice {
    /// From the client.
    Control(Lifecycle),
    /// A reader of connection `generation` reached EOF or stopped reading.
    Ended { generation: Generation, pipe: Pipe },
    /// The writer of connection `generation` could not write.
    WriteFailed(Generation),
    /// The hung-server guard of connection `generation` tripped.
    Backlog(Generation),
    /// The stream reader of connection `generation` saw the reply to the shutdown request.
    Replied(Generation),
}

/// The input of a connection a reader reads.
#[cfg(not(target_family = "wasm"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pipe {
    Stdout,
    Stderr,
    Socket,
}

/// A started worker bridge: what the client keeps, and the stream its traffic arrives on.
#[cfg(any(
    not(target_family = "wasm"),
    all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
))]
pub(crate) struct Started {
    pub(crate) link: Link,
    pub(crate) control: Control,
    pub(crate) inbound: Inbound,
}

impl Link {
    /// Queues `body`, one serialized JSON-RPC message, for the server.
    pub(crate) fn send(&self, body: Arc<[u8]>) {
        match self {
            Link::Memory(link) => link.send(&body),
            #[cfg(not(target_family = "wasm"))]
            Link::Stream(writer) => writer.send(body),
            #[cfg(all(feature = "websocket", not(target_family = "wasm")))]
            Link::Websocket { writer, waker } => {
                writer.send(body);
                // A thread that is gone reports its loss to the worker on its own.
                let _ = waker.wake();
            }
            // A closed queue means the socket is gone.
            #[cfg(all(feature = "websocket", target_arch = "wasm32", target_os = "unknown"))]
            Link::Browser(sender) => {
                let _ = sender.unbounded_send(body);
            }
        }
    }

    /// Closes the server's input once everything queued before this call is written. Memory's
    /// end closes when its sender drops instead.
    #[cfg(any(
        not(target_family = "wasm"),
        all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
    ))]
    pub(crate) fn close(&self) {
        match self {
            Link::Memory(_) => {}
            #[cfg(not(target_family = "wasm"))]
            Link::Stream(writer) => writer.close(),
            #[cfg(all(feature = "websocket", not(target_family = "wasm")))]
            Link::Websocket { writer, waker } => {
                writer.close();
                let _ = waker.wake();
            }
            // The drain task sends the close frame once it has sent everything queued before.
            #[cfg(all(feature = "websocket", target_arch = "wasm32", target_os = "unknown"))]
            Link::Browser(sender) => sender.close_channel(),
        }
    }
}

impl Inbound {
    /// The bridge's next event. A bridge never ends without a [`Event::Stopped`].
    pub(crate) fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Event> {
        match self {
            Inbound::Memory(inbox) => inbox.poll_next(cx),
            #[cfg(any(
                not(target_family = "wasm"),
                all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
            ))]
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
    #[cfg(any(
        not(target_family = "wasm"),
        all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
    ))]
    pub(crate) fn send(&self, lifecycle: Lifecycle) {
        match self {
            Self::Memory => {}
            #[cfg(not(target_family = "wasm"))]
            Self::Stdio(handle) | Self::Tcp(handle) | Self::Listen(handle) => {
                handle.send(lifecycle);
            }
            #[cfg(all(feature = "websocket", not(target_family = "wasm")))]
            Self::Websocket(handle) => handle.send(lifecycle),
            #[cfg(all(feature = "websocket", target_arch = "wasm32", target_os = "unknown"))]
            Self::Browser(control) => control.send(lifecycle),
        }
    }
}

#[cfg(not(target_family = "wasm"))]
impl Handle {
    /// Tells the worker what the client decided. Ignored once the worker has stopped.
    pub(crate) fn send(&self, lifecycle: Lifecycle) {
        let _ = self.notices.send(Notice::Control(lifecycle));
    }
}

#[cfg(not(target_family = "wasm"))]
impl Pipe {
    /// The log entry for a line of non-LSP text read from this input.
    fn noise(self, text: String) -> log::Entry {
        match self {
            Pipe::Stdout => log::Entry::stdout(text),
            Pipe::Stderr => log::Entry::stderr(text),
            Pipe::Socket => log::Entry::socket(lsp_types::MessageType::LOG, text),
        }
    }
}

/// Runs `body` on a new thread called `name`.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn start(name: String, body: impl FnOnce() + Send + 'static) -> io::Result<()> {
    thread::Builder::new().name(name).spawn(body).map(drop)
}
