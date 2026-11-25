use gents_inference::admission::recovery_fixtures::{
    lookup_parent_request, recovery_outcome, InferenceRecoveryOutcome, StaleInferenceCallRow,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_store::graphql::escape_graphql_string;

#[tokio::test]
async fn parent_recovery_lookup_uses_agent_native_authority() {
    use gents_store::config_client::NodePermission;

    let signed_home = gents_documents::test_support::SignedHome::start(
        "inference-recovery-native-authority",
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let binding = gents_inference::test_support::default_agent(&signed_home).await;
    let claimed =
        gents_session::test_support::ClaimedRequest::client_signed(&signed_home, &binding)
            .ids("private-recovery", "private-recovery-session")
            .content("recover")
            .created_at("2026-09-01T00:00:00Z")
            .drive(gents_session::test_support::Stage::Admitted)
            .await;
    let node = signed_home.node.clone();
    let runtime = signed_home.node_identity.clone();
    let admin = signed_home.node_access();
    let agent = claimed.agent;
    let request_doc = claimed.doc_id;
    let agent_access = claimed.agent_access;
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
