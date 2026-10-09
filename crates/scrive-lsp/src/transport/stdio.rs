//! The stdio bridge: a language server as a child process, talked to over its stdin and stdout.
//! Four named threads serve it: a writer, a stdout reader, a stderr reader, and a supervisor
//! that owns the process.

use std::io::{self, Read, Write};
use std::process::{self, Child, ChildStdin, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use super::frame;
use super::lifecycle::{self, Handshake, Lifecycle};
use crate::client::builder;
use crate::{client, log, transport};

/// How long the supervisor waits for a command before checking the process again.
const IDLE: Duration = Duration::from_secs(1);
/// How often it checks while a grace period runs.
const POLL: Duration = Duration::from_millis(50);
/// Bytes per read from the server's stdout and stderr.
const CHUNK: usize = 64 * 1024;
/// Win32's `CREATE_NO_WINDOW` process creation flag.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The channel the bridge's threads report on.
type Feed = futures_channel::mpsc::UnboundedSender<transport::Event>;

/// A started server: what the client keeps, and the stream its traffic arrives on.
pub(crate) struct Started {
    pub(crate) writer: Writer,
    pub(crate) control: Control,
    pub(crate) events: futures_channel::mpsc::UnboundedReceiver<transport::Event>,
}

/// One connection's outgoing queue. Bodies go to the writer thread, which frames and writes them
/// in order. Clones share the queue; the server's stdin closes once every clone is gone.
#[derive(Clone, Debug)]
pub(crate) struct Writer {
    queue: mpsc::Sender<Arc<[u8]>>,
    backlog: Arc<Backlog>,
    /// Where the guard reports its trip.
    commands: mpsc::Sender<Command>,
}

/// Bytes queued for the server but not yet written, and the hung-server guard on them.
#[derive(Debug)]
struct Backlog {
    unwritten: AtomicUsize,
    limit: usize,
    tripped: AtomicBool,
}

/// The client's line to the supervisor.
#[derive(Debug)]
pub(crate) struct Control {
    commands: mpsc::Sender<Command>,
}

/// What the supervisor is told.
#[derive(Debug)]
enum Command {
    Lifecycle(Lifecycle),
    /// A reader reached EOF, or stopped reading.
    Ended(Pipe),
    /// The backlog passed its limit.
    Unresponsive,
}

#[derive(Clone, Copy, Debug)]
enum Pipe {
    Stdout,
    Stderr,
}

/// Owns the server process: reaps it, kills it, and reports how it stopped.
struct Supervisor {
    child: Child,
    /// `None` once every sender is gone; the supervisor then sleeps between checks.
    commands: Option<mpsc::Receiver<Command>>,
    events: Feed,
    /// Its own handle on the queue, so `shutdown` and `exit` queue behind the client's messages.
    writer: Option<Writer>,
    grace: Duration,
}

/// What the supervisor has seen.
#[derive(Debug, Default)]
struct Watch {
    /// Set once the process is reaped.
    exit: Option<Exit>,
    stdout: bool,
    stderr: bool,
    /// When the shutdown grace period ends; set by `Lifecycle::Shutdown`.
    shutdown: Option<Instant>,
    unresponsive: bool,
    /// When the readers' grace period ends; set once the process is reaped.
    drained: Option<Instant>,
}

/// How the process ended.
#[derive(Clone, Copy, Debug)]
struct Exit {
    code: Option<i32>,
    signal: Option<i32>,
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

impl Writer {
    /// Queues one serialized message. Dropped once the guard has tripped or the writer is gone.
    pub(crate) fn send(&self, body: Arc<[u8]>) {
        if self.backlog.tripped.load(Ordering::Relaxed) {
            return;
        }
        let length = body.len();
        // Counted before the send and given back after the write, so the count never goes
        // below zero.
        let unwritten = self.backlog.unwritten.fetch_add(length, Ordering::Relaxed) + length;
        if self.queue.send(body).is_err() {
            self.backlog.unwritten.fetch_sub(length, Ordering::Relaxed);
            return;
        }
        // Checked here, on the sending thread: the writer thread is the one stuck in
        // `write_all` when the server stops reading.
        if unwritten > self.backlog.limit && !self.backlog.tripped.swap(true, Ordering::Relaxed) {
            let _ = self.commands.send(Command::Unresponsive);
        }
    }
}

impl Backlog {
    fn new(limit: usize) -> Self {
        Self {
            unwritten: AtomicUsize::new(0),
            limit,
            tripped: AtomicBool::new(false),
        }
    }
}

impl Control {
    /// Tells the supervisor what the client decided. Ignored once the supervisor has stopped.
    pub(crate) fn send(&self, lifecycle: Lifecycle) {
        let _ = self.commands.send(Command::Lifecycle(lifecycle));
    }
}

impl Supervisor {
    fn run(mut self) {
        let mut watch = Watch::default();
        loop {
            let timeout = if watch.armed() { POLL } else { IDLE };
            match self.next(timeout) {
                Some(Command::Lifecycle(Lifecycle::Shutdown { handshake })) => {
                    self.shut_down(handshake, &mut watch);
                }
                Some(Command::Ended(Pipe::Stdout)) => {
                    watch.stdout = true;
                    // A live server whose stdout closed can't answer any more. During a shutdown
                    // the grace deadline decides instead, so a server finishing its exit isn't
                    // cut short.
                    if watch.exit.is_none() && watch.shutdown.is_none() {
                        watch.exit = Some(self.reap());
                    }
                }
                Some(Command::Ended(Pipe::Stderr)) => watch.stderr = true,
                Some(Command::Unresponsive) if watch.exit.is_none() => {
                    watch.unresponsive = true;
                    watch.exit = Some(self.kill());
                }
                Some(Command::Unresponsive) | None => {}
            }
            let now = Instant::now();
            if watch.exit.is_none() {
                watch.exit = match self.child.try_wait() {
                    Ok(Some(status)) => Some(Exit::from(status)),
                    Ok(None) if watch.shutdown.is_some_and(|deadline| now >= deadline) => {
                        Some(self.kill())
                    }
                    Ok(None) => None,
                    Err(_) => Some(Exit::UNKNOWN),
                };
            }
            if let Some(exit) = watch.exit {
                self.writer = None;
                // Everything a reader sent before its EOF is ahead of this `Stopped`, so no late
                // frame or stderr line follows it.
                let drained = *watch.drained.get_or_insert(now + self.grace);
                if (watch.stdout && watch.stderr) || now >= drained {
                    let _ = self
                        .events
                        .unbounded_send(transport::Event::Stopped(watch.reason(exit)));
                    return;
                }
            }
        }
    }

    /// The next command, or `None` once `timeout` passes.
    fn next(&mut self, timeout: Duration) -> Option<Command> {
        let Some(commands) = &self.commands else {
            thread::sleep(timeout);
            return None;
        };
        match commands.recv_timeout(timeout) {
            Ok(command) => Some(command),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.commands = None;
                None
            }
        }
    }

    /// Queues `shutdown` (after a handshake) and `exit` behind the client's messages, and starts
    /// the grace period: on the command, since a writer blocked on a full pipe never closes
    /// stdin.
    fn shut_down(&mut self, handshake: Handshake, watch: &mut Watch) {
        if watch.shutdown.is_some() {
            return;
        }
        watch.shutdown = Some(Instant::now() + self.grace);
        if let Some(writer) = self.writer.take() {
            match handshake {
                Handshake::Done => writer.send(lifecycle::shutdown()),
                Handshake::Pending => {}
            }
            writer.send(lifecycle::exit());
        }
    }

    /// Reaps a process that may already be exiting, killing it if it isn't.
    fn reap(&mut self) -> Exit {
        match self.child.try_wait() {
            Ok(Some(status)) => Exit::from(status),
            Ok(None) | Err(_) => self.kill(),
        }
    }

    /// Kills and reaps, so no zombie is left.
    fn kill(&mut self) -> Exit {
        let _ = self.child.kill();
        self.child.wait().map_or(Exit::UNKNOWN, Exit::from)
    }
}

impl Watch {
    /// Whether a deadline is running, so the supervisor polls at `POLL`.
    fn armed(&self) -> bool {
        self.shutdown.is_some() || self.exit.is_some()
    }

    /// A shutdown the client asked for wins, even if the process then had to be killed.
    fn reason(&self, exit: Exit) -> client::Reason {
        if self.shutdown.is_some() {
            client::Reason::Shutdown
        } else if self.unresponsive {
            client::Reason::Unresponsive
        } else {
            client::Reason::Exited {
                code: exit.code,
                signal: exit.signal,
            }
        }
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

/// Starts `command` with piped stdio and the four threads that serve it.
pub(crate) fn spawn(
    command: &mut process::Command,
    grace: Duration,
    limit: usize,
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
    let mut child = command.spawn().map_err(builder::Error::Spawn)?;
    let stdin = child.stdin.take().expect("stdin is piped");
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");

    let (queue, outgoing) = mpsc::channel();
    let (commands, received) = mpsc::channel();
    let (events, incoming) = futures_channel::mpsc::unbounded();
    let backlog = Arc::new(Backlog::new(limit));
    let writer = Writer {
        queue,
        backlog: Arc::clone(&backlog),
        commands: commands.clone(),
    };
    let (handoff, slot) = mpsc::sync_channel(1);

    let supervisor = {
        let (events, writer) = (events.clone(), writer.clone());
        move || {
            if let Ok(child) = slot.recv() {
                let supervisor = Supervisor {
                    child,
                    commands: Some(received),
                    events,
                    writer: Some(writer),
                    grace,
                };
                supervisor.run();
            }
        }
    };
    let reader = {
        let (events, commands) = (events.clone(), commands.clone());
        move || read(stdout, &events, &commands)
    };
    let stderr_reader = {
        let commands = commands.clone();
        move || drain(stderr, &events, &commands)
    };
    // The supervisor starts first and gets its child only once every thread runs, so a failed
    // spawn leaves the child here to be killed.
    let started = start("supervisor", supervisor)
        .and_then(|()| start("writer", move || write(stdin, &outgoing, &backlog)))
        .and_then(|()| start("reader", reader))
        .and_then(|()| start("stderr", stderr_reader));
    if let Err(error) = started {
        // The threads already running end on their own: the supervisor's hand-off closes, and
        // the readers see EOF once the process is gone.
        let _ = child.kill();
        let _ = child.wait();
        return Err(builder::Error::Thread(error));
    }
    handoff
        .send(child)
        .expect("the supervisor waits for its child before anything else");
    Ok(Started {
        writer,
        control: Control { commands },
        events: incoming,
    })
}

fn start(role: &str, body: impl FnOnce() + Send + 'static) -> io::Result<()> {
    thread::Builder::new()
        .name(format!("scrive-lsp {role}"))
        .spawn(body)
        .map(drop)
}

/// Frames and writes every queued body until every `Writer` is gone, then closes stdin by
/// dropping it. A write error means the server died; its stdout EOF reports that.
fn write(mut stdin: ChildStdin, outgoing: &mpsc::Receiver<Arc<[u8]>>, backlog: &Backlog) {
    for body in outgoing {
        let written = stdin.write_all(&frame::encode(&body));
        backlog.unwritten.fetch_sub(body.len(), Ordering::Relaxed);
        if written.is_err() {
            return;
        }
    }
}

/// Decodes stdout until EOF, then tells the supervisor. It reads on after `Events` is dropped,
/// so the server never blocks on a full pipe.
fn read(mut stdout: impl Read, events: &Feed, commands: &mpsc::Sender<Command>) {
    let mut decoder = frame::Decoder::default();
    let mut chunk = vec![0; CHUNK];
    loop {
        let read = match stdout.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        if !deliver(decoder.push(&chunk[..read]), events) {
            break;
        }
    }
    if let Some(line) = decoder.finish() {
        let _ = events.unbounded_send(transport::Event::Log(Arc::from([log::Entry::stdout(line)])));
    }
    let _ = commands.send(Command::Ended(Pipe::Stdout));
}

/// Forwards one read's items in order, consecutive noise lines as one log batch. `false` once
/// the stream is corrupt.
fn deliver(items: Vec<frame::Item>, events: &Feed) -> bool {
    let mut noise = Vec::new();
    for item in items {
        let (event, corrupt) = match item {
            frame::Item::Noise(text) => {
                noise.push(log::Entry::stdout(text));
                continue;
            }
            frame::Item::Body(body) => (transport::Event::Message(Arc::from(body)), false),
            frame::Item::Skipped { length } => (oversized(length), false),
            frame::Item::Corrupt { length } => (oversized(length), true),
        };
        flush(&mut noise, events);
        let _ = events.unbounded_send(event);
        if corrupt {
            return false;
        }
    }
    flush(&mut noise, events);
    true
}

fn oversized(length: u64) -> transport::Event {
    transport::Event::Error(client::Error::Oversized { length })
}

/// Sends the batched noise lines as one log event, if there are any.
fn flush(noise: &mut Vec<log::Entry>, events: &Feed) {
    if !noise.is_empty() {
        let _ = events.unbounded_send(transport::Event::Log(Arc::from(std::mem::take(noise))));
    }
}

/// Reads stderr until EOF, one log event per read with one entry per line, then tells the
/// supervisor.
fn drain(mut stderr: impl Read, events: &Feed, commands: &mpsc::Sender<Command>) {
    let mut lines = Lines::default();
    let mut chunk = vec![0; CHUNK];
    loop {
        match stderr.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => send_lines(lines.push(&chunk[..read]), events),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    send_lines(lines.finish().into_iter().collect(), events);
    let _ = commands.send(Command::Ended(Pipe::Stderr));
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

    /// Everything the reader sent, in order.
    fn sent(
        mut incoming: futures_channel::mpsc::UnboundedReceiver<transport::Event>,
    ) -> Vec<transport::Event> {
        let mut events = Vec::new();
        while let Ok(event) = incoming.try_recv() {
            events.push(event);
        }
        events
    }

    /// The texts of a log event's entries, or `None` for any other event.
    fn texts(event: &transport::Event) -> Option<Vec<&str>> {
        let transport::Event::Log(entries) = event else {
            return None;
        };
        Some(entries.iter().map(log::Entry::text).collect())
    }

    fn body(event: &transport::Event) -> Option<&[u8]> {
        let transport::Event::Message(body) = event else {
            return None;
        };
        Some(body)
    }

    /// Frames become messages, and the noise lines between them one log batch per run.
    #[test]
    fn the_reader_forwards_frames_and_batches_noise_per_read() {
        let mut stdout = b"banner\n".to_vec();
        stdout.extend(frame::encode(b"{\"a\":1}"));
        stdout.extend(b"a\nb\n");
        stdout.extend(frame::encode(b"{\"b\":2}"));
        let (events, incoming) = futures_channel::mpsc::unbounded();
        let (commands, received) = mpsc::channel();
        read(stdout.as_slice(), &events, &commands);
        let events = sent(incoming);
        assert_eq!(events.len(), 4, "four events: {events:?}");
        assert_eq!(texts(&events[0]), Some(vec!["banner"]), "the banner");
        assert_eq!(body(&events[1]), Some(&b"{\"a\":1}"[..]), "the first frame");
        assert_eq!(
            texts(&events[2]),
            Some(vec!["a", "b"]),
            "both lines in one batch"
        );
        assert_eq!(
            body(&events[3]),
            Some(&b"{\"b\":2}"[..]),
            "the second frame"
        );
        assert!(
            matches!(received.try_recv(), Ok(Command::Ended(Pipe::Stdout))),
            "the supervisor hears of the EOF"
        );
        assert!(
            matches!(&events[0], transport::Event::Log(entries)
                if entries[0].source() == log::Source::Stdout),
            "the noise is stdout's"
        );
    }

    /// A length above 1 GiB is reported and ends the reading; a frame after it is not
    /// forwarded.
    #[test]
    fn the_reader_stops_on_a_corrupt_length() {
        let mut stdout = b"Content-Length: 2000000000\r\n\r\n".to_vec();
        stdout.extend(frame::encode(b"{}"));
        let (events, incoming) = futures_channel::mpsc::unbounded();
        let (commands, received) = mpsc::channel();
        read(stdout.as_slice(), &events, &commands);
        let events = sent(incoming);
        assert!(
            matches!(
                events.as_slice(),
                [transport::Event::Error(client::Error::Oversized {
                    length: 2_000_000_000
                })]
            ),
            "only the error: {events:?}"
        );
        assert!(
            matches!(received.try_recv(), Ok(Command::Ended(Pipe::Stdout))),
            "the supervisor hears of the end"
        );
    }

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

    /// A writer whose queue nobody drains, with room for `limit` bytes.
    fn stalled(limit: usize) -> (Writer, mpsc::Receiver<Arc<[u8]>>, mpsc::Receiver<Command>) {
        let (queue, outgoing) = mpsc::channel();
        let (commands, received) = mpsc::channel();
        let writer = Writer {
            queue,
            backlog: Arc::new(Backlog::new(limit)),
            commands,
        };
        (writer, outgoing, received)
    }

    /// The first send past the limit trips the guard once; later sends are dropped.
    #[test]
    fn the_guard_trips_once_past_its_limit() {
        let (writer, outgoing, received) = stalled(10);
        writer.send(Arc::from(&b"123456"[..]));
        assert!(
            received.try_recv().is_err(),
            "below the limit nothing trips"
        );
        writer.send(Arc::from(&b"123456"[..]));
        assert!(
            matches!(received.try_recv(), Ok(Command::Unresponsive)),
            "past the limit the guard trips"
        );
        writer.send(Arc::from(&b"123456"[..]));
        assert!(received.try_recv().is_err(), "it trips once");
        assert_eq!(
            outgoing.try_iter().count(),
            2,
            "the send after the trip is dropped"
        );
    }

    /// A send that finds the writer gone takes its bytes back, so a dead writer never trips the
    /// guard.
    #[test]
    fn a_send_to_a_gone_writer_gives_its_bytes_back() {
        let (writer, outgoing, received) = stalled(10);
        drop(outgoing);
        for _ in 0..3 {
            writer.send(Arc::from(&b"123456"[..]));
        }
        assert_eq!(
            writer.backlog.unwritten.load(Ordering::Relaxed),
            0,
            "nothing is counted"
        );
        assert!(received.try_recv().is_err(), "the guard never trips");
    }
}
