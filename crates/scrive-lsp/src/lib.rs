//! `scrive-lsp` — a Language Server Protocol client for scrive.
//!
//! A [`Client`] owns its connection to one server. A [`client::Builder`] terminal picks the
//! bridge (natively `Builder::stdio` for a server process, and `Builder::connect` and
//! `Builder::listen` for a server on a TCP socket; with the `websocket` feature
//! `Builder::websocket` for one behind a WebSocket; everywhere
//! [`Builder::memory`](client::Builder::memory) for an in-process server) and returns the client
//! with its [`client::Events`], the stream of everything the server sends. The host runs that
//! stream and hands each event to [`Client::receive`], which returns the [`Update`]s to apply.
//!
//! The protocol itself is a pure state machine that does no I/O, reads no clock and spawns
//! nothing; only the client and the bridges do. That keeps the protocol testable as data in,
//! data out, and the crate buildable for wasm32.
//!
//! - registering and syncing documents → [`Client::open`], [`Client::sync`], [`Client::save`]
//! - completion → [`Client::complete`]
//! - signature help → [`Client::signature_help`]
//! - hover → [`Client::hover`]
//! - goto definition → [`Client::definition`]
//! - rename → [`Client::rename`]
//! - formatting → [`Client::format`]
//! - inlay hints → [`Client::inlays`]; tooltips, label jumps and inserts → [`Client::interact`]
//! - what the client hands back → [`Update`] ([`update`]), [`client::Status`], [`log`], [`trace`]
//! - when a lost server starts again → [`restart`]
//! - URI identity and file paths → [`uri`]
//!
//! Payloads are [`lsp_types`], and the in-process bridge takes an [`lsp_server`] connection;
//! both are re-exported so a host and this crate always agree on one version. So is `rustls`
//! natively with the `websocket` feature, for `Builder::tls`.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod client;
mod completion;
mod diagnostics;
mod edits;
mod encoding;
mod hover;
mod inlay;
pub mod log;
mod markdown;
mod message;
pub mod restart;
mod session;
mod signature;
mod snippet;
pub mod trace;
mod transport;
pub mod update;
pub mod uri;
mod workspace;

pub use client::{Client, Error};
pub(crate) use encoding::Encoding;
pub use lsp_server;
pub use lsp_types;
#[cfg(all(feature = "websocket", not(target_family = "wasm")))]
pub use rustls;
pub use update::Update;
