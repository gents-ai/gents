use super::*;
use crate::identity::AgentIdentity;
use crate::llm::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};

struct PendingTool;

impl ToolDyn for PendingTool {
    fn name(&self) -> String {
        "slow_tool".into()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "slow_tool".into(),
                description: "Held work for principal-scoped process controls".into(),
                parameters: json!({"type":"object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(std::future::pending())
    }
}

async fn invoke(hook: &DefraSessionHook, id: &str, name: &str, args: &str) -> serde_json::Value {
    accept_hook_tool_call(hook, id, name, args, None).await;
    let ToolCallHookAction::Skip { reason } = hook.on_tool_call(name, None, id, args).await else {
        panic!("accepted {name} must return a tool result")
    };
    serde_json::from_str(&reason).expect("canonical process-control result JSON")
}

async fn finish_owned_request(hook: &DefraSessionHook, request_id: &str) {
    publish_claimed_provider_turn(
        hook,
        request_id,
        Message::Assistant {
            id: Some(format!("final-{request_id}")),
            content: vec![AssistantContent::Text(Text {
                text: "process controls complete".into(),
            })],
        },
    )
    .await;
    let request_doc_id = {
        let fixtures = hook_execution_fixtures().lock().await;
        fixtures
            .get(&hook_execution_fixture_key(hook, request_id))
            .expect("owned process-control request fixture")
            .lifecycle
            .request()
            .doc_id
            .clone()
    };
    let session_id = hook.session_id().await.expect("owned session");
    let agent_did = hook.agent_did.clone();
    let requester_did = hook.active_requester_did().await;
    let headers = crate::config_client::ConfigAccess::transact_local(
        hook.node.as_ref(),
        None,
        "test.process_control_scope.headers",
        |txn| {
            let request_doc_id = request_doc_id.clone();
            let session_id = session_id.clone();
            let agent_did = agent_did.clone();
            let requester_did = requester_did.clone();
            Box::pin(async move {
                crate::session::load_request_headers_in_txn(
                    txn,
                    &session_id,
                    &agent_did,
                    requester_did.as_deref(),
                    &request_doc_id,
                )
                .await
            })
        },
    )
    .await
    .expect("load exact canonical request headers");
    let selected = headers
        .iter()
        .filter(|header| {
            matches!(
                &header.message.role,
                gents_protocol::output::MessageRole::Assistant
            ) && matches!(
                &header.message.publication,
                gents_protocol::output::MessagePublication::RequestExecution { .. }
            )
        })
        .max_by_key(|header| header.message.sequence)
        .expect("owned request has an accepted assistant header")
        .doc_id
        .clone();
    let mut fixtures = hook_execution_fixtures().lock().await;
    let fixture = fixtures
        .get_mut(&hook_execution_fixture_key(hook, request_id))
        .expect("owned process-control request fixture");
    fixture
        .lifecycle
        .terminalize_owned(
            crate::lifecycle::RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::Message {
                message_doc_id: selected,
            },
            None,
        )
        .await
        .expect("finish owned request through terminal owner");
}

#[tokio::test]
async fn accepted_cross_agent_process_controls_preserve_the_owners_running_job() {
    run_process_control_scope(None).await;
    run_process_control_scope(Some("did:key:shared-process-requester")).await;
}

async fn run_process_control_scope(requester_did: Option<&str>) {
    let dir = tempfile::tempdir().unwrap();
    let owner_identity =
        crate::identity::KeyIdentity::load_or_create(dir.path().join("owner.key"), None).unwrap();
    let foreign_identity =
        crate::identity::KeyIdentity::load_or_create(dir.path().join("foreign.key"), None).unwrap();
    let owner_did = owner_identity.did();
    let foreign_did = foreign_identity.did();
    assert_ne!(owner_did, foreign_did);
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(dir.path())
            .with_node_identity_did(owner_did)
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    for did in [owner_did, foreign_did] {
        crate::test_support::install_test_behavior(&node, did, "general").await;
    }

    let executions = BackgroundExecutionRegistry::default();
    let owner = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        owner_did,
        FailurePolicy::default(),
    )
    .with_background_tool_registry(BackgroundToolRegistry::from_tools(
        vec![Box::new(PendingTool)],
        &["slow_tool".into()],
    ))
    .with_background_execution_registry(executions.clone());
    owner
        .on_completion_call(&user_text_message("run scoped background work"), &[])
        .await;
    let session_id = owner.session_id().await.unwrap();
    for did in [owner_did, foreign_did] {
        crate::session::ensure_session_with_behavior_id_and_requester_did(
            &node,
            &session_id,
            "general",
            did,
            "general",
            requester_did,
        )
        .await
        .unwrap();
    }
    let foreign = DefraSessionHook::resume_with_identity_policy(
        node.clone(),
        &session_id,
        "general",
        foreign_did,
        requester_did,
        FailurePolicy::default(),
    )
    .await
    .unwrap();
    let suffix = uuid::Uuid::new_v4();
    let owner_request = format!("process-control-owner-{suffix}");
    let foreign_request = format!("process-control-foreign-{suffix}");
    let owner_cancel_request = format!("process-control-owner-cancel-{suffix}");
    let deadline = Utc::now() + chrono::Duration::minutes(5);
    bind_interruptible_request_with_requester(
        &node,
        &owner,
        &owner_request,
        &session_id,
        deadline,
        requester_did,
    )
    .await;
    // The real session FIFO admits one owned request at a time. The foreign
    // caller receives its own request after the spawning parent completes;
    // the background worker must outlive that parent completion.
    assert_eq!(
        owner.session_id().await.as_deref(),
        Some(session_id.as_str())
    );
    assert_eq!(
        foreign.session_id().await.as_deref(),
        Some(session_id.as_str())
    );
    assert_eq!(owner.active_requester_did().await.as_deref(), requester_did);

    let receipt = invoke(
        &owner,
        "cross-agent-spawn",
        "spawn_process",
        r#"{"tool_name":"slow_tool","args":{}}"#,
    )
    .await;
    assert_eq!(receipt["status"], "running");
    let tool_call_id = receipt["tool_call_id"]
        .as_str()
        .expect("accepted spawned process ID")
        .to_owned();
    let args = json!({"tool_call_id":tool_call_id}).to_string();
    let row = fetch_tool_call_row(&node, &session_id, &tool_call_id).await;
    assert_eq!(row["request_id"], owner_request);
    assert_eq!(row["agent_did"], owner_did);
    assert_eq!(row["requester_did"].as_str(), requester_did);
    assert_eq!(row["lifecycle_state"], "running");
    assert_eq!(row["await_mode"], "background");

    let owner_list = invoke(&owner, "owner-list", "list_processes", "{}").await;
    assert!(owner_list["entries"]
        .as_array()
        .expect("owner process entries")
        .iter()
        .any(|entry| entry["tool_call_id"] == tool_call_id));
    let owner_read = invoke(&owner, "owner-read", "read_process", &args).await;
    assert_eq!(owner_read["status"], "running");

    finish_owned_request(&owner, &owner_request).await;
    let after_owner_completion = fetch_tool_call_row(&node, &session_id, &tool_call_id).await;
    assert_eq!(after_owner_completion["lifecycle_state"], "running");
    bind_interruptible_request_with_requester(
        &node,
        &foreign,
        &foreign_request,
        &session_id,
        deadline,
        requester_did,
    )
    .await;
    assert_eq!(
        foreign.active_requester_did().await.as_deref(),
        requester_did
    );

    let foreign_list = invoke(&foreign, "foreign-list", "list_processes", "{}").await;
    assert!(!foreign_list["entries"]
        .as_array()
        .expect("foreign process entries")
        .iter()
        .any(|entry| entry["tool_call_id"] == tool_call_id));
    let foreign_read = invoke(&foreign, "foreign-read", "read_process", &args).await;
    assert_eq!(foreign_read["ok"], false);
    assert_eq!(foreign_read["failure_class"], "tool_not_allowed");
    let foreign_read_row = fetch_tool_call_row(&node, &session_id, "foreign-read").await;
    assert_eq!(foreign_read_row["request_id"], foreign_request);
    assert_eq!(foreign_read_row["agent_did"], foreign_did);
    assert_eq!(foreign_read_row["requester_did"].as_str(), requester_did);
    for name in ["wait_process", "cancel_process"] {
        let denial = invoke(&foreign, &format!("foreign-{name}"), name, &args).await;
        assert_eq!(denial["ok"], false, "{name}: {denial}");
        assert!(denial["message"]
            .as_str()
            .is_some_and(|message| message.contains("not manageable by this session principal")),
            "{name}: {denial}");
    }
    let denied = crate::tool_control::cancel_session_background_process(
        node.clone(),
        &BackgroundExecutionRegistry::default(),
        foreign_did,
        requester_did,
        &session_id,
        &tool_call_id,
    )
    .await
    .unwrap();
    assert!(matches!(
        denied,
        crate::tool_control::CancelBackgroundToolCallOutcome::NotFound
    ));
    let still_running = fetch_tool_call_row(&node, &session_id, &tool_call_id).await;
    assert_eq!(still_running["lifecycle_state"], "running");
    assert!(still_running["cancel_cause"].is_null());

    finish_owned_request(&foreign, &foreign_request).await;
    bind_interruptible_request_with_requester(
        &node,
        &owner,
        &owner_cancel_request,
        &session_id,
        deadline,
        requester_did,
    )
    .await;
    let cancelled = invoke(&owner, "owner-cancel", "cancel_process", &args).await;
    assert_eq!(cancelled["status"], "cancelled");
    executions.wait_for_completion(&tool_call_id).await;
    let final_row = fetch_tool_call_row(&node, &session_id, &tool_call_id).await;
    assert_eq!(final_row["lifecycle_state"], "cancelled");
    hook_execution_fixtures()
        .lock()
        .await
        .remove(&hook_execution_fixture_key(&owner, &owner_request));
    hook_execution_fixtures()
        .lock()
        .await
        .remove(&hook_execution_fixture_key(&foreign, &foreign_request));
    hook_execution_fixtures()
        .lock()
        .await
        .remove(&hook_execution_fixture_key(&owner, &owner_cancel_request));
    node.shutdown().await;
}
