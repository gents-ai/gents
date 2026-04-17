use defra_agent::RequestLifecycle;
use serde::Deserialize;

mod support;

use support::{
    create_request, create_response_with_content_and_status, create_response_with_status,
    first_row, test_db, upsert_conversation, AGENT_DID,
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
