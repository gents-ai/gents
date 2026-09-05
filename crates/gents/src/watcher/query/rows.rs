use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

pub(super) const BACKGROUND_COMPLETION_AGING_THRESHOLD: chrono::Duration =
    chrono::Duration::seconds(30);

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

/// Result of [`AgentRequestRow::preclaim_signal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PreclaimSignal {
    /// No terminal or malformed signal; ordinary session-based claimability
    /// applies.
    None,
    /// Interrupted, or `valid_until` has expired: claim immediately so the
    /// row can be terminalized.
    Terminal,
    /// `valid_until` is present but did not parse: never claimable.
    Malformed,
}

pub(super) fn is_pending(row: &AgentRequestRow) -> bool {
    row.lifecycle_state == Some(RequestLifecycleState::Pending)
}

/// Pre-claim disposition from `interrupt_requested_at` and `valid_until`,
/// checked before the watcher issues any claim-scoped query.
pub(super) fn preclaim_signal(row: &AgentRequestRow) -> PreclaimSignal {
    if normalize_optional_string(row.interrupt_requested_at.clone()).is_some() {
        return PreclaimSignal::Terminal;
    }
    match crate::lifecycle::parse_valid_until(row.valid_until.as_deref(), chrono::Utc::now()) {
        crate::lifecycle::TtlOutcome::Expired(_) => PreclaimSignal::Terminal,
        // Fail closed: an unparseable TTL is not evidence the request is
        // still live, so it must not be claimed as if unset.
        crate::lifecycle::TtlOutcome::Malformed(_) => PreclaimSignal::Malformed,
        crate::lifecycle::TtlOutcome::NotSet | crate::lifecycle::TtlOutcome::Live(_) => {
            PreclaimSignal::None
        }
    }
}

pub(super) fn is_aged_background_completion_wakeup(
    row: &AgentRequestRow,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    if row.execution_origin.as_deref() != Some("scheduled")
        || !crate::lifecycle::queue::is_automated_wakeup(row.metadata.as_deref())
    {
        return false;
    }
    row.created_at
        .as_deref()
        .and_then(|created_at| chrono::DateTime::parse_from_rfc3339(created_at).ok())
        .map(|created_at| {
            now.signed_duration_since(created_at.with_timezone(&chrono::Utc))
                >= BACKGROUND_COMPLETION_AGING_THRESHOLD
        })
        .unwrap_or(false)
}
