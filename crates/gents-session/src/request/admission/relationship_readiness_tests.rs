use std::sync::Arc;

use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};

use super::{AgentRequestAdmissionVerifier, ClientClaimAdmission, ClientClaimAuthorization};
use crate::request::lifecycle::{
    build_signed_client_request, ClaimOutcome, ExecutionOrigin, RequestIdentity, RequestLifecycle,
    RequestSigner, RequestSpec,
};
use gents_protocol::row::AgentRequest;
use gents_store::config_client::{ConfigAccess, NodePermission};
use gents_store::identity::{KeyIdentity, NodeIdentity};

async fn decision(
    access: &ConfigAccess,
    admission: &ClientClaimAdmission,
    request: &AgentRequest,
) -> ClientClaimAuthorization {
    access
        .transact_readonly("test.claim_relationship_readiness", |txn| {
            Box::pin(async move { admission.authorize_in_txn(txn, request).await })
        })
        .await
        .unwrap()
}

async fn set_pending(
    access: &ConfigAccess,
    operation: &'static str,
    session: &str,
    session_pending: Option<bool>,
    request: &str,
    request_pending: Option<bool>,
) {
    access
        .transact_write(operation, |txn| {
            Box::pin(async move {
                for (collection, schema, doc, pending) in [
                    ("AgentSession", gents_protocol::schemas::AGENT_SESSION, session, session_pending),
                    ("AgentRequest", gents_protocol::schemas::AGENT_REQUEST, request, request_pending),
                ] {
                    txn.execute_client_signed_update(
                        &format!(
                            r#"mutation($input: {collection}MutationInputArg!) {{ update_{collection}(docID: "{}", input: $input) {{ _docID }} }}"#,
                            gents_store::graphql::escape_graphql_string(doc)
                        ),
                        &serde_json::json!({"input": {"relationships_pending": pending}}),
                        schema,
                    )
                    .await?;
                }
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn visible_request_is_not_claimed_until_both_relationship_sets_are_published() {
    exercise_readiness(false).await;
}

#[tokio::test]
async fn same_agent_client_waits_for_both_relationship_sets() {
    exercise_readiness(true).await;
}

async fn exercise_readiness(same_agent: bool) {
    let snapshot = gents_lean_vocab::lean_contract_snapshot();
    let partial = snapshot
        .delegated_publication_cases
        .iter()
        .find(|case| case["name"] == "visible-before-publication-complete")
        .unwrap();
    let published = snapshot
        .delegated_publication_cases
        .iter()
        .find(|case| case["name"] == "published")
        .unwrap();
    assert_eq!(partial["expected_target_read"], true);
    let home = tempfile::tempdir().unwrap();
    let db = gents_inference::test_support::test_db(
        "exercise-readiness-relationship-sets",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = db.node.clone();
    let runtime: Arc<dyn NodeIdentity> = Arc::new(
        gents_store::test_support::held_runtime_identity(db.node_identity.did()),
    );
    let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
    // The user's Node and Agent are client-signed over HTTP; embedded access
    // refuses non-runtime writes without a managed Agent signer by design.
    let binding = gents_store::identity::provision_agent_identity(runtime.as_ref()).unwrap();
    let selected_identity =
        gents_store::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap();
    let admin = ConfigAccess::local_as(node.clone(), runtime.did().parse().unwrap());
    gents_store::identity::GrantSet::OperatorUser
        .grant(&admin, &user.did().parse().unwrap())
        .await
        .unwrap();
    for permission in [
        NodePermission::DocumentRead,
        NodePermission::DocumentUpdate,
        NodePermission::DacRelationAdd,
    ] {
        admin
            .grant_node_permission(&binding.agent_did.parse().unwrap(), permission)
            .await
            .unwrap();
    }
    let user_access = db.client_access(&user);
    let (node_doc, agent_doc) = user_access
        .transact_write("test.readiness.owner", |txn| {
            let (runtime_did, binding) = (runtime.did(), &binding);
            Box::pin(async move {
                let created = txn
                    .execute_client_signed_create(
                        "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
                        &serde_json::json!({"input": {"node_did": runtime_did}}),
                        gents_protocol::schemas::NODE,
                    )
                    .await?;
                let node_doc = gents_store::graphql::created_doc_id(&created, "Node")?;
                let created = txn
                    .execute_client_signed_create(
                        "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
                        &serde_json::json!({"input": {
                            "node_did": runtime_did,
                            "agent_did": binding.agent_did,
                            "creation_nonce": binding.creation_nonce,
                            "inference_profile_id": "fixture",
                            "enabled": true,
                        }}),
                        gents_protocol::schemas::AGENT,
                    )
                    .await?;
                let agent_doc = gents_store::graphql::created_doc_id(&created, "Agent")?;
                Ok::<_, anyhow::Error>((node_doc, agent_doc))
            })
        })
        .await
        .unwrap();
    user_access
        .add_actor_relationship(
            "Node",
            &node_doc,
            "runtime_reader",
            &runtime.did().parse().unwrap(),
        )
        .await
        .unwrap();
    user_access
        .add_resource_relationship("Agent", &agent_doc, "parent", "nodes", &node_doc)
        .await
        .unwrap();
    user_access
        .add_actor_relationship(
            "Agent",
            &agent_doc,
            "executor",
            &binding.agent_did.parse().unwrap(),
        )
        .await
        .unwrap();
    let signer: &dyn NodeIdentity = if same_agent {
        selected_identity.as_ref()
    } else {
        &user
    };
    let author_access = if same_agent {
        ConfigAccess::local_managed_agent(
            node.clone(),
            signer.did().parse().unwrap(),
            selected_identity.as_ref(),
        )
        .unwrap()
    } else {
        db.client_access(&user)
    };
    let create = build_signed_client_request(
        &author_access,
        RequestSpec::new(
            RequestPurpose::Normal,
            RequestIdentity {
                request_id: "readiness-request".into(),
                node_did: runtime.did().into(),
                requester_did: Some(signer.did().into()),
                agent_did: binding.agent_did.clone(),
                session_id: "readiness-session".into(),
                content: "work".into(),
                execution_origin: ExecutionOrigin::Interactive,
                created_at: "2026-11-04T00:00:00Z".into(),
            },
            AgentRequestAdmissionRecord::client(signer.did()),
        ),
        RequestSigner::Identity(signer),
    )
    .await
    .unwrap();
    let mut input = gents_store::config_client::client_request_input(&create).unwrap();
    input["relationships_pending"] = true.into();
    let request_doc = author_access
        .transact_write("test.readiness.request", |txn| {
            let input = &input;
            Box::pin(async move {
                let response = txn
                    .execute_client_signed_create(
                        "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                        &serde_json::json!({"input": input}),
                        gents_protocol::schemas::AGENT_REQUEST,
                    )
                    .await?;
                Ok::<_, anyhow::Error>(gents_store::graphql::created_doc_id(
                    &response,
                    "AgentRequest",
                )?)
            })
        })
        .await
        .unwrap();
    let session_doc = create.session_doc_id.as_deref().unwrap();
    gents_store::test_support::relationships::resource_in_txn(
        &author_access,
        "AgentRequest",
        &request_doc,
        "session",
        "sessions",
        session_doc,
        false,
    )
    .await
    .unwrap();
    let agent_access = ConfigAccess::local_as(node.clone(), binding.agent_did.parse().unwrap());
    let rows = agent_access
        .execute(&format!(
            "{{ AgentRequest {{ {} }} }}",
            gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
        ))
        .await
        .unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(rows["data"]["AgentRequest"][0].clone()).unwrap();
    let request = AgentRequest::try_from(row).unwrap();
    let (_owner, authority) = gents_p2p::p2p_reconcile::enrollment_authority_channel();
    let verifier = AgentRequestAdmissionVerifier::new(node.clone(), runtime.clone(), authority);
    let (request, admission) = verifier
        .verify_fresh_for_claim(&request, &binding.agent_did)
        .await
        .unwrap();
    let admission = admission.expect("client claim readiness admission");
    assert_eq!(partial["expected_claim"], "unavailable");
    assert_eq!(
        decision(&agent_access, &admission, &request).await,
        ClientClaimAuthorization::Unavailable
    );
    if same_agent {
        let mut without_admission = RequestLifecycle::new_with_execution_binding(
            node.clone(),
            request.clone(),
            60,
            ExecutionOrigin::Interactive,
            String::new(),
        );
        without_admission
            .bind_managed_agent(selected_identity.as_ref())
            .unwrap();
        assert!(matches!(
            without_admission.claim_with_identity().await.unwrap(),
            ClaimOutcome::Queued
        ));
    }
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        request.clone(),
        60,
        ExecutionOrigin::Interactive,
        String::new(),
    );
    lifecycle
        .bind_managed_agent(selected_identity.as_ref())
        .unwrap();
    lifecycle.set_client_claim_admission(Some(admission.clone()));
    let unadmitted_before = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        request.clone(),
        60,
        ExecutionOrigin::Interactive,
        String::new(),
    )
    .claimed_client_provenance();
    let admitted_before = lifecycle.claimed_client_provenance();
    assert!(matches!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Queued
    ));

    set_pending(
        &author_access,
        "test.readiness.session_pending",
        session_doc,
        Some(true),
        &request_doc,
        Some(false),
    )
    .await;
    assert_eq!(
        decision(&agent_access, &admission, &request).await,
        ClientClaimAuthorization::Unavailable
    );
    set_pending(
        &author_access,
        "test.readiness.unknown_request",
        session_doc,
        Some(false),
        &request_doc,
        None,
    )
    .await;
    assert_eq!(
        decision(&agent_access, &admission, &request).await,
        ClientClaimAuthorization::Unavailable
    );
    set_pending(
        &author_access,
        "test.readiness.unknown_session",
        session_doc,
        None,
        &request_doc,
        Some(false),
    )
    .await;
    assert_eq!(
        decision(&agent_access, &admission, &request).await,
        ClientClaimAuthorization::Unavailable
    );
    set_pending(
        &author_access,
        "test.readiness.restore_pending",
        session_doc,
        Some(false),
        &request_doc,
        Some(true),
    )
    .await;
    crate::request::lifecycle::complete_client_request_relationships(&author_access, &request_doc)
        .await
        .unwrap();
    assert_eq!(published["expected_claim"], "claimed");
    assert_eq!(
        decision(&agent_access, &admission, &request).await,
        ClientClaimAuthorization::Allowed
    );
    assert!(matches!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    ));
    let admitted_after = lifecycle.claimed_client_provenance();
    let label = |actual: &str, fixture: &str, modeled: &str| {
        if actual == fixture {
            modeled.to_owned()
        } else {
            actual.to_owned()
        }
    };
    for case in gents_lean_vocab::families::mailbox_access_claimed_recipient_served::cases() {
        let observed = match (case.input.present, case.input.committed) {
            (false, false) => &unadmitted_before,
            (true, false) => &admitted_before,
            (true, true) => &admitted_after,
            (false, true) => panic!("the readiness fixture commits only admitted claims"),
        };
        case.check(
            gents_lean_vocab::families::mailbox_access_claimed_recipient_served::RecipientOutput {
                signer: observed
                    .as_ref()
                    .map(|retained| label(&retained.signer_did, signer.did(), "delegate")),
                request_doc: observed
                    .as_ref()
                    .map(|retained| label(&retained.request_doc_id, &request_doc, "request")),
                session_doc: observed
                    .as_ref()
                    .map(|retained| label(&retained.session_doc_id, session_doc, "session")),
                agent_doc: observed
                    .as_ref()
                    .map(|retained| label(&retained.agent_doc_id, &agent_doc, "agent")),
            },
        );
    }
    node.shutdown().await.unwrap();
}
