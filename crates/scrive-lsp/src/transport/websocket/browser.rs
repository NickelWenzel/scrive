//! The browser WebSocket bridge: the page's `WebSocket`, driven by its callbacks, and one task per
//! client that dials, loses, dials again and shuts the connection down as the native socket
//! worker does.

use std::cell::Cell;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_channel::{mpsc, oneshot};
use futures_core::Stream;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasmtimer::std::Instant;

use crate::client::builder;
use crate::transport::lifecycle::{self, Handshake, Lifecycle};
use crate::transport::{self, settings, Feed, Generation, Link, Settings, Started};
use crate::{client, log};

/// Bounds one attempt to open a socket, as a native dial is bounded per address.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// A `ws://` or `wss://` URL. The browser parses the rest, and does TLS.
pub(crate) struct Endpoint {
    url: String,
}

/// The client's line to a browser bridge's task.
#[derive(Debug)]
pub(crate) struct Control {
    lifecycle: mpsc::UnboundedSender<Lifecycle>,
}

/// What a socket's callbacks tell the task.
enum Signal {
    Opened,
    Closed {
        code: u16,
        reason: String,
    },
    /// The reply to the shutdown request arrived.
    Replied,
}

/// The channel the callbacks signal on, each signal tagged with its socket's id.
type Signals = mpsc::UnboundedSender<(u64, Signal)>;

/// A browser `WebSocket` and the callbacks it calls.
struct Socket {
    socket: web_sys::WebSocket,
    /// Tells this socket's signals from those of the attempts before it.
    id: u64,
    /// Kept alive as long as the socket may call them, and detached before they drop.
    _callbacks: Callbacks,
    /// Tells the message callback to look for the shutdown reply.
    closing: Rc<Cell<bool>>,
    /// Ends the drain task when the socket goes.
    gone: Option<oneshot::Sender<()>>,
    /// The drain task's end of `gone`, until the task starts.
    ended: Option<oneshot::Receiver<()>>,
}

struct Callbacks {
    _open: Closure<dyn FnMut()>,
    _message: Closure<dyn FnMut(web_sys::MessageEvent)>,
    _close: Closure<dyn FnMut(web_sys::CloseEvent)>,
}

/// A connection's outgoing queue: the drain task's end, and a link to it.
struct Queue {
    receiver: mpsc::UnboundedReceiver<Arc<[u8]>>,
    link: Link,
    /// Whether the client already holds the link; otherwise opening announces it.
    announce: Announce,
}

#[derive(Clone, Copy)]
enum Announce {
    /// The client holds the link: the first connection's queue.
    Held,
    /// The client gets the link with `Reconnected` when the socket opens.
    OnOpen,
}

/// One drain step.
enum Step {
    Send(Arc<[u8]>),
    Close,
    Stop,
}

/// Owns the connection across its generations, like the native socket worker.
struct Task {
    url: String,
    /// The connection the task is on. It runs ahead of the client by at most one, after a loss.
    generation: Generation,
    state: State,
    /// Dials since the last handshake; indexes the backoff.
    retry: u32,
    settings: Settings,
    /// How long the first dials retry.
    budget: Duration,
    events: Feed,
    /// `None` once the client is gone.
    lifecycle: Option<mpsc::UnboundedReceiver<Lifecycle>>,
    signals: mpsc::UnboundedReceiver<(u64, Signal)>,
    /// Handed to each new socket's callbacks.
    sender: Signals,
    /// The id of the next socket.
    sockets: u64,
    /// The one timer, reset for each deadline.
    timer: Pin<Box<wasmtimer::tokio::Sleep>>,
}

enum State {
    /// The first dials of a generation, retried on their own schedule.
    Dialing(Dialing),
    /// A dial after a loss: one attempt, then a backoff.
    Redialing {
        socket: Socket,
        queue: Queue,
        until: Instant,
    },
    /// The connection of the current generation is open.
    Live(Connection),
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

struct Dialing {
    /// When the dials give up.
    deadline: Instant,
    /// When the next attempt starts, while none is open.
    at: Instant,
    /// The attempt under way, and when it gives up.
    attempt: Option<(Socket, Instant)>,
    /// Failed attempts so far; indexes the schedule.
    failed: u32,
    /// Why the last attempt failed.
    failure: Option<io::Error>,
    queue: Queue,
}

/// Where a finished shutdown sequence leads.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ending {
    /// `Stopped(Shutdown)`, and the task is done.
    Stopped,
    /// Waiting for the client, after it chose to stop a live connection.
    Idle,
}

/// An open connection.
struct Connection {
    socket: Socket,
    /// The task's own link, so `shutdown` and `exit` queue behind the client's messages.
    link: Link,
    /// When the server has to have answered `initialize`, until the client says it did.
    deadline: Option<Instant>,
}

/// What woke the task.
enum Wake {
    Control(Option<Lifecycle>),
    Signal(u64, Signal),
    Timer,
}

/// Whether the task goes on.
enum Flow {
    Continue,
    Done,
}

impl Endpoint {
    /// Checks `url`'s scheme; the browser parses the rest when the first socket opens.
    pub(crate) fn new(url: &str) -> Result<Self, builder::Error> {
        super::scheme(url).map_err(|reason| builder::Error::Url {
            url: url.to_owned(),
            reason,
        })?;
        Ok(Self {
            url: url.to_owned(),
        })
    }
}

impl Control {
    /// Tells the task what the client decided. Ignored once the task has stopped.
    pub(crate) fn send(&self, lifecycle: Lifecycle) {
        let _ = self.lifecycle.unbounded_send(lifecycle);
    }
}

impl Socket {
    /// Opens socket `id` to `url` for connection `generation`. Its messages go to `events`, its
    /// open and close to `signals`; `announce` is the link the opening tells the client of.
    fn open(
        url: &str,
        (id, generation): (u64, Generation),
        events: &Feed,
        signals: &Signals,
        announce: Option<Link>,
    ) -> Result<Self, JsValue> {
        let socket = web_sys::WebSocket::new(url)?;
        // A `Blob` would need an asynchronous read.
        socket.set_binary_type(web_sys::BinaryType::Arraybuffer);
        let closing = Rc::new(Cell::new(false));
        let open = {
            let (events, signals) = (events.clone(), signals.clone());
            let mut announce = announce;
            Closure::<dyn FnMut()>::new(move || {
                // Announced from the callback, so it precedes every message of the socket.
                if let Some(link) = announce.take() {
                    let _ =
                        events.unbounded_send(transport::Event::Reconnected { generation, link });
                }
                let _ = signals.unbounded_send((id, Signal::Opened));
            })
        };
        let message = {
            let (events, signals, closing) = (events.clone(), signals.clone(), Rc::clone(&closing));
            Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
                let Some(body) = body(&event.data(), &events) else {
                    return;
                };
                if closing.get() && lifecycle::is_shutdown_reply(&body) {
                    let _ = signals.unbounded_send((id, Signal::Replied));
                    return;
                }
                let _ = events.unbounded_send(transport::Event::Message {
                    generation,
                    body: Arc::from(body),
                });
            })
        };
        let close = {
            let signals = signals.clone();
            Closure::<dyn FnMut(web_sys::CloseEvent)>::new(move |event: web_sys::CloseEvent| {
                let _ = signals.unbounded_send((
                    id,
                    Signal::Closed {
                        code: event.code(),
                        reason: event.reason(),
                    },
                ));
            })
        };
        socket.set_onopen(Some(open.as_ref().unchecked_ref()));
        socket.set_onmessage(Some(message.as_ref().unchecked_ref()));
        socket.set_onclose(Some(close.as_ref().unchecked_ref()));
        let (gone, ended) = oneshot::channel();
        Ok(Self {
            socket,
            id,
            _callbacks: Callbacks {
                _open: open,
                _message: message,
                _close: close,
            },
            closing,
            gone: Some(gone),
            ended: Some(ended),
        })
    }

    /// Starts sending `queue` on the open socket. Sending before `open` throws.
    fn drain(&mut self, queue: mpsc::UnboundedReceiver<Arc<[u8]>>) {
        if let Some(ended) = self.ended.take() {
            wasm_bindgen_futures::spawn_local(drain(self.socket.clone(), queue, ended));
        }
    }
}

/// A socket outlives its owner, since closing is asynchronous: its late events must not call
/// the closures that drop with it.
impl Drop for Socket {
    fn drop(&mut self) {
        self.socket.set_onopen(None);
        self.socket.set_onmessage(None);
        self.socket.set_onclose(None);
        let _ = self.socket.close();
        if let Some(gone) = self.gone.take() {
            let _ = gone.send(());
        }
    }
}

impl Queue {
    /// A queue the client doesn't hold yet.
    fn fresh() -> Self {
        let (sender, receiver) = mpsc::unbounded();
        Self {
            receiver,
            link: Link::Browser(sender),
            announce: Announce::OnOpen,
        }
    }

    /// The link opening a socket on this queue announces, if any.
    fn announced(&self) -> Option<Link> {
        match self.announce {
            Announce::Held => None,
            Announce::OnOpen => Some(self.link.clone()),
        }
    }
}

impl Task {
    async fn run(mut self) {
        loop {
            let wake = std::future::poll_fn(|cx| self.poll_wake(cx)).await;
            let flow = match wake {
                Wake::Control(Some(lifecycle)) => self.control(lifecycle, Instant::now()),
                // The client is gone without a word: the same as its shutdown before a handshake.
                Wake::Control(None) => {
                    self.lifecycle = None;
                    self.control(
                        Lifecycle::Shutdown {
                            generation: self.generation,
                            handshake: Handshake::Pending,
                        },
                        Instant::now(),
                    )
                }
                Wake::Signal(id, signal) => self.signal(id, signal, Instant::now()),
                // The timer can fire a hair before the clock reads its deadline.
                Wake::Timer => self.tick(Instant::now().max(self.timer.deadline())),
            };
            if let Flow::Done = flow {
                return;
            }
        }
    }

    fn poll_wake(&mut self, cx: &mut Context<'_>) -> Poll<Wake> {
        if let Some(lifecycle) = &mut self.lifecycle {
            if let Poll::Ready(next) = Pin::new(lifecycle).poll_next(cx) {
                return Poll::Ready(Wake::Control(next));
            }
        }
        // The task holds a sender, so the signals never end.
        if let Poll::Ready(Some((id, signal))) = Pin::new(&mut self.signals).poll_next(cx) {
            return Poll::Ready(Wake::Signal(id, signal));
        }
        if let Some(deadline) = self.deadline() {
            if self.timer.deadline() != deadline {
                self.timer.as_mut().reset(deadline);
            }
            if self.timer.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Wake::Timer);
            }
        }
        Poll::Pending
    }

    /// When the current state's wait is over, if it has one.
    fn deadline(&self) -> Option<Instant> {
        match &self.state {
            State::Dialing(dialing) => Some(match &dialing.attempt {
                Some((_, until)) => *until,
                None => dialing.at,
            }),
            State::Redialing { until, .. } | State::Backoff { until } => Some(*until),
            State::Live(connection) => connection.deadline,
            State::Closing { sequence, .. } => Some(sequence.until()),
            State::Waiting | State::Idle => None,
        }
    }

    /// Acts on what the client decided, if it still applies to the current generation.
    fn control(&mut self, lifecycle: Lifecycle, now: Instant) -> Flow {
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
            // Nothing is open that needs a goodbye: a pending attempt is dropped.
            (
                Lifecycle::Shutdown { .. },
                state @ (State::Dialing(_)
                | State::Redialing { .. }
                | State::Waiting
                | State::Backoff { .. }
                | State::Idle),
            ) => {
                drop(state);
                self.report(transport::Event::Stopped(client::Reason::Shutdown));
                return Flow::Done;
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
                | State::Redialing { .. }
                | State::Waiting
                | State::Backoff { .. }
                | State::Idle,
            ) if generation == self.generation => {
                self.generation = generation.next();
                State::Idle
            }
            // Whatever is open is dropped without a loss: the client already let go of it.
            (
                Lifecycle::Restart(generation),
                State::Live(_)
                | State::Closing {
                    ending: Ending::Idle,
                    ..
                }
                | State::Dialing(_)
                | State::Redialing { .. }
                | State::Waiting
                | State::Backoff { .. }
                | State::Idle,
            ) if generation >= self.generation => {
                self.generation = generation;
                self.retry = 0;
                self.dial(Dialing::new(now, self.budget, Queue::fresh()), now)
            }
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
        Flow::Continue
    }

    /// Acts on socket `id`'s callback, if it is the socket the state holds.
    fn signal(&mut self, id: u64, signal: Signal, now: Instant) -> Flow {
        let state = std::mem::replace(&mut self.state, State::Idle);
        self.state = match (signal, state) {
            (Signal::Opened, State::Dialing(mut dialing))
                if dialing
                    .attempt
                    .as_ref()
                    .is_some_and(|(socket, _)| socket.id == id) =>
            {
                let (socket, _) = dialing
                    .attempt
                    .take()
                    .expect("the attempt was just checked");
                self.retry = 1;
                self.live(socket, dialing.queue, now)
            }
            (Signal::Opened, State::Redialing { socket, queue, .. }) if socket.id == id => {
                self.live(socket, queue, now)
            }
            (Signal::Closed { code, reason }, State::Dialing(mut dialing))
                if dialing
                    .attempt
                    .as_ref()
                    .is_some_and(|(socket, _)| socket.id == id) =>
            {
                dialing.attempt = None;
                self.dial_failed(dialing, self.refused(code, &reason), now)
            }
            (Signal::Closed { code, reason }, State::Redialing { socket, .. })
                if socket.id == id =>
            {
                drop(socket);
                let error = self.refused(code, &reason);
                self.redial_failed(error, now)
            }
            (Signal::Closed { code, reason }, State::Live(connection))
                if connection.socket.id == id =>
            {
                self.log_close(code, &reason);
                drop(connection);
                self.generation = self.generation.next();
                self.report(transport::Event::Lost {
                    generation: self.generation,
                    reason: client::Reason::Closed,
                });
                State::Waiting
            }
            (
                Signal::Closed { .. },
                State::Closing {
                    connection, ending, ..
                },
            ) if connection.socket.id == id => {
                return self.closed(connection, ending);
            }
            (
                Signal::Replied,
                State::Closing {
                    connection,
                    mut sequence,
                    ending,
                },
            ) if connection.socket.id == id => {
                sequence.replied(&connection.link, self.settings.grace, now);
                State::Closing {
                    connection,
                    sequence,
                    ending,
                }
            }
            (Signal::Opened | Signal::Closed { .. } | Signal::Replied, state) => state,
        };
        Flow::Continue
    }

    /// What time alone moves on: an attempt that is due or took too long, an `initialize`
    /// deadline, a backoff, a shutdown step.
    fn tick(&mut self, now: Instant) -> Flow {
        let state = std::mem::replace(&mut self.state, State::Idle);
        self.state = match state {
            State::Dialing(mut dialing) => match &dialing.attempt {
                Some((_, until)) if now >= *until => {
                    dialing.attempt = None;
                    self.dial_failed(dialing, io::ErrorKind::TimedOut.into(), now)
                }
                Some(_) => State::Dialing(dialing),
                None if now >= dialing.at => self.dial(dialing, now),
                None => State::Dialing(dialing),
            },
            State::Redialing { socket, until, .. } if now >= until => {
                drop(socket);
                self.redial_failed(io::ErrorKind::TimedOut.into(), now)
            }
            State::Live(connection) if connection.deadline.is_some_and(|until| now >= until) => {
                drop(connection);
                self.generation = self.generation.next();
                self.report(transport::Event::Lost {
                    generation: self.generation,
                    reason: client::Reason::Timeout,
                });
                State::Waiting
            }
            State::Backoff { until } if now >= until => self.redial(now),
            State::Closing {
                connection,
                mut sequence,
                ending,
            } if now >= sequence.until() => {
                match sequence.expired(&connection.link, self.settings.grace, now) {
                    lifecycle::Next::Wait => State::Closing {
                        connection,
                        sequence,
                        ending,
                    },
                    lifecycle::Next::Kill => return self.closed(connection, ending),
                }
            }
            state @ (State::Redialing { .. }
            | State::Live(_)
            | State::Waiting
            | State::Backoff { .. }
            | State::Idle
            | State::Closing { .. }) => state,
        };
        Flow::Continue
    }

    /// Opens the next of the first dials' attempts.
    fn dial(&mut self, mut dialing: Dialing, now: Instant) -> State {
        let announce = dialing.queue.announced();
        let id = self.next_socket();
        match Socket::open(&self.url, id, &self.events, &self.sender, announce) {
            Ok(socket) => {
                dialing.attempt = Some((socket, (now + OPEN_TIMEOUT).min(dialing.deadline)));
                State::Dialing(dialing)
            }
            Err(error) => self.dial_failed(dialing, unopened(&error), now),
        }
    }

    /// One of the first dials failed: retried until the budget runs out, which is a loss.
    fn dial_failed(&mut self, mut dialing: Dialing, error: io::Error, now: Instant) -> State {
        dialing.failure = Some(error);
        if now < dialing.deadline {
            dialing.at = (now + settings::step(dialing.failed)).min(dialing.deadline);
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

    /// Dials the connection of the current generation after a loss, on a queue of its own.
    fn redial(&mut self, now: Instant) -> State {
        let queue = Queue::fresh();
        let announce = queue.announced();
        let id = self.next_socket();
        match Socket::open(&self.url, id, &self.events, &self.sender, announce) {
            Ok(socket) => State::Redialing {
                socket,
                queue,
                until: now + OPEN_TIMEOUT,
            },
            Err(error) => self.redial_failed(unopened(&error), now),
        }
    }

    /// A dial after a loss failed: it counts against the restart policy, and the next dial
    /// follows after a longer wait.
    fn redial_failed(&mut self, error: io::Error, now: Instant) -> State {
        self.report(transport::Event::Attempting {
            generation: self.generation,
            failure: Arc::new(error),
        });
        let delay = self.settings.delay(self.retry);
        self.retry = self.retry.saturating_add(1);
        State::Backoff { until: now + delay }
    }

    /// `socket` opened: its queue starts draining.
    fn live(&mut self, mut socket: Socket, queue: Queue, now: Instant) -> State {
        let Queue { receiver, link, .. } = queue;
        socket.drain(receiver);
        State::Live(Connection {
            socket,
            link,
            deadline: self
                .settings
                .initialize_timeout
                .map(|timeout| now + timeout),
        })
    }

    /// Starts the shutdown sequence on `connection`, behind everything the client queued.
    fn close(
        &self,
        connection: Connection,
        handshake: Handshake,
        ending: Ending,
        now: Instant,
    ) -> State {
        connection.socket.closing.set(true);
        let sequence =
            lifecycle::Sequence::begin(&connection.link, handshake, self.settings.grace, now);
        State::Closing {
            connection,
            sequence,
            ending,
        }
    }

    /// The shutdown sequence is over: the socket goes.
    fn closed(&mut self, connection: Connection, ending: Ending) -> Flow {
        drop(connection);
        match ending {
            Ending::Stopped => {
                self.report(transport::Event::Stopped(client::Reason::Shutdown));
                Flow::Done
            }
            Ending::Idle => {
                self.state = State::Idle;
                Flow::Continue
            }
        }
    }

    /// The id and connection of the next socket.
    fn next_socket(&mut self) -> (u64, Generation) {
        self.sockets += 1;
        (self.sockets, self.generation)
    }

    /// Logs a socket that closed before it opened, and returns the failure. The browser says no
    /// more than the code: a refused connection is 1006.
    fn refused(&self, code: u16, reason: &str) -> io::Error {
        let text = format!("WebSocket closed before it opened: {code} {reason}");
        self.log(lsp_types::MessageType::WARNING, text.clone());
        io::Error::new(io::ErrorKind::ConnectionRefused, text)
    }

    /// Logs the server's close of a live connection; 1000 is a normal close.
    fn log_close(&self, code: u16, reason: &str) {
        let level = if code == 1000 {
            lsp_types::MessageType::INFO
        } else {
            lsp_types::MessageType::WARNING
        };
        self.log(
            level,
            format!("WebSocket closed by the server: {code} {reason}"),
        );
    }

    fn log(&self, level: lsp_types::MessageType, text: String) {
        log(&self.events, level, text);
    }

    fn report(&self, event: transport::Event) {
        // `Events` was dropped: nobody is left to tell.
        let _ = self.events.unbounded_send(event);
    }
}

impl Dialing {
    /// The first attempt at `now`, retried until `budget` has passed, on `queue`.
    fn new(now: Instant, budget: Duration, queue: Queue) -> Self {
        Self {
            deadline: now + budget,
            at: now,
            attempt: None,
            failed: 0,
            failure: None,
            queue,
        }
    }
}

/// Opens the first socket to `endpoint` at once, so a URL the browser rejects fails here, and
/// starts the task that carries the connection. The first dials retry until `budget` has passed.
pub(crate) fn start(
    endpoint: Endpoint,
    budget: Duration,
    settings: Settings,
) -> Result<Started, builder::Error> {
    let (events, incoming) = mpsc::unbounded();
    let (lifecycle, controlled) = mpsc::unbounded();
    let (sender, signals) = mpsc::unbounded();
    let (queued, receiver) = mpsc::unbounded();
    let link = Link::Browser(queued);
    let queue = Queue {
        receiver,
        link: link.clone(),
        announce: Announce::Held,
    };
    let now = Instant::now();
    let generation = Generation::FIRST;
    let mut dialing = Dialing::new(now, budget, queue);
    match Socket::open(&endpoint.url, (0, generation), &events, &sender, None) {
        Ok(socket) => dialing.attempt = Some((socket, (now + OPEN_TIMEOUT).min(dialing.deadline))),
        Err(error) if is_syntax_error(&error) => {
            return Err(builder::Error::Url {
                url: endpoint.url,
                reason: builder::error::Url::Unparsable,
            })
        }
        Err(error) => {
            dialing.failure = Some(unopened(&error));
            dialing.at = (now + settings::step(0)).min(dialing.deadline);
            dialing.failed = 1;
        }
    }
    let task = Task {
        url: endpoint.url,
        generation,
        state: State::Dialing(dialing),
        retry: 0,
        settings,
        budget,
        events,
        lifecycle: Some(controlled),
        signals,
        sender,
        sockets: 0,
        timer: Box::pin(wasmtimer::tokio::sleep(Duration::ZERO)),
    };
    wasm_bindgen_futures::spawn_local(task.run());
    Ok(Started {
        link,
        control: transport::Control::Browser(Control { lifecycle }),
        inbound: transport::Inbound::Channel(incoming),
    })
}

/// Sends `queue` on `socket` in order once it is open, until `gone` fires. The queue's end is
/// the close request: everything queued before it goes out first, then the close frame.
async fn drain(
    socket: web_sys::WebSocket,
    mut queue: mpsc::UnboundedReceiver<Arc<[u8]>>,
    mut gone: oneshot::Receiver<()>,
) {
    loop {
        let next = std::future::poll_fn(|cx| {
            if Pin::new(&mut gone).poll(cx).is_ready() {
                return Poll::Ready(Step::Stop);
            }
            match Pin::new(&mut queue).poll_next(cx) {
                Poll::Ready(Some(body)) => Poll::Ready(Step::Send(body)),
                Poll::Ready(None) => Poll::Ready(Step::Close),
                Poll::Pending => Poll::Pending,
            }
        })
        .await;
        match next {
            Step::Send(body) => {
                let text = std::str::from_utf8(&body)
                    .expect("the session serializes JSON, which is UTF-8");
                if socket.send_with_str(text).is_err() {
                    return;
                }
            }
            Step::Close => {
                let _ = socket.close_with_code(1000);
                return;
            }
            Step::Stop => return,
        }
    }
}

/// A message event's payload as bytes: a text frame, or a binary frame that is UTF-8. Any other
/// binary frame is logged and dropped.
fn body(data: &JsValue, events: &Feed) -> Option<Vec<u8>> {
    if let Some(text) = data.as_string() {
        return Some(text.into_bytes());
    }
    let buffer = data.dyn_ref::<js_sys::ArrayBuffer>()?;
    let bytes = js_sys::Uint8Array::new(buffer).to_vec();
    match std::str::from_utf8(&bytes) {
        Ok(_) => Some(bytes),
        Err(error) => {
            log(
                events,
                lsp_types::MessageType::WARNING,
                format!(
                    "dropped a binary WebSocket frame that isn't UTF-8 ({} bytes): {error}",
                    bytes.len()
                ),
            );
            None
        }
    }
}

fn log(events: &Feed, level: lsp_types::MessageType, text: String) {
    let _ = events.unbounded_send(transport::Event::Log(Arc::from([log::Entry::socket(
        level, text,
    )])));
}

/// A socket the browser refused to create.
fn unopened(error: &JsValue) -> io::Error {
    io::Error::other(format!("the browser refused the WebSocket: {error:?}"))
}

fn is_syntax_error(error: &JsValue) -> bool {
    error
        .dyn_ref::<js_sys::Error>()
        .is_some_and(|error| error.name() == "SyntaxError")
}
