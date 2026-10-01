//! Language services — completion, signature help, hover, inlay hints, and
//! the goto-definition, rename and format commands.
//!
//! Each service has two ways in. A **provider** is one small trait defined
//! here and satisfied by the app (not a god-trait with no-op defaults): the
//! editor calls it in `update()` and shows the answer the same frame. A host
//! whose answers come from elsewhere (a background thread, a language server)
//! leaves the provider unset; the editor then records a **request** stamped
//! with a [`Ticket`](ticket::Ticket), and the reply lands only if it carries
//! the ticket the editor still awaits, at the same revision. The plain data
//! both paths exchange lives here, so neither reaches an editor internal.
//! The commands have no provider: the editor always records a request.
//!
//! The controller state machines (completion / snippet session) that consume
//! these providers are core view-state and land in the submodules below.

pub mod completion;
pub mod definition;
pub mod format;
pub mod hover;
pub mod inlay;
pub mod providers;
pub mod rename;
pub mod signature;
pub mod snippet;
pub mod ticket;
