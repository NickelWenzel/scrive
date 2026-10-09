//! Why a bridge could not start.

use std::io;

/// Why [`Builder`](super::Builder)'s terminal method could not start the connection.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The server process could not be started, for example because the program is not found.
    #[error("the language server could not be started: {0}")]
    Spawn(#[source] io::Error),
    /// A thread the bridge needs could not be created.
    #[error("a language server thread could not be created: {0}")]
    Thread(#[source] io::Error),
}
