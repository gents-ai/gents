use super::*;
use crate::llm::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};
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
    let dir = tempfile::tempdir().unwrap();
    let identity =
        crate::store::identity::KeyIdentity::load_or_create(dir.path().join("agent.key"), None)
            .unwrap();
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(dir.path())
            .with_node_identity_did(identity.did())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    crate::test_support::install_test_agent(&node, identity.did(), "general").await;
    let executions = BackgroundExecutionRegistry::default();
    let hook =
        DefraSessionHook::with_identity(node.clone(), identity.did(), FailurePolicy::default())
            .with_background_tool_registry(BackgroundToolRegistry::from_tools(
                vec![Box::new(PanickingTool)],
                &["panicking_tool".into()],
            ))
            .with_background_execution_registry(executions.clone());
    let session_id = hook.session_id().await.unwrap();
    bind_signed_hook_request(
        &hook,
        "request-background-panic",
        Utc::now() + chrono::Duration::minutes(5),
    )
    .await;
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
    let row = fetch_tool_call_row(&node, &session_id, tool_call_id).await;
    assert_eq!(row["lifecycle_state"], "failed");
    assert!(row["result"]
        .as_str()
        .unwrap()
        .contains("intentional background tool panic"));
    let history =
        crate::session::load_history(&node, &session_id, &hook.node_did, Some(identity.did()))
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
}
