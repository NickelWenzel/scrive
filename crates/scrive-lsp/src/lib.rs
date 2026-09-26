//! `scrive-lsp` — a Language Server Protocol client for scrive that does no I/O.
//!
//! The host owns the transport. It hands each incoming JSON-RPC [`Message`] to the client and
//! sends whatever the client returns; nothing here reads a socket, a clock or a thread. That keeps
//! the crate a pure state machine that tests as data in, data out, and builds for wasm32.
//!
//! - the client state machine → [`Client`] ([`client`])
//! - completion → [`Client::complete`]
//! - signature help → [`Client::signature_help`]
//! - hover → [`Client::hover`]
//! - goto definition → [`Client::definition`]
//! - formatting → [`Client::format`]
//! - what the client hands back → [`Update`] ([`update`])
//! - the JSON-RPC envelope → [`message`]
//! - LSP positions ↔ scrive byte offsets → [`encoding`]
//! - URI identity → [`uri`]
//!
//! Payloads are [`lsp_types`], re-exported so a host and this crate always agree on one version.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod client;
mod completion;
mod diagnostics;
mod edits;
mod hover;
pub mod encoding;
mod markdown;
pub mod message;
mod signature;
mod snippet;
pub mod update;
pub mod uri;
mod workspace;

pub use client::{Client, Error, Output};
pub use encoding::Encoding;
pub use lsp_types;
pub use message::Message;
pub use update::Update;
