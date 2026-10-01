//! The fetch an editor records when its inlay hints are due: which bytes to
//! cover, under which ticket.

use std::ops::Range;

use crate::intel::ticket::Ticket;

/// A request for the hints in a byte span. The answer lands only under its
/// ticket, at the ticket's revision.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Request {
    ticket: Ticket,
    span: Range<u32>,
}

impl Request {
    /// A request for the hints in `span`, made under `ticket`.
    #[must_use]
    pub fn new(ticket: Ticket, span: Range<u32>) -> Self {
        Self { ticket, span }
    }

    /// The ticket the answer must carry.
    #[must_use]
    pub fn ticket(&self) -> Ticket {
        self.ticket
    }

    /// The byte span to cover, at the ticket's revision.
    #[must_use]
    pub fn span(&self) -> Range<u32> {
        self.span.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Revision;
    use crate::intel::ticket::Counter;

    /// A request hands back the ticket and span it was made with.
    #[test]
    fn a_request_keeps_its_ticket_and_span() {
        let ticket = Counter::new().issue(Revision(4));
        let request = Request::new(ticket, 3..9);
        assert_eq!(
            (request.ticket(), request.span()),
            (ticket, 3..9),
            "ticket and span survive"
        );
    }
}
