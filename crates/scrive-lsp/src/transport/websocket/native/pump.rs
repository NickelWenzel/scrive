//! A native WebSocket connection's I/O thread: one mio readiness loop that writes the queue,
//! reads the server and answers its pings.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use mio::{Events, Interest, Poll, Token};
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::protocol::CloseFrame;
use tungstenite::{Message, WebSocket};

use super::stream::Stream;
use crate::transport::writer::{Item, Outgoing};
use crate::transport::{self, lifecycle, Feed, Generation, Notice, Pipe};
use crate::{client, log};

pub(super) const SOCKET: Token = Token(0);
pub(super) const WAKE: Token = Token(1);
/// Messages read per turn before the loop serves writes and commands again.
const READ_BUDGET: usize = 256;

/// What the worker asks of a connection's I/O thread.
pub(super) enum Command {
    /// Drop the socket at once.
    Abort,
}

/// One connection's I/O thread.
pub(super) struct Pump {
    pub(super) socket: WebSocket<Stream>,
    pub(super) poll: Poll,
    pub(super) outgoing: Outgoing,
    pub(super) commands: mpsc::Receiver<Command>,
    pub(super) generation: Generation,
    pub(super) events: Feed,
    pub(super) notices: mpsc::Sender<Notice>,
    /// Tells the pump to look for the shutdown reply.
    pub(super) closing: Arc<AtomicBool>,
    /// How far the WebSocket close has got.
    pub(super) close: Close,
    /// What the socket is registered for.
    pub(super) interest: Interest,
}

/// How far the WebSocket close has got.
pub(super) enum Close {
    /// Not asked for.
    Open,
    /// The queue reached its close; every message ahead of it is written.
    Requested,
    /// The close frame is queued.
    Sent,
}

/// Whether tungstenite's buffer reached the socket.
enum Flush {
    Done,
    Blocked,
}

/// Whether the read phase drained the socket.
enum Drain {
    Idle,
    Budget,
}

/// Why a connection's I/O thread stopped.
enum End {
    /// The connection is gone: a close, EOF, or an error.
    Lost,
    /// The server sent a message of `length` bytes, more than the client reads.
    Oversize(u64),
    /// The worker let go of the connection.
    Aborted,
}

impl Pump {
    /// Runs the connection until it ends, then tells the worker, unless the worker ended it.
    pub(super) fn run(mut self) {
        let end = match self.poll.registry().register(
            self.socket.get_mut().socket(),
            SOCKET,
            self.interest,
        ) {
            Ok(()) => self.pump(),
            Err(_) => End::Lost,
        };
        if let End::Oversize(length) = end {
            self.report(transport::Event::Error {
                generation: self.generation,
                error: client::Error::Oversized { length },
            });
        }
        match end {
            End::Lost | End::Oversize(_) => {
                let _ = self.notices.send(Notice::Ended {
                    generation: self.generation,
                    pipe: Pipe::Socket,
                });
            }
            End::Aborted => {}
        }
    }

    fn pump(&mut self) -> End {
        let mut events = Events::with_capacity(4);
        loop {
            let written = match self.write() {
                Ok(flush) => flush,
                Err(end) => return end,
            };
            if let Err(end) = self.command() {
                return end;
            }
            let drained = match self.read() {
                Ok(drain) => drain,
                Err(end) => return end,
            };
            // Reads queue pongs and the close reply; they go out before the loop sleeps.
            let flushed = match self.flush() {
                Ok(flush) => flush,
                Err(end) => return end,
            };
            if self.register(&flushed).is_err() {
                return End::Lost;
            }
            // Edge-triggered: a read stopped by the budget may leave data no new edge announces,
            // and messages behind a write that blocked and then flushed get no wake either.
            let again = matches!(drained, Drain::Budget)
                || matches!((written, flushed), (Flush::Blocked, Flush::Done));
            match self.poll.poll(&mut events, again.then_some(Duration::ZERO)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return End::Lost,
            }
        }
    }

    /// Feeds queued messages to tungstenite while its buffer reaches the socket, so the
    /// backlog counts every byte not yet handed over.
    fn write(&mut self) -> Result<Flush, End> {
        loop {
            if let Flush::Blocked = self.flush()? {
                return Ok(Flush::Blocked);
            }
            if let Close::Requested | Close::Sent = self.close {
                return Ok(Flush::Done);
            }
            let body = match self.outgoing.next() {
                Some(Item::Body(body)) => body,
                Some(Item::Close) => {
                    self.close = Close::Requested;
                    return Ok(Flush::Done);
                }
                None => return Ok(Flush::Done),
            };
            let length = body.len();
            let text = tungstenite::Utf8Bytes::try_from(tungstenite::Bytes::from_owner(body))
                .expect("the session serializes JSON, which is UTF-8");
            let written = self.socket.write(Message::Text(text));
            // A frame that would block is already in tungstenite's buffer.
            self.outgoing.written(length);
            match written {
                Ok(()) => {}
                Err(error) if would_block(&error) => return Ok(Flush::Blocked),
                Err(error) => return Err(End::from(error)),
            }
        }
    }

    fn flush(&mut self) -> Result<Flush, End> {
        match self.socket.flush() {
            Ok(()) => Ok(Flush::Done),
            Err(error) if would_block(&error) => Ok(Flush::Blocked),
            Err(error) => Err(End::from(error)),
        }
    }

    fn read(&mut self) -> Result<Drain, End> {
        for _ in 0..READ_BUDGET {
            match self.socket.read() {
                Ok(Message::Text(text)) => self.deliver(text.as_bytes()),
                Ok(Message::Binary(bytes)) => match std::str::from_utf8(&bytes) {
                    Ok(_) => self.deliver(&bytes),
                    Err(error) => self.log(
                        lsp_types::MessageType::WARNING,
                        format!(
                            "dropped a binary WebSocket frame that isn't UTF-8 ({} bytes): {error}",
                            bytes.len()
                        ),
                    ),
                },
                Ok(Message::Close(frame)) => self.closed(frame),
                Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => {}
                Err(error) if would_block(&error) => return Ok(Drain::Idle),
                Err(error) => return Err(End::from(error)),
            }
        }
        Ok(Drain::Budget)
    }

    /// Ends on `Abort`, and sends the close frame once it is due.
    fn command(&mut self) -> Result<(), End> {
        match self.commands.try_recv() {
            Ok(Command::Abort) | Err(mpsc::TryRecvError::Disconnected) => return Err(End::Aborted),
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if let Close::Requested = self.close {
            self.close = Close::Sent;
            return match self.socket.close(None) {
                Ok(()) => Ok(()),
                Err(error) if would_block(&error) => Ok(()),
                Err(error) => Err(End::from(error)),
            };
        }
        Ok(())
    }

    /// Asks for writability exactly while the last flush blocked. Re-registering re-arms the
    /// edge, so a socket that is already writable fires at once.
    fn register(&mut self, flushed: &Flush) -> io::Result<()> {
        let interest = match flushed {
            Flush::Blocked => Interest::READABLE | Interest::WRITABLE,
            Flush::Done => Interest::READABLE,
        };
        if interest == self.interest {
            return Ok(());
        }
        self.interest = interest;
        self.poll
            .registry()
            .reregister(self.socket.get_mut().socket(), SOCKET, interest)
    }

    /// Forwards one message, unless it is the shutdown reply the worker waits for.
    fn deliver(&self, body: &[u8]) {
        if self.closing.load(Ordering::Relaxed) && lifecycle::is_shutdown_reply(body) {
            let _ = self.notices.send(Notice::Replied(self.generation));
            return;
        }
        self.report(transport::Event::Message {
            generation: self.generation,
            body: Arc::from(body),
        });
    }

    /// Logs a close the server started; tungstenite queues the reply. The answer to the client's
    /// own close says nothing new.
    fn closed(&self, frame: Option<CloseFrame>) {
        if let Close::Sent = self.close {
            return;
        }
        match frame {
            Some(CloseFrame { code, reason }) => {
                let level = if code == CloseCode::Normal {
                    lsp_types::MessageType::INFO
                } else {
                    lsp_types::MessageType::WARNING
                };
                let code = u16::from(code);
                self.log(
                    level,
                    format!("WebSocket closed by the server: {code} {reason}"),
                );
            }
            None => self.log(
                lsp_types::MessageType::WARNING,
                "WebSocket closed by the server without a code".to_owned(),
            ),
        }
    }

    fn log(&self, level: lsp_types::MessageType, text: String) {
        self.report(transport::Event::Log(Arc::from([log::Entry::socket(
            level, text,
        )])));
    }

    fn report(&self, event: transport::Event) {
        // `Events` was dropped: nobody is left to tell.
        let _ = self.events.unbounded_send(event);
    }
}

impl From<tungstenite::Error> for End {
    fn from(error: tungstenite::Error) -> Self {
        match error {
            tungstenite::Error::Capacity(tungstenite::error::CapacityError::MessageTooLong {
                size,
                ..
            }) => End::Oversize(u64::try_from(size).unwrap_or(u64::MAX)),
            _ => End::Lost,
        }
    }
}

fn would_block(error: &tungstenite::Error) -> bool {
    matches!(error, tungstenite::Error::Io(error) if error.kind() == io::ErrorKind::WouldBlock)
}
