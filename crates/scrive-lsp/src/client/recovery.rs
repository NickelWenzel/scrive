//! The client's side of losing and regaining a worker bridge's server: it decides each loss,
//! because only it knows whether a handshake ever completed and how many restarts the policy
//! has left.

#[cfg(all(test, not(target_family = "wasm")))]
mod tests;

use std::io;
use std::sync::Arc;
use std::time::Duration;
#[cfg(not(target_family = "wasm"))]
use std::time::Instant;

#[cfg(all(feature = "websocket", target_arch = "wasm32", target_os = "unknown"))]
use wasmtimer::std::Instant;

use super::{documents, Client, Connection, Error, Reason, Status};
use crate::transport::{self, Generation, Handshake, Lifecycle};
use crate::{restart, update, Update};

/// What the client keeps to decide on losses.
#[derive(Debug)]
pub(super) struct State {
    /// The newest generation this client has seen or announced.
    generation: Generation,
    /// The policy and its recorded losses.
    restarts: restart::Window,
    /// Whether any connection ever completed `initialize`. Until one has, a loss stops the
    /// client instead of restarting it.
    handshake: Handshake,
    /// Attempts since the client last reached `Running`.
    attempt: u32,
    /// The builder's `initialize_timeout`, which `Error::Timeout` reports.
    initialize_timeout: Option<Duration>,
}

/// What the client makes of a loss.
enum Decision {
    Restart,
    Stop(Reason),
}

impl State {
    pub(super) fn new(policy: restart::Policy, initialize_timeout: Option<Duration>) -> Self {
        Self {
            generation: Generation::FIRST,
            restarts: restart::Window::new(policy),
            handshake: Handshake::Pending,
            attempt: 0,
            initialize_timeout,
        }
    }

    /// The newest generation the client knows of.
    pub(super) fn generation(&self) -> Generation {
        self.generation
    }
}

impl Client {
    /// The current connection completed `initialize`: its worker can stop waiting for it, and
    /// later losses may restart the server.
    pub(super) fn handshaken(&mut self) {
        if let Connection::Live { generation, .. } = self.connection {
            self.control.send(Lifecycle::Handshaken(generation));
        }
        self.recovery.handshake = Handshake::Done;
        self.recovery.attempt = 0;
    }

    /// The server failed `initialize`: the connection is closed, and the client waits, revivable.
    pub(super) fn refused(&mut self) -> Vec<Update> {
        let mut updates = self.disconnect();
        let status = self.halt(self.recovery.generation, Reason::Initialize);
        self.status = status.clone();
        updates.push(Update::Status(status));
        updates
    }

    /// The connection before `generation` ended. Unless the client already moved past it or
    /// shut down, it settles what was in flight and decides whether the server comes back.
    pub(super) fn lost(&mut self, generation: Generation, reason: Reason) -> Vec<Update> {
        if matches!(self.connection, Connection::Shut(_)) || generation <= self.recovery.generation
        {
            return Vec::new();
        }
        self.recovery.generation = generation;
        let live = matches!(self.connection, Connection::Live { .. });
        self.connection = Connection::Reconnecting;
        let mut updates = Vec::new();
        if reason == Reason::Timeout {
            let after = self
                .recovery
                .initialize_timeout
                .expect("only an armed deadline runs out");
            updates.push(Update::Error(Error::Timeout { after }));
        }
        if live {
            updates.extend(self.disconnect());
        }
        let status = match self.decide(reason) {
            Decision::Restart => {
                self.recovery.attempt += 1;
                self.control.send(Lifecycle::Reconnect(generation));
                Status::Restarting {
                    attempt: self.recovery.attempt,
                }
            }
            Decision::Stop(reason) => self.halt(generation, reason),
        };
        self.status = status.clone();
        updates.push(Update::Status(status));
        updates
    }

    /// An attempt to bring up connection `generation` failed. It counts against the policy like
    /// a loss, so a server that can't start again doesn't retry forever.
    pub(super) fn attempting(
        &mut self,
        generation: Generation,
        failure: Arc<io::Error>,
    ) -> Vec<Update> {
        if !matches!(self.connection, Connection::Reconnecting)
            || generation != self.recovery.generation
        {
            return Vec::new();
        }
        let mut updates = vec![Update::Error(Error::Reconnect { source: failure })];
        let status = match self.recovery.restarts.admit(Instant::now()) {
            restart::Verdict::Restart => {
                self.recovery.attempt += 1;
                Status::Restarting {
                    attempt: self.recovery.attempt,
                }
            }
            restart::Verdict::Exhausted | restart::Verdict::Never => {
                self.halt(generation, Reason::GaveUp)
            }
        };
        self.status = status.clone();
        updates.push(Update::Status(status));
        updates
    }

    /// Connection `generation` is up: the session starts over on it with a fresh `initialize`.
    /// Its status stays `Restarting` until the handshake completes.
    pub(super) fn reconnected(
        &mut self,
        generation: Generation,
        link: transport::Link,
    ) -> Vec<Update> {
        if !matches!(self.connection, Connection::Reconnecting)
            || generation < self.recovery.generation
        {
            return Vec::new();
        }
        self.recovery.generation = generation;
        self.connection = Connection::Live { generation, link };
        let output = self.session.reinitialize();
        debug_assert!(
            output.updates.is_empty(),
            "a new initialize updates nothing"
        );
        self.send(output.messages, None);
        Vec::new()
    }

    /// `restart()` on a bridge whose worker can start the server again.
    pub(super) fn restart_worker(&mut self) -> Vec<update::Document> {
        if matches!(self.connection, Connection::Shut(_)) {
            return Vec::new();
        }
        let settled = if matches!(self.connection, Connection::Live { .. }) {
            self.disconnect()
        } else {
            Vec::new()
        };
        self.recovery.generation = self.recovery.generation.next();
        self.connection = Connection::Reconnecting;
        self.recovery.restarts.reset();
        self.recovery.attempt = 1;
        self.control
            .send(Lifecycle::Restart(self.recovery.generation));
        self.status = Status::Restarting { attempt: 1 };
        self.queue(self.status.clone());
        documents(settled)
    }

    /// No restart before the first successful `initialize`: a server that can't start once
    /// stops at once.
    fn decide(&mut self, reason: Reason) -> Decision {
        if self.recovery.handshake == Handshake::Pending {
            return Decision::Stop(reason);
        }
        match self.recovery.restarts.admit(Instant::now()) {
            restart::Verdict::Restart => Decision::Restart,
            restart::Verdict::Exhausted => Decision::Stop(Reason::GaveUp),
            restart::Verdict::Never => Decision::Stop(reason),
        }
    }

    /// Tells the worker not to bring connection `generation` back. Both sides move past it, so
    /// anything that raced the stop is dropped.
    fn halt(&mut self, generation: Generation, reason: Reason) -> Status {
        self.control.send(Lifecycle::Stop(generation));
        self.recovery.generation = generation.next();
        self.connection = Connection::Stopped;
        Status::Stopped(reason)
    }
}
