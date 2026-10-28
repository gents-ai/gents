use super::*;

pub(super) fn generated_backend_health_cases_pin_threshold_and_veto_shape() {
    let cases = lean_backend_health_cases();
    assert!(
        !cases.is_empty(),
        "Lean must emit backend health transition cases"
    );
    assert_eq!(
        gents::BackendProberOptions::default().failure_threshold_k,
        3,
        "backend prober default threshold drifted; update the required K coverage below"
    );
    for k in 1..=3usize {
        assert!(
            cases.iter().any(|case| case.threshold_k == k),
            "K={k} rows must be emitted (K=3 is the production default)"
        );
    }

    assert!(
        cases.iter().any(|case| {
            case.threshold_k == 3
                && case.start_state == "degraded"
                && case.start_count == 2
                && case.event == "probeFail"
                && case.next_state == "unhealthy"
        }),
        "B1 witness: at K=3 the third consecutive probeFail must demote to unhealthy"
    );

    let owner_states = [
        gents::BackendHealthState::Unknown,
        gents::BackendHealthState::Healthy,
        gents::BackendHealthState::Degraded,
        gents::BackendHealthState::Unhealthy,
        gents::BackendHealthState::CredentialsRequired,
        gents::BackendHealthState::SubscriptionNotEntitled,
    ];

    for case in cases {
        assert!(
            case.threshold_k >= 1,
            "case {} must carry a positive failure threshold",
            case.name
        );
        assert!(
            matches!(
                case.event.as_str(),
                "probeSuccess"
                    | "probeFail"
                    | "credentialsRejected"
                    | "subscriptionNotEntitled"
                    | "configurationChanged"
            ),
            "case {} carries unknown backend health event {:?}",
            case.name,
            case.event
        );
        let next_state = owner_states
            .iter()
            .copied()
            .find(|owner| owner.as_str() == case.next_state.as_str())
            .unwrap_or_else(|| {
                panic!(
                    "case {} emitted next_state {:?} outside the BackendHealthState owner vocabulary",
                    case.name, case.next_state
                )
            });
        assert_eq!(
            next_state.blocks_routing(),
            case.blocks_routing,
            "case {}: the routing veto must be the owner's projection of the next state",
            case.name
        );
    }

    for state in owner_states {
        assert!(
            cases.iter().any(|case| case.start_state == state.as_str()),
            "the emitted cases must start from every measured health state; missing {:?}",
            state.as_str()
        );
    }
}
