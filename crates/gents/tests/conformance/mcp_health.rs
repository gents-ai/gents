//! MCP health conformance: pins the generated transition-case rows to the real
//! service-health owner in `crates/gents-protocol/src/tool_service_health.rs`.
//!
//! The transition machine itself is fenced with stronger owners and is
//! deliberately NOT re-implemented here: the Lean theorems (`h2` … `h8`,
//! `degraded_count_lt_K`) prove the semantics, and the in-crate
//! `health_checker::tests::generated_mcp_health_cases_match_health_checker_transitions`
//! replays every emitted row through the real `step_service` owner and its
//! `HealthStatus` projection. This conformance pin keeps the row-shape
//! guarantees (threshold coverage including the production default, the H7
//! collapse witness, removal dropping state and count together, eviction being
//! probe-failure driven) and closes the coupling gap: every surviving
//! `next_state` must parse as the persisted `ToolServiceHealthState`
//! vocabulary and project, through the owner's `project()`, to the emitted
//! operator-facing `rust_projection`.

use super::*;

pub(super) fn generated_mcp_health_cases_pin_threshold_projection_shape() {
    let cases = lean_mcp_health_cases();
    assert!(
        !cases.is_empty(),
        "Lean must emit MCP health transition cases"
    );

    // The production health checker default must stay covered by the emitted
    // rows alongside the degenerate thresholds.
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

    // Every persisted Rust state must have a transition witness.
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

        // Coupling through the real owner: a surviving state must be in the
        // persisted ToolServiceHealthState vocabulary and must project to the
        // emitted operator-facing status (Lean `healthProjection`).
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
