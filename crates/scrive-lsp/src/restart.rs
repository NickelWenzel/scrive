//! Whether a lost server is started again.

#[cfg(any(
    not(target_family = "wasm"),
    all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
))]
use std::collections::VecDeque;
use std::time::Duration;
#[cfg(not(target_family = "wasm"))]
use std::time::Instant;

#[cfg(all(feature = "websocket", target_arch = "wasm32", target_os = "unknown"))]
use wasmtimer::std::Instant;

/// When the client starts a lost server again. A server is never restarted before its first
/// `initialize` succeeded: a server that cannot start once stops at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// A lost server stays stopped.
    Never,
    /// Restarts up to `count` times, and gives up once `count + 1` losses fall within `within`.
    UpTo {
        /// Restarts allowed inside one window.
        count: u32,
        /// The sliding window's width.
        within: Duration,
    },
}

/// The losses that count against a policy, oldest first.
#[cfg(any(
    not(target_family = "wasm"),
    all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
))]
#[derive(Debug)]
pub(crate) struct Window {
    policy: Policy,
    losses: VecDeque<Instant>,
}

/// What a [`Window`] rules for one more loss.
#[cfg(any(
    not(target_family = "wasm"),
    all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Restart,
    Exhausted,
    Never,
}

/// Four restarts within three minutes, as VS Code's language client allows.
impl Default for Policy {
    fn default() -> Self {
        Policy::UpTo {
            count: 4,
            within: Duration::from_secs(180),
        }
    }
}

#[cfg(any(
    not(target_family = "wasm"),
    all(feature = "websocket", target_arch = "wasm32", target_os = "unknown")
))]
impl Window {
    pub(crate) fn new(policy: Policy) -> Self {
        Self {
            policy,
            losses: VecDeque::new(),
        }
    }

    /// Records a loss at `now`, with vscode-languageclient's sliding window
    /// (`DefaultErrorHandler.closed`).
    pub(crate) fn admit(&mut self, now: Instant) -> Verdict {
        let Policy::UpTo { count, within } = self.policy else {
            return Verdict::Never;
        };
        self.losses.push_back(now);
        if self.losses.len() <= count as usize {
            return Verdict::Restart;
        }
        let oldest = *self.losses.front().expect("a loss was just pushed");
        if now.duration_since(oldest) <= within {
            Verdict::Exhausted
        } else {
            self.losses.pop_front();
            Verdict::Restart
        }
    }

    /// Forgets every recorded loss.
    pub(crate) fn reset(&mut self) {
        self.losses.clear();
    }
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;

    const MINUTE: Duration = Duration::from_secs(60);

    /// Four losses in quick succession are each restarted.
    #[test]
    fn default_policy_allows_four_restarts_within_three_minutes() {
        let mut window = Window::new(Policy::default());
        let start = Instant::now();
        for n in 0..4 {
            assert_eq!(
                window.admit(start + n * MINUTE / 2),
                Verdict::Restart,
                "loss {n}"
            );
        }
    }

    /// The fifth loss inside the window gives up.
    #[test]
    fn a_fifth_loss_within_the_window_is_exhausted() {
        let mut window = Window::new(Policy::default());
        let start = Instant::now();
        for n in 0..4 {
            let _ = window.admit(start + n * MINUTE / 2);
        }
        assert_eq!(
            window.admit(start + 3 * MINUTE),
            Verdict::Exhausted,
            "five losses within three minutes"
        );
    }

    /// A loss the window has moved past no longer counts.
    #[test]
    fn a_fifth_loss_after_the_window_restarts_and_slides_it() {
        let mut window = Window::new(Policy::default());
        let start = Instant::now();
        for n in 0..4 {
            let _ = window.admit(start + n * MINUTE);
        }
        assert_eq!(
            window.admit(start + 4 * MINUTE),
            Verdict::Restart,
            "the oldest loss is four minutes back"
        );
        assert_eq!(
            window.admit(start + 4 * MINUTE),
            Verdict::Exhausted,
            "the window slid: five losses within three minutes again"
        );
    }

    /// `Never` never restarts.
    #[test]
    fn never_rules_never() {
        let mut window = Window::new(Policy::Never);
        assert_eq!(window.admit(Instant::now()), Verdict::Never, "never");
    }

    /// A reset starts the count over.
    #[test]
    fn reset_forgets_every_loss() {
        let mut window = Window::new(Policy::UpTo {
            count: 1,
            within: MINUTE,
        });
        let now = Instant::now();
        assert_eq!(window.admit(now), Verdict::Restart, "the first loss");
        window.reset();
        assert_eq!(window.admit(now), Verdict::Restart, "counted afresh");
        assert_eq!(
            window.admit(now),
            Verdict::Exhausted,
            "then the window is full"
        );
    }
}
