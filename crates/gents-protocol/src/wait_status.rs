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

use crate::tool_call::ToolCallState;

/// Upper bound on recent-output evidence carried per projected entry. Callers
/// produce the preview through the canonical bounded-output slice owner and
/// drill down with their read tool; this constant only caps what is embedded.
pub const PREVIEW_MAX_BYTES: usize = 2048;

/// Serialized projection of the persisted `lifecycle_state`: a
/// [`ToolCallState`], or `Unknown` when the token is absent or unparseable.
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

impl From<ToolCallState> for WaitLifecycle {
    fn from(state: ToolCallState) -> Self {
        match state {
            ToolCallState::Pending => Self::Pending,
            ToolCallState::Running => Self::Running,
            ToolCallState::Completed => Self::Completed,
            ToolCallState::Failed => Self::Failed,
            ToolCallState::TimedOut => Self::TimedOut,
            ToolCallState::Cancelled => Self::Cancelled,
        }
    }
}

impl WaitLifecycle {
    pub fn from_persisted(value: Option<&str>) -> Self {
        value
            .map(str::trim)
            .and_then(ToolCallState::from_persisted)
            .map_or(Self::Unknown, Self::from)
    }

    pub fn tool_call_state(self) -> Option<ToolCallState> {
        match self {
            Self::Pending => Some(ToolCallState::Pending),
            Self::Running => Some(ToolCallState::Running),
            Self::Completed => Some(ToolCallState::Completed),
            Self::Failed => Some(ToolCallState::Failed),
            Self::TimedOut => Some(ToolCallState::TimedOut),
            Self::Cancelled => Some(ToolCallState::Cancelled),
            Self::Unknown => None,
        }
    }

    pub fn as_str(self) -> Option<&'static str> {
        self.tool_call_state().map(ToolCallState::as_str)
    }

    pub fn is_terminal(self) -> bool {
        self.tool_call_state()
            .is_some_and(ToolCallState::is_terminal)
    }

    /// Only an active (pending or running) lifecycle counts time against
    /// `started_at`.
    fn is_active(self) -> bool {
        self.tool_call_state()
            .is_some_and(|state| !state.is_terminal())
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
    fn lifecycle_and_elapsed_projection_cases() {
        let start = Some("2026-05-20T12:00:00Z");
        for (raw, end, now, lifecycle, token, running, terminal) in [
            (
                Some("running"),
                None,
                "2026-05-20T12:00:30Z",
                WaitLifecycle::Running,
                "running",
                Some(30),
                None,
            ),
            (
                Some("  running "),
                None,
                "2026-05-20T12:00:01Z",
                WaitLifecycle::Running,
                "running",
                Some(1),
                None,
            ),
            (
                Some("pending"),
                None,
                "2026-05-20T12:00:05Z",
                WaitLifecycle::Pending,
                "pending",
                Some(5),
                None,
            ),
            (
                Some("completed"),
                Some("2026-05-20T12:01:30Z"),
                "2026-05-20T12:02:00Z",
                WaitLifecycle::Completed,
                "completed",
                None,
                Some(90),
            ),
            (
                Some("completed"),
                Some("2026-05-20T12:01:30Z"),
                "2026-05-20T15:00:00Z",
                WaitLifecycle::Completed,
                "completed",
                None,
                Some(90),
            ),
            (
                Some("failed"),
                Some("2026-05-20T12:00:10Z"),
                "2026-05-20T12:05:00Z",
                WaitLifecycle::Failed,
                "failed",
                None,
                Some(10),
            ),
            (
                Some("timedOut"),
                Some("2026-05-20T12:00:10Z"),
                "2026-05-20T12:05:00Z",
                WaitLifecycle::TimedOut,
                "timedOut",
                None,
                Some(10),
            ),
            (
                Some("cancelled"),
                Some("2026-05-20T12:00:10Z"),
                "2026-05-20T12:05:00Z",
                WaitLifecycle::Cancelled,
                "cancelled",
                None,
                Some(10),
            ),
        ] {
            let p = WaitStatusProjection::from_parts(raw, start, end, ts(now));
            assert_eq!(p.lifecycle, lifecycle, "{raw:?}");
            assert_eq!(p.status_token(), token, "{raw:?}");
            assert_eq!(p.running_elapsed_seconds, running, "{raw:?}");
            assert_eq!(p.terminal_elapsed_seconds, terminal, "{raw:?}");
        }
        assert_eq!(
            WaitStatusProjection::from_parts(
                Some("  running "),
                start,
                None,
                ts("2026-05-20T12:00:01Z")
            )
            .lifecycle_state_raw
            .as_deref(),
            Some("running")
        );
        let unknown = WaitStatusProjection::from_parts(
            Some("weird-state"),
            None,
            None,
            ts("2026-05-20T12:00:01Z"),
        );
        assert_eq!(unknown.lifecycle_state_raw.as_deref(), Some("weird-state"));
        assert_eq!(unknown.lifecycle, WaitLifecycle::Unknown);
    }

    #[test]
    fn missing_malformed_and_skewed_inputs_omit_elapsed() {
        let now = ts("2026-05-20T12:00:30Z");
        for state in [None, Some(""), Some("   "), Some("garbage")] {
            let p =
                WaitStatusProjection::from_parts(state, Some("2026-05-20T12:00:00Z"), None, now);
            assert_eq!(
                (p.lifecycle, p.status_token()),
                (WaitLifecycle::Unknown, "unknown")
            );
            assert_eq!(
                (p.running_elapsed_seconds, p.terminal_elapsed_seconds),
                (None, None)
            );
        }
        for started in [None, Some(""), Some("   "), Some("not-a-time")] {
            let p = WaitStatusProjection::from_parts(Some("running"), started, None, now);
            assert_eq!(
                (p.lifecycle, p.started_at, p.running_elapsed_seconds),
                (WaitLifecycle::Running, None, None)
            );
        }
        for (state, started, ended, at) in [
            (
                "failed",
                Some("not-a-time"),
                Some("2026-05-20T12:00:10Z"),
                "2026-05-20T12:05:00Z",
            ),
            (
                "running",
                Some("2026-05-20T12:00:00Z"),
                None,
                "2026-05-20T11:59:00Z",
            ),
            (
                "completed",
                Some("2026-05-20T12:01:30Z"),
                Some("2026-05-20T12:00:00Z"),
                "2026-05-20T12:02:00Z",
            ),
            (
                "failed",
                Some("2026-05-20T12:00:00Z"),
                None,
                "2026-05-20T12:05:00Z",
            ),
        ] {
            let p = WaitStatusProjection::from_parts(Some(state), started, ended, ts(at));
            assert_eq!(
                (p.running_elapsed_seconds, p.terminal_elapsed_seconds),
                (None, None),
                "{state}"
            );
        }
    }
}
