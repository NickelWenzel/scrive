//! Why a bridge could not start.

use std::io;
use std::net::SocketAddr;

/// Why [`Builder`](super::Builder)'s terminal method could not start the connection.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The server process could not be started, for example because the program is not found.
    #[error("the language server could not be started: {0}")]
    Spawn(#[source] io::Error),
    /// A thread the bridge needs, or a WebSocket thread's event poll, could not be created.
    #[error("a language server thread could not be created: {0}")]
    Thread(#[source] io::Error),
    /// The address [`listen`](super::Builder::listen) was given could not be bound.
    #[error("the client could not listen on {address}: {source}")]
    Bind {
        /// The address `listen` was given.
        address: SocketAddr,
        /// Why binding failed.
        #[source]
        source: io::Error,
    },
    /// The URL [`websocket`](super::Builder::websocket) was given can't be dialed.
    #[cfg(feature = "websocket")]
    #[error("`{url}` is not a WebSocket URL: {reason}")]
    Url {
        /// The URL as given.
        url: String,
        /// What is wrong with it.
        reason: Url,
    },
    /// The default TLS configuration for `wss://` could not be built.
    #[cfg(feature = "websocket")]
    #[error("TLS configuration: {0}")]
    Tls(#[from] rustls::Error),
}

/// What is wrong with a WebSocket URL.
#[cfg(feature = "websocket")]
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Url {
    /// It doesn't parse as a URL with a host.
    #[error("it doesn't parse as a URL with a host")]
    Unparsable,
    /// Its scheme is neither `ws` nor `wss`.
    #[error("its scheme is neither ws nor wss")]
    Scheme,
}
