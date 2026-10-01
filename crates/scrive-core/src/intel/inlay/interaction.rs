//! A gesture on an installed hint that the host must answer: show a tooltip,
//! follow a label part's location, or insert the hint as text.

use crate::intel::inlay::Key;
use crate::intel::ticket::Ticket;

/// One gesture on the hint keyed `key`, recorded under `ticket`. The host
/// answers it only while its hint set is still the one at the ticket's
/// revision.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Interaction {
    ticket: Ticket,
    key: Key,
    gesture: Gesture,
}

/// What the user did to the hint.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Gesture {
    /// The pointer rests on label part `part` of the hint.
    Tooltip {
        /// The hovered label part's index.
        part: u32,
    },
    /// Follow label part `part`'s location.
    Jump {
        /// The clicked label part's index.
        part: u32,
    },
    /// Insert the hint, which renders at `offset`, as buffer text.
    Insert {
        /// The hint's render offset.
        offset: u32,
    },
}

impl Interaction {
    /// Ask for the tooltip of label part `part` of hint `key`. The host falls
    /// back to the hint's own tooltip.
    #[must_use]
    pub fn tooltip(ticket: Ticket, key: Key, part: u32) -> Self {
        Self {
            ticket,
            key,
            gesture: Gesture::Tooltip { part },
        }
    }

    /// Follow label part `part` of hint `key`.
    #[must_use]
    pub fn jump(ticket: Ticket, key: Key, part: u32) -> Self {
        Self {
            ticket,
            key,
            gesture: Gesture::Jump { part },
        }
    }

    /// Insert hint `key`, which renders at `offset`.
    #[must_use]
    pub fn insert(ticket: Ticket, key: Key, offset: u32) -> Self {
        Self {
            ticket,
            key,
            gesture: Gesture::Insert { offset },
        }
    }

    /// The ticket the answer must carry.
    #[must_use]
    pub fn ticket(&self) -> Ticket {
        self.ticket
    }

    /// The hint the gesture targets.
    #[must_use]
    pub fn key(&self) -> Key {
        self.key
    }

    /// What the user did.
    #[must_use]
    pub fn gesture(&self) -> Gesture {
        self.gesture
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Revision;
    use crate::intel::ticket::Counter;

    /// Every gesture constructor keeps the ticket and key it was given.
    #[test]
    fn each_gesture_keeps_its_ticket_and_key() {
        let ticket = Counter::new().issue(Revision(0));
        let key = Key::new(7);
        let cases = [
            (
                Interaction::tooltip(ticket, key, 1),
                Gesture::Tooltip { part: 1 },
            ),
            (Interaction::jump(ticket, key, 2), Gesture::Jump { part: 2 }),
            (
                Interaction::insert(ticket, key, 9),
                Gesture::Insert { offset: 9 },
            ),
        ];
        for (interaction, gesture) in cases {
            assert_eq!(interaction.ticket(), ticket, "the ticket survives");
            assert_eq!(interaction.key(), key, "the key survives");
            assert_eq!(
                interaction.gesture(),
                gesture,
                "the gesture is the one asked for"
            );
        }
    }
}
