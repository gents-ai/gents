use gents_inference::admission::recovery_fixtures::{
    lookup_parent_request, recovery_outcome, InferenceRecoveryOutcome, StaleInferenceCallRow,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_store::graphql::escape_graphql_string;

#[tokio::test]
async fn parent_recovery_lookup_uses_agent_native_authority() {
    use gents_protocol::request_admission::{
        AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
    };
    use gents_store::config_client::{ConfigAccess, NodePermission};
    use gents_store::identity::NodeIdentity;

    let (db, user, owner, binding) =
        gents_inference::test_support::signed_test_node("inference-recovery-native-authority").await;
    let node = db.node.clone();
    let runtime = db.node_identity.clone();
    let agent = gents_store::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap();
    let admin = ConfigAccess::local_as(node.clone(), runtime.did().parse().unwrap());
    let create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "private-recovery",
        runtime.did(),
        user.did(),
        agent.did(),
        "private-recovery-session",
        "recover",
        "interactive",
        "2026-09-01T00:00:00Z",
        AgentRequestAdmissionRecord::client(user.did()),
    );
    let lifecycle =
        crate::call::lifecycle::admission_fixture::pending_client_fixture_request(
            &node,
            &owner,
            runtime.clone(),
            &user,
            agent.as_ref(),
            create,
        )
        .await
        .unwrap();
    let request_doc = lifecycle.request().doc_id.clone();
    let agent_access = ConfigAccess::local_managed_agent(
        node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    agent_access.write("test.inference_recovery.processing", &format!(
        r#"mutation {{ update_AgentRequest(docID: "{}", input: {{ lifecycle_state: "{}" }}) {{ _docID }} }}"#,
        escape_graphql_string(&request_doc), RequestLifecycleState::Processing.as_str(),
    )).await.unwrap();
    let row = StaleInferenceCallRow {
        doc_id: "call-doc".into(),
        call_id: "call".into(),
        request_id: "private-recovery".into(),
        request_doc_id: request_doc,
        agent_did: agent.did().into(),
        call_state: "running".into(),
    };
    let found = lookup_parent_request(&agent_access, runtime.did(), &row)
        .await
        .unwrap_or_else(|error| panic!("authorized parent lookup failed: {error:#}"));
    let parent = found;
    assert_eq!(parent.request_id, "private-recovery");
    assert!(recovery_outcome(&row, &parent).is_none());
    agent_access.write("test.inference_recovery.terminal", &format!(
        r#"mutation {{ update_AgentRequest(docID: "{}", input: {{ lifecycle_state: "{}" }}) {{ _docID }} }}"#,
        row.request_doc_id, RequestLifecycleState::Failed.as_str())).await.unwrap();
    let terminal = lookup_parent_request(&agent_access, runtime.did(), &row)
        .await
        .unwrap();
    assert!(matches!(
        recovery_outcome(&row, &terminal),
        Some(InferenceRecoveryOutcome::Failed)
    ));
    admin
        .revoke_node_permission(&agent.did().parse().unwrap(), NodePermission::DocumentRead)
        .await
        .unwrap();
    // Node DocumentRead is a node-level permission that does not confer document
    // visibility (see `grant_node_permission`). The agent's visibility of its own
    // parent AgentRequest is decided by that document's ACP policy, so revoking
    // the node permission leaves the parent visible and the lookup still resolves.
    let after_node_revoke = lookup_parent_request(&agent_access, runtime.did(), &row)
        .await
        .unwrap();
    assert_eq!(after_node_revoke.request_id, "private-recovery");
    node.shutdown().await.unwrap();
}
