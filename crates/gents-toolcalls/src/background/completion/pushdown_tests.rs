use crate::test_support::background_completion::{background_hints, desktop_owned_session};
use gents_session::request::lifecycle::queue::persist_background_completion_with_message_waking_with_access;
use gents_session::request::lifecycle::ExecutionOrigin;
use gents_session::test_support::AgentHome;
use gents_store::identity::NodeIdentity;

#[tokio::test]
async fn a_returned_result_is_admitted_into_a_paired_client_session() {
    let db = AgentHome::start("paired-client-completion").await;
    let session_id = "desktop-owned-session";
    let (desktop, _, parent, _) = desktop_owned_session(&db, session_id).await;
    let wake = persist_background_completion_with_message_waking_with_access(
        &db.agent_access,
        &db.node,
        &parent,
        gents_session::request::lifecycle::queue::BackgroundCompletionNotification {
            content: "gatekeeper answer",
            message_key: "background-completion-notification:agent-new:tool",
            wake_content: "review notifications",
            queue: background_hints(&parent),
            existing_notification_doc_id: None,
            wake: gents_protocol::request_lifecycle::RequestHopCause::Return,
        },
    )
    .await
    .unwrap()
    .request
    .expect("completion wake");
    let wake =
        gents_session::test_support::ClaimedRequest::existing(&db, &db.binding, &wake.doc_id)
            .origin(ExecutionOrigin::Scheduled)
            .backend("backend-test")
            .drive(gents_session::test_support::Stage::Claimed)
            .await
            .lifecycle;
    let verified = wake.request();
    assert_eq!(verified.requester_did.as_deref(), Some(desktop.did()));
    assert_eq!(verified.request_hop, parent.request_hop);
    crate::call::hook::DefraSessionHook::resume_with_actor_policy(
        db.node.clone(),
        session_id,
        db.node_did(),
        verified.requester_did.as_deref(),
        ::identity::Did::new(db.agent_did()).unwrap(),
        crate::call::hook::FailurePolicy::FailClosed,
    )
    .await
    .expect("the completion resumes the desktop-owned session");
    let sessions = db
        .agent_access
        .execute(&format!(
            "{{ AgentSession {{ {} }} }}",
            gents_protocol::session::AGENT_SESSION_FIELDS
        ))
        .await
        .unwrap()["data"]
        .clone();
    assert_eq!(sessions["AgentSession"].as_array().unwrap().len(), 1);
    assert_eq!(sessions["AgentSession"][0]["requester_did"], desktop.did());
}
