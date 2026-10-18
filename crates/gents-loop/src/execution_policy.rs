//! Native counterpart of the executable Lean owners in
//! `Proofs/RequestExecutionLease/`: turns observed durable facts into
//! admission, bounded-renewal, and revocation decisions. Policy helper only —
//! it never reads or writes. Lifecycle-changing writes (begin, renewal,
//! producer decisions, finalization, revocation) must CAS the exact observed
//! generation — and, where the decision reads it, the deadline — in the same
//! transaction under `config_client::txn::MutationWriteGate`. Raw output
//! appends only check the live lease and must not write the request row.
//!
//! The configured duration is a runtime obligation, not chosen here: it must
//! leave scheduling and commit-latency margin.
use gents_protocol::request_lifecycle::RequestLifecycleState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseObservation<'a> {
    pub request: RequestLifecycleState,
    pub generation: &'a str,
    pub deadline_ms: i64,
}

pub fn is_live(observed: LeaseObservation<'_>, expected_generation: &str, now_ms: i64) -> bool {
    now_ms >= 0 && observed.generation == expected_generation && observed.deadline_ms > now_ms
}

pub fn renewable_lifecycle(request: RequestLifecycleState) -> bool {
    matches!(
        request,
        RequestLifecycleState::Claimed | RequestLifecycleState::Processing
    )
}

pub fn renewal_interval_ms(duration_ms: i64) -> Option<i64> {
    if duration_ms <= 0 {
        return None;
    }
    Some(std::cmp::max(1, duration_ms / 2))
}

pub fn renewal_due_ms(duration_ms: i64, deadline_ms: i64) -> Option<i64> {
    if deadline_ms < 0 {
        return None;
    }
    Some((deadline_ms - renewal_interval_ms(duration_ms)?).max(0))
}

pub fn renewed_deadline_ms(now_ms: i64, duration_ms: i64) -> Option<i64> {
    if now_ms < 0 || duration_ms <= 0 {
        return None;
    }
    now_ms.checked_add(duration_ms)
}

pub fn authorize_renewal(
    observed: LeaseObservation<'_>,
    expected_generation: &str,
    expected_deadline_ms: i64,
    duration_ms: i64,
    now_ms: i64,
) -> Option<i64> {
    let new_deadline_ms = renewed_deadline_ms(now_ms, duration_ms)?;
    if observed.generation != expected_generation
        || observed.deadline_ms != expected_deadline_ms
        || observed.deadline_ms <= now_ms
        || now_ms < renewal_due_ms(duration_ms, observed.deadline_ms)?
        || observed.deadline_ms >= new_deadline_ms
        || !renewable_lifecycle(observed.request)
    {
        return None;
    }
    Some(new_deadline_ms)
}

fn authorize_processing_write(
    observed: LeaseObservation<'_>,
    expected_generation: &str,
    now_ms: i64,
) -> bool {
    observed.request == RequestLifecycleState::Processing
        && is_live(observed, expected_generation, now_ms)
}

pub fn authorize_output_append(
    observed: LeaseObservation<'_>,
    expected_generation: &str,
    now_ms: i64,
) -> bool {
    authorize_processing_write(observed, expected_generation, now_ms)
}

pub fn authorize_producer_decision(
    observed: LeaseObservation<'_>,
    expected_generation: &str,
    now_ms: i64,
) -> bool {
    authorize_processing_write(observed, expected_generation, now_ms)
}

pub fn authorize_begin(
    observed: LeaseObservation<'_>,
    expected_generation: &str,
    now_ms: i64,
) -> bool {
    observed.request == RequestLifecycleState::Claimed
        && is_live(observed, expected_generation, now_ms)
}

pub fn can_finalize(request: RequestLifecycleState, completed: bool) -> bool {
    if completed {
        request == RequestLifecycleState::Processing
    } else {
        matches!(
            request,
            RequestLifecycleState::Claimed | RequestLifecycleState::Processing
        )
    }
}

pub fn authorize_finalize(
    observed: LeaseObservation<'_>,
    expected_generation: &str,
    now_ms: i64,
    completed: bool,
) -> bool {
    can_finalize(observed.request, completed) && is_live(observed, expected_generation, now_ms)
}

pub fn authorize_execution_revocation(
    observed: LeaseObservation<'_>,
    expected: LeaseObservation<'_>,
    fresh_generation: &str,
    outcome: RequestLifecycleState,
) -> bool {
    let active_pair = matches!(
        observed.request,
        RequestLifecycleState::Claimed | RequestLifecycleState::Processing
    );
    active_pair
        && observed.request == expected.request
        && observed.generation == expected.generation
        && observed.deadline_ms == expected.deadline_ms
        && fresh_generation != observed.generation
        && matches!(
            outcome,
            RequestLifecycleState::Dead | RequestLifecycleState::Superseded
        )
}

pub fn provider_eof_is_failure(saw_explicit_final: bool) -> bool {
    !saw_explicit_final
}
