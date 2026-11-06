use std::sync::Arc;
use std::time::Duration;

use gents::NodeIdentity;

use crate::support::accepted_turn::boot_prepared_accepted_turn;
use crate::support::signed_mailbox::{
    prepare_signed_mailbox_turn, request_state_on, wait_for_request_terminal_on, SignedMailboxTurn,
};
use crate::support::streaming_backend::{StreamChunk, StreamResponse};
use crate::support::test_db;

async fn queue_user_message(
    turn: &SignedMailboxTurn,
    session_id: &str,
    request_id: &str,
    content: &str,
    active: &str,
) {
    let mut create = turn.request(request_id, session_id, content);
    create.input.queue = Some(gents_protocol::request_input::RequestQueue {
        delivery: Default::default(),
        position: None,
        source: gents_protocol::request_input::QueueSource::User,
        policy: gents_protocol::request_input::QueuePolicy::Append,
        key: None,
        queued_after_request_id: Some(active.into()),
        interrupted_request_id: None,
        background_completion_wake_version: None,
    });
    turn.submit(create).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn messages_queued_behind_a_busy_turn_are_answered_by_one_inference() {
    let db = test_db("queued-user-messages").await;
    let session_id = "queued-user-messages-session";
    let asking = "queued-user-messages-active";
    let prompt = "queued-user-messages-prompt";
    let (prepared, turn) = prepare_signed_mailbox_turn(
        &db,
        "queued-user-messages-backend",
        "queued-user-messages-model",
        asking,
        session_id,
        prompt,
        vec![StreamChunk::tool_call(
            "queued-user-messages-call",
            gents::session::mailbox::FILE_MAILBOX_ITEM_TOOL_NAME,
            serde_json::json!({"title":"Status", "summary":"working"}).to_string(),
        )],
        Vec::new(),
        None,
    )
    .await
    .unwrap();
    prepared.backend.enable_dynamic_followups(prompt);
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
    queue_user_message(&turn, session_id, first, "how are we looking", asking).await;
    queue_user_message(&turn, session_id, second, "we should move faster", asking).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    for request in [first, second] {
        assert_eq!(
            request_state_on(&turn.client, request)
                .await
                .unwrap()
                .as_deref(),
            Some("pending")
        );
    }
    let before = runtime.backend.observed_completion_requests();

    runtime
        .backend
        .enqueue_response(prompt, StreamResponse::completes(prompt, ["active done"]));
    let active_final = wait_for_request_terminal_on(&turn.client, asking, Duration::from_secs(60))
        .await
        .unwrap();
    let first_final = wait_for_request_terminal_on(&turn.client, first, Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(active_final, "completed");
    assert_eq!(first_final, "completed");
    assert_eq!(
        request_state_on(&turn.client, second)
            .await
            .unwrap()
            .as_deref(),
        Some("superseded")
    );
    let escaped = gents::store::graphql::escape_graphql_string(second);
    let superseded = turn.client.execute(&format!(r#"{{AgentRequest(filter: {{request_id: {{_eq: "{escaped}"}}}}) {{superseded_by_request admission_signature}}}}"#)).await.unwrap();
    let superseded = &superseded["data"]["AgentRequest"][0];
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
