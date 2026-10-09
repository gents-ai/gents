use std::sync::Arc;
use std::time::Duration;

use gents::NodeIdentity;

use crate::mailbox_tool_turn::{configure_engineer_mailbox, request_state};
use crate::support::accepted_turn::{
    boot_prepared_accepted_turn, prepare_accepted_turn, AcceptedTurnSpec,
};
use crate::support::live_inference::wait_for_request_terminal;
use crate::support::streaming_backend::{StreamChunk, StreamResponse};
use crate::support::test_db;

const AGENT: &str = "mailbox-turn-engineer";

async fn queue_user_message(
    db: &crate::support::TestDb,
    did: &str,
    session_id: &str,
    request_id: &str,
    content: &str,
    active: &str,
) {
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        request_id,
        did,
        did,
        AGENT,
        session_id,
        content,
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(did),
    );
    create.input.queue = Some(gents_protocol::request_input::RequestQueue {
        source: gents_protocol::request_input::QueueSource::User,
        policy: gents_protocol::request_input::QueuePolicy::Append,
        key: None,
        queued_after_request_id: Some(active.into()),
        interrupted_request_id: None,
        background_completion_wake_version: None,
    });
    gents::sign_agent_request_create_as_registered_target(&mut create)
        .await
        .unwrap();
    let created = db.node.execute(&create.graphql_mutation().unwrap()).await;
    assert!(!created.has_errors(), "{:?}", created.errors);
}

/// Two messages written while the session is busy are answered by one turn:
/// the first claims, the second is superseded into it, and both reach the
/// provider in order as distinct user messages.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn messages_queued_behind_a_busy_turn_are_answered_by_one_inference() {
    let db = test_db("queued-user-messages").await;
    let did = db.node_identity.did().to_string();
    let session_id = "queued-user-messages-session";
    let asking = "queued-user-messages-active";
    let prompt = "queued-user-messages-prompt";
    let prepared = prepare_accepted_turn(
        &db,
        AcceptedTurnSpec {
            backend_id: "queued-user-messages-backend",
            model: "queued-user-messages-model",
            parent_agent_id: AGENT,
            configured_agent_ids: &[AGENT],
            request_id: asking,
            session_id,
            prompt,
            accepted_chunks: vec![StreamChunk::tool_call(
                "queued-user-messages-call",
                gents::mailbox::FILE_MAILBOX_ITEM_TOOL_NAME,
                serde_json::json!({"title": "Status", "body": "working"}).to_string(),
            )],
            child_plans: Vec::new(),
            valid_until: None,
            request_hop: None,
            request_setup: None,
        },
    )
    .await;
    // The active turn's next provider turn waits on the test.
    prepared.backend.enable_dynamic_followups(prompt);
    configure_engineer_mailbox(&db, &did).await;
    let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
    let agent = gents::Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions::default(),
    )
    .await
    .unwrap();
    let runtime = boot_prepared_accepted_turn(&db, prepared, agent).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while runtime.backend.observed_requests(prompt) < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "active turn never reached its held follow-up"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let first = "queued-user-messages-first";
    let second = "queued-user-messages-second";
    queue_user_message(&db, &did, session_id, first, "how are we looking", asking).await;
    queue_user_message(
        &db,
        &did,
        session_id,
        second,
        "we should move faster",
        asking,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    for request in [first, second] {
        assert_eq!(
            request_state(db.node.as_ref(), request).await.as_deref(),
            Some("pending")
        );
    }
    let before = runtime.backend.observed_completion_requests();

    runtime
        .backend
        .enqueue_response(prompt, StreamResponse::completes(prompt, ["active done"]));
    let active_final =
        wait_for_request_terminal(db.node.as_ref(), asking, Duration::from_secs(60)).await;
    let first_final =
        wait_for_request_terminal(db.node.as_ref(), first, Duration::from_secs(60)).await;
    assert_eq!(active_final, "completed");
    assert_eq!(first_final, "completed");
    assert_eq!(
        request_state(db.node.as_ref(), second).await.as_deref(),
        Some("superseded")
    );
    let superseded = db
        .node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{second}" }} }}) {{ superseded_by_request admission_signature }} }}"#
        ))
        .await;
    let superseded = &superseded.data.unwrap()["AgentRequest"][0];
    assert_eq!(superseded["superseded_by_request"], first);
    assert!(superseded["admission_signature"]
        .as_str()
        .is_some_and(|signature| !signature.is_empty()));

    let bodies = runtime.backend.observed_completion_bodies();
    let folded_turns = bodies[before..]
        .iter()
        .map(ToString::to_string)
        .filter(|body| body.contains("how are we looking"))
        .collect::<Vec<_>>();
    assert_eq!(folded_turns.len(), 1, "one inference answers both messages");
    let body = &folded_turns[0];
    let first_at = body.find("how are we looking").unwrap();
    let second_at = body
        .find("we should move faster")
        .expect("the folded message reaches the provider");
    assert!(first_at < second_at, "folded messages keep queue order");
    runtime.shutdown().await;
}
