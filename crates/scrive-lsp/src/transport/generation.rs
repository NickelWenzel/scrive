//! Which connection of a client something belongs to.

use core::fmt;

/// Which connection of a client an event or command belongs to. The first connection is
/// [`FIRST`](Self::FIRST); each loss, stop or restart moves on to a newer one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Generation(u64);

impl fmt::Display for Generation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Generation {
    pub(crate) const FIRST: Self = Self(0);

    /// The connection after this one.
    #[cfg(any(
        not(target_family = "wasm"),
        all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
    ))]
    pub(crate) fn next(self) -> Self {
        Self(self.0 + 1)
    }
}
