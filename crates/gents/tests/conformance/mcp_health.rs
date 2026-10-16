use super::*;

pub(super) fn generated_mcp_health_cases_pin_threshold_projection_shape() {
    let cases = lean_mcp_health_cases();
    assert!(
        !cases.is_empty(),
        "Lean must emit MCP health transition cases"
    );

    assert_eq!(
        gents::HealthCheckerOptions::default().failure_threshold_k,
        3,
        "health checker default threshold drifted; update the required K coverage below"
    );
    assert!(
        cases.iter().any(|case| case.threshold_k == 3),
        "K=3 rows (the production health checker default) must be emitted"
    );
    assert!(
        cases.iter().any(|case| case.threshold_k >= 2),
        "K>=2 hysteresis rows must stay emitted alongside the K=1 collapse subset"
    );

    let k1 = cases
        .iter()
        .filter(|case| case.threshold_k == 1)
        .collect::<Vec<_>>();
    assert!(
        !k1.is_empty(),
        "the K=1 collapse subset (a single probeFail evicts) must be emitted"
    );
    assert!(
        k1.iter().any(|case| {
            case.start_state == "healthy"
                && case.event == "probeFail"
                && case.next_state.as_deref() == Some("evicted")
        }),
        "H7: at K=1 the first probeFail must collapse healthy -> evicted directly"
    );

    let starts = cases
        .iter()
        .map(|case| case.start_state.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    for state in gents_protocol::tool_service_health::ToolServiceHealthState::ALL {
        assert!(
            starts.contains(state.as_str()),
            "missing persisted state {state:?}"
        );
    }

    for case in cases {
        assert!(
            case.threshold_k >= 1,
            "case {} must carry a positive failure threshold",
            case.name
        );
        if case.next_state.as_deref() == Some("evicted") {
            assert_eq!(
                case.event, "probeFail",
                "case {}: eviction is probe-failure driven only",
                case.name
            );
        }
        assert_eq!(
            case.next_state.is_some(),
            case.next_count.is_some(),
            "case {}: removal drops both state and count together",
            case.name
        );
        assert_eq!(
            case.rust_projection.is_some(),
            case.next_state.is_some(),
            "case {}: the operator projection exists exactly when the model survives",
            case.name
        );

        if let Some(next_state) = case.next_state.as_deref() {
            let parsed =
                gents_protocol::tool_service_health::ToolServiceHealthState::parse(next_state)
                    .unwrap_or_else(|error| {
                        panic!(
                            "case {} emitted next_state outside the ToolServiceHealthState owner vocabulary: {error}",
                            case.name
                        )
                    });
            assert_eq!(
                parsed.project().as_str(),
                case.rust_projection
                    .as_deref()
                    .expect("projection exists whenever the model survives (checked above)",),
                "case {}: Lean rust_projection drifted from the owner's operator projection",
                case.name
            );
        }
    }
}
