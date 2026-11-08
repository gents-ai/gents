use super::*;

pub(super) async fn setup_private_hook(
    db: &crate::support::TestDb,
    request_id: &str,
    session_id: &str,
    registry: BackgroundToolRegistry,
) -> DefraSessionHook {
    let node_did = db.node_identity.did();
    let binding = crate::test_support::ensure_test_default_agent(&db.node, node_did).await;
    let agent = crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
        .expect("resolve the fixture's default Agent from its runtime root");
    setup_private_hook_for_agent(
        &db.node,
        node_did,
        agent.as_ref(),
        request_id,
        session_id,
        registry,
    )
    .await
}

pub(super) async fn setup_private_hook_for_agent(
    node: &Arc<defra_node::Node>,
    node_did: &str,
    agent_identity: &dyn NodeIdentity,
    request_id: &str,
    session_id: &str,
    registry: BackgroundToolRegistry,
) -> DefraSessionHook {
    let agent_did = agent_identity.did();
    let actor = identity::Did::new(agent_did).expect("fixture agent DID");
    let access =
        crate::ConfigAccess::local_managed_agent(node.clone(), actor.clone(), agent_identity)
            .expect("fixture Agent's managed access");
    let session = crate::store::graphql::escape_graphql_string(session_id);
    let escaped_node = crate::store::graphql::escape_graphql_string(node_did);
    let agent = crate::store::graphql::escape_graphql_string(agent_did);
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    access.write("r4c.create_session", &format!(r#"mutation {{ create_AgentSession(input: {{ session_id: "{session}", node_did: "{escaped_node}", requester_did: "{agent}", agent_did: "{agent}", created_at: "{now}" }}) {{ _docID }} }}"#)).await.expect("create R4C agent-owned session");

    let hook = DefraSessionHook::resume_with_actor_policy(
        node.clone(),
        session_id,
        node_did,
        Some(agent_did),
        actor,
        FailurePolicy::default(),
    )
    .await
    .expect("resume R4C agent-owned session")
    .with_managed_identity(agent_identity)
    .expect("bind the hook to its Agent's managed signer")
    .with_background_tool_registry(registry);
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        request_id,
        node_did,
        agent_did,
        agent_did,
        session_id,
        "R4C accepted control fixture",
        "interactive",
        now,
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(agent_did),
    );
    crate::sign_agent_request_create(agent_identity, &mut create)
        .await
        .expect("sign R4C request as selected agent");
    let created = access
        .write("r4c.create_request", &create.graphql_mutation().unwrap())
        .await
        .expect("create R4C request as selected agent");
    let request_doc_id = crate::store::graphql::created_doc_id(&created, "AgentRequest")
        .unwrap_or_else(|error| panic!("R4C request document ID: {error}; response: {created}"));
    let ids = [request_doc_id.clone()];
    let owners = access
        .transact_readonly("r4c.request_owner", |txn| {
            let ids = ids.clone();
            Box::pin(async move { txn.document_owners("AgentRequest", &ids).await })
        })
        .await
        .expect("observe R4C request owner as selected agent");
    assert_eq!(owners.len(), 1);
    assert_eq!(
        owners[0].owner.as_ref().map(identity::Did::as_str),
        Some(agent_did),
        "the selected agent must own its request"
    );
    let request_doc = crate::store::graphql::escape_graphql_string(&request_doc_id);
    let loaded = access.execute(&format!(r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{request_doc}" }} }}, limit: 1) {{ {} }} }}"#, gents_protocol::row::AGENT_REQUEST_FIELDS)).await.expect("load R4C request as selected agent");
    let rows = loaded["data"]["AgentRequest"]
        .as_array()
        .expect("R4C request query returned rows");
    let [row] = rows.as_slice() else {
        panic!("selected agent must see exactly one R4C request: {loaded}");
    };
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(row.clone()).expect("canonical R4C request row");
    let mut lifecycle = crate::request::lifecycle::RequestLifecycle::new_with_node_did(
        node.clone(),
        row.try_into().expect("canonical R4C request"),
        60,
    );
    lifecycle
        .bind_managed_agent(agent_identity)
        .expect("bind the request to its Agent's managed signer");
    assert_eq!(
        lifecycle.claim().await.unwrap(),
        crate::request::lifecycle::ClaimOutcome::Claimed
    );
    let writer = crate::request::streaming::DefraStreamWriter::new_for_lifecycle(
        node.clone(),
        &lifecycle,
        std::time::Duration::ZERO,
    )
    .unwrap();
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    hook.set_active_request_binding(
        Some(request_id.to_owned()),
        Some(request_doc_id),
        Some(agent_did.to_owned()),
    )
    .await;
    hook.set_request_deadline_at(Some(chrono::Utc::now() + chrono::Duration::minutes(5)))
        .await;
    hook_execution_fixtures().lock().await.insert(
        hook_execution_fixture_key(&hook, request_id),
        HookExecutionFixture {
            lifecycle,
            writer,
            turn: 0,
        },
    );
    hook
}

pub(super) async fn accepted_call(
    hook: &DefraSessionHook,
    name: &str,
    provider_call_id: Option<String>,
    internal_call_id: &str,
    args: &str,
) -> ToolCallHookAction {
    accept_hook_tool_call(
        hook,
        internal_call_id,
        name,
        args,
        provider_call_id.as_deref(),
    )
    .await;
    hook.on_tool_call(name, provider_call_id, internal_call_id, args)
        .await
}

pub(super) async fn assert_accepted_control_rows(
    hook: &DefraSessionHook,
    tool_name: &str,
    expected: usize,
) {
    let session_id = hook.session_id().await.expect("accepted control session");
    let session = crate::store::graphql::escape_graphql_string(&session_id);
    let name = crate::store::graphql::escape_graphql_string(tool_name);
    let requester = hook
        .active_requester_did()
        .await
        .expect("R4C selected agent");
    let access = crate::ConfigAccess::local_as(
        hook.node.clone(),
        identity::Did::new(requester.as_str()).unwrap(),
    );
    let response = access.execute(&format!(r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session}" }}, tool_name: {{ _eq: "{name}" }} }}) {{ _docID request_doc_id message_sequence lifecycle_state }} }}"#)).await.expect("accepted control row query");
    let rows = response["data"]["AgentToolCall"].as_array().unwrap();
    assert_eq!(rows.len(), expected, "exact accepted {tool_name} row count");
    for row in rows {
        let tool_doc_id = row["_docID"].as_str().unwrap();
        let request_doc_id = row["request_doc_id"].as_str().unwrap();
        let sequence = row["message_sequence"].as_u64().unwrap();
        let request_doc = crate::store::graphql::escape_graphql_string(request_doc_id);
        let headers = access.execute(&format!(r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{session}" }}, request_doc_id: {{ _eq: "{request_doc}" }}, sequence: {{ _eq: {sequence} }} }}) {{ _docID }} }}"#)).await.expect("accepted control header query");
        let header_rows = headers["data"]["AgentMessage"].as_array().unwrap();
        assert_eq!(header_rows.len(), 1, "one exact accepted assistant header");
        let header_doc_id = header_rows[0]["_docID"].as_str().unwrap();
        let (header, _) = crate::session::load_canonical_message(
            &access,
            header_doc_id,
            &hook.node_did,
            Some(&requester),
        )
        .await
        .expect("load canonical accepted control header");
        assert!(
            header.blocks.iter().any(|block| matches!(block,
                gents_protocol::output::MessageBlock::ToolCall { tool_call_doc_id, name, .. }
                    if tool_call_doc_id == tool_doc_id && name == tool_name
            )),
            "accepted header must bind exact physical {tool_name} row"
        );
    }
}
