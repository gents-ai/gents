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

/// Durable lease facts from the request owner's authoritative snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseObservation<'a> {
    pub request: RequestLifecycleState,
    pub generation: &'a str,
    /// Stored `execution_lease_expires_at` on the millisecond timeline; the
    /// only expiry (inclusive: `now >= deadline` rejects).
    pub deadline_ms: i64,
}

/// The observed generation matches and the deadline has not passed.
pub fn is_live(observed: LeaseObservation<'_>, expected_generation: &str, now_ms: i64) -> bool {
    now_ms >= 0 && observed.generation == expected_generation && observed.deadline_ms > now_ms
}

/// Lifecycles in which the owned completion loop may keep its explicit lease
/// alive.
pub fn renewable_lifecycle(request: RequestLifecycleState) -> bool {
    matches!(
        request,
        RequestLifecycleState::Claimed | RequestLifecycleState::Processing
    )
}

/// Half-duration renewal cadence (modeled `renewalInterval duration = max 1
/// (duration / 2)`). The model's duration is a Nat; a nonpositive native
/// duration is not a valid instantiation and fails closed instead of wrapping.
pub fn renewal_interval_ms(duration_ms: i64) -> Option<i64> {
    if duration_ms <= 0 {
        return None;
    }
    Some(std::cmp::max(1, duration_ms / 2))
}

/// First instant at which renewal is due, using the model's natural-number
/// subtraction.
pub fn renewal_due_ms(duration_ms: i64, deadline_ms: i64) -> Option<i64> {
    if deadline_ms < 0 {
        return None;
    }
    Some((deadline_ms - renewal_interval_ms(duration_ms)?).max(0))
}

/// Deadline a successful renewal installs: `now + duration`.
pub fn renewed_deadline_ms(now_ms: i64, duration_ms: i64) -> Option<i64> {
    if now_ms < 0 || duration_ms <= 0 {
        return None;
    }
    now_ms.checked_add(duration_ms)
}

/// Decide an explicit, bounded lease renewal.
///
/// Returns the new deadline to CAS into `execution_lease_expires_at`, or
/// `None` when the write must be skipped or the owner must stop (CAS mismatch,
/// expiry, early renewal, non-strict extension, non-renewable lifecycle,
/// nonpositive duration, or overflow). Only the owned completion loop's
/// independent renewal poll reaches this decision; output, publication,
/// dispatch, socket activity and replay never do.
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

/// Modeled `admitted` plus the `processing` lifecycle producer writes require;
/// the caller's transaction must not extend the deadline.
fn authorize_processing_write(
    observed: LeaseObservation<'_>,
    expected_generation: &str,
    now_ms: i64,
) -> bool {
    observed.request == RequestLifecycleState::Processing
        && is_live(observed, expected_generation, now_ms)
}

/// A raw output append checks the live lease but never renews it.
pub fn authorize_output_append(
    observed: LeaseObservation<'_>,
    expected_generation: &str,
    now_ms: i64,
) -> bool {
    authorize_processing_write(observed, expected_generation, now_ms)
}

/// Authorizes the request-side generation CAS for a producer transaction
/// (close/retract, accept-and-publish, dispatch); never extends the deadline.
pub fn authorize_producer_decision(
    observed: LeaseObservation<'_>,
    expected_generation: &str,
    now_ms: i64,
) -> bool {
    authorize_processing_write(observed, expected_generation, now_ms)
}

/// Authorizes the claimed → processing transition (modeled `begin`).
pub fn authorize_begin(
    observed: LeaseObservation<'_>,
    expected_generation: &str,
    now_ms: i64,
) -> bool {
    observed.request == RequestLifecycleState::Claimed
        && is_live(observed, expected_generation, now_ms)
}

/// Terminalization admission (modeled `canFinalize`): completion requires a
/// processing turn; every other outcome may terminalize claimed work.
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

/// Authorizes normal finalization under the live lease.
pub fn authorize_finalize(
    observed: LeaseObservation<'_>,
    expected_generation: &str,
    now_ms: i64,
    completed: bool,
) -> bool {
    can_finalize(observed.request, completed) && is_live(observed, expected_generation, now_ms)
}

/// External cancellation and policy revocation are independent of wall-clock
/// expiry. The caller CASes every observed field (lifecycle, generation,
/// deadline) in the same transaction and installs the fresh generation.
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

/// Transport EOF is insufficient to certify a completed provider turn.
pub fn provider_eof_is_failure(saw_explicit_final: bool) -> bool {
    !saw_explicit_final
}

#[cfg(test)]
mod tests {
    use super::*;

    const GEN: &str = "gen-1";
    const OTHER: &str = "gen-2";

    fn observation(request: RequestLifecycleState, deadline_ms: i64) -> LeaseObservation<'static> {
        LeaseObservation {
            request,
            generation: GEN,
            deadline_ms,
        }
    }

    #[test]
    fn expiry_is_inclusive_across_every_admission() {
        let observed = observation(RequestLifecycleState::Processing, 10);
        assert!(!is_live(observed, GEN, 10));
        assert!(!is_live(observed, GEN, 11));
        assert!(is_live(observed, GEN, 9));
        assert!(!authorize_output_append(observed, GEN, 10));
        assert!(!authorize_producer_decision(observed, GEN, 10));
        assert!(!authorize_begin(
            observation(RequestLifecycleState::Claimed, 10),
            GEN,
            10
        ));
        assert!(!authorize_finalize(observed, GEN, 10, true));
        assert!(authorize_renewal(observed, GEN, 10, 5, 10).is_none());
    }

    #[test]
    fn stale_generation_rejects_every_admission() {
        let observed = observation(RequestLifecycleState::Processing, 20);
        assert!(!is_live(observed, OTHER, 5));
        assert!(!authorize_output_append(observed, OTHER, 5));
        assert!(!authorize_producer_decision(observed, OTHER, 5));
        assert!(!authorize_finalize(observed, OTHER, 5, true));
        assert!(authorize_renewal(observed, OTHER, 20, 5, 10).is_none());
    }

    #[test]
    fn renewal_interval_matches_model() {
        assert_eq!(renewal_interval_ms(5), Some(2));
        assert_eq!(renewal_interval_ms(4), Some(2));
        assert_eq!(renewal_interval_ms(1), Some(1));
        assert_eq!(renewal_interval_ms(0), None);
        assert_eq!(renewal_interval_ms(-3), None);
        assert_eq!(renewal_due_ms(5, 10), Some(8));
        assert_eq!(renewal_due_ms(0, 10), None);
    }

    #[test]
    fn due_renewal_extends_to_now_plus_duration() {
        let observed = observation(RequestLifecycleState::Processing, 10);
        assert_eq!(authorize_renewal(observed, GEN, 10, 5, 8), Some(13));
        assert_eq!(authorize_renewal(observed, GEN, 10, 5, 7), None);
    }

    #[test]
    fn early_renewal_is_rejected() {
        let observed = observation(RequestLifecycleState::Processing, 10);
        assert_eq!(authorize_renewal(observed, GEN, 10, 5, 4), None);
    }

    #[test]
    fn stale_expected_deadline_cas_is_rejected() {
        let observed = observation(RequestLifecycleState::Processing, 13);
        assert_eq!(authorize_renewal(observed, GEN, 10, 5, 8), None);
    }

    #[test]
    fn one_tick_duration_cannot_renew_before_expiry() {
        let observed = observation(RequestLifecycleState::Processing, 10);
        assert_eq!(authorize_renewal(observed, GEN, 10, 1, 9), None);
    }

    #[test]
    fn renewable_lifecycle_includes_claimed_and_processing_and_excludes_terminal() {
        let deadline = 10;
        for (request, ok) in [
            (RequestLifecycleState::Claimed, true),
            (RequestLifecycleState::Processing, true),
            (RequestLifecycleState::Pending, false),
            (RequestLifecycleState::WorkspaceBindingPending, false),
            (RequestLifecycleState::Completed, false),
            (RequestLifecycleState::Failed, false),
            (RequestLifecycleState::Superseded, false),
            (RequestLifecycleState::Dead, false),
            (RequestLifecycleState::Interrupted, false),
        ] {
            assert_eq!(renewable_lifecycle(request), ok, "{request:?}");
            let expected = if ok { Some(13) } else { None };
            assert_eq!(
                authorize_renewal(observation(request, deadline), GEN, deadline, 5, 8),
                expected,
                "{request:?}"
            );
        }
    }

    #[test]
    fn renewal_must_strictly_extend_deadline() {
        assert_eq!(
            authorize_renewal(
                observation(RequestLifecycleState::Processing, 10),
                GEN,
                10,
                0,
                9
            ),
            None
        );
    }

    #[test]
    fn output_and_producer_writes_never_renew_and_require_processing() {
        let claimed = observation(RequestLifecycleState::Claimed, 10);
        assert!(!authorize_output_append(claimed, GEN, 5));
        assert!(!authorize_producer_decision(claimed, GEN, 5));
        let observed = observation(RequestLifecycleState::Processing, 10);
        assert!(authorize_output_append(observed, GEN, 5));
        assert!(authorize_producer_decision(observed, GEN, 5));
    }

    #[test]
    fn finalize_admission_follows_can_finalize() {
        let claimed = observation(RequestLifecycleState::Claimed, 10);
        let processing = observation(RequestLifecycleState::Processing, 10);
        assert!(!authorize_finalize(claimed, GEN, 5, true));
        assert!(authorize_finalize(claimed, GEN, 5, false));
        assert!(authorize_finalize(processing, GEN, 5, true));
        assert!(authorize_finalize(processing, GEN, 5, false));
    }

    #[test]
    fn revocation_matches_exact_observation_and_is_expiry_independent() {
        let observed = observation(RequestLifecycleState::Processing, 10);
        let expected = observation(RequestLifecycleState::Processing, 10);
        assert!(authorize_execution_revocation(
            observed,
            expected,
            OTHER,
            RequestLifecycleState::Superseded
        ));
        assert!(authorize_execution_revocation(
            observed,
            expected,
            OTHER,
            RequestLifecycleState::Dead
        ));
        assert!(!authorize_execution_revocation(
            observed,
            LeaseObservation {
                deadline_ms: 9,
                ..expected
            },
            OTHER,
            RequestLifecycleState::Dead
        ));
        assert!(!authorize_execution_revocation(
            observed,
            LeaseObservation {
                generation: OTHER,
                ..expected
            },
            OTHER,
            RequestLifecycleState::Dead
        ));
        assert!(!authorize_execution_revocation(
            observed,
            expected,
            GEN,
            RequestLifecycleState::Dead
        ));
        for outcome in [
            RequestLifecycleState::Completed,
            RequestLifecycleState::Failed,
            RequestLifecycleState::Interrupted,
        ] {
            assert!(
                !authorize_execution_revocation(observed, expected, OTHER, outcome),
                "{outcome:?}"
            );
        }
    }

    #[test]
    fn provider_eof_requires_explicit_final() {
        assert!(provider_eof_is_failure(false));
        assert!(!provider_eof_is_failure(true));
    }

    #[test]
    fn nonpositive_duration_fails_closed() {
        let observed = observation(RequestLifecycleState::Processing, 10);
        for duration in [0, -1, i64::MIN] {
            assert_eq!(renewal_interval_ms(duration), None, "{duration}");
            assert_eq!(renewed_deadline_ms(5, duration), None, "{duration}");
            assert_eq!(renewal_due_ms(duration, 10), None, "{duration}");
            assert_eq!(authorize_renewal(observed, GEN, 10, duration, 8), None);
        }
    }

    #[test]
    fn checked_timeline_arithmetic_fails_closed_on_overflow() {
        assert_eq!(renewed_deadline_ms(i64::MAX, 1), None);
        assert_eq!(renewed_deadline_ms(i64::MAX - 1, 2), None);
        assert_eq!(renewed_deadline_ms(i64::MIN, 1), None);
        assert_eq!(renewal_due_ms(1, i64::MIN), None);
        assert_eq!(renewal_due_ms(1, 0), Some(0));
        let observed = observation(RequestLifecycleState::Processing, i64::MAX - 5);
        assert_eq!(
            authorize_renewal(observed, GEN, i64::MAX - 5, 10, i64::MAX - 5),
            None
        );
    }

    #[test]
    fn stale_expected_lifecycle_rejects_revocation() {
        let observed = observation(RequestLifecycleState::Processing, 10);
        let expected = observation(RequestLifecycleState::Claimed, 10);
        assert!(!authorize_execution_revocation(
            observed,
            expected,
            OTHER,
            RequestLifecycleState::Dead
        ));
        let observed = observation(RequestLifecycleState::Claimed, 10);
        let expected = observation(RequestLifecycleState::Processing, 10);
        assert!(!authorize_execution_revocation(
            observed,
            expected,
            OTHER,
            RequestLifecycleState::Superseded
        ));
    }
}
