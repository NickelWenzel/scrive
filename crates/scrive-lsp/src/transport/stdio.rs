//! The stdio bridge: a language server as a child process, talked to over its stdin and stdout.
//! A supervisor thread owns the process and starts it again when the client says so; each
//! process gets a writer, a stdout reader and a stderr reader of its own.

use std::io::{self, Read};
use std::ops::ControlFlow;
use std::process::{self, Child, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use super::lifecycle::{self, Handshake, Lifecycle};
use super::reader::{self, Reader};
use super::{frame, Feed, Generation, Handle, Link, Notice, Pipe, Settings, Started, Writer};
use crate::client::builder;
use crate::{client, log, transport};

/// How long the supervisor waits for a notice before checking a live process again.
const IDLE: Duration = Duration::from_secs(1);
/// How often it checks while the shutdown sequence runs.
const POLL: Duration = Duration::from_millis(50);
/// Win32's `CREATE_NO_WINDOW` process creation flag.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Owns the server process across its generations: reaps it, kills it, starts it again, and
/// reports each loss for the client to decide on.
struct Supervisor {
    command: process::Command,
    /// The connection the supervisor is on. It runs ahead of the client by at most one, after
    /// a loss, and then waits for the client's answer.
    generation: Generation,
    state: State,
    /// Spawns since the last handshake; indexes the backoff.
    retry: u32,
    settings: Settings,
    events: Feed,
    notices: mpsc::Receiver<Notice>,
    /// Handed to the threads of each new connection.
    sender: mpsc::Sender<Notice>,
}

enum State {
    /// The process of the current generation runs.
    Live(Process),
    /// The process is gone (reaped); its readers get until `until` to reach EOF before the loss
    /// is reported.
    Draining {
        readers: Readers,
        reason: client::Reason,
        until: Instant,
    },
    /// The loss is reported; waiting for the client's `Reconnect` or `Stop`.
    Waiting,
    /// Respawning at `until`.
    Backoff { until: Instant },
    /// The client chose not to reconnect.
    Idle,
    /// Running the shutdown sequence on `process`.
    Closing {
        process: Process,
        sequence: lifecycle::Sequence,
        ending: Ending,
    },
}

/// Where a finished shutdown sequence leads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ending {
    /// `Stopped(Shutdown)`, and the supervisor is done.
    Stopped,
    /// Waiting for the client, after it chose to stop a live connection.
    Idle,
}

/// One server process, its outgoing queue, and what its readers reported.
struct Process {
    child: Child,
    /// The supervisor's own handle on the queue, so `shutdown` and `exit` queue behind the
    /// client's messages.
    link: Link,
    readers: Readers,
    /// When the process has to have answered `initialize`, until the client says it did.
    deadline: Option<Instant>,
}

/// Which of one connection's readers reached EOF.
struct Readers {
    generation: Generation,
    stdout: bool,
    stderr: bool,
    /// Tells the stdout reader to look for the shutdown reply.
    closing: Arc<AtomicBool>,
}

/// A started process whose readers wait for the go-ahead, so nothing it says reaches the
/// client before the connection is announced.
struct Spawned {
    child: Child,
    writer: Writer,
    readers: Readers,
    gates: [mpsc::Sender<()>; 2],
}

/// Why a process could not be started with the threads that serve it.
enum Failure {
    Spawn(io::Error),
    Thread(io::Error),
}

/// How the process ended.
#[derive(Clone, Copy, Debug)]
struct Exit {
    code: Option<i32>,
    signal: Option<i32>,
}

/// What ended a live connection.
#[derive(Clone, Copy, Debug)]
enum Cause {
    /// Its process exited, its stdout closed, or a write failed.
    Gone,
    /// It stopped reading its input.
    Unresponsive,
    /// It did not answer `initialize` in time.
    Timeout,
}

/// Splits stderr into lines across reads.
#[derive(Debug, Default)]
struct Lines {
    partial: Vec<u8>,
    /// Dropping the rest of a line longer than `frame::LINE_LIMIT`.
    overlong: bool,
}

impl From<ExitStatus> for Exit {
    fn from(status: ExitStatus) -> Self {
        #[cfg(unix)]
        let signal = std::os::unix::process::ExitStatusExt::signal(&status);
        #[cfg(not(unix))]
        let signal = None;
        Self {
            code: status.code(),
            signal,
        }
    }
}

impl Supervisor {
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
        let now = Instant::now();
        let wait = match &self.state {
            // A live process usually announces its end through its stdout reader; the timeout
            // catches one whose pipes a grandchild holds open.
            State::Live(process) => Some(process.deadline.map_or(IDLE, |deadline| {
                IDLE.min(deadline.saturating_duration_since(now))
            })),
            State::Closing { sequence, .. } => {
                Some(POLL.min(sequence.until().saturating_duration_since(now)))
            }
            State::Draining { until, .. } | State::Backoff { until } => {
                Some(until.saturating_duration_since(now))
            }
            State::Waiting | State::Idle => None,
        };
        let received = match wait {
            Some(wait) => match self.notices.recv_timeout(wait) {
                Ok(notice) => Ok(notice),
                Err(mpsc::RecvTimeoutError::Timeout) => return None,
                Err(mpsc::RecvTimeoutError::Disconnected) => Err(mpsc::RecvError),
            },
            None => self.notices.recv(),
        };
        Some(received.expect("the supervisor holds a sender of its own inbox"))
    }

    fn notice(&mut self, notice: Notice, now: Instant) -> ControlFlow<()> {
        match notice {
            Notice::Control(lifecycle) => return self.control(lifecycle, now),
            Notice::Ended { generation, pipe } => self.ended(generation, pipe, now),
            Notice::WriteFailed(generation) => self.lose(generation, Cause::Gone, now),
            Notice::Backlog(generation) => self.lose(generation, Cause::Unresponsive, now),
            Notice::Replied(generation) => {
                if let State::Closing {
                    process, sequence, ..
                } = &mut self.state
                {
                    if process.readers.generation == generation {
                        sequence.replied(&process.link, self.settings.grace, now);
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
                State::Live(process),
            ) => {
                let handshake = if generation == self.generation {
                    handshake
                } else {
                    Handshake::Pending
                };
                self.close(process, handshake, Ending::Stopped, now)
            }
            (
                Lifecycle::Shutdown { .. },
                State::Closing {
                    process, sequence, ..
                },
            ) => State::Closing {
                process,
                sequence,
                ending: Ending::Stopped,
            },
            // Nothing runs, or nothing that needs a goodbye: a pending respawn never happens.
            (
                Lifecycle::Shutdown { .. },
                State::Draining { .. } | State::Waiting | State::Backoff { .. } | State::Idle,
            ) => {
                self.report(transport::Event::Stopped(client::Reason::Shutdown));
                return ControlFlow::Break(());
            }
            (Lifecycle::Handshaken(generation), State::Live(mut process))
                if generation == self.generation =>
            {
                process.deadline = None;
                self.retry = 0;
                State::Live(process)
            }
            (Lifecycle::Reconnect(generation), State::Waiting) if generation == self.generation => {
                State::Backoff {
                    until: now + self.settings.delay(self.retry),
                }
            }
            // A live connection is closed without `shutdown`, as a failed `initialize` needs.
            (Lifecycle::Stop(generation), State::Live(process))
                if generation == self.generation =>
            {
                self.generation = generation.next();
                self.close(process, Handshake::Pending, Ending::Idle, now)
            }
            (
                Lifecycle::Stop(generation),
                State::Draining { .. } | State::Waiting | State::Backoff { .. } | State::Idle,
            ) if generation == self.generation => {
                self.generation = generation.next();
                State::Idle
            }
            // Whatever runs is killed without a loss: the client already let go of it.
            (
                Lifecycle::Restart(generation),
                State::Live(mut process)
                | State::Closing {
                    mut process,
                    ending: Ending::Idle,
                    ..
                },
            ) if generation >= self.generation => {
                let _ = process.end();
                self.respawn(generation)
            }
            (
                Lifecycle::Restart(generation),
                State::Draining { .. } | State::Waiting | State::Backoff { .. } | State::Idle,
            ) if generation >= self.generation => self.respawn(generation),
            // A restart never runs behind the client, and a shutdown under way wins.
            (Lifecycle::Restart(generation), state) => {
                debug_assert!(
                    generation >= self.generation,
                    "restart {generation} is behind {}",
                    self.generation
                );
                state
            }
            (
                Lifecycle::Handshaken(_) | Lifecycle::Reconnect(_) | Lifecycle::Stop(_),
                state,
            ) => state,
        };
        ControlFlow::Continue(())
    }

    /// A reader of connection `generation` reached EOF. A live server whose stdout closed can't
    /// answer any more.
    fn ended(&mut self, generation: Generation, pipe: Pipe, now: Instant) {
        let readers = match &mut self.state {
            State::Live(process) => &mut process.readers,
            State::Draining { readers, .. } => readers,
            State::Waiting | State::Backoff { .. } | State::Idle | State::Closing { .. } => return,
        };
        if readers.generation != generation {
            return;
        }
        match pipe {
            Pipe::Stdout => readers.stdout = true,
            Pipe::Stderr => readers.stderr = true,
        }
        if pipe == Pipe::Stdout {
            self.lose(generation, Cause::Gone, now);
        }
    }

    /// Connection `generation` ended while live: kill and reap its process.
    fn lose(&mut self, generation: Generation, cause: Cause, now: Instant) {
        let state = std::mem::replace(&mut self.state, State::Idle);
        self.state = match state {
            State::Live(process) if process.readers.generation == generation => {
                self.drain(process, cause, now)
            }
            state => state,
        };
    }

    /// What time alone moves on: a process that exited, a drain or a backoff that ran out, a
    /// shutdown step whose deadline passed.
    fn tick(&mut self, now: Instant) -> ControlFlow<()> {
        let state = std::mem::replace(&mut self.state, State::Idle);
        self.state = match state {
            State::Live(mut process) => match process.child.try_wait() {
                Ok(None) if process.deadline.is_some_and(|deadline| now >= deadline) => {
                    self.drain(process, Cause::Timeout, now)
                }
                Ok(None) => State::Live(process),
                Ok(Some(_)) | Err(_) => self.drain(process, Cause::Gone, now),
            },
            // Everything a reader sent before its EOF is ahead of the loss, so the crash text
            // comes first and no late reply lands after the session let go of its requests.
            State::Draining {
                readers,
                reason,
                until,
            } => {
                if (readers.stdout && readers.stderr) || now >= until {
                    self.generation = self.generation.next();
                    self.report(transport::Event::Lost {
                        generation: self.generation,
                        reason,
                    });
                    State::Waiting
                } else {
                    State::Draining {
                        readers,
                        reason,
                        until,
                    }
                }
            }
            State::Backoff { until } if now >= until => self.connect(),
            State::Closing {
                mut process,
                mut sequence,
                ending,
            } => {
                let over = match process.child.try_wait() {
                    Ok(Some(_)) | Err(_) => true,
                    Ok(None) if now >= sequence.until() => {
                        sequence.expired(&process.link, self.settings.grace, now)
                            == lifecycle::Next::Kill
                    }
                    Ok(None) => false,
                };
                if !over {
                    State::Closing {
                        process,
                        sequence,
                        ending,
                    }
                } else {
                    // Whatever the exit status, even after a kill, this is the shutdown.
                    let _ = process.end();
                    match ending {
                        Ending::Stopped => {
                            self.report(transport::Event::Stopped(client::Reason::Shutdown));
                            return ControlFlow::Break(());
                        }
                        Ending::Idle => State::Idle,
                    }
                }
            }
            state @ (State::Backoff { .. } | State::Waiting | State::Idle) => state,
        };
        ControlFlow::Continue(())
    }

    /// Starts the shutdown sequence on `process`, behind everything the client queued.
    fn close(&self, process: Process, handshake: Handshake, ending: Ending, now: Instant) -> State {
        process.readers.closing.store(true, Ordering::Relaxed);
        let sequence =
            lifecycle::Sequence::begin(&process.link, handshake, self.settings.grace, now);
        State::Closing {
            process,
            sequence,
            ending,
        }
    }

    /// Kills and reaps `process`, and lets its readers drain for one grace period.
    fn drain(&self, mut process: Process, cause: Cause, now: Instant) -> State {
        let exit = process.end();
        let reason = match cause {
            Cause::Gone => client::Reason::Exited {
                code: exit.code,
                signal: exit.signal,
            },
            Cause::Unresponsive => client::Reason::Unresponsive,
            Cause::Timeout => client::Reason::Timeout,
        };
        State::Draining {
            readers: process.readers,
            reason,
            until: now + self.settings.grace,
        }
    }

    /// Starts connection `generation` at once, with the backoff starting over.
    fn respawn(&mut self, generation: Generation) -> State {
        self.generation = generation;
        self.retry = 0;
        self.connect()
    }

    /// Starts the process of the current generation. The connection is announced before its
    /// readers start, so the client holds it before anything it sends arrives.
    fn connect(&mut self) -> State {
        let delay = self.settings.delay(self.retry);
        self.retry = self.retry.saturating_add(1);
        match launch(
            &mut self.command,
            self.generation,
            &self.settings,
            &self.events,
            &self.sender,
        ) {
            Ok(spawned) => {
                self.report(transport::Event::Reconnected {
                    generation: self.generation,
                    link: Link::Stream(spawned.writer.clone()),
                });
                State::Live(spawned.start(self.settings.initialize_timeout))
            }
            Err(Failure::Spawn(error) | Failure::Thread(error)) => {
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

    fn report(&self, event: transport::Event) {
        // `Events` was dropped: nobody is left to tell.
        let _ = self.events.unbounded_send(event);
    }
}

impl Process {
    /// Reaps the process, killing it first if it still runs, so no zombie is left and two
    /// servers never run at once.
    fn end(&mut self) -> Exit {
        if let Ok(Some(status)) = self.child.try_wait() {
            return Exit::from(status);
        }
        let _ = self.child.kill();
        self.child.wait().map_or(Exit::UNKNOWN, Exit::from)
    }
}

impl Spawned {
    /// Lets the readers start, and hands the process over with its `initialize` deadline armed.
    fn start(self, initialize_timeout: Option<Duration>) -> Process {
        let Self {
            child,
            writer,
            readers,
            gates,
        } = self;
        for gate in gates {
            let _ = gate.send(());
        }
        Process {
            child,
            link: Link::Stream(writer),
            readers,
            deadline: initialize_timeout.map(|timeout| Instant::now() + timeout),
        }
    }

    /// Kills and reaps the process; its readers never start.
    fn abandon(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Exit {
    const UNKNOWN: Self = Self {
        code: None,
        signal: None,
    };
}

impl Lines {
    /// The lines `bytes` completes: `\r` stripped, blank ones skipped, decoded lossily, each
    /// capped at `frame::LINE_LIMIT`.
    fn push(&mut self, mut bytes: &[u8]) -> Vec<String> {
        let mut lines = Vec::new();
        while !bytes.is_empty() {
            let newline = bytes.iter().position(|&b| b == b'\n');
            let part = &bytes[..newline.unwrap_or(bytes.len())];
            if !self.overlong {
                let room = frame::LINE_LIMIT - self.partial.len();
                self.partial
                    .extend_from_slice(&part[..part.len().min(room)]);
                if part.len() > room {
                    lines.extend(self.take());
                    self.overlong = true;
                }
            }
            let Some(newline) = newline else {
                break;
            };
            if !self.overlong {
                lines.extend(self.take());
            }
            self.overlong = false;
            bytes = &bytes[newline + 1..];
        }
        lines
    }

    /// The trailing line without a `\n`, at EOF.
    fn finish(mut self) -> Option<String> {
        if self.overlong {
            return None;
        }
        self.take()
    }

    fn take(&mut self) -> Option<String> {
        let line = self.partial.strip_suffix(b"\r").unwrap_or(&self.partial);
        let text = (!line.is_empty()).then(|| String::from_utf8_lossy(line).into_owned());
        self.partial.clear();
        text
    }
}

/// Starts `command` with piped stdio, the threads that serve it, and the supervisor that owns
/// it from then on.
pub(crate) fn spawn(
    mut command: process::Command,
    settings: Settings,
) -> Result<Started, builder::Error> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let (sender, notices) = mpsc::channel();
    let (events, incoming) = futures_channel::mpsc::unbounded();
    let generation = Generation::FIRST;
    let spawned =
        launch(&mut command, generation, &settings, &events, &sender).map_err(|failure| {
            match failure {
                Failure::Spawn(error) => builder::Error::Spawn(error),
                Failure::Thread(error) => builder::Error::Thread(error),
            }
        })?;
    let writer = spawned.writer.clone();
    let handle = Handle {
        notices: sender.clone(),
    };
    let (handoff, slot) = mpsc::sync_channel::<(process::Command, Spawned)>(1);
    let supervisor = move || {
        if let Ok((command, spawned)) = slot.recv() {
            let supervisor = Supervisor {
                command,
                generation,
                state: State::Live(spawned.start(settings.initialize_timeout)),
                retry: 1,
                settings,
                events,
                notices,
                sender,
            };
            supervisor.run();
        }
    };
    // The supervisor gets its process only once its thread runs, so a failed start leaves the
    // process here to be killed.
    if let Err(error) = transport::start("scrive-lsp supervisor".to_owned(), supervisor) {
        spawned.abandon();
        return Err(builder::Error::Thread(error));
    }
    handoff
        .send((command, spawned))
        .expect("the supervisor waits for its process before anything else");
    Ok(Started {
        link: Link::Stream(writer),
        control: transport::Control::Stdio(handle),
        inbound: transport::Inbound::Channel(incoming),
    })
}

/// Starts `command` as connection `generation`, with its writer and its readers, which wait for
/// [`Spawned::start`].
fn launch(
    command: &mut process::Command,
    generation: Generation,
    settings: &Settings,
    events: &Feed,
    notices: &mpsc::Sender<Notice>,
) -> Result<Spawned, Failure> {
    let mut child = command.spawn().map_err(Failure::Spawn)?;
    let stdin = child.stdin.take().expect("stdin is piped");
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");

    let (writer, outgoing) = Writer::new(generation, settings.limit, notices);
    let closing = Arc::new(AtomicBool::new(false));
    let (stdout_gate, stdout_opened) = mpsc::channel();
    let (stderr_gate, stderr_opened) = mpsc::channel::<()>();

    let writing = move || outgoing.write(stdin, drop);
    let reading = {
        let reader = Reader {
            events: events.clone(),
            notices: notices.clone(),
            generation,
            pipe: Pipe::Stdout,
            closing: Arc::clone(&closing),
        };
        move || {
            if stdout_opened.recv().is_ok() {
                reader.read(stdout);
            }
        }
    };
    let draining = {
        let (events, notices) = (events.clone(), notices.clone());
        move || {
            if stderr_opened.recv().is_ok() {
                drain(stderr, &events, &notices, generation);
            }
        }
    };
    let name = |role: &str| format!("scrive-lsp {role} #{generation}");
    let started = transport::start(name("writer"), writing)
        .and_then(|()| transport::start(name("reader"), reading))
        .and_then(|()| transport::start(name("stderr"), draining));
    if let Err(error) = started {
        // The threads already running end on their own: the writer's queue closes with
        // `writer`, and the readers' gates close unopened.
        let _ = child.kill();
        let _ = child.wait();
        return Err(Failure::Thread(error));
    }
    Ok(Spawned {
        child,
        writer,
        readers: Readers {
            generation,
            stdout: false,
            stderr: false,
            closing,
        },
        gates: [stdout_gate, stderr_gate],
    })
}

/// Reads stderr until EOF, one log event per read with one entry per line, then tells the
/// supervisor.
fn drain(
    mut stderr: impl Read,
    events: &Feed,
    notices: &mpsc::Sender<Notice>,
    generation: Generation,
) {
    let mut lines = Lines::default();
    let mut chunk = vec![0; reader::CHUNK];
    loop {
        match stderr.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => send_lines(lines.push(&chunk[..read]), events),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    send_lines(lines.finish().into_iter().collect(), events);
    let _ = notices.send(Notice::Ended {
        generation,
        pipe: Pipe::Stderr,
    });
}

fn send_lines(lines: Vec<String>, events: &Feed) {
    if lines.is_empty() {
        return;
    }
    let entries: Arc<[log::Entry]> = lines.into_iter().map(log::Entry::stderr).collect();
    let _ = events.unbounded_send(transport::Event::Log(entries));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A character split between two reads decodes whole.
    #[test]
    fn stderr_lines_keep_a_character_split_across_reads() {
        let mut lines = Lines::default();
        assert!(
            lines.push(b"caf\xC3").is_empty(),
            "the line is not complete"
        );
        assert_eq!(lines.push(b"\xA9\n"), ["café"], "the character is whole");
    }

    /// A line past 64 KiB is cut there, and the rest of it is dropped up to its `\n`.
    #[test]
    fn stderr_lines_are_capped_at_64_kib() {
        let mut lines = Lines::default();
        let long = vec![b'a'; frame::LINE_LIMIT + 10];
        assert_eq!(
            lines.push(&long),
            ["a".repeat(frame::LINE_LIMIT)],
            "the head is kept"
        );
        assert!(lines.push(b"tail").is_empty(), "the tail is dropped");
        assert_eq!(
            lines.push(b"tail\nnext\n"),
            ["next"],
            "the next line is whole"
        );
    }

    /// `\r\n` endings lose their `\r`, and blank lines are skipped.
    #[test]
    fn stderr_lines_strip_carriage_returns_and_skip_blank_lines() {
        let mut lines = Lines::default();
        assert_eq!(
            lines.push(b"one\r\n\r\n\ntwo\n"),
            ["one", "two"],
            "two lines"
        );
    }

    /// At EOF the line without a `\n` is flushed.
    #[test]
    fn stderr_flushes_its_unterminated_last_line_at_eof() {
        let mut lines = Lines::default();
        assert!(lines.push(b"last words").is_empty(), "no line yet");
        assert_eq!(lines.finish().as_deref(), Some("last words"), "flushed");
    }
}
