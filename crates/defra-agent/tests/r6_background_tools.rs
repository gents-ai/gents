//! R6 agent-facing background tool integration tests.

mod support;

use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::{BackgroundToolRegistry, DefraSessionHook, FailurePolicy};
use rig::agent::{PromptHook, ToolCallHookAction};
use rig::completion::ToolDefinition;
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse};
use rig::streaming::StreamingCompletionResponse;
use rig::tool::{ToolDyn, ToolError};
use rig::wasm_compat::WasmBoxedFuture;
use serde::Deserialize;
use serde_json::Value;

use support::{first_row, test_db};

#[derive(Clone, Default)]
struct TestModel;

#[allow(refining_impl_trait)]
impl CompletionModel for TestModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &Self::Client, _: impl Into<String>) -> Self {
        Self
    }

    async fn completion(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        Err(CompletionError::ProviderError(
            "completion is unused in R6 background tool tests".to_string(),
        ))
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        Err(CompletionError::ProviderError(
            "streaming is unused in R6 background tool tests".to_string(),
        ))
    }
}

struct StaticTool {
    name: &'static str,
    result: &'static str,
}

impl ToolDyn for StaticTool {
    fn name(&self) -> String {
        self.name.to_string()
    }

    fn definition<'a>(&'a self, _prompt: String) -> WasmBoxedFuture<'a, ToolDefinition> {
        Box::pin(async move {
            ToolDefinition {
                name: self.name.to_string(),
                description: "test tool".to_string(),
                parameters: serde_json::json!({"type":"object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> WasmBoxedFuture<'a, Result<String, ToolError>> {
        Box::pin(async move { Ok(self.result.to_string()) })
    }
}

struct PendingTool;

impl ToolDyn for PendingTool {
    fn name(&self) -> String {
        "slow_tool".to_string()
    }

    fn definition<'a>(&'a self, _prompt: String) -> WasmBoxedFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "slow_tool".to_string(),
                description: "test tool".to_string(),
                parameters: serde_json::json!({"type":"object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> WasmBoxedFuture<'a, Result<String, ToolError>> {
        Box::pin(std::future::pending())
    }
}

#[derive(Debug, Deserialize)]
struct ToolCallRow {
    tool_name: Option<String>,
    result: Option<String>,
    lifecycle_state: Option<String>,
    await_mode: Option<String>,
    child_request_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MessageRow {
    content: String,
}

#[derive(Debug, Deserialize)]
struct WakeRequestRow {
    metadata: Option<String>,
}

async fn setup_hook(
    test_name: &str,
    registry: BackgroundToolRegistry,
) -> (support::TestDb, DefraSessionHook, String, String) {
    let db = test_db(test_name).await;
    let session_id = format!("{test_name}-session");
    let request_id = format!("{test_name}-request");
    support::create_request(
        db.node.as_ref(),
        &request_id,
        &session_id,
        "processing",
        "2026-05-14T00:00:00Z",
    )
    .await;

    let hook = DefraSessionHook::resume_or_create_with_identity_policy(
        db.node.clone(),
        &session_id,
        "r6-background",
        support::AGENT_DID,
        FailurePolicy::default(),
    )
    .await
    .unwrap()
    .with_background_tool_registry(registry);
    hook.set_active_request_id(Some(request_id.clone())).await;
    hook.set_request_deadline_at(Some(chrono::Utc::now() + chrono::Duration::seconds(5)))
        .await;
    (db, hook, session_id, request_id)
}

async fn fetch_messages(node: &EmbeddedNode, session_id: &str) -> Vec<MessageRow> {
    let session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                order: {{ sequence: ASC }}
            ) {{ content }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "fetch AgentMessage rows failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

async fn wait_for_tool_completion_message(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> MessageRow {
    let marker = format!(r#"<tool-completion tool_call_id="{tool_call_id}""#);
    for _ in 0..20 {
        if let Some(message) = fetch_messages(node, session_id)
            .await
            .into_iter()
            .find(|message| message.content.contains(&marker))
        {
            return message;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("tool completion message for {tool_call_id} was not appended");
}

async fn fetch_background_wakes(node: &EmbeddedNode, session_id: &str) -> Vec<WakeRequestRow> {
    let session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    execution_origin: {{ _eq: "scheduled" }}
                }}
            ) {{ metadata }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "fetch background wake rows failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

fn registry(tools: Vec<Box<dyn ToolDyn>>, allowlist: &[&str]) -> BackgroundToolRegistry {
    BackgroundToolRegistry::from_tools(
        tools,
        &allowlist
            .iter()
            .map(|name| name.to_string())
            .collect::<Vec<_>>(),
    )
}

fn skip_reason_json(action: ToolCallHookAction) -> Value {
    let ToolCallHookAction::Skip { reason } = action else {
        panic!("expected Skip action, got {action:?}");
    };
    serde_json::from_str(&reason).expect("skip reason should be JSON")
}

async fn load_tool_call(node: &EmbeddedNode, session_id: &str, tool_call_id: &str) -> ToolCallRow {
    let session_id = escape_graphql_string(session_id);
    let tool_call_id = escape_graphql_string(tool_call_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    tool_call_id: {{ _eq: "{tool_call_id}" }}
                }}
                limit: 1
            ) {{
                tool_name
                result
                lifecycle_state
                await_mode
                child_request_id
            }}
        }}"#
    );
    first_row(&node.execute(&query).await, "AgentToolCall")
}

async fn count_tool_calls_by_name(node: &EmbeddedNode, session_id: &str, tool_name: &str) -> usize {
    let session_id = escape_graphql_string(session_id);
    let tool_name = escape_graphql_string(tool_name);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    tool_name: {{ _eq: "{tool_name}" }}
                }}
            ) {{
                tool_call_id
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "count AgentToolCall by name failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| value.as_array())
        .map_or(0, Vec::len)
}

#[tokio::test]
async fn background_tool_success_returns_handle_and_wait_tool_returns_terminal_envelope() {
    let (db, hook, session_id, _request_id) = setup_hook(
        "r6-background-success",
        registry(
            vec![Box::new(StaticTool {
                name: "test_tool",
                result: "done",
            })],
            &["test_tool"],
        ),
    )
    .await;

    let receipt = skip_reason_json(
        PromptHook::<TestModel>::on_tool_call(
            &hook,
            "background_tool",
            None,
            "meta-bg-1",
            r#"{"tool_name":"test_tool","args":{"x":1}}"#,
        )
        .await,
    );
    assert_eq!(receipt["status"], "running");
    let tool_call_id = receipt["tool_call_id"].as_str().unwrap().to_string();

    let waited = skip_reason_json(
        PromptHook::<TestModel>::on_tool_call(
            &hook,
            "wait_tool",
            None,
            "meta-wait-1",
            &serde_json::json!({ "tool_call_id": tool_call_id }).to_string(),
        )
        .await,
    );
    assert_eq!(waited["status"], "completed");
    assert_eq!(waited["result"], "done");

    let row = load_tool_call(db.node.as_ref(), &session_id, &tool_call_id).await;
    assert_eq!(row.tool_name.as_deref(), Some("test_tool"));
    assert_eq!(row.lifecycle_state.as_deref(), Some("completed"));
    assert_eq!(row.await_mode.as_deref(), Some("background"));
    assert_eq!(row.child_request_id.as_deref(), None);
    assert_eq!(row.result.as_deref(), Some("done"));
    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "wait_tool").await,
        0
    );

    let message =
        wait_for_tool_completion_message(db.node.as_ref(), &session_id, &tool_call_id).await;
    assert!(message.content.contains(r#"tool_name="test_tool""#));
    assert!(message.content.contains(r#"status="completed""#));
    assert!(message.content.contains("<result>done</result>"));

    let wakes = fetch_background_wakes(db.node.as_ref(), &session_id).await;
    assert_eq!(wakes.len(), 1);
    let metadata: serde_json::Value =
        serde_json::from_str(wakes[0].metadata.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["queue"]["source"], "background_completion");
    assert_eq!(
        metadata["queue"]["key"],
        format!("background_completion:{session_id}")
    );
}

#[tokio::test]
async fn background_tool_rejects_not_allowlisted_target() {
    let (_db, hook, _session_id, _request_id) = setup_hook(
        "r6-background-not-allowed",
        registry(
            vec![Box::new(StaticTool {
                name: "test_tool",
                result: "done",
            })],
            &["test_tool"],
        ),
    )
    .await;

    let error = skip_reason_json(
        PromptHook::<TestModel>::on_tool_call(
            &hook,
            "background_tool",
            None,
            "meta-bg-denied",
            r#"{"tool_name":"other_tool","args":{}}"#,
        )
        .await,
    );
    assert_eq!(error["failure_class"], "tool_not_allowed");
    assert_eq!(error["requested_tool_name"], "other_tool");
}

#[tokio::test]
async fn background_tool_rejects_when_parent_budget_is_exhausted() {
    let (db, hook, session_id, _request_id) = setup_hook(
        "r6-background-budget",
        registry(vec![Box::new(PendingTool)], &["slow_tool"]),
    )
    .await;

    for index in 0..8 {
        let receipt = skip_reason_json(
            PromptHook::<TestModel>::on_tool_call(
                &hook,
                "background_tool",
                None,
                &format!("meta-bg-budget-{index}"),
                r#"{"tool_name":"slow_tool","args":{}}"#,
            )
            .await,
        );
        assert_eq!(receipt["status"], "running");
    }

    let denied = skip_reason_json(
        PromptHook::<TestModel>::on_tool_call(
            &hook,
            "background_tool",
            None,
            "meta-bg-budget-denied",
            r#"{"tool_name":"slow_tool","args":{}}"#,
        )
        .await,
    );
    assert_eq!(denied["code"], "background_tool_budget_exceeded");
    assert_eq!(denied["current_backgrounded"], 8);
    assert_eq!(denied["max_backgrounded"], 8);
    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "slow_tool").await,
        8
    );
}

#[tokio::test]
async fn wait_tool_deadline_out_cancels_background_row_without_persisting_wait_call() {
    let (db, hook, session_id, _request_id) = setup_hook(
        "r6-background-wait-deadline",
        registry(vec![Box::new(PendingTool)], &["slow_tool"]),
    )
    .await;

    let receipt = skip_reason_json(
        PromptHook::<TestModel>::on_tool_call(
            &hook,
            "background_tool",
            None,
            "meta-bg-wait-deadline",
            r#"{"tool_name":"slow_tool","args":{}}"#,
        )
        .await,
    );
    let tool_call_id = receipt["tool_call_id"].as_str().unwrap().to_string();

    hook.set_request_deadline_at(Some(chrono::Utc::now() - chrono::Duration::milliseconds(1)))
        .await;

    let waited = skip_reason_json(
        PromptHook::<TestModel>::on_tool_call(
            &hook,
            "wait_tool",
            None,
            "meta-wait-deadline",
            &serde_json::json!({ "tool_call_id": tool_call_id }).to_string(),
        )
        .await,
    );
    assert_eq!(waited["status"], "cancelled");
    assert_eq!(waited["error"]["reason"], "parent_deadline_exceeded");

    let row = load_tool_call(db.node.as_ref(), &session_id, &tool_call_id).await;
    assert_eq!(row.lifecycle_state.as_deref(), Some("cancelled"));
    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "wait_tool").await,
        0
    );
}

#[tokio::test]
async fn cancel_tool_cancels_running_background_row_without_persisting_cancel_tool_call() {
    let (db, hook, session_id, _request_id) = setup_hook(
        "r6-background-cancel",
        registry(vec![Box::new(PendingTool)], &["slow_tool"]),
    )
    .await;

    let receipt = skip_reason_json(
        PromptHook::<TestModel>::on_tool_call(
            &hook,
            "background_tool",
            None,
            "meta-bg-slow",
            r#"{"tool_name":"slow_tool","args":{}}"#,
        )
        .await,
    );
    let tool_call_id = receipt["tool_call_id"].as_str().unwrap().to_string();

    let cancelled = skip_reason_json(
        PromptHook::<TestModel>::on_tool_call(
            &hook,
            "cancel_tool",
            None,
            "meta-cancel-slow",
            &serde_json::json!({ "tool_call_id": tool_call_id }).to_string(),
        )
        .await,
    );
    assert_eq!(cancelled["status"], "cancelled");

    let row = load_tool_call(db.node.as_ref(), &session_id, &tool_call_id).await;
    assert_eq!(row.lifecycle_state.as_deref(), Some("cancelled"));
    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "cancel_tool").await,
        0
    );

    let message =
        wait_for_tool_completion_message(db.node.as_ref(), &session_id, &tool_call_id).await;
    assert!(message.content.contains(r#"status="cancelled""#));
    assert!(message.content.contains("<reason>explicit_cancel</reason>"));

    let wakes = fetch_background_wakes(db.node.as_ref(), &session_id).await;
    assert_eq!(wakes.len(), 1);
}
