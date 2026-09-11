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
    test_db, AGENT_DID, AGENT_NAME, BACKEND_ID,
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

fn background_wake_input(session_id: &str) -> gents_protocol::request_input::RequestInput {
    use gents_protocol::request_input::{QueuePolicy, QueueSource, RequestInput, RequestQueue};
    RequestInput {
        queue: Some(RequestQueue {
            source: QueueSource::BackgroundCompletion,
            policy: QueuePolicy::Coalesce,
            key: Some(format!("background_completion:{session_id}")),
            queued_after_request_id: Some("foreground-parent".into()),
            interrupted_request_id: None,
            background_completion_wake_version: Some(1),
        }),
        ..Default::default()
    }
}

#[tokio::test]
async fn failed_background_wake_redrive_is_bounded_and_idempotent() {
    let _trace = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_test_writer()
            .with_max_level(tracing::Level::WARN)
            .finish(),
    );
    let db = test_db("lifecycle-background-wake-redrive").await;
    let agent_did = db.node_identity.did().to_string();
    let input = background_wake_input("wake-redrive-session");
    let input_literal =
        gents_protocol::graphql::graphql_input_literal(&serde_json::to_value(&input).unwrap())
            .unwrap();
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "failed-wake",
                agent_did: "{agent_did}",
                requester_did: "{agent_did}",
                behavior_id: "{AGENT_NAME}",
                session_id: "wake-redrive-session",
                retry_parent_request: "",
                retry_root_request: "failed-wake",
                superseded_by_request: "",
                content: "continue after background completion",
                input: {input_literal},
                lifecycle_state: "failed",
                backend_id: "{BACKEND_ID}",
                max_total_tokens: 4096,
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
    let mut session = crate::support::session_document(
        "wake-redrive-session",
        AGENT_NAME,
        "2026-08-12T00:00:00Z",
    );
    session.agent_did = agent_did.clone();
    session.requester_did = Some(agent_did.clone());
    crate::support::create_session_document(&db.node, &session).await;
    crate::support::seed_session_observation_from_request(
        &db.node,
        "wake-redrive-session",
        "failed-wake",
        "continue after background completion",
    )
    .await;

    // Background wake recovery deliberately checks the head across requesters.
    // A newer interactive request blocks the old wake even in another scope.
    let foreign = serde_json::json!({
        "request_id":"foreign-interactive", "agent_did":agent_did,
        "requester_did":"did:test:foreign-requester", "behavior_id":AGENT_NAME,
        "session_id":"wake-redrive-session", "content":"foreign interactive",
        "lifecycle_state":"pending", "execution_origin":"interactive",
        "created_at":"2099-01-01T00:00:00Z", "retry_count":0, "max_retries":3,
        "subagent_depth":0
    });
    let foreign_input = gents_protocol::graphql::graphql_input_literal(&foreign).unwrap();
    let response = db
        .node
        .execute(&format!(
            "mutation {{ create_AgentRequest(input: {foreign_input}) {{ _docID }} }}"
        ))
        .await;
    assert!(
        !response.has_errors(),
        "foreign request fixture: {:?}",
        response.errors
    );
    let blocked = RequestLifecycle::redrive_failed_background_wakeups(&db.node, &agent_did)
        .await
        .expect("newer interactive head gate");
    assert_eq!(blocked.ineligible, 1);
    assert_eq!(blocked.redriven, 0);
    assert_eq!(blocked.failed, 0);
    // Remove only the competing fixture to exercise successful recovery below.
    let response = db.node.execute(&format!(
        r#"mutation {{ delete_AgentRequest(filter: {{ agent_did: {{ _eq: "{agent_did}" }}, request_id: {{ _eq: "foreign-interactive" }} }}) {{ _docID }} }}"#
    )).await;
    assert!(
        !response.has_errors(),
        "remove competing fixture: {:?}",
        response.errors
    );

    let (first, concurrent) = tokio::join!(
        RequestLifecycle::redrive_failed_background_wakeups(&db.node, &agent_did),
        RequestLifecycle::redrive_failed_background_wakeups(&db.node, &agent_did),
    );
    let first = first.expect("first concurrent redrive");
    let concurrent = concurrent.expect("second concurrent redrive");
    assert_eq!(first.scanned, 1);
    assert_eq!(concurrent.scanned, 1);
    assert_eq!(
        first.redriven + concurrent.redriven,
        1,
        "redrive reports: {first:?}, {concurrent:?}"
    );
    assert_eq!(first.already_redriven + concurrent.already_redriven, 1);
    assert_eq!(first.failed + concurrent.failed, 0);

    let rows = background_wake_retry_rows(&db.node, "wake-redrive-session").await;
    assert_eq!(rows.len(), 2);
    let successor = rows
        .iter()
        .find(|row| {
            row.request_id != "failed-wake"
                && row.requester_did.as_deref() == Some(agent_did.as_str())
        })
        .expect("retry successor");
    assert_eq!(
        successor.lifecycle_state,
        Some(RequestLifecycleState::Pending)
    );
    assert_eq!(successor.execution_origin.as_deref(), Some("scheduled"));
    let source = rows
        .iter()
        .find(|row| row.request_id == "failed-wake")
        .unwrap();
    assert!(source.doc_id.is_some());
    assert_eq!(successor.retry_parent_request_doc_id, source.doc_id);
    assert_eq!(
        successor.admission_kind.as_deref(),
        Some("runtime-internal")
    );
    assert_eq!(
        successor.admission_signer_did.as_deref(),
        Some(agent_did.as_str())
    );
    assert!(successor
        .admission_signature
        .as_deref()
        .is_some_and(|signature| !signature.is_empty()));
    // Claim owns inference selection and budget pinning for the new request.
    assert_eq!(successor.backend_id, None);
    assert_eq!(successor.max_total_tokens, None);
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
    assert_eq!(successor.input.as_ref(), Some(&input));
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
    let input = background_wake_input(session_id);
    let input_literal =
        gents_protocol::graphql::graphql_input_literal(&serde_json::to_value(&input).unwrap())
            .unwrap();
    let terminalized_at = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "failed-wake-backoff", agent_did: "{AGENT_DID}",
                behavior_id: "{AGENT_NAME}", session_id: "{session_id}",
                retry_parent_request: "", retry_root_request: "failed-wake-backoff",
                superseded_by_request: "", content: "continue", input: {input_literal},
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
    create_agent_session(&db.node, session_id, AGENT_NAME, &terminalized_at).await;
    crate::support::seed_session_observation_from_request(
        &db.node,
        session_id,
        "failed-wake-backoff",
        "continue",
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

    create_request(
        &db.node,
        "later-interactive-request",
        session_id,
        "pending",
        "2099-01-01T00:00:00Z",
    )
    .await;
    crate::support::seed_session_observation_from_request(
        &db.node,
        session_id,
        "later-interactive-request",
        "new user turn",
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
                    _docID request_id requester_did content lifecycle_state execution_origin
                    retry_parent_request retry_parent_request_doc_id retry_root_request retry_count max_retries
                    admission_kind admission_signer_did admission_signature backend_id max_total_tokens
                    input deadline valid_until
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
    crate::support::seed_session_observation_from_request(
        node,
        session_id,
        request_id,
        "stuck request",
    )
    .await;
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
        AGENT_DID.to_string(),
    );
    lifecycle = lifecycle.with_request_doc_id(Some(interrupted_doc.clone()));
    lifecycle.start_running().await.unwrap();
    let child = serde_json::json!({
        "request_id":"tool-cascade-child", "agent_did":AGENT_DID,
        "behavior_id":AGENT_NAME, "session_id":"tool-cascade-child-session",
        "content":"child", "lifecycle_state":"processing", "execution_origin":"interactive",
        "created_at":"2026-03-23T00:00:00Z", "retry_count":0, "max_retries":3, "subagent_depth":1,
        "caused_by_parent_request_id":"tool-cascade-parent",
        "caused_by_parent_request_doc_id":interrupted_doc,
        "caused_by_parent_tool_call_id":"tool-cascade-call",
        "caused_by_parent_tool_call_doc_id":lifecycle.doc_id().unwrap()
    });
    let input = gents_protocol::graphql::graphql_input_literal(&child).unwrap();
    let response = db
        .node
        .execute(&format!(
            "mutation {{ create_AgentRequest(input: {input}) {{ _docID }} }}"
        ))
        .await;
    assert!(
        !response.has_errors(),
        "child fixture: {:?}",
        response.errors
    );

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
        AGENT_DID.to_string(),
    );
    lifecycle = lifecycle.with_request_doc_id(Some(interrupted_doc.clone()));
    lifecycle.start_running().await.unwrap();
    let child = serde_json::json!({
        "request_id":"tool-detach-child", "agent_did":AGENT_DID,
        "behavior_id":AGENT_NAME, "session_id":"tool-detach-child-session",
        "content":"child", "lifecycle_state":"processing", "execution_origin":"interactive",
        "created_at":"2026-03-23T00:00:00Z", "retry_count":0, "max_retries":3, "subagent_depth":1,
        "caused_by_parent_request_id":"tool-detach-parent",
        "caused_by_parent_request_doc_id":interrupted_doc,
        "caused_by_parent_tool_call_id":"tool-detach-call",
        "caused_by_parent_tool_call_doc_id":lifecycle.doc_id().unwrap()
    });
    let input = gents_protocol::graphql::graphql_input_literal(&child).unwrap();
    let response = db
        .node
        .execute(&format!(
            "mutation {{ create_AgentRequest(input: {input}) {{ _docID }} }}"
        ))
        .await;
    assert!(
        !response.has_errors(),
        "child fixture: {:?}",
        response.errors
    );

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
