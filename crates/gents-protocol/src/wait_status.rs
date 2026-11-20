//! Truthful wait-status projection shared by the model-facing process tools
//! and the desktop operations snapshot.
//!
//! Facts are derived only from authoritative lifecycle owners (persisted
//! `lifecycle_state`, `started_at`, `completed_at` on AgentToolCall):
//!
//! - Lifecycle certainty is separate from timestamp certainty. A malformed
//!   `started_at` never downgrades a valid running/terminal state; it only
//!   makes elapsed time unknown.
//! - `pending` stays `pending`; it is never relabeled `running`.
//! - Unparseable or absent lifecycle state projects as `unknown`; nothing
//!   defaults to `running`.
//! - Running age is computed only against a valid `started_at` at or before
//!   the observation instant (skewed clocks yield `None`, never a negative
//!   age) and only while the state is running or pending.
//! - Terminal elapsed is anchored to `completed_at - started_at`, so it is
//!   stable across repeated projections. Missing or invalid terminal
//!   timestamps yield `None`, never "now".

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Upper bound on recent-output evidence carried per projected entry. Callers
/// produce the preview through the canonical bounded-output slice owner and
/// drill down with their read tool; this constant only caps what is embedded.
pub const PREVIEW_MAX_BYTES: usize = 2048;

/// Lifecycle classification parsed from the persisted `lifecycle_state`
/// string. Mirrors the persisted token set exactly; anything else is
/// `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WaitLifecycle {
    Pending,
    Running,
    Completed,
    Failed,
    TimedOut,
    Cancelled,
    Unknown,
}

impl WaitLifecycle {
    pub fn from_persisted(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some("pending") => Self::Pending,
            Some("running") => Self::Running,
            Some("completed") => Self::Completed,
            Some("failed") => Self::Failed,
            Some("timedOut") => Self::TimedOut,
            Some("cancelled") => Self::Cancelled,
            _ => Self::Unknown,
        }
    }

    pub fn as_str(self) -> Option<&'static str> {
        match self {
            Self::Pending => Some("pending"),
            Self::Running => Some("running"),
            Self::Completed => Some("completed"),
            Self::Failed => Some("failed"),
            Self::TimedOut => Some("timedOut"),
            Self::Cancelled => Some("cancelled"),
            Self::Unknown => None,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::TimedOut | Self::Cancelled
        )
    }

    /// Only an active (pending or running) lifecycle counts time against
    /// `started_at`.
    fn is_active(self) -> bool {
        matches!(self, Self::Pending | Self::Running)
    }
}

fn parse_rfc3339(value: Option<&str>) -> Option<DateTime<Utc>> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

/// Shared projection of one tool call's wait status. Pure: no I/O, no clock
/// access beyond the caller-supplied `now`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitStatusProjection {
    pub lifecycle: WaitLifecycle,
    /// Raw persisted lifecycle token, preserved verbatim even when unknown.
    pub lifecycle_state_raw: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    /// Age of an active call in whole seconds. `None` when terminal, unknown,
    /// or when `started_at` is missing/unparseable/in the future.
    pub running_elapsed_seconds: Option<i64>,
    /// Terminal duration in whole seconds (`completed_at - started_at`);
    /// stable across repeated projections. `None` when the lifecycle is not
    /// terminal or either endpoint is missing/unparseable/inverted.
    pub terminal_elapsed_seconds: Option<i64>,
}

impl WaitStatusProjection {
    pub fn from_parts(
        lifecycle_state: Option<&str>,
        started_at: Option<&str>,
        completed_at: Option<&str>,
        now: DateTime<Utc>,
    ) -> Self {
        let lifecycle = WaitLifecycle::from_persisted(lifecycle_state);
        let started_at = parse_rfc3339(started_at);
        let completed_at = parse_rfc3339(completed_at);
        let running_elapsed_seconds = if lifecycle.is_active() {
            started_at
                .filter(|&started| started <= now)
                .map(|started| (now - started).num_seconds().max(0))
        } else {
            None
        };
        let terminal_elapsed_seconds = if lifecycle.is_terminal() {
            match (started_at, completed_at) {
                (Some(started), Some(completed)) if completed >= started => {
                    Some((completed - started).num_seconds())
                }
                _ => None,
            }
        } else {
            None
        };
        Self {
            lifecycle,
            lifecycle_state_raw: lifecycle_state
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            started_at,
            completed_at,
            running_elapsed_seconds,
            terminal_elapsed_seconds,
        }
    }

    /// Truthful status token for consumers that render a single string:
    /// the parsed lifecycle token, or `unknown` when unparseable/absent.
    /// Never falls back to `running`.
    pub fn status_token(&self) -> &'static str {
        self.lifecycle.as_str().unwrap_or("unknown")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> DateTime<Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn running_projection_reports_lifecycle_and_age() {
        let p = WaitStatusProjection::from_parts(
            Some("running"),
            Some("2026-05-20T12:00:00Z"),
            None,
            ts("2026-05-20T12:00:30Z"),
        );
        assert_eq!(p.lifecycle, WaitLifecycle::Running);
        assert_eq!(p.status_token(), "running");
        assert_eq!(p.running_elapsed_seconds, Some(30));
        assert_eq!(p.terminal_elapsed_seconds, None);
    }

    #[test]
    fn pending_is_preserved_not_relabeled_running() {
        let p = WaitStatusProjection::from_parts(
            Some("pending"),
            Some("2026-05-20T12:00:00Z"),
            None,
            ts("2026-05-20T12:00:05Z"),
        );
        assert_eq!(p.lifecycle, WaitLifecycle::Pending);
        assert_eq!(p.status_token(), "pending");
        assert_eq!(p.running_elapsed_seconds, Some(5));
    }

    #[test]
    fn terminal_elapsed_is_stable_regardless_of_now() {
        let p1 = WaitStatusProjection::from_parts(
            Some("completed"),
            Some("2026-05-20T12:00:00Z"),
            Some("2026-05-20T12:01:30Z"),
            ts("2026-05-20T12:02:00Z"),
        );
        let p2 = WaitStatusProjection::from_parts(
            Some("completed"),
            Some("2026-05-20T12:00:00Z"),
            Some("2026-05-20T12:01:30Z"),
            ts("2026-05-20T15:00:00Z"),
        );
        assert_eq!(p1.terminal_elapsed_seconds, Some(90));
        assert_eq!(p2.terminal_elapsed_seconds, Some(90));
        assert_eq!(p1.running_elapsed_seconds, None);
        assert!(p1.lifecycle.is_terminal());
    }

    #[test]
    fn terminal_states_project_without_running_age() {
        for state in ["failed", "timedOut", "cancelled", "completed"] {
            let p = WaitStatusProjection::from_parts(
                Some(state),
                Some("2026-05-20T12:00:00Z"),
                Some("2026-05-20T12:00:10Z"),
                ts("2026-05-20T12:05:00Z"),
            );
            assert!(p.lifecycle.is_terminal(), "{state}");
            assert_eq!(p.running_elapsed_seconds, None);
            assert_eq!(p.terminal_elapsed_seconds, Some(10));
        }
    }

    #[test]
    fn missing_state_is_unknown_not_running() {
        let now = ts("2026-05-20T12:00:30Z");
        for state in [None, Some(""), Some("   "), Some("garbage")] {
            let p =
                WaitStatusProjection::from_parts(state, Some("2026-05-20T12:00:00Z"), None, now);
            assert_eq!(p.lifecycle, WaitLifecycle::Unknown, "{state:?}");
            assert_eq!(p.status_token(), "unknown");
            assert_eq!(p.running_elapsed_seconds, None);
            assert_eq!(p.terminal_elapsed_seconds, None);
        }
    }

    #[test]
    fn malformed_started_at_keeps_lifecycle_but_kills_elapsed() {
        let now = ts("2026-05-20T12:00:30Z");
        for started in [None, Some(""), Some("   "), Some("not-a-time")] {
            let p = WaitStatusProjection::from_parts(Some("running"), started, None, now);
            assert_eq!(p.lifecycle, WaitLifecycle::Running, "{started:?}");
            assert_eq!(p.status_token(), "running");
            assert_eq!(p.started_at, None);
            assert_eq!(p.running_elapsed_seconds, None, "{started:?}");
        }
    }

    #[test]
    fn malformed_terminal_timestamps_keep_terminal_lifecycle() {
        let p = WaitStatusProjection::from_parts(
            Some("failed"),
            Some("not-a-time"),
            Some("2026-05-20T12:00:10Z"),
            ts("2026-05-20T12:05:00Z"),
        );
        assert!(p.lifecycle.is_terminal());
        assert_eq!(p.terminal_elapsed_seconds, None);
    }

    #[test]
    fn clock_behind_started_at_reports_no_negative_age() {
        let p = WaitStatusProjection::from_parts(
            Some("running"),
            Some("2026-05-20T12:00:00Z"),
            None,
            ts("2026-05-20T11:59:00Z"),
        );
        assert_eq!(
            p.running_elapsed_seconds, None,
            "skewed clock claims nothing"
        );
    }

    #[test]
    fn inverted_terminal_timestamps_report_no_duration() {
        let p = WaitStatusProjection::from_parts(
            Some("completed"),
            Some("2026-05-20T12:01:30Z"),
            Some("2026-05-20T12:00:00Z"),
            ts("2026-05-20T12:02:00Z"),
        );
        assert_eq!(
            p.terminal_elapsed_seconds, None,
            "skewed endpoints claim nothing"
        );
    }

    #[test]
    fn terminal_without_completed_at_has_no_duration() {
        let p = WaitStatusProjection::from_parts(
            Some("failed"),
            Some("2026-05-20T12:00:00Z"),
            None,
            ts("2026-05-20T12:05:00Z"),
        );
        assert_eq!(p.terminal_elapsed_seconds, None);
    }

    #[test]
    fn raw_state_is_preserved_verbatim() {
        let p = WaitStatusProjection::from_parts(
            Some("  running "),
            Some("2026-05-20T12:00:00Z"),
            None,
            ts("2026-05-20T12:00:01Z"),
        );
        assert_eq!(p.lifecycle_state_raw.as_deref(), Some("running"));
        let unknown = WaitStatusProjection::from_parts(
            Some("weird-state"),
            None,
            None,
            ts("2026-05-20T12:00:01Z"),
        );
        assert_eq!(unknown.lifecycle_state_raw.as_deref(), Some("weird-state"));
        assert_eq!(unknown.lifecycle, WaitLifecycle::Unknown);
    }
}
