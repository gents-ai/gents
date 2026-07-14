use defra_agent::{
    fetch_interrupt_requested_at,
    tool_call_lifecycle::{AwaitMode, CancelPolicy, ToolCallLifecycle},
    RequestLifecycle,
};
use serde::Deserialize;

use crate::support::snapshots::fetch_tool_call_snapshots_for_session;
use crate::support::{
    create_conversation_document, create_request, create_response_with_content_and_status,
    create_response_with_status, first_row, test_db, test_db_with_duplicate_capable_conversations,
    upsert_conversation, AGENT_DID, AGENT_NAME,
};

#[derive(Debug, Clone, Deserialize)]
struct StatusRow {
    status: String,
}

/// Thread-scoped tracing capture so a test can assert on the WARN a recovery
/// pass emits, without touching the process-global subscriber.
#[derive(Clone, Default)]
struct LogCapture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl LogCapture {
    fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("capture lock")).into_owned()
    }
}

impl std::io::Write for LogCapture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("capture lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
    type Writer = LogCapture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn capture_subscriber(capture: &LogCapture) -> impl tracing::Subscriber {
    use tracing_subscriber::layer::SubscriberExt;
    tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(capture.clone()),
    )
}

async fn conversation_status_by_doc_id(
    node: &defra_agent::defra_node::EmbeddedNode,
    doc_id: &str,
) -> String {
    let query = format!(
        r#"{{
            AgentConversation(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                limit: 1
            ) {{ status }}
        }}"#
    );
    let resp = node.execute(&query).await;
    first_row::<ConversationRow>(&resp, "AgentConversation").status
}

#[derive(Debug, Clone, Deserialize)]
struct ResponseStatusRow {
    status: String,
    content: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ConversationRow {
    status: String,
}

async fn mark_request_interrupted(node: &defra_agent::defra_node::EmbeddedNode, doc_id: &str) {
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
    let db = test_db("lifecycle-recover-error").await;
    create_request(
        &db.node,
        "stuck-1",
        "session-1",
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;

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
    let db = test_db("lifecycle-recover-complete").await;
    create_request(
        &db.node,
        "stuck-complete",
        "session-complete",
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;
    create_response_with_status(
        &db.node,
        "stuck-complete",
        "stuck-complete",
        "session-complete",
        "complete",
    )
    .await;
    upsert_conversation(
        &db.node,
        "session-complete",
        "stuck-complete",
        "hello",
        "processing",
    )
    .await;

    let report = RequestLifecycle::recover_all(&db.node, AGENT_DID)
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
    let db = test_db("lifecycle-recover-partial").await;
    create_request(
        &db.node,
        "stuck-partial",
        "session-partial",
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;
    create_response_with_content_and_status(
        &db.node,
        "stuck-partial",
        "stuck-partial",
        "session-partial",
        "partial reply",
        "streaming",
    )
    .await;
    upsert_conversation(
        &db.node,
        "session-partial",
        "stuck-partial",
        "hello",
        "processing",
    )
    .await;

    let report = RequestLifecycle::recover_all(&db.node, AGENT_DID)
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
                ) { status content }
            }"#,
        )
        .await;
    let response = first_row::<ResponseStatusRow>(&response_resp, "AgentResponse");
    assert_eq!(response.status, "error");
    assert!(response.content.contains("[Response interrupted"));

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
async fn recover_all_creates_error_response_when_response_doc_is_missing() {
    let db = test_db("lifecycle-recover-missing").await;
    create_request(
        &db.node,
        "stuck-missing",
        "session-missing",
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;
    upsert_conversation(
        &db.node,
        "session-missing",
        "stuck-missing",
        "hello",
        "processing",
    )
    .await;

    let report = RequestLifecycle::recover_all(&db.node, AGENT_DID)
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

/// #693 defect 1: a store carrying two `AgentConversation` docs with one
/// `session_id` must still recover — the canonical doc (latest `updated_at`,
/// `_docID` tie-break) is repaired, the duplicate is flagged by a WARN naming
/// the session_id and duplicate count, and is otherwise left untouched.
#[tokio::test]
async fn recover_all_recovers_canonical_conversation_when_session_id_is_duplicated() {
    let db = test_db_with_duplicate_capable_conversations("lifecycle-recover-dupe").await;
    let stale_doc = create_conversation_document(
        &db.node,
        "session-dupe",
        AGENT_NAME,
        "Stale title",
        "processing",
        "req-dupe",
        "2026-01-01T00:00:00Z",
    )
    .await;
    let canonical_doc = create_conversation_document(
        &db.node,
        "session-dupe",
        AGENT_NAME,
        "Rich title",
        "processing",
        "req-dupe",
        "2026-06-01T00:00:00Z",
    )
    .await;
    assert_ne!(stale_doc, canonical_doc, "seeding must mint two documents");

    let capture = LogCapture::default();
    let report = {
        let _guard = tracing::subscriber::set_default(capture_subscriber(&capture));
        RequestLifecycle::recover_all(&db.node, AGENT_DID)
            .await
            .unwrap()
    };

    // One duplicated session = one recovery, and the count reflects only
    // recoveries that actually happened (#693 defect 2).
    assert_eq!(report.conversations_recovered, 1);
    assert_eq!(report.conversations_failed, 0);

    // The canonical (newest) doc is recovered; latest_request_id has no
    // matching request, so it re-activates.
    assert_eq!(
        conversation_status_by_doc_id(&db.node, &canonical_doc).await,
        "active"
    );
    // The duplicate is flagged, not reaped or mutated (v1: operators sweep).
    assert_eq!(
        conversation_status_by_doc_id(&db.node, &stale_doc).await,
        "processing"
    );

    let logs = capture.contents();
    assert!(
        logs.contains("duplicate AgentConversation"),
        "expected duplicate WARN, got logs:\n{logs}"
    );
    assert!(
        logs.contains("session-dupe"),
        "duplicate WARN must name the session_id, got logs:\n{logs}"
    );
    assert!(
        logs.contains("duplicate_count=2"),
        "duplicate WARN must carry the duplicate count, got logs:\n{logs}"
    );
}

/// #693 defect 2: a conversation recovery attempt that fails must be counted
/// as a failure, never as a recovery. Failure injected via a behavior_id
/// conflict between the stuck doc and its newer duplicate twin.
#[tokio::test]
async fn recover_all_counts_only_successful_conversation_recoveries() {
    let db = test_db_with_duplicate_capable_conversations("lifecycle-recover-dupe-fail").await;
    create_conversation_document(
        &db.node,
        "session-mismatch",
        "behavior-alpha",
        "Stale title",
        "processing",
        "req-mismatch",
        "2026-01-01T00:00:00Z",
    )
    .await;
    create_conversation_document(
        &db.node,
        "session-mismatch",
        "behavior-beta",
        "Rich title",
        "active",
        "req-mismatch",
        "2026-06-01T00:00:00Z",
    )
    .await;

    let report = RequestLifecycle::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();

    assert_eq!(
        report.conversations_recovered, 0,
        "a failed recovery attempt must not be counted as recovered"
    );
    assert_eq!(report.conversations_failed, 1);
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
        "did:defra-agent:test".to_string(),
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
        "did:defra-agent:test".to_string(),
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
        "did:defra-agent:test".to_string(),
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
        "did:defra-agent:test".to_string(),
        "tool-cascade-call".to_string(),
        1,
        "spawn_subagent".to_string(),
        "{}".to_string(),
        chrono::Utc::now() + chrono::Duration::minutes(5),
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
        "tool-cascade-child".to_string(),
        "did:defra-agent:target".to_string(),
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
        "did:defra-agent:test".to_string(),
        "tool-detach-call".to_string(),
        1,
        "spawn_subagent".to_string(),
        "{}".to_string(),
        chrono::Utc::now() + chrono::Duration::minutes(5),
        AwaitMode::Background,
        CancelPolicy::Detach,
        "tool-detach-child".to_string(),
        "did:defra-agent:target".to_string(),
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
