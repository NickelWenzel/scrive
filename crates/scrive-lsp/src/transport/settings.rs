//! What the builder hands a worker bridge.

use std::time::Duration;

/// How much each restart's backoff grows, and where it stops (monaco's reconnecting socket).
const GROWTH: f64 = 1.3;
const CAP: Duration = Duration::from_secs(10);

/// What the builder hands a worker bridge.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Settings {
    /// How long each step of the shutdown sequence, and a dead connection's output, may take.
    pub(crate) grace: Duration,
    /// The first delay before a restart.
    pub(crate) backoff: Duration,
    /// Unwritten bytes past which the server counts as unresponsive.
    pub(crate) limit: usize,
    /// How long a connection may take to answer `initialize`; `None` waits forever.
    pub(crate) initialize_timeout: Option<Duration>,
}

impl Settings {
    /// The wait before restart number `retry`: the backoff, growing ×1.3 per retry up to 10 s.
    pub(crate) fn delay(&self, retry: u32) -> Duration {
        grow(self.backoff, retry, CAP)
    }
}

/// `base`, grown ×1.3 `retry` times, at most `cap`.
fn grow(base: Duration, retry: u32, cap: Duration) -> Duration {
    if base.is_zero() {
        return Duration::ZERO;
    }
    let exponent = i32::try_from(retry).unwrap_or(i32::MAX);
    Duration::try_from_secs_f64(base.as_secs_f64() * GROWTH.powi(exponent))
        .map_or(cap, |delay| delay.min(cap))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backoff(backoff: Duration) -> Settings {
        Settings {
            grace: Duration::ZERO,
            backoff,
            limit: 0,
            initialize_timeout: None,
        }
    }

    /// The backoff starts at its base, grows by 30% per retry and stops at 10 s.
    #[test]
    fn backoff_grows_by_thirty_percent_up_to_ten_seconds() {
        let second = backoff(Duration::from_secs(1));
        assert_eq!(second.delay(0), Duration::from_secs(1), "the base");
        assert_eq!(second.delay(1), Duration::from_millis(1300), "30% more");
        assert_eq!(second.delay(100), CAP, "capped");
        assert_eq!(second.delay(u32::MAX), CAP, "still capped");
        assert_eq!(
            backoff(Duration::ZERO).delay(7),
            Duration::ZERO,
            "no backoff stays none"
        );
    }
}
