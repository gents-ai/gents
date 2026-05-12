use defra_agent::lifecycle::ClaimOutcome;
use defra_agent::watcher::{AgentRequest, DefraWatcher};
use defra_agent::RequestLifecycle;
use serde::Deserialize;

mod support;

use support::{create_request, first_row, test_db, AGENT_DID, AGENT_NAME};

#[derive(Debug, Clone, Deserialize)]
struct StatusRow {
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct BehaviorRow {
    behavior_id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct DeadlineRow {
    deadline: String,
}

#[tokio::test]
async fn pending_request_hydrates_sampling_fields_and_metadata() {
    let db = test_db("request-sampling-metadata").await;
    let request_id = "req-sampling-metadata";
    let session_id = "session-sampling-metadata";
    let metadata = r#" { "run_id": "foo" } "#;
    let deadline = "2026-03-23T00:05:00Z";
    let escaped_metadata = defra_agent::graphql::escape_graphql_string(metadata);
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "{AGENT_DID}",
                behavior_id: "{AGENT_NAME}",
                session_id: "{session_id}",
                retry_parent_request: "",
                retry_root_request: "{request_id}",
                superseded_by_request: "",
                content: "visible prompt",
                temperature: 0.0,
                top_p: 0.95,
                top_k: 40,
                max_tokens: 512,
                metadata: "{escaped_metadata}",
                status: "pending",
                lifecycle_state: "pending",
                backend_id: "",
                execution_origin: "interactive",
                created_at: "2026-03-23T00:00:00Z",
                deadline: "{deadline}",
                retry_count: 0,
                max_retries: 3
            }}) {{ _docID }}
        }}"#,
    );
    let resp = db.node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create request failed: {:?}",
        resp.errors
    );
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{
                _docID
            }}
        }}"#,
    );
    let resp = db.node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "query request failed: {:?}",
        resp.errors
    );
    let doc_id = first_row::<support::DocIdRow>(&resp, "AgentRequest").doc_id;

    let watcher = DefraWatcher::new(db.node.clone(), AGENT_DID);
    let request = watcher
        .try_fetch_request(&doc_id)
        .await
        .unwrap()
        .expect("pending request");

    assert_eq!(request.temperature, Some(0.0));
    assert_eq!(request.top_p, Some(0.95));
    assert_eq!(request.top_k, Some(40));
    assert_eq!(request.max_tokens, Some(512));
    assert_eq!(request.metadata.as_deref(), Some(metadata));
    assert_eq!(request.deadline.as_deref(), Some(deadline));
    assert_eq!(request.content, "visible prompt");
    assert!(!request.content.contains("run_id"));
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
        temperature: None,
        top_p: None,
        top_k: None,
        max_tokens: None,
        metadata: None,
        execution_origin: None,
        created_at: later,
        deadline: None,
        subagent_depth: 0,
        caused_by_parent_request_id: None,
        caused_by_parent_tool_call_id: None,
    };

    let mut lifecycle =
        RequestLifecycle::new_with_agent_did(db.node.clone(), AGENT_NAME, AGENT_DID, request, 300);
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
        temperature: None,
        top_p: None,
        top_k: None,
        max_tokens: None,
        metadata: None,
        execution_origin: None,
        created_at: "2026-03-23T00:00:00Z".into(),
        deadline: None,
        subagent_depth: 0,
        caused_by_parent_request_id: None,
        caused_by_parent_tool_call_id: None,
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
        temperature: None,
        top_p: None,
        top_k: None,
        max_tokens: None,
        metadata: None,
        execution_origin: None,
        created_at: created_at.into(),
        deadline: None,
        subagent_depth: 0,
        caused_by_parent_request_id: None,
        caused_by_parent_tool_call_id: None,
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
async fn claim_preserves_explicit_request_deadline() {
    let db = test_db("lifecycle-explicit-deadline").await;
    let request_id = "req-explicit-deadline";
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let explicit_deadline_at = chrono::Utc::now() + chrono::Duration::minutes(5);
    let explicit_deadline = explicit_deadline_at.to_rfc3339();
    let escaped_session_id = defra_agent::graphql::escape_graphql_string(&session_id);
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
                status: "pending",
                lifecycle_state: "pending",
                backend_id: "",
                execution_origin: "interactive",
                created_at: "{created_at}",
                deadline: "{explicit_deadline}",
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
                ) { deadline }
            }"#,
        )
        .await;
    let persisted = first_row::<DeadlineRow>(&resp, "AgentRequest").deadline;
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
    let escaped_session_id = defra_agent::graphql::escape_graphql_string(&session_id);
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
                status: "pending",
                lifecycle_state: "pending",
                backend_id: "",
                execution_origin: "interactive",
                created_at: "{created_at}",
                deadline: "{invalid_deadline}",
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
                ) { deadline }
            }"#,
        )
        .await;
    let persisted = first_row::<DeadlineRow>(&resp, "AgentRequest").deadline;
    assert_ne!(persisted, invalid_deadline);

    let persisted_deadline = chrono::DateTime::parse_from_rfc3339(&persisted)
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert!(persisted_deadline >= before_claim + chrono::Duration::seconds(120));
    assert!(persisted_deadline <= after_claim + chrono::Duration::seconds(121));
}
