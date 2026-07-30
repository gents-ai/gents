use super::*;

pub(super) fn generated_backend_health_cases_pin_threshold_and_veto_shape() {
    let cases = lean_backend_health_cases();
    assert!(
        !cases.is_empty(),
        "Lean must emit backend health transition cases"
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

    for case in cases {
        assert!(
            case.threshold_k >= 1,
            "case {} must carry a positive failure threshold",
            case.name
        );
        match case.event.as_str() {
            "probeSuccess" => {
                assert_eq!(
                    (case.next_state.as_str(), case.next_count),
                    ("healthy", 0),
                    "case {}: one success must promote to healthy with a clean counter",
                    case.name
                );
            }
            "probeFail" => {
                assert_eq!(
                    case.next_count,
                    case.start_count + 1,
                    "case {}: probeFail increments the consecutive-failure counter by 1",
                    case.name
                );
                assert_eq!(
                    case.next_state == "unhealthy",
                    case.next_count >= case.threshold_k,
                    "case {}: demotion happens exactly at the K threshold",
                    case.name
                );
            }
            other => panic!("case {} carries unknown event {other:?}", case.name),
        }
        assert_eq!(
            case.blocks_routing,
            case.next_state == "unhealthy",
            "case {}: the routing veto fires exactly on unhealthy",
            case.name
        );
    }
}
