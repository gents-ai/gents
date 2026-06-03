use super::*;

use defra_agent::background_completion::{
    project_background_subagent_completion, BackgroundCompletionOutcome,
};
use defra_agent::tool_call_lifecycle::{
    create_subagent_request_with_request_id, AwaitMode, CancelPolicy, ToolCallLifecycle,
};
use defra_agent::{AgentBehaviorDocument, ToolSelectionDocument};

pub(super) fn generated_tool_execution_cases_cover_preflight_and_retry_contracts() {
    let unreachable =
        lean_tool_preflight_case("preflight_unreachable_valid_blocks_serviceUnavailable");
    assert_eq!(unreachable.decision, "block");
    assert_eq!(
        unreachable.failure_class.as_deref(),
        Some("serviceUnavailable")
    );

    let invalid = lean_tool_preflight_case("preflight_healthy_invalid_blocks_argumentInvalid");
    assert_eq!(invalid.decision, "block");
    assert_eq!(invalid.failure_class.as_deref(), Some("argumentInvalid"));

    for name in [
        "preflight_healthy_valid_dispatch",
        "preflight_stale_valid_dispatch",
    ] {
        let case = lean_tool_preflight_case(name);
        assert_eq!(case.decision, "dispatch", "{name}");
        assert_eq!(case.failure_class, None, "{name}");
    }

    let safe_read = lean_tool_retry_case("retry_mcpListTools_unknown_transport_retrySafeRead");
    assert_eq!(safe_read.disposition, "retrySafeRead");

    let idempotent =
        lean_tool_retry_case("retry_mcpCall_idempotent_transport_retryIdempotentToolCall");
    assert_eq!(idempotent.disposition, "retryIdempotentToolCall");

    for name in [
        "retry_mcpCall_unknown_transport_doNotRetry",
        "retry_mcpCall_nonIdempotent_transport_doNotRetry",
        "retry_nativeCommand_idempotent_transport_doNotRetry",
    ] {
        let case = lean_tool_retry_case(name);
        assert_eq!(case.disposition, "doNotRetry", "{name}");
    }
}

fn slot_rows_from_contract<'a>(
    backend_ids: &'a [String],
    row_states: &'a [String],
) -> impl Iterator<Item = InferenceCallSlotRow<'a>> {
    backend_ids
        .iter()
        .zip(row_states)
        .map(|(backend_id, state)| InferenceCallSlotRow::new(backend_id.as_str(), state.as_str()))
}

pub(super) fn generated_slot_accounting_cases_pin_inference_and_fleet_contracts() {
    for case in &lean_contract_snapshot().inference_slot_accounting_cases {
        assert_eq!(
            case.row_backend_ids.len(),
            case.row_states.len(),
            "Inference slot case {} emitted mismatched row arrays",
            case.name
        );
        if case.row_states.len() == 1 {
            let row = InferenceCallSlotRow::new(
                case.row_backend_ids[0].as_str(),
                case.row_states[0].as_str(),
            );
            assert_eq!(
                slot_contribution(row, &case.backend_id),
                case.expected_contribution,
                "Inference slot case {} drifted from Rust slot contribution",
                case.name
            );
        }
        let reconstructed = reconstructed_running_slot_count(
            slot_rows_from_contract(&case.row_backend_ids, &case.row_states),
            &case.backend_id,
        );
        assert_eq!(
            reconstructed, case.reconstructed_running_count,
            "Inference slot case {} drifted from Rust admission reconstruction",
            case.name
        );
    }

    let queued = lean_inference_slot_accounting_case("queued_contributes_zero");
    assert_eq!(queued.property.as_str(), "state_contribution");
    assert_eq!(queued.pre_state.as_str(), "queued");
    assert_eq!(queued.contribution, 0);
    assert_eq!(queued.reconstructed_running_count, 0);

    let running = lean_inference_slot_accounting_case("running_contributes_one");
    assert_eq!(running.pre_state.as_str(), "running");
    assert_eq!(running.contribution, 1);
    assert_eq!(running.expected_contribution, 1);

    for name in [
        "cancelled_terminal_contributes_zero",
        "completed_terminal_contributes_zero",
        "failed_terminal_contributes_zero",
    ] {
        let case = lean_inference_slot_accounting_case(name);
        assert_eq!(case.property.as_str(), "state_contribution");
        assert_eq!(case.contribution, 0, "{name}");
        assert_eq!(case.reconstructed_running_count, 0, "{name}");
    }

    for name in [
        "cancelled_releases_slot",
        "completed_releases_slot",
        "failed_releases_slot",
    ] {
        let case = lean_inference_slot_accounting_case(name);
        assert_eq!(case.property.as_str(), "terminal_release", "{name}");
        assert_eq!(case.pre_state.as_str(), "running", "{name}");
        assert_eq!(case.pre_contribution, 1, "{name}");
        assert_eq!(case.post_contribution, 0, "{name}");
        assert!(case.released_slot, "{name}");
    }

    for name in [
        "permit_drop_failed_terminalization_not_counted",
        "permit_drop_cancelled_terminalization_not_counted",
    ] {
        let case = lean_inference_slot_accounting_case(name);
        assert_eq!(
            case.property.as_str(),
            "permit_drop_terminalization",
            "{name}"
        );
        assert!(case.permit_drop_terminalization, "{name}");
        assert_eq!(case.post_contribution, 0, "{name}");
    }

    let bounded = lean_inference_slot_accounting_case(
        "reconstructed_running_count_bounded_by_max_concurrent",
    );
    assert_eq!(bounded.reconstructed_running_count, 1);
    assert_eq!(bounded.max_concurrent, 1);
    assert!(bounded.bounded_by_max_concurrent);
    assert_eq!(
        bounded.row_states,
        vec![
            "running".to_string(),
            "queued".to_string(),
            "completed".to_string(),
            "running".to_string()
        ]
    );

    let fleet_ledger = lean_contract_snapshot()
        .coverage_ledger
        .iter()
        .find(|entry| entry.category == "fleet_cases" && entry.domain == "FleetSlotAccounting")
        .expect("FleetSlotAccounting coverage ledger entry must be emitted");
    assert_eq!(
        fleet_ledger.accepted_boundary.as_str(),
        "boundary.fleet-slot-accounting.derived-view",
        "FleetSlotAccounting must be classified as a derived boundary, not a persisted aggregate"
    );

    for case in &lean_contract_snapshot().fleet_slot_accounting_cases {
        assert_eq!(
            case.row_backend_ids.len(),
            case.row_states.len(),
            "Fleet slot case {} emitted mismatched projection row arrays",
            case.name
        );
        if case.row_states.len() == 1 {
            if case.admission_state == "released" {
                let expected_terminal_state = match case.request_state.as_str() {
                    "completed" => "completed",
                    "failed" => "failed",
                    "interrupted" | "superseded" | "dead" => "cancelled",
                    other => panic!(
                        "Fleet slot released case {} has non-terminal request_state={other}",
                        case.name
                    ),
                };
                assert_eq!(
                    case.row_states[0].as_str(),
                    expected_terminal_state,
                    "Fleet slot released case {} projected the wrong terminal InferenceCall state",
                    case.name
                );
            }
            let row = InferenceCallSlotRow::new(
                case.row_backend_ids[0].as_str(),
                case.row_states[0].as_str(),
            );
            assert_eq!(
                slot_contribution(row, &case.backend_id),
                case.expected_contribution,
                "Fleet slot case {} drifted from Rust slot contribution",
                case.name
            );
        }
        let reconstructed = reconstructed_running_slot_count(
            slot_rows_from_contract(&case.row_backend_ids, &case.row_states),
            &case.backend_id,
        );
        assert_eq!(
            reconstructed, case.reconstructed_running_count,
            "Fleet slot case {} drifted from Rust admission reconstruction",
            case.name
        );
        assert_eq!(
            reconstructed, case.slot_count,
            "Fleet slot case {} must be a derived projection over admission reconstruction",
            case.name
        );
        assert_eq!(
            case.scheduler_running, case.slot_count,
            "Fleet slot case {} must keep aggregate running reconstructed from slot count",
            case.name
        );
        assert_eq!(
            case.contribution, case.expected_contribution,
            "Fleet slot case {} must compute its expected contribution",
            case.name
        );
        assert_eq!(
            case.bounded_by_max_concurrent,
            case.slot_count <= case.max_concurrent,
            "Fleet slot case {} must compute its max_concurrent bound",
            case.name
        );
        assert!(
            case.aggregate_reconstructed_not_persisted,
            "Fleet slot case {} must preserve reconstructed-not-persisted policy",
            case.name
        );
    }

    let waiting = lean_fleet_slot_accounting_case("fleet_waiting_contributes_zero");
    assert_eq!(waiting.admission_state.as_str(), "waiting");
    assert_eq!(waiting.contribution, 0);

    let acquired = lean_fleet_slot_accounting_case("fleet_acquired_contributes_one");
    assert_eq!(acquired.admission_state.as_str(), "acquired");
    assert_eq!(acquired.contribution, 1);

    let executing = lean_fleet_slot_accounting_case("fleet_executing_contributes_one");
    assert_eq!(executing.admission_state.as_str(), "executing");
    assert_eq!(executing.contribution, 1);

    let released = lean_fleet_slot_accounting_case("fleet_released_terminal_contributes_zero");
    assert_eq!(released.request_state.as_str(), "completed");
    assert_eq!(released.admission_state.as_str(), "released");
    assert_eq!(released.contribution, 0);

    let fleet_bound = lean_fleet_slot_accounting_case(
        "fleet_reconstructed_running_count_bounded_by_max_concurrent",
    );
    assert_eq!(fleet_bound.slot_count, fleet_bound.scheduler_running);
    assert_eq!(fleet_bound.slot_count, 2);
    assert_eq!(fleet_bound.reconstructed_running_count, 2);
    assert_eq!(
        fleet_bound.row_states,
        vec![
            "running".to_string(),
            "running".to_string(),
            "queued".to_string(),
            "completed".to_string()
        ]
    );
    assert_eq!(fleet_bound.max_concurrent, 2);
    assert!(fleet_bound.bounded_by_max_concurrent);
}

pub(super) async fn generated_queue_deadline_cases_pin_r4a_contract_rows() {
    let cases = lean_queue_deadline_cases();
    assert_eq!(cases.len(), 5);

    let emitted_names = cases
        .iter()
        .map(|case| case.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        emitted_names,
        [
            "active_request_blocks_later_same_session_claim",
            "terminal_active_allows_next_pending_same_session_claim",
            "background_completion_session_coalesces_one_pending_wakeup",
            "cancel_drains_automated_wakeups_preserves_user_pending",
            "claim_preserves_explicit_deadline",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>()
    );

    for case in cases {
        assert_eq!(case.session_id, 900, "{}", case.name);
        assert!(
            case.superseded_request_ids.is_empty(),
            "{} must be a queue/deadline contract, not a supersession contract",
            case.name
        );
    }

    let blocked = lean_queue_deadline_case("active_request_blocks_later_same_session_claim");
    assert_eq!(blocked.group, "queue_admission");
    assert_eq!(blocked.action, "claimNext");
    assert!(!blocked.legal);
    assert!(blocked.blocked_by_active);
    assert_eq!(blocked.pre_active_request_id, Some(100));
    assert_eq!(blocked.post_active_request_id, Some(100));
    assert_eq!(blocked.pre_pending_request_ids, vec![101]);
    assert_eq!(blocked.post_pending_request_ids, vec![101]);
    assert_eq!(blocked.claimed_request_id, None);
    assert!(blocked.post_terminal_request_ids.is_empty());

    let terminal =
        lean_queue_deadline_case("terminal_active_allows_next_pending_same_session_claim");
    assert_eq!(terminal.group, "queue_admission");
    assert_eq!(terminal.action, "finishActive_then_claimNext");
    assert!(terminal.legal);
    assert!(!terminal.blocked_by_active);
    assert_eq!(terminal.pre_active_request_id, Some(100));
    assert_eq!(terminal.pre_pending_request_ids, vec![101]);
    assert_eq!(terminal.post_active_request_id, Some(101));
    assert_eq!(terminal.claimed_request_id, Some(101));
    assert!(terminal.post_pending_request_ids.is_empty());
    assert_eq!(terminal.post_terminal_request_ids, vec![100]);

    let coalesced =
        lean_queue_deadline_case("background_completion_session_coalesces_one_pending_wakeup");
    assert_eq!(coalesced.group, "queue_coalesce");
    assert_eq!(coalesced.action, "coalescePending_twice");
    assert!(coalesced.legal);
    assert_eq!(
        coalesced.queue_key.as_deref(),
        Some("background_completion:900")
    );
    assert!(coalesced.pre_pending_request_ids.is_empty());
    assert_eq!(coalesced.post_pending_request_ids, vec![201]);
    assert_eq!(coalesced.post_coalesced_pending_count, 1);
    assert!(coalesced.post_terminal_request_ids.is_empty());

    let cancel = lean_queue_deadline_case("cancel_drains_automated_wakeups_preserves_user_pending");
    assert_eq!(cancel.group, "queue_cancel");
    assert_eq!(cancel.action, "drainAutomated");
    assert!(cancel.legal);
    assert_eq!(
        cancel.queue_key.as_deref(),
        Some("background_completion:900")
    );
    assert_eq!(cancel.pre_pending_request_ids, vec![301, 302]);
    assert_eq!(cancel.post_pending_request_ids, vec![302]);
    assert_eq!(cancel.automated_drained_request_ids, vec![301]);
    assert_eq!(cancel.preserved_user_pending_request_ids, vec![302]);
    assert_eq!(cancel.post_terminal_request_ids, vec![301]);
    assert_eq!(cancel.post_coalesced_pending_count, 0);

    let deadline = lean_queue_deadline_case("claim_preserves_explicit_deadline");
    assert_eq!(deadline.group, "claim_deadline");
    assert_eq!(deadline.action, "claim");
    assert!(deadline.legal);
    assert_eq!(deadline.claimed_request_id, Some(401));
    assert_eq!(deadline.pre_request_deadline, Some(50));
    assert_eq!(deadline.synthesized_claim_deadline, Some(51));
    assert_eq!(deadline.post_deadline, Some(50));
    assert!(
        deadline.post_deadline < deadline.synthesized_claim_deadline,
        "explicit request deadline should remain tighter than the synthesized claim deadline"
    );
    assert!(deadline.explicit_deadline_preserved);

    for case in cases {
        drive_queue_deadline_case(case).await;
    }
}

async fn drive_queue_deadline_case(case: &lean_vocab_test::LeanQueueDeadlineConformanceCase) {
    match case.name.as_str() {
        "active_request_blocks_later_same_session_claim" => {
            drive_active_request_blocks_later_same_session_claim(case).await;
        }
        "terminal_active_allows_next_pending_same_session_claim" => {
            drive_terminal_active_allows_next_pending_same_session_claim(case).await;
        }
        "background_completion_session_coalesces_one_pending_wakeup" => {
            drive_background_completion_session_coalesces_one_pending_wakeup(case).await;
        }
        "cancel_drains_automated_wakeups_preserves_user_pending" => {
            drive_cancel_drains_automated_wakeups_preserves_user_pending(case).await;
        }
        "claim_preserves_explicit_deadline" => {
            drive_claim_preserves_explicit_deadline(case).await;
        }
        other => panic!("unhandled queue/deadline conformance case {other}"),
    }
}

#[derive(Debug, Clone, Deserialize)]
struct QueueRuntimeRow {
    request_id: String,
    status: String,
    lifecycle_state: Option<String>,
    metadata: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QueueRuntimeSnapshot {
    active_request_id: Option<usize>,
    pending_request_ids: Vec<usize>,
    terminal_request_ids: Vec<usize>,
    coalesced_pending_count: usize,
}

#[derive(Debug, Clone, Deserialize)]
struct DeadlineRuntimeRow {
    status: String,
    lifecycle_state: Option<String>,
    deadline: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SessionIdRow {
    session_id: String,
}

fn symbolic_request_id(
    runtime_request_id: &str,
    generated_ids: &std::collections::BTreeMap<String, usize>,
) -> Option<usize> {
    runtime_request_id
        .parse::<usize>()
        .ok()
        .or_else(|| generated_ids.get(runtime_request_id).copied())
}

fn row_is_pending(row: &QueueRuntimeRow) -> bool {
    row.status == "pending" && row.lifecycle_state.as_deref() == Some("pending")
}

fn row_is_active(row: &QueueRuntimeRow) -> bool {
    row.status == "processing"
        && matches!(
            row.lifecycle_state.as_deref(),
            Some("claimed" | "processing")
        )
}

fn row_is_terminal(row: &QueueRuntimeRow) -> bool {
    matches!(
        row.lifecycle_state.as_deref(),
        Some("completed" | "failed" | "superseded" | "dead" | "interrupted")
    ) || matches!(
        row.status.as_str(),
        "completed" | "error" | "superseded" | "dead" | "interrupted"
    )
}

fn row_matches_coalesced_key(row: &QueueRuntimeRow, queue_key: Option<&str>) -> bool {
    let Some(queue_key) = queue_key else {
        return false;
    };
    if !row_is_pending(row) {
        return false;
    }
    let Some(metadata) = row.metadata.as_deref() else {
        return false;
    };
    let Ok(metadata) = serde_json::from_str::<serde_json::Value>(metadata) else {
        return false;
    };
    let Some(queue) = metadata.get("queue") else {
        return false;
    };
    queue.get("source").and_then(serde_json::Value::as_str) == Some("background_completion")
        && queue.get("policy").and_then(serde_json::Value::as_str) == Some("coalesce")
        && queue.get("key").and_then(serde_json::Value::as_str) == Some(queue_key)
}

async fn fetch_queue_runtime_snapshot(
    node: &EmbeddedNode,
    session_id: &str,
    queue_key: Option<&str>,
    generated_ids: &std::collections::BTreeMap<String, usize>,
) -> QueueRuntimeSnapshot {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                order: [{{ created_at: ASC }}, {{ request_id: ASC }}]
            ) {{
                request_id
                status
                lifecycle_state
                metadata
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "queue snapshot query failed: {:?}",
        response.errors
    );
    let rows: Vec<QueueRuntimeRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();

    let active_request_ids = rows
        .iter()
        .filter(|row| row_is_active(row))
        .filter_map(|row| symbolic_request_id(&row.request_id, generated_ids))
        .collect::<Vec<_>>();
    assert!(
        active_request_ids.len() <= 1,
        "queue snapshot should expose at most one active request: {active_request_ids:?}"
    );

    let pending_request_ids = rows
        .iter()
        .filter(|row| row_is_pending(row))
        .filter_map(|row| symbolic_request_id(&row.request_id, generated_ids))
        .collect::<Vec<_>>();
    let terminal_request_ids = rows
        .iter()
        .filter(|row| row_is_terminal(row))
        .filter_map(|row| symbolic_request_id(&row.request_id, generated_ids))
        .collect::<Vec<_>>();
    let coalesced_pending_count = rows
        .iter()
        .filter(|row| row_matches_coalesced_key(row, queue_key))
        .count();

    QueueRuntimeSnapshot {
        active_request_id: active_request_ids.first().copied(),
        pending_request_ids,
        terminal_request_ids,
        coalesced_pending_count,
    }
}

fn assert_pre_queue_snapshot(
    case: &lean_vocab_test::LeanQueueDeadlineConformanceCase,
    snapshot: &QueueRuntimeSnapshot,
) {
    assert_eq!(
        snapshot.active_request_id, case.pre_active_request_id,
        "{} pre active request drifted",
        case.name
    );
    assert_eq!(
        snapshot.pending_request_ids, case.pre_pending_request_ids,
        "{} pre pending queue drifted",
        case.name
    );
}

fn assert_post_queue_snapshot(
    case: &lean_vocab_test::LeanQueueDeadlineConformanceCase,
    snapshot: &QueueRuntimeSnapshot,
) {
    assert_eq!(
        snapshot.active_request_id, case.post_active_request_id,
        "{} post active request drifted",
        case.name
    );
    assert_eq!(
        snapshot.pending_request_ids, case.post_pending_request_ids,
        "{} post pending queue drifted",
        case.name
    );
    assert_eq!(
        snapshot.terminal_request_ids, case.post_terminal_request_ids,
        "{} terminalized requests drifted",
        case.name
    );
    assert_eq!(
        snapshot.coalesced_pending_count, case.post_coalesced_pending_count,
        "{} coalesced pending count drifted",
        case.name
    );
}

fn request_from_parts(
    doc_id: String,
    request_id: usize,
    session_id: &str,
    created_at: &str,
    deadline: Option<String>,
) -> defra_agent::AgentRequest {
    let mut request = build_request(
        doc_id,
        request_id.to_string(),
        session_id.to_string(),
        created_at.to_string(),
    );
    request.deadline = deadline;
    request
}

fn lifecycle_for(
    node: &std::sync::Arc<EmbeddedNode>,
    request: defra_agent::AgentRequest,
    deadline_duration_secs: u64,
) -> RequestLifecycle {
    RequestLifecycle::new_with_agent_did(
        node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        deadline_duration_secs,
    )
}

async fn drive_active_request_blocks_later_same_session_claim(
    case: &lean_vocab_test::LeanQueueDeadlineConformanceCase,
) {
    let db = test_db("queue-deadline-active-blocks").await;
    let session_id = case.session_id.to_string();
    let active_id = case.pre_active_request_id.expect("active request id");
    let pending_id = case
        .pre_pending_request_ids
        .first()
        .copied()
        .expect("pending request id");
    let active_created_at = "2026-03-23T00:00:10Z";
    let pending_created_at = "2026-03-23T00:00:20Z";

    let active_doc_id = create_request(
        &db.node,
        &active_id.to_string(),
        &session_id,
        "pending",
        active_created_at,
    )
    .await;
    let pending_doc_id = create_request(
        &db.node,
        &pending_id.to_string(),
        &session_id,
        "pending",
        pending_created_at,
    )
    .await;

    let active_request = request_from_parts(
        active_doc_id,
        active_id,
        &session_id,
        active_created_at,
        None,
    );
    let mut active_lifecycle = lifecycle_for(&db.node, active_request, DEADLINE_SECS);
    assert_eq!(
        active_lifecycle.claim().await.unwrap(),
        ClaimOutcome::Claimed
    );

    let generated_ids = std::collections::BTreeMap::new();
    let pre = fetch_queue_runtime_snapshot(&db.node, &session_id, None, &generated_ids).await;
    assert_pre_queue_snapshot(case, &pre);

    let pending_request = request_from_parts(
        pending_doc_id,
        pending_id,
        &session_id,
        pending_created_at,
        None,
    );
    let mut pending_lifecycle = lifecycle_for(&db.node, pending_request, DEADLINE_SECS);
    assert_eq!(
        pending_lifecycle.claim().await.unwrap(),
        ClaimOutcome::Queued
    );
    assert_eq!(case.claimed_request_id, None);

    let post = fetch_queue_runtime_snapshot(&db.node, &session_id, None, &generated_ids).await;
    assert_post_queue_snapshot(case, &post);
}

async fn drive_terminal_active_allows_next_pending_same_session_claim(
    case: &lean_vocab_test::LeanQueueDeadlineConformanceCase,
) {
    let db = test_db("queue-deadline-terminal-allows").await;
    let session_id = case.session_id.to_string();
    let active_id = case.pre_active_request_id.expect("active request id");
    let pending_id = case.claimed_request_id.expect("claimed request id");
    let active_created_at = "2026-03-23T00:00:10Z";
    let pending_created_at = "2026-03-23T00:00:20Z";

    let active_doc_id = create_request(
        &db.node,
        &active_id.to_string(),
        &session_id,
        "pending",
        active_created_at,
    )
    .await;
    let pending_doc_id = create_request(
        &db.node,
        &pending_id.to_string(),
        &session_id,
        "pending",
        pending_created_at,
    )
    .await;

    let active_request = request_from_parts(
        active_doc_id,
        active_id,
        &session_id,
        active_created_at,
        None,
    );
    let mut active_lifecycle = lifecycle_for(&db.node, active_request, DEADLINE_SECS);
    assert_eq!(
        active_lifecycle.claim().await.unwrap(),
        ClaimOutcome::Claimed
    );

    let generated_ids = std::collections::BTreeMap::new();
    let pre = fetch_queue_runtime_snapshot(&db.node, &session_id, None, &generated_ids).await;
    assert_pre_queue_snapshot(case, &pre);

    active_lifecycle.complete().await.unwrap();

    let pending_request = request_from_parts(
        pending_doc_id,
        pending_id,
        &session_id,
        pending_created_at,
        None,
    );
    let mut pending_lifecycle = lifecycle_for(&db.node, pending_request, DEADLINE_SECS);
    assert_eq!(
        pending_lifecycle.claim().await.unwrap(),
        ClaimOutcome::Claimed
    );

    let post = fetch_queue_runtime_snapshot(&db.node, &session_id, None, &generated_ids).await;
    assert_post_queue_snapshot(case, &post);
}

async fn drive_background_completion_session_coalesces_one_pending_wakeup(
    case: &lean_vocab_test::LeanQueueDeadlineConformanceCase,
) {
    let db = test_db("queue-deadline-coalesce").await;
    let session_id = case.session_id.to_string();
    let parent_request_id = "queue-deadline-coalesce-parent";
    install_background_completion_fixture(db.node.as_ref()).await;
    create_queue_request(
        db.node.as_ref(),
        parent_request_id,
        &session_id,
        "completed",
        "2026-03-23T00:00:00Z",
        "interactive",
        None,
        Some(&(chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339()),
    )
    .await;

    let generated_ids = std::collections::BTreeMap::new();
    let pre = fetch_queue_runtime_snapshot(
        &db.node,
        &session_id,
        case.queue_key.as_deref(),
        &generated_ids,
    )
    .await;
    assert_pre_queue_snapshot(case, &pre);

    let (child_a, child_session_a) = create_background_child_bridge(
        &db.node,
        parent_request_id,
        &session_id,
        "queue-deadline-coalesce-a",
        1,
    )
    .await;
    let (child_b, child_session_b) = create_background_child_bridge(
        &db.node,
        parent_request_id,
        &session_id,
        "queue-deadline-coalesce-b",
        2,
    )
    .await;
    persist_child_completion(
        db.node.as_ref(),
        &child_a,
        &child_session_a,
        "child A complete",
    )
    .await;
    persist_child_completion(
        db.node.as_ref(),
        &child_b,
        &child_session_b,
        "child B complete",
    )
    .await;

    let wake_a = projected_wake_request_id(
        project_background_subagent_completion(db.node.clone(), &child_a, AGENT_DID)
            .await
            .unwrap(),
    );
    let wake_b = projected_wake_request_id(
        project_background_subagent_completion(db.node.clone(), &child_b, AGENT_DID)
            .await
            .unwrap(),
    );
    assert_eq!(
        wake_a, wake_b,
        "{} should coalesce both completions into one wake-up",
        case.name
    );

    let mut generated_ids = std::collections::BTreeMap::new();
    generated_ids.insert(
        wake_a,
        *case
            .post_pending_request_ids
            .first()
            .expect("coalesced pending id"),
    );
    let post = fetch_queue_runtime_snapshot(
        &db.node,
        &session_id,
        case.queue_key.as_deref(),
        &generated_ids,
    )
    .await;
    assert_post_queue_snapshot(case, &post);
}

async fn drive_cancel_drains_automated_wakeups_preserves_user_pending(
    case: &lean_vocab_test::LeanQueueDeadlineConformanceCase,
) {
    let db = test_db("queue-deadline-cancel-drain").await;
    let session_id = case.session_id.to_string();
    let parent_request_id = "queue-deadline-cancel-parent";
    create_queue_request(
        db.node.as_ref(),
        parent_request_id,
        &session_id,
        "completed",
        "2026-03-23T00:00:00Z",
        "interactive",
        None,
        None,
    )
    .await;

    let automated_id = case.automated_drained_request_ids[0];
    let user_id = case.preserved_user_pending_request_ids[0];
    create_queue_request(
        db.node.as_ref(),
        &automated_id.to_string(),
        &session_id,
        "pending",
        "2026-03-23T00:00:10Z",
        "scheduled",
        Some(&automated_queue_metadata(
            case.queue_key.as_deref().expect("queue key"),
            parent_request_id,
        )),
        None,
    )
    .await;
    create_queue_request(
        db.node.as_ref(),
        &user_id.to_string(),
        &session_id,
        "pending",
        "2026-03-23T00:00:20Z",
        "scheduled",
        Some(&user_queue_metadata()),
        None,
    )
    .await;

    let generated_ids = std::collections::BTreeMap::new();
    let pre = fetch_queue_runtime_snapshot(
        &db.node,
        &session_id,
        case.queue_key.as_deref(),
        &generated_ids,
    )
    .await;
    assert_pre_queue_snapshot(case, &pre);

    defra_agent::interrupt_request(db.node.as_ref(), parent_request_id)
        .await
        .unwrap();

    let post = fetch_queue_runtime_snapshot(
        &db.node,
        &session_id,
        case.queue_key.as_deref(),
        &generated_ids,
    )
    .await;
    assert_post_queue_snapshot(case, &post);
    assert_eq!(
        post.pending_request_ids, case.preserved_user_pending_request_ids,
        "{} should preserve only the user pending row",
        case.name
    );
}

async fn drive_claim_preserves_explicit_deadline(
    case: &lean_vocab_test::LeanQueueDeadlineConformanceCase,
) {
    let db = test_db("queue-deadline-explicit-deadline").await;
    let session_id = case.session_id.to_string();
    let request_id = case.claimed_request_id.expect("claimed request id");
    let created_at = chrono::Utc::now().to_rfc3339();
    let explicit_deadline_at =
        chrono::Utc::now() + chrono::Duration::seconds(case.pre_request_deadline.unwrap() as i64);
    let explicit_deadline = explicit_deadline_at.to_rfc3339();
    let doc_id = create_queue_request(
        db.node.as_ref(),
        &request_id.to_string(),
        &session_id,
        "pending",
        &created_at,
        "interactive",
        None,
        Some(&explicit_deadline),
    )
    .await;
    let request = request_from_parts(
        doc_id,
        request_id,
        &session_id,
        &created_at,
        Some(explicit_deadline.clone()),
    );

    let before_claim = chrono::Utc::now();
    let mut lifecycle = lifecycle_for(
        &db.node,
        request,
        case.synthesized_claim_deadline.unwrap() as u64,
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);

    let row = fetch_deadline_runtime_row(db.node.as_ref(), request_id).await;
    assert_eq!(row.status, "processing");
    assert_eq!(row.lifecycle_state.as_deref(), Some("claimed"));

    let persisted_deadline = chrono::DateTime::parse_from_rfc3339(&row.deadline).unwrap();
    assert_eq!(
        persisted_deadline,
        chrono::DateTime::parse_from_rfc3339(&explicit_deadline).unwrap(),
        "{} should preserve the request's explicit deadline",
        case.name
    );
    assert!(
        persisted_deadline.with_timezone(&chrono::Utc)
            < before_claim
                + chrono::Duration::seconds(case.synthesized_claim_deadline.unwrap() as i64),
        "{} should keep the tighter explicit deadline instead of the synthesized claim deadline",
        case.name
    );
    assert!(case.explicit_deadline_preserved);
}

#[allow(clippy::too_many_arguments)]
async fn create_queue_request(
    node: &EmbeddedNode,
    request_id: &str,
    session_id: &str,
    status: &str,
    created_at: &str,
    execution_origin: &str,
    metadata: Option<&str>,
    deadline: Option<&str>,
) -> String {
    let lifecycle_state = match status {
        "pending" => "pending",
        "processing" => "processing",
        "completed" => "completed",
        "interrupted" => "interrupted",
        "superseded" => "superseded",
        "dead" => "dead",
        "error" => "failed",
        other => panic!("unsupported queue request status {other}"),
    };
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_created_at = escape_graphql_string(created_at);
    let escaped_execution_origin = escape_graphql_string(execution_origin);
    let metadata_field = metadata
        .map(|metadata| format!(r#", metadata: "{}""#, escape_graphql_string(metadata)))
        .unwrap_or_default();
    let deadline_field = deadline
        .map(|deadline| format!(r#", deadline: "{}""#, escape_graphql_string(deadline)))
        .unwrap_or_default();
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                agent_did: "{AGENT_DID}",
                behavior_id: "{AGENT_NAME}",
                session_id: "{escaped_session_id}",
                retry_parent_request: "",
                retry_root_request: "{escaped_request_id}",
                superseded_by_request: "",
                content: "queue deadline conformance",
                status: "{status}",
                lifecycle_state: "{lifecycle_state}",
                backend_id: "",
                execution_origin: "{escaped_execution_origin}",
                failure_reason: "",
                created_at: "{escaped_created_at}",
                retry_count: 0,
                max_retries: {max_retries},
                subagent_depth: 0{metadata_field}{deadline_field}
            }}) {{ _docID }}
        }}"#,
        max_retries = defra_agent::lifecycle::DEFAULT_REQUEST_MAX_RETRIES,
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create queue request failed: {:?}",
        response.errors
    );

    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                limit: 1
            ) {{ _docID }}
        }}"#
    );
    support::first_row::<support::DocIdRow>(&node.execute(&query).await, "AgentRequest").doc_id
}

fn automated_queue_metadata(queue_key: &str, queued_after_request_id: &str) -> String {
    json!({
        "queue": {
            "source": "background_completion",
            "policy": "coalesce",
            "key": queue_key,
            "queued_after_request_id": queued_after_request_id,
        }
    })
    .to_string()
}

fn user_queue_metadata() -> String {
    json!({
        "queue": {
            "source": "user",
            "policy": "append",
            "key": null,
            "queued_after_request_id": null,
        }
    })
    .to_string()
}

async fn install_background_completion_fixture(node: &EmbeddedNode) {
    const TOOL_SELECTION_ID: &str = "queue-deadline-tools";
    const CHILD_BEHAVIOR_ID: &str = "queue-deadline-child";

    defra_agent::upsert_tool_selection(
        node,
        &ToolSelectionDocument {
            selection_id: TOOL_SELECTION_ID.to_string(),
            agent_did: AGENT_DID.to_string(),
            subagent_targets: Some(vec![CHILD_BEHAVIOR_ID.to_string()]),
            subagent_spawn_enabled: Some(true),
            subagent_background_enabled: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    defra_agent::upsert_agent_behavior(
        node,
        &AgentBehaviorDocument {
            behavior_id: AGENT_NAME.to_string(),
            agent_did: AGENT_DID.to_string(),
            display_name: Some("Queue deadline parent".to_string()),
            description: None,
            summary: None,
            system_prompt: None,
            backend_id: None,
            model_name: None,
            tool_selection_id: Some(TOOL_SELECTION_ID.to_string()),
            inference_profile_id: None,
            compaction_strategy: None,
            compaction_threshold: None,
            enabled: true,
            created_at: Some("2026-03-23T00:00:00Z".to_string()),
        },
    )
    .await
    .unwrap();
    defra_agent::upsert_agent_behavior(
        node,
        &AgentBehaviorDocument {
            behavior_id: CHILD_BEHAVIOR_ID.to_string(),
            agent_did: AGENT_DID.to_string(),
            display_name: Some("Queue deadline child".to_string()),
            description: None,
            summary: None,
            system_prompt: None,
            backend_id: None,
            model_name: None,
            tool_selection_id: None,
            inference_profile_id: None,
            compaction_strategy: None,
            compaction_threshold: None,
            enabled: true,
            created_at: Some("2026-03-23T00:00:01Z".to_string()),
        },
    )
    .await
    .unwrap();
}

async fn create_background_child_bridge(
    node: &std::sync::Arc<EmbeddedNode>,
    parent_request_id: &str,
    parent_session_id: &str,
    tool_call_id: &str,
    message_sequence: u32,
) -> (String, String) {
    const CHILD_BEHAVIOR_ID: &str = "queue-deadline-child";

    let child_request_id = format!("{parent_request_id}-{tool_call_id}-child");
    create_subagent_request_with_request_id(
        node.as_ref(),
        child_request_id.clone(),
        parent_request_id.to_string(),
        tool_call_id.to_string(),
        0,
        AGENT_DID.to_string(),
        CHILD_BEHAVIOR_ID.to_string(),
        format!("prompt for {tool_call_id}"),
        Some(chrono::Utc::now() + chrono::Duration::minutes(4)),
    )
    .await
    .unwrap();
    let child_session_id = child_session_id(node.as_ref(), &child_request_id).await;

    let mut lifecycle = ToolCallLifecycle::new_subagent(
        node.clone(),
        parent_request_id.to_string(),
        parent_session_id.to_string(),
        tool_call_id.to_string(),
        message_sequence,
        "spawn_subagent".to_string(),
        json!({
            "behavior_id": CHILD_BEHAVIOR_ID,
            "prompt": format!("prompt for {tool_call_id}"),
            "await_mode": AwaitMode::Background.as_str(),
        })
        .to_string(),
        chrono::Utc::now() + chrono::Duration::minutes(5),
        AwaitMode::Background,
        CancelPolicy::Cascade,
        child_request_id.clone(),
    );
    lifecycle.start_running().await.unwrap();

    (child_request_id, child_session_id)
}

async fn child_session_id(node: &EmbeddedNode, child_request_id: &str) -> String {
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_child_request_id}" }} }},
                limit: 1
            ) {{ session_id }}
        }}"#
    );
    support::first_row::<SessionIdRow>(&node.execute(&query).await, "AgentRequest").session_id
}

async fn persist_child_completion(
    node: &EmbeddedNode,
    child_request_id: &str,
    child_session_id: &str,
    final_response: &str,
) {
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let update_request = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_child_request_id}" }} }},
                input: {{ status: "completed", lifecycle_state: "completed" }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&update_request).await;
    assert!(
        !response.has_errors(),
        "update child AgentRequest completed failed: {:?}",
        response.errors
    );

    let assistant = Message::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::Text(Text {
            text: final_response.to_string(),
        })),
    };
    let escaped_message = escape_graphql_string(&serde_json::to_string(&assistant).unwrap());
    let escaped_child_session_id = escape_graphql_string(child_session_id);
    let now = chrono::Utc::now().to_rfc3339();
    let create_message = format!(
        r#"mutation {{
            create_AgentMessage(input: {{
                message_key: "{escaped_child_session_id}:1",
                session_id: "{escaped_child_session_id}",
                sequence: 1,
                role: "assistant",
                content: "{escaped_message}",
                timestamp: "{now}"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&create_message).await;
    assert!(
        !response.has_errors(),
        "create child AgentMessage failed: {:?}",
        response.errors
    );

    let create_response = format!(
        r#"mutation {{
            create_AgentResponse(input: {{
                response_key: "{escaped_child_request_id}",
                request_id: "{escaped_child_request_id}",
                agent_did: "{AGENT_DID}",
                behavior_id: "queue-deadline-child",
                session_id: "{escaped_child_session_id}",
                content: "",
                reasoning: "",
                status: "completed",
                error_message: "",
                token_count: 0,
                progress_seq: 0,
                materialized_message_sequence: 1,
                materialized_at: "{now}",
                created_at: "{now}",
                completed_at: "{now}"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&create_response).await;
    assert!(
        !response.has_errors(),
        "create child AgentResponse failed: {:?}",
        response.errors
    );
}

fn projected_wake_request_id(outcome: BackgroundCompletionOutcome) -> String {
    let BackgroundCompletionOutcome::Projected {
        wake_request_id, ..
    } = outcome
    else {
        panic!("expected background completion projection, got {outcome:?}");
    };
    wake_request_id
}

async fn fetch_deadline_runtime_row(node: &EmbeddedNode, request_id: usize) -> DeadlineRuntimeRow {
    let escaped_request_id = escape_graphql_string(&request_id.to_string());
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                limit: 1
            ) {{ status lifecycle_state deadline }}
        }}"#
    );
    support::first_row::<DeadlineRuntimeRow>(&node.execute(&query).await, "AgentRequest")
}

#[test]
fn generated_command_policy_cases_cover_policy_sandbox_and_env_contracts() {
    let forbidden = lean_command_policy_case("forbidden_prefix_wins_over_allowed_prefix_order");
    assert_eq!(forbidden.category, "forbidden_prefix");
    assert_eq!(forbidden.decision, "deny");
    assert_eq!(forbidden.denial_reason.as_deref(), Some("forbiddenPrefix"));
    assert_eq!(
        forbidden.matched_prefix.as_ref(),
        Some(&vec!["git".to_string()])
    );
    let second_forbidden = lean_command_policy_case("forbidden_prefix_second_configured_match");
    assert_eq!(
        second_forbidden.matched_prefix.as_ref(),
        Some(&vec!["git".to_string(), "diff".to_string()])
    );

    let allowed =
        lean_command_policy_case("allowed_prefix_required_precedes_network_and_allowlist");
    assert_eq!(allowed.decision, "deny");
    assert_eq!(
        allowed.denial_reason.as_deref(),
        Some("allowedPrefixRequired")
    );
    assert_eq!(
        allowed.denied_argv.as_ref(),
        Some(&vec!["curl".to_string(), "https://example.com".to_string()])
    );

    let configured =
        lean_command_policy_case("allowed_prefix_authorizes_read_only_diagnostic_command");
    assert_eq!(configured.category, "read_only_configured_prefix");
    assert_eq!(configured.decision, "allow");

    let configured_forbidden =
        lean_command_policy_case("forbidden_prefix_overrides_configured_read_only_diagnostic");
    assert_eq!(configured_forbidden.decision, "deny");
    assert_eq!(
        configured_forbidden.denial_reason.as_deref(),
        Some("forbiddenPrefix")
    );

    let curl = lean_command_policy_case("disabled_network_read_only_curl_denies_before_allowlist");
    assert_eq!(
        curl.denial_reason.as_deref(),
        Some("disabledNetworkCommand")
    );
    assert_eq!(curl.denied_command.as_deref(), Some("curl"));

    let workspace = lean_command_sandbox_case("workspace_write_enforced_selects_macos_seatbelt");
    assert_eq!(workspace.decision, "selected");
    assert_eq!(workspace.sandbox.as_deref(), Some("macos_seatbelt"));

    let unrestricted = lean_command_sandbox_case("unrestricted_selects_unsandboxed_unrestricted");
    assert_eq!(
        unrestricted.sandbox.as_deref(),
        Some("unsandboxed_unrestricted")
    );

    let key = lean_command_env_case("env_key_marker_dropped");
    assert_eq!(key.input_name, "OPENAI_API_KEY");
    assert_eq!(key.expected_output_value, None);

    let pager = lean_command_env_case("env_pager_forced_cat");
    assert_eq!(pager.expected_output_value.as_deref(), Some("cat"));
    let pager_absent = lean_command_env_case("env_pager_absent_still_forced_cat");
    assert_eq!(pager_absent.expected_output_value.as_deref(), Some("cat"));
}
