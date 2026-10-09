//! An opt-in record of every message that crosses the connection.

use core::fmt;
use std::sync::Arc;

/// Whether the client records traffic as [`Update::Trace`](crate::Update::Trace). A server
/// process's shutdown handshake (`shutdown`, its reply, and `exit`) is not traced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// No traces.
    #[default]
    Off,
    /// One entry per message, in both directions.
    Messages,
}

/// Which way a message went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// From the server.
    Incoming,
    /// To the server.
    Outgoing,
}

/// One message as it crossed the connection. An outgoing entry records what the client handed
/// to the transport; one sent while there is no connection is traced all the same.
#[derive(Clone)]
pub struct Entry {
    direction: Direction,
    method: Option<Arc<str>>,
    json: Arc<[u8]>,
}

/// A summary only: a message can be megabytes.
impl fmt::Debug for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Entry")
            .field("direction", &self.direction)
            .field("method", &self.method)
            .field("bytes", &self.json.len())
            .finish()
    }
}

impl Entry {
    pub(crate) fn new(direction: Direction, method: Option<&str>, json: Arc<[u8]>) -> Self {
        Self {
            direction,
            method: method.map(Arc::from),
            json,
        }
    }

    /// Which way the message went.
    #[must_use]
    pub fn direction(&self) -> Direction {
        self.direction
    }

    /// The message's method; for a reply, the method of the request it answers. `None` for a
    /// reply to a request the client no longer waits for (cancelled or superseded), and for one
    /// that does not decode.
    #[must_use]
    pub fn method(&self) -> Option<&str> {
        self.method.as_deref()
    }

    /// The message's JSON text as sent or received.
    #[must_use]
    pub fn json(&self) -> &[u8] {
        &self.json
    }
}
