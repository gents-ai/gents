use super::*;

pub(super) fn generated_mcp_health_cases_pin_threshold_projection_shape() {
    let cases = lean_mcp_health_cases();
    assert!(
        !cases.is_empty(),
        "Lean must emit MCP health transition cases"
    );

    let k1 = cases
        .iter()
        .filter(|case| case.threshold_k == 1)
        .collect::<Vec<_>>();
    assert!(
        !k1.is_empty(),
        "the K=1 projection subset (what the health checker consumes today) must be emitted"
    );
    assert!(
        k1.iter().any(|case| {
            case.start_state == "healthy"
                && case.event == "probeFail"
                && case.next_state.as_deref() == Some("evicted")
        }),
        "H7: at K=1 the first probeFail must collapse healthy -> evicted directly"
    );
    assert!(
        cases.iter().any(|case| case.threshold_k >= 2),
        "K>=2 future rows must stay emitted alongside the K=1 projection"
    );

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
    }
}
