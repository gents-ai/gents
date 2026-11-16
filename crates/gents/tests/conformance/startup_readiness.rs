use gents::runtime::startup_readiness::{BuildOutcome, BuildStanding, StartupReadinessOptions};

pub(super) fn generated_startup_readiness_cases_pin_bounded_barrier_release() {
    assert!(StartupReadinessOptions::default().build_failure_budget > 0);
    gents_lean_vocab::families::contracts_startup_readiness::replay::<String>(|case| {
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

        let post_standing = match standing {
            BuildStanding::Pending { .. } => "pending",
            BuildStanding::Ready => "ready",
            BuildStanding::Demoted => "demoted",
            BuildStanding::Superseded => "superseded",
        };

        if standing == BuildStanding::Ready {
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

        gents_lean_vocab::families::contracts_startup_readiness::StartupReadinessOutput {
            post_standing: post_standing.to_string(),
            blocks_ready: !standing.released(),
        }
    });
}
