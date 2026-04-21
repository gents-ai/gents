//! Shared types for request interruption signaling.

use chrono::{DateTime, Utc};

/// Signal sent from scheduler to daemon when a request's `interrupt_requested_at`
/// field flips from null to non-null.
#[derive(Debug, Clone)]
pub struct InterruptIntent {
    /// RFC3339 timestamp the submitter wrote to `interrupt_requested_at`.
    pub at: DateTime<Utc>,
}
