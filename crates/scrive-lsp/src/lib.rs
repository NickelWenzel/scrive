//! `scrive-lsp` — a Language Server Protocol client for scrive that does no I/O.
//!
//! The host owns the transport. It hands each incoming JSON-RPC [`Message`] to the client and
//! sends whatever the client returns; nothing here reads a socket, a clock or a thread. That keeps
//! the crate a pure state machine that tests as data in, data out, and builds for wasm32.
//!
//! - the JSON-RPC envelope → [`message`]
//!
//! Payloads are [`lsp_types`], re-exported so a host and this crate always agree on one version.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod message;

pub use lsp_types;
pub use message::Message;
