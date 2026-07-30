use super::*;

use gents::startup_readiness::{BuildOutcome, BuildStanding};

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

        assert_eq!(
            !standing.released(),
            case.blocks_ready,
            "{}: released() must agree with the model's Ready-blocking claim",
            case.witness
        );

        if observed == "ready" {
            assert!(
                case.outcomes.iter().any(|outcome| outcome == "started"),
                "{}: ready without a started outcome would fake health",
                case.witness
            );
        }

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
