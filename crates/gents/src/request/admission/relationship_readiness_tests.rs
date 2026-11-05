use std::sync::Arc;

use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};

use super::{AgentRequestAdmissionVerifier, ClientClaimAdmission, ClientClaimAuthorization};
use crate::request::lifecycle::{
    build_signed_client_request, ClaimOutcome, ExecutionOrigin, RequestIdentity, RequestLifecycle,
    RequestSigner, RequestSpec,
};
use crate::request::watcher::AgentRequest;
use crate::store::config_client::{ConfigAccess, NodePermission};
use crate::store::identity::{KeyIdentity, NodeIdentity};

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

#[tokio::test]
async fn visible_request_is_not_claimed_until_both_relationship_sets_are_published() {
    exercise_readiness(false).await;
}

#[tokio::test]
async fn same_agent_client_waits_for_both_relationship_sets() {
    exercise_readiness(true).await;
}

async fn exercise_readiness(same_agent: bool) {
    let snapshot = crate::lean_vocab_test::lean_contract_snapshot();
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
    let runtime: Arc<dyn NodeIdentity> =
        Arc::new(KeyIdentity::load_or_create(home.path().join("runtime.key"), None).unwrap());
    let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .with_node_acp_enabled()
            .build()
            .await
            .unwrap(),
    );
    crate::store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    let binding = crate::store::identity::provision_agent_identity(runtime.as_ref()).unwrap();
    let selected_identity =
        crate::store::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap();
    let admin = ConfigAccess::local_as(node.clone(), runtime.did().parse().unwrap());
    for did in [user.did(), binding.agent_did.as_str()] {
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DacRelationAdd,
        ] {
            admin
                .grant_node_permission(&did.parse().unwrap(), permission)
                .await
                .unwrap();
        }
    }
    let user_access = ConfigAccess::local_as(node.clone(), user.did().parse().unwrap());
    let response = user_access
        .write(
            "test.readiness.node",
            &format!(
                r#"mutation {{ create_Node(input: {{ node_did: "{}" }}) {{ _docID }} }}"#,
                runtime.did()
            ),
        )
        .await
        .unwrap();
    let node_doc = crate::store::graphql::created_doc_id(&response, "Node").unwrap();
    let response = user_access.write("test.readiness.agent", &format!(
        r#"mutation {{ create_Agent(input: {{ node_did: "{}", agent_did: "{}", creation_nonce: "{}", inference_profile_id: "fixture", enabled: true }}) {{ _docID }} }}"#,
        runtime.did(), binding.agent_did, binding.creation_nonce,
    )).await.unwrap();
    let agent_doc = crate::store::graphql::created_doc_id(&response, "Agent").unwrap();
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
    let author_access = ConfigAccess::local_as(node.clone(), signer.did().parse().unwrap());
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
    let response = author_access
        .write(
            "test.readiness.request",
            &create.graphql_client_publication_mutation().unwrap(),
        )
        .await
        .unwrap();
    let request_doc = crate::store::graphql::created_doc_id(&response, "AgentRequest").unwrap();
    let session_doc = create.session_doc_id.as_deref().unwrap();
    author_access
        .add_resource_relationship(
            "AgentRequest",
            &request_doc,
            "session",
            "sessions",
            session_doc,
        )
        .await
        .unwrap();
    let agent_access = ConfigAccess::local_as(node.clone(), binding.agent_did.parse().unwrap());
    let rows = agent_access
        .execute(&format!(
            "{{ AgentRequest {{ {} }} }}",
            crate::SIGNED_REQUEST_FIELDS
        ))
        .await
        .unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(rows["data"]["AgentRequest"][0].clone()).unwrap();
    let request = AgentRequest::try_from(row).unwrap();
    let (_owner, authority) = crate::runtime::agent::p2p_reconcile::enrollment_authority_channel();
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
            &binding.agent_did,
            runtime.did(),
            request.clone(),
            60,
            ExecutionOrigin::Interactive,
            String::new(),
        );
        assert!(matches!(
            without_admission.claim_with_identity().await.unwrap(),
            ClaimOutcome::Queued
        ));
    }
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        &binding.agent_did,
        runtime.did(),
        request.clone(),
        60,
        ExecutionOrigin::Interactive,
        String::new(),
    );
    lifecycle.set_client_claim_admission(Some(admission.clone()));
    assert!(matches!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Queued
    ));

    let session = crate::store::graphql::escape_graphql_string(session_doc);
    let document = crate::store::graphql::escape_graphql_string(&request_doc);
    author_access.write("test.readiness.session_pending", &format!(
        r#"mutation {{ update_AgentSession(docID: "{session}", input: {{ relationships_pending: true }}) {{ _docID }} update_AgentRequest(docID: "{document}", input: {{ relationships_pending: false }}) {{ _docID }} }}"#,
    )).await.unwrap();
    assert_eq!(
        decision(&agent_access, &admission, &request).await,
        ClientClaimAuthorization::Unavailable
    );
    author_access.write("test.readiness.unknown_request", &format!(
        r#"mutation {{ update_AgentSession(docID: "{session}", input: {{ relationships_pending: false }}) {{ _docID }} update_AgentRequest(docID: "{document}", input: {{ relationships_pending: null }}) {{ _docID }} }}"#,
    )).await.unwrap();
    assert_eq!(
        decision(&agent_access, &admission, &request).await,
        ClientClaimAuthorization::Unavailable
    );
    author_access.write("test.readiness.unknown_session", &format!(
        r#"mutation {{ update_AgentSession(docID: "{session}", input: {{ relationships_pending: null }}) {{ _docID }} update_AgentRequest(docID: "{document}", input: {{ relationships_pending: false }}) {{ _docID }} }}"#,
    )).await.unwrap();
    assert_eq!(
        decision(&agent_access, &admission, &request).await,
        ClientClaimAuthorization::Unavailable
    );
    author_access.write("test.readiness.restore_pending", &format!(
        r#"mutation {{ update_AgentSession(docID: "{session}", input: {{ relationships_pending: false }}) {{ _docID }} update_AgentRequest(docID: "{document}", input: {{ relationships_pending: true }}) {{ _docID }} }}"#,
    )).await.unwrap();
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
    node.shutdown().await.unwrap();
}
