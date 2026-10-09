//! The native WebSocket bridge: tungstenite over a mio socket, `wss://` through rustls, one I/O
//! thread per connection. The socket worker dials, loses and shuts it down as it does TCP.

mod pump;
mod stream;

use std::io;
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use mio::{Interest, Poll, Waker};
use tungstenite::handshake::client::ClientHandshake;
use tungstenite::http::Uri;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{HandshakeError, WebSocket};

use self::pump::{Close, Command, Pump};
use self::stream::{Stream, Tls};
use super::Scheme;
use crate::client::builder;
use crate::transport::writer::Outgoing;
use crate::transport::{self, frame, tcp, Feed, Generation, Link, Notice, Writer};

/// The longest message read, the same as the stream bridges decode.
const LIMIT: usize = frame::BODY_LIMIT as usize;
/// Bounds each read and write of the TLS and WebSocket handshakes, as a dial is bounded.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// A validated `ws://` or `wss://` URL and, for `wss://`, the TLS configuration to dial with.
pub(crate) struct Endpoint {
    uri: Uri,
    /// The host and port dialed, resolved again on each dial.
    address: (String, u16),
    tls: Option<Secure>,
}

/// What a `wss://` dial verifies the server against.
struct Secure {
    config: Arc<rustls::ClientConfig>,
    name: rustls::pki_types::ServerName<'static>,
}

/// A connection that completed its handshakes, before its thread runs.
pub(crate) struct Socket(Box<WebSocket<Stream>>);

/// A connection's outgoing queue, and the poll its thread waits on, which the queue's link wakes.
pub(crate) struct Queue {
    writer: Writer,
    outgoing: Outgoing,
    poll: Poll,
    waker: Arc<Waker>,
}

/// A connection's I/O thread, started and waiting for its queue.
pub(crate) struct Pending {
    socket: Socket,
    pump: mpsc::Sender<Pump>,
    generation: Generation,
    events: Feed,
    notices: mpsc::Sender<Notice>,
}

/// The worker's handle on a connection's I/O thread, which it releases rather than joins.
pub(crate) struct Connection {
    commands: mpsc::Sender<Command>,
    waker: Arc<Waker>,
}

impl Endpoint {
    /// Parses `url`. A `wss://` URL dials with `tls`, or else with the webpki roots; `tls` is
    /// ignored for `ws://`.
    pub(crate) fn new(
        url: &str,
        tls: Option<Arc<rustls::ClientConfig>>,
    ) -> Result<Self, builder::Error> {
        let invalid = |reason| builder::Error::Url {
            url: url.to_owned(),
            reason,
        };
        let uri: Uri = url
            .parse()
            .map_err(|_| invalid(builder::error::Url::Unparsable))?;
        let scheme = super::scheme(&uri).map_err(invalid)?;
        let Some(host) = uri.host() else {
            return Err(invalid(builder::error::Url::Unparsable));
        };
        let host = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host)
            .to_owned();
        let port = uri.port_u16().unwrap_or(match scheme {
            Scheme::Plain => 80,
            Scheme::Secure => 443,
        });
        let tls = match scheme {
            Scheme::Plain => None,
            Scheme::Secure => Some(Secure {
                config: tls.map_or_else(default_tls, Ok)?,
                name: rustls::pki_types::ServerName::try_from(host.clone())
                    .map_err(|_| invalid(builder::error::Url::Unparsable))?,
            }),
        };
        Ok(Self {
            uri,
            address: (host, port),
            tls,
        })
    }

    /// Dials once, within `deadline` if there is one, and runs the TLS and WebSocket handshakes
    /// in blocking mode, each read and write bounded. Returns the socket non-blocking.
    pub(crate) fn dial(&self, deadline: Option<Instant>) -> io::Result<Socket> {
        let stream = tcp::dial(&self.address, deadline)?;
        stream.set_nodelay(true)?;
        // mio's stream has no timeout or blocking-mode setters, so a clone of the same socket
        // sets them. On Windows mio can only register its own stream type, and a WebSocket's
        // stream type is fixed at the handshake, so the conversion comes first.
        let control = stream.try_clone()?;
        control.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
        control.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
        let socket = mio::net::TcpStream::from_std(stream);
        let stream = match &self.tls {
            None => Stream::Plain(socket),
            Some(secure) => {
                let connection =
                    rustls::ClientConnection::new(Arc::clone(&secure.config), secure.name.clone())
                        .map_err(io::Error::other)?;
                Stream::Tls(Box::new(
                    Tls::handshake(connection, socket).map_err(timed_out)?,
                ))
            }
        };
        let config = WebSocketConfig::default()
            .max_message_size(Some(LIMIT))
            .max_frame_size(Some(LIMIT));
        let (socket, _response) =
            tungstenite::client::client_with_config(self.uri.clone(), stream, Some(config))
                .map_err(handshake_failed)?;
        control.set_read_timeout(None)?;
        control.set_write_timeout(None)?;
        control.set_nonblocking(true)?;
        Ok(Socket(Box::new(socket)))
    }
}

impl Queue {
    /// The queue of connection `generation`, whose guard trips past `limit` unwritten bytes.
    pub(crate) fn new(
        generation: Generation,
        limit: usize,
        notices: &mpsc::Sender<Notice>,
    ) -> io::Result<Self> {
        let poll = Poll::new()?;
        let waker = Arc::new(Waker::new(poll.registry(), pump::WAKE)?);
        let (writer, outgoing) = Writer::new(generation, limit, notices);
        Ok(Self {
            writer,
            outgoing,
            poll,
            waker,
        })
    }

    /// The client's handle on this queue.
    pub(crate) fn link(&self) -> Link {
        Link::Websocket {
            writer: self.writer.clone(),
            waker: Arc::clone(&self.waker),
        }
    }
}

impl Pending {
    /// Starts the I/O thread of connection `generation`; it waits for [`go`](Self::go), so
    /// nothing the server says reaches the client before the connection is announced.
    pub(crate) fn start(
        socket: Socket,
        generation: Generation,
        events: &Feed,
        notices: &mpsc::Sender<Notice>,
    ) -> io::Result<Self> {
        let (pump, filled) = mpsc::channel::<Pump>();
        transport::start(
            format!("scrive-lsp websocket io #{generation}"),
            move || {
                if let Ok(pump) = filled.recv() {
                    pump.run();
                }
            },
        )?;
        Ok(Self {
            socket,
            pump,
            generation,
            events: events.clone(),
            notices: notices.clone(),
        })
    }

    /// Hands the thread its socket and queue and lets it run. Returns the worker's handle, and
    /// the flag that tells the thread to look for the shutdown reply.
    pub(crate) fn go(self, queue: Queue) -> (Connection, Arc<AtomicBool>) {
        let (commands, received) = mpsc::channel();
        let closing = Arc::new(AtomicBool::new(false));
        let connection = Connection {
            commands,
            waker: Arc::clone(&queue.waker),
        };
        let pump = Pump {
            socket: *self.socket.0,
            poll: queue.poll,
            outgoing: queue.outgoing,
            commands: received,
            generation: self.generation,
            events: self.events,
            notices: self.notices,
            closing: Arc::clone(&closing),
            close: Close::Open,
            interest: Interest::READABLE,
        };
        // A thread that already ended drops the pump, and its socket with it.
        let _ = self.pump.send(pump);
        (connection, closing)
    }
}

impl Connection {
    /// Drops the socket at once and lets go of the thread.
    pub(crate) fn end(self) {
        let _ = self.commands.send(Command::Abort);
        // A thread that is gone needs no wake.
        let _ = self.waker.wake();
    }
}

/// rustls with the ring provider and the webpki roots. The provider is named, never the process
/// default, which panics when no provider or two are compiled in.
fn default_tls() -> Result<Arc<rustls::ClientConfig>, builder::Error> {
    let roots = webpki_roots::TLS_SERVER_ROOTS
        .iter()
        .cloned()
        .collect::<rustls::RootCertStore>();
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(Arc::new(config))
}

/// A handshake read or write that ran out of time is `WouldBlock` on Unix and `TimedOut` on
/// Windows.
fn timed_out(error: io::Error) -> io::Error {
    match error.kind() {
        io::ErrorKind::WouldBlock => io::ErrorKind::TimedOut.into(),
        _ => error,
    }
}

fn handshake_failed(error: HandshakeError<ClientHandshake<Stream>>) -> io::Error {
    match error {
        HandshakeError::Interrupted(_) => io::ErrorKind::TimedOut.into(),
        HandshakeError::Failure(tungstenite::Error::Io(error)) => timed_out(error),
        // A refused upgrade keeps its HTTP status in the message.
        HandshakeError::Failure(error) => io::Error::other(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default configuration uses ring's cipher suites.
    #[test]
    fn default_tls_uses_the_ring_provider() {
        let config = default_tls().expect("the default configuration builds");
        let suites = |suites: &[rustls::SupportedCipherSuite]| {
            suites.iter().map(|suite| suite.suite()).collect::<Vec<_>>()
        };
        assert_eq!(
            suites(&config.crypto_provider().cipher_suites),
            suites(&rustls::crypto::ring::default_provider().cipher_suites),
            "ring's suites"
        );
    }

    /// Building the default configuration installs no process-wide provider.
    #[test]
    fn default_tls_installs_no_process_provider() {
        let _ = default_tls().expect("the default configuration builds");
        assert!(
            rustls::crypto::CryptoProvider::get_default().is_none(),
            "no process default"
        );
    }

    /// An IPv6 literal is dialed without its brackets, on the scheme's port unless one is given.
    #[test]
    fn ipv6_hosts_lose_their_brackets() {
        let endpoint = Endpoint::new("ws://[::1]:9/", None).expect("the URL parses");
        assert_eq!(endpoint.address, ("::1".to_owned(), 9), "the given port");
        let endpoint = Endpoint::new("wss://[::1]/", None).expect("the URL parses");
        assert_eq!(endpoint.address, ("::1".to_owned(), 443), "wss's port");
    }
}
