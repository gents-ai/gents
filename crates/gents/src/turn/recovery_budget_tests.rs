use std::sync::Arc;

use defra_node::Node as EmbeddedNode;
use gents_inference::admission::recovery_fixtures::{
    load_stale_inference_calls, persist_existing_call_terminal, recover_inference_call_row,
    InferenceCallRecord, InferenceRecoveryOutcome, PendingCallMetadata, ProviderCallUsage,
    StaleInferenceCallRow,
};
use gents_inference::admission::CallKind;
use gents_store::graphql::escape_graphql_string;
use serde_json::Value;

async fn grant_test_agent(
    db: &crate::support::TestDb,
    signer: &gents_store::identity::ManagedAgentSigner,
) {
    use gents_store::config_client::NodePermission;
    use gents_store::identity::NodeIdentity as _;
    let admin = gents_store::config_client::ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(db.node_identity.did()).unwrap(),
    );
    let actor = identity::Did::new(signer.did()).unwrap();
    for permission in [
        NodePermission::CollectionGet,
        NodePermission::DocumentRead,
        NodePermission::DocumentUpdate,
        NodePermission::DocumentDelete,
        NodePermission::DacRelationAdd,
        NodePermission::DacRelationDelete,
    ] {
        admin
            .grant_node_permission(&actor, permission)
            .await
            .unwrap();
    }
}

fn agent_access(
    node: &Arc<EmbeddedNode>,
    signer: &gents_store::identity::ManagedAgentSigner,
) -> gents_store::config_client::ConfigAccess {
    gents_store::config_client::ConfigAccess::local_with_agent_signer(
        node.clone(),
        identity::Did::new(signer.did()).unwrap(),
        signer.clone(),
    )
    .unwrap()
}

async fn seed_call(
    node: &Arc<EmbeddedNode>,
    signer: &gents_store::identity::ManagedAgentSigner,
    initial_state: &str,
    request_doc_id: &str,
) -> StaleInferenceCallRow {
    let agent_did = escape_graphql_string(signer.did());
    let state = escape_graphql_string(initial_state);
    let owner = escape_graphql_string(signer.node_did());
    let request_doc = escape_graphql_string(request_doc_id);
    let access = agent_access(node, signer);
    access
        .write(
            "test.recovery_call",
            &format!(
                r#"mutation {{ add_InferenceCall(input: {{
            call_id: "recovery-cas-call", runtime_instance_id: "recovery-cas-runtime",
            request_id: "recovery-cas-request", request_doc_id: "{request_doc}", call_seq: 1,
            backend_id: "recovery-cas-backend", agent_did: "{agent_did}", node_did: "{owner}",
            call_kind: "inference", attempt: 1, call_state: "{state}",
            queued_at: "2026-09-01T00:00:00Z", priority: 0,
            queue_depth_at_enqueue: 0, controller_generation: 0,
            backend_config_fingerprint: "test"
        }}) {{ _docID }} }}"#,
            ),
        )
        .await
        .unwrap();
    let mut snapshots = load_stale_inference_calls(&access, signer.node_did(), signer.did())
        .await
        .unwrap();
    assert_eq!(snapshots.len(), 1);
    snapshots.remove(0)
}

async fn stored_call(
    node: &Arc<EmbeddedNode>,
    signer: &gents_store::identity::ManagedAgentSigner,
) -> Value {
    let value = agent_access(node, signer).execute(
        r#"{ InferenceCall { call_state failure_reason ended_at prompt_tokens completion_tokens } }"#,
    ).await.unwrap();
    let response: query::QueryResponse = serde_json::from_value(value).unwrap();
    gents_store::graphql::rows::<Value>(&response, "InferenceCall")
        .unwrap()
        .remove(0)
}

fn pending_call(
    call_id: &str,
    signer: &gents_store::identity::ManagedAgentSigner,
) -> PendingCallMetadata {
    PendingCallMetadata {
        agent_signer: Some(signer.clone()),
        session_id: "session-test".to_string(),
        requester_did: None,
        request_purpose: gents_protocol::request_admission::RequestPurpose::Normal,
        model_name: None,
        profile_id: None,
        call_id: call_id.into(),
        runtime_instance_id: "recovery-cas-runtime".into(),
        request_id: "recovery-cas-request".into(),
        request_doc_id: "recovery-cas-request-doc".into(),
        call_seq: 1,
        backend_id: "recovery-cas-backend".into(),
        agent_did: signer.did().into(),
        node_did: signer.node_did().into(),
        call_kind: CallKind::Inference,
        attempt: 1,
    }
}

/// Lean namespace InferenceCall.Persistence: terminal_winner_preserves_outcome_and_stamp and
/// late_usage_preserves_terminal_projection / late_usage_is_recorded.
#[tokio::test]
async fn recovery_winner_preserves_terminal_stamp_and_rehydrates_late_usage() {
    use gents_session::request::lifecycle::{RequestIdentity, RequestSigner, RequestSpec};
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
    use gents_store::identity::NodeIdentity;

    let (db, user, access, binding) =
        crate::test_support::root::signed_test_node("inference-recovery-parent").await;
    let node = db.node.clone();
    let identity = db.node_identity.clone();
    let agent = gents_store::identity::resolve_agent_identity(identity.as_ref(), &binding).unwrap();
    let signer = agent.managed_agent_signer().unwrap();
    grant_test_agent(&db, &signer).await;
    let request_id = "recovery-cas-request";
    let create = gents_session::request::lifecycle::build_signed_client_request(
        &access,
        RequestSpec::new(
            RequestPurpose::Normal,
            RequestIdentity {
                request_id: request_id.into(),
                node_did: identity.did().to_owned(),
                requester_did: Some(user.did().to_owned()),
                agent_did: agent.did().to_owned(),
                session_id: "recovery-cas-session".into(),
                content: "Resume the request".into(),
                execution_origin: gents_loop::execution_origin::ExecutionOrigin::Interactive,
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            },
            AgentRequestAdmissionRecord::client(user.did()),
        ),
        RequestSigner::Identity(&user),
    )
    .await
    .unwrap();
    gents_session::request::lifecycle::submit_client_request(&access, &create)
        .await
        .unwrap();
    let response = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}) {{_docID}} }}"#,
            escape_graphql_string(request_id),
        ))
        .await
        .unwrap();
    let parent_doc_id = response["data"]["AgentRequest"][0]["_docID"]
        .as_str()
        .unwrap();
    let parent = gents_session::request::admission::load_request_for_admission_test_as(
        &node,
        parent_doc_id,
        agent.did(),
    )
    .await
    .unwrap();
    // A client request is claimable only with the native claim admission its
    // verifier issues for this Agent; without it the claim is withheld as Queued.
    let (_owner, authority) = gents_p2p::p2p_reconcile::enrollment_authority_channel();
    let verifier = gents_session::request::admission::AgentRequestAdmissionVerifier::new(
        node.clone(),
        identity.clone(),
        authority,
    );
    let (parent, claim_admission) = verifier
        .verify_fresh_for_claim_with_identity(&parent, agent.as_ref())
        .await
        .unwrap();
    assert!(
        claim_admission.is_some(),
        "fixture client request lacks native claim admission"
    );
    let mut lifecycle = gents_session::request::lifecycle::RequestLifecycle::new_with_execution_binding(
        node.clone(),
        parent,
        120,
        gents_loop::execution_origin::ExecutionOrigin::Interactive,
        "recovery-cas-backend",
    );
    lifecycle.bind_managed_agent(agent.as_ref()).unwrap();
    lifecycle.set_client_claim_admission(claim_admission);
    lifecycle.set_configured_max_total_tokens(Some(100));
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        gents_session::request::lifecycle::ClaimOutcome::Claimed
    );
    let request = lifecycle.request().clone();
    let snapshot = seed_call(&node, &signer, "running", &request.doc_id).await;
    assert!(recover_inference_call_row(
        &agent_access(&node, &signer),
        &snapshot,
        InferenceRecoveryOutcome::Failed
    )
    .await
    .unwrap());
    let terminal = stored_call(&node, &signer).await;
    let mut metadata = pending_call("recovery-cas-call", &signer);
    metadata.request_doc_id = request.doc_id.clone();
    metadata.node_did = request.node_did.clone();
    let call = InferenceCallRecord::without_controller(metadata);
    let usage = ProviderCallUsage(gents_protocol::usage::TokenUsage {
        input_tokens: Some(17),
        output_tokens: Some(9),
        cached_input_tokens: Some(3),
        final_response: true,
        ..Default::default()
    });
    persist_existing_call_terminal(
        node.clone(),
        &call,
        &snapshot.doc_id,
        "completed",
        None,
        Some(usage),
        gents_loop::rig_compat::CachedInputTokensObservation::NotAvailable,
    )
    .await
    .unwrap();
    let stored = stored_call(&node, &signer).await;
    for field in ["call_state", "failure_reason", "ended_at"] {
        assert_eq!(
            stored[field], terminal[field],
            "late completion changed terminal {field}"
        );
    }
    assert_eq!(stored["prompt_tokens"], 17);
    assert_eq!(stored["completion_tokens"], 9);
    let budget = super::completion_factory::aggregate_token_budget_for_request(
        &agent_access(&node, &signer),
        &request,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        budget.snapshot().unwrap().remaining(),
        74,
        "recovery must not discard observed usage on restart"
    );
    persist_existing_call_terminal(
        node.clone(),
        &call,
        &snapshot.doc_id,
        "completed",
        None,
        Some(usage),
        gents_loop::rig_compat::CachedInputTokensObservation::NotAvailable,
    )
    .await
    .unwrap();
    assert_eq!(stored_call(&node, &signer).await, stored);
    let budget = super::completion_factory::aggregate_token_budget_for_request(
        &agent_access(&node, &signer),
        &request,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(budget.snapshot().unwrap().remaining(), 74);
}

