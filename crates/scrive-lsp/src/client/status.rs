//! Where a client's connection stands.

use std::io;
use std::sync::Arc;

/// Where a client's connection stands.
#[derive(Clone, Debug)]
pub enum Status {
    /// `initialize` is out and unanswered. Every client starts here.
    Starting,
    /// The handshake completed; requests go to the server.
    Running,
    /// The server was lost and is being started again; every request declines until it runs.
    /// `attempt` counts from 1 since the client last reached `Running`.
    Restarting {
        /// The attempt this is.
        attempt: u32,
    },
    /// The connection is over: nothing goes out and every request declines. After
    /// [`Reason::Shutdown`], and for a bridge that cannot restart, this is final and the client's
    /// [`Events`](super::Events) ends. After any other reason a server process's client keeps
    /// its stream running.
    Stopped(Reason),
}

/// Why a connection stopped.
#[derive(Clone, Debug)]
pub enum Reason {
    /// [`Client::shutdown`](super::Client::shutdown) ended it.
    Shutdown,
    /// The server closed its end.
    Closed,
    /// The server failed `initialize`, or its result did not decode.
    Initialize,
    /// The bridge could not run, for example a thread failed to spawn.
    Failed(Arc<io::Error>),
    /// The server process ended. On Unix `signal` is set when a signal killed it (a `SIGSEGV`
    /// or `SIGABRT` means a crash); otherwise `code` is its exit code. A process the client
    /// killed shows the kill (`signal: Some(9)` on Unix, `code: Some(1)` on Windows): one whose
    /// stdout closed while it kept running, or that announced a message over 1 GiB.
    Exited {
        /// The exit code, when it exited by itself.
        code: Option<i32>,
        /// The signal that killed it, on Unix.
        signal: Option<i32>,
    },
    /// The server stopped reading its input: more messages than it could take waited to be
    /// written (256 MiB), so it was killed.
    Unresponsive,
    /// The restart policy ran out: too many losses within its window.
    GaveUp,
}

/// `Restarting` compares by its attempt, `Stopped` by its reason.
impl PartialEq for Status {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Starting, Self::Starting) | (Self::Running, Self::Running) => true,
            (Self::Restarting { attempt: a }, Self::Restarting { attempt: b }) => a == b,
            (Self::Stopped(a), Self::Stopped(b)) => a == b,
            (
                Self::Starting | Self::Running | Self::Restarting { .. } | Self::Stopped(_),
                _,
            ) => false,
        }
    }
}

impl Eq for Status {}

/// `Failed` compares by its error's kind, since `io::Error` has no equality.
impl PartialEq for Reason {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Shutdown, Self::Shutdown)
            | (Self::Closed, Self::Closed)
            | (Self::Initialize, Self::Initialize)
            | (Self::Unresponsive, Self::Unresponsive)
            | (Self::GaveUp, Self::GaveUp) => true,
            (Self::Failed(a), Self::Failed(b)) => a.kind() == b.kind(),
            (
                Self::Exited { code, signal },
                Self::Exited {
                    code: other_code,
                    signal: other_signal,
                },
            ) => code == other_code && signal == other_signal,
            (
                Self::Shutdown
                | Self::Closed
                | Self::Initialize
                | Self::Failed(_)
                | Self::Exited { .. }
                | Self::Unresponsive
                | Self::GaveUp,
                _,
            ) => false,
        }
    }
}

impl Eq for Reason {}
