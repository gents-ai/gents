use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::llm::ToolCallHookAction;
use gents::tool_call_lifecycle::{
    create_subagent_request_with_request_id, AwaitMode, CancelPolicy,
};
use gents::{fetch_interrupt_requested_at, DefraSessionHook, FailurePolicy};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde_json::{json, Value};

use super::r4c_private_support::{accepted_call, bind_accepted_request};
use crate::support::fixtures::{
    configure_subagent_behavior, spawn_subagent_source, subagent_target,
};
use crate::support::test_db;

const PARENT_BEHAVIOR_ID: &str = "r4c-parent";
const CHILD_BEHAVIOR_ID: &str = "r4c-child";

async fn setup_db(
    name: &str,
) -> (
    crate::support::TestDb,
    crate::support::fixtures::SubagentSourceGuard,
) {
    let db = test_db(name).await;
    let agent_did = db.node_identity.did().to_string();
    configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        CHILD_BEHAVIOR_ID,
        "r4c-child-tools",
        Vec::new(),
        false,
        false,
        None,
    )
    .await;
    configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        PARENT_BEHAVIOR_ID,
        "r4c-parent-tools",
        vec![subagent_target(
            &agent_did,
            CHILD_BEHAVIOR_ID,
            &agent_did,
            CHILD_BEHAVIOR_ID,
        )],
        true,
        true,
        None,
    )
    .await;
    let source = spawn_subagent_source(
        db.node.clone(),
        &agent_did,
        PARENT_BEHAVIOR_ID,
        CHILD_BEHAVIOR_ID,
    );
    (db, source)
}

async fn create_parent_hook(
    db: &crate::support::TestDb,
    request_id: &str,
    session_id: &str,
) -> DefraSessionHook {
    let deadline = chrono::Utc::now() + chrono::Duration::minutes(5);
    gents::session::ensure_session_with_behavior_id_and_requester_did(
        db.node.as_ref(),
        session_id,
        PARENT_BEHAVIOR_ID,
        db.node_identity.did(),
        PARENT_BEHAVIOR_ID,
        Some(db.node_identity.did()),
    )
    .await
    .unwrap();
    let hook = DefraSessionHook::resume_with_identity_policy(
        db.node.clone(),
        session_id,
        PARENT_BEHAVIOR_ID,
        db.node_identity.did(),
        Some(db.node_identity.did()),
        FailurePolicy::default(),
    )
    .await
    .unwrap();
    bind_accepted_request(
        db,
        &hook,
        PARENT_BEHAVIOR_ID,
        request_id,
        session_id,
        deadline,
    )
    .await;
    hook
}

async fn spawn_background_child(
    node: &EmbeddedNode,
    hook: &DefraSessionHook,
    internal_call_id: &str,
    prompt: &str,
) -> Value {
    let args = json!({
        "name": CHILD_BEHAVIOR_ID,
        "prompt": prompt,
        "await_mode": "background"
    })
    .to_string();
    let action = accepted_call(
        hook,
        "spawn_subagent",
        Some(format!("model-{internal_call_id}")),
        internal_call_id,
        &args,
    )
    .await;
    let mut receipt = skip_reason_json(action);
    assert_eq!(receipt["ok"], true);
    let child_request_id = receipt["child_request_id"]
        .as_str()
        .expect("child_request_id")
        .to_string();
    let child_session_id = wait_for_child_session_id(node, &child_request_id).await;
    receipt["child_session_id"] = Value::String(child_session_id);
    receipt
}

async fn wait_for_child_session_id(node: &EmbeddedNode, child_request_id: &str) -> String {
    let escaped = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped}" }} }},
                limit: 1
            ) {{ request_id session_id }}
        }}"#
    );
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let response = node.execute(&query).await;
        if let Some(row) =
            crate::support::first_optional_row::<AgentRequestRow>(&response, "AgentRequest")
        {
            if let Some(session_id) = row.session_id.filter(|value| !value.is_empty()) {
                return session_id;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for child AgentRequest {child_request_id} session id"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

async fn steer_subagent(hook: &DefraSessionHook, internal_call_id: &str, args: Value) -> Value {
    let action = accepted_call(
        hook,
        "steer_subagent",
        Some(format!("model-{internal_call_id}")),
        internal_call_id,
        &args.to_string(),
    )
    .await;
    skip_reason_json(action)
}

fn skip_reason_json(action: ToolCallHookAction) -> Value {
    let ToolCallHookAction::Skip { reason } = action else {
        panic!("expected Skip action, got {action:?}");
    };
    serde_json::from_str(&reason).expect("skip reason should be JSON")
}

async fn fetch_request(node: &EmbeddedNode, request_id: &str) -> AgentRequestRow {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{
                request_id
                session_id
                behavior_id
                content
                lifecycle_state
                input
                subagent_depth
                caused_by_parent_request_id
                caused_by_parent_request_doc_id
                caused_by_parent_tool_call_id
                caused_by_parent_tool_call_doc_id
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    crate::support::first_row(&response, "AgentRequest")
}

async fn update_request_state(node: &EmbeddedNode, request_id: &str, lifecycle_state: &str) {
    let request_id = escape_graphql_string(request_id);
    let lifecycle_state = escape_graphql_string(lifecycle_state);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                input: {{
                    lifecycle_state: "{lifecycle_state}"
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
    agent_did: &str,
    request_id: &str,
    session_id: &str,
    execution_origin: &str,
    input: &str,
) {
    let request_id = escape_graphql_string(request_id);
    let agent_did = escape_graphql_string(agent_did);
    let behavior_id = escape_graphql_string(CHILD_BEHAVIOR_ID);
    let session_id = escape_graphql_string(session_id);
    let execution_origin = escape_graphql_string(execution_origin);
    let input = serde_json::from_str::<serde_json::Value>(input).expect("request input JSON");
    let input =
        gents_protocol::graphql::graphql_input_literal(&input).expect("request input GraphQL");
    let now = chrono::Utc::now();
    let created_at = escape_graphql_string(&now.to_rfc3339());
    let deadline = escape_graphql_string(&(now + chrono::Duration::minutes(5)).to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "{agent_did}",
                requester_did: "{agent_did}",
                behavior_id: "{behavior_id}",
                session_id: "{session_id}",
                retry_parent_request: "",
                retry_root_request: "{request_id}",
                superseded_by_request: "",
                content: "queued child session request",
                lifecycle_state: "pending",
                backend_id: "",
                execution_origin: "{execution_origin}",
                input: {input},
                failure_reason: "",
                created_at: "{created_at}",
                deadline: "{deadline}",
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

fn queue_metadata(
    source: &str,
    policy: &str,
    key: Option<&str>,
    queued_after_request_id: Option<&str>,
) -> String {
    let mut input = json!({
        "queue": {
            "source": source,
            "policy": policy,
            "key": key,
            "queued_after_request_id": queued_after_request_id
        }
    });
    if source == "background_completion" {
        input["queue"]["background_completion_wake_version"] = json!(1);
    }
    input.to_string()
}

#[tokio::test]
async fn steer_subagent_append_enqueues_with_steering_source() {
    let (db, _source) = setup_db("r4c-steer-append").await;
    let hook = create_parent_hook(&db, "parent-append", "session-append").await;
    let child = spawn_background_child(db.node.as_ref(), &hook, "spawn-append", "do work").await;
    let child_request_id = child["child_request_id"].as_str().unwrap();
    let child_session_id = child["child_session_id"].as_str().unwrap();

    let result = steer_subagent(
        &hook,
        "steer-append",
        json!({
            "child_request_id": child_request_id,
            "message": "also check the staging config",
            "interrupt": false
        }),
    )
    .await;
    let queued_request_id = result["queued_request_id"].as_str().unwrap();
    assert_eq!(result["interrupted_active_request_id"], Value::Null);
    assert!(result["drained_wake_up_request_ids"]
        .as_array()
        .unwrap()
        .is_empty());

    let queued = fetch_request(db.node.as_ref(), queued_request_id).await;
    assert_eq!(queued.session_id.as_deref(), Some(child_session_id));
    assert_eq!(queued.behavior_id.as_deref(), Some(CHILD_BEHAVIOR_ID));
    assert_eq!(
        queued.content.as_deref(),
        Some("also check the staging config")
    );
    assert_eq!(queued.subagent_depth, Some(1));
    let parent_request_doc_id =
        crate::support::exact_request_doc_id(db.node.as_ref(), child_request_id).await;
    assert_eq!(
        queued.caused_by_parent_request_id.as_deref(),
        Some(child_request_id)
    );
    assert_eq!(
        queued.caused_by_parent_request_doc_id.as_deref(),
        Some(parent_request_doc_id.as_str())
    );
    assert_eq!(queued.caused_by_parent_tool_call_id.as_deref(), None);
    assert_eq!(queued.caused_by_parent_tool_call_doc_id.as_deref(), None);
    assert_eq!(queued.lifecycle_state, Some(RequestLifecycleState::Pending));
    let queue = queued.input.as_ref().unwrap().queue.as_ref().unwrap();
    assert_eq!(
        queue.source,
        gents_protocol::request_input::QueueSource::Steering
    );
    assert_eq!(
        queue.policy,
        gents_protocol::request_input::QueuePolicy::Append
    );
}

#[tokio::test]
async fn steer_subagent_append_persists_admission_without_transcript_message() {
    let (db, _source) = setup_db("r4c-steer-message").await;
    let hook = create_parent_hook(&db, "parent-message", "session-message").await;
    let child = spawn_background_child(db.node.as_ref(), &hook, "spawn-message", "do work").await;
    let child_request_id = child["child_request_id"].as_str().unwrap();
    let child_session_id = child["child_session_id"].as_str().unwrap();

    let result = steer_subagent(
        &hook,
        "steer-message",
        json!({
            "child_request_id": child_request_id,
            "message": "also check the staging config"
        }),
    )
    .await;

    let queued_request_id = result["queued_request_id"].as_str().unwrap();
    let queued = fetch_request(db.node.as_ref(), queued_request_id).await;
    assert_eq!(
        queued.content.as_deref(),
        Some("also check the staging config")
    );
    let session_id = escape_graphql_string(child_session_id);
    let response = db.node.execute(&format!(
        r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ _docID }} }}"#
    )).await;
    let rows: Vec<Value> = gents::graphql::rows(&response, "AgentMessage").unwrap();
    assert!(
        rows.is_empty(),
        "enqueue must not publish transcript output"
    );
}

#[tokio::test]
async fn steer_subagent_rejects_terminal_child() {
    let (db, _source) = setup_db("r4c-steer-terminal").await;
    let hook = create_parent_hook(&db, "parent-terminal", "session-terminal").await;
    let child = spawn_background_child(db.node.as_ref(), &hook, "spawn-terminal", "do work").await;
    let child_request_id = child["child_request_id"].as_str().unwrap();
    update_request_state(db.node.as_ref(), child_request_id, "completed").await;

    let result = steer_subagent(
        &hook,
        "steer-terminal",
        json!({
            "child_request_id": child_request_id,
            "message": "do more"
        }),
    )
    .await;
    assert_eq!(result["ok"].as_bool(), Some(false));
    assert_eq!(
        result["failure_class"].as_str(),
        Some("invalid_tool_arguments")
    );
}

#[tokio::test]
async fn steer_subagent_rejects_unauthorized_child() {
    let (db, _source) = setup_db("r4c-steer-unauthorized").await;
    let hook_1 = create_parent_hook(&db, "parent-one", "session-one").await;
    let hook_2 = create_parent_hook(&db, "parent-two", "session-two").await;
    let child = spawn_background_child(db.node.as_ref(), &hook_2, "spawn-sibling", "do work").await;
    let child_request_id = child["child_request_id"].as_str().unwrap();

    let result = steer_subagent(
        &hook_1,
        "steer-unauthorized",
        json!({
            "child_request_id": child_request_id,
            "message": "hi"
        }),
    )
    .await;
    assert_eq!(result["ok"].as_bool(), Some(false));
    assert_eq!(result["failure_class"].as_str(), Some("tool_not_allowed"));
}

#[tokio::test]
async fn steer_subagent_keeps_one_accepted_parent_control_row() {
    let (db, _source) = setup_db("r4c-steer-no-row").await;
    let parent_session_id = "session-no-row";
    let hook = create_parent_hook(&db, "parent-no-row", parent_session_id).await;
    let child = spawn_background_child(db.node.as_ref(), &hook, "spawn-no-row", "do work").await;
    let child_request_id = child["child_request_id"].as_str().unwrap();

    let _ = steer_subagent(
        &hook,
        "steer-no-row",
        json!({
            "child_request_id": child_request_id,
            "message": "x"
        }),
    )
    .await;

    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), parent_session_id, "steer_subagent").await,
        1
    );
}

#[tokio::test]
async fn steer_subagent_interrupt_latches_active_child_request() {
    let (db, _source) = setup_db("r4c-steer-interrupt").await;
    let hook = create_parent_hook(&db, "parent-interrupt", "session-interrupt").await;
    let child = spawn_background_child(db.node.as_ref(), &hook, "spawn-interrupt", "do work").await;
    let child_request_id = child["child_request_id"].as_str().unwrap();
    update_request_state(db.node.as_ref(), child_request_id, "claimed").await;

    let result = steer_subagent(
        &hook,
        "steer-interrupt",
        json!({
            "child_request_id": child_request_id,
            "message": "stop, do this instead",
            "interrupt": true
        }),
    )
    .await;

    assert_eq!(
        result["interrupted_active_request_id"].as_str(),
        Some(child_request_id)
    );
    assert!(
        fetch_interrupt_requested_at(db.node.as_ref(), child_request_id)
            .await
            .unwrap()
            .is_some()
    );
    let queued = fetch_request(
        db.node.as_ref(),
        result["queued_request_id"].as_str().unwrap(),
    )
    .await;
    let queue = queued.input.as_ref().unwrap().queue.as_ref().unwrap();
    assert_eq!(
        queue.interrupted_request_id.as_deref(),
        Some(child_request_id)
    );
}

#[tokio::test]
async fn steer_subagent_interrupt_drains_automated_wakeups() {
    let (db, source) = setup_db("r4c-steer-drain").await;
    let hook = create_parent_hook(&db, "parent-drain", "session-drain").await;
    let child = spawn_background_child(db.node.as_ref(), &hook, "spawn-drain", "do work").await;
    let child_request_id = child["child_request_id"].as_str().unwrap();
    let child_session_id = child["child_session_id"].as_str().unwrap();
    drop(source);
    update_request_state(db.node.as_ref(), child_request_id, "claimed").await;
    let wake_request_id = "r4c-steer-drain-wake";
    create_child_session_queued_request(
        db.node.as_ref(),
        db.node_identity.did(),
        wake_request_id,
        child_session_id,
        "scheduled",
        &queue_metadata(
            "background_completion",
            "coalesce",
            Some(&format!("background_completion:{child_session_id}")),
            Some(child_request_id),
        ),
    )
    .await;

    let result = steer_subagent(
        &hook,
        "steer-drain",
        json!({
            "child_request_id": child_request_id,
            "message": "redirect",
            "interrupt": true
        }),
    )
    .await;

    let drained = result["drained_wake_up_request_ids"].as_array().unwrap();
    assert_eq!(drained, &vec![json!(wake_request_id)]);
    let wake = fetch_request(db.node.as_ref(), wake_request_id).await;
    assert_eq!(
        wake.lifecycle_state,
        Some(RequestLifecycleState::Interrupted)
    );
}

#[tokio::test]
async fn steer_subagent_interrupt_without_active_request_drains_visible_wakes_only() {
    let (db, source) = setup_db("r4c-steer-no-active-drain").await;
    let hook = create_parent_hook(&db, "parent-no-active", "session-no-active").await;
    let child = spawn_background_child(db.node.as_ref(), &hook, "spawn-no-active", "do work").await;
    let child_request_id = child["child_request_id"].as_str().unwrap();
    let child_session_id = child["child_session_id"].as_str().unwrap();
    drop(source);
    assert_eq!(
        fetch_request(db.node.as_ref(), child_request_id)
            .await
            .lifecycle_state,
        Some(RequestLifecycleState::Pending)
    );

    let wake_request_id = "r4c-steer-no-active-wake";
    let user_request_id = "r4c-steer-no-active-user";
    let key = format!("background_completion:{child_session_id}");
    create_child_session_queued_request(
        db.node.as_ref(),
        db.node_identity.did(),
        wake_request_id,
        child_session_id,
        "scheduled",
        &queue_metadata(
            "background_completion",
            "coalesce",
            Some(&key),
            Some(child_request_id),
        ),
    )
    .await;
    create_child_session_queued_request(
        db.node.as_ref(),
        db.node_identity.did(),
        user_request_id,
        child_session_id,
        "interactive",
        &queue_metadata("user", "append", None, None),
    )
    .await;

    let result = steer_subagent(
        &hook,
        "steer-no-active",
        json!({
            "child_request_id": child_request_id,
            "message": "redirect",
            "interrupt": true
        }),
    )
    .await;

    assert_eq!(result["child_request_id"], child_request_id);
    assert_eq!(result["child_session_id"], child_session_id);
    let queued_request_id = result["queued_request_id"]
        .as_str()
        .expect("steering returns its admitted request identity");
    assert_eq!(
        fetch_request(db.node.as_ref(), queued_request_id)
            .await
            .lifecycle_state,
        Some(RequestLifecycleState::Pending)
    );
    assert_eq!(result["interrupted_active_request_id"], Value::Null);
    assert_eq!(
        result["drained_wake_up_request_ids"],
        json!([wake_request_id])
    );
    assert_eq!(
        fetch_request(db.node.as_ref(), wake_request_id)
            .await
            .lifecycle_state,
        Some(RequestLifecycleState::Interrupted)
    );
    assert_eq!(
        fetch_request(db.node.as_ref(), user_request_id)
            .await
            .lifecycle_state,
        Some(RequestLifecycleState::Pending)
    );
    assert_eq!(
        fetch_request(db.node.as_ref(), child_request_id)
            .await
            .lifecycle_state,
        Some(RequestLifecycleState::Pending)
    );
    assert_eq!(
        fetch_interrupt_requested_at(db.node.as_ref(), child_request_id)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn steer_subagent_interrupt_replay_preserves_later_automated_wakeup() {
    let (db, source) = setup_db("r4c-steer-late-wake").await;
    let hook = create_parent_hook(&db, "parent-late-wake", "session-late-wake").await;
    let child = spawn_background_child(db.node.as_ref(), &hook, "spawn-late-wake", "do work").await;
    let child_request_id = child["child_request_id"].as_str().unwrap();
    let child_session_id = child["child_session_id"].as_str().unwrap();
    drop(source);
    update_request_state(db.node.as_ref(), child_request_id, "claimed").await;
    let child_doc_id =
        crate::support::exact_request_doc_id(db.node.as_ref(), child_request_id).await;
    gents::interrupt_request_by_doc_id(
        db.node.as_ref(),
        &child_doc_id,
        db.node_identity.did(),
        Some(db.node_identity.did()),
    )
    .await
    .unwrap();

    let wake_request_id = "r4c-steer-late-wake-request";
    create_child_session_queued_request(
        db.node.as_ref(),
        db.node_identity.did(),
        wake_request_id,
        child_session_id,
        "scheduled",
        &queue_metadata(
            "background_completion",
            "coalesce",
            Some(&format!("background_completion:{child_session_id}")),
            Some(child_request_id),
        ),
    )
    .await;

    let result = steer_subagent(
        &hook,
        "steer-late-wake",
        json!({
            "child_request_id": child_request_id,
            "message": "redirect",
            "interrupt": true
        }),
    )
    .await;

    assert_eq!(
        result["interrupted_active_request_id"].as_str(),
        Some(child_request_id)
    );
    assert!(result["drained_wake_up_request_ids"]
        .as_array()
        .unwrap()
        .is_empty());
    let wake = fetch_request(db.node.as_ref(), wake_request_id).await;
    assert_eq!(wake.lifecycle_state, Some(RequestLifecycleState::Pending));
}

#[tokio::test]
async fn steer_subagent_interrupt_leaves_grandchild_subagents_running() {
    let (db, source) = setup_db("r4c-steer-cascade").await;
    let hook = create_parent_hook(&db, "parent-cascade", "session-cascade").await;
    let parent_deadline = chrono::Utc::now() + chrono::Duration::minutes(5);
    let child = spawn_background_child(db.node.as_ref(), &hook, "spawn-cascade", "do work").await;
    let child_request_id = child["child_request_id"].as_str().unwrap().to_string();
    let child_session_id = child["child_session_id"].as_str().unwrap().to_string();
    drop(source);
    let grandchild_request_id = "r4c-steer-grandchild";
    let child_request_doc_id =
        crate::support::exact_request_doc_id(db.node.as_ref(), &child_request_id).await;
    let child_row =
        crate::support::load_request_row_by_logical_id(db.node.as_ref(), &child_request_id).await;
    assert_eq!(
        child_row.session_id.as_deref(),
        Some(child_session_id.as_str())
    );
    let mut child_owner = gents::lifecycle::RequestLifecycle::new_with_agent_did(
        db.node.clone(),
        CHILD_BEHAVIOR_ID,
        db.node_identity.did(),
        child_row.try_into().unwrap(),
        60,
    );
    assert_eq!(
        child_owner.claim().await.unwrap(),
        gents::lifecycle::ClaimOutcome::Claimed
    );
    let descendant_bridge = gents::tool_call_lifecycle::admission_fixture::publish_accepted_on_claimed_request(
        db.node.clone(),
        &mut child_owner,
        db.node_identity.did(),
        0,
        "spawn_subagent",
        "internal-steer-descendant",
        json!({"name": CHILD_BEHAVIOR_ID, "prompt": "grandchild prompt", "await_mode": "background"}),
        Some(gents::streaming::SpawnAdmissionPlan {
            tool_call_id: "internal-steer-descendant".into(),
            child_request_id: grandchild_request_id.into(),
            spawn_target_did: db.node_identity.did().into(),
            spawn_behavior_id: CHILD_BEHAVIOR_ID.into(),
            delegated_workspace: None,
            await_mode: AwaitMode::Background,
        }),
        AwaitMode::Background,
        CancelPolicy::Cascade,
        true,
    )
    .await
    .unwrap();
    let descendant_bridge_doc_id = descendant_bridge
        .doc_id()
        .expect("descendant bridge document id")
        .to_string();
    let _grandchild_session_id = create_subagent_request_with_request_id(
        db.node.as_ref(),
        grandchild_request_id.to_string(),
        child_request_id.clone(),
        child_request_doc_id,
        "internal-steer-descendant".to_string(),
        descendant_bridge_doc_id,
        1,
        db.node_identity.did().to_string(),
        CHILD_BEHAVIOR_ID.to_string(),
        "grandchild prompt".to_string(),
        Some(parent_deadline - chrono::Duration::minutes(1)),
    )
    .await
    .unwrap();

    let result = steer_subagent(
        &hook,
        "steer-cascade",
        json!({
            "child_request_id": child_request_id,
            "message": "redirect",
            "interrupt": true
        }),
    )
    .await;
    assert_eq!(
        result["interrupted_active_request_id"].as_str(),
        Some(child_request_id.as_str()),
        "{result}"
    );

    assert!(
        fetch_interrupt_requested_at(db.node.as_ref(), grandchild_request_id)
            .await
            .unwrap()
            .is_none(),
        "interrupting the child thread must not reach its own subagents"
    );
}
