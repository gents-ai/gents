//! Retry pacing for a background compaction worker whose passes fail.
//!
//! A worker wakes on every flush and at least once per poll interval. A
//! failure that does not clear on its own (an exhausted file descriptor
//! table, a full disk) would otherwise be retried at that rate forever,
//! burning I/O and flooding the log. After each consecutive failure the
//! worker waits twice as long before the next attempt, up to
//! [`MAX_DELAY`]; the first pass that succeeds resets it.

use std::time::{Duration, Instant};

const BASE_DELAY: Duration = Duration::from_secs(1);
const MAX_DELAY: Duration = Duration::from_secs(60);

#[derive(Debug, Default)]
pub(crate) struct FailureBackoff {
    consecutive_failures: u32,
    retry_at: Option<Instant>,
}

impl FailureBackoff {
    /// Whether a pass may run at `now`.
    pub(crate) fn ready(&self, now: Instant) -> bool {
        self.retry_at.is_none_or(|at| now >= at)
    }

    pub(crate) fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.retry_at = None;
    }

    /// Record a failed pass at `now` and return how long the worker will
    /// hold off.
    pub(crate) fn record_failure(&mut self, now: Instant) -> Duration {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        let doublings = (self.consecutive_failures - 1).min(16);
        let delay = BASE_DELAY.saturating_mul(1 << doublings).min(MAX_DELAY);
        self.retry_at = Some(now + delay);
        delay
    }

    pub(crate) fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_worker_is_ready() {
        assert!(FailureBackoff::default().ready(Instant::now()));
    }

    #[test]
    fn delays_double_and_cap() {
        let mut backoff = FailureBackoff::default();
        let now = Instant::now();
        let delays: Vec<u64> = (0..10)
            .map(|_| backoff.record_failure(now).as_secs())
            .collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 32, 60, 60, 60, 60]);
        assert_eq!(backoff.consecutive_failures(), 10);
    }

    #[test]
    fn a_failure_holds_the_worker_until_its_delay_passes() {
        let mut backoff = FailureBackoff::default();
        let now = Instant::now();
        let delay = backoff.record_failure(now);
        assert!(!backoff.ready(now));
        assert!(!backoff.ready(now + delay - Duration::from_millis(1)));
        assert!(backoff.ready(now + delay));
    }

    #[test]
    fn success_resets_the_schedule() {
        let mut backoff = FailureBackoff::default();
        let now = Instant::now();
        for _ in 0..5 {
            backoff.record_failure(now);
        }
        backoff.record_success();
        assert!(backoff.ready(now));
        assert_eq!(backoff.record_failure(now), BASE_DELAY);
    }

    #[test]
    fn a_very_long_failure_streak_does_not_overflow() {
        let mut backoff = FailureBackoff {
            consecutive_failures: u32::MAX - 1,
            retry_at: None,
        };
        let now = Instant::now();
        assert_eq!(backoff.record_failure(now), MAX_DELAY);
        assert_eq!(backoff.record_failure(now), MAX_DELAY);
    }
}
