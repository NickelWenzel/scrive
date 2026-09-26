//! Formatting: the request an editor records for Shift+Alt+F. The answer is a
//! set of edits against the revision the ticket names.

use crate::intel::ticket::Ticket;

/// A request to format the whole document.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct FormatRequest {
    /// The ticket naming the revision the format was asked at.
    pub ticket: Ticket,
    /// The indent width in spaces. scrive indents with spaces, so a formatter
    /// should insert spaces at this width.
    pub tab_size: u32,
}

impl FormatRequest {
    /// A format request at indent width `tab_size`, made under `ticket`.
    #[must_use]
    pub fn new(ticket: Ticket, tab_size: u32) -> Self {
        Self { ticket, tab_size }
    }
}
