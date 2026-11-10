use super::*;
use crate::inference::llm::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};
use crate::store::identity::NodeIdentity;

struct PanickingTool;

impl ToolDyn for PanickingTool {
    fn name(&self) -> String {
        "panicking_tool".into()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "panicking_tool".into(),
                description: "Panic regression at the background execution owner".into(),
                parameters: json!({"type": "object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async { panic!("intentional background tool panic") })
    }
}

#[tokio::test]
async fn accepted_background_panic_terminalizes_and_notifies_before_release() {
    use gents_protocol::request_admission::{
        AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
    };

    let (db, user, owner, binding) =
        crate::test_support::signed_test_node("hook-background-panic").await;
    let node = db.node.clone();
    let agent = crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
        .unwrap();
    let agent_access = crate::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let session_id = "background-panic-session";
    let create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "request-background-panic",
        db.node_identity.did(),
        user.did(),
        agent.did(),
        session_id,
        "spawn",
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        AgentRequestAdmissionRecord::client(user.did()),
    );
    let mut lifecycle =
        crate::tools::call::lifecycle::admission_fixture::pending_client_fixture_request(
            &node,
            &owner,
            db.node_identity.clone(),
            &user,
            agent.as_ref(),
            create,
        )
        .await
        .unwrap();
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        crate::request::lifecycle::ClaimOutcome::Claimed
    );
    let executions = BackgroundExecutionRegistry::default();
    let hook = DefraSessionHook::resume_with_actor_policy(
        node.clone(),
        session_id,
        db.node_identity.did(),
        Some(user.did()),
        identity::Did::new(agent.did()).unwrap(),
        FailurePolicy::default(),
    )
    .await
    .unwrap()
    .with_managed_identity(agent.as_ref())
    .unwrap()
    .with_background_tool_registry(BackgroundToolRegistry::from_tools(
        vec![Box::new(PanickingTool)],
        &["panicking_tool".into()],
    ))
    .with_background_execution_registry(executions.clone());
    bind_claimed_hook_request(&hook, lifecycle, Utc::now() + chrono::Duration::minutes(5)).await;
    hook.on_completion_call(&user_text_message("run tool"), &[])
        .await;
    let args = r#"{"tool_name":"panicking_tool","args":{}}"#;
    accept_hook_tool_call(&hook, "meta-background-panic", "spawn_process", args, None).await;
    let action = hook
        .on_tool_call("spawn_process", None, "meta-background-panic", args)
        .await;
    let ToolCallHookAction::Skip { reason } = action else {
        panic!("expected spawn receipt")
    };
    let receipt: serde_json::Value = serde_json::from_str(&reason).unwrap();
    let tool_call_id = receipt["tool_call_id"].as_str().unwrap();
    executions.wait_for_completion(tool_call_id).await;
    let escaped = crate::store::graphql::escape_graphql_string(tool_call_id);
    let response = agent_access.execute(&format!(r#"{{ AgentToolCall(filter: {{ tool_call_id: {{_eq: "{escaped}"}}}}, limit: 1) {{_docID request_doc_id session_id node_did requester_did lifecycle_state}} }}"#)).await.unwrap();
    let row = &response["data"]["AgentToolCall"][0];
    assert_eq!(row["lifecycle_state"], "failed");
    let output = crate::tools::background::status::observe_canonical_tool_output_with_access(
        &agent_access,
        row["_docID"].as_str().unwrap(),
        row["request_doc_id"].as_str().unwrap(),
        session_id,
        db.node_identity.did(),
        Some(user.did()),
    )
    .await
    .unwrap()
    .into_text();
    assert!(output.contains("intentional background tool panic"));
    let history = crate::session::load_history_with_access(
        &agent_access,
        session_id,
        db.node_identity.did(),
        Some(user.did()),
    )
    .await
    .unwrap();
    let marker = format!("<tool-completion tool_call_id=\"{tool_call_id}\"");
    let notification = history
        .iter()
        .filter_map(|message| match message {
            Message::User { content } => Some(content),
            _ => None,
        })
        .flatten()
        .filter_map(|content| match content {
            UserContent::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .find(|text| text.contains(&marker))
        .unwrap_or_else(|| {
            panic!("registry release must follow durable completion publication: {history:?}")
        });
    assert!(notification.contains("status=\"failed\""));
    assert!(notification.contains("<reason>tool_panicked</reason>"));
    hook_execution_fixtures()
        .lock()
        .await
        .remove(&hook_execution_fixture_key(
            &hook,
            "request-background-panic",
        ));
}
