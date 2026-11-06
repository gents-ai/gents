use crate::store::identity::NodeIdentity;

struct PendingTool;

impl crate::inference::llm::tool::ToolDyn for PendingTool {
    fn name(&self) -> String {
        "slow_tool".into()
    }
    fn definition<'a>(
        &'a self,
        _prompt: String,
    ) -> crate::inference::llm::tool::BoxFuture<'a, crate::inference::llm::tool::ToolDefinition>
    {
        Box::pin(async {
            crate::inference::llm::tool::ToolDefinition {
                name: "slow_tool".into(),
                description: "test tool".into(),
                parameters: serde_json::json!({"type":"object"}),
            }
        })
    }
    fn call<'a>(
        &'a self,
        _args: String,
    ) -> crate::inference::llm::tool::BoxFuture<
        'a,
        Result<String, crate::inference::llm::tool::ToolError>,
    > {
        Box::pin(std::future::pending())
    }
}

async fn publish_hook_action(
    hook: &crate::tools::call::hook::DefraSessionHook,
    writer: &crate::request::streaming::DefraStreamWriter,
    owner: &crate::request::lifecycle::RequestLifecycle,
    turn: usize,
    internal_id: &str,
    tool_name: &str,
    args: serde_json::Value,
) -> (crate::inference::llm::ToolCallHookAction, String) {
    writer
        .start_provider_attempt(
            &owner.request().doc_id,
            turn,
            0,
            format!("inference.{}", turn + 1).parse().unwrap(),
        )
        .await;
    let message = gents_protocol::message::Message::Assistant {
        request_id: None,
        id: Some(format!("hook-assistant-{internal_id}")),
        content: vec![gents_protocol::message::AssistantContent::ToolCall(
            gents_protocol::message::ToolCall {
                raw_arguments: None,
                id: internal_id.into(),
                call_id: None,
                function: gents_protocol::message::ToolFunction::new(
                    tool_name.into(),
                    args.clone(),
                ),
                signature: None,
                additional_params: None,
            },
        )],
    };
    let mut published = writer
        .publish_native_turn(owner, turn, 0, &message)
        .await
        .unwrap();
    assert_eq!(published.accepted_tools.len(), 1);
    let accepted = published.accepted_tools.pop().unwrap();
    hook.register_stream_tool_call_identity(internal_id, &accepted.id, None)
        .await;
    hook.adopt_accepted_tool_calls(vec![(internal_id.into(), accepted)])
        .await
        .unwrap();
    let action = hook
        .on_tool_call(tool_name, None, internal_id, &args.to_string())
        .await;
    (action, published.message_doc_id)
}

async fn publish_hook_call(
    hook: &crate::tools::call::hook::DefraSessionHook,
    writer: &crate::request::streaming::DefraStreamWriter,
    owner: &crate::request::lifecycle::RequestLifecycle,
    turn: usize,
    internal_id: &str,
    tool_name: &str,
    args: serde_json::Value,
) -> (serde_json::Value, String) {
    let (action, header) =
        publish_hook_action(hook, writer, owner, turn, internal_id, tool_name, args).await;
    let crate::inference::llm::ToolCallHookAction::Skip { reason } = action else {
        panic!("accepted hook call {internal_id} did not return a result: {action:?}");
    };
    (serde_json::from_str(&reason).unwrap(), header)
}

#[tokio::test]
async fn generated_absent_requester_process_control_uses_accepted_hook_calls() {
    use crate::request::lifecycle::{ClaimOutcome, RequestTerminalOutcome, TerminalizeResult};
    use crate::tools::background::status::ProcessControlScope;
    use gents_protocol::request_admission::{
        AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
    };

    let originating = crate::lean_vocab_test::lean_r6_backgrounding_case(
        "originating_request_without_matching_requester_is_denied",
    );
    let absent =
        crate::lean_vocab_test::lean_r6_backgrounding_case("absent_requester_next_turn_authorized");
    let empty =
        crate::lean_vocab_test::lean_r6_backgrounding_case("empty_requester_does_not_alias_absent");
    let same_requester = crate::lean_vocab_test::lean_r6_backgrounding_case(
        "read_process_same_requester_next_turn_authorized",
    );
    let admission_case = crate::lean_vocab_test::lean_agent_request_admission_cases()
        .iter()
        .find(|case| case.name == "local-self-absent-requester")
        .unwrap();
    let (db, user, client, binding) =
        crate::test_support::signed_test_node("background-hook-scope").await;
    let node = db.node.clone();
    let runtime = db.node_identity.clone();
    let agent = crate::store::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap();
    let access = crate::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let session = "background-hook-scope-session";
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let make_request = |id| {
        AgentRequestCreate::base(
            RequestPurpose::Normal,
            id,
            runtime.did(),
            user.did(),
            agent.did(),
            session,
            "spawn",
            "interactive",
            &now,
            AgentRequestAdmissionRecord::client(user.did()),
        )
    };
    let mut origin =
        crate::tools::call::lifecycle::admission_fixture::pending_client_fixture_request(
            &node,
            &client,
            runtime.clone(),
            &user,
            agent.as_ref(),
            make_request("scope-origin"),
        )
        .await
        .unwrap();
    assert_eq!(
        origin.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    let writer = crate::request::streaming::DefraStreamWriter::new_for_lifecycle(
        node.clone(),
        &origin,
        std::time::Duration::ZERO,
    )
    .unwrap();
    origin.begin_owned_execution(&writer).await.unwrap();
    let registry = crate::tools::call::hook::BackgroundToolRegistry::from_tools(
        vec![Box::new(PendingTool)],
        &["slow_tool".into()],
    );
    let hook = crate::tools::call::hook::DefraSessionHook::resume_with_actor_policy(
        node.clone(),
        session,
        runtime.did(),
        Some(user.did()),
        identity::Did::new(agent.did()).unwrap(),
        crate::tools::call::hook::FailurePolicy::default(),
    )
    .await
    .unwrap()
    .with_managed_identity(agent.as_ref())
    .unwrap()
    .with_background_tool_registry(registry);
    hook.set_active_request_binding(
        Some("scope-origin".into()),
        Some(origin.request().doc_id.clone()),
        Some(user.did().into()),
    )
    .await;
    hook.set_request_deadline_at(Some(chrono::Utc::now() + chrono::Duration::minutes(5)))
        .await;
    let (spawned, header_doc_id) = publish_hook_call(
        &hook,
        &writer,
        &origin,
        0,
        "scope-spawn",
        "spawn_process",
        serde_json::json!({"tool_name":"slow_tool","args":{}}),
    )
    .await;
    assert_eq!(spawned["ok"], true);
    let handle = spawned["tool_call_id"].as_str().unwrap().to_owned();

    // Historical optional scopes still compare exactly, independently of admission.
    let scope = |requester| ProcessControlScope {
        request_id: "historical-next".into(),
        session_id: session.into(),
        node_did: runtime.did().into(),
        requester_did: requester,
    };
    assert_eq!(
        scope(None).authorizes(session, runtime.did(), None),
        absent.legal
    );
    assert_eq!(
        scope(Some(user.did().into())).authorizes(session, runtime.did(), Some(user.did())),
        same_requester.legal
    );
    assert_eq!(
        scope(Some(String::new())).authorizes(session, runtime.did(), None),
        empty.legal
    );
    assert_eq!(
        scope(Some("did:requester".into())).authorizes(session, runtime.did(), None),
        originating.legal
    );

    hook.set_active_request_binding(
        Some("scope-origin".into()),
        Some(origin.request().doc_id.clone()),
        Some("did:requester".into()),
    )
    .await;
    let (denied, _) = publish_hook_action(
        &hook,
        &writer,
        &origin,
        1,
        "scope-origin-denied",
        "read_process",
        serde_json::json!({"tool_call_id":handle}),
    )
    .await;
    let crate::inference::llm::ToolCallHookAction::Terminate { reason } = denied else {
        panic!("forged requester must fail closed: {denied:?}")
    };
    assert!(
        reason.contains("canonical header is unresolved or unauthorized"),
        "{reason}"
    );
    assert_eq!(
        origin
            .terminalize_owned(
                RequestTerminalOutcome::Completed,
                gents_protocol::output::TerminalOutput::Message {
                    message_doc_id: header_doc_id
                },
                None
            )
            .await
            .unwrap(),
        TerminalizeResult::Won
    );

    let mut next =
        crate::tools::call::lifecycle::admission_fixture::pending_client_fixture_request(
            &node,
            &client,
            runtime.clone(),
            &user,
            agent.as_ref(),
            make_request("scope-next"),
        )
        .await
        .unwrap();
    assert_eq!(
        next.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    let next_writer = crate::request::streaming::DefraStreamWriter::new_for_lifecycle(
        node.clone(),
        &next,
        std::time::Duration::ZERO,
    )
    .unwrap();
    next.begin_owned_execution(&next_writer).await.unwrap();
    hook.set_active_request_binding(
        Some("scope-next".into()),
        Some(next.request().doc_id.clone()),
        Some(user.did().into()),
    )
    .await;
    let (read, _) = publish_hook_call(
        &hook,
        &next_writer,
        &next,
        0,
        "scope-read",
        "read_process",
        serde_json::json!({"tool_call_id":handle}),
    )
    .await;
    assert_eq!(read["status"], "running", "{read}");
    assert_eq!(read["tool_call_id"], handle);
    let (cancelled, _) = publish_hook_call(
        &hook,
        &next_writer,
        &next,
        1,
        "scope-cleanup",
        "cancel_process",
        serde_json::json!({"tool_call_id":handle}),
    )
    .await;
    assert_eq!(cancelled["status"], "cancelled");

    // Retained documents can have no requester even though current authors cannot submit one.
    let mut historical = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "scope-historical",
        runtime.did(),
        agent.did(),
        agent.did(),
        session,
        "historical",
        "interactive",
        &now,
        AgentRequestAdmissionRecord::local_self(agent.did()),
    );
    let payload = {
        let mut fields = historical.signing_fields();
        fields.requester_did = None;
        historical.admission.signing_payload(&fields)
    };
    historical.admission.signature = agent.sign(&payload).await.unwrap();
    let mut input = crate::store::config_client::client_request_input(&historical).unwrap();
    input["requester_did"] = serde_json::Value::Null;
    access.transact_write("test.historical_request", |txn| {
        let input = &input;
        Box::pin(async move { txn.execute_with_variables("mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }", &serde_json::json!({"input":input})).await })
    }).await.unwrap();
    let response = access.execute(&format!(r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "scope-historical" }} }}) {{ {} }} }}"#, crate::request::watcher::AGENT_REQUEST_FIELDS)).await.unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    let request: crate::request::watcher::AgentRequest = row.try_into().unwrap();
    assert!(request.requester_did.is_none());
    let (_owner, authority) = crate::runtime::agent::p2p_reconcile::enrollment_authority_channel();
    let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
        node.clone(),
        runtime.clone(),
        authority,
    );
    let result = verifier
        .verify_fresh_for_claim_with_identity(&request, agent.as_ref())
        .await;
    assert!(!admission_case.expected_admitted);
    assert_eq!(
        result.as_ref().err().is_some_and(|error| error.is_denied()),
        admission_case.expected_disposition == "deny",
        "{result:?}"
    );
    let response = access
        .execute(
            "{ AgentToolCall(filter: { request_id: { _eq: \"scope-historical\" } }) { _docID } }",
        )
        .await
        .unwrap();
    assert_eq!(
        response["data"]["AgentToolCall"].as_array().unwrap().len(),
        0
    );
    node.shutdown().await.expect("shut down test node");
}
