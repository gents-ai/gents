use gents::{
    fetch_interrupt_requested_at,
    tool_call_lifecycle::{AwaitMode, CancelPolicy, ToolCallLifecycle},
    RequestLifecycle,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;

use crate::support::snapshots::{
    fetch_message_snapshots_for_session, fetch_tool_call_snapshots_for_session,
};
use crate::support::{
    create_agent_session, create_request, create_request_for_agent_with_signed_fields, first_row,
    test_db, upsert_conversation, upsert_conversation_for_agent, AGENT_DID, AGENT_NAME, BACKEND_ID,
};

type StatusRow = AgentRequestRow;

#[derive(Debug, Clone, Deserialize)]
struct ResponseStatusRow {
    status: String,
    content: String,
}

#[derive(Debug, Clone, Deserialize)]
struct NotificationDeliveryRow {
    completion_notification_delivered_at: Option<String>,
}

#[tokio::test]
async fn failed_background_wake_redrive_is_bounded_and_idempotent() {
    let db = test_db("lifecycle-background-wake-redrive").await;
    let agent_did = db.node_identity.did().to_string();
    let metadata = serde_json::json!({
        "queue": {
            "source": "background_completion",
            "policy": "coalesce",
            "key": "background_completion:wake-redrive-session",
            "queued_after_request_id": "foreground-parent"
        },
        "background_completion_wake_version": 1
    })
    .to_string();
    let escaped_metadata = gents::graphql::escape_graphql_string(&metadata);
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "failed-wake",
                agent_did: "{agent_did}",
                behavior_id: "{AGENT_NAME}",
                session_id: "wake-redrive-session",
                retry_parent_request: "",
                retry_root_request: "failed-wake",
                superseded_by_request: "",
                content: "continue after background completion",
                metadata: "{escaped_metadata}",
                lifecycle_state: "failed",
                backend_id: "{BACKEND_ID}",
                execution_origin: "scheduled",
                failure_reason: "backend admission failed",
                terminalized_at: "2026-08-12T00:00:00Z",
                terminal_redrive_attempts: 0,
                created_at: "2026-08-12T00:00:00Z",
                deadline: "2026-08-12T00:00:01Z",
                retry_count: 1,
                max_retries: 3,
                valid_until: "2026-08-12T00:00:01Z",
                subagent_depth: 0
            }}) {{ _docID }}
        }}"#
    );
    let response = db.node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create failed wake: {:?}",
        response.errors
    );
    upsert_conversation_for_agent(
        &db.node,
        &agent_did,
        "wake-redrive-session",
        "failed-wake",
        "continue after background completion",
        "active",
    )
    .await;

    let (first, concurrent) = tokio::join!(
        RequestLifecycle::redrive_failed_background_wakeups(&db.node, &agent_did),
        RequestLifecycle::redrive_failed_background_wakeups(&db.node, &agent_did),
    );
    let first = first.expect("first concurrent redrive");
    let concurrent = concurrent.expect("second concurrent redrive");
    assert_eq!(first.scanned, 1);
    assert_eq!(concurrent.scanned, 1);
    assert_eq!(first.redriven + concurrent.redriven, 1);
    assert_eq!(first.already_redriven + concurrent.already_redriven, 1);
    assert_eq!(first.failed + concurrent.failed, 0);

    let rows = background_wake_retry_rows(&db.node, "wake-redrive-session").await;
    assert_eq!(rows.len(), 2);
    let successor = rows
        .iter()
        .find(|row| row.request_id != "failed-wake")
        .expect("retry successor");
    assert_eq!(
        successor.lifecycle_state,
        Some(RequestLifecycleState::Pending)
    );
    assert_eq!(successor.execution_origin.as_deref(), Some("scheduled"));
    assert_eq!(
        successor.retry_parent_request.as_deref(),
        Some("failed-wake")
    );
    assert_eq!(successor.retry_root_request.as_deref(), Some("failed-wake"));
    assert_eq!(
        successor.content.as_deref(),
        Some(gents::background_completion::BACKGROUND_COMPLETION_WAKE_PROMPT)
    );
    assert_eq!(successor.retry_count, Some(2));
    assert_eq!(successor.max_retries, Some(3));
    assert_eq!(successor.metadata.as_deref(), Some(metadata.as_str()));
    assert_eq!(successor.deadline, None);
    assert_eq!(successor.valid_until, None);

    let second = RequestLifecycle::redrive_failed_background_wakeups(&db.node, &agent_did)
        .await
        .expect("repeat redrive");
    assert_eq!(second.redriven, 0);
    assert_eq!(second.already_redriven, 1);
    assert_eq!(
        background_wake_retry_rows(&db.node, "wake-redrive-session")
            .await
            .len(),
        2
    );
}

#[tokio::test]
async fn failed_background_wake_waits_for_persisted_backoff() {
    let db = test_db("lifecycle-background-wake-backoff").await;
    let session_id = "wake-backoff-session";
    let metadata = serde_json::json!({
        "queue": {
            "source": "background_completion",
            "policy": "coalesce",
            "key": format!("background_completion:{session_id}"),
            "queued_after_request_id": "foreground-parent"
        },
        "background_completion_wake_version": 1
    })
    .to_string();
    let escaped_metadata = gents::graphql::escape_graphql_string(&metadata);
    let terminalized_at = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "failed-wake-backoff", agent_did: "{AGENT_DID}",
                behavior_id: "{AGENT_NAME}", session_id: "{session_id}",
                retry_parent_request: "", retry_root_request: "failed-wake-backoff",
                superseded_by_request: "", content: "continue", metadata: "{escaped_metadata}",
                lifecycle_state: "failed", backend_id: "{BACKEND_ID}",
                execution_origin: "scheduled", failure_reason: "provider failed",
                terminalized_at: "{terminalized_at}", terminal_redrive_attempts: 0,
                created_at: "{terminalized_at}", retry_count: 1, max_retries: 3,
                subagent_depth: 0
            }}) {{ _docID }}
        }}"#
    );
    let response = db.node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create failed wake: {:?}",
        response.errors
    );
    upsert_conversation(
        &db.node,
        session_id,
        "failed-wake-backoff",
        "continue",
        "active",
    )
    .await;
    let message = format!(
        r#"mutation {{
            create_AgentMessage(input: {{
                message_key: "background-completion-notification:child-backoff:subagent",
                session_id: "{session_id}", agent_did: "{AGENT_DID}",
                request_id: "failed-wake-backoff", sequence: 1, role: "user",
                content: "child finished", timestamp: "{terminalized_at}"
            }}) {{ _docID }}
        }}"#
    );
    let response = db.node.execute(&message).await;
    assert!(
        !response.has_errors(),
        "create completion notification: {:?}",
        response.errors
    );

    let report = RequestLifecycle::redrive_failed_background_wakeups(&db.node, AGENT_DID)
        .await
        .expect("deferred redrive sweep");
    assert_eq!(report.scanned, 1);
    assert_eq!(report.deferred, 1);
    assert_eq!(report.redriven, 0);
    assert_eq!(
        background_wake_retry_rows(&db.node, session_id).await.len(),
        1
    );
    let diagnostics = gents::load_background_completion_diagnostics(
        &gents::config_client::ConfigAccess::Local(db.node.clone()),
        AGENT_DID,
    )
    .await
    .expect("load persisted completion diagnostics");
    assert_eq!(diagnostics.pending_notifications, 1);
    assert_eq!(diagnostics.stranded_notifications, 0);
    assert_eq!(diagnostics.epochs.len(), 1);
    assert_eq!(diagnostics.epochs[0].state, "retry_backoff");
    assert_eq!(diagnostics.epochs[0].attempt_count, 2);
    assert!(diagnostics.epochs[0].next_retry_at.is_some());

    upsert_conversation(
        &db.node,
        session_id,
        "later-interactive-request",
        "new user turn",
        "active",
    )
    .await;
    let displaced = gents::load_background_completion_diagnostics(
        &gents::config_client::ConfigAccess::Local(db.node.clone()),
        AGENT_DID,
    )
    .await
    .expect("load displaced completion diagnostics");
    assert_eq!(displaced.pending_notifications, 1);
    assert_eq!(displaced.stranded_notifications, 1);
    assert_eq!(displaced.epochs[0].state, "retry_ineligible_not_latest");
    assert_eq!(displaced.epochs[0].next_retry_at, None);
}

async fn background_wake_retry_rows(
    node: &gents::defra_node::EmbeddedNode,
    session_id: &str,
) -> Vec<AgentRequestRow> {
    let session_id = gents::graphql::escape_graphql_string(session_id);
    let response = node
        .execute(&format!(
            r#"{{
                AgentRequest(
                    filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                    order: {{ created_at: ASC }}
                ) {{
                    request_id content lifecycle_state execution_origin
                    retry_parent_request retry_root_request retry_count max_retries
                    metadata deadline valid_until
                }}
            }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "fetch background wake retries: {:?}",
        response.errors
    );
    serde_json::from_value(response.data.expect("wake retry data")["AgentRequest"].clone())
        .expect("decode wake retry rows")
}

async fn mark_request_interrupted(node: &gents::defra_node::EmbeddedNode, doc_id: &str) {
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                input: {{ lifecycle_state: "interrupted" }}
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

async fn seed_accepted_request_projection(
    node: &gents::defra_node::EmbeddedNode,
    session_id: &str,
    request_id: &str,
) {
    create_agent_session(node, session_id, AGENT_NAME, "2026-03-23T00:00:00Z").await;
    upsert_conversation(node, session_id, request_id, "stuck request", "active").await;
}

async fn set_execution_lease(
    node: &gents::defra_node::EmbeddedNode,
    request_doc_id: &str,
    generation: &str,
    expires_at: chrono::DateTime<chrono::Utc>,
    progress_seq: u64,
) {
    let request_doc_id = gents::graphql::escape_graphql_string(request_doc_id);
    let generation = gents::graphql::escape_graphql_string(generation);
    let expires_at = gents::graphql::escape_graphql_string(&expires_at.to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{request_doc_id}" }} }},
                input: {{
                    execution_generation: "{generation}",
                    execution_lease_expires_at: "{expires_at}",
                    execution_progress_seq: {progress_seq}
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "set execution lease failed: {:?}",
        response.errors
    );
}

async fn create_response_for_request(
    node: &gents::defra_node::EmbeddedNode,
    response_key: &str,
    request_id: &str,
    request_doc_id: &str,
    session_id: &str,
    content: &str,
    status: &str,
) {
    let response_key = gents::graphql::escape_graphql_string(response_key);
    let request_id = gents::graphql::escape_graphql_string(request_id);
    let request_doc_id = gents::graphql::escape_graphql_string(request_doc_id);
    let session_id = gents::graphql::escape_graphql_string(session_id);
    let content = gents::graphql::escape_graphql_string(content);
    let escaped_agent_did = gents::graphql::escape_graphql_string(AGENT_DID);
    let escaped_agent_name = gents::graphql::escape_graphql_string(AGENT_NAME);
    let completed_at = if matches!(status, "complete" | "error") {
        "2026-03-23T00:01:00Z"
    } else {
        ""
    };
    let status = gents::graphql::escape_graphql_string(status);
    let mutation = format!(
        r#"mutation {{
            create_AgentResponse(input: {{
                response_key: "{response_key}",
                request_id: "{request_id}",
                request_doc_id: "{request_doc_id}",
                agent_did: "{escaped_agent_did}",
                behavior_id: "{escaped_agent_name}",
                session_id: "{session_id}",
                content: "{content}",
                status: "{status}",
                token_count: 0,
                progress_seq: 0,
                reasoning_progress_seq: 0,
                created_at: "2026-03-23T00:00:00Z",
                completed_at: "{completed_at}"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create request-bound response failed: {:?}",
        response.errors
    );
}

#[tokio::test]
async fn recover_all_marks_requests_as_error() {
    let db = test_db("lifecycle-recover-error").await;
    let request_doc_id = create_request(
        &db.node,
        "stuck-1",
        "session-1",
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;
    set_execution_lease(
        &db.node,
        &request_doc_id,
        "expired-generation",
        chrono::Utc::now() - chrono::Duration::minutes(1),
        3,
    )
    .await;
    seed_accepted_request_projection(&db.node, "session-1", "stuck-1").await;

    let report = RequestLifecycle::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(report.requests_recovered, 1);

    let resp = db
        .node
        .execute(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "stuck-1" } },
                    limit: 1
                ) { request_id lifecycle_state execution_generation }
            }"#,
        )
        .await;
    let request = first_row::<StatusRow>(&resp, "AgentRequest");
    assert_eq!(request.lifecycle_state, Some(RequestLifecycleState::Failed));
    assert_ne!(
        request.execution_generation.as_deref(),
        Some("expired-generation"),
        "recovery must take ownership with a fresh generation"
    );
}

#[tokio::test]
async fn recover_all_preserves_completed_response_after_lease_expiry() {
    let db = test_db("lifecycle-recover-complete").await;
    let request_doc_id = create_request(
        &db.node,
        "stuck-complete",
        "session-complete",
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;
    set_execution_lease(
        &db.node,
        &request_doc_id,
        "expired-completed-generation",
        chrono::Utc::now() - chrono::Duration::minutes(1),
        2,
    )
    .await;
    seed_accepted_request_projection(&db.node, "session-complete", "stuck-complete").await;
    create_response_for_request(
        &db.node,
        "stuck-complete",
        "stuck-complete",
        &request_doc_id,
        "session-complete",
        "",
        "complete",
    )
    .await;
    let report = RequestLifecycle::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(report.requests_recovered, 1);

    let request_resp = db
        .node
        .execute(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "stuck-complete" } },
                    limit: 1
                ) { request_id lifecycle_state }
            }"#,
        )
        .await;
    assert_eq!(
        first_row::<StatusRow>(&request_resp, "AgentRequest").lifecycle_state,
        Some(RequestLifecycleState::Completed)
    );

    let response = db
        .node
        .execute(
            r#"{
                AgentResponse(
                    filter: { response_key: { _eq: "stuck-complete" } },
                    limit: 1
                ) { status content }
            }"#,
        )
        .await;
    let response = first_row::<ResponseStatusRow>(&response, "AgentResponse");
    assert_eq!(response.status, "complete");
    assert_eq!(response.content, "", "completed content must be preserved");
}

#[tokio::test]
async fn recover_all_marks_partial_streams_error() {
    let db = test_db("lifecycle-recover-partial").await;
    let request_doc_id = create_request(
        &db.node,
        "stuck-partial",
        "session-partial",
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;
    set_execution_lease(
        &db.node,
        &request_doc_id,
        "expired-partial-generation",
        chrono::Utc::now() - chrono::Duration::minutes(1),
        7,
    )
    .await;
    seed_accepted_request_projection(&db.node, "session-partial", "stuck-partial").await;
    create_response_for_request(
        &db.node,
        "stuck-partial",
        "stuck-partial",
        &request_doc_id,
        "session-partial",
        "partial reply",
        "streaming",
    )
    .await;
    let report = RequestLifecycle::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(report.responses_recovered, 1);
    assert_eq!(report.requests_recovered, 1);

    let response_resp = db
        .node
        .execute(
            r#"{
                AgentResponse(
                    filter: { response_key: { _eq: "stuck-partial" } },
                    limit: 1
                ) { status content }
            }"#,
        )
        .await;
    let response = first_row::<ResponseStatusRow>(&response_resp, "AgentResponse");
    assert_eq!(response.status, "error");
    assert!(response.content.contains("[Response interrupted"));
}

#[tokio::test]
async fn recover_all_creates_error_response_when_response_doc_is_missing() {
    let db = test_db("lifecycle-recover-missing").await;
    let request_doc_id = create_request(
        &db.node,
        "stuck-missing",
        "session-missing",
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;
    set_execution_lease(
        &db.node,
        &request_doc_id,
        "expired-missing-generation",
        chrono::Utc::now() - chrono::Duration::minutes(1),
        0,
    )
    .await;
    seed_accepted_request_projection(&db.node, "session-missing", "stuck-missing").await;
    let report = RequestLifecycle::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(report.responses_recovered, 1);
    assert_eq!(report.requests_recovered, 1);

    let response_resp = db
        .node
        .execute(
            r#"{
                AgentResponse(
                    filter: { response_key: { _eq: "stuck-missing" } },
                    limit: 1
                ) { status content }
            }"#,
        )
        .await;
    let response = first_row::<ResponseStatusRow>(&response_resp, "AgentResponse");
    assert_eq!(response.status, "error");
    assert!(response
        .content
        .contains("daemon restarted before response could be generated"));
}

#[tokio::test]
async fn recover_all_leaves_live_execution_lease_untouched() {
    let db = test_db("lifecycle-recover-live-lease").await;
    let request_doc_id = create_request(
        &db.node,
        "live-request",
        "live-session",
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;
    set_execution_lease(
        &db.node,
        &request_doc_id,
        "live-generation",
        chrono::Utc::now() + chrono::Duration::minutes(5),
        4,
    )
    .await;
    seed_accepted_request_projection(&db.node, "live-session", "live-request").await;
    create_response_for_request(
        &db.node,
        "live-request",
        "live-request",
        &request_doc_id,
        "live-session",
        "still running",
        "streaming",
    )
    .await;

    let report = RequestLifecycle::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(report.responses_recovered, 0);
    assert_eq!(report.requests_recovered, 0);

    let request_response = db
        .node
        .execute(
            r#"{
                AgentRequest(filter: { request_id: { _eq: "live-request" } }, limit: 1) {
                    request_id lifecycle_state execution_generation execution_progress_seq
                }
            }"#,
        )
        .await;
    let request = first_row::<StatusRow>(&request_response, "AgentRequest");
    assert_eq!(
        request.lifecycle_state,
        Some(RequestLifecycleState::Processing)
    );
    assert_eq!(
        request.execution_generation.as_deref(),
        Some("live-generation")
    );
    assert_eq!(request.execution_progress_seq, Some(4));

    let response = db
        .node
        .execute(
            r#"{
                AgentResponse(filter: { response_key: { _eq: "live-request" } }, limit: 1) {
                    status content
                }
            }"#,
        )
        .await;
    let response = first_row::<ResponseStatusRow>(&response, "AgentResponse");
    assert_eq!(response.status, "streaming");
    assert_eq!(response.content, "still running");
}

#[tokio::test]
async fn recover_all_interrupts_an_expired_lease_with_a_durable_interrupt() {
    let db = test_db("lifecycle-recover-expired-interrupt").await;
    let request_doc_id = create_request(
        &db.node,
        "interrupted-request",
        "interrupted-session",
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;
    set_execution_lease(
        &db.node,
        &request_doc_id,
        "expired-interrupt-generation",
        chrono::Utc::now() - chrono::Duration::minutes(1),
        1,
    )
    .await;
    seed_accepted_request_projection(&db.node, "interrupted-session", "interrupted-request").await;
    let interrupt_requested_at = chrono::Utc::now().to_rfc3339();
    let escaped_request_doc_id = gents::graphql::escape_graphql_string(&request_doc_id);
    let escaped_interrupt_requested_at =
        gents::graphql::escape_graphql_string(&interrupt_requested_at);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{escaped_request_doc_id}" }} }},
                input: {{ interrupt_requested_at: "{escaped_interrupt_requested_at}" }}
            ) {{ _docID }}
        }}"#
    );
    let response = db.node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "set durable interrupt failed: {:?}",
        response.errors
    );

    let report = RequestLifecycle::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(report.requests_recovered, 1);
    assert_eq!(report.responses_recovered, 1);

    let request_response = db
        .node
        .execute(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "interrupted-request" } },
                    limit: 1
                ) { request_id lifecycle_state failure_reason }
            }"#,
        )
        .await;
    let request = first_row::<StatusRow>(&request_response, "AgentRequest");
    assert_eq!(
        request.lifecycle_state,
        Some(RequestLifecycleState::Interrupted)
    );
    assert_eq!(request.failure_reason.as_deref(), Some("interrupted"));

    let response = db
        .node
        .execute(
            r#"{
                AgentResponse(
                    filter: { response_key: { _eq: "interrupted-request" } },
                    limit: 1
                ) { status content }
            }"#,
        )
        .await;
    assert_eq!(
        first_row::<ResponseStatusRow>(&response, "AgentResponse").status,
        "error"
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
    let db = test_db("tool-call-repair-notification").await;
    let agent_did = db.node_identity.did().to_string();
    create_request_for_agent_with_signed_fields(
        &db.node,
        &agent_did,
        "tool-notification-req",
        "tool-notification-session",
        "processing",
        "2026-03-23T00:00:00Z",
        None,
        None,
        None,
        None,
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
