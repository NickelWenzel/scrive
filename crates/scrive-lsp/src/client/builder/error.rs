//! Why a bridge could not start.

#[cfg(not(target_family = "wasm"))]
use std::io;
#[cfg(not(target_family = "wasm"))]
use std::net::SocketAddr;

/// Why [`Builder`](super::Builder)'s terminal method could not start the connection.
///
/// Which variants exist depends on the target and on the `websocket` feature, and cargo unifies
/// features across a build: any crate in the tree that turns `websocket` on adds variants. A
/// match on it must therefore end with a wildcard arm, or use `if let`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[cfg(not(target_family = "wasm"))]
    /// The server process could not be started, for example because the program is not found.
    #[error("the language server could not be started: {0}")]
    Spawn(#[source] io::Error),
    #[cfg(not(target_family = "wasm"))]
    /// A thread the bridge needs, or a WebSocket thread's event poll, could not be created.
    #[error("a language server thread could not be created: {0}")]
    Thread(#[source] io::Error),
    #[cfg(not(target_family = "wasm"))]
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
    #[cfg(all(feature = "websocket", not(target_family = "wasm")))]
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
