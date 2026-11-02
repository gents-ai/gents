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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PreclaimSignal {
    None,
    Terminal,
    Malformed,
}

pub(super) fn is_pending(row: &AgentRequestRow) -> bool {
    row.lifecycle_state == Some(RequestLifecycleState::Pending)
}

pub(super) fn preclaim_signal(row: &AgentRequestRow) -> PreclaimSignal {
    if normalize_optional_string(row.interrupt_requested_at.clone()).is_some() {
        return PreclaimSignal::Terminal;
    }
    match crate::request::lifecycle::parse_valid_until(
        row.valid_until.as_deref(),
        chrono::Utc::now(),
    ) {
        crate::request::lifecycle::TtlOutcome::Expired(_) => PreclaimSignal::Terminal,
        crate::request::lifecycle::TtlOutcome::Malformed(_) => PreclaimSignal::Malformed,
        crate::request::lifecycle::TtlOutcome::NotSet
        | crate::request::lifecycle::TtlOutcome::Live(_) => PreclaimSignal::None,
    }
}

pub(super) fn is_aged_background_completion_wakeup(
    row: &AgentRequestRow,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    if row.execution_origin.as_deref() != Some("scheduled")
        || !row
            .input
            .as_ref()
            .and_then(|input| input.queue.as_ref())
            .is_some_and(|queue| {
                queue.source == gents_protocol::request_input::QueueSource::BackgroundCompletion
                    && queue.policy == gents_protocol::request_input::QueuePolicy::Coalesce
                    && queue
                        .key
                        .as_deref()
                        .is_some_and(|key| !key.trim().is_empty())
            })
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
