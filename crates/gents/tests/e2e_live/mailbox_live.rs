use std::sync::Arc;
use std::time::Duration;

use gents::session::mailbox::{list_mailbox_items_on, MailboxStatus};
use gents::NodeIdentity;

use crate::support::accepted_turn::boot_prepared_accepted_turn;
use crate::support::live_inference::live_target;
use crate::support::signed_mailbox::{prepare_signed_mailbox_turn, wait_for_request_terminal_on};
use crate::support::test_db;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: set GENTS_EVAL_TARGET and pass --ignored"]
async fn real_model_files_a_stamped_mailbox_item_through_granted_surface() {
    let target = live_target();
    let db = test_db("mailbox-real-inference").await;
    let request_id = "request-mailbox-live";
    let session_id = "session-mailbox-live";
    let prompt = "Call file_mailbox_item exactly once with title='Mailbox live verified', then answer MAILBOX_FILED. The tool owns notification identity and handling.";
    let (prepared, turn) = prepare_signed_mailbox_turn(
        &db,
        target.backend_id(),
        target.model(),
        request_id,
        session_id,
        prompt,
        Vec::new(),
        Vec::new(),
        Some(&target),
    )
    .await
    .unwrap();
    let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
    let agent = gents::Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let runtime = boot_prepared_accepted_turn(&db, prepared, agent).await;
    assert_eq!(
        wait_for_request_terminal_on(&turn.client, request_id, Duration::from_secs(120))
            .await
            .unwrap(),
        "completed"
    );

    let items = list_mailbox_items_on(&turn.client, turn.user.did(), Some(MailboxStatus::Open))
        .await
        .unwrap();
    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert_eq!(item.title, "Mailbox live verified");
    assert_eq!(item.requester_did, turn.user.did());
    assert_eq!(item.node_did, turn.node_did);
    assert_eq!(item.target_agent_did, turn.agent.did());
    assert_eq!(item.source_kind, "agent");
    assert_eq!(
        item.source_id,
        gents::session::mailbox::NotificationIdentity::Event
            .source_id(
                &turn.node_did,
                turn.user.did(),
                turn.agent.did(),
                request_id
            )
            .unwrap()
    );
    assert_eq!(item.request_id.as_deref(), Some(request_id));
    assert!(item.cause_doc_id.is_none());
    let query = format!(
        r#"{{AgentToolCall(filter: {{request_id: {{_eq: "{}"}}, tool_name: {{_eq: "file_mailbox_item"}}}}) {{lifecycle_state}}}}"#,
        gents::store::graphql::escape_graphql_string(request_id)
    );
    let tool_calls = turn.agent_access.execute(&query).await.unwrap();
    assert_eq!(
        tool_calls["data"]["AgentToolCall"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    runtime.shutdown().await;
}
