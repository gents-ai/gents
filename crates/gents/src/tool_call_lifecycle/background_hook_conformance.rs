//! Background process-control conformance at the production hook seam.
//! Native admissions come from canonical assistant publication.

use crate::graphql::escape_graphql_string;
use crate::identity::AgentIdentity;
use crate::tool_call_lifecycle::ToolCallLifecycle;
use std::sync::Arc;

struct PendingTool;

impl crate::llm::tool::ToolDyn for PendingTool {
    fn name(&self) -> String {
        "slow_tool".into()
    }
    fn definition<'a>(
        &'a self,
        _prompt: String,
    ) -> crate::llm::tool::BoxFuture<'a, crate::llm::tool::ToolDefinition> {
        Box::pin(async {
            crate::llm::tool::ToolDefinition {
                name: "slow_tool".into(),
                description: "test tool".into(),
                parameters: serde_json::json!({"type":"object"}),
            }
        })
    }
    fn call<'a>(
        &'a self,
        _args: String,
    ) -> crate::llm::tool::BoxFuture<'a, Result<String, crate::llm::tool::ToolError>> {
        Box::pin(std::future::pending())
    }
}

async fn tool_state(node: &crate::defra_node::EmbeddedNode, tool_id: &str) -> Option<String> {
    let tool_id = escape_graphql_string(tool_id);
    let response = node.execute(&format!(r#"{{ AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{tool_id}" }} }}, limit: 1) {{ lifecycle_state }} }}"#)).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let row: serde_json::Value = crate::graphql::first_row(&response, "AgentToolCall")
        .unwrap()
        .unwrap();
    row["lifecycle_state"].as_str().map(str::to_owned)
}

async fn publish_hook_action(
    hook: &crate::hook::DefraSessionHook,
    writer: &crate::streaming::DefraStreamWriter,
    owner: &crate::lifecycle::RequestLifecycle,
    turn: usize,
    internal_id: &str,
    tool_name: &str,
    args: serde_json::Value,
) -> (crate::llm::ToolCallHookAction, String) {
    writer
        .start_provider_attempt(
            &owner.request().doc_id,
            turn,
            0,
            format!("inference.{}", turn + 1).parse().unwrap(),
        )
        .await;
    let message = gents_protocol::message::Message::Assistant {
        id: Some(format!("hook-assistant-{internal_id}")),
        content: vec![gents_protocol::message::AssistantContent::ToolCall(
            gents_protocol::message::ToolCall {
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
    hook: &crate::hook::DefraSessionHook,
    writer: &crate::streaming::DefraStreamWriter,
    owner: &crate::lifecycle::RequestLifecycle,
    turn: usize,
    internal_id: &str,
    tool_name: &str,
    args: serde_json::Value,
) -> (serde_json::Value, String) {
    let (action, header) =
        publish_hook_action(hook, writer, owner, turn, internal_id, tool_name, args).await;
    let crate::llm::ToolCallHookAction::Skip { reason } = action else {
        panic!("accepted hook call {internal_id} did not return a result: {action:?}");
    };
    (serde_json::from_str(&reason).unwrap(), header)
}

#[tokio::test]
async fn generated_absent_requester_process_control_uses_accepted_hook_calls() {
    let path = std::env::temp_dir().join(format!("background-hook-scope-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    let identity = crate::KeyIdentity::load_or_create(path.join("agent.key"), None).unwrap();
    let did = identity.did().to_owned();
    let node = Arc::new(
        crate::defra_node::EmbeddedNode::builder()
            .data_path(&path)
            .with_node_identity_did(&did)
            .build()
            .await
            .unwrap(),
    );
    crate::schema::ensure_runtime_schemas(&node).await.unwrap();
    crate::test_support::install_test_behavior(&node, &did, "general").await;
    let session_id = "background-hook-scope-session";
    let request_id = "background-hook-scope-origin";
    let mut origin = crate::tool_call_lifecycle::admission_fixture::claimed_request(
        &node, request_id, session_id, &did,
    )
    .await;
    let writer =
        crate::streaming::DefraStreamWriter::new(node.clone(), &did, std::time::Duration::ZERO);
    origin.begin_owned_execution(&writer).await.unwrap();
    let registry = crate::hook::BackgroundToolRegistry::from_tools(
        vec![Box::new(PendingTool)],
        &["slow_tool".into()],
    );
    let hook = crate::hook::DefraSessionHook::resume_with_identity_policy(
        node.clone(),
        session_id,
        "general",
        &did,
        None,
        crate::hook::FailurePolicy::default(),
    )
    .await
    .unwrap()
    .with_background_tool_registry(registry);
    hook.set_active_request_binding(
        Some(request_id.into()),
        Some(origin.request().doc_id.clone()),
        None,
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
    let handle = spawned["tool_call_id"]
        .as_str()
        .expect("background handle")
        .to_owned();
    let row = node.execute(&format!(r#"{{ AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{}" }} }}, limit: 1) {{ requester_did }} }}"#, escape_graphql_string(&handle))).await;
    assert!(!row.has_errors(), "{:?}", row.errors);
    let row: serde_json::Value = crate::graphql::first_row(&row, "AgentToolCall")
        .unwrap()
        .unwrap();
    assert!(row
        .get("requester_did")
        .is_some_and(serde_json::Value::is_null));

    // Exercise the originating-request denial while its owner is live. The
    // owned completion boundary must close it before a second request can
    // claim the same principal's execution slot.
    let originating_name = "originating_request_without_matching_requester_is_denied";
    let originating_case = crate::lean_vocab_test::lean_r6_backgrounding_case(originating_name);
    assert!(!originating_case.legal);
    let background = ToolCallLifecycle::load(node.clone(), session_id, &handle)
        .await
        .unwrap()
        .expect("persisted background owner");
    let originating_scope = crate::background_tools::ProcessControlScope {
        request_id: request_id.into(),
        session_id: session_id.into(),
        agent_did: did.clone(),
        requester_did: Some("did:requester".into()),
    };
    assert!(
        !originating_scope.authorizes(
            background.session_id(),
            background.agent_did(),
            background.requester_did(),
        ),
        "{originating_name}: persisted owner must reject mismatched requester"
    );
    hook.set_active_request_binding(
        Some(request_id.into()),
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
    let crate::llm::ToolCallHookAction::Terminate { reason } = denied else {
        panic!("{originating_name}: mismatched physical header must fail closed: {denied:?}");
    };
    assert!(
        reason.contains("canonical header is unresolved or unauthorized"),
        "{originating_name}: {reason}"
    );

    assert_eq!(
        origin
            .terminalize_owned(
                crate::lifecycle::RequestTerminalOutcome::Completed,
                gents_protocol::output::TerminalOutput::Message {
                    message_doc_id: header_doc_id
                },
                None,
            )
            .await
            .unwrap(),
        crate::lifecycle::TerminalizeResult::Won
    );

    let next_id = "background-hook-scope-next";
    let mut next = crate::tool_call_lifecycle::admission_fixture::claimed_request(
        &node, next_id, session_id, &did,
    )
    .await;
    let next_writer =
        crate::streaming::DefraStreamWriter::new(node.clone(), &did, std::time::Duration::ZERO);
    next.begin_owned_execution(&next_writer).await.unwrap();
    let absent_name = "absent_requester_next_turn_authorized";
    assert!(crate::lean_vocab_test::lean_r6_backgrounding_case(absent_name).legal);
    hook.set_active_request_binding(
        Some(next_id.into()),
        Some(next.request().doc_id.clone()),
        None,
    )
    .await;
    let (read, _) = publish_hook_call(
        &hook,
        &next_writer,
        &next,
        0,
        "scope-read-absent",
        "read_process",
        serde_json::json!({"tool_call_id":handle}),
    )
    .await;
    assert_eq!(read["status"], "running", "{absent_name}: {read}");
    assert_eq!(read["tool_call_id"], handle, "{absent_name}");

    let empty_name = "empty_requester_does_not_alias_absent";
    assert!(!crate::lean_vocab_test::lean_r6_backgrounding_case(empty_name).legal);
    let empty_scope = crate::background_tools::ProcessControlScope {
        request_id: next_id.into(),
        session_id: session_id.into(),
        agent_did: did.clone(),
        requester_did: Some(String::new()),
    };
    assert!(
        !empty_scope.authorizes(
            background.session_id(),
            background.agent_did(),
            background.requester_did(),
        ),
        "{empty_name}: empty requester must not alias absence"
    );
    hook.set_active_request_binding(
        Some(next_id.into()),
        Some(next.request().doc_id.clone()),
        Some(String::new()),
    )
    .await;
    let (denied, _) = publish_hook_call(
        &hook,
        &next_writer,
        &next,
        1,
        "scope-read-empty",
        "read_process",
        serde_json::json!({"tool_call_id":handle}),
    )
    .await;
    assert_eq!(denied["ok"], false, "{empty_name}: {denied}");
    assert_eq!(
        denied["failure_class"], "tool_not_allowed",
        "{empty_name}: {denied}"
    );
    assert_eq!(tool_state(&node, &handle).await.as_deref(), Some("running"));
    let escaped_handle = escape_graphql_string(&handle);
    let response = node.execute(&format!(r#"{{ AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{escaped_handle}" }} }}, limit: 1) {{ cancel_cause }} }}"#)).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let row: serde_json::Value = crate::graphql::first_row(&response, "AgentToolCall")
        .unwrap()
        .unwrap();
    assert!(
        row["cancel_cause"].is_null(),
        "denied reads must not cancel the background process"
    );
    hook.set_active_request_binding(
        Some(next_id.into()),
        Some(next.request().doc_id.clone()),
        None,
    )
    .await;
    let (cancelled, _) = publish_hook_call(
        &hook,
        &next_writer,
        &next,
        2,
        "scope-cleanup",
        "cancel_process",
        serde_json::json!({"tool_call_id":handle}),
    )
    .await;
    assert_eq!(cancelled["status"], "cancelled");
    node.shutdown().await;
    std::fs::remove_dir_all(path).unwrap();
}
