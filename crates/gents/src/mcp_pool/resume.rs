//! Bounded client-side resume policy for rmcp streamable-HTTP sessions (#639).
//!
//! rmcp's `SseAutoReconnectStream` resumes a closed SSE stream forever: a
//! graceful close consults the retry policy with `current_times == 0` every
//! time, so `max_times`-style caps can never fire. Against a server that
//! answers resume-of-a-dead-session with 200 + empty stream (the rmcp
//! streamable-HTTP server does exactly this), the client re-resumes in a hot
//! loop — ~5/s sustained in the 2026-07-06/07 incidents — and the #626 idle
//! TTL cannot break it because an actively-failing session is never idle.
//!
//! [`SessionResumePolicy`] is installed as the transport's `retry_config` and
//! closes both holes from our side:
//!
//! - A granted resume whose stream dies within
//!   [`RAPID_STREAM_DEATH_THRESHOLD`] is the dead-session empty-stream
//!   signature: the session is poisoned after that one attempt and no resume
//!   is ever granted again.
//! - Consecutive resume *errors* are capped at
//!   [`MAX_CONSECUTIVE_RESUME_FAILURES`], then the session is poisoned —
//!   defense in depth so no future resume path can hot-loop either.
//!
//! Poisoning is terminal for the session only: the pool observes it via
//! [`SessionResumePolicy::is_poisoned`] on next use, drops the connection
//! (which terminates the server-side session through the #626 drop →
//! cancellation → worker-cleanup DELETE path) and connects fresh.
//!
//! One policy instance is shared by every SSE stream of one transport (the
//! standalone GET stream and any POST-response streams), so interleaved
//! closes from different streams can in rare cases be misread as a rapid
//! death. The consequence is one extra session re-initialization — accepted
//! for the simplicity of keeping the policy stateless per-stream.
//!
//! Scope note: rmcp bypasses `retry_config.retry(0)` when an SSE stream
//! supplies a `retry:` field, because the server-provided interval takes
//! precedence. The production pathology this module bounds was a 200 + empty
//! stream with no `retry:` field; bounding a server that repeatedly sends only
//! `retry:` control frames and then closes requires an upstream rmcp seam or a
//! custom transport wrapper.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rmcp::transport::common::client_side_sse::SseRetryPolicy;
use tokio::time::Instant;

/// Delay before reconnecting after a graceful SSE stream close (mirrors
/// rmcp's default base backoff).
pub(crate) const RESUME_RECONNECT_DELAY: Duration = Duration::from_secs(1);

/// A granted resume whose stream lives shorter than this is treated as the
/// dead-session empty-stream signature and poisons the session. Healthy
/// streams live minutes-to-forever; the pathological empty stream closes
/// within one round-trip.
pub(crate) const RAPID_STREAM_DEATH_THRESHOLD: Duration = Duration::from_secs(10);

pub(crate) const MAX_CONSECUTIVE_RESUME_FAILURES: usize = 3;

#[derive(Debug, Default)]
pub struct McpResumeStats {
    pub resume_attempts: AtomicU64,
    pub resume_failures: AtomicU64,
    /// Sessions declared terminal (empty-stream signature or failure cap).
    pub sessions_poisoned: AtomicU64,
    pub session_reinits: AtomicU64,
    pub connect_failures: AtomicU64,
}

#[derive(Debug)]
pub(crate) struct SessionResumePolicy {
    service_id: String,
    stats: Arc<McpResumeStats>,
    poisoned: AtomicBool,
    last_grant: Mutex<Option<Grant>>,
}

#[derive(Debug, Clone, Copy)]
struct Grant {
    at: Instant,
    delay: Duration,
}

impl SessionResumePolicy {
    pub(crate) fn new(service_id: impl Into<String>, stats: Arc<McpResumeStats>) -> Self {
        Self {
            service_id: service_id.into(),
            stats,
            poisoned: AtomicBool::new(false),
            last_grant: Mutex::new(None),
        }
    }

    /// Policy with its own private stats — for connections whose stats are
    /// not registered with a pool (test connectors).
    #[cfg(test)]
    pub(crate) fn detached(service_id: &str) -> Arc<Self> {
        Arc::new(Self::new(service_id, Arc::new(McpResumeStats::default())))
    }

    pub(crate) fn is_poisoned(&self) -> bool {
        self.poisoned.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn poison_for_test(&self) {
        self.poison("test");
    }

    #[cfg(test)]
    pub(crate) fn stats(&self) -> &McpResumeStats {
        &self.stats
    }

    fn poison(&self, reason: &str) {
        if !self.poisoned.swap(true, Ordering::AcqRel) {
            self.stats.sessions_poisoned.fetch_add(1, Ordering::Relaxed);
            // At most one warn per session generation — the pathology being
            // fixed is log flood, so the failure path must not add to it.
            tracing::warn!(
                service_id = %self.service_id,
                reason,
                resume_attempts = self.stats.resume_attempts.load(Ordering::Relaxed),
                resume_failures = self.stats.resume_failures.load(Ordering::Relaxed),
                "MCP SSE resume is terminal for this session; the pool will \
                 re-initialize a fresh session on next use (#639)"
            );
        }
    }

    fn grant(&self, delay: Duration) -> Duration {
        self.stats.resume_attempts.fetch_add(1, Ordering::Relaxed);
        *self.last_grant.lock().expect("last_grant lock") = Some(Grant {
            at: Instant::now(),
            delay,
        });
        delay
    }
}

impl SseRetryPolicy for SessionResumePolicy {
    fn retry(&self, current_times: usize) -> Option<Duration> {
        if self.is_poisoned() {
            return None;
        }

        if current_times >= 1 {
            self.stats.resume_failures.fetch_add(1, Ordering::Relaxed);
            if current_times >= MAX_CONSECUTIVE_RESUME_FAILURES {
                self.poison("resume attempts kept erroring");
                return None;
            }
            return Some(self.grant(RESUME_RECONNECT_DELAY));
        }

        let previous_grant = *self.last_grant.lock().expect("last_grant lock");
        if let Some(grant) = previous_grant {
            let stream_lifetime = Instant::now()
                .saturating_duration_since(grant.at)
                .saturating_sub(grant.delay);
            if stream_lifetime < RAPID_STREAM_DEATH_THRESHOLD {
                self.poison("resume returned an immediately-dead (empty) stream");
                return None;
            }
        }
        Some(self.grant(RESUME_RECONNECT_DELAY))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering::SeqCst;

    #[tokio::test(start_paused = true)]
    async fn resume_after_long_lived_stream_stays_granted() {
        let policy = SessionResumePolicy::detached("healthy-service");

        let delay = policy
            .retry(0)
            .expect("first resume after a stream close must be granted");
        // The granted stream lives well past the rapid-death threshold.
        tokio::time::advance(delay + Duration::from_secs(300)).await;
        let delay = policy
            .retry(0)
            .expect("resume after a long-lived stream must stay granted");
        tokio::time::advance(delay + Duration::from_secs(300)).await;
        assert!(
            policy.retry(0).is_some(),
            "healthy reconnect cadence must never poison the session"
        );
        assert!(!policy.is_poisoned());
    }

    #[tokio::test(start_paused = true)]
    async fn empty_stream_resume_is_terminal_after_one_attempt() {
        let policy = SessionResumePolicy::detached("dead-session-service");

        let delay = policy
            .retry(0)
            .expect("first resume after a stream close must be granted");
        // The granted resume comes back as a 200 + empty stream: it closes
        // essentially immediately after the reconnect delay elapses.
        tokio::time::advance(delay + Duration::from_millis(50)).await;
        assert_eq!(
            policy.retry(0),
            None,
            "a resume that yielded an immediately-dead stream must be terminal"
        );
        assert!(policy.is_poisoned());
        assert_eq!(policy.stats().resume_attempts.load(SeqCst), 1);
        assert_eq!(policy.stats().sessions_poisoned.load(SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn consecutive_resume_errors_hit_cap_and_poison() {
        let policy = SessionResumePolicy::detached("erroring-service");

        assert!(policy.retry(1).is_some(), "first resume error retries");
        assert!(policy.retry(2).is_some(), "second resume error retries");
        assert_eq!(
            policy.retry(MAX_CONSECUTIVE_RESUME_FAILURES),
            None,
            "the failure cap must poison the session"
        );
        assert!(policy.is_poisoned());
        assert_eq!(policy.stats().resume_failures.load(SeqCst), 3);
        assert_eq!(policy.stats().sessions_poisoned.load(SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn poisoned_policy_never_grants_again() {
        let policy = SessionResumePolicy::detached("poisoned-service");
        policy.poison_for_test();

        assert_eq!(policy.retry(0), None, "graceful-close path must stay shut");
        assert_eq!(policy.retry(1), None, "error path must stay shut");
        tokio::time::advance(Duration::from_secs(3600)).await;
        assert_eq!(
            policy.retry(0),
            None,
            "poisoning is permanent for the session"
        );
        assert_eq!(
            policy.stats().sessions_poisoned.load(SeqCst),
            1,
            "poisoning must be counted once"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn error_path_success_followed_by_immediate_death_is_terminal() {
        let policy = SessionResumePolicy::detached("flapping-service");

        // One resume error, then the retried connect "succeeds"…
        let delay = policy.retry(1).expect("first resume error retries");
        // …but the stream it produced dies immediately (empty stream).
        tokio::time::advance(delay + Duration::from_millis(50)).await;
        assert_eq!(
            policy.retry(0),
            None,
            "an error-path resume that produced a dead stream must be terminal"
        );
        assert!(policy.is_poisoned());
    }
}
