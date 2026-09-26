//! Goto-definition: the request an editor records for F12. The host answers
//! with a range in the same document through the editor's `set_definition`,
//! or opens the target itself when it lies elsewhere.

use crate::intel::ticket::Ticket;

/// A request for the definition of the symbol at `offset`.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct DefinitionRequest {
    /// The ticket the answer must carry to land.
    pub ticket: Ticket,
    /// The caret offset the request was made at.
    pub offset: u32,
}

impl DefinitionRequest {
    /// A definition request at `offset`, made under `ticket`.
    #[must_use]
    pub fn new(ticket: Ticket, offset: u32) -> Self {
        Self { ticket, offset }
    }
}
