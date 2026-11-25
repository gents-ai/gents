use gents_protocol::row::AgentRequest;
use gents_protocol::session::{
    SessionCreationCause, SessionCreationReceipt, SessionCreationStarter, SessionProvenance,
    SessionReference,
};
use gents_session::request::context::{DELEGATED_ASSIGNMENT, WORKFLOW_RETURN};
use gents_session::test_support::lineage_fixtures::{
    create_started_session, fixture_session, started_node, FIXTURE_DID,
};
use gents_store::config_client::ConfigAccess;

fn request(session_id: &str, doc_id: &str) -> AgentRequest {
    AgentRequest {
        retry_parent_request_doc_id: None,
        purpose: gents_protocol::request_admission::RequestPurpose::Normal,
        doc_id: doc_id.to_owned(),
        request_id: format!("request-{session_id}-{doc_id}"),
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
        interrupt_generation: None,
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
    let mut forged = fixture_session(FIXTURE_DID, "forged-agent-new");
    forged.provenance = Some(SessionProvenance {
        parent_request_doc_id: Some("parent-request".to_owned()),
        opening_request_doc_id: Some("opening-request".to_owned()),
        started_by: Some(source.clone()),
        creation_receipt: Some(SessionCreationReceipt {
            node_did: FIXTURE_DID.to_owned(),
            session_id: "forged-agent-new".to_owned(),
            session_doc_id: "forged-doc".to_owned(),
            requester_did: Some(FIXTURE_DID.to_owned()),
            agent_did: "worker".to_owned(),
            opening_request_doc_id: "opening-request".to_owned(),
            cause: SessionCreationCause::AgentNew,
            cause_binding: "forged-call".to_owned(),
            starter: Some(SessionCreationStarter {
                source: source.clone(),
                parent_request_doc_id: "parent-request".to_owned(),
            }),
            signature: vec![0u8; 64],
        }),
        ..Default::default()
    });
    let forged_input =
        gents_protocol::graphql::graphql_input_literal(&serde_json::to_value(&forged).unwrap())
            .unwrap();
    ConfigAccess::write_local(
        &db.node,
        "test_seed_forged_agent_new",
        &format!("mutation {{ create_AgentSession(input: {forged_input}) {{ _docID }} }}"),
    )
    .await
    .expect("seed forged session");

    let access = ConfigAccess::local(db.node.clone());
    let tool_config = gents_tools::surface::AgentToolSurfaceConfig::default();
    let render = |session_id: &'static str, doc_id: &'static str, model: Option<&'static str>| {
        let access = access.clone();
        let tool_config = tool_config.clone();
        async move {
            super::inference::render_request_context_message(
                &access,
                model,
                &tool_config,
                &request(session_id, doc_id),
                None,
            )
            .await
            .expect("the context renders")
            .map(|message| format!("{message:?}"))
        }
    };

    let job_child = render("job-child", "doc-job-child", None)
        .await
        .expect("workflow child receives context");
    assert!(job_child.contains(WORKFLOW_RETURN), "{job_child}");
    assert!(!job_child.contains(DELEGATED_ASSIGNMENT), "{job_child}");

    let agent_new_opening = render("agent-new-child", "opening-request", None)
        .await
        .expect("agent_new opening receives delegated assignment");
    assert!(
        agent_new_opening.contains(DELEGATED_ASSIGNMENT),
        "{agent_new_opening}"
    );
    assert!(
        !agent_new_opening.contains(WORKFLOW_RETURN),
        "{agent_new_opening}"
    );

    assert_eq!(
        render("agent-new-child", "later-message-request", None).await,
        None
    );
    assert_eq!(
        render("forged-agent-new", "opening-request", None).await,
        None
    );
    assert_eq!(render("client-opened", "opening-request", None).await, None);

    let with_model = render("job-child", "doc-job-child", Some("model-a"))
        .await
        .expect("a profile's model is named");
    assert!(with_model.contains(WORKFLOW_RETURN), "{with_model}");
    assert!(
        with_model.contains("Your profile's model: model-a."),
        "{with_model}"
    );
}
