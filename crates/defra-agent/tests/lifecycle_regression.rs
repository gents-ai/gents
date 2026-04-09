use defra_agent::lifecycle::ClaimOutcome;
use defra_agent::watcher::AgentRequest;
use defra_agent::RequestLifecycle;
use serde::Deserialize;

mod support;

use support::{
    create_request, create_response, create_response_with_content_and_status,
    create_response_with_status, first_row, test_db, upsert_conversation, AGENT_DID, AGENT_NAME,
};

#[derive(Debug, Clone, Deserialize)]
struct StatusRow {
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponseStatusRow {
    status: String,
    content: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ConversationRow {
    status: String,
    latest_request_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProgressRow {
    progress_seq: i64,
}

#[derive(Debug, Clone, Deserialize)]
struct BehaviorRow {
    behavior_id: String,
}

#[tokio::test]
async fn claim_rejects_when_another_non_terminal_request_exists() {
    let db = test_db("lifecycle-dedup").await;
    let session_id = uuid::Uuid::new_v4().to_string();
    let earlier = chrono::Utc::now().to_rfc3339();

    create_request(&db.node, "req-earlier", &session_id, "processing", &earlier).await;

    let later = (chrono::Utc::now() + chrono::Duration::seconds(1)).to_rfc3339();
    let doc_id = create_request(&db.node, "req-later", &session_id, "pending", &later).await;
    let request = AgentRequest {
        doc_id,
        request_id: "req-later".into(),
        agent_did: AGENT_DID.into(),
        behavior_id: Some(AGENT_NAME.into()),
        session_id,
        content: "second".into(),
        created_at: later,
    };

    let mut lifecycle = RequestLifecycle::new(db.node.clone(), AGENT_NAME, request, 300);
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Superseded);

    let resp = db
        .node
        .execute(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "req-later" } },
                    limit: 1
                ) { status }
            }"#,
        )
        .await;
    assert_eq!(
        first_row::<StatusRow>(&resp, "AgentRequest").status,
        "superseded"
    );
}

#[tokio::test]
async fn claim_suppresses_later_pending_duplicates() {
    let db = test_db("lifecycle-dedup-suppress").await;
    let session_id = uuid::Uuid::new_v4().to_string();
    let early_doc_id = create_request(
        &db.node,
        "req-early",
        &session_id,
        "pending",
        "2026-03-23T00:00:00Z",
    )
    .await;
    create_request(
        &db.node,
        "req-late",
        &session_id,
        "pending",
        "2026-03-23T00:00:01Z",
    )
    .await;

    let request = AgentRequest {
        doc_id: early_doc_id,
        request_id: "req-early".into(),
        agent_did: AGENT_DID.into(),
        behavior_id: Some(AGENT_NAME.into()),
        session_id: session_id.clone(),
        content: "first".into(),
        created_at: "2026-03-23T00:00:00Z".into(),
    };

    let mut lifecycle = RequestLifecycle::new(db.node.clone(), AGENT_NAME, request, 300);
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);

    let resp = db
        .node
        .execute(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "req-late" } },
                    limit: 1
                ) { status }
            }"#,
        )
        .await;
    assert_eq!(
        first_row::<StatusRow>(&resp, "AgentRequest").status,
        "superseded"
    );
}

#[tokio::test]
async fn claim_preserves_explicit_behavior_id() {
    let db = test_db("lifecycle-explicit-behavior").await;
    let request_id = "req-explicit";
    let session_id = "session-explicit";
    let created_at = "2026-03-23T00:00:00Z";
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "{AGENT_DID}",
                behavior_id: "code",
                session_id: "{session_id}",
                retry_parent_request: "",
                retry_root_request: "{request_id}",
                superseded_by_request: "",
                content: "hello",
                status: "pending",
                lifecycle_state: "pending",
                admission_state: "released",
                backend_id: "",
                execution_origin: "interactive",
                created_at: "{created_at}",
                retry_count: 0,
                max_retries: {max_retries}
            }}) {{ _docID }}
        }}"#,
        max_retries = defra_agent::lifecycle::DEFAULT_REQUEST_MAX_RETRIES,
    );
    let resp = db.node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create request failed: {:?}",
        resp.errors
    );

    let doc_id = first_row::<support::DocIdRow>(
        &db.node
            .execute(
                r#"{
                    AgentRequest(filter: { request_id: { _eq: "req-explicit" } }, limit: 1) {
                        _docID
                    }
                }"#,
            )
            .await,
        "AgentRequest",
    )
    .doc_id;
    let request = AgentRequest {
        doc_id: doc_id.clone(),
        request_id: request_id.into(),
        agent_did: AGENT_DID.into(),
        behavior_id: Some("code".into()),
        session_id: session_id.into(),
        content: "hello".into(),
        created_at: created_at.into(),
    };

    let mut lifecycle = RequestLifecycle::new(db.node.clone(), AGENT_NAME, request, 300);
    assert_eq!(lifecycle.behavior_id(), "code");
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);

    let resp = db
        .node
        .execute(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "req-explicit" } },
                    limit: 1
                ) { behavior_id }
            }"#,
        )
        .await;
    assert_eq!(
        first_row::<BehaviorRow>(&resp, "AgentRequest").behavior_id,
        "code"
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

#[tokio::test]
async fn complete_does_not_overwrite_conversation_for_newer_request() {
    let db = test_db("lifecycle-stale-complete").await;
    let session_id = "session-stale";
    let first_doc_id = create_request(
        &db.node,
        "req-first",
        session_id,
        "pending",
        "2026-03-23T00:00:00Z",
    )
    .await;
    let first_request = AgentRequest {
        doc_id: first_doc_id,
        request_id: "req-first".into(),
        agent_did: AGENT_DID.into(),
        behavior_id: Some(AGENT_NAME.into()),
        session_id: session_id.into(),
        content: "hello".into(),
        created_at: "2026-03-23T00:00:00Z".into(),
    };
    let mut lifecycle = RequestLifecycle::new(db.node.clone(), AGENT_NAME, first_request, 300);
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);

    upsert_conversation(&db.node, session_id, "req-second", "second", "processing").await;

    lifecycle.complete().await.unwrap();

    let conversation_resp = db
        .node
        .execute(
            r#"{
                AgentConversation(
                    filter: { session_id: { _eq: "session-stale" } },
                    limit: 1
                ) { status latest_request_id }
            }"#,
        )
        .await;
    let conversation = first_row::<ConversationRow>(&conversation_resp, "AgentConversation");
    assert_eq!(
        conversation.latest_request_id.as_deref(),
        Some("req-second")
    );
    assert_eq!(conversation.status, "processing");
}

#[tokio::test]
async fn advance_increments_progress_seq() {
    let db = test_db("lifecycle-advance").await;
    let request_doc_id = create_request(
        &db.node,
        "req-1",
        "session-1",
        "pending",
        "2026-03-23T00:00:00Z",
    )
    .await;
    let response_doc_id = create_response(&db.node, "resp-1").await;
    let request = AgentRequest {
        doc_id: request_doc_id,
        request_id: "req-1".into(),
        agent_did: AGENT_DID.into(),
        behavior_id: Some(AGENT_NAME.into()),
        session_id: "session-1".into(),
        content: "hello".into(),
        created_at: "2026-03-23T00:00:00Z".into(),
    };

    let mut lifecycle = RequestLifecycle::new(db.node.clone(), AGENT_NAME, request, 300);
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle.set_response_doc_id(&response_doc_id);
    lifecycle.advance().await.unwrap();
    lifecycle.advance().await.unwrap();
    lifecycle.advance().await.unwrap();

    let query = format!(
        r#"{{
            AgentResponse(
                filter: {{ _docID: {{ _eq: "{response_doc_id}" }} }},
                limit: 1
            ) {{ progress_seq }}
        }}"#
    );
    let resp = db.node.execute(&query).await;
    assert_eq!(
        first_row::<ProgressRow>(&resp, "AgentResponse").progress_seq,
        3
    );
}
