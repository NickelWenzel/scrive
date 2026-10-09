//! The TCP bridge: a language server on a socket, which the client dials or accepts once. The
//! socket worker carries the connection; each connection gets a writer and a reader thread.

use std::io;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use super::reader::Reader;
use super::writer::Outgoing;
use super::{socket, Feed, Generation, Notice, Pipe, Settings, Started};
use crate::client::builder;
use crate::transport;

// Bounds a dial to one address, so neither a retry nor Drop waits out the OS SYN timeout
// (about 127 s on Linux).
const DIAL_TIMEOUT: Duration = Duration::from_secs(5);

/// Dials the server once, within the deadline if there is one.
pub(crate) type Dial = Box<dyn Fn(Option<Instant>) -> io::Result<TcpStream> + Send>;

/// A connection's reader and writer threads, started and waiting: the writer for its queue, the
/// reader for the go-ahead, so nothing the server says reaches the client before the connection
/// is announced.
pub(crate) struct Threads {
    queue: mpsc::Sender<Outgoing>,
    gate: mpsc::Sender<()>,
    closing: Arc<AtomicBool>,
}

impl Threads {
    /// Starts the reader and the writer of connection `generation` on clones of `stream`.
    pub(crate) fn start(
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

    /// Hands the writer its queue and lets the reader start. Returns the flag that tells the
    /// reader to look for the shutdown reply.
    pub(crate) fn go(self, outgoing: Outgoing) -> Arc<AtomicBool> {
        let _ = self.queue.send(outgoing);
        let _ = self.gate.send(());
        self.closing
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
    let dial: Dial = Box::new(move |deadline| dial(&address, deadline));
    socket::connect(socket::Endpoint::Tcp(dial), budget, settings)
}

/// Binds `address` and starts a worker that accepts exactly one connection. Returns the bound
/// address, with the port the OS picked for port 0.
pub(crate) fn listen(
    address: SocketAddr,
    settings: Settings,
) -> Result<(Started, SocketAddr), builder::Error> {
    let bind = |source| builder::Error::Bind { address, source };
    let listener = TcpListener::bind(address).map_err(bind)?;
    listener.set_nonblocking(true).map_err(bind)?;
    let bound = listener.local_addr().map_err(bind)?;
    let started = socket::listen(listener, settings)?;
    Ok((started, bound))
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
}
