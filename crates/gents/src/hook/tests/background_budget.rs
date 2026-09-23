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
                description: "Held background work for admission accounting".into(),
                parameters: json!({"type": "object"}),
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
    serde_json::from_str(&reason).unwrap()
}

async fn assert_background_rows(node: &EmbeddedNode, request: &str, count: usize, state: &str) {
    let response = node.execute(&format!(
        r#"{{ AgentToolCall(filter: {{ request_id: {{_eq: "{}"}}, await_mode: {{_eq: "background"}} }}) {{ _docID lifecycle_state tool_name }} }}"#,
        crate::graphql::escape_graphql_string(request),
    )).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let data = response.data.unwrap();
    let rows = data["AgentToolCall"].as_array().unwrap();
    assert_eq!(rows.len(), count);
    for row in rows {
        assert_eq!(row["lifecycle_state"], state, "{row}");
        assert_eq!(row["tool_name"], "slow_tool", "{row}");
    }
}

#[tokio::test]
async fn generated_background_budget_uses_accepted_dispatch() {
    let witness = crate::lean_vocab_test::lean_r6_background_theorem_witnesses()
        .into_iter()
        .find(|w| w.witness_kind == "admission_bound")
        .expect("modeled admission bound");
    let dir = tempfile::tempdir().unwrap();
    let identity =
        crate::identity::KeyIdentity::load_or_create(dir.path().join("agent.key"), None).unwrap();
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(dir.path())
            .with_node_identity_did(identity.did())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    crate::test_support::install_test_behavior(&node, identity.did(), "general").await;
    let executions = BackgroundExecutionRegistry::default();
    let hook = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        identity.did(),
        FailurePolicy::default(),
    )
    .with_background_tool_registry(BackgroundToolRegistry::from_tools(
        vec![Box::new(PendingTool)],
        &["slow_tool".into()],
    ))
    .with_background_execution_registry(executions.clone());
    hook.on_completion_call(&user_text_message("fill background capacity"), &[])
        .await;
    let session = hook.session_id().await.unwrap();
    crate::session::create_session_with_behavior_id(
        &node,
        &session,
        "general",
        identity.did(),
        "general",
    )
    .await
    .unwrap();
    let request_id = format!("budget-{}", uuid::Uuid::new_v4());
    bind_interruptible_request(
        &node,
        &hook,
        &request_id,
        &session,
        Utc::now() + chrono::Duration::minutes(5),
    )
    .await;
    let args = r#"{"tool_name":"slow_tool","args":{}}"#;
    let mut children = Vec::new();
    for index in 0..witness.numeric_bound {
        let receipt = invoke(&hook, &format!("budget-{index}"), "spawn_process", args).await;
        assert_eq!(receipt["status"], "running");
        assert_eq!(receipt["await_mode"], witness.kind_field("await_mode"));
        let child = receipt["tool_call_id"].as_str().unwrap().to_owned();
        let row = fetch_tool_call_row(&node, &session, &child).await;
        assert_eq!(row["await_mode"], witness.kind_field("await_mode"));
        assert_eq!(row["cancel_policy"], witness.kind_field("cancel_policy"));
        assert_background_rows(&node, &request_id, index + 1, "running").await;
        children.push(child);
    }
    let denied = invoke(&hook, "budget-overflow", "spawn_process", args).await;
    assert_eq!(
        denied["code"],
        witness.kind_field("error_code_on_violation")
    );
    assert_eq!(
        denied["current_backgrounded"].as_u64(),
        Some(witness.numeric_bound as u64)
    );
    assert_eq!(
        denied["max_backgrounded"].as_u64(),
        Some(witness.numeric_bound as u64)
    );
    assert_background_rows(&node, &request_id, witness.numeric_bound, "running").await;
    for (index, child) in children.iter().enumerate() {
        let args = json!({"tool_call_id": child}).to_string();
        let result = invoke(&hook, &format!("cancel-{index}"), "cancel_process", &args).await;
        assert_eq!(result["status"], "cancelled");
        executions.wait_for_completion(child).await;
    }
    assert_background_rows(&node, &request_id, witness.numeric_bound, "cancelled").await;
    hook_execution_fixtures().lock().await.remove(&request_id);
    node.shutdown().await;
}
