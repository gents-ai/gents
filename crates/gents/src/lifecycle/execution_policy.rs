//! Authorization shared by durable execution writes and generated Lean cases.
//! Database writers must CAS the authorized observation in the same transaction.
use gents_protocol::request_lifecycle::RequestLifecycleState;

#[derive(Debug, Clone, Copy)]
pub(crate) struct ExecutionObservation<'a> {
    pub(crate) request: RequestLifecycleState,
    /// None means no response; false means an already-terminal response.
    pub(crate) response_streaming: Option<bool>,
    pub(crate) generation: &'a str,
    pub(crate) deadline: i64,
    pub(crate) progress_seq: u64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ExecutionOperation {
    Begin,
    Progress {
        new_deadline: i64,
    },
    Finalize {
        completed: bool,
    },
    /// A non-semantic response write is fenced, but never extends the lease.
    Observe,
}

pub(crate) fn authorize_live_execution(
    observed: ExecutionObservation<'_>,
    expected_generation: &str,
    now: i64,
    operation: ExecutionOperation,
) -> bool {
    if observed.generation != expected_generation || observed.deadline < now {
        return false;
    }
    let claimed =
        observed.request == RequestLifecycleState::Claimed && observed.response_streaming.is_none();
    let processing = observed.request == RequestLifecycleState::Processing
        && observed.response_streaming == Some(true);
    match operation {
        ExecutionOperation::Begin => claimed,
        ExecutionOperation::Progress { new_deadline } => {
            processing && observed.deadline < new_deadline
        }
        ExecutionOperation::Finalize { completed } => processing || (!completed && claimed),
        ExecutionOperation::Observe => processing,
    }
}

/// External cancellation is intentionally independent of wall-clock expiry.
/// Revocation takes a fresh generation, and must match every observed CAS field.
pub(crate) fn authorize_execution_revocation(
    observed: ExecutionObservation<'_>,
    expected: ExecutionObservation<'_>,
    fresh_generation: &str,
    outcome: RequestLifecycleState,
) -> bool {
    let active_pair = (observed.request == RequestLifecycleState::Claimed
        && observed.response_streaming.is_none())
        || (observed.request == RequestLifecycleState::Processing
            && observed.response_streaming == Some(true));
    active_pair
        && observed.generation == expected.generation
        && observed.deadline == expected.deadline
        && observed.progress_seq == expected.progress_seq
        && fresh_generation != observed.generation
        && matches!(
            outcome,
            RequestLifecycleState::Dead | RequestLifecycleState::Superseded
        )
}

/// Transport EOF is insufficient to certify a completed provider turn.
pub(crate) fn provider_eof_is_failure(saw_explicit_final: bool) -> bool {
    !saw_explicit_final
}
