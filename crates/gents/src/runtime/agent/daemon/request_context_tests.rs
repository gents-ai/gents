use gents_protocol::row::AgentRequest;
use gents_protocol::session::{SessionCreationCause, SessionReference};
use gents_session::request::context::WORKFLOW_RETURN;
use gents_session::test_support::lineage_fixtures::{
    create_started_session, fixture_session, started_node, FIXTURE_DID,
};
use gents_store::config_client::ConfigAccess;

fn request(session_id: &str) -> AgentRequest {
    AgentRequest {
        retry_parent_request_doc_id: None,
        purpose: gents_protocol::request_admission::RequestPurpose::Normal,
        doc_id: format!("doc-{session_id}"),
        request_id: format!("request-{session_id}"),
        node_did: FIXTURE_DID.to_owned(),
        requester_did: Some(FIXTURE_DID.to_owned()),
        agent_did: "worker".to_owned(),
        session_id: session_id.to_owned(),
        content: "a child's task".to_owned(),
        max_total_tokens: None,
        input: Default::default(),
        execution_origin: Some("interactive".to_owned()),
        created_at: "2026-11-21T00:00:00Z".to_owned(),
        deadline: None,
        execution_generation: None,
        execution_lease_expires_at: None,
        execution_lease_secs: None,
        request_hop: 0,
        caused_by_parent_request_id: None,
        caused_by_parent_request_doc_id: None,
        caused_by_parent_tool_call_id: None,
        caused_by_parent_tool_call_doc_id: None,
        caused_by_trigger_id: None,
        caused_by_trigger_kind: None,
        caused_by_source_doc_id: None,
        caused_by_correlation: None,
        caused_by_trigger_context: None,
        workspace_id: None,
        workspace_owner_node_did: None,
        workspace_authority: None,
        workspace_seal_hash: None,
        executed_model_name: None,
        executed_reasoning_effort: None,
        executed_service_tier: None,
        executed_inference_profile_id: None,
        executed_runtime_generation: None,
    }
}

#[tokio::test]
async fn only_a_verified_workflow_child_is_told_where_its_answer_returns() {
    let db = started_node("request-context-workflow-child").await;
    let source = SessionReference {
        node_did: FIXTURE_DID.to_owned(),
        session_id: "caller".to_owned(),
        requester_did: Some(FIXTURE_DID.to_owned()),
    };
    for (session_id, cause) in [
        ("job-child", SessionCreationCause::PluginJob),
        ("agent-new-child", SessionCreationCause::AgentNew),
    ] {
        create_started_session(
            &db.node,
            fixture_session(FIXTURE_DID, session_id),
            source.clone(),
            "parent-request",
            "opening-request",
            cause,
        )
        .await;
    }
    let access = ConfigAccess::local(db.node.clone());
    let tool_config = gents_tools::surface::AgentToolSurfaceConfig::default();
    let mut rendered = Vec::new();
    for session_id in ["job-child", "agent-new-child", "client-opened"] {
        rendered.push(
            super::inference::render_request_context_message(
                &access,
                None,
                &tool_config,
                &request(session_id),
                None,
            )
            .await
            .expect("the context renders")
            .map(|message| format!("{message:?}")),
        );
    }
    assert!(
        rendered[0]
            .as_deref()
            .is_some_and(|context| context.contains(WORKFLOW_RETURN)),
        "{rendered:?}"
    );
    assert_eq!(rendered[1..], [None, None]);
    let with_model = super::inference::render_request_context_message(
        &access,
        Some("model-a"),
        &tool_config,
        &request("job-child"),
        None,
    )
    .await
    .expect("the context renders")
    .map(|message| format!("{message:?}"))
    .expect("a profile's model is named");
    assert!(with_model.contains(WORKFLOW_RETURN), "{with_model}");
    assert!(
        with_model.contains("Your profile's model: model-a."),
        "{with_model}"
    );
}
