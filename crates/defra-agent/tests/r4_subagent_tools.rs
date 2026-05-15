//! R4 agent-facing subagent tool integration tests.

mod support;

use std::time::Duration;

use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::tool_call_lifecycle::{
    create_subagent_request_with_request_id, AwaitMode, CancelPolicy, CascadeDispatch,
    ToolCallLifecycle, MAX_SUBAGENT_DEPTH,
};
use defra_agent::{
    fetch_interrupt_requested_at, interrupt_request, load_history, upsert_agent_behavior,
    upsert_tool_selection, AgentBehavior, DefraSessionHook, FailurePolicy, ToolSelectionDocument,
};
use rig::agent::{PromptHook, ToolCallHookAction};
use rig::completion::message::{
    AssistantContent, Message, Text, ToolCall, ToolFunction, ToolResult, ToolResultContent,
    UserContent,
};
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse};
use rig::one_or_many::OneOrMany;
use rig::streaming::StreamingCompletionResponse;
use serde::Deserialize;
use serde_json::{json, Value};

use support::{first_optional_row, first_row, test_db};

const AGENT_DID: &str = "did:defra-agent:r4-subagent-tools";
const PARENT_BEHAVIOR_ID: &str = "r4-parent";
const CHILD_BEHAVIOR_ID: &str = "r4-child";

#[derive(Clone, Default)]
struct TestModel;

#[allow(refining_impl_trait)]
impl CompletionModel for TestModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &Self::Client, _: impl Into<String>) -> Self {
        Self
    }

    async fn completion(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        Err(CompletionError::ProviderError(
            "completion is unused in R4 subagent tool tests".to_string(),
        ))
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        Err(CompletionError::ProviderError(
            "streaming is unused in R4 subagent tool tests".to_string(),
        ))
    }
}

#[derive(Debug, Deserialize)]
struct ToolCallRow {
    request_id: Option<String>,
    tool_name: Option<String>,
    args: Option<String>,
    result: Option<String>,
    lifecycle_state: Option<String>,
    await_mode: Option<String>,
    cancel_policy: Option<String>,
    child_request_id: Option<String>,
    unclaimed_deadline_at: Option<String>,
    cancel_cascade_intent_at: Option<String>,
    cancel_pending_remote_ack: Option<bool>,
    #[allow(dead_code)]
    stuck_since: Option<String>,
    tool_failure_class: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChildRequestRow {
    request_id: String,
    session_id: String,
    behavior_id: String,
    content: String,
    status: Option<String>,
    lifecycle_state: Option<String>,
    failure_reason: Option<String>,
    subagent_depth: Option<i64>,
    deadline: Option<String>,
    caused_by_parent_request_id: Option<String>,
    caused_by_parent_tool_call_id: Option<String>,
    caused_by_trigger_id: Option<String>,
    caused_by_trigger_kind: Option<String>,
}

async fn setup_spawn_fixture(
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
    setup_spawn_fixture_with_flags(
        test_name,
        targets,
        parent_subagent_depth,
        true,
        background_enabled,
    )
    .await
}

async fn setup_spawn_fixture_with_flags(
    test_name: &str,
    targets: Vec<&str>,
    parent_subagent_depth: u32,
    spawn_enabled: bool,
    background_enabled: bool,
) -> (
    support::TestDb,
    DefraSessionHook,
    String,
    String,
    chrono::DateTime<chrono::Utc>,
) {
    setup_spawn_fixture_with_flags_and_deadline(
        test_name,
        targets,
        parent_subagent_depth,
        spawn_enabled,
        background_enabled,
        chrono::Utc::now() + chrono::Duration::minutes(5),
    )
    .await
}

async fn setup_spawn_fixture_with_flags_and_deadline(
    test_name: &str,
    targets: Vec<&str>,
    parent_subagent_depth: u32,
    spawn_enabled: bool,
    background_enabled: bool,
    parent_deadline: chrono::DateTime<chrono::Utc>,
) -> (
    support::TestDb,
    DefraSessionHook,
    String,
    String,
    chrono::DateTime<chrono::Utc>,
) {
    let db = test_db(test_name).await;
    upsert_tool_selection(
        db.node.as_ref(),
        &ToolSelectionDocument {
            selection_id: "r4-parent-tools".to_string(),
            agent_did: AGENT_DID.to_string(),
            subagent_targets: Some(targets.into_iter().map(str::to_string).collect()),
            subagent_spawn_enabled: Some(spawn_enabled),
            subagent_background_enabled: Some(background_enabled),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    upsert_agent_behavior(
        db.node.as_ref(),
        &AgentBehavior {
            behavior_id: PARENT_BEHAVIOR_ID.to_string(),
            agent_did: AGENT_DID.to_string(),
            display_name: Some("R4 parent".to_string()),
            system_prompt: None,
            backend_id: None,
            model_name: None,
            tool_selection_id: Some("r4-parent-tools".to_string()),
            inference_profile_id: None,
            compaction_strategy: None,
            compaction_threshold: None,
            enabled: true,
            created_at: Some("2026-05-12T00:00:00Z".to_string()),
        },
    )
    .await
    .unwrap();
    upsert_agent_behavior(
        db.node.as_ref(),
        &AgentBehavior {
            behavior_id: CHILD_BEHAVIOR_ID.to_string(),
            agent_did: AGENT_DID.to_string(),
            display_name: Some("R4 child".to_string()),
            system_prompt: None,
            backend_id: None,
            model_name: None,
            tool_selection_id: None,
            inference_profile_id: None,
            compaction_strategy: None,
            compaction_threshold: None,
            enabled: true,
            created_at: Some("2026-05-12T00:00:01Z".to_string()),
        },
    )
    .await
    .unwrap();

    let session_id = format!("{test_name}-session");
    let request_id = format!("{test_name}-parent");
    create_parent_request(
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
        PARENT_BEHAVIOR_ID,
        AGENT_DID,
        FailurePolicy::default(),
    )
    .await
    .unwrap();
    hook.set_active_request_id(Some(request_id.clone())).await;
    hook.set_request_deadline_at(Some(parent_deadline)).await;

    (db, hook, session_id, request_id, parent_deadline)
}

async fn create_parent_request(
    node: &EmbeddedNode,
    request_id: &str,
    session_id: &str,
    subagent_depth: u32,
    deadline: chrono::DateTime<chrono::Utc>,
) {
    let request_id = escape_graphql_string(request_id);
    let session_id = escape_graphql_string(session_id);
    let behavior_id = escape_graphql_string(PARENT_BEHAVIOR_ID);
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
        "create parent AgentRequest failed: {:?}",
        response.errors
    );
}

async fn fetch_tool_call(node: &EmbeddedNode, session_id: &str, tool_call_id: &str) -> ToolCallRow {
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_tool_call_id = escape_graphql_string(tool_call_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }},
                    tool_call_id: {{ _eq: "{escaped_tool_call_id}" }}
                }},
                limit: 1
            ) {{
                request_id
                tool_name
                args
                result
                lifecycle_state
                await_mode
                cancel_policy
                child_request_id
                unclaimed_deadline_at
                cancel_cascade_intent_at
                cancel_pending_remote_ack
                stuck_since
                tool_failure_class
            }}
        }}"#
    );
    first_row(&node.execute(&query).await, "AgentToolCall")
}

async fn wait_for_tool_call_await_mode(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
    expected_await_mode: &str,
) -> ToolCallRow {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let row = fetch_tool_call(node, session_id, tool_call_id).await;
        if row.await_mode.as_deref() == Some(expected_await_mode) {
            return row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for tool call {tool_call_id} await_mode={expected_await_mode}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn count_tool_calls_by_name(node: &EmbeddedNode, session_id: &str, tool_name: &str) -> usize {
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_tool_name = escape_graphql_string(tool_name);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }},
                    tool_name: {{ _eq: "{escaped_tool_name}" }}
                }}
            ) {{ _docID }}
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
        .and_then(|rows| rows.as_array())
        .map_or(0, Vec::len)
}

async fn fetch_child_request(node: &EmbeddedNode, child_request_id: &str) -> ChildRequestRow {
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_child_request_id}" }} }},
                limit: 1
            ) {{
                request_id
                session_id
                behavior_id
                content
                status
                lifecycle_state
                failure_reason
                subagent_depth
                deadline
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
                caused_by_trigger_id
                caused_by_trigger_kind
            }}
        }}"#
    );
    first_row(&node.execute(&query).await, "AgentRequest")
}

async fn fetch_child_request_optional(
    node: &EmbeddedNode,
    child_request_id: &str,
) -> Option<ChildRequestRow> {
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_child_request_id}" }} }},
                limit: 1
            ) {{
                request_id
                session_id
                behavior_id
                content
                status
                lifecycle_state
                failure_reason
                subagent_depth
                deadline
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
                caused_by_trigger_id
                caused_by_trigger_kind
            }}
        }}"#
    );
    first_optional_row(&node.execute(&query).await, "AgentRequest")
}

async fn override_child_agent_did(node: &EmbeddedNode, child_request_id: &str, agent_did: &str) {
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_child_request_id}" }} }},
                input: {{ agent_did: "{escaped_agent_did}" }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "override child agent_did failed: {:?}",
        response.errors
    );
}

async fn child_request_for_tool(
    node: &EmbeddedNode,
    parent_tool_call_id: &str,
) -> Option<ChildRequestRow> {
    let escaped_parent_tool_call_id = escape_graphql_string(parent_tool_call_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    caused_by_parent_tool_call_id: {{ _eq: "{escaped_parent_tool_call_id}" }}
                }},
                limit: 1
            ) {{
                request_id
                session_id
                behavior_id
                content
                status
                lifecycle_state
                failure_reason
                subagent_depth
                deadline
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
                caused_by_trigger_id
                caused_by_trigger_kind
            }}
        }}"#
    );
    first_optional_row(&node.execute(&query).await, "AgentRequest")
}

async fn wait_for_child_request_for_tool(
    node: &EmbeddedNode,
    parent_tool_call_id: &str,
) -> ChildRequestRow {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(row) = child_request_for_tool(node, parent_tool_call_id).await {
            return row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for child AgentRequest for tool call {parent_tool_call_id}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
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

    let escaped_agent_did = escape_graphql_string(AGENT_DID);
    let escaped_behavior_id = escape_graphql_string(CHILD_BEHAVIOR_ID);
    let create_response = format!(
        r#"mutation {{
            create_AgentResponse(input: {{
                response_key: "{escaped_child_request_id}",
                request_id: "{escaped_child_request_id}",
                agent_did: "{escaped_agent_did}",
                behavior_id: "{escaped_behavior_id}",
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

async fn persist_child_terminal(
    node: &EmbeddedNode,
    child_request_id: &str,
    lifecycle_state: &str,
    failure_reason: Option<&str>,
) {
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let escaped_lifecycle_state = escape_graphql_string(lifecycle_state);
    let status = match lifecycle_state {
        "completed" => "completed",
        "superseded" => "superseded",
        "failed" | "dead" | "interrupted" => "error",
        other => other,
    };
    let failure_reason_field = failure_reason
        .map(|reason| {
            let escaped = escape_graphql_string(reason);
            format!(r#", failure_reason: "{escaped}""#)
        })
        .unwrap_or_default();
    let update_request = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_child_request_id}" }} }},
                input: {{
                    status: "{status}",
                    lifecycle_state: "{escaped_lifecycle_state}"
                    {failure_reason_field}
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&update_request).await;
    assert!(
        !response.has_errors(),
        "update child AgentRequest {lifecycle_state} failed: {:?}",
        response.errors
    );
}

async fn update_request_state(
    node: &EmbeddedNode,
    request_id: &str,
    status: &str,
    lifecycle_state: &str,
) {
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_status = escape_graphql_string(status);
    let escaped_lifecycle_state = escape_graphql_string(lifecycle_state);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                input: {{
                    status: "{escaped_status}",
                    lifecycle_state: "{escaped_lifecycle_state}"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "update AgentRequest state failed: {:?}",
        response.errors
    );
}

async fn create_child_session_queued_request(
    node: &EmbeddedNode,
    request_id: &str,
    session_id: &str,
    execution_origin: &str,
    metadata: &str,
) {
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_agent_did = escape_graphql_string(AGENT_DID);
    let escaped_behavior_id = escape_graphql_string(CHILD_BEHAVIOR_ID);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_execution_origin = escape_graphql_string(execution_origin);
    let escaped_metadata = escape_graphql_string(metadata);
    let now = chrono::Utc::now();
    let escaped_created_at = escape_graphql_string(&now.to_rfc3339());
    let escaped_deadline =
        escape_graphql_string(&(now + chrono::Duration::minutes(5)).to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                agent_did: "{escaped_agent_did}",
                behavior_id: "{escaped_behavior_id}",
                session_id: "{escaped_session_id}",
                retry_parent_request: "",
                retry_root_request: "{escaped_request_id}",
                superseded_by_request: "",
                content: "queued child session request",
                status: "pending",
                lifecycle_state: "pending",
                backend_id: "",
                execution_origin: "{escaped_execution_origin}",
                metadata: "{escaped_metadata}",
                failure_reason: "",
                created_at: "{escaped_created_at}",
                deadline: "{escaped_deadline}",
                retry_count: 0,
                max_retries: 3,
                subagent_depth: 1
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create queued child AgentRequest failed: {:?}",
        response.errors
    );
}

fn queue_metadata(
    source: &str,
    policy: &str,
    key: Option<&str>,
    queued_after_request_id: Option<&str>,
) -> String {
    json!({
        "queue": {
            "source": source,
            "policy": policy,
            "key": key,
            "queued_after_request_id": queued_after_request_id
        }
    })
    .to_string()
}

fn skip_reason_json(action: ToolCallHookAction) -> Value {
    let ToolCallHookAction::Skip { reason } = action else {
        panic!("expected Skip action, got {action:?}");
    };
    serde_json::from_str(&reason).expect("skip reason should be JSON")
}

#[tokio::test]
async fn spawn_subagent_background_materializes_child_and_bridge() {
    let (db, hook, session_id, request_id, parent_deadline) = setup_spawn_fixture(
        "spawn_subagent_background",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    let child_deadline = parent_deadline - chrono::Duration::minutes(1);
    let args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "child prompt from spawn tool",
        "await_mode": "background",
        "deadline": child_deadline.to_rfc3339()
    })
    .to_string();

    let action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "spawn_subagent",
        Some("model-call-1".to_string()),
        "internal-spawn-1",
        &args,
    )
    .await;
    let receipt = skip_reason_json(action);
    assert_eq!(receipt["ok"], true);
    assert_eq!(receipt["behavior_id"], CHILD_BEHAVIOR_ID);
    assert_eq!(receipt["await_mode"], "background");
    assert_eq!(receipt["status"], "running");
    let child_request_id = receipt["child_request_id"]
        .as_str()
        .expect("child_request_id")
        .to_string();
    let child_session_id = receipt["child_session_id"]
        .as_str()
        .expect("child_session_id")
        .to_string();

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "internal-spawn-1").await;
    assert_eq!(tool.request_id.as_deref(), Some(request_id.as_str()));
    assert_eq!(tool.tool_name.as_deref(), Some("spawn_subagent"));
    assert_eq!(tool.args.as_deref(), Some(args.as_str()));
    assert_eq!(tool.lifecycle_state.as_deref(), Some("running"));
    assert_eq!(tool.await_mode.as_deref(), Some("background"));
    assert_eq!(tool.cancel_policy.as_deref(), Some("cascade"));
    assert_eq!(
        tool.child_request_id.as_deref(),
        Some(child_request_id.as_str())
    );
    let unclaimed_deadline_at = tool
        .unclaimed_deadline_at
        .as_deref()
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&chrono::Utc))
        .expect("background spawn should set unclaimed_deadline_at");
    let delta = (unclaimed_deadline_at - chrono::Utc::now()).num_seconds();
    assert!(
        (45..=75).contains(&delta),
        "unclaimed_deadline_at should be about 60s out, got {delta}s"
    );

    let child = fetch_child_request(db.node.as_ref(), &child_request_id).await;
    assert_eq!(child.request_id, child_request_id);
    assert_eq!(child.session_id, child_session_id);
    assert_eq!(child.behavior_id, CHILD_BEHAVIOR_ID);
    assert_eq!(child.content, "child prompt from spawn tool");
    assert_eq!(child.lifecycle_state.as_deref(), Some("pending"));
    assert_eq!(child.subagent_depth, Some(1));
    assert_eq!(
        child.deadline.as_deref(),
        Some(child_deadline.to_rfc3339().as_str())
    );
    assert_eq!(
        child.caused_by_parent_request_id.as_deref(),
        Some(request_id.as_str())
    );
    assert_eq!(
        child.caused_by_parent_tool_call_id.as_deref(),
        Some("internal-spawn-1")
    );
    assert_eq!(
        child.caused_by_trigger_id.as_deref(),
        Some("internal-spawn-1")
    );
    assert_eq!(child.caused_by_trigger_kind.as_deref(), Some("subagent"));
}

#[tokio::test]
async fn background_cross_deployment_spawn_writes_bridge_without_local_child() {
    let (db, hook, session_id, request_id, _parent_deadline) = setup_spawn_fixture(
        "spawn_subagent_cross_deployment_background",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    upsert_agent_behavior(
        db.node.as_ref(),
        &AgentBehavior {
            behavior_id: CHILD_BEHAVIOR_ID.to_string(),
            agent_did: "did:defra-agent:r5-remote-child".to_string(),
            display_name: Some("R5 remote child".to_string()),
            system_prompt: None,
            backend_id: None,
            model_name: None,
            tool_selection_id: None,
            inference_profile_id: None,
            compaction_strategy: None,
            compaction_threshold: None,
            enabled: true,
            created_at: Some("2026-05-14T00:00:00Z".to_string()),
        },
    )
    .await
    .unwrap();

    let args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "remote child prompt from spawn tool",
        "await_mode": "background"
    })
    .to_string();

    let action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "spawn_subagent",
        Some("model-call-r5-remote-spawn".to_string()),
        "internal-r5-remote-spawn",
        &args,
    )
    .await;
    let receipt = skip_reason_json(action);
    assert_eq!(receipt["ok"], true);
    assert_eq!(receipt["behavior_id"], CHILD_BEHAVIOR_ID);
    assert_eq!(receipt["await_mode"], "background");
    assert_eq!(receipt["status"], "running");
    assert!(
        receipt["child_session_id"].is_null(),
        "A does not know the child session until B claims and replicates the AgentRequest"
    );
    let child_request_id = receipt["child_request_id"]
        .as_str()
        .expect("child_request_id")
        .to_string();

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "internal-r5-remote-spawn").await;
    assert_eq!(tool.request_id.as_deref(), Some(request_id.as_str()));
    assert_eq!(tool.lifecycle_state.as_deref(), Some("running"));
    assert_eq!(tool.await_mode.as_deref(), Some("background"));
    assert_eq!(tool.cancel_policy.as_deref(), Some("cascade"));
    assert_eq!(
        tool.child_request_id.as_deref(),
        Some(child_request_id.as_str())
    );
    assert!(
        tool.unclaimed_deadline_at.is_some(),
        "cross-deployment bridge keeps the unclaimed-spawn deadline"
    );
    assert!(
        fetch_child_request_optional(db.node.as_ref(), &child_request_id)
            .await
            .is_none(),
        "A must not materialize the B-owned child request"
    );
}

#[tokio::test]
async fn cross_deployment_cancel_writes_cascade_intent_on_bridge() {
    let (db, hook, session_id, _request_id, _parent_deadline) = setup_spawn_fixture(
        "cross_deployment_cancel_intent",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    let args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "remote child prompt",
        "await_mode": "background"
    })
    .to_string();

    let action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "spawn_subagent",
        Some("model-call-xdep-cancel".to_string()),
        "internal-xdep-cancel",
        &args,
    )
    .await;
    let receipt = skip_reason_json(action);
    let child_request_id = receipt["child_request_id"]
        .as_str()
        .expect("child_request_id")
        .to_string();
    override_child_agent_did(
        db.node.as_ref(),
        &child_request_id,
        "did:defra-agent:remote",
    )
    .await;

    let mut lifecycle =
        ToolCallLifecycle::load(db.node.clone(), &session_id, "internal-xdep-cancel")
            .await
            .unwrap()
            .expect("bridge should be persisted");
    let dispatch = lifecycle
        .cancel_during_run_with_cascade_dispatch(AGENT_DID)
        .await
        .unwrap()
        .expect("cascade dispatch");
    assert!(
        matches!(dispatch, CascadeDispatch::RemoteIntentWritten),
        "remote child should write bridge intent"
    );

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "internal-xdep-cancel").await;
    assert!(
        tool.cancel_cascade_intent_at.is_some(),
        "remote branch must set cancel_cascade_intent_at"
    );
    assert_eq!(tool.cancel_pending_remote_ack, Some(true));
    let child_interrupt = fetch_interrupt_requested_at(db.node.as_ref(), &child_request_id)
        .await
        .unwrap();
    assert!(
        child_interrupt.is_none(),
        "remote branch must not write child interrupt_requested_at"
    );
}

#[tokio::test]
async fn single_deployment_cancel_dispatch_still_interrupts_child() {
    let (db, hook, session_id, _request_id, _parent_deadline) = setup_spawn_fixture(
        "single_deployment_cancel_interrupt",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    let args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "local child prompt",
        "await_mode": "background"
    })
    .to_string();

    let action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "spawn_subagent",
        Some("model-call-local-cancel".to_string()),
        "internal-local-cancel",
        &args,
    )
    .await;
    let receipt = skip_reason_json(action);
    let child_request_id = receipt["child_request_id"]
        .as_str()
        .expect("child_request_id")
        .to_string();

    let mut lifecycle =
        ToolCallLifecycle::load(db.node.clone(), &session_id, "internal-local-cancel")
            .await
            .unwrap()
            .expect("bridge should be persisted");
    let dispatch = lifecycle
        .cancel_during_run_with_cascade_dispatch(AGENT_DID)
        .await
        .unwrap()
        .expect("cascade dispatch");
    let CascadeDispatch::Local(intent) = dispatch else {
        panic!("local child should use local cascade dispatch");
    };
    assert_eq!(intent.child_request_id, child_request_id);
    interrupt_request(db.node.as_ref(), &intent.child_request_id)
        .await
        .unwrap();

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "internal-local-cancel").await;
    assert!(
        tool.cancel_cascade_intent_at.is_none(),
        "local branch must not set bridge cancel intent"
    );
    let child_interrupt = fetch_interrupt_requested_at(db.node.as_ref(), &child_request_id)
        .await
        .unwrap();
    assert!(
        child_interrupt.is_some(),
        "local branch should still interrupt the child"
    );
}

#[tokio::test]
async fn foreground_spawn_subagent_waits_for_child_completion() {
    let (db, hook, session_id, _request_id, parent_deadline) = setup_spawn_fixture(
        "spawn_subagent_foreground",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    let args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "foreground child prompt",
        "deadline": (parent_deadline - chrono::Duration::minutes(1)).to_rfc3339()
    })
    .to_string();

    let hook_for_wait = hook.clone();
    let args_for_wait = args.clone();
    let wait_handle = tokio::spawn(async move {
        PromptHook::<TestModel>::on_tool_call(
            &hook_for_wait,
            "spawn_subagent",
            Some("model-call-fg".to_string()),
            "internal-spawn-fg",
            &args_for_wait,
        )
        .await
    });

    let child = wait_for_child_request_for_tool(db.node.as_ref(), "internal-spawn-fg").await;
    persist_child_completion(
        db.node.as_ref(),
        &child.request_id,
        &child.session_id,
        "foreground final answer",
    )
    .await;

    let action = tokio::time::timeout(Duration::from_secs(5), wait_handle)
        .await
        .expect("foreground wait should complete")
        .expect("foreground task should not panic");
    let result = skip_reason_json(action);
    assert_eq!(result["ok"], true);
    assert_eq!(result["await_mode"], "foreground");
    assert_eq!(result["status"], "completed");
    assert_eq!(result["final_response"], "foreground final answer");

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "internal-spawn-fg").await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("completed"));
    assert_eq!(tool.result.as_deref(), Some("foreground final answer"));
}

#[tokio::test]
async fn foreground_spawn_subagent_parent_deadline_marks_bridge_dead() {
    let parent_deadline = chrono::Utc::now() + chrono::Duration::milliseconds(250);
    let (db, hook, session_id, _request_id, _parent_deadline) =
        setup_spawn_fixture_with_flags_and_deadline(
            "foreground_spawn_deadline",
            vec![CHILD_BEHAVIOR_ID],
            0,
            true,
            true,
            parent_deadline,
        )
        .await;
    let args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "foreground child that will exceed parent deadline"
    })
    .to_string();

    let action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "spawn_subagent",
        Some("model-call-fg-deadline".to_string()),
        "internal-spawn-fg-deadline",
        &args,
    )
    .await;
    let result = skip_reason_json(action);
    assert_eq!(result["ok"], false);
    assert_eq!(result["await_mode"], "foreground");
    assert_eq!(result["status"], "dead");
    assert!(result["error"]["reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("parent request deadline exceeded")));

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "internal-spawn-fg-deadline").await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("failed"));
}

#[tokio::test]
async fn foreground_spawn_subagent_cancellation_cascades_to_child_and_unblocks_wait() {
    let (db, hook, session_id, _request_id, _parent_deadline) =
        setup_spawn_fixture("foreground_spawn_cancel", vec![CHILD_BEHAVIOR_ID], 0, true).await;
    let args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "foreground child that will be cancelled"
    })
    .to_string();

    let hook_for_wait = hook.clone();
    let args_for_wait = args.clone();
    let wait_handle = tokio::spawn(async move {
        PromptHook::<TestModel>::on_tool_call(
            &hook_for_wait,
            "spawn_subagent",
            Some("model-call-fg-cancel".to_string()),
            "internal-spawn-fg-cancel",
            &args_for_wait,
        )
        .await
    });

    let child = wait_for_child_request_for_tool(db.node.as_ref(), "internal-spawn-fg-cancel").await;
    let mut lifecycle =
        ToolCallLifecycle::load(db.node.clone(), &session_id, "internal-spawn-fg-cancel")
            .await
            .unwrap()
            .expect("foreground bridge should be persisted");
    lifecycle.cancel_during_run().await.unwrap();
    let intent = lifecycle
        .bridge_cancel_cascade()
        .await
        .unwrap()
        .expect("foreground bridge should return cascade intent");
    assert_eq!(intent.child_request_id, child.request_id);
    interrupt_request(db.node.as_ref(), &intent.child_request_id)
        .await
        .unwrap();

    let action = tokio::time::timeout(Duration::from_secs(5), wait_handle)
        .await
        .expect("foreground wait should unblock after cancellation")
        .expect("foreground task should not panic");
    let result = skip_reason_json(action);
    assert_eq!(result["ok"], false);
    assert_eq!(result["await_mode"], "foreground");
    assert_eq!(result["status"], "interrupted");

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "internal-spawn-fg-cancel").await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("cancelled"));
    let child_interrupt = fetch_interrupt_requested_at(db.node.as_ref(), &child.request_id)
        .await
        .unwrap();
    assert!(
        child_interrupt.is_some(),
        "cascade cancellation should latch interrupt_requested_at on the child"
    );
}

#[tokio::test]
async fn foreground_spawn_subagent_user_backgrounding_returns_background_receipt() {
    let (db, hook, session_id, _request_id, _parent_deadline) = setup_spawn_fixture(
        "foreground_spawn_backgrounded",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    let args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "foreground child that will be backgrounded"
    })
    .to_string();

    let hook_for_wait = hook.clone();
    let args_for_wait = args.clone();
    let wait_handle = tokio::spawn(async move {
        PromptHook::<TestModel>::on_tool_call(
            &hook_for_wait,
            "spawn_subagent",
            Some("model-call-fg-backgrounded".to_string()),
            "internal-spawn-fg-backgrounded",
            &args_for_wait,
        )
        .await
    });

    let child =
        wait_for_child_request_for_tool(db.node.as_ref(), "internal-spawn-fg-backgrounded").await;
    let mut lifecycle = ToolCallLifecycle::load(
        db.node.clone(),
        &session_id,
        "internal-spawn-fg-backgrounded",
    )
    .await
    .unwrap()
    .expect("foreground bridge should be persisted");
    lifecycle.background().await.unwrap();

    let action = tokio::time::timeout(Duration::from_secs(5), wait_handle)
        .await
        .expect("foreground wait should unblock after backgrounding")
        .expect("foreground task should not panic");
    let result = skip_reason_json(action);
    assert_eq!(result["ok"], true);
    assert_eq!(result["await_mode"], "background");
    assert_eq!(result["status"], "running");
    assert_eq!(result["backgrounded"], true);
    assert_eq!(result["child_request_id"], child.request_id);

    let tool = fetch_tool_call(
        db.node.as_ref(),
        &session_id,
        "internal-spawn-fg-backgrounded",
    )
    .await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("running"));
    assert_eq!(tool.await_mode.as_deref(), Some("background"));
}

#[tokio::test]
async fn foreground_spawn_subagent_maps_child_terminal_failures() {
    let cases = [
        ("failed", "failed", "failed", Some("child failed reason")),
        ("dead", "dead", "failed", None),
        ("interrupted", "interrupted", "cancelled", None),
        ("superseded", "superseded", "failed", None),
    ];

    for (child_state, expected_status, expected_tool_state, failure_reason) in cases {
        let test_name = format!("foreground_spawn_terminal_{child_state}");
        let internal_call_id = format!("internal-spawn-terminal-{child_state}");
        let (db, hook, session_id, _request_id, _parent_deadline) =
            setup_spawn_fixture(&test_name, vec![CHILD_BEHAVIOR_ID], 0, true).await;
        let args = json!({
            "behavior_id": CHILD_BEHAVIOR_ID,
            "prompt": format!("foreground child terminal {child_state}")
        })
        .to_string();

        let hook_for_wait = hook.clone();
        let args_for_wait = args.clone();
        let internal_call_id_for_wait = internal_call_id.clone();
        let wait_handle = tokio::spawn(async move {
            PromptHook::<TestModel>::on_tool_call(
                &hook_for_wait,
                "spawn_subagent",
                Some(format!("model-call-{child_state}")),
                &internal_call_id_for_wait,
                &args_for_wait,
            )
            .await
        });

        let child = wait_for_child_request_for_tool(db.node.as_ref(), &internal_call_id).await;
        persist_child_terminal(
            db.node.as_ref(),
            &child.request_id,
            child_state,
            failure_reason,
        )
        .await;

        let action = tokio::time::timeout(Duration::from_secs(5), wait_handle)
            .await
            .expect("foreground wait should complete after child terminal")
            .expect("foreground task should not panic");
        let result = skip_reason_json(action);
        assert_eq!(result["ok"], false);
        assert_eq!(result["await_mode"], "foreground");
        assert_eq!(result["status"], expected_status);
        if let Some(reason) = failure_reason {
            assert_eq!(result["error"]["reason"], reason);
            assert_eq!(result["error"]["failure_class"], "external");
        }

        let tool = fetch_tool_call(db.node.as_ref(), &session_id, &internal_call_id).await;
        assert_eq!(
            tool.lifecycle_state.as_deref(),
            Some(expected_tool_state),
            "unexpected tool state for child terminal {child_state}"
        );
        if let Some(reason) = failure_reason {
            assert_eq!(tool.result.as_deref(), Some(reason));
            assert_eq!(tool.tool_failure_class.as_deref(), Some("external"));
        }
    }
}

#[tokio::test]
async fn spawn_subagent_skip_payload_is_persisted_to_transcript() {
    let (db, hook, session_id, _request_id, parent_deadline) = setup_spawn_fixture(
        "spawn_subagent_skip_transcript",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    let args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "child prompt for transcript",
        "await_mode": "background",
        "deadline": (parent_deadline - chrono::Duration::minutes(1)).to_rfc3339()
    })
    .to_string();

    let action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "spawn_subagent",
        Some("model-call-transcript".to_string()),
        "internal-spawn-transcript",
        &args,
    )
    .await;
    let ToolCallHookAction::Skip { reason } = action else {
        panic!("expected Skip action");
    };
    let child_request_id = serde_json::from_str::<Value>(&reason).unwrap()["child_request_id"]
        .as_str()
        .unwrap()
        .to_string();

    hook.persist_message(&Message::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::ToolCall(ToolCall {
            id: "internal-spawn-transcript".to_string(),
            call_id: Some("model-call-transcript".to_string()),
            function: ToolFunction {
                name: "spawn_subagent".to_string(),
                arguments: serde_json::from_str(&args).unwrap(),
            },
            signature: None,
            additional_params: None,
        })),
    })
    .await
    .unwrap();
    hook.persist_stream_tool_result_message(
        &ToolResult {
            id: "internal-spawn-transcript".to_string(),
            call_id: Some("model-call-transcript".to_string()),
            content: OneOrMany::one(ToolResultContent::Text(Text {
                text: reason.clone(),
            })),
        },
        "internal-spawn-transcript",
    )
    .await
    .unwrap();

    let history = load_history(db.node.as_ref(), &session_id).await.unwrap();
    assert!(history.iter().any(|message| {
        matches!(
            message,
            Message::User { content }
                if matches!(content.first_ref(), UserContent::ToolResult(tool_result)
                    if matches!(tool_result.content.first_ref(), ToolResultContent::Text(Text { text })
                        if text.contains(&child_request_id)
                            && text.contains("\"await_mode\": \"background\"")))
        )
    }));
}

#[tokio::test]
async fn spawn_subagent_rejects_unauthorized_target_without_child_request() {
    let (db, hook, session_id, _request_id, _parent_deadline) = setup_spawn_fixture(
        "spawn_subagent_unauthorized",
        vec!["different-child"],
        0,
        true,
    )
    .await;
    let args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "should not spawn",
        "await_mode": "background"
    })
    .to_string();

    let action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "spawn_subagent",
        None,
        "internal-spawn-denied",
        &args,
    )
    .await;
    let error = skip_reason_json(action);
    assert_eq!(error["ok"], false);
    assert_eq!(error["failure_class"], "tool_not_allowed");
    assert_eq!(error["requested_tool_name"], CHILD_BEHAVIOR_ID);

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "internal-spawn-denied").await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("failed"));
    assert_eq!(
        tool.tool_failure_class.as_deref(),
        Some("serviceUnavailable")
    );
    assert!(tool
        .result
        .as_deref()
        .is_some_and(|result| result.contains("\"tool_not_allowed\"")));
    assert!(
        child_request_for_tool(db.node.as_ref(), "internal-spawn-denied")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn spawn_subagent_rejects_when_spawn_disabled_without_child_request() {
    let (db, hook, session_id, _request_id, _parent_deadline) = setup_spawn_fixture_with_flags(
        "spawn_subagent_spawn_disabled",
        vec![CHILD_BEHAVIOR_ID],
        0,
        false,
        true,
    )
    .await;
    let args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "should not spawn",
        "await_mode": "background"
    })
    .to_string();

    let action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "spawn_subagent",
        None,
        "internal-spawn-disabled",
        &args,
    )
    .await;
    let error = skip_reason_json(action);
    assert_eq!(error["failure_class"], "tool_not_allowed");

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "internal-spawn-disabled").await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("failed"));
    assert_eq!(
        tool.tool_failure_class.as_deref(),
        Some("serviceUnavailable")
    );
    assert!(
        child_request_for_tool(db.node.as_ref(), "internal-spawn-disabled")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn spawn_subagent_rejects_background_when_background_disabled_without_child_request() {
    let (db, hook, session_id, _request_id, _parent_deadline) = setup_spawn_fixture(
        "spawn_subagent_background_disabled",
        vec![CHILD_BEHAVIOR_ID],
        0,
        false,
    )
    .await;
    let args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "should not spawn in background",
        "await_mode": "background"
    })
    .to_string();

    let action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "spawn_subagent",
        None,
        "internal-spawn-bg-disabled",
        &args,
    )
    .await;
    let error = skip_reason_json(action);
    assert_eq!(error["failure_class"], "tool_not_allowed");
    assert_eq!(error["requested_tool_name"], "background");

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "internal-spawn-bg-disabled").await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("failed"));
    assert_eq!(
        tool.tool_failure_class.as_deref(),
        Some("serviceUnavailable")
    );
    assert!(
        child_request_for_tool(db.node.as_ref(), "internal-spawn-bg-disabled")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn spawn_subagent_rejects_deadline_after_parent_without_child_request() {
    let (db, hook, session_id, _request_id, parent_deadline) =
        setup_spawn_fixture("spawn_subagent_deadline", vec![CHILD_BEHAVIOR_ID], 0, true).await;
    let args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "deadline too late",
        "await_mode": "background",
        "deadline": (parent_deadline + chrono::Duration::seconds(1)).to_rfc3339()
    })
    .to_string();

    let action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "spawn_subagent",
        None,
        "internal-spawn-deadline",
        &args,
    )
    .await;
    let error = skip_reason_json(action);
    assert_eq!(error["failure_class"], "invalid_tool_arguments");
    assert_eq!(error["path"], "/deadline");

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "internal-spawn-deadline").await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("failed"));
    assert_eq!(tool.tool_failure_class.as_deref(), Some("argumentInvalid"));
    assert!(
        child_request_for_tool(db.node.as_ref(), "internal-spawn-deadline")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn spawn_subagent_rejects_depth_ceiling_without_child_request() {
    let (db, hook, session_id, _request_id, _parent_deadline) = setup_spawn_fixture(
        "spawn_subagent_depth",
        vec![CHILD_BEHAVIOR_ID],
        MAX_SUBAGENT_DEPTH,
        true,
    )
    .await;
    let args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "too deep",
        "await_mode": "background"
    })
    .to_string();

    let action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "spawn_subagent",
        None,
        "internal-spawn-depth",
        &args,
    )
    .await;
    let error = skip_reason_json(action);
    assert_eq!(error["ok"], false);
    assert_eq!(error["failure_class"], "invalid_tool_arguments");
    assert_eq!(error["code"], "subagent_depth_exceeded");
    assert_eq!(error["parent_subagent_depth"], json!(MAX_SUBAGENT_DEPTH));
    assert_eq!(error["max_subagent_depth"], json!(MAX_SUBAGENT_DEPTH));

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "internal-spawn-depth").await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("failed"));
    assert_eq!(tool.tool_failure_class.as_deref(), Some("argumentInvalid"));
    assert!(
        child_request_for_tool(db.node.as_ref(), "internal-spawn-depth")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn wait_subagent_waits_on_existing_bridge_without_lifecycle_row() {
    let (db, hook, session_id, _request_id, _parent_deadline) = setup_spawn_fixture(
        "wait_subagent_existing_bridge",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    let spawn_args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "background child for wait_subagent",
        "await_mode": "background"
    })
    .to_string();

    let spawn_action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "spawn_subagent",
        Some("model-call-wait-spawn".to_string()),
        "internal-wait-spawn",
        &spawn_args,
    )
    .await;
    let spawn_receipt = skip_reason_json(spawn_action);
    assert_eq!(spawn_receipt["ok"], true);
    assert_eq!(spawn_receipt["await_mode"], "background");
    let child_request_id = spawn_receipt["child_request_id"]
        .as_str()
        .expect("child_request_id")
        .to_string();
    let child_session_id = spawn_receipt["child_session_id"]
        .as_str()
        .expect("child_session_id")
        .to_string();

    let hook_for_wait = hook.clone();
    let wait_args = json!({ "child_request_id": child_request_id }).to_string();
    let wait_handle = tokio::spawn(async move {
        PromptHook::<TestModel>::on_tool_call(
            &hook_for_wait,
            "wait_subagent",
            Some("model-call-wait".to_string()),
            "internal-wait-tool",
            &wait_args,
        )
        .await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;
    let foregrounded_bridge =
        fetch_tool_call(db.node.as_ref(), &session_id, "internal-wait-spawn").await;
    assert_eq!(
        foregrounded_bridge.await_mode.as_deref(),
        Some("foreground")
    );
    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "wait_subagent").await,
        0
    );

    persist_child_completion(
        db.node.as_ref(),
        &child_request_id,
        &child_session_id,
        "wait_subagent final answer",
    )
    .await;

    let action = tokio::time::timeout(Duration::from_secs(5), wait_handle)
        .await
        .expect("wait_subagent should complete after child completion")
        .expect("wait_subagent task should not panic");
    let result = skip_reason_json(action);
    assert_eq!(result["ok"], true);
    assert_eq!(result["await_mode"], "foreground");
    assert_eq!(result["status"], "completed");
    assert_eq!(result["final_response"], "wait_subagent final answer");

    let completed_bridge =
        fetch_tool_call(db.node.as_ref(), &session_id, "internal-wait-spawn").await;
    assert_eq!(
        completed_bridge.lifecycle_state.as_deref(),
        Some("completed")
    );
    assert_eq!(
        completed_bridge.result.as_deref(),
        Some("wait_subagent final answer")
    );
    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "wait_subagent").await,
        0
    );
}

#[tokio::test]
async fn wait_subagent_maps_child_terminal_failures_without_lifecycle_row() {
    let cases = [
        (
            "failed",
            "failed",
            "failed",
            Some("child failed reason"),
            "child failed reason",
        ),
        (
            "dead",
            "dead",
            "failed",
            None,
            "child request reached terminal state dead",
        ),
        (
            "interrupted",
            "interrupted",
            "cancelled",
            None,
            "child request was interrupted",
        ),
        (
            "superseded",
            "superseded",
            "failed",
            None,
            "child request was superseded",
        ),
    ];

    for (
        child_state,
        expected_status,
        expected_tool_state,
        failure_reason,
        expected_error_reason,
    ) in cases
    {
        let test_name = format!("wait_subagent_terminal_{child_state}");
        let internal_call_id = format!("internal-wait-terminal-spawn-{child_state}");
        let (db, hook, session_id, _request_id, _parent_deadline) =
            setup_spawn_fixture(&test_name, vec![CHILD_BEHAVIOR_ID], 0, true).await;
        let spawn_args = json!({
            "behavior_id": CHILD_BEHAVIOR_ID,
            "prompt": format!("background child terminal {child_state}"),
            "await_mode": "background"
        })
        .to_string();

        let spawn_action = PromptHook::<TestModel>::on_tool_call(
            &hook,
            "spawn_subagent",
            Some(format!("model-call-wait-terminal-spawn-{child_state}")),
            &internal_call_id,
            &spawn_args,
        )
        .await;
        let spawn_receipt = skip_reason_json(spawn_action);
        assert_eq!(spawn_receipt["ok"], true);
        assert_eq!(spawn_receipt["await_mode"], "background");
        let child_request_id = spawn_receipt["child_request_id"]
            .as_str()
            .expect("child_request_id")
            .to_string();
        let background_bridge =
            fetch_tool_call(db.node.as_ref(), &session_id, &internal_call_id).await;
        assert_eq!(
            background_bridge.await_mode.as_deref(),
            Some("background"),
            "spawn_subagent should persist a background bridge before wait_subagent starts"
        );
        assert_eq!(
            background_bridge.lifecycle_state.as_deref(),
            Some("running")
        );

        let hook_for_wait = hook.clone();
        let wait_args = json!({ "child_request_id": child_request_id }).to_string();
        let wait_handle = tokio::spawn(async move {
            PromptHook::<TestModel>::on_tool_call(
                &hook_for_wait,
                "wait_subagent",
                Some(format!("model-call-wait-terminal-{child_state}")),
                "internal-wait-terminal",
                &wait_args,
            )
            .await
        });

        let foregrounded_bridge = wait_for_tool_call_await_mode(
            db.node.as_ref(),
            &session_id,
            &internal_call_id,
            "foreground",
        )
        .await;
        assert_eq!(
            foregrounded_bridge.lifecycle_state.as_deref(),
            Some("running")
        );

        persist_child_terminal(
            db.node.as_ref(),
            &child_request_id,
            child_state,
            failure_reason,
        )
        .await;

        let action = tokio::time::timeout(Duration::from_secs(5), wait_handle)
            .await
            .expect("wait_subagent should complete after child terminal")
            .expect("wait_subagent task should not panic");
        let result = skip_reason_json(action);
        assert_eq!(result["ok"], false);
        assert_eq!(result["await_mode"], "foreground");
        assert_eq!(result["status"], expected_status);
        assert_eq!(result["error"]["reason"], expected_error_reason);
        assert_eq!(result["error"]["failure_class"], "external");

        let bridge = fetch_tool_call(db.node.as_ref(), &session_id, &internal_call_id).await;
        assert_eq!(
            bridge.lifecycle_state.as_deref(),
            Some(expected_tool_state),
            "unexpected bridge state for child terminal {child_state}"
        );
        if let Some(reason) = failure_reason {
            assert_eq!(bridge.result.as_deref(), Some(reason));
            assert_eq!(bridge.tool_failure_class.as_deref(), Some("external"));
        }
        assert_eq!(
            count_tool_calls_by_name(db.node.as_ref(), &session_id, "wait_subagent").await,
            0
        );
    }
}

#[tokio::test]
async fn wait_subagent_rejects_unlinked_child_without_lifecycle_row() {
    let (db, hook, session_id, _request_id, _parent_deadline) = setup_spawn_fixture(
        "wait_subagent_unlinked_child",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    let wait_args = json!({ "child_request_id": "not-this-parents-child" }).to_string();

    let action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "wait_subagent",
        Some("model-call-wait-denied".to_string()),
        "internal-wait-denied",
        &wait_args,
    )
    .await;
    let error = skip_reason_json(action);
    assert_eq!(error["ok"], false);
    assert_eq!(error["failure_class"], "service_unavailable");
    assert_eq!(error["tool_name"], "wait_subagent");
    assert_eq!(error["path"], "/child_request_id");
    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "wait_subagent").await,
        0
    );
}

#[tokio::test]
async fn wait_subagent_from_resumed_hook_cascades_parent_interrupt() {
    let (db, hook, session_id, request_id, parent_deadline) = setup_spawn_fixture(
        "wait_subagent_resumed_interrupt",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    let spawn_args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "background child for resumed wait cancellation",
        "await_mode": "background"
    })
    .to_string();

    let spawn_action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "spawn_subagent",
        Some("model-call-wait-resume-spawn".to_string()),
        "internal-wait-resume-spawn",
        &spawn_args,
    )
    .await;
    let spawn_receipt = skip_reason_json(spawn_action);
    let child_request_id = spawn_receipt["child_request_id"]
        .as_str()
        .expect("child_request_id")
        .to_string();

    let resumed_hook = DefraSessionHook::resume_or_create_with_identity_policy(
        db.node.clone(),
        &session_id,
        PARENT_BEHAVIOR_ID,
        AGENT_DID,
        FailurePolicy::default(),
    )
    .await
    .unwrap();
    resumed_hook
        .set_active_request_id(Some(request_id.clone()))
        .await;
    resumed_hook
        .set_request_deadline_at(Some(parent_deadline))
        .await;

    let hook_for_wait = resumed_hook.clone();
    let wait_args = json!({ "child_request_id": child_request_id }).to_string();
    let wait_handle = tokio::spawn(async move {
        PromptHook::<TestModel>::on_tool_call(
            &hook_for_wait,
            "wait_subagent",
            Some("model-call-wait-resume".to_string()),
            "internal-wait-resume",
            &wait_args,
        )
        .await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;
    let foregrounded_bridge =
        fetch_tool_call(db.node.as_ref(), &session_id, "internal-wait-resume-spawn").await;
    assert_eq!(
        foregrounded_bridge.await_mode.as_deref(),
        Some("foreground")
    );

    interrupt_request(db.node.as_ref(), &request_id)
        .await
        .unwrap();

    let action = tokio::time::timeout(Duration::from_secs(5), wait_handle)
        .await
        .expect("resumed wait_subagent should unblock after parent interrupt")
        .expect("resumed wait_subagent task should not panic");
    let result = skip_reason_json(action);
    assert_eq!(result["ok"], false);
    assert_eq!(result["await_mode"], "foreground");
    assert_eq!(result["status"], "interrupted");

    let cancelled_bridge =
        fetch_tool_call(db.node.as_ref(), &session_id, "internal-wait-resume-spawn").await;
    assert_eq!(
        cancelled_bridge.lifecycle_state.as_deref(),
        Some("cancelled")
    );
    let child_interrupt = fetch_interrupt_requested_at(db.node.as_ref(), &child_request_id)
        .await
        .unwrap();
    assert!(
        child_interrupt.is_some(),
        "wait_subagent cancellation should cascade to the child request"
    );
    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "wait_subagent").await,
        0
    );
}

#[tokio::test]
async fn wait_subagent_returns_background_receipt_when_bridge_is_backgrounded() {
    let (db, hook, session_id, _request_id, _parent_deadline) = setup_spawn_fixture(
        "wait_subagent_backgrounded",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    let spawn_args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "background child for wait backgrounding",
        "await_mode": "background"
    })
    .to_string();

    let spawn_action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "spawn_subagent",
        Some("model-call-wait-bg-spawn".to_string()),
        "internal-wait-bg-spawn",
        &spawn_args,
    )
    .await;
    let spawn_receipt = skip_reason_json(spawn_action);
    let child_request_id = spawn_receipt["child_request_id"]
        .as_str()
        .expect("child_request_id")
        .to_string();

    let hook_for_wait = hook.clone();
    let wait_args = json!({ "child_request_id": child_request_id }).to_string();
    let wait_handle = tokio::spawn(async move {
        PromptHook::<TestModel>::on_tool_call(
            &hook_for_wait,
            "wait_subagent",
            Some("model-call-wait-bg".to_string()),
            "internal-wait-bg",
            &wait_args,
        )
        .await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut lifecycle =
        ToolCallLifecycle::load(db.node.clone(), &session_id, "internal-wait-bg-spawn")
            .await
            .unwrap()
            .expect("wait_subagent should foreground the original bridge");
    lifecycle.background().await.unwrap();

    let action = tokio::time::timeout(Duration::from_secs(5), wait_handle)
        .await
        .expect("wait_subagent should unblock after bridge backgrounding")
        .expect("wait_subagent task should not panic");
    let result = skip_reason_json(action);
    assert_eq!(result["ok"], true);
    assert_eq!(result["await_mode"], "background");
    assert_eq!(result["status"], "running");
    assert_eq!(result["backgrounded"], true);
    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "wait_subagent").await,
        0
    );
}

#[tokio::test]
async fn cancel_subagent_cancels_bridge_active_descendants_and_owned_queue() {
    let (db, hook, session_id, _request_id, parent_deadline) =
        setup_spawn_fixture("cancel_subagent_active", vec![CHILD_BEHAVIOR_ID], 0, true).await;
    let spawn_args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "background child for cancel_subagent",
        "await_mode": "background"
    })
    .to_string();

    let spawn_action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "spawn_subagent",
        Some("model-call-cancel-spawn".to_string()),
        "internal-cancel-spawn",
        &spawn_args,
    )
    .await;
    let spawn_receipt = skip_reason_json(spawn_action);
    let child_request_id = spawn_receipt["child_request_id"]
        .as_str()
        .expect("child_request_id")
        .to_string();
    let child_session_id = spawn_receipt["child_session_id"]
        .as_str()
        .expect("child_session_id")
        .to_string();
    update_request_state(
        db.node.as_ref(),
        &child_request_id,
        "processing",
        "processing",
    )
    .await;

    let automated_request_id = "cancel-subagent-active-auto-queue";
    create_child_session_queued_request(
        db.node.as_ref(),
        automated_request_id,
        &child_session_id,
        "scheduled",
        &queue_metadata(
            "background_completion",
            "coalesce",
            Some("background_completion:cancel-subagent-active"),
            Some(&child_request_id),
        ),
    )
    .await;
    let steering_request_id = "cancel-subagent-active-steering-queue";
    create_child_session_queued_request(
        db.node.as_ref(),
        steering_request_id,
        &child_session_id,
        "interactive",
        &queue_metadata("steering", "append", None, Some(&child_request_id)),
    )
    .await;
    let user_request_id = "cancel-subagent-active-user-queue";
    create_child_session_queued_request(
        db.node.as_ref(),
        user_request_id,
        &child_session_id,
        "interactive",
        &queue_metadata("user", "append", None, Some(&child_request_id)),
    )
    .await;

    let grandchild_request_id = "cancel-subagent-active-grandchild";
    let mut descendant_bridge = ToolCallLifecycle::new_subagent(
        db.node.clone(),
        child_request_id.clone(),
        child_session_id.clone(),
        "internal-cancel-descendant".to_string(),
        1,
        "spawn_subagent".to_string(),
        "{}".to_string(),
        parent_deadline,
        AwaitMode::Background,
        CancelPolicy::Cascade,
        grandchild_request_id.to_string(),
    );
    descendant_bridge.start_running().await.unwrap();
    let _grandchild_session_id = create_subagent_request_with_request_id(
        db.node.as_ref(),
        grandchild_request_id.to_string(),
        child_request_id.clone(),
        "internal-cancel-descendant".to_string(),
        1,
        AGENT_DID.to_string(),
        CHILD_BEHAVIOR_ID.to_string(),
        "grandchild prompt".to_string(),
        Some(parent_deadline - chrono::Duration::minutes(1)),
    )
    .await
    .unwrap();

    let collision_action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "bash",
        None,
        "internal-cancel-descendant",
        "{\"cmd\":\"still running\"}",
    )
    .await;
    assert!(matches!(collision_action, ToolCallHookAction::Continue));

    let cancel_args = json!({
        "child_request_id": child_request_id.clone(),
        "reason": "parent no longer needs this work"
    })
    .to_string();
    let action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "cancel_subagent",
        Some("model-call-cancel".to_string()),
        "internal-cancel-tool",
        &cancel_args,
    )
    .await;
    let result = skip_reason_json(action);
    assert_eq!(result["ok"], true);
    assert_eq!(result["status"], "cancelled");
    assert_eq!(result["child_request_id"], child_request_id);
    assert_eq!(result["child_session_id"], child_session_id);
    assert_eq!(result["active_interrupted"], true);
    assert_eq!(result["descendants_cancelled"], 1);
    assert_eq!(result["queued_drained"], 2);

    let root_bridge = fetch_tool_call(db.node.as_ref(), &session_id, "internal-cancel-spawn").await;
    assert_eq!(root_bridge.lifecycle_state.as_deref(), Some("cancelled"));
    let descendant = fetch_tool_call(
        db.node.as_ref(),
        &child_session_id,
        "internal-cancel-descendant",
    )
    .await;
    assert_eq!(descendant.lifecycle_state.as_deref(), Some("cancelled"));
    let parent_collision =
        fetch_tool_call(db.node.as_ref(), &session_id, "internal-cancel-descendant").await;
    assert_eq!(
        parent_collision.lifecycle_state.as_deref(),
        Some("running"),
        "descendant cancellation must not consume same-id parent-session lifecycle state"
    );
    assert!(
        fetch_interrupt_requested_at(db.node.as_ref(), &child_request_id)
            .await
            .unwrap()
            .is_some(),
        "cancel_subagent should interrupt the child request"
    );
    assert!(
        fetch_interrupt_requested_at(db.node.as_ref(), grandchild_request_id)
            .await
            .unwrap()
            .is_some(),
        "cancel_subagent should cascade to live descendant subagents"
    );

    let automated = fetch_child_request(db.node.as_ref(), automated_request_id).await;
    assert_eq!(automated.status.as_deref(), Some("interrupted"));
    assert_eq!(automated.lifecycle_state.as_deref(), Some("interrupted"));
    assert!(automated
        .failure_reason
        .as_deref()
        .is_some_and(|reason| reason.contains("parent no longer needs this work")));
    let steering = fetch_child_request(db.node.as_ref(), steering_request_id).await;
    assert_eq!(steering.status.as_deref(), Some("interrupted"));
    assert_eq!(steering.lifecycle_state.as_deref(), Some("interrupted"));
    let user = fetch_child_request(db.node.as_ref(), user_request_id).await;
    assert_eq!(user.status.as_deref(), Some("pending"));
    assert_eq!(user.lifecycle_state.as_deref(), Some("pending"));
    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "cancel_subagent").await,
        0
    );
}

#[tokio::test]
async fn cancel_subagent_rejects_unlinked_child_without_lifecycle_row() {
    let (db, hook, session_id, _request_id, _parent_deadline) = setup_spawn_fixture(
        "cancel_subagent_unlinked_child",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    let cancel_args = json!({ "child_request_id": "not-this-parents-child" }).to_string();

    let action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "cancel_subagent",
        Some("model-call-cancel-denied".to_string()),
        "internal-cancel-denied",
        &cancel_args,
    )
    .await;
    let error = skip_reason_json(action);
    assert_eq!(error["ok"], false);
    assert_eq!(error["failure_class"], "service_unavailable");
    assert_eq!(error["tool_name"], "cancel_subagent");
    assert_eq!(error["path"], "/child_request_id");
    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "cancel_subagent").await,
        0
    );
}
