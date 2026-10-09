//! The byte stream a native WebSocket runs over: the socket itself, or rustls on it.

use std::io::{self, Read, Write};

/// A connection's socket, plain or with TLS.
pub(super) enum Stream {
    Plain(mio::net::TcpStream),
    Tls(Box<Tls>),
}

/// rustls over a socket that may be non-blocking. Unlike `rustls::StreamOwned`, a read hands out
/// plaintext rustls already holds even while a write is blocked, and a write never reports zero
/// bytes for a full buffer: either stalls an edge-triggered loop that waits on the socket.
pub(super) struct Tls {
    connection: rustls::ClientConnection,
    socket: mio::net::TcpStream,
}

impl Stream {
    /// The socket underneath, for registering it with a poll.
    pub(super) fn socket(&mut self) -> &mut mio::net::TcpStream {
        match self {
            Stream::Plain(socket) => socket,
            Stream::Tls(tls) => &mut tls.socket,
        }
    }
}

impl Tls {
    /// Runs the TLS handshake on `socket`, which must be blocking, so its timeouts bound it.
    pub(super) fn handshake(
        mut connection: rustls::ClientConnection,
        mut socket: mio::net::TcpStream,
    ) -> io::Result<Self> {
        while connection.is_handshaking() {
            connection.complete_io(&mut socket)?;
        }
        // The loop feeds one message at a time and flushes it before the next, so the buffer
        // holds at most one message.
        connection.set_buffer_limit(None);
        Ok(Self { connection, socket })
    }

    /// Writes TLS records until none are left or the socket would block.
    fn send(&mut self) -> io::Result<()> {
        while self.connection.wants_write() {
            if self.connection.write_tls(&mut self.socket)? == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
        }
        Ok(())
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(socket) => socket.read(buf),
            Stream::Tls(tls) => tls.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(socket) => socket.write(buf),
            Stream::Tls(tls) => tls.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(socket) => socket.flush(),
            Stream::Tls(tls) => tls.flush(),
        }
    }
}

impl Read for Tls {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.connection.reader().read(buf) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                // Includes a peer that closed without `close_notify`: `UnexpectedEof`.
                result => return result,
            }
            self.connection.read_tls(&mut self.socket)?;
            self.connection
                .process_new_packets()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            // Records the peer asked for, such as a key update's answer. A failure here shows on
            // the next flush.
            let _ = self.send();
        }
    }
}

impl Write for Tls {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.connection.writer().write(buf)?;
        // The bytes are taken either way; a socket that blocks keeps them for `flush`.
        let _ = self.send();
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.connection.writer().flush()?;
        self.send()
    }
}
