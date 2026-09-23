//! Explicit execution-lease policy for durable request work (#1571).
//!
//! This is the native production counterpart of the executable Lean owners in
//! `Proofs/RequestExecutionLease/`: it turns observed durable facts into
//! admission, bounded-renewal, and revocation decisions. It is a policy helper,
//! not a state machine: it never reads or writes. Lifecycle-changing writes
//! (begin, renewal, producer decisions, finalization, revocation) must CAS the
//! exact observed generation — and, where the decision reads it, the deadline —
//! in the same transaction under `config_client::txn::MutationWriteGate`. Raw
//! output appends only check the live lease and must not write the request
//! row: admission here never adds a per-flush request CAS.
//!
//! Canonical rules implemented here:
//! - One explicit deadline. Effective expiry is exactly the stored deadline;
//!   there is no output scan. Expiry is inclusive: `now >= deadline` rejects.
//! - Outputs never renew. Raw appends, producer decisions, socket activity and
//!   replay check the live lease without extending it; only
//!   [`authorize_renewal`] advances the deadline.
//! - Bounded renewal: due at `deadline - max(1, duration / 2)`, CAS on the
//!   expected deadline, strictly extending to `now + duration`, and only in
//!   renewable lifecycles (`claimed`, `processing`).
//! - Revocation is external policy authority, independent of wall-clock expiry.
//!
//! The millisecond timeline is the native instantiation of the model's discrete
//! time. The configured duration is a runtime obligation, not chosen here: it
//! must leave scheduling and commit-latency margin, and a duration that cannot
//! strictly extend before expiry is rejected per renewal below.
use gents_protocol::request_lifecycle::RequestLifecycleState;

/// Durable lease facts reread under the mutation gate. There is no
/// `response_streaming` or `progress_seq` input: output-derived liveness is
/// retired, and the stored deadline is the only expiry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseObservation<'a> {
    pub request: RequestLifecycleState,
    pub generation: &'a str,
    /// Stored `execution_lease_expires_at` on the millisecond timeline.
    pub deadline_ms: i64,
}

/// Exact liveness: the observed generation matches and the deadline has not
/// passed. Expiry is inclusive — `now == deadline` is already expired, so the
/// old owner cannot append, publish, dispatch, finalize or renew there even if
/// its generation still matches.
pub fn is_live(observed: LeaseObservation<'_>, expected_generation: &str, now_ms: i64) -> bool {
    now_ms >= 0 && observed.generation == expected_generation && observed.deadline_ms > now_ms
}

/// Lifecycles in which the owned completion loop may keep its explicit lease
/// alive. This is not an implicit relinquishment or output-derived timeout policy.
pub fn renewable_lifecycle(request: RequestLifecycleState) -> bool {
    matches!(
        request,
        RequestLifecycleState::Claimed | RequestLifecycleState::Processing
    )
}

/// Half-duration renewal cadence, the native instantiation of the modeled
/// `renewalInterval duration = max 1 (duration / 2)`. The model's duration is
/// a Nat; a nonpositive native duration is not a valid instantiation and fails
/// closed instead of wrapping.
pub fn renewal_interval_ms(duration_ms: i64) -> Option<i64> {
    if duration_ms <= 0 {
        return None;
    }
    Some(std::cmp::max(1, duration_ms / 2))
}

/// First instant at which renewal is due, using the model's natural-number
/// subtraction. Negative timestamps are outside this timeline.
pub fn renewal_due_ms(duration_ms: i64, deadline_ms: i64) -> Option<i64> {
    if deadline_ms < 0 {
        return None;
    }
    Some((deadline_ms - renewal_interval_ms(duration_ms)?).max(0))
}

/// Deadline a successful renewal installs: `now + duration`. Whenever
/// [`authorize_renewal`] returns `Some`, this strictly extends the observed
/// deadline. Fails closed on a nonpositive duration or on addition overflow.
pub fn renewed_deadline_ms(now_ms: i64, duration_ms: i64) -> Option<i64> {
    if now_ms < 0 || duration_ms <= 0 {
        return None;
    }
    now_ms.checked_add(duration_ms)
}

/// Decide an explicit, bounded lease renewal.
///
/// Returns the new deadline to CAS into `execution_lease_expires_at`, or
/// `None` when the write must be skipped or the owner must stop:
/// - generation or expected-deadline CAS mismatch (reread the authoritative
///   lease; retrying the same expected deadline cannot extend it again),
/// - the lease is already expired (`now >= deadline`),
/// - the renewal is early (`now < renewal_due`): a skipped write, not a
///   failed request,
/// - the new deadline would not strictly extend the observed one,
/// - the lifecycle is not renewable,
/// - the duration is nonpositive (the model's duration is a Nat) or the
///   checked arithmetic overflows.
///
/// Only the owned completion loop's independent renewal poll reaches this
/// decision. Output, publication, dispatch, socket activity and replay never
/// do; a due renewal that wins before expiry prevents recovery, output alone
/// does not.
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

/// Modeled `admitted` plus the `processing` lifecycle that producer writes
/// require. Identity: the caller's transaction must not extend the deadline.
fn authorize_processing_write(
    observed: LeaseObservation<'_>,
    expected_generation: &str,
    now_ms: i64,
) -> bool {
    observed.request == RequestLifecycleState::Processing
        && is_live(observed, expected_generation, now_ms)
}

/// A raw output append (or other non-semantic response write) checks the live
/// lease but never updates it. It requires `processing`, matching the modeled
/// `appendOutput` admission.
pub fn authorize_output_append(
    observed: LeaseObservation<'_>,
    expected_generation: &str,
    now_ms: i64,
) -> bool {
    authorize_processing_write(observed, expected_generation, now_ms)
}

/// Authorizes the request-side generation CAS for a producer transaction
/// (close/retract, accept-and-publish, dispatch). Identity: it never extends
/// the deadline and never changes the lifecycle; the canonical output and
/// publication effects remain composition obligations of their own owners.
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

/// Terminalization admission (modeled `canFinalize`): completion requires an
/// accepted, processing turn; every other outcome may still terminalize
/// claimed work (provider EOF on a claimed request fails it atomically).
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

/// Authorizes normal finalization under the live lease. At or after the
/// deadline the owner cannot finalize, even with a matching generation.
pub fn authorize_finalize(
    observed: LeaseObservation<'_>,
    expected_generation: &str,
    now_ms: i64,
    completed: bool,
) -> bool {
    can_finalize(observed.request, completed) && is_live(observed, expected_generation, now_ms)
}

/// External cancellation and policy revocation are intentionally independent
/// of wall-clock expiry: they may terminate a live or already-expired lease.
/// They must match the exact observed lifecycle, generation and deadline, take
/// a fresh generation, and may only end the request as `dead` or `superseded`.
///
/// The caller CASes every observed field (lifecycle, generation, deadline) in
/// the same transaction and installs the fresh generation on success.
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

    // Modeled boundary: `admitted` requires `now < deadline`; the conformance
    // case `deadline_boundary_rejects_raw_append` fixes now == deadline.
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

    // Modeled `renewalInterval duration = max 1 (duration / 2)`. A nonpositive
    // duration is not a valid Nat instantiation and fails closed.
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

    // Conformance: `explicit_owner_heartbeat_extends_at_due_time`
    // (duration 5, deadline 10, now 8 → 13).
    #[test]
    fn due_renewal_extends_to_now_plus_duration() {
        let observed = observation(RequestLifecycleState::Processing, 10);
        assert_eq!(authorize_renewal(observed, GEN, 10, 5, 8), Some(13));
        // Boundary: now == due - 1 is still early.
        assert_eq!(authorize_renewal(observed, GEN, 10, 5, 7), None);
    }

    // Conformance: `early_renewal_is_rejected` (now 5 < due 8 for deadline 10,
    // duration 5). An early timer poll is a skipped write, not a failure.
    #[test]
    fn early_renewal_is_rejected() {
        let observed = observation(RequestLifecycleState::Processing, 10);
        assert_eq!(authorize_renewal(observed, GEN, 10, 5, 4), None);
    }

    // Conformance: `stale_expected_deadline_cannot_renew` and
    // `same_deadline_replay_after_renewal_is_rejected` — the CAS is on the
    // exact expected deadline.
    #[test]
    fn stale_expected_deadline_cas_is_rejected() {
        let observed = observation(RequestLifecycleState::Processing, 13);
        assert_eq!(authorize_renewal(observed, GEN, 10, 5, 8), None);
    }

    // Conformance: `one_tick_duration_cannot_advance_before_expiry` — a
    // one-tick lease cannot both advance its deadline and renew strictly
    // before expiry, so it is not a usable native renewal configuration.
    #[test]
    fn one_tick_duration_cannot_renew_before_expiry() {
        let observed = observation(RequestLifecycleState::Processing, 10);
        assert_eq!(authorize_renewal(observed, GEN, 10, 1, 9), None);
    }

    // Conformance: `terminal_lifecycle_rejects_renewal_even_with_active_lease`
    // and `foreground_tool_wait_explicitly_renews`.
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

    // Modeled theorem: renewal success strictly advances the deadline
    // (`deadline < now + duration`). The one-tick case above is the modeled
    // `one_tick_duration_cannot_advance_before_expiry` boundary where the
    // strictly-extending guard, not the due check, rejects.
    #[test]
    fn renewal_must_strictly_extend_deadline() {
        // duration 0 can never strictly extend.
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

    // Modeled: `appendOutput` is identity and never renews; producer
    // decisions (`closeOrRetract`, `acceptAndPublish`, `dispatch`) are also
    // identity. Admission requires `processing`.
    #[test]
    fn output_and_producer_writes_never_renew_and_require_processing() {
        let claimed = observation(RequestLifecycleState::Claimed, 10);
        assert!(!authorize_output_append(claimed, GEN, 5));
        assert!(!authorize_producer_decision(claimed, GEN, 5));
        let observed = observation(RequestLifecycleState::Processing, 10);
        assert!(authorize_output_append(observed, GEN, 5));
        assert!(authorize_producer_decision(observed, GEN, 5));
    }

    // Conformance: `completion_rejects_claimed_request` and
    // `provider_eof_fails_claimed_request_atomically`.
    #[test]
    fn finalize_admission_follows_can_finalize() {
        let claimed = observation(RequestLifecycleState::Claimed, 10);
        let processing = observation(RequestLifecycleState::Processing, 10);
        assert!(!authorize_finalize(claimed, GEN, 5, true));
        assert!(authorize_finalize(claimed, GEN, 5, false));
        assert!(authorize_finalize(processing, GEN, 5, true));
        assert!(authorize_finalize(processing, GEN, 5, false));
    }

    // Modeled: `policy_authority_can_supersede_live_generation`,
    // `policy_revocation_rejects_wrong_expected_generation`,
    // `policy_revocation_rejects_non_policy_outcome`. Revocation is
    // independent of wall-clock expiry.
    #[test]
    fn revocation_matches_exact_observation_and_is_expiry_independent() {
        let observed = observation(RequestLifecycleState::Processing, 10);
        let expected = observation(RequestLifecycleState::Processing, 10);
        // Live and expired alike: revocation does not consult the clock.
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
        // Wrong expected deadline.
        assert!(!authorize_execution_revocation(
            observed,
            LeaseObservation {
                deadline_ms: 9,
                ..expected
            },
            OTHER,
            RequestLifecycleState::Dead
        ));
        // Wrong expected generation.
        assert!(!authorize_execution_revocation(
            observed,
            LeaseObservation {
                generation: OTHER,
                ..expected
            },
            OTHER,
            RequestLifecycleState::Dead
        ));
        // Fresh generation must differ from the observed one.
        assert!(!authorize_execution_revocation(
            observed,
            expected,
            GEN,
            RequestLifecycleState::Dead
        ));
        // Only dead/superseded are policy outcomes.
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

    // Modeled: `provider_eof_without_final_fails` and the generated case
    // vocabulary (`providerEofCases`).
    #[test]
    fn provider_eof_requires_explicit_final() {
        assert!(provider_eof_is_failure(false));
        assert!(!provider_eof_is_failure(true));
    }

    // The model's duration is a Nat; the native i64 instantiation must fail
    // closed on nonpositive durations and on checked-add/sub overflow instead
    // of panicking or wrapping.
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
        // A due renewal whose new deadline overflows is rejected, not wrapped.
        let observed = observation(RequestLifecycleState::Processing, i64::MAX - 5);
        assert_eq!(
            authorize_renewal(observed, GEN, i64::MAX - 5, 10, i64::MAX - 5),
            None
        );
    }

    // The revocation CAS covers the lifecycle too: a stale expected lifecycle
    // (the request moved on since the caller read it) must be rejected.
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
        // Symmetrically, a claimed observation against a processing expectation.
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
