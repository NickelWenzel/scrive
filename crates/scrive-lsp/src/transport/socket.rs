//! The worker of the bridges whose server is on a socket: it dials or accepts, carries one
//! connection at a time, and dials again when the client says so. The bridges differ only in what
//! they dial and in the threads that carry a connection.

use std::io;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use super::lifecycle::{self, Handshake, Lifecycle};
use super::settings::grow;
#[cfg(feature = "websocket")]
use super::websocket;
use super::writer::Outgoing;
use super::{tcp, Feed, Generation, Handle, Link, Notice, Settings, Started, Writer};
use crate::client::builder;
use crate::{client, transport};

// The wait after a failed first dial, growing ×1.3 up to `STEP_CAP`. A server started together
// with its client is often still binding its port; a 1 s first step would add a second to
// every such start.
const FIRST_STEP: Duration = Duration::from_millis(50);
const STEP_CAP: Duration = Duration::from_secs(1);
// How long a pending accept waits on the inbox between polls; it bounds accept latency only,
// since a notice wakes the wait at once.
const ACCEPT_POLL: Duration = Duration::from_millis(100);

/// Owns the connection across its generations: dials, tears down, dials again, and reports each
/// loss for the client to decide on.
struct Worker {
    mode: Mode,
    /// The connection the worker is on. It runs ahead of the client by at most one, after a
    /// loss, and then waits for the client's answer.
    generation: Generation,
    state: State,
    /// Dials since the last handshake; indexes the backoff.
    retry: u32,
    settings: Settings,
    events: Feed,
    notices: mpsc::Receiver<Notice>,
    /// Handed to the threads of each new connection.
    sender: mpsc::Sender<Notice>,
}

/// How the worker comes by a connection.
enum Mode {
    /// It dials `endpoint`, and dials again after a loss. Before any connection, and after a
    /// restart, failed dials retry until `budget` has passed.
    Connect {
        endpoint: Endpoint,
        budget: Duration,
    },
    /// It accepts one connection, and that connection's loss ends it: a server that dialed in
    /// is started again by whoever started it.
    Listen,
}

enum State {
    /// Dialing until a connection is up or the budget runs out.
    Dialing(Dialing),
    /// Waiting for the server to dial in; `queue` is the first connection's.
    Accepting { listener: TcpListener, queue: Queue },
    /// The connection of the current generation is up.
    Live(Connection),
    /// The connection of `generation` is torn down; its reader gets until `until` to reach
    /// EOF before the loss is reported.
    Draining {
        generation: Generation,
        ended: bool,
        reason: client::Reason,
        until: Instant,
    },
    /// The loss is reported; waiting for the client's `Reconnect` or `Stop`.
    Waiting,
    /// Dialing again at `until`.
    Backoff { until: Instant },
    /// The client chose not to reconnect.
    Idle,
    /// Running the shutdown sequence on `connection`.
    Closing {
        connection: Connection,
        sequence: lifecycle::Sequence,
        ending: Ending,
    },
}

/// The first dials of a generation, retried on their own schedule.
struct Dialing {
    /// When the dials give up.
    deadline: Instant,
    /// When the next dial starts.
    at: Instant,
    /// Failed dials so far; indexes the schedule.
    failed: u32,
    /// Why the last dial failed.
    failure: Option<io::Error>,
    /// The first connection's queue, which the client already holds. A connection after a
    /// restart gets a new one.
    queue: Option<Queue>,
}

/// Where a finished shutdown sequence leads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ending {
    /// `Stopped(Shutdown)`, and the worker is done.
    Stopped,
    /// Waiting for the client, after it chose to stop a live connection.
    Idle,
}

/// What a dialing worker dials.
pub(crate) enum Endpoint {
    /// A TCP address, resolved again on each dial.
    Tcp(tcp::Dial),
    /// A WebSocket URL.
    #[cfg(feature = "websocket")]
    Websocket(websocket::Endpoint),
}

/// A connection's outgoing queue, made before its socket so the client can send at once.
enum Queue {
    /// Bodies framed with `Content-Length`, written by a writer thread.
    Stream { writer: Writer, outgoing: Outgoing },
    /// Messages sent one per frame by a WebSocket's I/O thread.
    #[cfg(feature = "websocket")]
    Websocket(websocket::Queue),
}

/// A socket that is up, before its threads run.
enum Dialed {
    Tcp(TcpStream),
    #[cfg(feature = "websocket")]
    Websocket(websocket::Socket),
}

/// A connection's threads, started and waiting for its queue.
enum Pending {
    Tcp {
        threads: tcp::Threads,
        stream: TcpStream,
    },
    #[cfg(feature = "websocket")]
    Websocket(websocket::Pending),
}

/// What tears a started connection down.
enum Socket {
    /// Shut down to release the reader and the writer, which are never joined.
    Tcp(TcpStream),
    #[cfg(feature = "websocket")]
    Websocket(websocket::Connection),
}

/// One connection: the worker's handle on its socket and queue, and what its reader reported.
struct Connection {
    generation: Generation,
    socket: Socket,
    /// The worker's own handle on the queue, so `shutdown` and `exit` queue behind the client's
    /// messages.
    link: Link,
    /// The reader reached EOF.
    ended: bool,
    /// Tells the reader to look for the shutdown reply.
    closing: Arc<AtomicBool>,
    /// When the server has to have answered `initialize`, until the client says it did.
    deadline: Option<Instant>,
}

impl Worker {
    fn run(mut self) {
        loop {
            if let Some(notice) = self.next() {
                if self.notice(notice, Instant::now()).is_break() {
                    return;
                }
            }
            if self.tick(Instant::now()).is_break() {
                return;
            }
        }
    }

    /// The next notice, or `None` once the current state's wait is over.
    fn next(&self) -> Option<Notice> {
        let until = match &self.state {
            State::Dialing(dialing) => Some(dialing.at),
            State::Accepting { .. } => Some(Instant::now() + ACCEPT_POLL),
            State::Live(connection) => connection.deadline,
            State::Closing { sequence, .. } => Some(sequence.until()),
            State::Draining { until, .. } | State::Backoff { until } => Some(*until),
            State::Waiting | State::Idle => None,
        };
        let received = match until {
            Some(until) => {
                match self
                    .notices
                    .recv_timeout(until.saturating_duration_since(Instant::now()))
                {
                    Ok(notice) => Ok(notice),
                    Err(mpsc::RecvTimeoutError::Timeout) => return None,
                    Err(mpsc::RecvTimeoutError::Disconnected) => Err(mpsc::RecvError),
                }
            }
            None => self.notices.recv(),
        };
        Some(received.expect("the worker holds a sender of its own inbox"))
    }

    fn notice(&mut self, notice: Notice, now: Instant) -> ControlFlow<()> {
        match notice {
            Notice::Control(lifecycle) => return self.control(lifecycle, now),
            Notice::Ended { generation, .. } => self.ended(generation, now),
            Notice::WriteFailed(generation) => self.lose(generation, client::Reason::Closed, now),
            Notice::Backlog(generation) => {
                self.lose(generation, client::Reason::Unresponsive, now);
            }
            Notice::Replied(generation) => {
                if let State::Closing {
                    connection,
                    sequence,
                    ..
                } = &mut self.state
                {
                    if connection.generation == generation {
                        sequence.replied(&connection.link, self.settings.grace, now);
                    }
                }
            }
        }
        ControlFlow::Continue(())
    }

    /// Acts on what the client decided, if it still applies to the current generation.
    fn control(&mut self, lifecycle: Lifecycle, now: Instant) -> ControlFlow<()> {
        let state = std::mem::replace(&mut self.state, State::Idle);
        self.state = match (lifecycle, state) {
            (
                Lifecycle::Shutdown {
                    generation,
                    handshake,
                },
                State::Live(connection),
            ) => {
                let handshake = if generation == self.generation {
                    handshake
                } else {
                    Handshake::Pending
                };
                self.close(connection, handshake, Ending::Stopped, now)
            }
            (
                Lifecycle::Shutdown { .. },
                State::Closing {
                    connection,
                    sequence,
                    ..
                },
            ) => State::Closing {
                connection,
                sequence,
                ending: Ending::Stopped,
            },
            // Nothing is up that needs a goodbye: a pending dial or accept never happens. The
            // listener closes before the stop is reported, so its port is free by then.
            (
                Lifecycle::Shutdown { .. },
                state @ (State::Dialing(_)
                | State::Accepting { .. }
                | State::Draining { .. }
                | State::Waiting
                | State::Backoff { .. }
                | State::Idle),
            ) => {
                drop(state);
                self.report(transport::Event::Stopped(client::Reason::Shutdown));
                return ControlFlow::Break(());
            }
            (Lifecycle::Handshaken(generation), State::Live(mut connection))
                if generation == self.generation =>
            {
                connection.deadline = None;
                self.retry = 0;
                State::Live(connection)
            }
            (Lifecycle::Reconnect(generation), State::Waiting) if generation == self.generation => {
                State::Backoff {
                    until: now + self.settings.delay(self.retry),
                }
            }
            // A live connection is closed without `shutdown`, as a failed `initialize` needs.
            (Lifecycle::Stop(generation), State::Live(connection))
                if generation == self.generation =>
            {
                self.generation = generation.next();
                self.close(connection, Handshake::Pending, Ending::Idle, now)
            }
            (
                Lifecycle::Stop(generation),
                State::Dialing(_)
                | State::Accepting { .. }
                | State::Draining { .. }
                | State::Waiting
                | State::Backoff { .. }
                | State::Idle,
            ) if generation == self.generation => {
                self.generation = generation.next();
                State::Idle
            }
            // Whatever is up is torn down without a loss: the client already let go of it.
            (
                Lifecycle::Restart(generation),
                State::Live(connection)
                | State::Closing {
                    connection,
                    ending: Ending::Idle,
                    ..
                },
            ) if generation >= self.generation && self.redials() => {
                connection.end();
                self.restart(generation, now)
            }
            (
                Lifecycle::Restart(generation),
                State::Dialing(_)
                | State::Draining { .. }
                | State::Waiting
                | State::Backoff { .. }
                | State::Idle,
            ) if generation >= self.generation && self.redials() => self.restart(generation, now),
            // A restart never runs behind the client, and a shutdown under way wins.
            (Lifecycle::Restart(generation), state) => {
                debug_assert!(
                    generation >= self.generation,
                    "restart {generation} is behind {}",
                    self.generation
                );
                state
            }
            (Lifecycle::Handshaken(_) | Lifecycle::Reconnect(_) | Lifecycle::Stop(_), state) => {
                state
            }
        };
        ControlFlow::Continue(())
    }

    /// The reader of connection `generation` reached EOF: the server closed its end.
    fn ended(&mut self, generation: Generation, now: Instant) {
        match &mut self.state {
            State::Live(connection) if connection.generation == generation => {
                connection.ended = true;
                self.lose(generation, client::Reason::Closed, now);
            }
            State::Closing { connection, .. } if connection.generation == generation => {
                connection.ended = true;
            }
            State::Draining {
                generation: draining,
                ended,
                ..
            } if *draining == generation => *ended = true,
            State::Dialing(_)
            | State::Accepting { .. }
            | State::Live(_)
            | State::Draining { .. }
            | State::Waiting
            | State::Backoff { .. }
            | State::Idle
            | State::Closing { .. } => {}
        }
    }

    /// Connection `generation` ended while live: tear it down.
    fn lose(&mut self, generation: Generation, reason: client::Reason, now: Instant) {
        let state = std::mem::replace(&mut self.state, State::Idle);
        self.state = match state {
            State::Live(connection) if connection.generation == generation => {
                self.drain(connection, reason, now)
            }
            state => state,
        };
    }

    /// What time alone moves on: a dial that is due, an `initialize` deadline, a drain or a
    /// shutdown step whose deadline passed.
    fn tick(&mut self, now: Instant) -> ControlFlow<()> {
        let state = std::mem::replace(&mut self.state, State::Idle);
        self.state = match state {
            State::Dialing(dialing) if now >= dialing.at => self.dial(dialing),
            State::Accepting { listener, queue } => match listener.accept() {
                Ok((stream, _)) => {
                    drop(listener);
                    let mut queue = Some(queue);
                    // Windows and the BSDs hand out accepted sockets in the listener's
                    // non-blocking mode, which would make the reader spin.
                    let opened = stream
                        .set_nonblocking(false)
                        .and_then(|()| self.open(Dialed::Tcp(stream), &mut queue));
                    match opened {
                        Ok(connection) => {
                            self.retry = 1;
                            State::Live(connection)
                        }
                        Err(error) => return self.fail(error),
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::Interrupted
                            | io::ErrorKind::ConnectionAborted
                    ) =>
                {
                    State::Accepting { listener, queue }
                }
                Err(error) => {
                    drop((listener, queue));
                    return self.fail(error);
                }
            },
            State::Live(connection) if connection.deadline.is_some_and(|until| now >= until) => {
                self.drain(connection, client::Reason::Timeout, now)
            }
            // Everything the reader sent before its EOF is ahead of the loss, so no late reply
            // lands after the session let go of its requests.
            State::Draining {
                ended,
                reason,
                until,
                ..
            } if ended || now >= until => match self.mode {
                Mode::Connect { .. } => {
                    self.generation = self.generation.next();
                    self.report(transport::Event::Lost {
                        generation: self.generation,
                        reason,
                    });
                    State::Waiting
                }
                Mode::Listen => {
                    self.report(transport::Event::Stopped(reason));
                    return ControlFlow::Break(());
                }
            },
            State::Backoff { until } if now >= until => self.redial(),
            State::Closing {
                connection,
                mut sequence,
                ending,
            } => {
                let over = connection.ended
                    || (now >= sequence.until()
                        && sequence.expired(&connection.link, self.settings.grace, now)
                            == lifecycle::Next::Kill);
                if !over {
                    State::Closing {
                        connection,
                        sequence,
                        ending,
                    }
                } else {
                    connection.end();
                    match ending {
                        Ending::Stopped => {
                            self.report(transport::Event::Stopped(client::Reason::Shutdown));
                            return ControlFlow::Break(());
                        }
                        Ending::Idle => State::Idle,
                    }
                }
            }
            state @ (State::Dialing(_)
            | State::Live(_)
            | State::Draining { .. }
            | State::Backoff { .. }
            | State::Waiting
            | State::Idle) => state,
        };
        ControlFlow::Continue(())
    }

    /// Starts the shutdown sequence on `connection`, behind everything the client queued.
    fn close(
        &self,
        connection: Connection,
        handshake: Handshake,
        ending: Ending,
        now: Instant,
    ) -> State {
        connection.closing.store(true, Ordering::Relaxed);
        let sequence =
            lifecycle::Sequence::begin(&connection.link, handshake, self.settings.grace, now);
        State::Closing {
            connection,
            sequence,
            ending,
        }
    }

    /// Tears `connection` down, and lets its reader drain for one grace period.
    fn drain(&self, connection: Connection, reason: client::Reason, now: Instant) -> State {
        let (generation, ended) = (connection.generation, connection.ended);
        connection.end();
        State::Draining {
            generation,
            ended,
            reason,
            until: now + self.settings.grace,
        }
    }

    /// Dials connection `generation` at once, with a fresh budget and the backoff starting over.
    fn restart(&mut self, generation: Generation, now: Instant) -> State {
        self.generation = generation;
        self.retry = 0;
        let Mode::Connect { budget, .. } = &self.mode else {
            unreachable!("only a dialing worker restarts");
        };
        State::Dialing(Dialing::new(now, *budget, None))
    }

    /// Whether the worker can bring a connection up again.
    fn redials(&self) -> bool {
        match self.mode {
            Mode::Connect { .. } => true,
            Mode::Listen => false,
        }
    }

    /// One dial of the first ones: up on success, retried on failure until the budget runs out,
    /// which is a loss.
    fn dial(&mut self, mut dialing: Dialing) -> State {
        if Instant::now() < dialing.deadline || dialing.failure.is_none() {
            let opened = self
                .connect(Some(dialing.deadline))
                .and_then(|dialed| self.open(dialed, &mut dialing.queue));
            match opened {
                Ok(connection) => {
                    self.retry = 1;
                    return State::Live(connection);
                }
                Err(error) => dialing.failure = Some(error),
            }
        }
        let now = Instant::now();
        if now < dialing.deadline {
            dialing.at = (now + grow(FIRST_STEP, dialing.failed, STEP_CAP)).min(dialing.deadline);
            dialing.failed = dialing.failed.saturating_add(1);
            return State::Dialing(dialing);
        }
        let failure = dialing
            .failure
            .unwrap_or_else(|| io::ErrorKind::TimedOut.into());
        self.generation = self.generation.next();
        self.report(transport::Event::Lost {
            generation: self.generation,
            reason: client::Reason::Failed(Arc::new(failure)),
        });
        State::Waiting
    }

    /// Dials the connection of the current generation after a loss. A failure counts against
    /// the restart policy, and the next dial follows after a longer wait.
    fn redial(&mut self) -> State {
        let delay = self.settings.delay(self.retry);
        self.retry = self.retry.saturating_add(1);
        let opened = self
            .connect(None)
            .and_then(|dialed| self.open(dialed, &mut None));
        match opened {
            Ok(connection) => State::Live(connection),
            Err(error) => {
                self.report(transport::Event::Attempting {
                    generation: self.generation,
                    failure: Arc::new(error),
                });
                State::Backoff {
                    until: Instant::now() + delay,
                }
            }
        }
    }

    fn connect(&self, deadline: Option<Instant>) -> io::Result<Dialed> {
        let Mode::Connect { endpoint, .. } = &self.mode else {
            unreachable!("a listening worker never dials");
        };
        match endpoint {
            Endpoint::Tcp(dial) => dial(deadline).map(Dialed::Tcp),
            #[cfg(feature = "websocket")]
            Endpoint::Websocket(endpoint) => endpoint.dial(deadline).map(Dialed::Websocket),
        }
    }

    /// The bridge can't go on: it stops with `error`.
    fn fail(&self, error: io::Error) -> ControlFlow<()> {
        self.report(transport::Event::Stopped(client::Reason::Failed(Arc::new(
            error,
        ))));
        ControlFlow::Break(())
    }

    /// Starts connection `self.generation` on `dialed`, on `queue` if the client already holds
    /// it. A new queue is announced before the connection's threads go, so the client holds it
    /// before anything the server sends arrives.
    fn open(&self, dialed: Dialed, queue: &mut Option<Queue>) -> io::Result<Connection> {
        let generation = self.generation;
        let pending = Pending::start(dialed, generation, &self.events, &self.sender)?;
        let queue = match queue.take() {
            Some(queue) => queue,
            None => {
                let queue = self
                    .mode
                    .queue(generation, self.settings.limit, &self.sender)?;
                self.report(transport::Event::Reconnected {
                    generation,
                    link: queue.link(),
                });
                queue
            }
        };
        let link = queue.link();
        let (socket, closing) = pending.go(queue);
        Ok(Connection {
            generation,
            socket,
            link,
            ended: false,
            closing,
            deadline: self
                .settings
                .initialize_timeout
                .map(|timeout| Instant::now() + timeout),
        })
    }

    fn report(&self, event: transport::Event) {
        // `Events` was dropped: nobody is left to tell.
        let _ = self.events.unbounded_send(event);
    }
}

impl Dialing {
    /// The first dial at `now`, retried until `budget` has passed.
    fn new(now: Instant, budget: Duration, queue: Option<Queue>) -> Self {
        Self {
            deadline: now + budget,
            at: now,
            failed: 0,
            failure: None,
            queue,
        }
    }
}

impl Mode {
    /// The queue of connection `generation`, whose guard trips past `limit` unwritten bytes.
    fn queue(
        &self,
        generation: Generation,
        limit: usize,
        notices: &mpsc::Sender<Notice>,
    ) -> io::Result<Queue> {
        match self {
            Mode::Connect {
                endpoint: Endpoint::Tcp(_),
                ..
            }
            | Mode::Listen => {
                let (writer, outgoing) = Writer::new(generation, limit, notices);
                Ok(Queue::Stream { writer, outgoing })
            }
            #[cfg(feature = "websocket")]
            Mode::Connect {
                endpoint: Endpoint::Websocket(_),
                ..
            } => websocket::Queue::new(generation, limit, notices).map(Queue::Websocket),
        }
    }

    /// The bridge's name in its threads' names.
    fn name(&self) -> &'static str {
        match self {
            Mode::Connect {
                endpoint: Endpoint::Tcp(_),
                ..
            }
            | Mode::Listen => "tcp",
            #[cfg(feature = "websocket")]
            Mode::Connect {
                endpoint: Endpoint::Websocket(_),
                ..
            } => "websocket",
        }
    }
}

impl Queue {
    /// The client's handle on this queue.
    fn link(&self) -> Link {
        match self {
            Queue::Stream { writer, .. } => Link::Stream(writer.clone()),
            #[cfg(feature = "websocket")]
            Queue::Websocket(queue) => queue.link(),
        }
    }
}

impl Pending {
    /// Starts the threads of connection `generation` on `dialed`.
    fn start(
        dialed: Dialed,
        generation: Generation,
        events: &Feed,
        notices: &mpsc::Sender<Notice>,
    ) -> io::Result<Self> {
        match dialed {
            Dialed::Tcp(stream) => {
                match tcp::Threads::start(&stream, generation, events, notices) {
                    Ok(threads) => Ok(Pending::Tcp { threads, stream }),
                    Err(error) => {
                        let _ = stream.shutdown(Shutdown::Both);
                        Err(error)
                    }
                }
            }
            #[cfg(feature = "websocket")]
            Dialed::Websocket(socket) => {
                websocket::Pending::start(socket, generation, events, notices)
                    .map(Pending::Websocket)
            }
        }
    }

    /// Hands the threads their queue and lets them run. Returns what tears the connection down,
    /// and the flag that tells its reader to look for the shutdown reply.
    fn go(self, queue: Queue) -> (Socket, Arc<AtomicBool>) {
        match (self, queue) {
            (Pending::Tcp { threads, stream }, Queue::Stream { outgoing, .. }) => {
                (Socket::Tcp(stream), threads.go(outgoing))
            }
            #[cfg(feature = "websocket")]
            (Pending::Websocket(pending), Queue::Websocket(queue)) => {
                let (connection, closing) = pending.go(queue);
                (Socket::Websocket(connection), closing)
            }
            #[cfg(feature = "websocket")]
            (Pending::Tcp { .. }, Queue::Websocket(_))
            | (Pending::Websocket(_), Queue::Stream { .. }) => {
                unreachable!("a worker's connections and queues come from its one endpoint")
            }
        }
    }
}

impl Connection {
    /// Releases the connection's threads, which are never joined, and lets go.
    fn end(self) {
        match self.socket {
            // Both ways, which wakes the reader blocked on it.
            Socket::Tcp(stream) => {
                let _ = stream.shutdown(Shutdown::Both);
            }
            #[cfg(feature = "websocket")]
            Socket::Websocket(connection) => connection.end(),
        }
    }
}

/// Starts a worker that dials `endpoint` and carries one connection at a time. The first dials
/// retry until `budget` has passed.
pub(crate) fn connect(
    endpoint: Endpoint,
    budget: Duration,
    settings: Settings,
) -> Result<Started, builder::Error> {
    let now = Instant::now();
    spawn(Mode::Connect { endpoint, budget }, settings, |queue| {
        State::Dialing(Dialing::new(now, budget, Some(queue)))
    })
}

/// Starts a worker that accepts exactly one connection on `listener`, which is non-blocking.
pub(crate) fn listen(listener: TcpListener, settings: Settings) -> Result<Started, builder::Error> {
    spawn(Mode::Listen, settings, |queue| State::Accepting {
        listener,
        queue,
    })
}

/// Starts the worker in the state `first` makes of the first connection's queue.
fn spawn(
    mode: Mode,
    settings: Settings,
    first: impl FnOnce(Queue) -> State,
) -> Result<Started, builder::Error> {
    let (sender, notices) = mpsc::channel();
    let (events, incoming) = futures_channel::mpsc::unbounded();
    let generation = Generation::FIRST;
    let queue = mode
        .queue(generation, settings.limit, &sender)
        .map_err(builder::Error::Thread)?;
    let link = queue.link();
    let handle = Handle {
        notices: sender.clone(),
    };
    let control = match mode {
        Mode::Connect {
            endpoint: Endpoint::Tcp(_),
            ..
        } => transport::Control::Tcp(handle),
        #[cfg(feature = "websocket")]
        Mode::Connect {
            endpoint: Endpoint::Websocket(_),
            ..
        } => transport::Control::Websocket(handle),
        Mode::Listen => transport::Control::Listen(handle),
    };
    let name = format!("scrive-lsp {} #{generation}", mode.name());
    let worker = Worker {
        mode,
        generation,
        state: first(queue),
        retry: 0,
        settings,
        events,
        notices,
        sender,
    };
    transport::start(name, move || {
        worker.run();
    })
    .map_err(builder::Error::Thread)?;
    Ok(Started {
        link,
        control,
        inbound: transport::Inbound::Channel(incoming),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first-dial schedule starts at 50 ms and stops growing at 1 s.
    #[test]
    fn the_first_dials_retry_from_fifty_milliseconds_up_to_a_second() {
        assert_eq!(grow(FIRST_STEP, 0, STEP_CAP), FIRST_STEP, "the first step");
        assert_eq!(
            grow(FIRST_STEP, 1, STEP_CAP),
            Duration::from_millis(65),
            "30% more"
        );
        assert_eq!(grow(FIRST_STEP, 50, STEP_CAP), STEP_CAP, "capped");
    }
}
