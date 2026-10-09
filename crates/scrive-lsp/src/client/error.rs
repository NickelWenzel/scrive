//! What went wrong with a message or a command, reported as [`Update::Error`](crate::Update::Error).

use core::fmt;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use scrive_core::DocId;

use crate::{message, uri};

/// What went wrong with a message from the server or a command, reported as
/// [`Update::Error`](crate::Update::Error) or by [`Client::open`](super::Client::open).
#[derive(Clone, Debug, thiserror::Error)]
pub enum Error {
    /// A notification or response payload did not decode as the method's type.
    #[error("`{method}` payload does not decode: {source}")]
    Decode {
        /// The method whose payload failed.
        method: String,
        /// The decoding error.
        source: Arc<serde_json::Error>,
    },
    /// A message from the server is not JSON-RPC.
    #[error("the server sent a message that is not JSON-RPC: {source}")]
    Envelope {
        /// Why it does not decode.
        source: Arc<serde_json::Error>,
    },
    /// The server answered a request with an error.
    #[error("server failed `{method}`: {error}")]
    Server {
        /// The document the request was for; `None` for `initialize`.
        doc_id: Option<DocId>,
        /// The request's method.
        method: String,
        /// The server's error object.
        error: Server,
    },
    /// Another document is already registered under this URI.
    #[error("`{uri}` is already open as another document")]
    DuplicateUri {
        /// The normalized URI.
        uri: uri::Key,
    },
    /// A rename's edit touches a document that moved since the request, opened or closed since,
    /// or is not at the version the edit names. None of the rename applies.
    #[error("the rename's edit for `{uri}` is stale: the document changed since the request")]
    StaleEdit {
        /// The stale document.
        uri: uri::Key,
    },
    /// A rename's edit asks for a file operation, which the client does not perform. None of the
    /// rename applies.
    #[error("unsupported workspace edit: a `{operation}` file operation")]
    Unsupported {
        /// The operation's `kind`: `create`, `rename` or `delete`.
        operation: String,
    },
    /// A server message announced a body longer than the client decodes (64 MiB). It was
    /// dropped unread, and a dropped reply never settles its request. Above 1 GiB, and always
    /// on a WebSocket, the connection ended too.
    #[error("a server message announced {length} bytes, more than the client decodes")]
    Oversized {
        /// The announced length: the `Content-Length`, or the WebSocket message's.
        length: u64,
    },
    /// An attempt to start the server again failed; the next one follows after a backoff,
    /// unless the restart policy gives up.
    #[error("could not start the server again: {source}")]
    Reconnect {
        /// Why the attempt failed.
        source: Arc<io::Error>,
    },
    /// The server did not answer `initialize` within `after`.
    #[error("the server did not answer `initialize` within {after:?}")]
    Timeout {
        /// The builder's `initialize_timeout`.
        after: Duration,
    },
    /// This bridge cannot start its server again: an in-process server belongs to the host, and
    /// a server that dialed in to [`listen`](super::Builder::listen) is started again by
    /// whoever started it.
    #[error("this bridge cannot restart its server")]
    Unrestartable,
}

/// A JSON-RPC error object from the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    code: i64,
    message: String,
}

impl fmt::Display for Server {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl Server {
    pub(crate) fn new(error: message::Error) -> Self {
        Self {
            code: error.code,
            message: error.message,
        }
    }

    /// The JSON-RPC or LSP error code, e.g. `-32603` (internal error).
    #[must_use]
    pub fn code(&self) -> i64 {
        self.code
    }

    /// The server's description; empty when it sent none.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}
