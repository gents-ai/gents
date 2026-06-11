//! Background conformance home: R6 tool backgrounding, background-theorem
//! witnesses (admission budget, cascade cancellation), subagent delegation
//! graph, and R4c background-work observable shapes.

use super::*;

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
    cancel_cause: Option<String>,
    cancel_cascade_intent_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BackgroundedRow {
    lifecycle_state: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct BackgroundTheoremChildRequestRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_id: String,
    agent_did: String,
    behavior_id: Option<String>,
    session_id: String,
    content: String,
    temperature: Option<f64>,
    top_p: Option<f64>,
    top_k: Option<i64>,
    max_tokens: Option<i64>,
    metadata: Option<String>,
    execution_origin: Option<String>,
    created_at: String,
    deadline: Option<String>,
    subagent_depth: Option<u32>,
    caused_by_parent_request_id: Option<String>,
    caused_by_parent_tool_call_id: Option<String>,
    status: String,
    lifecycle_state: Option<String>,
}

impl BackgroundTheoremChildRequestRow {
    fn into_agent_request(self) -> defra_agent::watcher::AgentRequest {
        defra_agent::watcher::AgentRequest {
            doc_id: self.doc_id,
            request_id: self.request_id,
            agent_did: self.agent_did,
            behavior_id: self
                .behavior_id
                .and_then(|value| (!value.trim().is_empty()).then_some(value)),
            session_id: self.session_id,
            content: self.content,
            temperature: self.temperature,
            top_p: self.top_p,
            top_k: self.top_k,
            max_tokens: self.max_tokens,
            metadata: self.metadata,
            execution_origin: self
                .execution_origin
                .and_then(|value| (!value.trim().is_empty()).then_some(value)),
            created_at: self.created_at,
            deadline: self
                .deadline
                .and_then(|value| (!value.trim().is_empty()).then_some(value)),
            subagent_depth: self.subagent_depth.unwrap_or(0),
            caused_by_parent_request_id: self.caused_by_parent_request_id,
            caused_by_parent_tool_call_id: self.caused_by_parent_tool_call_id,
        }
    }
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
    let session_id = format!("{test_name}-session");
    let request_id = format!("{test_name}-request");
    support::create_request(
        db.node.as_ref(),
        &request_id,
        &session_id,
        "processing",
        "2026-05-19T00:00:00Z",
    )
    .await;

    let hook = DefraSessionHook::resume_or_create_with_identity_policy(
        db.node.clone(),
        &session_id,
        "r6-background-theorem",
        AGENT_DID,
        FailurePolicy::default(),
    )
    .await
    .expect("resume background theorem hook")
    .with_background_tool_registry(registry);
    hook.set_active_request_id(Some(request_id.clone())).await;
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
    let parent_deadline = chrono::Utc::now() + chrono::Duration::minutes(5);
    let selection_id = format!("{test_name}-tools");

    upsert_tool_selection(
        db.node.as_ref(),
        &ToolSelectionDocument {
            selection_id: selection_id.clone(),
            agent_did: AGENT_DID.to_string(),
            subagent_targets: Some(
                targets
                    .into_iter()
                    .map(|behavior_id| {
                        defra_agent::subagent_target_entry(
                            behavior_id,
                            AGENT_DID,
                            behavior_id,
                            None,
                        )
                    })
                    .collect(),
            ),
            subagent_spawn_enabled: Some(true),
            subagent_background_enabled: Some(background_enabled),
            ..Default::default()
        },
    )
    .await
    .expect("upsert theorem tool selection");
    upsert_agent_behavior(
        db.node.as_ref(),
        &AgentBehaviorDocument {
            behavior_id: BACKGROUND_THEOREM_PARENT_BEHAVIOR_ID.to_string(),
            agent_did: AGENT_DID.to_string(),
            display_name: Some("R6 theorem parent".to_string()),
            description: None,
            summary: None,
            system_prompt: None,
            backend_id: None,
            model_name: None,
            tool_selection_id: Some(selection_id),
            inference_profile_id: None,
            compaction_strategy: None,
            compaction_threshold: None,
            skill_refs: Vec::new(),
            skill_excludes: Vec::new(),
            enabled: true,
            created_at: Some("2026-05-19T00:00:00Z".to_string()),
        },
    )
    .await
    .expect("upsert theorem parent behavior");
    upsert_agent_behavior(
        db.node.as_ref(),
        &AgentBehaviorDocument {
            behavior_id: BACKGROUND_THEOREM_CHILD_BEHAVIOR_ID.to_string(),
            agent_did: AGENT_DID.to_string(),
            display_name: Some("R6 theorem child".to_string()),
            description: None,
            summary: None,
            system_prompt: None,
            backend_id: None,
            model_name: None,
            tool_selection_id: None,
            inference_profile_id: None,
            compaction_strategy: None,
            compaction_threshold: None,
            skill_refs: Vec::new(),
            skill_excludes: Vec::new(),
            enabled: true,
            created_at: Some("2026-05-19T00:00:01Z".to_string()),
        },
    )
    .await
    .expect("upsert theorem child behavior");

    let session_id = format!("{test_name}-session");
    let request_id = format!("{test_name}-parent");
    create_background_theorem_parent_request(
        db.node.as_ref(),
        &request_id,
        &session_id,
        parent_subagent_depth,
        parent_deadline,
    )
    .await;

    let hook = DefraSessionHook::resume_or_create_with_identity_policy(
        db.node.clone(),
        &session_id,
        BACKGROUND_THEOREM_PARENT_BEHAVIOR_ID,
        AGENT_DID,
        FailurePolicy::default(),
    )
    .await
    .expect("resume background theorem parent hook");
    hook.set_active_request_id(Some(request_id.clone())).await;
    hook.set_request_deadline_at(Some(parent_deadline)).await;

    (db, hook, session_id, request_id, parent_deadline)
}

async fn create_background_theorem_parent_request(
    node: &EmbeddedNode,
    request_id: &str,
    session_id: &str,
    subagent_depth: u32,
    deadline: chrono::DateTime<chrono::Utc>,
) {
    let request_id = escape_graphql_string(request_id);
    let session_id = escape_graphql_string(session_id);
    let behavior_id = escape_graphql_string(BACKGROUND_THEOREM_PARENT_BEHAVIOR_ID);
    let agent_did = escape_graphql_string(AGENT_DID);
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
                status: "processing",
                lifecycle_state: "processing",
                backend_id: "",
                execution_origin: "interactive",
                metadata: "",
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

async fn fetch_background_theorem_child_request(
    node: &EmbeddedNode,
    child_request_id: &str,
) -> BackgroundTheoremChildRequestRow {
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
                behavior_id
                session_id
                content
                temperature
                top_p
                top_k
                max_tokens
                metadata
                execution_origin
                created_at
                deadline
                subagent_depth
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
                status
                lifecycle_state
            }}
        }}"#
    );
    first_row(&node.execute(&query).await, "AgentRequest")
}

async fn fetch_background_theorem_child_request_optional(
    node: &EmbeddedNode,
    child_request_id: &str,
) -> Option<BackgroundTheoremChildRequestRow> {
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
                behavior_id
                session_id
                content
                temperature
                top_p
                top_k
                max_tokens
                metadata
                execution_origin
                created_at
                deadline
                subagent_depth
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
                status
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
) -> BackgroundTheoremChildRequestRow {
    let timeout_at = tokio::time::Instant::now() + Duration::from_secs(10);

    loop {
        if let Some(row) =
            fetch_background_theorem_child_request_optional(node, child_request_id).await
        {
            if row.lifecycle_state.as_deref() == Some(expected_state) {
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

pub(super) fn generated_r6_backgrounding_cases_pin_tool_backgrounding_contract() {
    let cases = lean_r6_backgrounding_cases();
    assert_eq!(cases.len(), 7);

    let names = cases
        .iter()
        .map(|case| case.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names,
        [
            "background_tool_budget_count_7_admits_spawn",
            "background_tool_budget_count_8_rejects_spawn",
            "tool_kind_bridge_complete_persists_result",
            "tool_kind_bridge_failure_cancelled_projects_parent_cancelled",
            "background_recovery_running_live_parent_to_cancelled",
            "background_completion_source_writes_canonical_key",
            "legacy_subagent_completion_source_aliases_canonical_key",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>()
    );

    for case in cases {
        assert_eq!(case.max_backgrounded, 8, "{}", case.name);
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
        lean_r6_backgrounding_case("tool_kind_bridge_failure_cancelled_projects_parent_cancelled");
    assert_eq!(cancelled.terminal_state.as_str(), "cancelled");
    assert_eq!(cancelled.reason.as_deref(), Some("parent_cancelled"));

    let recovered =
        lean_r6_backgrounding_case("background_recovery_running_live_parent_to_cancelled");
    assert_eq!(
        recovered.action.as_str(),
        "TerminalizeBackgroundedAsInterrupted"
    );
    assert_eq!(recovered.terminal_state.as_str(), "cancelled");
    assert_eq!(recovered.reason.as_deref(), Some("interrupted_on_restart"));

    let canonical = lean_r6_backgrounding_case("background_completion_source_writes_canonical_key");
    assert_eq!(
        canonical.queue_source.as_deref(),
        Some("background_completion")
    );
    assert_eq!(
        canonical.queue_key.as_deref(),
        Some("background_completion:900")
    );

    let legacy =
        lean_r6_backgrounding_case("legacy_subagent_completion_source_aliases_canonical_key");
    assert_eq!(legacy.queue_source.as_deref(), Some("subagent_completion"));
    assert_eq!(legacy.queue_key.as_deref(), canonical.queue_key.as_deref());
}

pub(super) async fn generated_r6_background_theorem_witnesses_drive_admission_budget_invariant() {
    let witnesses = lean_r6_background_theorem_witnesses();
    assert_eq!(witnesses.len(), 2);

    let witness =
        lean_r6_background_theorem_witness("Subagent.BridgedState.backgrounded_budget_bounded");
    assert_eq!(witness.witness_kind.as_str(), "state_invariant");
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
    // After spawn convergence (#377) the child AgentRequest is materialized by
    // SubagentSource, not synchronously by the hook.  Hold a standalone source
    // for the lifetime of this test so the bridge row produces a child request.
    let _source = super::support::fixtures::spawn_subagent_source(
        db.node.clone(),
        AGENT_DID,
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
    assert_eq!(child.lifecycle_state.as_deref(), Some("pending"));
    let mut child_lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        BACKGROUND_THEOREM_CHILD_BEHAVIOR_ID,
        AGENT_DID,
        child.into_agent_request(),
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    assert_eq!(
        child_lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    child_lifecycle.begin_execution().await.unwrap();
    let child_pre =
        fetch_background_theorem_child_request(db.node.as_ref(), &child_request_id).await;
    assert_eq!(child_pre.status.as_str(), "processing");
    assert_eq!(
        child_pre.lifecycle_state.as_deref(),
        Some(witness.kind_field("child_pre_state"))
    );

    let mut lifecycle =
        ToolCallLifecycle::load(db.node.clone(), &session_id, "internal-theorem-cascade")
            .await
            .expect("load bridge lifecycle")
            .expect("bridge should be persisted");
    let dispatch = lifecycle
        .cancel_during_run_with_cascade_dispatch(CancelCause::Interrupted, AGENT_DID)
        .await
        .expect("cancel bridge with cascade dispatch")
        .expect("cascade dispatch");
    let CascadeDispatch::Local(intent) = dispatch else {
        panic!("local child must use local cascade dispatch");
    };
    assert_eq!(intent.child_request_id, child_request_id);

    interrupt_request(db.node.as_ref(), &intent.child_request_id)
        .await
        .expect("interrupt child request");
    // This isolated consumer has no daemon observer running, so explicitly
    // drive the same request-lifecycle interrupt arm used by the daemon.
    child_lifecycle
        .transition_to_interrupted()
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
    assert_eq!(child_post.status.as_str(), child_post_state_expected);
    assert_eq!(
        child_post.lifecycle_state.as_deref(),
        Some(child_post_state_expected)
    );
    let child_interrupt = fetch_interrupt_requested_at(db.node.as_ref(), &child_post.request_id)
        .await
        .expect("fetch child interrupt_requested_at");
    assert!(
        child_interrupt.is_some(),
        "cascade trace must preserve child interrupt_requested_at through {child_post_state_expected}"
    );
}

pub(super) fn generated_subagent_delegation_graph_cases_pin_gap2_contract() {
    let cases = lean_subagent_delegation_graph_cases();
    assert_eq!(
        cases.len(),
        3,
        "Lean should emit termination, acyclicity, and cascade graph witnesses"
    );

    let by_property = cases
        .iter()
        .map(|case| (case.property.as_str(), case))
        .collect::<HashMap<_, _>>();
    for property in ["termination", "acyclicity", "cascade_cancel"] {
        assert!(
            by_property.contains_key(property),
            "missing subagent delegation graph property {property}"
        );
    }

    for case in cases {
        assert_eq!(
            case.max_depth,
            usize::try_from(MAX_SUBAGENT_DEPTH).expect("MAX_SUBAGENT_DEPTH fits usize"),
            "Lean maxSubagentDepth drifted from Rust MAX_SUBAGENT_DEPTH"
        );
        assert!(
            case.path_length <= case.max_depth,
            "case {} exceeds the generated depth bound",
            case.name
        );
        assert!(case.acyclic, "case {} must assert acyclicity", case.name);
        assert!(case.bounded, "case {} must assert bounded paths", case.name);
        assert!(
            !case.theorem_name.trim().is_empty(),
            "case {} must cite a Lean theorem",
            case.name
        );
        assert!(
            case.edge_theorem.starts_with("Subagent.DelegationGraph."),
            "case {} must cite a graph edge/path theorem",
            case.name
        );
        if case.cascade_path {
            assert!(
                case.cascade_covered,
                "cascade graph case {} must assert edge interrupt coverage",
                case.name
            );
            assert_eq!(
                case.cascade_edge_theorem.as_deref(),
                Some("Subagent.BridgedState.cascade_cancels_child")
            );
        } else {
            assert!(!case.cascade_covered);
            assert!(case.cascade_edge_theorem.is_none());
        }
    }

    let termination = by_property["termination"];
    assert_eq!(
        termination.theorem_name.as_str(),
        "Subagent.DelegationGraph.delegation_path_length_bounded"
    );
    assert_eq!(
        termination.witness_kind.as_str(),
        "arbitrary_delegation_path"
    );
    assert_eq!(termination.parent_depth, 0);
    assert_eq!(termination.terminal_depth, termination.max_depth);

    let acyclicity = by_property["acyclicity"];
    assert_eq!(
        acyclicity.theorem_name.as_str(),
        "Subagent.DelegationGraph.delegation_paths_acyclic"
    );
    assert_eq!(
        acyclicity.edge_theorem.as_str(),
        "Subagent.DelegationGraph.no_self_delegation_edge"
    );

    let cascade = by_property["cascade_cancel"];
    assert_eq!(
        cascade.theorem_name.as_str(),
        "Subagent.DelegationGraph.cascade_cancel_covers_path"
    );
    assert_eq!(cascade.witness_kind.as_str(), "arbitrary_cascade_path");
}

pub(super) fn generated_r4c_background_work_cases_pin_observable_shapes() {
    let cases = lean_r4c_background_work_cases();
    assert_eq!(cases.len(), 6);

    let names = cases
        .iter()
        .map(LeanR4cBackgroundWorkCase::witness)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names,
        [
            "r4c.list_subagents.lineage_rejects",
            "r4c.read_subagent_transcript.cursor_advances",
            "r4c.read_subagent_transcript.hides_bridge_rows",
            "r4c.read_tool_output.dispatch_by_state",
            "r4c.steer_subagent.append_preserves_lineage",
            "r4c.steer_subagent.interrupt_composes",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>()
    );

    match lean_r4c_background_work_case("r4c.list_subagents.lineage_rejects") {
        LeanR4cBackgroundWorkCase::ListSubagentsLineageRejects {
            caller_request_id,
            sibling_request_id,
            sibling_child_id,
            caller_sees_sibling_child,
        } => {
            assert_eq!(caller_request_id, "r4c-w1-caller");
            assert_eq!(sibling_request_id, "r4c-w1-sibling");
            assert_eq!(sibling_child_id, "r4c-w1-sibling-child");
            assert!(!*caller_sees_sibling_child);
        }
        other => panic!("unexpected R4c witness variant: {other:?}"),
    }

    match lean_r4c_background_work_case("r4c.read_subagent_transcript.cursor_advances") {
        LeanR4cBackgroundWorkCase::ReadTranscriptCursorAdvances {
            child_session_id,
            first_since_sequence,
            first_through_sequence,
            first_next_sequence,
            second_since_sequence,
            second_through_sequence,
            no_gap,
            no_overlap,
        } => {
            assert_eq!(child_session_id, "r4c-w2-session");
            assert_eq!(*first_since_sequence, 0);
            assert_eq!(*first_through_sequence, 5);
            assert_eq!(*first_next_sequence, 6);
            assert_eq!(*second_since_sequence, 6);
            assert_eq!(*second_through_sequence, 10);
            assert_eq!(first_next_sequence, second_since_sequence);
            assert!(*no_gap);
            assert!(*no_overlap);
        }
        other => panic!("unexpected R4c witness variant: {other:?}"),
    }

    match lean_r4c_background_work_case("r4c.read_subagent_transcript.hides_bridge_rows") {
        LeanR4cBackgroundWorkCase::ReadTranscriptHidesBridgeRows {
            child_session_id,
            bridge_call_id,
            rendered_transcript,
        } => {
            assert_eq!(child_session_id, "r4c-w3-session");
            assert_eq!(bridge_call_id, "r4c-w3-bridge-call");
            assert_eq!(
                rendered_transcript,
                "[assistant seq=2]\nplain assistant message\n"
            );
            assert!(
                !rendered_transcript.contains(bridge_call_id),
                "rendered transcript must hide bridge tool-call rows"
            );
        }
        other => panic!("unexpected R4c witness variant: {other:?}"),
    }

    match lean_r4c_background_work_case("r4c.read_tool_output.dispatch_by_state") {
        LeanR4cBackgroundWorkCase::ReadToolOutputDispatchesByState {
            tool_call_id,
            running_source,
            terminal_source,
            running_payload,
            stale_running_payload,
            terminal_payload,
        } => {
            assert_eq!(tool_call_id, "r4c-w4-tool-call");
            // Shipped behavior (defra-agent#403): no live ring buffer was built,
            // so a running read has no live source and returns empty output; the
            // persisted completion is the only non-empty source, served at
            // terminal. This mirrors `background_tools.rs` and is independently
            // pinned by `read_tool_output_running_returns_empty_live_stream_without_ring_buffer`.
            assert_eq!(running_source, "none");
            assert_eq!(terminal_source, "persisted_tool_completion");
            assert_eq!(running_payload, "");
            assert_eq!(stale_running_payload, "");
            assert_eq!(terminal_payload, "persisted-completion-stdout");
            assert!(
                running_payload.is_empty(),
                "running reads must return empty output: no live ring buffer is maintained"
            );
            assert_ne!(
                terminal_payload, running_payload,
                "terminal reads serve the persisted result, never the (empty) running payload"
            );
        }
        other => panic!("unexpected R4c witness variant: {other:?}"),
    }

    match lean_r4c_background_work_case("r4c.steer_subagent.append_preserves_lineage") {
        LeanR4cBackgroundWorkCase::SteerAppendPreservesLineage {
            caller_request_id,
            child_session_id,
            queued_request_id,
            caused_by_parent_request_id,
            queue_source,
            queue_policy,
        } => {
            assert_eq!(caller_request_id, "r4c-w5-caller");
            assert_eq!(child_session_id, "r4c-w5-child-session");
            assert_eq!(queued_request_id, "r4c-w5-queued");
            assert_eq!(caused_by_parent_request_id, caller_request_id);
            assert_eq!(queue_source, "steering");
            assert_eq!(queue_policy, "append");
        }
        other => panic!("unexpected R4c witness variant: {other:?}"),
    }

    match lean_r4c_background_work_case("r4c.steer_subagent.interrupt_composes") {
        LeanR4cBackgroundWorkCase::SteerInterruptComposes {
            caller_request_id,
            child_session_id,
            interrupted_active_request_id,
            drained_wake_up_request_ids,
            drained_wake_up_queue_key,
            queued_request_id,
            queue_interrupted_request_id,
        } => {
            assert_eq!(caller_request_id, "r4c-w6-caller");
            assert_eq!(child_session_id, "r4c-w6-child-session");
            assert_eq!(interrupted_active_request_id, "r4c-w6-interrupted");
            assert_eq!(
                drained_wake_up_request_ids,
                &vec!["r4c-w6-wake-1".to_string(), "r4c-w6-wake-2".to_string()]
            );
            assert_eq!(
                drained_wake_up_queue_key,
                "background_completion:r4c-w6-child-session"
            );
            assert_eq!(
                drained_wake_up_queue_key,
                &format!("background_completion:{child_session_id}")
            );
            assert_eq!(queued_request_id, "r4c-w6-queued");
            assert_eq!(queue_interrupted_request_id, interrupted_active_request_id);
        }
        other => panic!("unexpected R4c witness variant: {other:?}"),
    }
}
