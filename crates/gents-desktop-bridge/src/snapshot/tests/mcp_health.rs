#[path = "../../../../../crates/gents/src/lean_vocab_test/support.rs"]
mod lean_vocab_test;

use gents_protocol::row::ToolServiceHealthStateRow;
use lean_vocab_test::lean_mcp_health_cases;

use crate::types::MCPServiceHealthView;

/// Translate an internal Lean `HealthState` name to the DefraDB-persisted
/// string vocabulary used by `health_checker::HealthStateInternal::to_defradb`.
/// Identity for every internal state — the persisted vocabulary mirrors
/// `Proofs/MCPHealth/State.lean :: HealthState.toDefraDB` exactly. The
/// public `HealthStatus` collapse (degraded → stale, evicted+reconnecting
/// → unreachable) is a separate projection applied in
/// `lean_state_to_health_status_projection` below.
fn lean_state_to_defradb_status(state: &str) -> &'static str {
    match state {
        "healthy" => "healthy",
        "degraded" => "degraded",
        "evicted" => "evicted",
        "reconnecting" => "reconnecting",
        other => panic!("Lean MCP health case produced unknown state {other:?}"),
    }
}

fn row_from_lean(case_name: &str, state: &str, count: usize) -> ToolServiceHealthStateRow {
    let status = lean_state_to_defradb_status(state);
    ToolServiceHealthStateRow {
        service_id: format!("contract-{}", case_name),
        agent_did: Some("did:test:contract-agent".to_string()),
        endpoint: Some("127.0.0.1:9201/mcp".to_string()),
        status: Some(status.to_string()),
        tool_count: Some(7),
        failure_count: Some(count as i64),
        k_max: Some(3),
        backoff_until: if status == "evicted" {
            Some("2026-04-21T12:00:30Z".to_string())
        } else {
            None
        },
        last_probe_at: Some("2026-04-21T12:00:00Z".to_string()),
        last_seen: Some("2026-04-21T12:00:00Z".to_string()),
        last_error_class: if status == "healthy" {
            None
        } else {
            Some("timeout".to_string())
        },
        last_error_message: if status == "healthy" {
            None
        } else {
            Some("probe timed out".to_string())
        },
        updated_at: Some("2026-04-21T12:00:00Z".to_string()),
    }
}

fn view_from_row(row: &ToolServiceHealthStateRow) -> MCPServiceHealthView {
    MCPServiceHealthView {
        service_id: row.service_id.clone(),
        agent_did: row.agent_did.clone(),
        endpoint: row.endpoint.clone(),
        status: row.status.clone(),
        tool_count: row.tool_count,
        failure_count: row.failure_count,
        k_max: row.k_max,
        backoff_until: row.backoff_until.clone(),
        last_probe_at: row.last_probe_at.clone(),
        last_seen: row.last_seen.clone(),
        last_error_class: row.last_error_class.clone(),
        last_error_message: row.last_error_message.clone(),
        updated_at: row.updated_at.clone(),
    }
}

#[test]
fn mcp_health_view_preserves_every_generated_lean_mcp_health_case_transition() {
    let cases = lean_mcp_health_cases();
    assert!(
        !cases.is_empty(),
        "Lean must emit at least one MCP health case"
    );

    let mut covered_states = std::collections::BTreeSet::<&'static str>::new();
    let mut covered_thresholds = std::collections::BTreeSet::<usize>::new();

    for case in cases {
        let (Some(next_state), Some(next_count)) = (case.next_state.as_deref(), case.next_count)
        else {
            continue;
        };

        let row = row_from_lean(&case.name, next_state, next_count);
        let view = view_from_row(&row);

        let expected_persisted_status = lean_state_to_defradb_status(next_state);
        assert_eq!(
            view.status.as_deref(),
            Some(expected_persisted_status),
            "Lean MCP health case {} status must survive view projection",
            case.name,
        );
        assert_eq!(
            view.failure_count,
            Some(next_count as i64),
            "Lean MCP health case {} failure_count must survive view projection",
            case.name,
        );
        assert_eq!(
            view.k_max,
            Some(3),
            "view k_max should mirror the row's k_max",
        );

        if let Some(projection) = case.rust_projection.as_deref() {
            let view_projection = match view.status.as_deref().unwrap_or("") {
                "healthy" => "healthy",
                "degraded" => "stale",
                "evicted" | "reconnecting" => "unreachable",
                other => panic!("unexpected view status {other:?}"),
            };
            assert_eq!(
                view_projection, projection,
                "Lean MCP health case {} HealthStatus projection must agree with view collapse",
                case.name,
            );
        }

        covered_states.insert(match next_state {
            "healthy" => "healthy",
            "degraded" => "degraded",
            "evicted" => "evicted",
            "reconnecting" => "reconnecting",
            _ => unreachable!(),
        });
        covered_thresholds.insert(case.threshold_k);
    }

    assert!(
        covered_states.contains("healthy"),
        "Lean MCP health cases must drive view through .healthy"
    );
    assert!(
        covered_states.contains("degraded"),
        "Lean MCP health cases must drive view through .degraded"
    );
    assert!(
        covered_states.contains("evicted"),
        "Lean MCP health cases must drive view through .evicted"
    );
    assert!(
        covered_thresholds.contains(&1),
        "Lean MCP health cases must include the K=1 collapse"
    );
    assert!(
        covered_thresholds.iter().any(|k| *k >= 2),
        "Lean MCP health cases must include K≥2 transitions so the bridge view exercises the failure-count flavor of degraded"
    );
}
