//! Request tickets: the stamp an async language-service reply must carry to
//! land. A ticket names one request, so two requests at the same revision are
//! still told apart and only the newer reply lands.

use crate::buffer::Revision;

/// One request's identity. Only a [`Counter`] mints tickets, and the only
/// readable part is the [`revision`](Self::revision) the request was made at.
/// Two tickets are equal only if they name the same request.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Ticket {
    seq: u64,
    revision: Revision,
}

impl Ticket {
    /// The document revision the request was made at. A reply computed for it
    /// is meaningful only while the document is still at this revision.
    #[must_use]
    pub fn revision(&self) -> Revision {
        self.revision
    }
}

/// Mints [`Ticket`]s that never repeat for this counter. Tickets from one
/// counter never collide, so each editor owns one.
#[derive(Debug, Default)]
pub struct Counter {
    issued: u64,
}

impl Counter {
    /// A counter that has issued nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Issue a fresh ticket for a request made at `revision`.
    #[must_use]
    pub fn issue(&mut self, revision: Revision) -> Ticket {
        self.issued += 1;
        Ticket {
            seq: self.issued,
            revision,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two requests at one revision get different tickets that both report
    /// that revision.
    #[test]
    fn tickets_at_one_revision_are_distinct() {
        let mut counter = Counter::new();
        let (a, b) = (counter.issue(Revision(3)), counter.issue(Revision(3)));
        assert_ne!(a, b, "each request is its own ticket");
        assert_eq!(
            (a.revision(), b.revision()),
            (Revision(3), Revision(3)),
            "both remember the revision"
        );
        assert_eq!(a, a, "a ticket equals itself");
    }
}
