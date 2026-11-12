use gents_protocol::row::AgentRequest;
use gents_session::request::lifecycle::queue::{QueuePolicy, QueueSource, RequestQueue};
use gents_session::request::lifecycle::test_support::TestDb;
use gents_session::request::lifecycle::{ExecutionOrigin, RequestTerminalOutcome};

pub fn background_hints(parent: &AgentRequest) -> RequestQueue {
    RequestQueue {
        delivery: Default::default(),
        position: None,
        source: QueueSource::BackgroundCompletion,
        policy: QueuePolicy::Coalesce,
        key: Some(format!("background_completion:{}", parent.session_id)),
        queued_after_request_id: Some(parent.request_id.clone()),
        interrupted_request_id: None,
        background_completion_wake_version: None,
    }
}

pub async fn desktop_owned_session(
    db: &TestDb,
    session_id: &str,
) -> (
    gents_store::identity::KeyIdentity,
    gents_protocol::request_admission::AgentRequestCreate,
    AgentRequest,
    gents_session::request::streaming::DefraStreamWriter,
) {
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};
    let desktop = gents_store::identity::KeyIdentity::load_or_create(
        db._tempdir.path().join("desktop.key"),
        None,
    )
    .unwrap();
    let mut create = AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        "desktop-parent",
        db.node_did(),
        desktop.did(),
        db.agent_did(),
        session_id,
        "run work",
        "interactive",
        "2030-01-01T00:00:00Z",
        AgentRequestAdmissionRecord::enrollment(
            desktop.did(),
            "enrollment",
            "digest",
            db.node_did(),
            1,
            "2099-01-01T00:00:00Z",
        ),
    );
    gents_session::request::admission::sign_agent_request_create(&desktop, &mut create)
        .await
        .unwrap();
    let desktop_mutation = create.graphql_mutation().unwrap();
    db.agent_access
        .transact_write("test.desktop_parent", |txn| {
            let desktop_mutation = &desktop_mutation;
            Box::pin(async move { Ok::<_, anyhow::Error>(txn.execute(desktop_mutation).await?) })
        })
        .await
        .unwrap();
    let parent = gents_session::request::binding::load_agent_request_with_access(
        &db.agent_access,
        "desktop-parent",
    )
    .await
    .unwrap()
    .unwrap();
    let writer = gents_session::request::streaming::DefraStreamWriter::new_managed(
        db.node.clone(),
        db.agent_identity.as_ref(),
        std::time::Duration::ZERO,
    )
    .unwrap();
    let mut parent_lifecycle =
        gents_session::request::lifecycle::RequestLifecycle::new_with_execution_binding(
            db.node.clone(),
            parent.clone(),
            60,
            ExecutionOrigin::Interactive,
            "backend-test",
        );
    parent_lifecycle
        .bind_managed_agent(db.agent_identity.as_ref())
        .unwrap();
    assert_eq!(
        parent_lifecycle.claim_with_identity().await.unwrap(),
        gents_session::request::lifecycle::ClaimOutcome::Claimed
    );
    parent_lifecycle
        .begin_owned_execution(&writer)
        .await
        .unwrap();
    parent_lifecycle
        .terminalize_owned(
            RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::NoMessage,
            None,
        )
        .await
        .unwrap();
    (desktop, create, parent, writer)
}

pub fn admission_verifier(
    db: &TestDb,
) -> gents_session::request::admission::AgentRequestAdmissionVerifier {
    let (_owner, authority) = gents_p2p::p2p_reconcile::enrollment_authority_channel();
    gents_session::request::admission::AgentRequestAdmissionVerifier::new(
        db.node.clone(),
        db.identity.clone(),
        authority,
    )
}
