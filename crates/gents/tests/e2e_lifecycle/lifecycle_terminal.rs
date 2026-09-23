use gents::lifecycle::ClaimOutcome;
use gents::lifecycle::RequestTerminalOutcome;
use gents::RequestLifecycle;

use crate::support::{build_request, create_request, first_row, test_db, AGENT_DID, AGENT_NAME};

#[tokio::test]
async fn missing_session_observation_does_not_block_terminal_request() {
    let db = test_db("lifecycle-missing-terminal-projection").await;
    let session_id = "session-missing-terminal-projection";
    let request_id = "req-missing-terminal-projection";
    let request_doc_id = create_request(
        &db.node,
        request_id,
        session_id,
        "pending",
        "2026-03-23T00:00:00Z",
    )
    .await;
    let request = build_request(
        request_doc_id,
        request_id.to_string(),
        session_id.to_string(),
        "2026-03-23T00:00:00Z".to_string(),
    );
    let mut lifecycle =
        RequestLifecycle::new_with_agent_did(db.node.clone(), AGENT_NAME, AGENT_DID, request, 300);
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    let response = db
        .node
        .execute(
            r#"mutation {
                delete_AgentSession(
                    filter: { session_id: { _eq: "session-missing-terminal-projection" } }
                ) { _docID }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    crate::support::begin_owned_execution(&mut lifecycle, &db.node)
        .await
        .unwrap();
    lifecycle
        .terminalize_owned(
            RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::NoMessage,
            None,
        )
        .await
        .unwrap();

    let response = db
        .node
        .execute(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "req-missing-terminal-projection" } }
                ) { lifecycle_state }
            }"#,
        )
        .await;
    let request = first_row::<serde_json::Value>(&response, "AgentRequest");
    assert_eq!(request["lifecycle_state"], "completed");
}

#[tokio::test]
async fn complete_does_not_overwrite_session_observation_for_newer_request() {
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
    let first_request = crate::support::build_request(
        first_doc_id,
        "req-first".into(),
        session_id.into(),
        "2026-03-23T00:00:00Z".into(),
    );
    let mut lifecycle = RequestLifecycle::new_with_agent_did(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        first_request,
        300,
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);

    create_request(
        &db.node,
        "req-second",
        session_id,
        "processing",
        "2026-03-23T00:00:01Z",
    )
    .await;
    crate::support::seed_session_observation_from_request(
        &db.node,
        session_id,
        "req-second",
        "second",
    )
    .await;

    crate::support::begin_owned_execution(&mut lifecycle, &db.node)
        .await
        .unwrap();
    lifecycle
        .terminalize_owned(
            RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::NoMessage,
            None,
        )
        .await
        .unwrap();

    let session = crate::support::snapshots::fetch_session_snapshot(&db.node, session_id)
        .await
        .expect("session");
    let latest = session
        .observation
        .expect("observation")
        .latest_request
        .expect("latest request");
    assert_eq!(latest.request_id, "req-second");
    assert_eq!(
        latest.lifecycle_state,
        gents_protocol::request_lifecycle::RequestLifecycleState::Processing
    );
}
