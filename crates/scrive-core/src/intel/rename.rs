//! Rename: the request an editor records when the user submits a new name for
//! the symbol under the caret. The answer is a set of edits; the editor's own
//! document takes them as an ordinary edit.

use crate::intel::ticket::Ticket;

/// A request to rename the symbol at `offset` to `new_name`.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct RenameRequest {
    /// The ticket naming the revision the rename was asked at.
    pub ticket: Ticket,
    /// The caret offset of the symbol to rename.
    pub offset: u32,
    /// The name the user typed. Never empty.
    pub new_name: String,
}

impl RenameRequest {
    /// A rename of the symbol at `offset` to `new_name`, made under `ticket`.
    #[must_use]
    pub fn new(ticket: Ticket, offset: u32, new_name: impl Into<String>) -> Self {
        Self {
            ticket,
            offset,
            new_name: new_name.into(),
        }
    }
}
