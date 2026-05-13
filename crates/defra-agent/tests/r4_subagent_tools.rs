//! R4 agent-facing subagent tool integration tests.

mod support;

use std::time::Duration;

use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::tool_call_lifecycle::MAX_SUBAGENT_DEPTH;
use defra_agent::{
    load_history, upsert_agent_behavior, upsert_tool_selection, AgentBehavior, DefraSessionHook,
    FailurePolicy, ToolSelectionDocument,
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
    tool_failure_class: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChildRequestRow {
    request_id: String,
    session_id: String,
    behavior_id: String,
    content: String,
    lifecycle_state: Option<String>,
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
    let parent_deadline = chrono::Utc::now() + chrono::Duration::minutes(5);
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
                tool_failure_class
            }}
        }}"#
    );
    first_row(&node.execute(&query).await, "AgentToolCall")
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
                lifecycle_state
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
                lifecycle_state
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
async fn spawn_subagent_foreground_waits_for_child_completion() {
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
