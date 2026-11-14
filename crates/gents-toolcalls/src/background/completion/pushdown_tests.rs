use crate::test_support::background_completion::{
    admission_verifier, background_hints, desktop_owned_session,
};
use gents_session::request::lifecycle::queue::persist_background_completion_with_message_waking_with_access;
use gents_session::request::lifecycle::test_support::test_db;
use gents_session::request::lifecycle::ExecutionOrigin;
use gents_store::identity::NodeIdentity;

#[tokio::test]
async fn a_returned_result_is_admitted_into_a_paired_client_session() {
    let db = test_db("paired-client-completion").await;
    let session_id = "desktop-owned-session";
    let (desktop, _, parent, _) = desktop_owned_session(&db, session_id).await;
    let wake = persist_background_completion_with_message_waking_with_access(
        &db.agent_access,
        &db.node,
        &parent,
        "gatekeeper answer",
        "background-completion-notification:agent-new:tool",
        "review notifications",
        background_hints(&parent),
        None,
        gents_protocol::request_lifecycle::RequestHopCause::Return,
    )
    .await
    .unwrap()
    .request
    .expect("completion wake");
    let wake = gents_session::request::admission::load_request_for_admission_test_as(
        &db.node,
        &wake.doc_id,
        db.agent_did(),
    )
    .await
    .unwrap();
    assert_eq!(wake.requester_did.as_deref(), Some(desktop.did()));
    assert_eq!(wake.request_hop, parent.request_hop);
    let verified = admission_verifier(&db)
        .verify_fresh(&wake, db.agent_did())
        .await
        .unwrap();
    let mut lifecycle =
        gents_session::request::lifecycle::RequestLifecycle::new_with_execution_binding(
            db.node.clone(),
            verified.clone(),
            60,
            ExecutionOrigin::Scheduled,
            "backend-test",
        );
    lifecycle
        .bind_managed_agent(db.agent_identity.as_ref())
        .unwrap();
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        gents_session::request::lifecycle::ClaimOutcome::Claimed
    );
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
