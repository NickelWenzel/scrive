//! The TCP bridge: a language server on a socket that the client dials. A worker thread owns the
//! connection and dials again when the client says so; each connection gets a writer and a
//! reader of its own.

use std::io;
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use super::lifecycle::{self, Handshake, Lifecycle};
use super::reader::Reader;
use super::settings::grow;
use super::writer::Outgoing;
use super::{Feed, Generation, Handle, Link, Notice, Pipe, Settings, Started, Writer};
use crate::client::builder;
use crate::{client, transport};

// Bounds a dial to one address, so neither a retry nor Drop waits out the OS SYN timeout
// (about 127 s on Linux).
const DIAL_TIMEOUT: Duration = Duration::from_secs(5);
// The wait after a failed first dial, growing ×1.3 up to `STEP_CAP`. A server started together
// with its client is often still binding its port; a 1 s first step would add a second to
// every such start.
const FIRST_STEP: Duration = Duration::from_millis(50);
const STEP_CAP: Duration = Duration::from_secs(1);

/// Dials the server once, within the deadline if there is one.
type Dial = Box<dyn Fn(Option<Instant>) -> io::Result<TcpStream> + Send>;

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
    /// It dials, and dials again after a loss. Before any connection, and after a restart,
    /// failed dials retry until `budget` has passed.
    Connect { dial: Dial, budget: Duration },
}

enum State {
    /// Dialing until a connection is up or the budget runs out.
    Dialing(Dialing),
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

/// A connection's outgoing queue, made before its socket so the client can send at once.
struct Queue {
    writer: Writer,
    outgoing: Outgoing,
}

/// One connection: the worker's handle on its socket and queue, and what its reader reported.
struct Connection {
    generation: Generation,
    /// Shut down to release the reader and the writer, which are never joined.
    stream: TcpStream,
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

/// A connection's reader and writer threads, started and waiting: the writer for its queue, the
/// reader for the go-ahead, so nothing the server says reaches the client before the connection
/// is announced.
struct Threads {
    queue: mpsc::Sender<Outgoing>,
    gate: mpsc::Sender<()>,
    closing: Arc<AtomicBool>,
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
            // Nothing is up that needs a goodbye: a pending dial never happens.
            (
                Lifecycle::Shutdown { .. },
                State::Dialing(_)
                | State::Draining { .. }
                | State::Waiting
                | State::Backoff { .. }
                | State::Idle,
            ) => {
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
            ) if generation >= self.generation => {
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
            ) if generation >= self.generation => self.restart(generation, now),
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
            } if ended || now >= until => {
                self.generation = self.generation.next();
                self.report(transport::Event::Lost {
                    generation: self.generation,
                    reason,
                });
                State::Waiting
            }
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
        let Mode::Connect { budget, .. } = &self.mode;
        State::Dialing(Dialing {
            deadline: now + *budget,
            at: now,
            failed: 0,
            failure: None,
            queue: None,
        })
    }

    /// One dial of the first ones: up on success, retried on failure until the budget runs out,
    /// which is a loss.
    fn dial(&mut self, mut dialing: Dialing) -> State {
        if Instant::now() < dialing.deadline || dialing.failure.is_none() {
            let opened = self
                .connect(Some(dialing.deadline))
                .and_then(|stream| self.open(stream, &mut dialing.queue));
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
            .and_then(|stream| self.open(stream, &mut None));
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

    fn connect(&self, deadline: Option<Instant>) -> io::Result<TcpStream> {
        let Mode::Connect { dial, .. } = &self.mode;
        dial(deadline)
    }

    /// Starts connection `self.generation` on `stream`, on `queue` if the client already holds
    /// it. A new queue is announced before the reader starts, so the client holds it before
    /// anything the server sends arrives.
    fn open(&self, stream: TcpStream, queue: &mut Option<Queue>) -> io::Result<Connection> {
        let generation = self.generation;
        let threads = match Threads::start(&stream, generation, &self.events, &self.sender) {
            Ok(threads) => threads,
            Err(error) => {
                let _ = stream.shutdown(Shutdown::Both);
                return Err(error);
            }
        };
        let Queue { writer, outgoing } = match queue.take() {
            Some(queue) => queue,
            None => {
                let queue = Queue::new(generation, self.settings.limit, &self.sender);
                self.report(transport::Event::Reconnected {
                    generation,
                    link: Link::Stream(queue.writer.clone()),
                });
                queue
            }
        };
        let closing = Arc::clone(&threads.closing);
        threads.go(outgoing);
        Ok(Connection {
            generation,
            stream,
            link: Link::Stream(writer),
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

impl Queue {
    fn new(generation: Generation, limit: usize, notices: &mpsc::Sender<Notice>) -> Self {
        let (writer, outgoing) = Writer::new(generation, limit, notices);
        Self { writer, outgoing }
    }
}

impl Connection {
    /// Shuts the socket down both ways, which wakes the reader blocked on it, and lets go.
    fn end(self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

impl Threads {
    /// Starts the reader and the writer of connection `generation` on clones of `stream`.
    fn start(
        stream: &TcpStream,
        generation: Generation,
        events: &Feed,
        notices: &mpsc::Sender<Notice>,
    ) -> io::Result<Self> {
        let (read, write) = prepare(stream)?;
        let closing = Arc::new(AtomicBool::new(false));
        let reader = Reader {
            events: events.clone(),
            notices: notices.clone(),
            generation,
            pipe: Pipe::Socket,
            closing: Arc::clone(&closing),
        };
        let (queue, filled) = mpsc::channel::<Outgoing>();
        let (gate, opened) = mpsc::channel();
        let name = |role: &str| format!("scrive-lsp tcp {role} #{generation}");
        // Threads already running end on their own when this fails: their channels close
        // unused.
        transport::start(name("writer"), move || {
            if let Ok(outgoing) = filled.recv() {
                outgoing.write(write, |stream| {
                    let _ = stream.shutdown(Shutdown::Write);
                });
            }
        })?;
        transport::start(name("reader"), move || {
            if opened.recv().is_ok() {
                reader.read(read);
            }
        })?;
        Ok(Self {
            queue,
            gate,
            closing,
        })
    }

    /// Hands the writer its queue and lets the reader start.
    fn go(self, outgoing: Outgoing) {
        let _ = self.queue.send(outgoing);
        let _ = self.gate.send(());
    }
}

/// Starts a worker that dials `address` and carries one connection at a time. The first dials
/// retry until `budget` has passed.
pub(crate) fn connect<A>(
    address: A,
    budget: Duration,
    settings: Settings,
) -> Result<Started, builder::Error>
where
    A: ToSocketAddrs + Send + 'static,
{
    let (sender, notices) = mpsc::channel();
    let (events, incoming) = futures_channel::mpsc::unbounded();
    let generation = Generation::FIRST;
    let queue = Queue::new(generation, settings.limit, &sender);
    let link = Link::Stream(queue.writer.clone());
    let handle = Handle {
        notices: sender.clone(),
    };
    let now = Instant::now();
    let worker = Worker {
        mode: Mode::Connect {
            dial: Box::new(move |deadline| dial(&address, deadline)),
            budget,
        },
        generation,
        state: State::Dialing(Dialing {
            deadline: now + budget,
            at: now,
            failed: 0,
            failure: None,
            queue: Some(queue),
        }),
        retry: 0,
        settings,
        events,
        notices,
        sender,
    };
    transport::start(format!("scrive-lsp tcp #{generation}"), move || {
        worker.run()
    })
    .map_err(builder::Error::Thread)?;
    Ok(Started {
        link,
        control: transport::Control::Tcp(handle),
        inbound: transport::Inbound::Channel(incoming),
    })
}

/// One dial: every address `address` resolves to, in order, each bounded by `DIAL_TIMEOUT` and
/// `deadline`. The error is the last address's.
pub(crate) fn dial<A: ToSocketAddrs>(
    address: &A,
    deadline: Option<Instant>,
) -> io::Result<TcpStream> {
    let mut failure = None;
    for address in address.to_socket_addrs()? {
        let timeout = match deadline {
            Some(deadline) => {
                let left = deadline.saturating_duration_since(Instant::now());
                // `connect_timeout` rejects a zero duration.
                if left.is_zero() {
                    return Err(failure.unwrap_or_else(|| io::ErrorKind::TimedOut.into()));
                }
                left.min(DIAL_TIMEOUT)
            }
            None => DIAL_TIMEOUT,
        };
        match TcpStream::connect_timeout(&address, timeout) {
            Ok(stream) => return Ok(stream),
            Err(error) => failure = Some(error),
        }
    }
    Err(failure
        .unwrap_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "resolves to no address")))
}

/// Turns Nagle's algorithm off on `stream` and returns its read half and its write half, which
/// are clones of it.
fn prepare(stream: &TcpStream) -> io::Result<(TcpStream, TcpStream)> {
    stream.set_nodelay(true)?;
    Ok((stream.try_clone()?, stream.try_clone()?))
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;

    /// Every stream the bridge reads or writes has Nagle's algorithm off.
    #[test]
    fn prepared_streams_disable_nagle() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (read, write) = prepare(&stream).unwrap();
        assert!(read.nodelay().unwrap(), "the read half");
        assert!(write.nodelay().unwrap(), "the write half");
    }

    /// A dial with no time left fails at once instead of handing `connect_timeout` a zero.
    #[test]
    fn a_dial_past_its_deadline_fails_without_trying() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let error = dial(&address, Some(Instant::now())).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{error}");
    }

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
