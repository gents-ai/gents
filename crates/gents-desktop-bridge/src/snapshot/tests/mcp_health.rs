use gents_protocol::row::ToolServiceHealthStateRow;

use crate::commands::mcp_health::view_from_row;

fn row_from_lean(
    case_name: &str,
    state: &str,
    count: usize,
    threshold_k: usize,
) -> ToolServiceHealthStateRow {
    let status = state;
    ToolServiceHealthStateRow {
        service_id: format!("contract-{}", case_name),
        node_did: Some("did:test:contract-node".to_string()),
        endpoint: Some("127.0.0.1:9201/mcp".to_string()),
        status: Some(status.to_string()),
        tool_count: Some(7),
        failure_count: Some(count as i64),
        k_max: Some(threshold_k as i64),
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

#[test]
fn mcp_health_view_preserves_every_generated_lean_mcp_health_case_transition() {
    gents_lean_vocab::families::contracts_mcp_health_view::replay::<String, String>(|case| {
        let row = row_from_lean(
            &case.name,
            &case.next_state,
            case.next_count as usize,
            case.threshold_k as usize,
        );
        let view = view_from_row(row).unwrap_or_else(|error| {
            panic!(
                "Lean MCP health case {} row must project to a view: {error}",
                case.name
            )
        });
        assert_eq!(
            view.status.as_deref(),
            Some(case.next_state.as_str()),
            "Lean MCP health case {} status must survive view projection",
            case.name,
        );
        assert_eq!(
            view.failure_count,
            Some(case.next_count as i64),
            "Lean MCP health case {} failure_count must survive view projection",
            case.name,
        );
        assert_eq!(
            view.k_max,
            Some(case.threshold_k as i64),
            "view k_max should mirror the row's k_max",
        );
        view.display_state
    });
}
