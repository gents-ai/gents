use std::sync::Arc;

use gents::{
    fetch_interrupt_requested_at,
    graphql::escape_graphql_string,
    lifecycle::{ClaimOutcome, ExecutionOrigin},
    tool_call_lifecycle::{AwaitMode, CancelPolicy, ToolCallLifecycle},
    AgentIdentity, DefraWatcher, RequestExecutionProvenance, RequestLifecycle,
};
use serde::Deserialize;

use crate::support::snapshots::{
    fetch_message_snapshots_for_session, fetch_tool_call_snapshots_for_session,
};
use crate::support::{
    build_request, conversation_status_by_doc_id, create_conversation_row,
    create_conversation_row_for_agent, create_request, create_request_for_agent, first_row,
    test_db, test_db_with_duplicate_tolerant_conversations,
    test_db_with_duplicate_tolerant_conversations_and_identity, test_db_with_identity,
    upsert_conversation_for_agent, TestDb, AGENT_DID, AGENT_NAME, BACKEND_ID, DEADLINE_SECS,
};

async fn signed_recovery_db(name: &str) -> (TestDb, String) {
    let identity: Arc<dyn AgentIdentity> = Arc::new(crate::support::fixtures::test_identity(name));
    let agent_did = identity.did().to_string();
    (test_db_with_identity(name, identity).await, agent_did)
}

async fn signed_duplicate_recovery_db(name: &str) -> (TestDb, String) {
    let identity: Arc<dyn AgentIdentity> = Arc::new(crate::support::fixtures::test_identity(name));
    let agent_did = identity.did().to_string();
    (
        test_db_with_duplicate_tolerant_conversations_and_identity(name, identity).await,
        agent_did,
    )
}

async fn create_signed_active_request(
    db: &TestDb,
    agent_did: &str,
    request_id: &str,
    session_id: &str,
) -> (String, RequestExecutionProvenance) {
    let doc_id = crate::support::interrupt::create_runtime_request(
        db.node.as_ref(),
        agent_did,
        AGENT_NAME,
        request_id,
        session_id,
        "recovery fixture",
    )
    .await;
    let request = DefraWatcher::new(db.node.clone(), agent_did)
        .try_fetch_request(&doc_id)
        .await
        .expect("load signed recovery request")
        .expect("signed recovery request should be visible");
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        agent_did,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    let provenance = lifecycle
        .execution_provenance()
        .expect("signed claim must expose exact provenance")
        .clone();
    lifecycle.begin_execution().await.unwrap();
    (doc_id, provenance)
}

async fn create_signed_assistant_message(
    node: &gents::defra_node::EmbeddedNode,
    agent_did: &str,
    request_id: &str,
    session_id: &str,
    request_doc_id: &str,
) -> (String, String, String) {
    let message_key = format!("{session_id}:assistant:1");
    let now = chrono::Utc::now().to_rfc3339();
    let response = node
        .execute(&format!(
            r#"mutation {{
                create_AgentMessage(input: {{
                    message_key: "{}"
                    session_id: "{}"
                    agent_did: "{}"
                    request_id: "{}"
                    request_doc_id: "{}"
                    sequence: 1
                    role: "assistant"
                    content: "recovered completion"
                    timestamp: "{}"
                }}) {{ _docID }}
            }}"#,
            escape_graphql_string(&message_key),
            escape_graphql_string(session_id),
            escape_graphql_string(agent_did),
            escape_graphql_string(request_id),
            escape_graphql_string(request_doc_id),
            escape_graphql_string(&now),
        ))
        .await;
    assert!(
        !response.has_errors(),
        "create signed assistant message failed: {:?}",
        response.errors
    );
    let message_query = node
        .execute(&format!(
            r#"query {{
                AgentMessage(filter: {{ message_key: {{ _eq: "{}" }} }}) {{ _docID }}
            }}"#,
            escape_graphql_string(&message_key),
        ))
        .await;
    let doc_id = first_row::<crate::support::DocIdRow>(&message_query, "AgentMessage").doc_id;
    let commits = node
        .execute(&format!(
            r#"query {{
                _commits(
                    docID: ["{}"]
                    filter: {{ fieldName: {{ _eq: "_C" }} }}
                ) {{ cid }}
            }}"#,
            escape_graphql_string(&doc_id),
        ))
        .await;
    assert!(
        !commits.has_errors(),
        "query message commit failed: {:?}",
        commits.errors
    );
    let rows = commits.data.as_ref().unwrap()["_commits"]
        .as_array()
        .expect("_commits array");
    assert_eq!(
        rows.len(),
        1,
        "immutable message must have one composite commit"
    );
    let cid = rows[0]["cid"].as_str().unwrap().to_string();
    let signer = node
        .verified_block_signer_did(&cid)
        .await
        .expect("verify assistant message signer");
    (doc_id, cid, signer)
}

#[allow(clippy::too_many_arguments)]
async fn create_exact_response(
    node: &gents::defra_node::EmbeddedNode,
    agent_did: &str,
    request_id: &str,
    session_id: &str,
    provenance: &RequestExecutionProvenance,
    content: &str,
    status: &str,
    final_message: Option<&(String, String, String)>,
) {
    let final_message_fields = final_message
        .map(|(doc_id, cid, signer)| {
            format!(
                r#"final_message_doc_id: "{}"
                   final_message_composite_commit_cid: "{}"
                   final_message_signer_did: "{}"
                   final_message_sequence: 1"#,
                escape_graphql_string(doc_id),
                escape_graphql_string(cid),
                escape_graphql_string(signer),
            )
        })
        .unwrap_or_default();
    let now = chrono::Utc::now().to_rfc3339();
    let completed_at = if status == "complete" {
        now.as_str()
    } else {
        ""
    };
    let response = node
        .execute(&format!(
            r#"mutation {{
                create_AgentResponse(input: {{
                    response_key: "{}"
                    request_id: "{}"
                    request_doc_id: "{}"
                    request_source_composite_commit_cid: "{}"
                    request_source_signer_did: "{}"
                    request_claim_composite_commit_cid: "{}"
                    request_claim_signer_did: "{}"
                    agent_did: "{}"
                    behavior_id: "{}"
                    session_id: "{}"
                    content: "{}"
                    status: "{}"
                    token_count: 0
                    progress_seq: 0
                    created_at: "{}"
                    completed_at: "{}"
                    {}
                }}) {{ _docID }}
            }}"#,
            escape_graphql_string(request_id),
            escape_graphql_string(request_id),
            escape_graphql_string(&provenance.source.version.doc_id),
            escape_graphql_string(&provenance.source.version.composite_commit_cid),
            escape_graphql_string(&provenance.source.signer_did),
            escape_graphql_string(&provenance.claim.version.composite_commit_cid),
            escape_graphql_string(&provenance.claim.signer_did),
            escape_graphql_string(agent_did),
            AGENT_NAME,
            escape_graphql_string(session_id),
            escape_graphql_string(content),
            status,
            escape_graphql_string(&now),
            escape_graphql_string(completed_at),
            final_message_fields,
        ))
        .await;
    assert!(
        !response.has_errors(),
        "create exact response failed: {:?}",
        response.errors
    );
}

#[derive(Debug, Clone, Deserialize)]
struct StatusRow {
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponseStatusRow {
    status: String,
    content: String,
    #[serde(default)]
    reasoning: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponseOutcomeRow {
    outcome_kind: String,
    reason_code: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConversationRow {
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct NotificationDeliveryRow {
    completion_notification_delivered_at: Option<String>,
}

async fn mark_request_interrupted(node: &gents::defra_node::EmbeddedNode, doc_id: &str) {
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                input: {{ status: "interrupted", lifecycle_state: "interrupted" }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "mark request interrupted failed: {:?}",
        resp.errors
    );
}

#[tokio::test]
async fn recover_all_marks_requests_as_error() {
    let (db, agent_did) = signed_recovery_db("lifecycle-recover-error").await;
    create_signed_active_request(&db, &agent_did, "stuck-1", "session-1").await;

    let report = RequestLifecycle::recover_all(&db.node, &agent_did)
        .await
        .unwrap();
    assert_eq!(report.responses_recovered, 1);
    assert_eq!(report.requests_recovered, 0);

    let resp = db
        .node
        .execute(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "stuck-1" } },
                    limit: 1
                ) { status }
            }"#,
        )
        .await;
    assert_eq!(
        first_row::<StatusRow>(&resp, "AgentRequest").status,
        "error"
    );
}

#[tokio::test]
async fn recover_all_preserves_completed_response() {
    let (db, agent_did) = signed_recovery_db("lifecycle-recover-complete").await;
    let (request_doc_id, provenance) =
        create_signed_active_request(&db, &agent_did, "stuck-complete", "session-complete").await;
    let final_message = create_signed_assistant_message(
        &db.node,
        &agent_did,
        "stuck-complete",
        "session-complete",
        &request_doc_id,
    )
    .await;
    gents::publish_completed_response_outcome_for_test(&db.node, &request_doc_id, &agent_did, 1)
        .await
        .expect("publish signed completed outcome");
    create_exact_response(
        &db.node,
        &agent_did,
        "stuck-complete",
        "session-complete",
        &provenance,
        "",
        "complete",
        Some(&final_message),
    )
    .await;
    upsert_conversation_for_agent(
        &db.node,
        "session-complete",
        "stuck-complete",
        &agent_did,
        "hello",
        "processing",
    )
    .await;

    let report = RequestLifecycle::recover_all(&db.node, &agent_did)
        .await
        .unwrap();
    assert_eq!(report.requests_recovered, 1);
    assert_eq!(report.conversations_recovered, 1);

    let request_resp = db
        .node
        .execute(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "stuck-complete" } },
                    limit: 1
                ) { status }
            }"#,
        )
        .await;
    assert_eq!(
        first_row::<StatusRow>(&request_resp, "AgentRequest").status,
        "completed"
    );

    let conversation_resp = db
        .node
        .execute(
            r#"{
                AgentConversation(
                    filter: { session_id: { _eq: "session-complete" } },
                    limit: 1
                ) { status latest_request_id }
            }"#,
        )
        .await;
    assert_eq!(
        first_row::<ConversationRow>(&conversation_resp, "AgentConversation").status,
        "completed"
    );
}

#[tokio::test]
async fn recover_all_marks_partial_streams_error_and_reactivates_conversation() {
    let (db, agent_did) = signed_recovery_db("lifecycle-recover-partial").await;
    let (_, provenance) =
        create_signed_active_request(&db, &agent_did, "stuck-partial", "session-partial").await;
    create_exact_response(
        &db.node,
        &agent_did,
        "stuck-partial",
        "session-partial",
        &provenance,
        "partial reply",
        "streaming",
        None,
    )
    .await;
    let reasoning_update = db
        .node
        .execute(
            r#"mutation {
                update_AgentResponse(
                    filter: { response_key: { _eq: "stuck-partial" } },
                    input: { reasoning: "partial thought" }
                ) { _docID }
            }"#,
        )
        .await;
    assert!(!reasoning_update.has_errors());
    upsert_conversation_for_agent(
        &db.node,
        "session-partial",
        "stuck-partial",
        &agent_did,
        "hello",
        "processing",
    )
    .await;

    let report = RequestLifecycle::recover_all(&db.node, &agent_did)
        .await
        .unwrap();
    assert_eq!(report.responses_recovered, 1);
    assert_eq!(report.requests_recovered, 1);
    assert_eq!(report.conversations_recovered, 1);

    let response_resp = db
        .node
        .execute(
            r#"{
                AgentResponse(
                    filter: { response_key: { _eq: "stuck-partial" } },
                    limit: 1
                ) { status content reasoning }
            }"#,
        )
        .await;
    let response = first_row::<ResponseStatusRow>(&response_resp, "AgentResponse");
    assert_eq!(response.status, "error");
    assert!(response.content.contains("[Response interrupted"));
    assert!(response.reasoning.contains("[Reasoning interrupted"));

    let conversation_resp = db
        .node
        .execute(
            r#"{
                AgentConversation(
                    filter: { session_id: { _eq: "session-partial" } },
                    limit: 1
                ) { status latest_request_id }
            }"#,
        )
        .await;
    assert_eq!(
        first_row::<ConversationRow>(&conversation_resp, "AgentConversation").status,
        "active"
    );
}

#[tokio::test]
async fn recover_all_publishes_error_outcome_when_response_doc_is_missing() {
    let (db, agent_did) = signed_recovery_db("lifecycle-recover-missing").await;
    create_signed_active_request(&db, &agent_did, "stuck-missing", "session-missing").await;
    upsert_conversation_for_agent(
        &db.node,
        "session-missing",
        "stuck-missing",
        &agent_did,
        "hello",
        "processing",
    )
    .await;

    let report = RequestLifecycle::recover_all(&db.node, &agent_did)
        .await
        .unwrap();
    assert_eq!(report.responses_recovered, 1);
    assert_eq!(report.requests_recovered, 0);
    assert_eq!(report.conversations_recovered, 1);

    let outcome_resp = db
        .node
        .execute(
            r#"{
                AgentResponseOutcome(
                    filter: { request_id: { _eq: "stuck-missing" } },
                    limit: 1
                ) { outcome_kind reason_code }
            }"#,
        )
        .await;
    let outcome = first_row::<ResponseOutcomeRow>(&outcome_resp, "AgentResponseOutcome");
    assert_eq!(outcome.outcome_kind, "error");
    assert_eq!(
        outcome.reason_code.as_deref(),
        Some("daemon_restart_missing_response")
    );
}

#[tokio::test]
async fn recover_all_times_out_expired_running_tool_calls() {
    let db = test_db("tool-call-recover-timeout").await;
    create_request(
        &db.node,
        "tool-timeout-req",
        "tool-timeout-session",
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;

    let mut lifecycle = ToolCallLifecycle::new(
        db.node.clone(),
        "tool-timeout-req".to_string(),
        "tool-timeout-session".to_string(),
        "did:test:test".to_string(),
        "tool-timeout-call".to_string(),
        1,
        "never".to_string(),
        "{}".to_string(),
        chrono::Utc::now() - chrono::Duration::seconds(1),
    );
    lifecycle.start_running().await.unwrap();

    let report = ToolCallLifecycle::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(report.tool_calls_recovered, 1);

    let snapshots = fetch_tool_call_snapshots_for_session(&db.node, "tool-timeout-session").await;
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].lifecycle_state.as_deref(), Some("timedOut"));
    assert_eq!(snapshots[0].cancel_cause.as_deref(), Some("deadline"));
    assert_eq!(snapshots[0].status, "completed");
    assert!(snapshots[0].result.contains("deadline exceeded"));
}

#[tokio::test]
async fn recover_all_repairs_terminal_background_tool_notification_once() {
    let (db, agent_did) = signed_recovery_db("tool-call-repair-notification").await;
    create_request_for_agent(
        &db.node,
        "tool-notification-req",
        "tool-notification-session",
        &agent_did,
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;

    let mut lifecycle = ToolCallLifecycle::new_background_tool(
        db.node.clone(),
        "tool-notification-req".to_string(),
        "tool-notification-session".to_string(),
        agent_did.clone(),
        "tool-notification-call".to_string(),
        1,
        "lookup".to_string(),
        "{}".to_string(),
        chrono::Utc::now() + chrono::Duration::minutes(5),
    );
    lifecycle.start_running().await.unwrap();
    assert!(lifecycle
        .bridge_complete("durable result".to_string())
        .await
        .unwrap());

    assert!(
        fetch_message_snapshots_for_session(&db.node, "tool-notification-session")
            .await
            .is_empty(),
        "the test precondition is a terminal tool with a missing notification"
    );

    let first = ToolCallLifecycle::recover_all(&db.node, &agent_did)
        .await
        .unwrap();
    assert_eq!(first.notifications_repaired, 1);
    let second = ToolCallLifecycle::recover_all(&db.node, &agent_did)
        .await
        .unwrap();
    assert_eq!(second.notifications_repaired, 0);

    let messages = fetch_message_snapshots_for_session(&db.node, "tool-notification-session").await;
    assert_eq!(messages.len(), 1, "repair must be durably idempotent");
    assert!(messages[0].content.contains("durable result"));

    let response = db
        .node
        .execute(
            r#"{
                AgentToolCall(
                    filter: { tool_call_id: { _eq: "tool-notification-call" } },
                    limit: 1
                ) { completion_notification_delivered_at }
            }"#,
        )
        .await;
    let row = first_row::<NotificationDeliveryRow>(&response, "AgentToolCall");
    assert!(
        row.completion_notification_delivered_at.is_some(),
        "successful notification append must advance the delivery marker"
    );
}

#[tokio::test]
async fn recover_all_cancels_running_tool_call_for_interrupted_parent_only() {
    let db = test_db("tool-call-recover-cancel").await;
    let interrupted_doc = create_request(
        &db.node,
        "tool-cancel-req",
        "tool-cancel-session",
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;
    create_request(
        &db.node,
        "tool-other-req",
        "tool-other-session",
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;
    mark_request_interrupted(&db.node, &interrupted_doc).await;

    let future_deadline = chrono::Utc::now() + chrono::Duration::minutes(5);
    let mut cancelled = ToolCallLifecycle::new(
        db.node.clone(),
        "tool-cancel-req".to_string(),
        "tool-cancel-session".to_string(),
        "did:test:test".to_string(),
        "tool-cancel-call".to_string(),
        1,
        "slow".to_string(),
        "{}".to_string(),
        future_deadline,
    );
    cancelled.start_running().await.unwrap();

    let mut unrelated = ToolCallLifecycle::new(
        db.node.clone(),
        "tool-other-req".to_string(),
        "tool-other-session".to_string(),
        "did:test:test".to_string(),
        "tool-other-call".to_string(),
        1,
        "slow".to_string(),
        "{}".to_string(),
        future_deadline,
    );
    unrelated.start_running().await.unwrap();

    let report = ToolCallLifecycle::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(report.tool_calls_recovered, 1);

    let cancelled_snapshots =
        fetch_tool_call_snapshots_for_session(&db.node, "tool-cancel-session").await;
    assert_eq!(
        cancelled_snapshots[0].lifecycle_state.as_deref(),
        Some("cancelled")
    );
    assert_eq!(
        cancelled_snapshots[0].cancel_cause.as_deref(),
        Some("interrupted")
    );

    let unrelated_snapshots =
        fetch_tool_call_snapshots_for_session(&db.node, "tool-other-session").await;
    assert_eq!(
        unrelated_snapshots[0].lifecycle_state.as_deref(),
        Some("running"),
        "unrelated running tool call should not be swept"
    );
}

#[tokio::test]
async fn recover_all_cascades_interrupted_parent_to_subagent_child() {
    let db = test_db("tool-call-recover-cascade").await;
    let interrupted_doc = create_request(
        &db.node,
        "tool-cascade-parent",
        "tool-cascade-parent-session",
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;
    create_request(
        &db.node,
        "tool-cascade-child",
        "tool-cascade-child-session",
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;
    mark_request_interrupted(&db.node, &interrupted_doc).await;

    let mut lifecycle = ToolCallLifecycle::new_subagent(
        db.node.clone(),
        "tool-cascade-parent".to_string(),
        "tool-cascade-parent-session".to_string(),
        "did:test:test".to_string(),
        "tool-cascade-call".to_string(),
        1,
        "spawn_subagent".to_string(),
        "{}".to_string(),
        chrono::Utc::now() + chrono::Duration::minutes(5),
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
        "tool-cascade-child".to_string(),
        "did:test:target".to_string(),
    );
    lifecycle.start_running().await.unwrap();

    let report = ToolCallLifecycle::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(report.tool_calls_recovered, 1);

    let snapshots =
        fetch_tool_call_snapshots_for_session(&db.node, "tool-cascade-parent-session").await;
    assert_eq!(snapshots[0].lifecycle_state.as_deref(), Some("cancelled"));

    let child_interrupt = fetch_interrupt_requested_at(&db.node, "tool-cascade-child")
        .await
        .unwrap();
    assert!(
        child_interrupt.is_some(),
        "cascade recovery should latch child interrupt_requested_at"
    );
}

#[tokio::test]
async fn recover_all_leaves_detached_subagent_tool_running() {
    let db = test_db("tool-call-recover-detach").await;
    let interrupted_doc = create_request(
        &db.node,
        "tool-detach-parent",
        "tool-detach-parent-session",
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;
    create_request(
        &db.node,
        "tool-detach-child",
        "tool-detach-child-session",
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;
    mark_request_interrupted(&db.node, &interrupted_doc).await;

    let mut lifecycle = ToolCallLifecycle::new_subagent(
        db.node.clone(),
        "tool-detach-parent".to_string(),
        "tool-detach-parent-session".to_string(),
        "did:test:test".to_string(),
        "tool-detach-call".to_string(),
        1,
        "spawn_subagent".to_string(),
        "{}".to_string(),
        chrono::Utc::now() + chrono::Duration::minutes(5),
        AwaitMode::Background,
        CancelPolicy::Detach,
        "tool-detach-child".to_string(),
        "did:test:target".to_string(),
    );
    lifecycle.start_running().await.unwrap();

    let report = ToolCallLifecycle::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(report.tool_calls_recovered, 0);

    let snapshots =
        fetch_tool_call_snapshots_for_session(&db.node, "tool-detach-parent-session").await;
    assert_eq!(
        snapshots[0].lifecycle_state.as_deref(),
        Some("running"),
        "detached bridge tool should remain running for the subagent runtime to reconcile"
    );

    let child_interrupt = fetch_interrupt_requested_at(&db.node, "tool-detach-child")
        .await
        .unwrap();
    assert!(
        child_interrupt.is_none(),
        "detached recovery should not interrupt the child request"
    );
}

/// #693: a store carrying two `AgentConversation` docs for one `session_id`.
///
/// Before the fix this failed twice over: the `session_id`-filtered upsert was
/// refused by DefraDB (`cannot upsert multiple matching documents`), so *both*
/// docs stayed `processing` — and the sweep still reported
/// `conversations_recovered == 2`, because it counted the rows it attempted
/// rather than the writes that landed. A fully failed pass logged as healthy.
///
/// The duplicate condition is real: `session_id` is unique-indexed in the
/// shipped schema, but DefraDB cannot add an index to an existing collection,
/// so hosts whose collection predates the index carry duplicates permanently
/// (replication can mint them too). Four production stores were held back on old
/// releases by this.
#[tokio::test]
async fn recover_all_recovers_canonical_conversation_of_a_duplicated_session() {
    let (db, agent_did) = signed_duplicate_recovery_db("lifecycle-recovery-duplicate").await;

    let (request_doc_id, provenance) =
        create_signed_active_request(&db, &agent_did, "dup-req", "session-dup").await;
    let final_message = create_signed_assistant_message(
        &db.node,
        &agent_did,
        "dup-req",
        "session-dup",
        &request_doc_id,
    )
    .await;
    gents::publish_completed_response_outcome_for_test(&db.node, &request_doc_id, &agent_did, 1)
        .await
        .expect("publish signed completed outcome");
    create_exact_response(
        &db.node,
        &agent_did,
        "dup-req",
        "session-dup",
        &provenance,
        "",
        "complete",
        Some(&final_message),
    )
    .await;

    let canonical = create_conversation_row_for_agent(
        &db.node,
        "session-dup",
        &agent_did,
        "Real conversation",
        "hello",
        "processing",
        "2026-03-23T00:00:00Z",
        "2026-03-23T00:05:00Z",
        "dup-req",
    )
    .await;
    let duplicate = create_conversation_row_for_agent(
        &db.node,
        "session-dup",
        &agent_did,
        "",
        "",
        "processing",
        "2026-03-22T00:00:00Z",
        "2026-03-22T00:00:00Z",
        "",
    )
    .await;
    assert_ne!(canonical, duplicate, "the seed must produce two documents");

    let report = RequestLifecycle::recover_all(&db.node, &agent_did)
        .await
        .expect("recovery must not fail on a duplicate store");

    assert_eq!(report.conversations_recovered, 1);
    assert_eq!(report.conversations_failed, 0);
    assert_eq!(report.duplicate_conversation_sessions, 1);

    assert_eq!(
        conversation_status_by_doc_id(&db.node, &canonical).await,
        "completed",
    );
    assert_eq!(
        conversation_status_by_doc_id(&db.node, &duplicate).await,
        "completed",
    );

    let second = RequestLifecycle::recover_all(&db.node, &agent_did)
        .await
        .expect("second pass");
    assert_eq!(second.conversations_recovered, 0);
    assert_eq!(second.conversations_failed, 0);
}

#[tokio::test]
async fn live_request_path_survives_a_duplicated_session() {
    let db = test_db_with_duplicate_tolerant_conversations("lifecycle-duplicate-live").await;

    let canonical = create_conversation_row(
        &db.node,
        "session-live",
        "Real conversation",
        "hello",
        "active",
        "2026-03-23T00:00:00Z",
        "2026-03-23T00:05:00Z",
        "req-old",
    )
    .await;
    let duplicate = create_conversation_row(
        &db.node,
        "session-live",
        "",
        "",
        "active",
        "2026-03-22T00:00:00Z",
        "2026-03-22T00:00:00Z",
        "",
    )
    .await;

    let doc_id = create_request(
        &db.node,
        "req-new",
        "session-live",
        "pending",
        "2026-03-24T00:00:00Z",
    )
    .await;
    let request = build_request(
        doc_id,
        "req-new".to_string(),
        "session-live".to_string(),
        "2026-03-24T00:00:00Z".to_string(),
    );
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    assert_eq!(
        lifecycle.claim_without_identity_for_test().await.unwrap(),
        ClaimOutcome::Claimed
    );

    lifecycle
        .prepare_session_with_identity()
        .await
        .expect("live conversation write must survive a duplicate store");

    assert_eq!(
        conversation_status_by_doc_id(&db.node, &canonical).await,
        "processing",
    );
    assert_eq!(
        conversation_status_by_doc_id(&db.node, &duplicate).await,
        "active",
    );
}
