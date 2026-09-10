//! Runtime backgrounding, admission, cancellation, dispatch, and status contracts.

use super::*;
use gents::lifecycle::RequestTerminalOutcome;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

const BACKGROUND_THEOREM_PARENT_BEHAVIOR_ID: &str = "r6-background-theorem-parent";
const BACKGROUND_THEOREM_CHILD_BEHAVIOR_ID: &str = "r6-background-theorem-child";

struct PendingTool;

impl ToolDyn for PendingTool {
    fn name(&self) -> String {
        "slow_tool".to_string()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "slow_tool".to_string(),
                description: "test tool".to_string(),
                parameters: json!({"type":"object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(std::future::pending())
    }
}

#[derive(Debug, Deserialize)]
struct BackgroundTheoremToolCallRow {
    await_mode: Option<String>,
    cancel_policy: Option<String>,
    child_request_id: Option<String>,
    lifecycle_state: Option<String>,
    result: Option<String>,
    cancel_cause: Option<String>,
    cancel_cascade_intent_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BackgroundedRow {
    lifecycle_state: Option<String>,
}

fn background_tool_registry(
    tools: Vec<Box<dyn ToolDyn>>,
    allowlist: &[&str],
) -> BackgroundToolRegistry {
    BackgroundToolRegistry::from_tools(
        tools,
        &allowlist
            .iter()
            .map(|name| name.to_string())
            .collect::<Vec<_>>(),
    )
}

fn skip_reason_json(action: ToolCallHookAction) -> Value {
    let ToolCallHookAction::Skip { reason } = action else {
        panic!("expected Skip action, got {action:?}");
    };
    serde_json::from_str(&reason).expect("skip reason should be JSON")
}

async fn setup_background_tool_hook(
    test_name: &str,
    registry: BackgroundToolRegistry,
) -> (support::TestDb, DefraSessionHook, String, String) {
    let db = test_db(test_name).await;
    let agent_did = db.node_identity.did().to_string();
    let session_id = format!("{test_name}-session");
    let request_id = format!("{test_name}-request");
    support::create_request_for_agent_with_signed_fields(
        db.node.as_ref(),
        &agent_did,
        &request_id,
        &session_id,
        "processing",
        "2026-05-19T00:00:00Z",
        None,
        None,
        None,
        None,
    )
    .await;
    support::create_agent_session(
        db.node.as_ref(),
        &session_id,
        "r6-background-theorem",
        "2026-05-19T00:00:00Z",
    )
    .await;

    let hook = DefraSessionHook::resume_with_identity_policy(
        db.node.clone(),
        &session_id,
        "r6-background-theorem",
        &agent_did,
        None,
        FailurePolicy::default(),
    )
    .await
    .expect("resume background theorem hook")
    .with_background_tool_registry(registry);
    hook.set_active_request_lineage(Some(request_id.clone()), None)
        .await
        .expect("bind persisted request lineage");
    hook.set_request_deadline_at(Some(chrono::Utc::now() + chrono::Duration::minutes(5)))
        .await;
    (db, hook, session_id, request_id)
}

async fn setup_background_spawn_fixture(
    test_name: &str,
    targets: Vec<&str>,
    parent_subagent_depth: u32,
    background_enabled: bool,
) -> (
    support::TestDb,
    DefraSessionHook,
    String,
    String,
    chrono::DateTime<chrono::Utc>,
) {
    let db = test_db(test_name).await;
    let agent_did = db.node_identity.did().to_string();
    let parent_deadline = chrono::Utc::now() + chrono::Duration::minutes(5);
    let selection_id = format!("{test_name}-tools");

    support::fixtures::configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        BACKGROUND_THEOREM_CHILD_BEHAVIOR_ID,
        &format!("{test_name}-child-tools"),
        Vec::new(),
        false,
        false,
        None,
    )
    .await;
    support::fixtures::configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        BACKGROUND_THEOREM_PARENT_BEHAVIOR_ID,
        &selection_id,
        targets
            .into_iter()
            .map(|behavior_id| {
                support::fixtures::subagent_target(&agent_did, behavior_id, &agent_did, behavior_id)
            })
            .collect(),
        true,
        background_enabled,
        None,
    )
    .await;

    let session_id = format!("{test_name}-session");
    let request_id = format!("{test_name}-parent");
    create_background_theorem_parent_request(
        db.node.as_ref(),
        &request_id,
        &session_id,
        &agent_did,
        parent_subagent_depth,
        parent_deadline,
    )
    .await;
    support::create_agent_session(
        db.node.as_ref(),
        &session_id,
        BACKGROUND_THEOREM_PARENT_BEHAVIOR_ID,
        "2026-05-19T00:00:00Z",
    )
    .await;

    let hook = DefraSessionHook::resume_with_identity_policy(
        db.node.clone(),
        &session_id,
        BACKGROUND_THEOREM_PARENT_BEHAVIOR_ID,
        &agent_did,
        None,
        FailurePolicy::default(),
    )
    .await
    .expect("resume background theorem parent hook");
    let request_doc_id = crate::support::exact_request_doc_id(db.node.as_ref(), &request_id).await;
    hook.set_active_request_binding(Some(request_id.clone()), Some(request_doc_id), None)
        .await;
    hook.set_request_deadline_at(Some(parent_deadline)).await;

    (db, hook, session_id, request_id, parent_deadline)
}

async fn create_background_theorem_parent_request(
    node: &EmbeddedNode,
    request_id: &str,
    session_id: &str,
    agent_did: &str,
    subagent_depth: u32,
    deadline: chrono::DateTime<chrono::Utc>,
) {
    let request_id = escape_graphql_string(request_id);
    let session_id = escape_graphql_string(session_id);
    let behavior_id = escape_graphql_string(BACKGROUND_THEOREM_PARENT_BEHAVIOR_ID);
    let agent_did = escape_graphql_string(agent_did);
    let created_at = chrono::Utc::now().to_rfc3339();
    let deadline = deadline.to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "{agent_did}",
                behavior_id: "{behavior_id}",
                session_id: "{session_id}",
                retry_parent_request: "",
                retry_root_request: "{request_id}",
                superseded_by_request: "",
                content: "parent prompt",
                lifecycle_state: "processing",
                backend_id: "",
                execution_origin: "interactive",
                failure_reason: "",
                created_at: "{created_at}",
                deadline: "{deadline}",
                retry_count: 0,
                max_retries: 3,
                subagent_depth: {subagent_depth}
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create background theorem parent AgentRequest failed: {:?}",
        response.errors
    );
}

async fn fetch_background_theorem_tool_call(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> BackgroundTheoremToolCallRow {
    let session_id = escape_graphql_string(session_id);
    let tool_call_id = escape_graphql_string(tool_call_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    tool_call_id: {{ _eq: "{tool_call_id}" }}
                }}
                limit: 1
            ) {{
                await_mode
                cancel_policy
                child_request_id
                lifecycle_state
                result
                cancel_cause
                cancel_cascade_intent_at
            }}
        }}"#
    );
    first_row(&node.execute(&query).await, "AgentToolCall")
}

async fn count_live_backgrounded_rows(
    node: &EmbeddedNode,
    request_id: &str,
) -> anyhow::Result<usize> {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    request_id: {{ _eq: "{request_id}" }},
                    await_mode: {{ _eq: "background" }}
                }}
            ) {{
                lifecycle_state
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query live backgrounded tool count for request failed: {:?}",
            response.errors
        );
    }
    let rows: Vec<BackgroundedRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    Ok(rows
        .into_iter()
        .filter(|row| {
            !matches!(
                row.lifecycle_state.as_deref(),
                Some("completed" | "failed" | "timedOut" | "cancelled")
            )
        })
        .count())
}

async fn count_tool_calls_by_name(node: &EmbeddedNode, session_id: &str, tool_name: &str) -> usize {
    let session_id = escape_graphql_string(session_id);
    let tool_name = escape_graphql_string(tool_name);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    tool_name: {{ _eq: "{tool_name}" }}
                }}
            ) {{
                tool_call_id
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "count AgentToolCall by name failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| value.as_array())
        .map(Vec::len)
        .unwrap_or(0)
}

async fn fetch_background_theorem_child_request_optional(
    node: &EmbeddedNode,
    child_request_id: &str,
) -> Option<AgentRequestRow> {
    let child_request_id = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{child_request_id}" }} }}
                limit: 1
            ) {{
                _docID
                request_id
                agent_did
                requester_did
                behavior_id
                session_id
                content
                temperature
                top_p
                top_k
                seed
                max_tokens
                input
                execution_origin
                created_at
                deadline
                subagent_depth
                caused_by_parent_request_id
                caused_by_parent_request_doc_id
                caused_by_parent_tool_call_id
                caused_by_parent_tool_call_doc_id
                lifecycle_state
            }}
        }}"#
    );
    first_optional_row(&node.execute(&query).await, "AgentRequest")
}

async fn wait_for_background_theorem_child_lifecycle_state(
    node: &EmbeddedNode,
    child_request_id: &str,
    expected_state: &str,
) -> AgentRequestRow {
    let timeout_at = tokio::time::Instant::now() + Duration::from_secs(10);

    loop {
        if let Some(row) =
            fetch_background_theorem_child_request_optional(node, child_request_id).await
        {
            let expected_state = RequestLifecycleState::parse(expected_state)
                .expect("Lean request lifecycle state must be valid");
            if row.lifecycle_state == Some(expected_state) {
                return row;
            }

            if tokio::time::Instant::now() >= timeout_at {
                panic!(
                    "timed out waiting for child {child_request_id} lifecycle_state={expected_state}; last row: {row:?}"
                );
            }
        } else if tokio::time::Instant::now() >= timeout_at {
            panic!(
                "timed out waiting for child {child_request_id} to be materialized (expected lifecycle_state={expected_state})"
            );
        }

        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

pub(super) async fn generated_r6_backgrounding_cases_drive_tool_backgrounding_contract() {
    let cases = lean_r6_backgrounding_cases();
    assert_eq!(cases.len(), 41);

    let names = cases
        .iter()
        .map(|case| case.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names,
        [
            "no_goal_preserves_background_wake",
            "active_goal_owns_background_continuation",
            "paused_goal_does_not_background_resume",
            "blocked_goal_does_not_background_resume",
            "usage_limited_goal_does_not_background_resume",
            "budget_limited_goal_owns_wrapup",
            "complete_goal_does_not_background_resume",
            "background_tool_budget_count_7_admits_spawn",
            "background_tool_budget_count_8_rejects_spawn",
            "tool_kind_background_mode_executes",
            "tool_kind_bridge_complete_persists_result",
            "tool_kind_explicit_cancel_projects_explicit_cancel",
            "background_recovery_running_live_parent_to_cancelled",
            "background_completion_source_writes_canonical_key",
            "terminal_completion_message_precedes_claimed_continuation",
            "failed_background_wake_with_budget_redrives",
            "failed_background_wake_exhausted_budget_stops",
            "generic_scheduled_failure_is_not_background_redrive",
            "non_latest_background_wake_does_not_redrive",
            "aged_background_wake_precedes_new_descendant",
            "fresh_background_wake_preserves_fifo",
            "completed_wake_acknowledges_exact_claim_snapshot",
            "failed_wake_retains_claim_snapshot_unacknowledged",
            "restart_before_claim_preserves_pending_notification",
            "inference_failure_retains_snapshot_for_bounded_redrive",
            "response_persisted_before_crash_recovers_completed_ack",
            "acknowledgement_projection_restart_is_atomic",
            "noncanonical_subagent_completion_source_is_rejected",
            "list_processes_same_requester_next_turn_authorized",
            "read_process_same_requester_next_turn_authorized",
            "wait_process_same_requester_next_turn_authorized",
            "cancel_process_same_requester_next_turn_authorized",
            "originating_request_without_matching_requester_is_denied",
            "absent_requester_next_turn_authorized",
            "empty_requester_does_not_alias_absent",
            "process_control_cross_session_denied",
            "process_control_cross_agent_denied",
            "process_control_cross_requester_denied",
            "wait_timeout_preserves_running_process",
            "caller_interrupt_preserves_running_process",
            "caller_deadline_preserves_running_process",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>()
    );

    for case in cases {
        assert_eq!(case.max_backgrounded, 8, "{}", case.name);
        assert!(
            case.pre_live_count <= case.max_backgrounded,
            "{}",
            case.name
        );
        assert_eq!(case.await_mode.as_str(), "background", "{}", case.name);
        assert_eq!(case.cancel_policy.as_str(), "cascade", "{}", case.name);
        assert_eq!(case.child_request_id.as_deref(), None, "{}", case.name);
    }

    let admit = lean_r6_backgrounding_case("background_tool_budget_count_7_admits_spawn");
    assert!(admit.legal);
    assert_eq!(admit.pre_live_count, 7);
    assert_eq!(admit.terminal_state.as_str(), "running");

    let reject = lean_r6_backgrounding_case("background_tool_budget_count_8_rejects_spawn");
    assert!(!reject.legal);
    assert_eq!(reject.pre_live_count, 8);
    assert_eq!(
        reject.error_code.as_deref(),
        Some("background_tool_budget_exceeded")
    );

    let completed = lean_r6_backgrounding_case("tool_kind_bridge_complete_persists_result");
    assert!(completed.legal);
    assert_eq!(completed.terminal_state.as_str(), "completed");
    assert_eq!(completed.result.as_deref(), Some("done"));

    let cancelled =
        lean_r6_backgrounding_case("tool_kind_explicit_cancel_projects_explicit_cancel");
    assert_eq!(cancelled.terminal_state.as_str(), "cancelled");
    assert_eq!(cancelled.reason.as_deref(), Some("explicit_cancel"));

    let backgrounded = lean_r6_backgrounding_case("tool_kind_background_mode_executes");
    assert!(backgrounded.legal);
    assert_eq!(backgrounded.action.as_str(), "background");
    assert_eq!(backgrounded.terminal_state.as_str(), "running");

    let recovered =
        lean_r6_backgrounding_case("background_recovery_running_live_parent_to_cancelled");
    assert_eq!(
        recovered.action.as_str(),
        "TerminalizeBackgroundedAsInterrupted"
    );
    assert_eq!(recovered.terminal_state.as_str(), "cancelled");
    assert_eq!(recovered.reason.as_deref(), Some("interrupted_on_restart"));
    assert_eq!(
        recovered.queue_source.as_deref(),
        Some("background_completion")
    );
    assert_eq!(
        recovered.queue_key.as_deref(),
        Some("background_completion:900")
    );

    let canonical = lean_r6_backgrounding_case("background_completion_source_writes_canonical_key");
    assert_eq!(
        canonical.queue_source.as_deref(),
        Some("background_completion")
    );
    assert_eq!(
        canonical.queue_key.as_deref(),
        Some("background_completion:900")
    );

    let noncanonical =
        lean_r6_backgrounding_case("noncanonical_subagent_completion_source_is_rejected");
    assert_eq!(
        noncanonical.queue_source.as_deref(),
        Some("subagent_completion")
    );
    assert_eq!(noncanonical.queue_key, None);

    let redrive = lean_r6_backgrounding_case("failed_background_wake_with_budget_redrives");
    assert!(redrive.legal);
    assert_eq!(redrive.group, "completion_redrive");
    assert_eq!(redrive.action, "redrive_failed_background_wake");
    assert_eq!(redrive.retry_count, Some(1));
    assert_eq!(redrive.max_retries, Some(3));
    assert_eq!(redrive.post_retry_count, Some(2));
    assert_eq!(redrive.retry_delay_seconds, Some(10));
    assert_eq!(
        gents::lifecycle::background_wake_retry_delay(redrive.retry_count.unwrap() as i64)
            .num_seconds(),
        redrive.retry_delay_seconds.unwrap() as i64
    );
    assert_eq!(redrive.is_latest, Some(true));

    let exhausted = lean_r6_backgrounding_case("failed_background_wake_exhausted_budget_stops");
    assert!(!exhausted.legal);
    assert_eq!(exhausted.retry_count, exhausted.max_retries);
    assert_eq!(exhausted.post_retry_count, None);

    let generic = lean_r6_backgrounding_case("generic_scheduled_failure_is_not_background_redrive");
    assert!(!generic.legal);
    assert_eq!(generic.queue_source.as_deref(), Some("user"));

    let non_latest = lean_r6_backgrounding_case("non_latest_background_wake_does_not_redrive");
    assert!(!non_latest.legal);
    assert_eq!(non_latest.is_latest, Some(false));

    let aged = lean_r6_backgrounding_case("aged_background_wake_precedes_new_descendant");
    assert!(aged.legal);
    assert_eq!(aged.group, "completion_admission");
    assert_eq!(aged.action, "rank_pending_background_wake");
    assert_eq!(aged.reason.as_deref(), Some("aged_priority"));

    let fresh = lean_r6_backgrounding_case("fresh_background_wake_preserves_fifo");
    assert!(!fresh.legal);
    assert_eq!(fresh.group, "completion_admission");
    assert_eq!(fresh.reason.as_deref(), Some("fifo"));

    let acknowledged =
        lean_r6_backgrounding_case("completed_wake_acknowledges_exact_claim_snapshot");
    assert!(acknowledged.legal);
    assert_eq!(acknowledged.group, "completion_acknowledgement");
    assert_eq!(acknowledged.terminal_state, "completed");
    assert_eq!(
        acknowledged.result.as_deref(),
        Some("attempted=1,acknowledged=1")
    );
    assert_eq!(acknowledged.reason.as_deref(), Some("completed_ack"));

    let retained = lean_r6_backgrounding_case("failed_wake_retains_claim_snapshot_unacknowledged");
    assert!(retained.legal);
    assert_eq!(retained.group, "completion_acknowledgement");
    assert_eq!(retained.terminal_state, "failed");
    assert_eq!(
        retained.result.as_deref(),
        Some("attempted=1,acknowledged=0")
    );
    assert_eq!(retained.reason.as_deref(), Some("failed_unacknowledged"));

    let before_claim =
        lean_r6_backgrounding_case("restart_before_claim_preserves_pending_notification");
    assert!(before_claim.legal);
    assert_eq!(before_claim.group, "completion_failure_boundary");
    assert_eq!(before_claim.action, "restart_before_claim");
    assert_eq!(before_claim.terminal_state, "pending");
    assert_eq!(
        before_claim.result.as_deref(),
        Some("attempted=0,acknowledged=0")
    );
    assert_eq!(before_claim.reason.as_deref(), Some("pending_reclaim"));

    let during_inference =
        lean_r6_backgrounding_case("inference_failure_retains_snapshot_for_bounded_redrive");
    assert!(during_inference.legal);
    assert_eq!(during_inference.action, "fail_during_inference");
    assert_eq!(during_inference.terminal_state, "failed");
    assert_eq!(
        during_inference.result.as_deref(),
        Some("attempted=1,acknowledged=0")
    );
    assert_eq!(during_inference.reason.as_deref(), Some("bounded_retry"));

    let after_response =
        lean_r6_backgrounding_case("response_persisted_before_crash_recovers_completed_ack");
    assert!(after_response.legal);
    assert_eq!(after_response.action, "recover_after_response_persistence");
    assert_eq!(after_response.terminal_state, "completed");
    assert_eq!(
        after_response.result.as_deref(),
        Some("attempted=1,acknowledged=1")
    );
    assert_eq!(
        after_response.reason.as_deref(),
        Some("recovered_completed_ack")
    );

    let during_ack = lean_r6_backgrounding_case("acknowledgement_projection_restart_is_atomic");
    assert!(during_ack.legal);
    assert_eq!(during_ack.action, "project_acknowledgement_after_restart");
    assert_eq!(during_ack.terminal_state, "completed");
    assert_eq!(
        during_ack.result.as_deref(),
        Some("attempted=1,acknowledged=1")
    );
    assert_eq!(during_ack.reason.as_deref(), Some("atomic_ack_projection"));

    for case in cases.iter().filter(|case| case.group == "native_lifecycle") {
        drive_r6_native_lifecycle_case(case).await;
    }

    for case in cases
        .iter()
        .filter(|case| case.group == "completion_continuation_owner")
    {
        drive_r6_completion_owner_case(case).await;
    }

    let continuation =
        lean_r6_backgrounding_case("terminal_completion_message_precedes_claimed_continuation");
    drive_r6_completion_continuation_case(continuation).await;

    for action in [
        "list_processes",
        "read_process",
        "wait_process",
        "cancel_process",
    ] {
        let case = cases
            .iter()
            .find(|case| {
                case.group == "process_control_authorization"
                    && case.action == action
                    && case.reason.as_deref() == Some("same_requester_next_turn")
            })
            .unwrap_or_else(|| panic!("missing same-principal process control case for {action}"));
        assert!(case.legal, "{} must remain authorized", case.name);
    }

    for scenario in ["cross_session", "cross_agent", "cross_requester"] {
        let case = cases
            .iter()
            .find(|case| {
                case.group == "process_control_authorization"
                    && case.reason.as_deref() == Some(scenario)
            })
            .unwrap_or_else(|| panic!("missing denied process control case for {scenario}"));
        assert!(!case.legal, "{} must be denied", case.name);
    }

    assert!(lean_r6_backgrounding_case("absent_requester_next_turn_authorized").legal);
    assert!(!lean_r6_backgrounding_case("empty_requester_does_not_alias_absent").legal);

    for reason in [
        "wait_timeout",
        "caller_interrupted",
        "caller_deadline_exceeded",
    ] {
        let case = cases
            .iter()
            .find(|case| case.group == "wait_boundary" && case.reason.as_deref() == Some(reason))
            .unwrap_or_else(|| panic!("missing wait boundary case for {reason}"));
        assert!(case.legal, "{} must not request cancellation", case.name);
        assert_eq!(case.terminal_state, "running", "{}", case.name);
    }
    process_control_requester_absence_cases_drive_owner_authorization().await;
}

// Exercise request binding, persisted ownership and the read-process envelope.
async fn process_control_requester_absence_cases_drive_owner_authorization() {
    let (db, hook, session_id, request_id) = setup_background_tool_hook(
        "r6-process-control-absent-requester",
        background_tool_registry(vec![Box::new(PendingTool)], &["slow_tool"]),
    )
    .await;
    let spawn = skip_reason_json(
        hook.on_tool_call(
            "spawn_process",
            None,
            "meta-bg-absent-requester",
            r#"{"tool_name":"slow_tool","args":{}}"#,
        )
        .await,
    );
    assert_eq!(spawn["ok"].as_bool(), Some(true));
    let tool_call_id = spawn["tool_call_id"]
        .as_str()
        .expect("background handle")
        .to_string();
    let query = format!(
        r#"{{ AgentToolCall(filter: {{
        session_id: {{ _eq: "{}" }}, tool_call_id: {{ _eq: "{}" }}
    }}, limit: 1) {{ requester_did }} }}"#,
        escape_graphql_string(&session_id),
        escape_graphql_string(&tool_call_id)
    );
    let row: Value = first_row(&db.node.execute(&query).await, "AgentToolCall");
    assert_eq!(
        row.get("requester_did"),
        Some(&Value::Null),
        "the selected owner field must be present and null"
    );

    let next_request_id = format!("{request_id}-next");
    support::create_request_for_agent_with_signed_fields(
        db.node.as_ref(),
        db.node_identity.did(),
        &next_request_id,
        &session_id,
        "processing",
        "2026-05-19T00:00:01Z",
        None,
        None,
        None,
        None,
    )
    .await;
    for (name, caller_request, requester) in [
        (
            "absent_requester_next_turn_authorized",
            &next_request_id,
            None,
        ),
        (
            "empty_requester_does_not_alias_absent",
            &next_request_id,
            Some(""),
        ),
        (
            "originating_request_without_matching_requester_is_denied",
            &request_id,
            Some("did:requester"),
        ),
    ] {
        let case = lean_r6_backgrounding_case(name);
        hook.set_active_request_lineage(
            Some(caller_request.clone()),
            requester.map(str::to_string),
        )
        .await
        .expect("bind requester scope");
        let read = skip_reason_json(
            hook.on_tool_call(
                "read_process",
                None,
                &format!("read-{name}"),
                &json!({ "tool_call_id": tool_call_id }).to_string(),
            )
            .await,
        );
        if case.legal {
            assert_eq!(read["status"].as_str(), Some("running"), "{name}: {read}");
            assert_eq!(
                read["tool_call_id"].as_str(),
                Some(tool_call_id.as_str()),
                "{name}"
            );
        } else {
            assert_eq!(read["ok"].as_bool(), Some(false), "{name}: {read}");
            assert_eq!(
                read["failure_class"].as_str(),
                Some("tool_not_allowed"),
                "{name}"
            );
        }
    }
    let owner =
        fetch_background_theorem_tool_call(db.node.as_ref(), &session_id, &tool_call_id).await;
    assert_eq!(owner.lifecycle_state.as_deref(), Some("running"));
    assert!(
        owner.cancel_cause.is_none(),
        "denied reads must not cancel the job"
    );

    // Restore the owner scope and stop the pending task after observing denials.
    hook.set_active_request_lineage(Some(request_id), None)
        .await
        .expect("restore owner scope");
    let cancelled = skip_reason_json(
        hook.on_tool_call(
            "cancel_process",
            None,
            "cleanup-absent-requester",
            &json!({ "tool_call_id": tool_call_id }).to_string(),
        )
        .await,
    );
    assert_eq!(cancelled["status"].as_str(), Some("cancelled"));
}

async fn fetch_completion_wakes(node: &EmbeddedNode, session_id: &str) -> Vec<AgentRequestRow> {
    let session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }}
                    execution_origin: {{ _eq: "scheduled" }}
                }}
                order: {{ created_at: ASC }}
            ) {{
                request_id
                lifecycle_state
                input
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "fetch completion wakes failed: {:?}",
        response.errors
    );
    let data = response.data.expect("completion wake query data");
    serde_json::from_value(data["AgentRequest"].clone()).expect("parse completion wake rows")
}

async fn drive_r6_completion_continuation_case(case: &lean_vocab_test::LeanR6BackgroundingCase) {
    use gents::background_completion::{
        project_background_subagent_completion, BackgroundCompletionOutcome,
    };

    assert!(case.legal, "composed Lean acceptance path must execute");
    assert_eq!(case.action, "terminalize_append_notification_enqueue_claim");
    assert_eq!(case.terminal_state, "completed");
    assert_eq!(
        case.result.as_deref(),
        Some("assistant_wait_precedes_notification")
    );
    assert_eq!(case.reason.as_deref(), Some("continuation_claimed"));
    assert_eq!(case.queue_source.as_deref(), Some("background_completion"));

    let bridge_case = lean_bridge_step_cases()
        .iter()
        .find(|candidate| candidate.name == "bridge_step_complete_child_completed")
        .expect("generated completed bridge case");
    let (db, _lifecycle, _tool_call_id, child_request_id, parent_session_id) =
        seed_bridge_step_fixture(bridge_case).await;
    let parent_request_id = format!("{}-parent", bridge_case.name);
    let child_session_id = fetch_child_session_id(db.node.as_ref(), &child_request_id).await;

    // Materialize the model-visible wait call before the child terminalizes.
    // This is the durable sequence reservation represented by the composed
    // Lean witness.
    let wait_message = serde_json::to_string(&Message::Assistant {
        id: None,
        content: vec![AssistantContent::ToolCall(ToolCall {
            id: "r6-wait-result".to_string(),
            call_id: Some("r6-wait-call".to_string()),
            function: ToolFunction {
                name: "wait_subagent".to_string(),
                arguments: json!({ "child_request_id": child_request_id }),
            },
            signature: None,
            additional_params: None,
        })],
    })
    .expect("serialize wait assistant message");
    let escaped_session_id = escape_graphql_string(&parent_session_id);
    let escaped_request_id = escape_graphql_string(&parent_request_id);
    let agent_did = escape_graphql_string(db.node_identity.did());
    let escaped_wait_message = escape_graphql_string(&wait_message);
    let reserve = db
        .node
        .execute(&format!(
            r#"mutation {{
                create_AgentMessage(input: {{
                    message_key: "{escaped_session_id}:1"
                    session_id: "{escaped_session_id}"
                    agent_did: "{agent_did}"
                    request_id: "{escaped_request_id}"
                    sequence: 1
                    role: "assistant"
                    content: "{escaped_wait_message}"
                    reasoning: ""
                    timestamp: "2026-05-19T00:00:02Z"
                }}) {{ _docID }}
            }}"#
        ))
        .await;
    assert!(
        !reserve.has_errors(),
        "reserve assistant wait row failed: {:?}",
        reserve.errors
    );

    persist_bridge_step_child_completion(db.node.as_ref(), &child_request_id, &child_session_id)
        .await;

    let outcome = project_background_subagent_completion(
        db.node.clone(),
        &child_request_id,
        db.node_identity.did(),
    )
    .await
    .expect("project terminal background completion");
    assert!(
        matches!(outcome, BackgroundCompletionOutcome::Projected { .. }),
        "terminal bridge must project before continuation: {outcome:?}"
    );

    let messages_before_claim =
        fetch_message_snapshots_for_session(db.node.as_ref(), &parent_session_id).await;
    assert_eq!(
        messages_before_claim.len(),
        2,
        "reserved assistant wait and terminal notification must both remain durable"
    );
    assert_eq!(messages_before_claim[0].role, "assistant");
    assert!(
        messages_before_claim[0]
            .content
            .contains("\"wait_subagent\""),
        "model-visible wait envelope missing: {:?}",
        messages_before_claim[0].content
    );
    assert_eq!(messages_before_claim[1].role, "user");
    assert!(
        messages_before_claim[1]
            .content
            .contains("<subagent-notification"),
        "model-visible completion envelope missing: {:?}",
        messages_before_claim[1].content
    );
    assert!(
        messages_before_claim[0].sequence < messages_before_claim[1].sequence,
        "assistant wait must precede its terminal notification: {messages_before_claim:#?}"
    );

    let wakes = fetch_completion_wakes(db.node.as_ref(), &parent_session_id).await;
    assert_eq!(wakes.len(), 1, "one completion must enqueue one wake");
    assert_eq!(
        wakes[0].lifecycle_state,
        Some(RequestLifecycleState::Pending)
    );
    let queue = wakes[0]
        .input
        .as_ref()
        .expect("typed wake input")
        .queue
        .as_ref()
        .expect("wake queue");
    assert_eq!(
        queue.source,
        gents_protocol::request_input::QueueSource::BackgroundCompletion
    );
    assert_eq!(
        queue.policy,
        gents_protocol::request_input::QueuePolicy::Coalesce
    );
    assert_eq!(
        queue.key.as_deref(),
        Some(format!("background_completion:{parent_session_id}").as_str())
    );
    assert_eq!(queue.background_completion_wake_version, Some(1));
    assert_eq!(
        case.queue_key.as_deref(),
        Some("background_completion:900"),
        "Lean uses opaque session 900 as the canonical runtime-key representative"
    );

    // The foreground parent owns the session until terminal. Its completion
    // releases the FIFO head, after which the normal watcher claims the
    // generated wake as the next agent turn.
    set_request_lifecycle_state_by_request_id(db.node.as_ref(), &parent_request_id, "completed")
        .await;
    let mut watcher = DefraWatcher::new(db.node.clone(), db.node_identity.did());
    let claimed = tokio::time::timeout(Duration::from_secs(2), watcher.next_request())
        .await
        .expect("completion wake should become claimable")
        .expect("watcher should remain open")
        .expect("completion wake should load");
    assert_eq!(claimed.request_id, wakes[0].request_id);
    assert_eq!(claimed.session_id, parent_session_id);

    let messages_after_claim =
        fetch_message_snapshots_for_session(db.node.as_ref(), &parent_session_id).await;
    assert_eq!(
        messages_after_claim, messages_before_claim,
        "claiming the continuation must retain the notification provider history"
    );
}

async fn drive_r6_native_lifecycle_case(case: &lean_vocab_test::LeanR6BackgroundingCase) {
    let db = test_db(&format!("r6-native-lifecycle-{}", case.name)).await;
    let request_id = format!("{}-request", case.name);
    let session_id = format!("{}-session", case.name);
    let tool_call_id = format!("{}-tool", case.name);
    let deadline = chrono::Utc::now() + chrono::Duration::minutes(5);
    let mut lifecycle = if case.action == "background" {
        ToolCallLifecycle::new(
            db.node.clone(),
            request_id,
            session_id.clone(),
            AGENT_DID.to_string(),
            tool_call_id.clone(),
            1,
            "bash_unrestricted".to_string(),
            "{}".to_string(),
            deadline,
        )
    } else {
        ToolCallLifecycle::new_background_tool(
            db.node.clone(),
            request_id,
            session_id.clone(),
            AGENT_DID.to_string(),
            tool_call_id.clone(),
            1,
            "bash_unrestricted".to_string(),
            "{}".to_string(),
            deadline,
        )
    };
    lifecycle
        .start_running()
        .await
        .unwrap_or_else(|error| panic!("{} start_running failed: {error:#}", case.name));

    match case.action.as_str() {
        "background" => lifecycle.background().await.unwrap(),
        "bridge_complete" => {
            assert!(
                lifecycle
                    .bridge_complete(case.result.clone().unwrap_or_default())
                    .await
                    .unwrap(),
                "{} must win the running-state compare",
                case.name
            );
        }
        "bridge_failure" => {
            assert!(
                lifecycle
                    .bridge_failure(gents::tool_call_lifecycle::ChildTerminal::Interrupted)
                    .await
                    .unwrap(),
                "{} must win the running-state compare",
                case.name
            );
        }
        other => panic!("unhandled native lifecycle action {other}"),
    }

    let row =
        fetch_background_theorem_tool_call(db.node.as_ref(), &session_id, &tool_call_id).await;
    assert_eq!(
        row.await_mode.as_deref(),
        Some(case.await_mode.as_str()),
        "{} await mode drifted",
        case.name
    );
    assert_eq!(
        row.cancel_policy.as_deref(),
        Some(case.cancel_policy.as_str()),
        "{} cancel policy drifted",
        case.name
    );
    assert_eq!(
        row.child_request_id.as_deref(),
        case.child_request_id.as_deref(),
        "{} child-link kind drifted",
        case.name
    );
    assert_eq!(
        row.lifecycle_state.as_deref(),
        Some(case.terminal_state.as_str()),
        "{} lifecycle projection drifted",
        case.name
    );
    if let Some(expected) = case.result.as_deref() {
        assert_eq!(row.result.as_deref(), Some(expected), "{}", case.name);
    }
    if case.action == "bridge_failure" {
        assert_eq!(
            row.cancel_cause.as_deref(),
            Some("interrupted"),
            "{} cancellation cause drifted",
            case.name
        );
    }
}

pub(super) async fn generated_r6_background_theorem_witnesses_drive_admission_budget_invariant() {
    let witnesses = lean_r6_background_theorem_witnesses();
    assert_eq!(witnesses.len(), 2);

    let witness = lean_r6_background_theorem_witness("Subagent.admitted_background_count_bounded");
    assert_eq!(witness.witness_kind.as_str(), "admission_bound");
    assert_eq!(
        witness.scenario.as_str(),
        "background_tool_admission_respects_max_backgrounded_per_parent"
    );

    let max_backgrounded = witness.numeric_bound;
    let await_mode_expected = witness.kind_field("await_mode");
    let cancel_policy_expected = witness.kind_field("cancel_policy");
    let error_code_expected = witness.kind_field("error_code_on_violation");

    let (db, hook, session_id, request_id) = setup_background_tool_hook(
        "r6-background-theorem-budget",
        background_tool_registry(vec![Box::new(PendingTool)], &["slow_tool"]),
    )
    .await;

    for index in 0..max_backgrounded {
        let internal_call_id = format!("meta-theorem-bg-{index}");
        let receipt = skip_reason_json(
            hook.on_tool_call(
                "spawn_process",
                None,
                &internal_call_id,
                r#"{"tool_name":"slow_tool","args":{}}"#,
            )
            .await,
        );
        assert_eq!(receipt["status"].as_str(), Some("running"));
        assert_eq!(receipt["await_mode"].as_str(), Some(await_mode_expected));
        let background_tool_call_id = receipt["tool_call_id"]
            .as_str()
            .expect("background receipt tool_call_id");

        let row = fetch_background_theorem_tool_call(
            db.node.as_ref(),
            &session_id,
            background_tool_call_id,
        )
        .await;
        assert_eq!(row.await_mode.as_deref(), Some(await_mode_expected));
        assert_eq!(row.cancel_policy.as_deref(), Some(cancel_policy_expected));

        let live = count_live_backgrounded_rows(db.node.as_ref(), &request_id)
            .await
            .expect("count live backgrounded rows");
        assert!(
            live <= max_backgrounded,
            "live count {live} exceeded witness bound {max_backgrounded} after admit #{index}"
        );
        assert_eq!(live, index + 1);
    }

    let denied = skip_reason_json(
        hook.on_tool_call(
            "spawn_process",
            None,
            "meta-theorem-bg-overflow",
            r#"{"tool_name":"slow_tool","args":{}}"#,
        )
        .await,
    );
    assert_eq!(denied["code"].as_str(), Some(error_code_expected));
    assert_eq!(
        denied["current_backgrounded"]
            .as_u64()
            .map(|value| value as usize),
        Some(max_backgrounded)
    );
    assert_eq!(
        denied["max_backgrounded"]
            .as_u64()
            .map(|value| value as usize),
        Some(max_backgrounded)
    );

    let live_after = count_live_backgrounded_rows(db.node.as_ref(), &request_id)
        .await
        .expect("count live backgrounded rows after denial");
    assert_eq!(live_after, max_backgrounded);
    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "slow_tool").await,
        max_backgrounded
    );
}

/// Drives the local cascade-dispatch trace witness through the child request's
/// persisted `interrupted` post-state.
pub(super) async fn generated_r6_background_theorem_witnesses_drive_cascade_cancellation_trace() {
    let witness = lean_r6_background_theorem_witness("Subagent.BridgedState.cascade_cancels_child");
    assert_eq!(witness.witness_kind.as_str(), "reachability_trace");
    assert_eq!(
        witness.scenario.as_str(),
        "parent_terminal_with_cascade_bridge_interrupts_processing_child"
    );
    assert_eq!(witness.numeric_bound, 2);

    let cancel_policy_expected = witness.kind_field("cancel_policy");
    let child_post_state_expected = witness.kind_field("child_post_state");
    assert_eq!(witness.kind_field("child_pre_state"), "processing");
    assert_eq!(witness.kind_field("child_pre_admission"), "executing");

    let (db, hook, session_id, _request_id, _parent_deadline) = setup_background_spawn_fixture(
        "r6-background-theorem-cascade",
        vec![BACKGROUND_THEOREM_CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    let agent_did = db.node_identity.did().to_string();
    // After spawn convergence (#377) the child AgentRequest is materialized by
    // SubagentSource, not synchronously by the hook.  Hold a standalone source
    // for the lifetime of this test so the bridge row produces a child request.
    let _source = super::support::fixtures::spawn_subagent_source(
        db.node.clone(),
        &agent_did,
        BACKGROUND_THEOREM_PARENT_BEHAVIOR_ID,
        BACKGROUND_THEOREM_CHILD_BEHAVIOR_ID,
    );
    let args = json!({
        "name": BACKGROUND_THEOREM_CHILD_BEHAVIOR_ID,
        "prompt": "child for cascade theorem witness",
        "await_mode": "background"
    })
    .to_string();

    let action = hook
        .on_tool_call(
            "spawn_subagent",
            Some("model-call-theorem-cascade".to_string()),
            "internal-theorem-cascade",
            &args,
        )
        .await;
    let receipt = skip_reason_json(action);
    let child_request_id = receipt["child_request_id"]
        .as_str()
        .expect("child_request_id")
        .to_string();

    let tool = fetch_background_theorem_tool_call(
        db.node.as_ref(),
        &session_id,
        "internal-theorem-cascade",
    )
    .await;
    assert_eq!(tool.cancel_policy.as_deref(), Some(cancel_policy_expected));
    assert_eq!(
        tool.child_request_id.as_deref(),
        Some(child_request_id.as_str())
    );

    // Wait for SubagentSource to materialize the child (post-convergence #377:
    // the child is no longer created synchronously by the hook).
    let child = wait_for_background_theorem_child_lifecycle_state(
        db.node.as_ref(),
        &child_request_id,
        "pending",
    )
    .await;
    assert_eq!(child.lifecycle_state, Some(RequestLifecycleState::Pending));
    let mut child_lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        BACKGROUND_THEOREM_CHILD_BEHAVIOR_ID,
        &agent_did,
        gents::watcher::AgentRequest::try_from(child)
            .expect("canonical child AgentRequest must satisfy runtime boundary"),
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    assert_eq!(
        child_lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    crate::support::begin_owned_execution(&mut child_lifecycle, &db.node)
        .await
        .unwrap();
    let child_pre =
        fetch_background_theorem_child_request_optional(db.node.as_ref(), &child_request_id)
            .await
            .expect("processing child");
    assert_eq!(
        child_pre.lifecycle_state,
        Some(
            RequestLifecycleState::parse(witness.kind_field("child_pre_state"))
                .expect("Lean request lifecycle state")
        )
    );
    let mut lifecycle =
        ToolCallLifecycle::load(db.node.clone(), &session_id, "internal-theorem-cascade")
            .await
            .expect("load bridge lifecycle")
            .expect("bridge should be persisted");
    let dispatch = lifecycle
        .cancel_during_run_with_cascade_dispatch(CancelCause::Interrupted, &agent_did)
        .await
        .expect("cancel bridge with cascade dispatch")
        .expect("cascade dispatch");
    let gents::tool_call_lifecycle::CascadeDispatch::Local { intent, child } = dispatch else {
        panic!("local child must use local cascade dispatch");
    };
    assert_eq!(intent.child_request_id, child_request_id);
    gents::interrupt_request_by_doc_id(
        db.node.as_ref(),
        child
            .doc_id
            .as_deref()
            .expect("verified physical cascade child"),
        child
            .agent_did
            .as_deref()
            .expect("verified local child principal"),
        child.requester_did.as_deref(),
    )
    .await
    .expect("interrupt child request");
    // This isolated consumer has no daemon observer running; use its terminal owner.
    child_lifecycle
        .terminalize_owned_without_stream(RequestTerminalOutcome::Interrupted, Some("interrupted"))
        .await
        .expect("drive child interrupt_processing transition");

    let tool = fetch_background_theorem_tool_call(
        db.node.as_ref(),
        &session_id,
        "internal-theorem-cascade",
    )
    .await;
    assert_eq!(tool.cancel_cause.as_deref(), Some("interrupted"));
    assert!(
        tool.cancel_cascade_intent_at.is_none(),
        "local cascade dispatch must not leave a remote bridge intent"
    );
    let child_post = wait_for_background_theorem_child_lifecycle_state(
        db.node.as_ref(),
        &child_request_id,
        child_post_state_expected,
    )
    .await;
    assert_eq!(
        child_post.lifecycle_state,
        Some(
            RequestLifecycleState::parse(child_post_state_expected)
                .expect("Lean request lifecycle state")
        )
    );
    let child_interrupt = fetch_interrupt_requested_at(db.node.as_ref(), &child_post.request_id)
        .await
        .expect("fetch child interrupt_requested_at");
    assert!(
        child_interrupt.is_some(),
        "cascade trace must preserve child interrupt_requested_at through {child_post_state_expected}"
    );
}

pub(super) fn delegation_depth_matches_runtime_limit() {
    let cases = lean_subagent_delegation_graph_cases();
    assert!(!cases.is_empty());
    for case in cases {
        assert_eq!(
            case.max_depth,
            usize::try_from(MAX_SUBAGENT_DEPTH).expect("MAX_SUBAGENT_DEPTH fits usize"),
            "{}: Lean and runtime delegation depth limits differ",
            case.name,
        );
    }
}

pub(super) fn unmaterialized_child_status_matches_runtime_vocabulary() {
    let LeanR4cBackgroundWorkCase::UnmaterializedChildVisible {
        listed_status,
        read_lifecycle_state,
        ..
    } = lean_r4c_background_work_case("r4c.list_subagents.unmaterialized_child_visible")
    else {
        panic!("unmaterialized child witness variant drifted");
    };
    for status in [listed_status, read_lifecycle_state] {
        assert_eq!(
            status,
            gents::__test_internals::AWAITING_CHILD_MATERIALIZATION
        );
    }
}

/// Drives the Lean `r4c.read_tool_output.dispatch_by_state` witness (#937)
/// through the real hook: a running native background tool serves its live
/// ring-buffer tail; a later-request hook sharing the process registry serves
/// the same live output; an explicitly unshared hook models daemon restart and
/// serves empty output for the still-running row. After completion every hook
/// serves the persisted result. Payloads and paging numbers are the
/// Lean-computed witness values.
pub(super) async fn generated_read_tool_output_witness_drives_hook_dispatch() {
    let LeanR4cBackgroundWorkCase::ReadToolOutputDispatchesByState {
        running_payload,
        running_no_buffer_payload,
        terminal_payload,
        running_next_offset,
        running_total_bytes,
        running_has_more,
        terminal_total_bytes,
        ..
    } = lean_r4c_background_work_case("r4c.read_tool_output.dispatch_by_state")
    else {
        panic!("read_tool_output witness variant drifted");
    };

    let tempdir = tempfile::tempdir().expect("tempdir");
    let tools = gents::ToolSet::builder()
        .bash_unrestricted(tempdir.path())
        .build()
        .build_native_tools()
        .expect("native tools should build");
    let (db, hook, session_id, request_id) = setup_background_tool_hook(
        "r4c-read-dispatch-witness",
        background_tool_registry(tools, &["bash_unrestricted"]),
    )
    .await;
    let shared_processes = gents::BackgroundExecutionRegistry::default();
    let hook = hook.with_background_execution_registry(shared_processes.clone());

    let spawn = skip_reason_json(
        hook.on_tool_call(
            "spawn_process",
            None,
            "read-dispatch-spawn",
            &json!({
                "tool_name": "bash_unrestricted",
                "args": {
                    "command": format!(
                        "printf {running_payload}; sleep 5; printf done"
                    ),
                    "args": [],
                    "timeout_secs": 10
                }
            })
            .to_string(),
        )
        .await,
    );
    let tool_call_id = spawn["tool_call_id"]
        .as_str()
        .expect("spawn receipt tool_call_id")
        .to_string();

    // Running + live snapshot → the ring-buffer tail, with the Lean-computed
    // continuation cursor. Bounded poll: the payload lands as soon as the
    // tool's first printf is flushed into the live writer.
    let mut running = json!({});
    for attempt in 0..80 {
        running = skip_reason_json(
            hook.on_tool_call(
                "read_process",
                None,
                &format!("read-dispatch-running-{attempt}"),
                &json!({ "tool_call_id": tool_call_id }).to_string(),
            )
            .await,
        );
        if running["output"].as_str() == Some(running_payload) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(running["status"].as_str(), Some("running"));
    assert_eq!(running["output"].as_str(), Some(running_payload.as_str()));
    assert_eq!(running["next_offset"].as_u64(), Some(*running_next_offset));
    assert_eq!(running["total_bytes"].as_u64(), Some(*running_total_bytes));
    assert_eq!(running["has_more"].as_bool(), Some(*running_has_more));
    assert_eq!(running["exited"].as_bool(), Some(false));

    // A new request gets a new hook, but the daemon-owned process registry
    // carries the live ring buffer across that request boundary.
    let next_request_id = format!("{request_id}-next");
    support::create_request_for_agent_with_signed_fields(
        db.node.as_ref(),
        db.node_identity.did(),
        &next_request_id,
        &session_id,
        "processing",
        "2026-05-19T00:00:01Z",
        None,
        None,
        None,
        None,
    )
    .await;
    let next_turn_hook = DefraSessionHook::resume_with_identity_policy(
        db.node.clone(),
        &session_id,
        "r6-background-theorem",
        db.node_identity.did(),
        None,
        FailurePolicy::default(),
    )
    .await
    .expect("resume next-turn hook")
    .with_background_execution_registry(shared_processes);
    next_turn_hook
        .set_active_request_lineage(Some(next_request_id), None)
        .await
        .expect("bind persisted request lineage");
    next_turn_hook
        .set_request_deadline_at(Some(chrono::Utc::now() + chrono::Duration::minutes(5)))
        .await;

    let next_turn_read = skip_reason_json(
        next_turn_hook
            .on_tool_call(
                "read_process",
                None,
                "read-dispatch-next-turn",
                &json!({ "tool_call_id": tool_call_id }).to_string(),
            )
            .await,
    );
    assert_eq!(next_turn_read["status"].as_str(), Some("running"));
    assert_eq!(
        next_turn_read["output"].as_str(),
        Some(running_payload.as_str()),
        "a later request must observe the originating request's live output"
    );
    assert_eq!(
        next_turn_read["total_bytes"].as_u64(),
        Some(*running_total_bytes)
    );

    let next_turn_list = skip_reason_json(
        next_turn_hook
            .on_tool_call("list_processes", None, "list-dispatch-next-turn", "{}")
            .await,
    );
    let listed = next_turn_list["entries"]
        .as_array()
        .expect("list_processes entries")
        .iter()
        .find(|entry| entry["tool_call_id"].as_str() == Some(tool_call_id.as_str()))
        .expect("running process listed on later request");
    assert_eq!(listed["stdout_bytes"].as_u64(), Some(*running_total_bytes));

    // Running + NO snapshot: a second hook on the same session has a fresh
    // (empty) live-output registry — exactly what a restarted daemon would
    // observe for this still-running row before recovery interrupts it.
    let restarted_hook = DefraSessionHook::resume_with_identity_policy(
        db.node.clone(),
        &session_id,
        "r6-background-theorem",
        db.node_identity.did(),
        None,
        FailurePolicy::default(),
    )
    .await
    .expect("resume restart-shaped hook");
    restarted_hook
        .set_active_request_lineage(Some(request_id.clone()), None)
        .await
        .expect("bind persisted request lineage");
    restarted_hook
        .set_request_deadline_at(Some(chrono::Utc::now() + chrono::Duration::minutes(5)))
        .await;
    let no_buffer = skip_reason_json(
        restarted_hook
            .on_tool_call(
                "read_process",
                None,
                "read-dispatch-no-buffer",
                &json!({ "tool_call_id": tool_call_id }).to_string(),
            )
            .await,
    );
    assert_eq!(
        no_buffer["status"].as_str(),
        Some("running"),
        "restart-shaped read must still observe the durable running row"
    );
    assert_eq!(
        no_buffer["output"].as_str(),
        Some(running_no_buffer_payload.as_str()),
        "a running row with no live snapshot must serve empty output"
    );
    assert_eq!(no_buffer["exited"].as_bool(), Some(false));

    // Terminal → persisted completion, from BOTH hooks: the durable result
    // does not depend on the volatile registry.
    let waited = skip_reason_json(
        hook.on_tool_call(
            "wait_process",
            None,
            "read-dispatch-wait",
            &json!({ "tool_call_id": tool_call_id }).to_string(),
        )
        .await,
    );
    assert_eq!(waited["status"].as_str(), Some("completed"));
    for (label, reader) in [
        ("live", &hook),
        ("next-turn", &next_turn_hook),
        ("restarted", &restarted_hook),
    ] {
        let terminal = skip_reason_json(
            reader
                .on_tool_call(
                    "read_process",
                    None,
                    &format!("read-dispatch-terminal-{label}"),
                    &json!({ "tool_call_id": tool_call_id }).to_string(),
                )
                .await,
        );
        assert_eq!(terminal["status"].as_str(), Some("completed"), "{label}");
        assert_eq!(
            terminal["output"].as_str(),
            Some(terminal_payload.as_str()),
            "{label}: terminal reads serve the persisted completion"
        );
        assert_eq!(
            terminal["total_bytes"].as_u64(),
            Some(*terminal_total_bytes),
            "{label}"
        );
        assert_eq!(terminal["exited"].as_bool(), Some(true), "{label}");
    }
}

/// Drives the Lean `bridge_step_cases` (#937) — outcomes computed by running
/// `Subagent.BridgedState.step` — through the production seams: child
/// terminals project through `project_background_subagent_completion` (the
/// chokepoint that owns the complete/failure guards) and cascade decisions
/// through `ToolCallLifecycle::bridge_cancel_cascade`.
pub(super) async fn generated_bridge_step_cases_drive_bridge_lifecycle() {
    let cases = lean_bridge_step_cases();
    assert_eq!(cases.len(), 10, "Lean bridge-step case family drifted");

    let mut driven = 0usize;
    let mut model_only = 0usize;
    for case in cases {
        match case.event.as_str() {
            "bridge_complete" | "bridge_failure" => {
                if !case.bridge_committed {
                    // Model-only guard: at this seam a persisted bridge row is
                    // committed by construction (`start_running` persisted it),
                    // so the uncommitted shape cannot be seeded. Pin its
                    // contract shape instead of silently skipping.
                    assert!(!case.legal, "{}", case.name);
                    assert_eq!(case.post_tool_state, None, "{}", case.name);
                    model_only += 1;
                    continue;
                }
                drive_bridge_step_projection_case(case).await;
                driven += 1;
            }
            "bridge_cancel_cascade" => {
                drive_bridge_step_cascade_case(case).await;
                driven += 1;
            }
            other => panic!("unhandled bridge step event {other}"),
        }
    }
    assert_eq!(driven, 9, "every seedable bridge-step row must be driven");
    assert_eq!(
        model_only, 1,
        "exactly the uncommitted-bridge row is model-only"
    );
}

async fn seed_bridge_step_fixture(
    case: &lean_vocab_test::LeanBridgeStepCase,
) -> (support::TestDb, ToolCallLifecycle, String, String, String) {
    let db = test_db(&format!("bridge-step-{}", case.name)).await;
    let agent_did = db.node_identity.did().to_string();
    let parent_request_id = format!("{}-parent", case.name);
    let parent_session_id = format!("{}-parent-session", case.name);
    let tool_call_id = format!("{}-tool", case.name);
    let child_request_id = format!("{}-child", case.name);

    support::fixtures::configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        BACKGROUND_THEOREM_PARENT_BEHAVIOR_ID,
        &format!("{}-bridge-tools", case.name),
        Vec::new(),
        false,
        false,
        None,
    )
    .await;
    create_background_theorem_parent_request(
        db.node.as_ref(),
        &parent_request_id,
        &parent_session_id,
        &agent_did,
        0,
        chrono::Utc::now() + chrono::Duration::minutes(5),
    )
    .await;
    if case.parent_state == "interrupted" {
        set_request_lifecycle_state_by_request_id(
            db.node.as_ref(),
            &parent_request_id,
            "interrupted",
        )
        .await;
    }

    let cancel_policy = match case.cancel_policy.as_str() {
        "cascade" => CancelPolicy::Cascade,
        "detach" => CancelPolicy::Detach,
        other => panic!("unhandled cancel policy {other}"),
    };
    let parent_request_doc_id =
        crate::support::exact_request_doc_id(db.node.as_ref(), &parent_request_id).await;
    let mut lifecycle = ToolCallLifecycle::new_subagent(
        db.node.clone(),
        parent_request_id.clone(),
        parent_session_id.clone(),
        agent_did.clone(),
        tool_call_id.clone(),
        1,
        "spawn_subagent".to_string(),
        "{}".to_string(),
        chrono::Utc::now() + chrono::Duration::minutes(5),
        AwaitMode::Background,
        cancel_policy,
        child_request_id.clone(),
        agent_did.clone(),
    )
    .with_request_doc_id(Some(parent_request_doc_id.clone()));
    lifecycle.start_running().await.unwrap();
    let parent_tool_call_doc_id = lifecycle.doc_id().expect("bridge document id").to_string();

    gents::tool_call_lifecycle::create_subagent_request_with_request_id(
        db.node.as_ref(),
        child_request_id.clone(),
        parent_request_id.clone(),
        parent_request_doc_id,
        tool_call_id.clone(),
        parent_tool_call_doc_id,
        0,
        agent_did,
        "bridge-step-child".to_string(),
        format!("prompt for {tool_call_id}"),
        Some(chrono::Utc::now() + chrono::Duration::minutes(4)),
    )
    .await
    .expect("create bridged child request");

    (
        db,
        lifecycle,
        tool_call_id,
        child_request_id,
        parent_session_id,
    )
}

async fn drive_bridge_step_projection_case(case: &lean_vocab_test::LeanBridgeStepCase) {
    use gents::background_completion::{
        project_background_subagent_completion, BackgroundCompletionOutcome,
    };

    let (db, _lifecycle, tool_call_id, child_request_id, _parent_session_id) =
        seed_bridge_step_fixture(case).await;
    let child_session_id = fetch_child_session_id(db.node.as_ref(), &child_request_id).await;

    match case.child_state.as_str() {
        "completed" => {
            persist_bridge_step_child_completion(
                db.node.as_ref(),
                &child_request_id,
                &child_session_id,
            )
            .await;
        }
        "processing" => {
            set_request_lifecycle_state_by_request_id(
                db.node.as_ref(),
                &child_request_id,
                "processing",
            )
            .await;
        }
        "interrupted" => {
            set_request_lifecycle_state_by_request_id(
                db.node.as_ref(),
                &child_request_id,
                "interrupted",
            )
            .await;
        }
        "failed" => {
            set_request_lifecycle_state_by_request_id(
                db.node.as_ref(),
                &child_request_id,
                "failed",
            )
            .await;
        }
        "dead" => {
            set_request_lifecycle_state_by_request_id(db.node.as_ref(), &child_request_id, "dead")
                .await;
        }
        other => panic!("unhandled child state {other}"),
    }

    let outcome = project_background_subagent_completion(
        db.node.clone(),
        &child_request_id,
        db.node_identity.did(),
    )
    .await
    .expect("project background completion");
    let row_state = fetch_bridge_step_tool_state(db.node.as_ref(), &tool_call_id).await;

    if case.legal {
        assert!(
            matches!(outcome, BackgroundCompletionOutcome::Projected { .. }),
            "{}: durable child terminal must project, got {outcome:?}",
            case.name
        );
        assert_eq!(
            row_state.as_deref(),
            case.post_tool_state.as_deref(),
            "{}: projected bridge state drifted from the Lean step",
            case.name
        );
    } else if case.child_state == "processing" {
        assert!(
            matches!(outcome, BackgroundCompletionOutcome::NotTerminal),
            "{}: a live child must not project, got {outcome:?}",
            case.name
        );
        assert_eq!(
            row_state.as_deref(),
            Some("running"),
            "{}: rejected step must leave the bridge running",
            case.name
        );
    } else {
        // bridge_failure with a completed child: the failure projection can
        // never fire — the projection dispatches on the actual durable
        // terminal, so the bridge completes instead of failing.
        assert_eq!(case.child_state, "completed", "{}", case.name);
        assert!(
            matches!(outcome, BackgroundCompletionOutcome::Projected { .. }),
            "{}: completed child projects completion, got {outcome:?}",
            case.name
        );
        assert_eq!(
            row_state.as_deref(),
            Some("completed"),
            "{}: a completed child must never project a failure state",
            case.name
        );
    }
}

async fn drive_bridge_step_cascade_case(case: &lean_vocab_test::LeanBridgeStepCase) {
    let (db, mut lifecycle, _tool_call_id, child_request_id, _parent_session_id) =
        seed_bridge_step_fixture(case).await;
    set_request_lifecycle_state_by_request_id(db.node.as_ref(), &child_request_id, "processing")
        .await;

    if case.parent_state == "processing" {
        // Rejected shape: the bridge is still running (and the parent live),
        // so the cascade decision is illegal at the Rust seam too.
        assert!(!case.legal, "{}", case.name);
        assert!(
            lifecycle.bridge_cancel_cascade().await.is_err(),
            "{}: cascade on a running bridge must be rejected",
            case.name
        );
        return;
    }

    lifecycle
        .cancel_during_run(CancelCause::Interrupted)
        .await
        .expect("cancel bridge before cascade decision");
    let intent = lifecycle
        .bridge_cancel_cascade()
        .await
        .expect("cascade decision");
    if case.post_child_interrupt_set {
        assert!(case.legal, "{}", case.name);
        let intent = intent.expect("cascade policy must produce a cascade intent");
        assert_eq!(
            intent.child_request_id, child_request_id,
            "{}: cascade intent must target the bridged child",
            case.name
        );
    } else {
        assert!(!case.legal, "{}", case.name);
        assert!(
            intent.is_none(),
            "{}: detach must not produce a cascade intent",
            case.name
        );
    }
}

async fn set_request_lifecycle_state_by_request_id(
    node: &EmbeddedNode,
    request_id: &str,
    lifecycle_state: &str,
) {
    let request_id = escape_graphql_string(request_id);
    let lifecycle_state = escape_graphql_string(lifecycle_state);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                input: {{ lifecycle_state: "{lifecycle_state}" }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "set request lifecycle_state failed: {:?}",
        response.errors
    );
}

async fn fetch_child_session_id(node: &EmbeddedNode, child_request_id: &str) -> String {
    #[derive(Deserialize)]
    struct SessionRow {
        session_id: String,
    }
    let child_request_id = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{child_request_id}" }} }},
                limit: 1
            ) {{ session_id }}
        }}"#
    );
    first_row::<SessionRow>(&node.execute(&query).await, "AgentRequest").session_id
}

async fn fetch_bridge_step_tool_state(node: &EmbeddedNode, tool_call_id: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct StateRow {
        lifecycle_state: Option<String>,
    }
    let tool_call_id = escape_graphql_string(tool_call_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ tool_call_id: {{ _eq: "{tool_call_id}" }} }},
                limit: 1
            ) {{ lifecycle_state }}
        }}"#
    );
    first_row::<StateRow>(&node.execute(&query).await, "AgentToolCall").lifecycle_state
}

async fn persist_bridge_step_child_completion(
    node: &EmbeddedNode,
    child_request_id: &str,
    child_session_id: &str,
) {
    set_request_lifecycle_state_by_request_id(node, child_request_id, "completed").await;

    let assistant = Message::Assistant {
        id: None,
        content: vec![AssistantContent::Text(Text {
            text: "bridge step child final".to_string(),
        })],
    };
    let escaped_message = escape_graphql_string(&serde_json::to_string(&assistant).unwrap());
    let escaped_child_session_id = escape_graphql_string(child_session_id);
    let escaped_child_request_id = escape_graphql_string(child_request_id);
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
        "create bridge-step child AgentMessage failed: {:?}",
        response.errors
    );

    let create_response = format!(
        r#"mutation {{
            create_AgentResponse(input: {{
                response_key: "{escaped_child_request_id}",
                request_id: "{escaped_child_request_id}",
                agent_did: "{AGENT_DID}",
                behavior_id: "bridge-step-child",
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
        "create bridge-step child AgentResponse failed: {:?}",
        response.errors
    );
}

async fn drive_r6_completion_owner_case(case: &lean_vocab_test::LeanR6BackgroundingCase) {
    use crate::support::{
        create_request_for_agent_with_signed_fields, create_session_document, session_document,
    };
    use gents::goal::{load_canonical_goal, set_goal, GoalStatus};
    let status = case.goal_status.as_deref().map(|status| match status {
        "active" => GoalStatus::Active,
        "paused" => GoalStatus::Paused,
        "blocked" => GoalStatus::Blocked,
        "usage_limited" => GoalStatus::UsageLimited,
        "budget_limited" => GoalStatus::BudgetLimited,
        "complete" => GoalStatus::Complete,
        other => panic!("unknown generated Goal status {other}"),
    });
    let lean_redrive = lean_r6_backgrounding_case("failed_background_wake_with_budget_redrives");
    assert_eq!(
        lean_redrive.post_parent_request_id, lean_redrive.redrive_source_request_id,
        "the modeled successor names its failed source as parent"
    );
    // Separate stores keep the historical failed-wake witness from coalescing
    // with the notification witness's deliberately pending no-Goal wake.
    for redrive in [false, true] {
        let db = test_db(&format!("{}-{redrive}", case.name)).await;
        let did = db.node_identity.did();
        let session = "completion-owner-session";
        let parent_id = format!(
            "completion-owner-parent-{}",
            lean_redrive.pre_parent_request_id.unwrap()
        );
        let parent = parent_id.as_str();
        let parent_doc = create_request_for_agent_with_signed_fields(
            &db.node,
            did,
            parent,
            session,
            "completed",
            "2026-07-15T00:00:00Z",
            None,
            None,
            None,
            None,
        )
        .await;
        if let Some(status) = status {
            set_goal(
                &db.node,
                did,
                session,
                Some("One continuation owner"),
                Some(status),
                Some(Some(10)),
            )
            .await
            .unwrap();
        }
        let before = load_canonical_goal(&db.node, did, session).await.unwrap();
        if redrive {
            // Map abstract Lean request IDs to concrete IDs/documents in this store.
            let failed_wake = format!(
                "failed-wake-{}",
                lean_redrive.redrive_source_request_id.unwrap()
            );
            let source_depth = lean_redrive.pre_depth.unwrap();
            let source_retry_count = lean_redrive.retry_count.unwrap();
            let max_retries = lean_redrive.max_retries.unwrap();
            let source_deadline = chrono::DateTime::from_timestamp(
                lean_redrive.pre_execution_deadline.unwrap() as i64,
                0,
            )
            .unwrap()
            .to_rfc3339();
            // Real historical scheduled wake fixture, matching the existing
            // failed-wake recovery test's persisted preconditions.
            let input = gents_protocol::request_input::RequestInput {
                queue: Some(gents_protocol::request_input::RequestQueue {
                    source: gents_protocol::request_input::QueueSource::BackgroundCompletion,
                    policy: gents_protocol::request_input::QueuePolicy::Coalesce,
                    key: Some(format!("background_completion:{session}")),
                    queued_after_request_id: Some(parent.to_owned()),
                    interrupted_request_id: None,
                    background_completion_wake_version: Some(1),
                }),
                ..Default::default()
            };
            let input = gents_protocol::graphql::graphql_input_literal(
                &serde_json::to_value(input).unwrap(),
            )
            .unwrap();
            let escaped_parent = escape_graphql_string(parent);
            let escaped_parent_doc = escape_graphql_string(&parent_doc);
            let escaped_failed_wake = escape_graphql_string(&failed_wake);
            let escaped_source_deadline = escape_graphql_string(&source_deadline);
            let response = db.node.execute(&format!(r#"mutation {{
                create_AgentRequest(input: {{ request_id: "{escaped_failed_wake}", agent_did: "{}",
                    behavior_id: "{}", session_id: "{session}", content: "background input",
                    input: {input}, execution_origin: "scheduled", lifecycle_state: "failed",
                    failure_reason: "backend admission failed", terminalized_at: "2026-07-15T00:00:00Z",
                    created_at: "2026-07-15T00:00:00Z", retry_count: {source_retry_count}, max_retries: {max_retries},
                    retry_root_request: "{escaped_failed_wake}", terminal_redrive_attempts: 0,
                    backend_id: "{}", subagent_depth: {source_depth}, deadline: "{escaped_source_deadline}",
                    caused_by_parent_request_id: "{escaped_parent}",
                    caused_by_parent_request_doc_id: "{escaped_parent_doc}"
                }}) {{ _docID }} }}"#, escape_graphql_string(did),
                crate::support::AGENT_NAME, crate::support::BACKEND_ID)).await;
            assert!(!response.has_errors(), "{:?}", response.errors);
            // Observation is deliberately absent: authoritative request rows,
            // not a conversation/cache head, must govern recovery eligibility.
            let mut session_doc =
                session_document(session, crate::support::AGENT_NAME, "2026-07-15T00:00:00Z");
            session_doc.agent_did = did.to_owned();
            create_session_document(&db.node, &session_doc).await;
            let first = gents::RequestLifecycle::redrive_failed_background_wakeups(&db.node, did)
                .await
                .unwrap();
            assert_eq!(
                first.redriven > 0,
                case.redrive_allowed.unwrap(),
                "{}",
                case.name
            );
            assert_eq!(first.failed, 0);
            let second = gents::RequestLifecycle::redrive_failed_background_wakeups(&db.node, did)
                .await
                .unwrap();
            assert_eq!(second.redriven, 0, "replay must not add another successor");
            let requests = db
                .node
                .execute(
                    "{ AgentRequest { _docID request_id retry_count retry_parent_request \
                     retry_parent_request_doc_id max_retries backend_id caused_by_parent_request_id \
                     caused_by_parent_request_doc_id subagent_depth deadline lifecycle_state } }",
                )
                .await;
            assert!(!requests.has_errors(), "{:?}", requests.errors);
            let data = requests.data.expect("request query data");
            let rows = data["AgentRequest"].as_array().unwrap();
            assert_eq!(rows.len(), 2 + usize::from(case.redrive_allowed.unwrap()));
            if case.redrive_allowed.unwrap() {
                // Runtime counterpart of the Lean redrive lineage fields: the
                // successor preserves the failed wake's subagent depth, takes
                // the failed wake as its retry parent, and creates no new
                // execution deadline. Retry counts come from the Lean
                // completion_redrive case used to seed this source.
                let source = rows
                    .iter()
                    .find(|row| row["request_id"] == failed_wake)
                    .expect("failed wake row");
                let successor = rows
                    .iter()
                    .find(|row| row["request_id"] != parent && row["request_id"] != failed_wake)
                    .expect("redrive successor row");
                assert_eq!(source["lifecycle_state"], "failed", "{}", case.name);
                assert_eq!(source["subagent_depth"].as_u64(), Some(source_depth as u64));
                assert_eq!(source["deadline"].as_str(), Some(source_deadline.as_str()));
                assert_eq!(source["caused_by_parent_request_id"], parent);
                assert_eq!(source["caused_by_parent_request_doc_id"], parent_doc);
                assert_eq!(
                    successor["caused_by_parent_request_id"].as_str(),
                    Some(failed_wake.as_str()),
                );
                assert_eq!(
                    successor["caused_by_parent_request_doc_id"]
                        .as_str()
                        .expect("causal parent docID"),
                    source["_docID"].as_str().expect("source docID"),
                );
                assert_eq!(
                    source["retry_count"].as_u64(),
                    lean_redrive.retry_count.map(|value| value as u64),
                    "{}",
                    case.name
                );
                assert_eq!(
                    successor["retry_parent_request"].as_str(),
                    Some(failed_wake.as_str()),
                    "{}: the successor's retry parent is the failed source request",
                    case.name
                );
                assert_eq!(
                    successor["retry_parent_request_doc_id"]
                        .as_str()
                        .expect("retry parent docID"),
                    source["_docID"].as_str().expect("source docID"),
                    "{}: the retry parent must link by document id",
                    case.name
                );
                assert_eq!(
                    successor["subagent_depth"].as_u64(),
                    lean_redrive.post_depth.map(|depth| depth as u64),
                    "{}: redrive preserves the failed source's subagent depth",
                    case.name
                );
                assert_eq!(
                    successor["retry_count"].as_u64(),
                    lean_redrive.post_retry_count.map(|value| value as u64),
                    "{}",
                    case.name
                );
                assert_eq!(
                    successor["max_retries"], max_retries,
                    "retry publication preserves the original ceiling"
                );
                assert!(
                    successor["backend_id"].is_null(),
                    "fresh claim resolves inference; failed backend is not carried forward"
                );
                assert!(
                    successor["deadline"].is_null(),
                    "{}: redrive must not mint a new execution deadline",
                    case.name
                );
            }
        } else {
            let mut tool = ToolCallLifecycle::new_background_tool(
                db.node.clone(),
                parent.into(),
                session.into(),
                did.into(),
                "native-tool".into(),
                1,
                "bash".into(),
                "{}".into(),
                chrono::Utc::now() + chrono::Duration::minutes(5),
            );
            tool.start_running().await.unwrap();
            assert!(tool
                .bridge_complete("durable native output".into())
                .await
                .unwrap());
            // Crash boundary: native execution is terminal, notification has
            // not been delivered. Only the existing delivery reconciler acts.
            let row = db
                .node
                .execute("{ AgentToolCall { started_at deadline_at completed_at } }")
                .await;
            assert!(!row.has_errors());
            let row = &row.data.as_ref().unwrap()["AgentToolCall"][0];
            let pending = db.node.execute(&format!(r#"mutation {{ update_AgentToolCall(
                filter: {{tool_call_id: {{_eq: "native-tool"}}}}, input: {{
                    status: "completionPending", started_at: "{}", deadline_at: "{}", completed_at: "{}"
                }}) {{_docID}} }}"#, row["started_at"].as_str().unwrap(),
                row["deadline_at"].as_str().unwrap(), row["completed_at"].as_str().unwrap())).await;
            assert!(!pending.has_errors(), "{:?}", pending.errors);
            let first =
                ToolCallLifecycle::reconcile_background_completion_side_effects(&db.node, did)
                    .await
                    .unwrap();
            assert_eq!(first.side_effects_converged, 1);
            let second =
                ToolCallLifecycle::reconcile_background_completion_side_effects(&db.node, did)
                    .await
                    .unwrap();
            assert!(second.is_noop());
            let observed = db.node.execute("{ AgentRequest { _docID request_id } AgentMessage { request_id request_doc_id content } AgentToolCall { status completion_notification_delivered_at } }").await;
            assert!(!observed.has_errors(), "{:?}", observed.errors);
            let data = observed.data.unwrap();
            let messages = data["AgentMessage"].as_array().unwrap();
            assert_eq!(!messages.is_empty(), case.notification_persisted.unwrap());
            assert_eq!(
                messages.len(),
                1,
                "notification replay must not duplicate input"
            );
            assert!(messages[0]["content"]
                .as_str()
                .unwrap()
                .contains("durable native output"));
            let requests = data["AgentRequest"].as_array().unwrap();
            assert_eq!(requests.len(), 1 + usize::from(case.wake_created.unwrap()));
            let consumer = if case.wake_created.unwrap() {
                requests
                    .iter()
                    .find(|row| row["request_id"] != parent)
                    .unwrap()
            } else {
                requests
                    .iter()
                    .find(|row| row["_docID"] == parent_doc)
                    .unwrap()
            };
            assert_eq!(messages[0]["request_id"], consumer["request_id"]);
            assert_eq!(messages[0]["request_doc_id"], consumer["_docID"]);
            assert_eq!(data["AgentToolCall"][0]["status"], "completed");
            assert!(data["AgentToolCall"][0]["completion_notification_delivered_at"].is_string());
        }
        let after = load_canonical_goal(&db.node, did, session).await.unwrap();
        assert_eq!(
            serde_json::to_value(before).unwrap(),
            serde_json::to_value(after).unwrap(),
            "{}: delivery/redrive may not change Goal state or budget",
            case.name
        );
    }
}

// Same session and requester, different runtime principal. This checks the
// process-control owner, not transport authentication or DefraDB ACP.
#[tokio::test]
async fn cross_agent_process_controls_preserve_the_owners_running_job() {
    let (db, owner, session_id, request_id) = setup_background_tool_hook(
        "r6-cross-agent-controls",
        background_tool_registry(vec![Box::new(PendingTool)], &["slow_tool"]),
    )
    .await;
    let requester = "did:key:process-owner";
    owner
        .set_active_request_lineage(Some(request_id.clone()), Some(requester.to_string()))
        .await
        .unwrap();
    let receipt = skip_reason_json(
        owner
            .on_tool_call(
                "spawn_process",
                None,
                "cross-agent-spawn",
                r#"{"tool_name":"slow_tool","args":{}}"#,
            )
            .await,
    );
    let tool_call_id = receipt["tool_call_id"].as_str().expect("spawned tool ID");
    let args = json!({ "tool_call_id": tool_call_id }).to_string();
    let foreign_did = "did:key:foreign-process-agent";
    assert_ne!(db.node_identity.did(), foreign_did);
    let foreign = DefraSessionHook::resume_with_identity_policy(
        db.node.clone(),
        &session_id,
        "r6-background-theorem",
        foreign_did,
        None,
        FailurePolicy::default(),
    )
    .await
    .unwrap();
    foreign
        .set_active_request_lineage(Some(request_id), Some(requester.to_string()))
        .await
        .unwrap();
    foreign
        .set_request_deadline_at(Some(chrono::Utc::now() + chrono::Duration::minutes(5)))
        .await;

    // A real row must be visible to its owner before checking the foreign view.
    for (label, caller, visible) in [("owner", &owner, true), ("foreign", &foreign, false)] {
        let listed = skip_reason_json(
            caller
                .on_tool_call("list_processes", None, &format!("{label}-list"), "{}")
                .await,
        );
        assert_eq!(
            listed["entries"]
                .as_array()
                .expect("entries")
                .iter()
                .any(|row| row["tool_call_id"].as_str() == Some(tool_call_id)),
            visible,
            "{label}"
        );
        let read = skip_reason_json(
            caller
                .on_tool_call("read_process", None, &format!("{label}-read"), &args)
                .await,
        );
        if visible {
            assert_eq!(read["status"].as_str(), Some("running"));
        } else {
            assert_eq!(read["ok"].as_bool(), Some(false));
            assert_eq!(read["failure_class"].as_str(), Some("tool_not_allowed"));
        }
    }
    for name in ["wait_process", "cancel_process"] {
        let denied = skip_reason_json(
            foreign
                .on_tool_call(name, None, &format!("foreign-{name}"), &args)
                .await,
        );
        assert_eq!(denied["ok"].as_bool(), Some(false), "{name}: {denied}");
        assert!(denied["message"].as_str().is_some_and(|message|
            message.contains("not manageable by this session principal")),
            "{name}: unexpected denial {denied}");
    }
    let executions = gents::BackgroundExecutionRegistry::default();
    let denied = gents::tool_control::cancel_session_background_process(
        db.node.clone(),
        &executions,
        foreign_did,
        Some(requester),
        &session_id,
        tool_call_id,
    )
    .await
    .unwrap();
    assert!(matches!(
        denied,
        gents::CancelBackgroundToolCallOutcome::NotFound
    ));
    let row = fetch_background_theorem_tool_call(db.node.as_ref(), &session_id, tool_call_id).await;
    assert_eq!(row.lifecycle_state.as_deref(), Some("running"));
    assert_eq!(row.cancel_cause.as_deref(), None);

    // The owner can still cancel after every denied attempt; also release the worker.
    let cancelled = skip_reason_json(
        owner
            .on_tool_call("cancel_process", None, "owner-cancel", &args)
            .await,
    );
    assert_eq!(cancelled["status"].as_str(), Some("cancelled"));
}
