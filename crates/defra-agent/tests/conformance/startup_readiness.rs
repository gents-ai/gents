use super::*;

use defra_agent::startup_readiness::{BuildOutcome, BuildStanding};

/// Fence for the bounded startup-readiness barrier (#559).
///
/// Drives the real `BuildStanding` — the same state machine the slot loop
/// steps — through every vector the Lean model emits. The barrier used to hang
/// forever on a behavior that was snapshot-runnable but persistently failed to
/// build its client; these vectors pin that a spent budget (or a retirement)
/// releases the barrier without ever claiming health, and that nothing anywhere
/// requires a process restart.
pub(super) fn generated_startup_readiness_cases_pin_bounded_barrier_release() {
    let cases = lean_startup_readiness_cases();
    assert_eq!(
        cases.len(),
        6,
        "the Lean startup-readiness contract must stay fully consumed"
    );

    for case in cases {
        assert!(
            !case.requires_restart,
            "{}: release must follow from the budget or retirement, never a restart",
            case.witness
        );
        let budget = u32::try_from(case.budget).expect("budget fits u32");
        assert!(
            budget > 0,
            "{}: the runtime enforces budget >= 1",
            case.witness
        );

        let mut standing = BuildStanding::seeded();
        for outcome in &case.outcomes {
            let outcome = match outcome.as_str() {
                "started" => BuildOutcome::Started,
                "failed" => BuildOutcome::Failed,
                other => panic!("{}: unmodeled outcome {other:?}", case.witness),
            };
            standing = standing.step(budget, outcome);
        }
        if case.retired_after {
            standing = standing.retire();
        }

        let observed = match standing {
            BuildStanding::Pending { .. } => "pending",
            BuildStanding::Ready => "ready",
            BuildStanding::Demoted => "demoted",
            BuildStanding::Superseded => "superseded",
        };
        assert_eq!(
            observed, case.post_standing,
            "{}: the runtime's standing must land where the model says",
            case.witness
        );

        // `blocks_ready` is the liveness claim: only a pending standing may
        // hold the barrier, and every released standing lets Ready fire.
        assert_eq!(
            !standing.released(),
            case.blocks_ready,
            "{}: released() must agree with the model's Ready-blocking claim",
            case.witness
        );

        // Health soundness: `ready` requires a genuine start
        // (RuntimeReconcile.StartupReadiness.ready_requires_a_start).
        if observed == "ready" {
            assert!(
                case.outcomes.iter().any(|outcome| outcome == "started"),
                "{}: ready without a started outcome would fake health",
                case.witness
            );
        }

        // Absorption: further outcomes never move a released standing
        // (released_absorbing), so a demotion can never flap back.
        if standing.released() {
            let after = standing
                .step(budget, BuildOutcome::Failed)
                .step(budget, BuildOutcome::Started);
            assert_eq!(
                after, standing,
                "{}: released standings must be absorbing",
                case.witness
            );
        }
    }
}
