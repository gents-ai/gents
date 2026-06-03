use super::*;
use std::sync::Arc;

const RECOVERY_CREATED_AT: &str = "2026-03-23T00:00:00Z";

pub(super) async fn generated_recovery_sweep_cases_drive_startup_recovery_contract() {
    let cases = lean_recovery_sweep_cases();
    assert_eq!(
        cases.len(),
        19,
        "Lean should emit one row per registered recovery predicate witness"
    );

    let expected_sweep_ids = [
        "request_lifecycle_recover_all_requests",
        "request_lifecycle_recover_all_streaming_responses",
        "tool_call_lifecycle_recover_all_running_calls",
        "tool_call_lifecycle_recover_detached_bridge_rows",
        "inference_call_recover_all_stale_calls",
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    let actual_sweep_ids = cases
        .iter()
        .map(|case| case.sweep_id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual_sweep_ids, expected_sweep_ids,
        "Lean recovery sweep registry drifted"
    );

    for case in cases {
        assert_recovery_case_metadata(case);
        drive_recovery_sweep_case(case).await;
    }
}

pub(super) fn generated_recovery_equivalence_cases_pin_uninterrupted_convergence_contract() {
    let sweep_cases = lean_recovery_sweep_cases();
    let equivalence_cases = lean_recovery_equivalence_cases();
    assert_eq!(
        equivalence_cases.len(),
        sweep_cases.len(),
        "Lean must emit one uninterrupted-equivalence witness per recovery sweep case"
    );
    assert_eq!(
        equivalence_cases.len(),
        19,
        "Lean recovery equivalence witness count drifted"
    );

    let sweep_by_name = sweep_cases
        .iter()
        .map(|case| (case.name.as_str(), case))
        .collect::<HashMap<_, _>>();
    let mut seen_sources = BTreeSet::new();
    for case in equivalence_cases {
        let source = sweep_by_name
            .get(case.source_sweep_case.as_str())
            .unwrap_or_else(|| {
                panic!(
                    "recovery equivalence case {} references unknown sweep case {}",
                    case.name, case.source_sweep_case
                )
            });
        assert!(
            seen_sources.insert(case.source_sweep_case.as_str()),
            "duplicate recovery equivalence witness for {}",
            case.source_sweep_case
        );
        assert_eq!(case.sweep_id, source.sweep_id, "sweep id drifted");
        assert_eq!(case.collection, source.collection, "collection drifted");
        assert_eq!(
            case.rust_function, source.rust_function,
            "Rust function drifted"
        );
        assert_eq!(
            case.cadence, "startup",
            "recovery equivalence cadence drifted"
        );
        assert_eq!(case.pre_state, source.pre_state, "pre-state drifted");
        assert_eq!(
            case.recovered_state, source.terminal_state,
            "recovery terminal state drifted"
        );
        assert_eq!(
            case.uninterrupted_state, source.terminal_state,
            "uninterrupted terminal state drifted"
        );
        assert!(
            case.equivalent,
            "recovery case {} must equal the uninterrupted terminalization path",
            case.name
        );
        assert!(
            !case.reexecutes,
            "recovery case {} must not claim tool/request re-execution",
            case.name
        );
        assert!(
            !case.can_hang,
            "recovery case {} must not permit hanging after startup recovery",
            case.name
        );
        assert_eq!(
            case.theorem.as_str(),
            expected_recovery_equivalence_theorem(case.sweep_id.as_str()),
            "wrong concrete Lean equivalence theorem for {}",
            case.name
        );
        assert_eq!(
            case.aggregate_theorem.as_str(),
            "Recovery.RecoveryEquivalence.finite_stale_rows_converge_to_uninterrupted"
        );
    }
    assert_eq!(seen_sources.len(), sweep_cases.len());
}

fn expected_recovery_equivalence_theorem(sweep_id: &str) -> &'static str {
    match sweep_id {
        "request_lifecycle_recover_all_requests" => "Recovery.requestRecover_matches_uninterrupted",
        "request_lifecycle_recover_all_streaming_responses" => {
            "Recovery.responseRecover_matches_uninterrupted"
        }
        "tool_call_lifecycle_recover_all_running_calls" => {
            "Recovery.toolCallRecover_matches_uninterrupted"
        }
        "tool_call_lifecycle_recover_detached_bridge_rows" => {
            "Recovery.detachedBridgeRecover_matches_uninterrupted"
        }
        "inference_call_recover_all_stale_calls" => {
            "Recovery.inferenceCallRecover_matches_uninterrupted"
        }
        other => panic!("unhandled recovery equivalence sweep id {other}"),
    }
}

fn assert_recovery_case_metadata(case: &lean_vocab_test::LeanRecoverySweepCase) {
    assert_eq!(case.cadence.as_str(), "startup");
    assert_eq!(
        case.implementation_status.as_str(),
        "implemented",
        "recovery case {} must be implemented before the runtime drive can consume it",
        case.name
    );
    assert!(
        case.measure_before > case.measure_after,
        "recovery case {} must decrease its measure",
        case.name
    );
    assert_eq!(
        case.measure_after, 0,
        "recovery case {} must reach zero measure",
        case.name
    );
    assert_ne!(
        case.terminal_state.as_str(),
        "running",
        "recovery case {} must not leave a stale row running",
        case.name
    );
    assert!(
        !case.deadline_audit_ref.trim().is_empty(),
        "recovery case {} must name its audit reference",
        case.name
    );
}

async fn drive_recovery_sweep_case(case: &lean_vocab_test::LeanRecoverySweepCase) {
    match case.collection.as_str() {
        "AgentRequest" => drive_request_recovery_case(case).await,
        "AgentResponse" => drive_response_recovery_case(case).await,
        "AgentToolCall" => drive_tool_call_recovery_case(case).await,
        "InferenceCall" => drive_inference_call_recovery_case(case).await,
        other => panic!("unhandled recovery collection {other} for {}", case.name),
    }
}

async fn drive_request_recovery_case(case: &lean_vocab_test::LeanRecoverySweepCase) {
    let db = test_db(&format!("recovery-sweep-{}", case.name)).await;
    let request_id = format!("{}-request", case.name);
    let session_id = format!("{}-session", case.name);
    let doc_id = create_request(
        &db.node,
        &request_id,
        &session_id,
        "processing",
        RECOVERY_CREATED_AT,
    )
    .await;
    set_request_lifecycle_state(&db.node, &doc_id, case.pre_state.as_str()).await;

    let report = RequestLifecycle::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(
        report.requests_recovered, 1,
        "request recovery case {} should recover one request",
        case.name
    );

    let row = fetch_request_recovery_row(&db.node, &request_id).await;
    assert_eq!(
        row.lifecycle_state.as_str(),
        case.terminal_state.as_str(),
        "request recovery case {} terminal state drifted",
        case.name
    );
}

async fn drive_response_recovery_case(case: &lean_vocab_test::LeanRecoverySweepCase) {
    let db = test_db(&format!("recovery-sweep-{}", case.name)).await;
    let request_id = format!("{}-request", case.name);
    let session_id = format!("{}-session", case.name);
    create_response_with_status(
        &db.node,
        &request_id,
        &request_id,
        &session_id,
        case.pre_state.as_str(),
    )
    .await;

    let report = RequestLifecycle::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(
        report.responses_recovered, 1,
        "response recovery case {} should recover one response",
        case.name
    );

    let row = fetch_response_recovery_row(&db.node, &request_id).await;
    assert_eq!(
        row.status.as_str(),
        case.terminal_state.as_str(),
        "response recovery case {} terminal state drifted",
        case.name
    );
}

async fn drive_tool_call_recovery_case(case: &lean_vocab_test::LeanRecoverySweepCase) {
    let db = test_db(&format!("recovery-sweep-{}", case.name)).await;
    let parent_request_id = format!("{}-parent", case.name);
    let parent_session_id = format!("{}-parent-session", case.name);
    let tool_call_id = format!("{}-tool", case.name);
    seed_tool_parent_and_row(
        db.node.clone(),
        case,
        &parent_request_id,
        &parent_session_id,
        &tool_call_id,
    )
    .await;

    let report = ToolCallLifecycle::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(
        report.tool_calls_recovered, 1,
        "tool recovery case {} should recover one tool call",
        case.name
    );

    let row = fetch_tool_recovery_row(&db.node, &tool_call_id).await;
    assert_eq!(
        row.lifecycle_state.as_deref(),
        Some(case.terminal_state.as_str()),
        "tool recovery case {} terminal state drifted",
        case.name
    );
    assert_eq!(
        row.status.as_deref(),
        Some("completed"),
        "tool recovery case {} must persist completed status with terminal lifecycle_state",
        case.name
    );
    if case.terminal_state == "timedOut" {
        assert_eq!(
            row.tool_failure_class.as_deref(),
            Some("external"),
            "timeout recovery should persist external failure class"
        );
        assert_eq!(
            row.cancel_cause.as_deref(),
            Some("deadline"),
            "timeout recovery should persist cancel_cause=deadline"
        );
    }
    if case.terminal_state == "cancelled" {
        assert_eq!(
            row.cancel_cause.as_deref(),
            Some("interrupted"),
            "cancel recovery should persist cancel_cause=interrupted"
        );
    }
}

async fn drive_inference_call_recovery_case(case: &lean_vocab_test::LeanRecoverySweepCase) {
    let db = test_db(&format!("recovery-sweep-{}", case.name)).await;
    let request_id = format!("{}-request", case.name);
    let session_id = format!("{}-session", case.name);
    let parent_doc_id = create_request(
        &db.node,
        &request_id,
        &session_id,
        "processing",
        RECOVERY_CREATED_AT,
    )
    .await;
    match case.name.as_str() {
        "inference_interrupted_parent_to_cancelled" => {
            set_request_status_and_lifecycle(
                &db.node,
                &parent_doc_id,
                "interrupted",
                "interrupted",
            )
            .await;
        }
        "inference_queued_stale_to_cancelled" | "inference_running_stale_to_failed" => {
            set_request_status_and_lifecycle(&db.node, &parent_doc_id, "completed", "completed")
                .await;
        }
        other => panic!("unhandled inference recovery case {other}"),
    }
    insert_inference_call(&db.node, &request_id, case.pre_state.as_str()).await;

    let report = InferenceCall::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(
        report.calls_recovered, 1,
        "inference recovery case {} should recover one call",
        case.name
    );

    let row = fetch_inference_recovery_row(&db.node, &request_id).await;
    assert_eq!(
        row.call_state.as_str(),
        case.terminal_state.as_str(),
        "inference recovery case {} terminal state drifted",
        case.name
    );
    let terminal_row = InferenceCallSlotRow::new(BACKEND_ID, row.call_state.as_str());
    assert_eq!(slot_contribution(terminal_row, BACKEND_ID), 0);
    assert_eq!(
        reconstructed_running_slot_count([terminal_row], BACKEND_ID),
        0,
        "terminal InferenceCall recovery case {} must reconstruct zero running slots",
        case.name
    );
}

async fn seed_tool_parent_and_row(
    node: Arc<EmbeddedNode>,
    case: &lean_vocab_test::LeanRecoverySweepCase,
    parent_request_id: &str,
    parent_session_id: &str,
    tool_call_id: &str,
) {
    let parent_doc_id = create_request(
        &node,
        parent_request_id,
        parent_session_id,
        "processing",
        RECOVERY_CREATED_AT,
    )
    .await;
    let future_deadline = chrono::Utc::now() + chrono::Duration::minutes(5);
    let past_deadline = chrono::Utc::now() - chrono::Duration::seconds(5);
    let mut lifecycle = match case.name.as_str() {
        "tool_backgrounded_running_live_parent_to_cancelled" => {
            ToolCallLifecycle::new_background_tool(
                node.clone(),
                parent_request_id.to_string(),
                parent_session_id.to_string(),
                tool_call_id.to_string(),
                1,
                "background_tool".to_string(),
                "{}".to_string(),
                future_deadline,
            )
        }
        "tool_running_child_completed_to_completed"
        | "tool_running_child_failed_to_failed"
        | "tool_running_child_interrupted_to_cancelled" => {
            let child_request_id = format!("{tool_call_id}-child");
            let child_state = match case.name.as_str() {
                "tool_running_child_completed_to_completed" => "completed",
                "tool_running_child_failed_to_failed" => "failed",
                "tool_running_child_interrupted_to_cancelled" => "interrupted",
                _ => unreachable!(),
            };
            seed_child_request(&node, &child_request_id, child_state).await;
            ToolCallLifecycle::new_subagent(
                node.clone(),
                parent_request_id.to_string(),
                parent_session_id.to_string(),
                tool_call_id.to_string(),
                1,
                "spawn_subagent".to_string(),
                "{}".to_string(),
                future_deadline,
                AwaitMode::Foreground,
                CancelPolicy::Cascade,
                child_request_id,
            )
        }
        "detached_bridge_child_completed_to_completed"
        | "detached_bridge_child_failed_to_failed"
        | "detached_bridge_child_interrupted_to_cancelled"
        | "detached_bridge_terminal_parent_to_failed"
        | "detached_bridge_deadline_exceeded_to_timed_out" => {
            let child_request_id = format!("{tool_call_id}-child");
            let child_state = match case.name.as_str() {
                "detached_bridge_child_completed_to_completed" => "completed",
                "detached_bridge_child_failed_to_failed" => "failed",
                "detached_bridge_child_interrupted_to_cancelled" => "interrupted",
                _ => "processing",
            };
            seed_child_request(&node, &child_request_id, child_state).await;
            if case.name == "detached_bridge_terminal_parent_to_failed" {
                set_request_status_and_lifecycle(&node, &parent_doc_id, "completed", "completed")
                    .await;
            }
            ToolCallLifecycle::new_subagent(
                node.clone(),
                parent_request_id.to_string(),
                parent_session_id.to_string(),
                tool_call_id.to_string(),
                1,
                "spawn_subagent".to_string(),
                "{}".to_string(),
                if case.name == "detached_bridge_deadline_exceeded_to_timed_out" {
                    past_deadline
                } else {
                    future_deadline
                },
                AwaitMode::Background,
                CancelPolicy::Detach,
                child_request_id,
            )
        }
        "tool_running_deadline_exceeded_to_timed_out" => ToolCallLifecycle::new(
            node.clone(),
            parent_request_id.to_string(),
            parent_session_id.to_string(),
            tool_call_id.to_string(),
            1,
            "slow_tool".to_string(),
            "{}".to_string(),
            past_deadline,
        ),
        "tool_running_parent_interrupted_to_cancelled" => {
            set_request_status_and_lifecycle(&node, &parent_doc_id, "interrupted", "interrupted")
                .await;
            ToolCallLifecycle::new(
                node.clone(),
                parent_request_id.to_string(),
                parent_session_id.to_string(),
                tool_call_id.to_string(),
                1,
                "slow_tool".to_string(),
                "{}".to_string(),
                future_deadline,
            )
        }
        "tool_running_terminal_parent_to_failed" => {
            set_request_status_and_lifecycle(&node, &parent_doc_id, "completed", "completed").await;
            ToolCallLifecycle::new(
                node.clone(),
                parent_request_id.to_string(),
                parent_session_id.to_string(),
                tool_call_id.to_string(),
                1,
                "slow_tool".to_string(),
                "{}".to_string(),
                future_deadline,
            )
        }
        "tool_running_unclaimed_cross_deployment_spawn_to_failed" => {
            let child_request_id = format!("{tool_call_id}-remote-child");
            ToolCallLifecycle::new_subagent(
                node.clone(),
                parent_request_id.to_string(),
                parent_session_id.to_string(),
                tool_call_id.to_string(),
                1,
                "spawn_subagent".to_string(),
                "{}".to_string(),
                future_deadline,
                AwaitMode::Background,
                CancelPolicy::Cascade,
                child_request_id,
            )
        }
        other => panic!("unhandled tool recovery case {other}"),
    };
    lifecycle.start_running().await.unwrap();

    if case.name == "tool_running_unclaimed_cross_deployment_spawn_to_failed" {
        set_tool_unclaimed_deadline(&node, tool_call_id, "2020-01-01T00:00:00Z").await;
    }
}

async fn seed_child_request(node: &EmbeddedNode, request_id: &str, lifecycle_state: &str) {
    let session_id = format!("{request_id}-session");
    match lifecycle_state {
        "completed" => {
            create_request(
                node,
                request_id,
                &session_id,
                "completed",
                RECOVERY_CREATED_AT,
            )
            .await;
            create_response_with_content_and_status(
                node,
                request_id,
                request_id,
                &session_id,
                "child final answer",
                "complete",
            )
            .await;
        }
        "failed" => {
            create_request(node, request_id, &session_id, "error", RECOVERY_CREATED_AT).await;
        }
        "interrupted" => {
            let doc_id = create_request(
                node,
                request_id,
                &session_id,
                "processing",
                RECOVERY_CREATED_AT,
            )
            .await;
            set_request_status_and_lifecycle(node, &doc_id, "interrupted", "interrupted").await;
        }
        "processing" => {
            create_request(
                node,
                request_id,
                &session_id,
                "processing",
                RECOVERY_CREATED_AT,
            )
            .await;
        }
        other => panic!("unsupported child lifecycle state {other}"),
    };
}

async fn set_request_status_and_lifecycle(
    node: &EmbeddedNode,
    doc_id: &str,
    status: &str,
    lifecycle_state: &str,
) {
    let doc_id = escape_graphql_string(doc_id);
    let status = escape_graphql_string(status);
    let lifecycle_state = escape_graphql_string(lifecycle_state);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                input: {{ status: "{status}", lifecycle_state: "{lifecycle_state}" }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "set request status/lifecycle failed: {:?}",
        resp.errors
    );
}

async fn set_tool_unclaimed_deadline(node: &EmbeddedNode, tool_call_id: &str, at: &str) {
    #[derive(Debug, Deserialize)]
    struct ToolDateTimeRow {
        started_at: Option<String>,
        deadline_at: Option<String>,
    }

    let escaped_tool_call_id = escape_graphql_string(tool_call_id);
    let read_query = format!(
        r#"{{
            AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{escaped_tool_call_id}" }} }}, limit: 1) {{
                started_at
                deadline_at
            }}
        }}"#
    );
    let row: ToolDateTimeRow = first_row(&node.execute(&read_query).await, "AgentToolCall");
    let started_at = datetime_update_field("started_at", row.started_at.as_deref());
    let deadline_at = datetime_update_field("deadline_at", row.deadline_at.as_deref());
    let at = escape_graphql_string(at);
    let mutation = format!(
        r#"mutation {{
            update_AgentToolCall(
                filter: {{ tool_call_id: {{ _eq: "{escaped_tool_call_id}" }} }},
                input: {{ unclaimed_deadline_at: "{at}"{started_at}{deadline_at} }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "set tool unclaimed deadline failed: {:?}",
        resp.errors
    );
}

fn datetime_update_field(field: &str, value: Option<&str>) -> String {
    value
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!(r#", {field}: "{}""#, escape_graphql_string(value)))
        .unwrap_or_default()
}

async fn insert_inference_call(node: &EmbeddedNode, request_id: &str, call_state: &str) {
    let call_id = format!("{request_id}-call");
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            add_InferenceCall(input: {{
                call_id: "{call_id}",
                runtime_instance_id: "runtime-recovery-test",
                request_id: "{request_id}",
                call_seq: 1,
                backend_id: "{BACKEND_ID}",
                behavior_id: "{AGENT_NAME}",
                agent_did: "{AGENT_DID}",
                call_kind: "inference",
                attempt: 1,
                call_state: "{call_state}",
                queued_at: "{now}",
                started_at: "{now}",
                priority: 0,
                queue_depth_at_enqueue: 0,
                controller_generation: 0,
                backend_config_fingerprint: "test"
            }}) {{ _docID }}
        }}"#,
        call_id = escape_graphql_string(&call_id),
        request_id = escape_graphql_string(request_id),
        call_state = escape_graphql_string(call_state),
        now = escape_graphql_string(&now),
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "insert inference call failed: {:?}",
        resp.errors
    );
}

#[derive(Debug, Deserialize)]
struct RequestRecoveryRow {
    lifecycle_state: String,
}

async fn fetch_request_recovery_row(node: &EmbeddedNode, request_id: &str) -> RequestRecoveryRow {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{
                lifecycle_state
            }}
        }}"#
    );
    first_row(&node.execute(&query).await, "AgentRequest")
}

#[derive(Debug, Deserialize)]
struct ResponseRecoveryRow {
    status: String,
}

async fn fetch_response_recovery_row(node: &EmbeddedNode, request_id: &str) -> ResponseRecoveryRow {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentResponse(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{
                status
            }}
        }}"#
    );
    first_row(&node.execute(&query).await, "AgentResponse")
}

#[derive(Debug, Deserialize)]
struct ToolRecoveryRow {
    status: Option<String>,
    lifecycle_state: Option<String>,
    tool_failure_class: Option<String>,
    cancel_cause: Option<String>,
}

async fn fetch_tool_recovery_row(node: &EmbeddedNode, tool_call_id: &str) -> ToolRecoveryRow {
    let tool_call_id = escape_graphql_string(tool_call_id);
    let query = format!(
        r#"{{
            AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{tool_call_id}" }} }}, limit: 1) {{
                status
                lifecycle_state
                tool_failure_class
                cancel_cause
            }}
        }}"#
    );
    first_row(&node.execute(&query).await, "AgentToolCall")
}

#[derive(Debug, Deserialize)]
struct InferenceRecoveryRow {
    call_state: String,
}

async fn fetch_inference_recovery_row(
    node: &EmbeddedNode,
    request_id: &str,
) -> InferenceRecoveryRow {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            InferenceCall(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{
                call_state
            }}
        }}"#
    );
    first_row(&node.execute(&query).await, "InferenceCall")
}
