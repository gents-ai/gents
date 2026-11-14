//! Native counterpart of the executable Lean owners in
//! `Proofs/Session/RequestExecutionLease/RequestExecutionLease/`: turns observed durable facts into
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
        RequestLifecycleState::Claimed
            | RequestLifecycleState::Processing
            | RequestLifecycleState::ResumingBackend
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
    matches!(
        observed.request,
        RequestLifecycleState::Claimed | RequestLifecycleState::ResumingBackend
    ) && is_live(observed, expected_generation, now_ms)
}

pub fn can_finalize(request: RequestLifecycleState, completed: bool) -> bool {
    if completed {
        request == RequestLifecycleState::Processing
    } else {
        matches!(
            request,
            RequestLifecycleState::Claimed
                | RequestLifecycleState::Processing
                | RequestLifecycleState::ResumingBackend
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

/// Mirrors the outcome guard of Lean `RequestExecutionLease.finalizeParked`: a
/// parked request holds no lease, so interruption, deadline expiry and policy
/// supersession settle it here rather than through revocation.
pub fn can_finalize_parked(request: RequestLifecycleState, outcome: RequestLifecycleState) -> bool {
    request == RequestLifecycleState::WaitingForBackend
        && matches!(
            outcome,
            RequestLifecycleState::Failed
                | RequestLifecycleState::Interrupted
                | RequestLifecycleState::Superseded
        )
}

pub fn authorize_execution_revocation(
    observed: LeaseObservation<'_>,
    expected: LeaseObservation<'_>,
    fresh_generation: &str,
    outcome: RequestLifecycleState,
) -> bool {
    let active_pair = matches!(
        observed.request,
        RequestLifecycleState::Claimed
            | RequestLifecycleState::Processing
            | RequestLifecycleState::ResumingBackend
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

#[cfg(test)]
mod conformance {
    use super::*;

    #[test]
    fn parked_terminal_outcomes_match_the_generated_model() {
        gents_lean_vocab::families::backend_wake_parked_finalize::replay::<
            RequestLifecycleState,
            RequestLifecycleState,
        >(|i| can_finalize_parked(i.request, i.outcome));
    }

    #[test]
    fn provider_eof_is_failure_matches_lean() {
        gents_lean_vocab::families::request_execution_lease_provider_eof_is_failure::replay(|i| {
            provider_eof_is_failure(i.saw_explicit_final)
        });
    }

    #[test]
    fn native_admission_follows_the_lean_lease_model() {
        gents_lean_vocab::families::request_execution_lease_contracts_admission::replay::<
            RequestLifecycleState,
        >(|case| {
            let generation = case.pre.lease.generation.expect("active lease").to_string();
            let observed = LeaseObservation {
                request: case.pre.request,
                generation: &generation,
                deadline_ms: i64::try_from(case.pre.lease.explicit_deadline.expect("active lease"))
                    .unwrap(),
            };
            let expected = case.action.generation.expect("fenced action").to_string();
            let now = i64::try_from(case.pre.now).unwrap();
            match case.action.kind.as_str() {
                "begin" => authorize_begin(observed, &expected, now),
                "append_output" => authorize_output_append(observed, &expected, now),
                "authorize_producer_decision" => {
                    authorize_producer_decision(observed, &expected, now)
                }
                kind => panic!("{}: no native admission for {kind}", case.name),
            }
        });
    }

    #[test]
    fn native_renewal_follows_the_lean_lease_model() {
        gents_lean_vocab::families::request_execution_lease_contracts_renewal::replay::<
            RequestLifecycleState,
        >(|case| {
            assert_eq!(case.action.kind, "renew", "{}", case.name);
            let generation = case.pre.lease.generation.expect("active lease").to_string();
            let observed = LeaseObservation {
                request: case.pre.request,
                generation: &generation,
                deadline_ms: i64::try_from(case.pre.lease.explicit_deadline.expect("active lease"))
                    .unwrap(),
            };
            authorize_renewal(
                observed,
                &case.action.generation.expect("fenced renewal").to_string(),
                i64::try_from(case.action.expected_deadline.expect("renewal CAS deadline"))
                    .unwrap(),
                i64::try_from(case.pre.lease.duration.expect("active lease")).unwrap(),
                i64::try_from(case.pre.now).unwrap(),
            )
            .map(|deadline| u64::try_from(deadline).unwrap())
        });
    }
}
