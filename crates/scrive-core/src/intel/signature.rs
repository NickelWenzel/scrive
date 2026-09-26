//! Signature help — the `SignatureHelp` trait the app satisfies plus the
//! plain data it returns. No controller: signature help is stateless per query
//! (the app re-runs it on `(` / `,` / edits and shows or hides the one-line box
//! from the reply), so unlike completion there is no sticky state machine.
//!
//! A provider answers synchronously, the same frame. With no provider set, the
//! editor records a [`SignatureRequest`] instead; its reply lands only under
//! the request's ticket, so an answer for a call the caret has left is dropped.

use core::ops::Range;

use crate::intel::ticket::Ticket;
use crate::{DocId, Point};

/// The signature-help seam.
pub trait SignatureHelp {
    /// The signature of the call the caret is inside, or `None` when it is not
    /// inside a known call (the box closes).
    fn signature(&mut self, cx: &SignatureCx) -> Option<SignatureInfo>;
}

/// A revision-stamped signature request — everything the provider may read.
#[derive(Clone, Debug)]
pub struct SignatureCx {
    /// Which document the request is for.
    pub doc: DocId,
    /// The document revision the request was snapshotted at.
    pub revision: u64,
    /// Caret position, always clipped to a `char` boundary so the provider can
    /// slice `lookback` without splitting a multi-byte character.
    pub position: Point,
    /// The same `LOOKBACK_LINES` lookback as `CompletionCx` — `enclosingCall` +
    /// the active-parameter count need nothing else.
    pub lookback: String,
}

/// A resolved signature: the rendered line plus which parameter is active.
#[derive(Clone, Debug)]
pub struct SignatureInfo {
    /// The signature line, e.g. `wait(timer: duration)`.
    pub label: String,
    /// Byte ranges of each parameter's label within [`label`](Self::label). The
    /// provider builds `label`, so these are exact (no substring matching).
    pub params: Vec<Range<u32>>,
    /// The active parameter — the top-level comma count, clamped to
    /// `params.len() - 1`.
    pub active: u32,
    /// Optional documentation for the call.
    pub doc: Option<String>,
}

impl SignatureInfo {
    /// The byte range of the active parameter within `label`, if any — the
    /// substring the box highlights.
    #[must_use]
    pub fn active_param(&self) -> Option<Range<u32>> {
        self.params.get(self.active as usize).cloned()
    }
}

/// A signature-help request an editor records for an async source when no
/// synchronous [`SignatureHelp`] provider is set. Answer it through the
/// editor's `set_signature` with [`ticket`](Self::ticket); `None` closes the
/// box.
#[derive(Clone, Debug)]
pub struct SignatureRequest {
    ticket: Ticket,
    position: Point,
}

impl SignatureRequest {
    /// A request for signature help at `position` (the caret), made under
    /// `ticket`.
    #[must_use]
    pub fn new(ticket: Ticket, position: Point) -> Self {
        Self { ticket, position }
    }

    /// The ticket the reply must carry.
    #[must_use]
    pub fn ticket(&self) -> Ticket {
        self.ticket
    }

    /// The caret position (row, byte column) to query at.
    #[must_use]
    pub fn position(&self) -> Point {
        self.position
    }
}
