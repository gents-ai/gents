use super::prepare_goal_continuation;

#[tokio::test]
async fn pin_goal_continuation_preparation() {
    let tempdir = tempfile::tempdir().unwrap();
    let _identity =
        gents_session::request::lifecycle::test_support::pin_fixed_signing_identity(tempdir.path());

    let mut parent = gents_session::request::lifecycle::test_support::pin_parent_request();
    parent.workspace_id = Some("ws-goal-1".to_string());
    parent.workspace_owner_node_did = Some("did:key:workspace-owner".to_string());
    parent.workspace_authority = Some("readWrite".to_string());
    parent.workspace_seal_hash = Some("seal-goal-1".to_string());

    let goal_id = "goal-1";
    let continuation_sequence: i64 = 3;
    let content = "continue the goal";
    let agent_did = parent.agent_did.clone();

    let mut create = prepare_goal_continuation(
        &parent,
        agent_did,
        goal_id,
        content,
        continuation_sequence,
        false,
        "2030-01-01T00:00:00Z",
        parent.request_hop,
    )
    .expect("prepare goal continuation");
    gents_session::request::lifecycle::materialize::sign_request(
        &mut create,
        gents_session::request::lifecycle::materialize::RequestSigner::RegisteredTarget,
    )
    .await
    .expect("sign goal continuation request");

    let fields = create.graphql_input_fields().expect("graphql_input_fields");
    assert_eq!(
        gents_session::request::lifecycle::test_support::normalize_pin_fields(&fields),
        "request_id: \"goal-cont-00000000000000000003-355ac0cefea9f3d0afab06ffb90fd1ec\", purpose: \"normal\", node_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", requester_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", agent_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", session_id: \"sess-pin-parent\", retry_root_request: \"goal-cont-00000000000000000003-355ac0cefea9f3d0afab06ffb90fd1ec\", retry_key: \"goal-continuation:355ac0cefea9f3d0afab06ffb90fd1ec\", content: \"continue the goal\", input: { goal_continuation: { sequence: 3, wrapup: false }, queue: { key: \"goal:355ac0cefea9f3d0afab06ffb90fd1ec\", policy: \"coalesce\", queued_after_request_id: \"pin-parent-request\", source: \"goal\" } }, execution_origin: \"scheduled\", caused_by_trigger_id: \"goal-1\", caused_by_trigger_kind: \"goal\", caused_by_correlation: \"corr-parent\", caused_by_trigger_context: \"{\\\"a\\\":\\\"b\\\"}\", created_at: \"2030-01-01T00:00:00Z\", retry_count: 0, max_retries: 3, request_hop: 2, caused_by_parent_request_id: \"pin-parent-request\", caused_by_parent_request_doc_id: \"pin-parent-doc\", workspace_id: \"ws-goal-1\", workspace_owner_node_did: \"did:key:workspace-owner\", workspace_authority: \"readWrite\", workspace_seal_hash: \"seal-goal-1\", admission_kind: \"runtime-internal\", admission_signer_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", admission_signature: \"<SIGNATURE>\", runtime_issuer_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", runtime_source_request_id: \"pin-parent-request\", runtime_source_kind: \"local-control\", lifecycle_state: \"pending\", failure_reason: \"\""
    );
}
