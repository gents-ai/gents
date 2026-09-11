use std::time::Duration;

use gents::background_completion::{
    project_background_subagent_completion, BackgroundCompletionOutcome,
};
use gents::config_client::{
    apply_desired_state_plan, ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::defra_node::EmbeddedNode;
use gents::document_config::{
    AgentBehavior, AgentContext, SubagentTargetDocument, SubagentTools, Tools,
};
use gents::graphql::escape_graphql_string;
use gents::llm::message::{
    AssistantContent, Message, Text, ToolCall, ToolFunction, ToolResult, ToolResultContent,
    UserContent,
};
use gents::llm::ToolCallHookAction;
use gents::tool_call_lifecycle::{
    create_subagent_request_with_request_id, AwaitMode, CancelPolicy, ToolCallLifecycle,
};
use gents::{fetch_interrupt_requested_at, DefraSessionHook, FailurePolicy};
use gents_protocol::request_input::{QueuePolicy, QueueSource};
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;
use serde_json::json;

use crate::support::fixtures::{bind_behavior_backend, spawn_subagent_source};
use crate::support::{first_row, test_db};

const PARENT_BEHAVIOR_ID: &str = "r4-completion-parent";
const CHILD_BEHAVIOR_ID: &str = "r4-completion-child";
const BACKEND_ID: &str = "r4-completion-backend";
/// Stable endpoint used only to satisfy backend connectivity config; this
/// fixture drives persistence seams, never a live model call.
const BACKEND_ENDPOINT: &str = "http://127.0.0.1:1/v1";

#[derive(Debug, Deserialize)]
struct RequestSessionRow {
    session_id: String,
}

#[derive(Debug, Deserialize)]
struct ToolCallRow {
    result: Option<String>,
    lifecycle_state: Option<String>,
    await_mode: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MessageRow {
    sequence: u32,
    role: String,
    content: String,
    request_id: Option<String>,
    request_doc_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChildRequestStateRow {
    lifecycle_state: Option<String>,
    failure_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResponseStateRow {
    status: Option<String>,
    content: Option<String>,
    error_message: Option<String>,
}

/// Install the canonical configuration bundle and parent request/session for
/// every scenario through the shared desired-state owners: an explicit
/// `SubagentTargetDocument` (owner `agent_did`, destination
/// `target_agent_did`), a canonical `Tools` document selecting it by
/// `target_id`, an `AgentContext` binding that Tools doc, and an
/// `AgentBehavior` binding `context_id` + `inference_profile_id`. Inference
/// selection comes from `support::fixtures::bind_behavior_backend`, the
/// existing shared owner; no implicit bootstrap defaults are invented.
async fn install_canonical_behavior_bundle(node: &EmbeddedNode, agent_did: &str) {
    // Seed both explicit behavior chains through the shared fixture owner.
    // The parent and child may share a backend, but each behavior owns an
    // explicit profile and never relies on an inferred bootstrap default.
    bind_behavior_backend(
        node,
        agent_did,
        PARENT_BEHAVIOR_ID,
        BACKEND_ID,
        BACKEND_ENDPOINT,
        "test-model",
    )
    .await;
    bind_behavior_backend(
        node,
        agent_did,
        CHILD_BEHAVIOR_ID,
        BACKEND_ID,
        BACKEND_ENDPOINT,
        "test-model",
    )
    .await;
    let parent_profile_id = format!("{PARENT_BEHAVIOR_ID}-inference");
    let child_profile_id = format!("{CHILD_BEHAVIOR_ID}-inference");

    let target = SubagentTargetDocument {
        target_id: format!("{PARENT_BEHAVIOR_ID}:{CHILD_BEHAVIOR_ID}"),
        agent_did: agent_did.to_string(),
        target_agent_did: agent_did.to_string(),
        behavior_id: CHILD_BEHAVIOR_ID.to_string(),
        name: CHILD_BEHAVIOR_ID.to_string(),
        description: None,
        tags: Vec::new(),
    };
    let tools = Tools {
        tools_id: format!("{PARENT_BEHAVIOR_ID}:tools"),
        agent_did: agent_did.to_string(),
        subagents: Some(SubagentTools {
            target_ids: vec![target.target_id.clone()],
            spawn_enabled: Some(true),
            background_enabled: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    };
    let context = AgentContext {
        context_id: format!("{PARENT_BEHAVIOR_ID}:context"),
        agent_did: agent_did.to_string(),
        display_name: None,
        description: None,
        system_prompt: None,
        tools_id: Some(tools.tools_id.clone()),
        compaction_id: None,
        skill_ids: Vec::new(),
        tags: Vec::new(),
    };
    let parent_behavior = AgentBehavior {
        behavior_id: PARENT_BEHAVIOR_ID.to_string(),
        agent_did: agent_did.to_string(),
        display_name: Some("R4 completion parent".to_string()),
        description: None,
        context_id: Some(context.context_id.clone()),
        inference_profile_id: parent_profile_id,
        enabled: true,
        tags: Vec::new(),
        created_at: Some("2026-05-12T00:00:00Z".to_string()),
    };
    let child_behavior = AgentBehavior {
        behavior_id: CHILD_BEHAVIOR_ID.to_string(),
        agent_did: agent_did.to_string(),
        display_name: Some("R4 completion child".to_string()),
        description: None,
        context_id: None,
        inference_profile_id: child_profile_id,
        enabled: true,
        tags: Vec::new(),
        created_at: Some("2026-05-12T00:00:01Z".to_string()),
    };
    // One desired-state transaction installs the whole scoped bundle so
    // reference validation sees the complete same-owner closure. Every
    // document is the complete canonical replacement, addressed by owner DID.
    ConfigAccess::transact_local(node, None, "r4_completion.canonical_bundle", |txn| {
        let target = target.clone();
        let tools = tools.clone();
        let context = context.clone();
        let parent_behavior = parent_behavior.clone();
        let child_behavior = child_behavior.clone();
        Box::pin(async move {
            let mut documents = Vec::new();
            for (collection, value) in [
                (
                    gents::Collection::SubagentTarget,
                    serde_json::to_value(&target)?,
                ),
                (gents::Collection::Tools, serde_json::to_value(&tools)?),
                (
                    gents::Collection::AgentContext,
                    serde_json::to_value(&context)?,
                ),
                (
                    gents::Collection::AgentBehavior,
                    serde_json::to_value(&parent_behavior)?,
                ),
                (
                    gents::Collection::AgentBehavior,
                    serde_json::to_value(&child_behavior)?,
                ),
            ] {
                documents.push(DesiredStateApplyDocument {
                    collection,
                    add: value.clone(),
                    update: value,
                });
            }
            apply_desired_state_plan(txn, &DesiredStateApplyPlan::new(documents)?).await
        })
    })
    .await
    .unwrap();
}

async fn setup_fixture(test_name: &str) -> (crate::support::TestDb, String, String) {
    let db = test_db(test_name).await;
    let agent_did = db.node_identity.did().to_string();
    install_canonical_behavior_bundle(db.node.as_ref(), &agent_did).await;

    let session_id = format!("{test_name}-parent-session");
    let request_id = format!("{test_name}-parent-request");
    create_parent_request(db.node.as_ref(), &agent_did, &request_id, &session_id).await;
    create_parent_agent_session(db.node.as_ref(), &agent_did, &session_id).await;
    (db, session_id, request_id)
}

async fn create_parent_agent_session(node: &EmbeddedNode, agent_did: &str, session_id: &str) {
    // Canonical AgentSession is the single durable session document; create it
    // under the same principal that owns the parent request.
    crate::support::create_session_document(
        node,
        &gents_protocol::session::AgentSession {
            session_id: session_id.to_string(),
            agent_did: agent_did.to_string(),
            requester_did: None,
            behavior_id: PARENT_BEHAVIOR_ID.to_string(),
            created_at: "2026-05-12T00:00:00Z".to_string(),
            closed_at: None,
            title: None,
            tags: Vec::new(),
            provenance: None,
            observation: None,
        },
    )
    .await;
}

async fn create_parent_request(
    node: &EmbeddedNode,
    agent_did: &str,
    request_id: &str,
    session_id: &str,
) {
    let request_id = escape_graphql_string(request_id);
    let session_id = escape_graphql_string(session_id);
    let behavior_id = escape_graphql_string(PARENT_BEHAVIOR_ID);
    let agent_did = escape_graphql_string(agent_did);
    let now = chrono::Utc::now().to_rfc3339();
    let deadline = (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
    // RequestInput replaces the retired metadata bag. This fixture carries no
    // invocation extras, so the optional JSON field remains absent.
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
                input: null,
                lifecycle_state: "processing",
                backend_id: "",
                execution_origin: "interactive",
                failure_reason: "",
                created_at: "{now}",
                deadline: "{deadline}",
                retry_count: 0,
                max_retries: 3,
                subagent_depth: 0
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

async fn create_child_and_bridge(
    node: &std::sync::Arc<EmbeddedNode>,
    parent_request_id: &str,
    parent_session_id: &str,
    tool_call_id: &str,
    await_mode: AwaitMode,
    message_sequence: u32,
) -> (String, String) {
    let child_request_id = format!("{parent_request_id}-{tool_call_id}-child");
    let parent_request_doc_id =
        crate::support::exact_request_doc_id(node.as_ref(), parent_request_id).await;
    let parent_request_id_escaped = escape_graphql_string(parent_request_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{parent_request_id_escaped}" }} }}, limit: 1) {{ agent_did }} }}"#
        ))
        .await;
    let agent_did = response.data.expect("parent agent query data")["AgentRequest"][0]["agent_did"]
        .as_str()
        .expect("parent agent DID")
        .to_string();

    let mut lifecycle = ToolCallLifecycle::new_subagent(
        node.clone(),
        parent_request_id.to_string(),
        parent_session_id.to_string(),
        agent_did.clone(),
        tool_call_id.to_string(),
        message_sequence,
        "spawn_subagent".to_string(),
        serde_json::json!({
            "name": CHILD_BEHAVIOR_ID,
            "prompt": format!("prompt for {tool_call_id}"),
            "await_mode": await_mode.as_str()
        })
        .to_string(),
        chrono::Utc::now() + chrono::Duration::minutes(5),
        await_mode,
        CancelPolicy::Cascade,
        child_request_id.clone(),
        agent_did.clone(),
    )
    .with_request_doc_id(Some(parent_request_doc_id.clone()));
    lifecycle.start_running().await.unwrap();
    let parent_tool_call_doc_id = lifecycle.doc_id().expect("bridge document id").to_string();

    create_subagent_request_with_request_id(
        node.as_ref(),
        child_request_id.clone(),
        parent_request_id.to_string(),
        parent_request_doc_id,
        tool_call_id.to_string(),
        parent_tool_call_doc_id,
        0,
        agent_did,
        CHILD_BEHAVIOR_ID.to_string(),
        format!("prompt for {tool_call_id}"),
        Some(chrono::Utc::now() + chrono::Duration::minutes(4)),
    )
    .await
    .unwrap();
    let child_session_id = child_session_id(node.as_ref(), &child_request_id).await;

    (child_request_id, child_session_id)
}

async fn child_session_id(node: &EmbeddedNode, child_request_id: &str) -> String {
    let child_request_id = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{child_request_id}" }} }},
                limit: 1
            ) {{ session_id }}
        }}"#
    );
    first_row::<RequestSessionRow>(&node.execute(&query).await, "AgentRequest").session_id
}

async fn request_agent_did(node: &EmbeddedNode, request_id: &str) -> String {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{ agent_did }} }}"#
    );
    node.execute(&query)
        .await
        .data
        .expect("request agent query data")["AgentRequest"][0]["agent_did"]
        .as_str()
        .expect("request agent DID")
        .to_string()
}

async fn wait_for_child_for_tool(node: &EmbeddedNode, tool_call_id: &str) -> (String, String) {
    let escaped = escape_graphql_string(tool_call_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    caused_by_parent_tool_call_id: {{ _eq: "{escaped}" }}
                }},
                limit: 1
            ) {{ request_id session_id }}
        }}"#
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let response = node.execute(&query).await;
        if let Some(row) =
            crate::support::first_optional_row::<ChildForToolRow>(&response, "AgentRequest")
        {
            return (row.request_id, row.session_id);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for child AgentRequest for tool call {tool_call_id}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[derive(Debug, Deserialize)]
struct ChildForToolRow {
    request_id: String,
    session_id: String,
}

fn skip_reason(action: ToolCallHookAction) -> String {
    let ToolCallHookAction::Skip { reason } = action else {
        panic!("expected Skip action, got {action:?}");
    };
    reason
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
                input: {{ lifecycle_state: "completed" }}
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
        content: vec![AssistantContent::Text(Text {
            text: final_response.to_string(),
        })],
    };
    let escaped_message = escape_graphql_string(&serde_json::to_string(&assistant).unwrap());
    let escaped_child_session_id = escape_graphql_string(child_session_id);
    let now = chrono::Utc::now().to_rfc3339();
    // Stamp the physical child request binding plus the child's owning
    // principal on the transcript row so the canonical final-response reader
    // (`load_child_final_response`) resolves it under the exact child scope.
    let escaped_agent_did = escape_graphql_string(&request_agent_did(node, child_request_id).await);
    let escaped_request_doc_id =
        escape_graphql_string(&crate::support::exact_request_doc_id(node, child_request_id).await);
    let create_message = format!(
        r#"mutation {{
            create_AgentMessage(input: {{
                message_key: "{escaped_child_session_id}:1",
                session_id: "{escaped_child_session_id}",
                agent_did: "{escaped_agent_did}",
                requester_did: null,
                request_id: "{escaped_child_request_id}",
                request_doc_id: "{escaped_request_doc_id}",
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
                request_doc_id: "{escaped_request_doc_id}",
                agent_did: "{escaped_agent_did}",
                behavior_id: "{escaped_child_behavior_id}",
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
        }}"#,
        escaped_child_behavior_id = escape_graphql_string(CHILD_BEHAVIOR_ID),
    );
    let response = node.execute(&create_response).await;
    assert!(
        !response.has_errors(),
        "create child AgentResponse failed: {:?}",
        response.errors
    );
}

async fn set_request_lifecycle(node: &EmbeddedNode, request_id: &str, state: &str) {
    let request_id = escape_graphql_string(request_id);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                input: {{ lifecycle_state: "{state}" }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "set request lifecycle failed: {:?}",
        response.errors
    );
}

async fn set_child_processing_deadline(
    node: &EmbeddedNode,
    request_id: &str,
    deadline: chrono::DateTime<chrono::Utc>,
) {
    let request_id = escape_graphql_string(request_id);
    let deadline = escape_graphql_string(&deadline.to_rfc3339());
    let lease_expiry =
        escape_graphql_string(&(chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                input: {{
                    lifecycle_state: "processing",
                    deadline: "{deadline}",
                    execution_generation: "child-deadline-owner",
                    execution_lease_expires_at: "{lease_expiry}",
                    execution_progress_seq: 1
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "set child processing deadline failed: {:?}",
        response.errors
    );
}

async fn create_streaming_child_response(
    node: &EmbeddedNode,
    child_request_id: &str,
    child_session_id: &str,
    content: &str,
) {
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let request_doc_id =
        escape_graphql_string(&crate::support::exact_request_doc_id(node, child_request_id).await);
    let escaped_child_session_id = escape_graphql_string(child_session_id);
    let escaped_content = escape_graphql_string(content);
    let escaped_agent_did = escape_graphql_string(&request_agent_did(node, child_request_id).await);
    let escaped_behavior_id = escape_graphql_string(CHILD_BEHAVIOR_ID);
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_AgentResponse(input: {{
                response_key: "{escaped_child_request_id}",
                request_id: "{escaped_child_request_id}",
                request_doc_id: "{request_doc_id}",
                agent_did: "{escaped_agent_did}",
                behavior_id: "{escaped_behavior_id}",
                session_id: "{escaped_child_session_id}",
                content: "{escaped_content}",
                reasoning: "",
                status: "streaming",
                error_message: "",
                token_count: 0,
                progress_seq: 1,
                created_at: "{now}"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create streaming child AgentResponse failed: {:?}",
        response.errors
    );
}

async fn fetch_child_request_state(
    node: &EmbeddedNode,
    child_request_id: &str,
) -> ChildRequestStateRow {
    let child_request_id = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{child_request_id}" }} }},
                limit: 1
            ) {{
                lifecycle_state
                failure_reason
            }}
        }}"#
    );
    first_row(&node.execute(&query).await, "AgentRequest")
}

async fn fetch_response_state(node: &EmbeddedNode, request_id: &str) -> ResponseStateRow {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentResponse(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                limit: 1
            ) {{
                status
                content
                error_message
            }}
        }}"#
    );
    first_row(&node.execute(&query).await, "AgentResponse")
}

async fn fetch_tool_call(node: &EmbeddedNode, session_id: &str, tool_call_id: &str) -> ToolCallRow {
    let session_id = escape_graphql_string(session_id);
    let tool_call_id = escape_graphql_string(tool_call_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    tool_call_id: {{ _eq: "{tool_call_id}" }}
                }},
                limit: 1
            ) {{ result lifecycle_state await_mode }}
        }}"#
    );
    first_row(&node.execute(&query).await, "AgentToolCall")
}

async fn fetch_parent_messages(node: &EmbeddedNode, session_id: &str) -> Vec<MessageRow> {
    let session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                order: {{ sequence: ASC }}
            ) {{ sequence role content request_id request_doc_id }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "message query failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

/// Read the durable background-completion wake rows through the canonical
/// typed input: select the bare `input` field, decode
/// `gents_protocol::row::AgentRequestRow`, and match the coalescing owner's
/// `QueueSource::BackgroundCompletion` source with the coalesce policy. The
/// legacy `metadata` bag reader is retired; missing or malformed typed input
/// fails decoding loudly instead of silently passing the count assertions.
async fn fetch_scheduled_wakes(node: &EmbeddedNode, session_id: &str) -> Vec<AgentRequestRow> {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                request_id
                session_id
                content
                lifecycle_state
                execution_origin
                input
                created_at
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "wake query failed: {:?}",
        response.errors
    );
    let rows = gents::graphql::rows::<AgentRequestRow>(&response, "AgentRequest")
        .expect("decode wake AgentRequest rows");
    rows.into_iter()
        .filter(|row| {
            row.session_id.as_deref() == Some(session_id)
                && row.execution_origin.as_deref() == Some("scheduled")
                && row
                    .input
                    .as_ref()
                    .and_then(|input| input.queue.as_ref())
                    .is_some_and(|queue| {
                        queue.source == QueueSource::BackgroundCompletion
                            && queue.policy == QueuePolicy::Coalesce
                    })
        })
        .collect()
}

#[tokio::test]
async fn background_completion_projects_bridge_notifies_and_enqueues_wake() {
    let (db, session_id, parent_request_id) = setup_fixture("background_completion_project").await;
    let (child_request_id, child_session_id) = create_child_and_bridge(
        &db.node,
        &parent_request_id,
        &session_id,
        "spawn-bg-1",
        AwaitMode::Background,
        1,
    )
    .await;
    persist_child_completion(
        db.node.as_ref(),
        &child_request_id,
        &child_session_id,
        "child final answer <ok>",
    )
    .await;

    let outcome = project_background_subagent_completion(
        db.node.clone(),
        &child_request_id,
        db.node_identity.did(),
    )
    .await
    .unwrap();
    assert!(matches!(
        outcome,
        BackgroundCompletionOutcome::Projected { .. }
    ));

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "spawn-bg-1").await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("completed"));
    assert_eq!(tool.await_mode.as_deref(), Some("background"));
    assert_eq!(tool.result.as_deref(), Some("child final answer <ok>"));

    let messages = fetch_parent_messages(db.node.as_ref(), &session_id).await;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].sequence, 3);
    assert_eq!(messages[0].role, "user");
    assert!(messages[0].content.contains(r#"<subagent-notification"#));
    assert!(messages[0].content.contains(r#"status="completed""#));
    assert!(messages[0]
        .content
        .contains("child final answer &lt;ok&gt;"));

    let wakes = fetch_scheduled_wakes(db.node.as_ref(), &session_id).await;
    assert_eq!(wakes.len(), 1);

    let again = project_background_subagent_completion(
        db.node.clone(),
        &child_request_id,
        db.node_identity.did(),
    )
    .await
    .unwrap();
    assert_eq!(again, BackgroundCompletionOutcome::AlreadyProjected);
    assert_eq!(
        fetch_parent_messages(db.node.as_ref(), &session_id)
            .await
            .len(),
        1
    );
    assert_eq!(
        fetch_scheduled_wakes(db.node.as_ref(), &session_id)
            .await
            .len(),
        1
    );
}

#[tokio::test]
async fn background_completion_recovers_side_effects_after_bridge_already_projected() {
    let (db, session_id, parent_request_id) = setup_fixture("background_completion_recovery").await;
    let (child_request_id, child_session_id) = create_child_and_bridge(
        &db.node,
        &parent_request_id,
        &session_id,
        "spawn-bg-recover",
        AwaitMode::Background,
        1,
    )
    .await;
    persist_child_completion(
        db.node.as_ref(),
        &child_request_id,
        &child_session_id,
        "child completed before observer side effects",
    )
    .await;

    let mut lifecycle = ToolCallLifecycle::load(db.node.clone(), &session_id, "spawn-bg-recover")
        .await
        .unwrap()
        .expect("bridge should exist");
    assert!(lifecycle
        .bridge_complete("child completed before observer side effects".to_string())
        .await
        .unwrap());
    assert!(fetch_parent_messages(db.node.as_ref(), &session_id)
        .await
        .is_empty());
    assert!(fetch_scheduled_wakes(db.node.as_ref(), &session_id)
        .await
        .is_empty());

    let outcome = project_background_subagent_completion(
        db.node.clone(),
        &child_request_id,
        db.node_identity.did(),
    )
    .await
    .unwrap();
    assert!(matches!(
        outcome,
        BackgroundCompletionOutcome::Projected { .. }
    ));
    let messages = fetch_parent_messages(db.node.as_ref(), &session_id).await;
    assert_eq!(messages.len(), 1);
    assert!(messages[0]
        .content
        .contains("child completed before observer side effects"));
    assert_eq!(
        fetch_scheduled_wakes(db.node.as_ref(), &session_id)
            .await
            .len(),
        1
    );

    let again = project_background_subagent_completion(
        db.node.clone(),
        &child_request_id,
        db.node_identity.did(),
    )
    .await
    .unwrap();
    assert_eq!(again, BackgroundCompletionOutcome::AlreadyProjected);
    assert_eq!(
        fetch_parent_messages(db.node.as_ref(), &session_id)
            .await
            .len(),
        1
    );
    assert_eq!(
        fetch_scheduled_wakes(db.node.as_ref(), &session_id)
            .await
            .len(),
        1
    );
}

#[tokio::test]
async fn background_notification_sorts_after_reserved_spawn_tool_result() {
    let (db, session_id, parent_request_id) = setup_fixture("background_completion_order").await;
    let _source = spawn_subagent_source(
        db.node.clone(),
        db.node_identity.did(),
        PARENT_BEHAVIOR_ID,
        CHILD_BEHAVIOR_ID,
    );
    let hook = DefraSessionHook::resume_with_identity_policy(
        db.node.clone(),
        &session_id,
        PARENT_BEHAVIOR_ID,
        db.node_identity.did(),
        None,
        FailurePolicy::default(),
    )
    .await
    .unwrap();
    hook.set_active_request_lineage(Some(parent_request_id.clone()), None)
        .await
        .expect("bind persisted request lineage");
    hook.set_request_deadline_at(Some(chrono::Utc::now() + chrono::Duration::minutes(5)))
        .await;

    let args = json!({
        "name": CHILD_BEHAVIOR_ID,
        "prompt": "background child can complete quickly",
        "await_mode": "background"
    })
    .to_string();
    let action = hook
        .on_tool_call(
            "spawn_subagent",
            Some("model-call-order".to_string()),
            "spawn-bg-order",
            &args,
        )
        .await;
    let receipt = skip_reason(action);
    let (child_request_id, child_session_id) =
        wait_for_child_for_tool(db.node.as_ref(), "spawn-bg-order").await;
    persist_child_completion(
        db.node.as_ref(),
        &child_request_id,
        &child_session_id,
        "fast background child done",
    )
    .await;
    project_background_subagent_completion(
        db.node.clone(),
        &child_request_id,
        db.node_identity.did(),
    )
    .await
    .unwrap();

    let messages_before_parent_persists =
        fetch_parent_messages(db.node.as_ref(), &session_id).await;
    assert_eq!(messages_before_parent_persists.len(), 1);
    assert_eq!(messages_before_parent_persists[0].sequence, 3);

    hook.persist_message(&Message::Assistant {
        id: None,
        content: vec![AssistantContent::ToolCall(ToolCall {
            id: "spawn-bg-order".to_string(),
            call_id: Some("model-call-order".to_string()),
            function: ToolFunction {
                name: "spawn_subagent".to_string(),
                arguments: serde_json::from_str(&args).unwrap(),
            },
            signature: None,
            additional_params: None,
        })],
    })
    .await
    .unwrap();
    hook.persist_stream_tool_result_message(
        &ToolResult {
            id: "spawn-bg-order".to_string(),
            call_id: Some("model-call-order".to_string()),
            content: vec![ToolResultContent::Text(Text { text: receipt })],
        },
        "spawn-bg-order",
    )
    .await
    .unwrap();

    let messages = fetch_parent_messages(db.node.as_ref(), &session_id).await;
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].sequence, 1);
    assert_eq!(messages[0].role, "assistant");
    assert!(messages[0].content.contains("spawn_subagent"));
    assert_eq!(messages[1].sequence, 2);
    assert_eq!(messages[1].role, "user");
    assert!(messages[1].content.contains("child_request_id"));
    assert_eq!(messages[2].sequence, 3);
    assert!(messages[2].content.contains("<subagent-notification"));
    let wakes = fetch_scheduled_wakes(db.node.as_ref(), &session_id).await;
    assert_eq!(wakes.len(), 1);
    assert_eq!(
        messages[2].request_id.as_deref(),
        Some(wakes[0].request_id.as_str())
    );
    assert_eq!(
        messages[2].request_doc_id.as_deref(),
        wakes[0].doc_id.as_deref()
    );
}

#[tokio::test]
async fn background_completion_compacts_multibyte_summary_without_panicking() {
    let (db, session_id, parent_request_id) = setup_fixture("background_completion_unicode").await;
    let (child_request_id, child_session_id) = create_child_and_bridge(
        &db.node,
        &parent_request_id,
        &session_id,
        "spawn-bg-unicode",
        AwaitMode::Background,
        1,
    )
    .await;
    let final_response = "é".repeat(3000);
    persist_child_completion(
        db.node.as_ref(),
        &child_request_id,
        &child_session_id,
        &final_response,
    )
    .await;

    let outcome = project_background_subagent_completion(
        db.node.clone(),
        &child_request_id,
        db.node_identity.did(),
    )
    .await
    .unwrap();
    assert!(matches!(
        outcome,
        BackgroundCompletionOutcome::Projected { .. }
    ));
    let messages = fetch_parent_messages(db.node.as_ref(), &session_id).await;
    assert_eq!(messages.len(), 1);
    assert!(messages[0].content.contains("<subagent-notification"));
    assert!(messages[0].content.contains("..."));
}

#[tokio::test]
async fn multiple_background_completions_append_notifications_and_coalesce_wake() {
    let (db, session_id, parent_request_id) = setup_fixture("background_completion_coalesce").await;
    let (child_a, session_a) = create_child_and_bridge(
        &db.node,
        &parent_request_id,
        &session_id,
        "spawn-bg-a",
        AwaitMode::Background,
        1,
    )
    .await;
    let (child_b, session_b) = create_child_and_bridge(
        &db.node,
        &parent_request_id,
        &session_id,
        "spawn-bg-b",
        AwaitMode::Background,
        1,
    )
    .await;
    persist_child_completion(db.node.as_ref(), &child_a, &session_a, "child A done").await;
    persist_child_completion(db.node.as_ref(), &child_b, &session_b, "child B done").await;

    let first =
        project_background_subagent_completion(db.node.clone(), &child_a, db.node_identity.did())
            .await
            .unwrap();
    let second =
        project_background_subagent_completion(db.node.clone(), &child_b, db.node_identity.did())
            .await
            .unwrap();
    assert!(matches!(
        first,
        BackgroundCompletionOutcome::Projected { .. }
    ));
    assert!(matches!(
        second,
        BackgroundCompletionOutcome::Projected { .. }
    ));
    let messages = fetch_parent_messages(db.node.as_ref(), &session_id).await;
    assert_eq!(messages.len(), 2);
    assert!(messages[0].content.contains("child A done"));
    assert!(messages[1].content.contains("child B done"));
    assert_eq!(
        fetch_scheduled_wakes(db.node.as_ref(), &session_id)
            .await
            .len(),
        1
    );
}

#[tokio::test]
async fn background_completion_does_not_interrupt_active_foreground_parent() {
    let (db, session_id, parent_request_id) =
        setup_fixture("background_completion_interleave").await;
    let (foreground_child, _) = create_child_and_bridge(
        &db.node,
        &parent_request_id,
        &session_id,
        "spawn-fg-a",
        AwaitMode::Foreground,
        1,
    )
    .await;
    set_request_lifecycle(db.node.as_ref(), &foreground_child, "processing").await;

    let (background_child, background_session) = create_child_and_bridge(
        &db.node,
        &parent_request_id,
        &session_id,
        "spawn-bg-b",
        AwaitMode::Background,
        2,
    )
    .await;
    persist_child_completion(
        db.node.as_ref(),
        &background_child,
        &background_session,
        "background child B done",
    )
    .await;

    let outcome = project_background_subagent_completion(
        db.node.clone(),
        &background_child,
        db.node_identity.did(),
    )
    .await
    .unwrap();
    assert!(matches!(
        outcome,
        BackgroundCompletionOutcome::Projected { .. }
    ));
    assert_eq!(
        fetch_parent_messages(db.node.as_ref(), &session_id)
            .await
            .len(),
        1
    );

    let wakes = fetch_scheduled_wakes(db.node.as_ref(), &session_id).await;
    assert_eq!(wakes.len(), 1);
}

#[tokio::test]
async fn recovery_leaves_running_background_bridge_after_clean_parent_completion() {
    let (db, session_id, parent_request_id) =
        setup_fixture("background_completion_recovery_skip").await;
    let (child_request_id, _) = create_child_and_bridge(
        &db.node,
        &parent_request_id,
        &session_id,
        "spawn-bg-recovery-skip",
        AwaitMode::Background,
        1,
    )
    .await;
    set_request_lifecycle(db.node.as_ref(), &child_request_id, "processing").await;
    set_request_lifecycle(db.node.as_ref(), &parent_request_id, "completed").await;

    let report = ToolCallLifecycle::recover_all(db.node.as_ref(), db.node_identity.did())
        .await
        .unwrap();
    assert_eq!(report.tool_calls_recovered, 0);

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "spawn-bg-recovery-skip").await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("running"));
    assert_eq!(tool.await_mode.as_deref(), Some("background"));
    let interrupt = fetch_interrupt_requested_at(db.node.as_ref(), &child_request_id)
        .await
        .unwrap();
    assert!(
        interrupt.is_none(),
        "clean parent completion must not interrupt its linked background child"
    );
}

#[tokio::test]
async fn recovery_terminalizes_expired_background_child_before_projection() {
    let (db, session_id, parent_request_id) =
        setup_fixture("background_completion_expired_child").await;
    let (child_request_id, child_session_id) = create_child_and_bridge(
        &db.node,
        &parent_request_id,
        &session_id,
        "spawn-bg-expired-child",
        AwaitMode::Background,
        1,
    )
    .await;
    let expired_deadline = chrono::Utc::now() - chrono::Duration::seconds(1);
    set_child_processing_deadline(db.node.as_ref(), &child_request_id, expired_deadline).await;
    create_streaming_child_response(
        db.node.as_ref(),
        &child_request_id,
        &child_session_id,
        "partial child output",
    )
    .await;

    let report = ToolCallLifecycle::recover_all(db.node.as_ref(), db.node_identity.did())
        .await
        .unwrap();
    assert_eq!(report.tool_calls_recovered, 1);

    let child = fetch_child_request_state(db.node.as_ref(), &child_request_id).await;
    assert_eq!(child.lifecycle_state.as_deref(), Some("dead"));
    assert!(
        child
            .failure_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("child request deadline exceeded")),
        "child failure reason should explain deadline expiry: {:?}",
        child.failure_reason
    );

    let response = fetch_response_state(db.node.as_ref(), &child_request_id).await;
    assert_eq!(response.status.as_deref(), Some("error"));
    assert_eq!(response.content.as_deref(), Some("partial child output"));
    let request_doc_id = crate::support::exact_request_doc_id(&db.node, &child_request_id).await;
    let owner = db
        .node
        .execute(&format!(
            r#"{{
            AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}) {{ execution_generation }}
            AgentResponse(filter: {{ request_doc_id: {{ _eq: "{doc_id}" }} }}) {{ _docID }}
        }}"#,
            doc_id = escape_graphql_string(&request_doc_id),
        ))
        .await;
    assert!(!owner.has_errors(), "{:?}", owner.errors);
    let owner = owner.data.unwrap();
    assert_ne!(
        owner["AgentRequest"][0]["execution_generation"],
        "child-deadline-owner"
    );
    assert_eq!(owner["AgentResponse"].as_array().unwrap().len(), 1);
    assert!(
        response
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("child request deadline exceeded")),
        "response error message should explain deadline expiry: {:?}",
        response.error_message
    );

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "spawn-bg-expired-child").await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("failed"));
    assert_eq!(tool.await_mode.as_deref(), Some("background"));

    let messages = fetch_parent_messages(db.node.as_ref(), &session_id).await;
    assert_eq!(messages.len(), 1);
    assert!(messages[0].content.contains(r#"<subagent-notification"#));
    assert!(messages[0].content.contains(r#"status="dead""#));
    assert!(messages[0].content.contains(&child_request_id));

    let wakes = fetch_scheduled_wakes(db.node.as_ref(), &session_id).await;
    assert_eq!(wakes.len(), 1);
}

#[tokio::test]
async fn stale_hook_sequence_does_not_overwrite_background_notification() {
    let (db, session_id, parent_request_id) =
        setup_fixture("background_completion_hook_sequence").await;
    let hook = DefraSessionHook::resume_with_identity_policy(
        db.node.clone(),
        &session_id,
        PARENT_BEHAVIOR_ID,
        db.node_identity.did(),
        None,
        FailurePolicy::default(),
    )
    .await
    .unwrap();

    let (child_request_id, child_session_id) = create_child_and_bridge(
        &db.node,
        &parent_request_id,
        &session_id,
        "spawn-bg-stale-hook",
        AwaitMode::Background,
        1,
    )
    .await;
    persist_child_completion(
        db.node.as_ref(),
        &child_request_id,
        &child_session_id,
        "notification must survive",
    )
    .await;
    project_background_subagent_completion(
        db.node.clone(),
        &child_request_id,
        db.node_identity.did(),
    )
    .await
    .unwrap();

    hook.persist_message(&Message::User {
        content: vec![UserContent::Text(Text {
            text: "parent hook resumes".to_string(),
        })],
    })
    .await
    .unwrap();

    let messages = fetch_parent_messages(db.node.as_ref(), &session_id).await;
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].sequence, 3);
    assert!(messages[0].content.contains("notification must survive"));
    assert_eq!(messages[1].sequence, 4);
    assert!(messages[1].content.contains("parent hook resumes"));
}
