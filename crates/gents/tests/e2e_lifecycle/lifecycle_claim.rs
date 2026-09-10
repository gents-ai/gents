use gents::lifecycle::ClaimOutcome;
use gents::watcher::{AgentRequest, DefraWatcher};
use gents::RequestLifecycle;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;

use crate::support::{
    create_request, create_request_with_valid_until, first_row, set_interrupt_requested_at,
    test_db, AGENT_DID, AGENT_NAME,
};

type StatusRow = AgentRequestRow;

#[derive(Debug, Clone, Deserialize)]
struct BehaviorRow {
    behavior_id: String,
}

type DeadlineRow = AgentRequestRow;

#[tokio::test]
async fn claim_queues_when_earlier_processing_request_exists() {
    let db = test_db("lifecycle-dedup").await;
    let session_id = uuid::Uuid::new_v4().to_string();
    let earlier = chrono::Utc::now().to_rfc3339();

    create_request(&db.node, "req-earlier", &session_id, "processing", &earlier).await;

    let later = (chrono::Utc::now() + chrono::Duration::seconds(1)).to_rfc3339();
    let doc_id = create_request(&db.node, "req-later", &session_id, "pending", &later).await;
    let request = AgentRequest {
        content: "second".into(),
        ..crate::support::build_request(doc_id, "req-later".into(), session_id, later)
    };

    let mut lifecycle =
        RequestLifecycle::new_with_agent_did(db.node.clone(), AGENT_NAME, AGENT_DID, request, 300);
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Queued);

    let resp = db
        .node
        .execute(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "req-later" } },
                    limit: 1
                ) { request_id lifecycle_state }
            }"#,
        )
        .await;
    let row = first_row::<StatusRow>(&resp, "AgentRequest");
    assert_eq!(row.lifecycle_state, Some(RequestLifecycleState::Pending));
}

#[tokio::test]
async fn queued_request_interrupt_wins_before_queue_block() {
    let db = test_db("lifecycle-queued-interrupt").await;
    let session_id = uuid::Uuid::new_v4().to_string();
    let earlier = "2026-03-23T00:00:00Z";

    create_request(
        &db.node,
        "req-earlier-active",
        &session_id,
        "processing",
        earlier,
    )
    .await;

    let later = "2026-03-23T00:00:01Z";
    let doc_id = create_request(
        &db.node,
        "req-later-interrupted",
        &session_id,
        "pending",
        later,
    )
    .await;
    let interrupt_at = chrono::Utc::now().to_rfc3339();
    set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;

    let request = AgentRequest {
        content: "second".into(),
        ..crate::support::build_request(
            doc_id,
            "req-later-interrupted".into(),
            session_id,
            later.into(),
        )
    };

    let mut lifecycle =
        RequestLifecycle::new_with_agent_did(db.node.clone(), AGENT_NAME, AGENT_DID, request, 300);
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Interrupted);

    let resp = db
        .node
        .execute(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "req-later-interrupted" } },
                    limit: 1
                ) { request_id lifecycle_state }
            }"#,
        )
        .await;
    let row = first_row::<StatusRow>(&resp, "AgentRequest");
    assert_eq!(
        row.lifecycle_state,
        Some(RequestLifecycleState::Interrupted)
    );
}

#[tokio::test]
async fn queued_request_valid_until_wins_before_queue_block() {
    let db = test_db("lifecycle-queued-valid-until").await;
    let session_id = uuid::Uuid::new_v4().to_string();
    let earlier = "2026-03-23T00:00:00Z";

    create_request(
        &db.node,
        "req-earlier-active",
        &session_id,
        "processing",
        earlier,
    )
    .await;

    let later = "2026-03-23T00:00:01Z";
    let expired_at = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    let doc_id = create_request_with_valid_until(
        &db.node,
        "req-later-expired",
        &session_id,
        "pending",
        later,
        Some(&expired_at),
    )
    .await;

    let request = AgentRequest {
        content: "second".into(),
        ..crate::support::build_request(
            doc_id,
            "req-later-expired".into(),
            session_id,
            later.into(),
        )
    };

    let mut lifecycle =
        RequestLifecycle::new_with_agent_did(db.node.clone(), AGENT_NAME, AGENT_DID, request, 300);
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Expired);

    let resp = db
        .node
        .execute(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "req-later-expired" } },
                    limit: 1
                ) { request_id lifecycle_state }
            }"#,
        )
        .await;
    let row = first_row::<StatusRow>(&resp, "AgentRequest");
    assert_eq!(row.lifecycle_state, Some(RequestLifecycleState::Dead));
}

#[tokio::test]
async fn earliest_pending_claim_leaves_later_same_session_pending() {
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
        content: "first".into(),
        ..crate::support::build_request(
            early_doc_id,
            "req-early".into(),
            session_id.clone(),
            "2026-03-23T00:00:00Z".into(),
        )
    };

    let mut lifecycle =
        RequestLifecycle::new_with_agent_did(db.node.clone(), AGENT_NAME, AGENT_DID, request, 300);
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);

    let resp = db
        .node
        .execute(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "req-late" } },
                    limit: 1
                ) { request_id lifecycle_state }
            }"#,
        )
        .await;
    let row = first_row::<StatusRow>(&resp, "AgentRequest");
    assert_eq!(row.lifecycle_state, Some(RequestLifecycleState::Pending));
}

#[tokio::test]
async fn same_timestamp_queue_order_uses_request_id_tie_break() {
    let db = test_db("lifecycle-same-timestamp-order").await;
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = "2026-03-23T00:00:00Z";
    let first_doc_id = create_request(&db.node, "req-a", &session_id, "pending", created_at).await;
    let second_doc_id = create_request(&db.node, "req-b", &session_id, "pending", created_at).await;

    let second_request = AgentRequest {
        content: "second".into(),
        ..crate::support::build_request(
            second_doc_id,
            "req-b".into(),
            session_id.clone(),
            created_at.into(),
        )
    };
    let mut second_lifecycle = RequestLifecycle::new_with_agent_did(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        second_request,
        300,
    );
    assert_eq!(
        second_lifecycle.claim().await.unwrap(),
        ClaimOutcome::Queued
    );

    let first_request = AgentRequest {
        content: "first".into(),
        ..crate::support::build_request(first_doc_id, "req-a".into(), session_id, created_at.into())
    };
    let mut first_lifecycle = RequestLifecycle::new_with_agent_did(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        first_request,
        300,
    );
    assert_eq!(
        first_lifecycle.claim().await.unwrap(),
        ClaimOutcome::Claimed
    );
}

#[tokio::test]
async fn terminal_earlier_request_allows_later_same_session_claim() {
    let db = test_db("lifecycle-terminal-allows-next").await;
    let session_id = uuid::Uuid::new_v4().to_string();

    create_request(
        &db.node,
        "req-earlier-terminal",
        &session_id,
        "completed",
        "2026-03-23T00:00:00Z",
    )
    .await;
    let later_doc_id = create_request(
        &db.node,
        "req-later-after-terminal",
        &session_id,
        "pending",
        "2026-03-23T00:00:01Z",
    )
    .await;

    let request = AgentRequest {
        content: "second".into(),
        ..crate::support::build_request(
            later_doc_id,
            "req-later-after-terminal".into(),
            session_id,
            "2026-03-23T00:00:01Z".into(),
        )
    };

    let mut lifecycle =
        RequestLifecycle::new_with_agent_did(db.node.clone(), AGENT_NAME, AGENT_DID, request, 300);
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);

    let resp = db
        .node
        .execute(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "req-later-after-terminal" } },
                    limit: 1
                ) { request_id lifecycle_state }
            }"#,
        )
        .await;
    let row = first_row::<StatusRow>(&resp, "AgentRequest");
    assert_eq!(row.lifecycle_state, Some(RequestLifecycleState::Claimed));
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
                lifecycle_state: "pending",
                backend_id: "",
                execution_origin: "interactive",
                created_at: "{created_at}",
                retry_count: 0,
                max_retries: {max_retries}
            }}) {{ _docID }}
        }}"#,
        max_retries = gents::lifecycle::DEFAULT_REQUEST_MAX_RETRIES,
    );
    let resp = db.node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create request failed: {:?}",
        resp.errors
    );

    let doc_id = first_row::<crate::support::DocIdRow>(
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
        behavior_id: "code".into(),
        ..crate::support::build_request(
            doc_id.clone(),
            request_id.into(),
            session_id.into(),
            created_at.into(),
        )
    };

    let mut lifecycle =
        RequestLifecycle::new_with_agent_did(db.node.clone(), AGENT_NAME, AGENT_DID, request, 300);
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
async fn claim_rejects_a_behavior_change_without_mutating_the_session() {
    let db = test_db("lifecycle-behavior-pin").await;
    let mutation = format!(
        r#"mutation {{
            session: create_AgentSession(input: {{
                session_id: "session-pinned",
                agent_did: "{AGENT_DID}",
                behavior_id: "general",
                created_at: "2026-03-23T00:00:00Z"
            }}) {{ _docID }}
            request: create_AgentRequest(input: {{
                request_id: "req-switch",
                agent_did: "{AGENT_DID}",
                behavior_id: "code",
                session_id: "session-pinned",
                retry_root_request: "req-switch",
                content: "switch behavior",
                lifecycle_state: "pending",
                execution_origin: "interactive",
                created_at: "2026-03-23T00:00:01Z",
                retry_count: 0,
                max_retries: 3
            }}) {{ _docID }}
        }}"#,
    );
    let response = db.node.execute(&mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let doc_id = first_row::<crate::support::DocIdRow>(
        &db.node
            .execute(
                r#"{
                    AgentRequest(filter: { request_id: { _eq: "req-switch" } }) { _docID }
                }"#,
            )
            .await,
        "AgentRequest",
    )
    .doc_id;
    let request = DefraWatcher::new(db.node.clone(), AGENT_DID)
        .try_fetch_request(&doc_id)
        .await
        .unwrap()
        .expect("pending request");
    let mut lifecycle =
        RequestLifecycle::new_with_agent_did(db.node.clone(), AGENT_NAME, AGENT_DID, request, 300);
    let error = lifecycle
        .claim()
        .await
        .expect_err("behavior switch must fail");
    assert!(error.to_string().contains("pinned to behavior general"));

    let request = db
        .node
        .execute(
            r#"{
                AgentRequest(filter: { request_id: { _eq: "req-switch" } }) {
                    request_id
                    lifecycle_state
                }
            }"#,
        )
        .await;
    let request = first_row::<StatusRow>(&request, "AgentRequest");
    assert_eq!(
        request.lifecycle_state,
        Some(RequestLifecycleState::Pending)
    );

    let sessions = db
        .node
        .execute(
            r#"{
                AgentSession(filter: { session_id: { _eq: "session-pinned" } }) { behavior_id }
            }"#,
        )
        .await;
    let rows = sessions
        .data
        .as_ref()
        .and_then(|data| data.get("AgentSession"))
        .and_then(serde_json::Value::as_array)
        .expect("session rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0]
            .get("behavior_id")
            .and_then(serde_json::Value::as_str),
        Some("general")
    );
}

#[tokio::test]
async fn claim_preserves_explicit_request_deadline() {
    let db = test_db("lifecycle-explicit-deadline").await;
    let request_id = "req-explicit-deadline";
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let explicit_deadline_at = chrono::Utc::now() + chrono::Duration::minutes(5);
    let explicit_deadline = explicit_deadline_at.to_rfc3339();
    let escaped_session_id = gents::graphql::escape_graphql_string(&session_id);
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "{AGENT_DID}",
                behavior_id: "{AGENT_NAME}",
                session_id: "{escaped_session_id}",
                retry_parent_request: "",
                retry_root_request: "{request_id}",
                superseded_by_request: "",
                content: "hello",
                lifecycle_state: "pending",
                backend_id: "",
                execution_origin: "interactive",
                created_at: "{created_at}",
                deadline: "{explicit_deadline}",
                retry_count: 0,
                max_retries: {max_retries}
            }}) {{ _docID }}
        }}"#,
        max_retries = gents::lifecycle::DEFAULT_REQUEST_MAX_RETRIES,
    );
    let resp = db.node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create request failed: {:?}",
        resp.errors
    );

    let doc_id = first_row::<crate::support::DocIdRow>(
        &db.node
            .execute(
                r#"{
                    AgentRequest(
                        filter: { request_id: { _eq: "req-explicit-deadline" } },
                        limit: 1
                    ) { _docID }
                }"#,
            )
            .await,
        "AgentRequest",
    )
    .doc_id;
    let watcher = DefraWatcher::new(db.node.clone(), AGENT_DID);
    let request = watcher
        .try_fetch_request(&doc_id)
        .await
        .unwrap()
        .expect("pending request");

    let mut lifecycle =
        RequestLifecycle::new_with_agent_did(db.node.clone(), AGENT_NAME, AGENT_DID, request, 3600);
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);

    let resp = db
        .node
        .execute(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "req-explicit-deadline" } },
                    limit: 1
                ) { request_id deadline }
            }"#,
        )
        .await;
    let persisted = first_row::<DeadlineRow>(&resp, "AgentRequest")
        .deadline
        .expect("AgentRequest.deadline");
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(&persisted).unwrap(),
        chrono::DateTime::parse_from_rfc3339(&explicit_deadline).unwrap()
    );
}

#[tokio::test]
async fn claim_synthesizes_deadline_when_request_deadline_is_invalid() {
    let db = test_db("lifecycle-invalid-deadline").await;
    let request_id = "req-invalid-deadline";
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let invalid_deadline = "not-a-deadline";
    let escaped_session_id = gents::graphql::escape_graphql_string(&session_id);
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "{AGENT_DID}",
                behavior_id: "{AGENT_NAME}",
                session_id: "{escaped_session_id}",
                retry_parent_request: "",
                retry_root_request: "{request_id}",
                superseded_by_request: "",
                content: "hello",
                lifecycle_state: "pending",
                backend_id: "",
                execution_origin: "interactive",
                created_at: "{created_at}",
                deadline: "{invalid_deadline}",
                retry_count: 0,
                max_retries: {max_retries}
            }}) {{ _docID }}
        }}"#,
        max_retries = gents::lifecycle::DEFAULT_REQUEST_MAX_RETRIES,
    );
    let resp = db.node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create request failed: {:?}",
        resp.errors
    );

    let doc_id = first_row::<crate::support::DocIdRow>(
        &db.node
            .execute(
                r#"{
                    AgentRequest(
                        filter: { request_id: { _eq: "req-invalid-deadline" } },
                        limit: 1
                    ) { _docID }
                }"#,
            )
            .await,
        "AgentRequest",
    )
    .doc_id;
    let watcher = DefraWatcher::new(db.node.clone(), AGENT_DID);
    let request = watcher
        .try_fetch_request(&doc_id)
        .await
        .unwrap()
        .expect("pending request");
    assert_eq!(request.deadline.as_deref(), Some(invalid_deadline));

    let before_claim = chrono::Utc::now();
    let mut lifecycle =
        RequestLifecycle::new_with_agent_did(db.node.clone(), AGENT_NAME, AGENT_DID, request, 120);
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    let after_claim = chrono::Utc::now();

    let resp = db
        .node
        .execute(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "req-invalid-deadline" } },
                    limit: 1
                ) { request_id deadline }
            }"#,
        )
        .await;
    let persisted = first_row::<DeadlineRow>(&resp, "AgentRequest")
        .deadline
        .expect("AgentRequest.deadline");
    assert_ne!(persisted, invalid_deadline);

    let persisted_deadline = chrono::DateTime::parse_from_rfc3339(&persisted)
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert!(persisted_deadline >= before_claim + chrono::Duration::seconds(120));
    assert!(persisted_deadline <= after_claim + chrono::Duration::seconds(121));
}
