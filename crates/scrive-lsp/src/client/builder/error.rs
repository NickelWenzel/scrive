//! Why a bridge could not start.

use std::io;
use std::net::SocketAddr;

/// Why [`Builder`](super::Builder)'s terminal method could not start the connection.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The server process could not be started, for example because the program is not found.
    #[error("the language server could not be started: {0}")]
    Spawn(#[source] io::Error),
    /// A thread the bridge needs could not be created.
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
}
