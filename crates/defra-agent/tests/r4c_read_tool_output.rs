//! Integration tests for R4c read_tool_output.

mod support;

use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::tool_call_lifecycle::ToolCallLifecycle;
use defra_agent::{BackgroundToolRegistry, DefraSessionHook, FailurePolicy};
use rig::agent::{PromptHook, ToolCallHookAction};
use rig::completion::ToolDefinition;
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse};
use rig::streaming::StreamingCompletionResponse;
use rig::tool::{ToolDyn, ToolError};
use rig::wasm_compat::WasmBoxedFuture;
use serde_json::{json, Value};

use support::{test_db, AGENT_DID};

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
            "completion is unused in R4c read_tool_output tests".to_string(),
        ))
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        Err(CompletionError::ProviderError(
            "streaming is unused in R4c read_tool_output tests".to_string(),
        ))
    }
}

struct StaticTool {
    name: &'static str,
    result: String,
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
                parameters: json!({"type":"object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> WasmBoxedFuture<'a, Result<String, ToolError>> {
        Box::pin(async move { Ok(self.result.clone()) })
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
                parameters: json!({"type":"object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> WasmBoxedFuture<'a, Result<String, ToolError>> {
        Box::pin(std::future::pending())
    }
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
        "r4c-read-tool-output",
        AGENT_DID,
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

async fn setup_hook_on_db(
    db: &support::TestDb,
    request_id: &str,
    session_id: &str,
    registry: BackgroundToolRegistry,
) -> (DefraSessionHook, String, String) {
    support::create_request(
        db.node.as_ref(),
        request_id,
        session_id,
        "processing",
        "2026-05-14T00:00:00Z",
    )
    .await;
    let hook = DefraSessionHook::resume_or_create_with_identity_policy(
        db.node.clone(),
        session_id,
        "r4c-read-tool-output",
        AGENT_DID,
        FailurePolicy::default(),
    )
    .await
    .unwrap()
    .with_background_tool_registry(registry);
    hook.set_active_request_id(Some(request_id.to_string()))
        .await;
    hook.set_request_deadline_at(Some(chrono::Utc::now() + chrono::Duration::seconds(5)))
        .await;
    (hook, session_id.to_string(), request_id.to_string())
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

async fn background_tool(
    hook: &DefraSessionHook,
    internal_call_id: &str,
    tool_name: &str,
) -> Value {
    skip_reason_json(
        PromptHook::<TestModel>::on_tool_call(
            hook,
            "background_tool",
            Some(format!("model-{internal_call_id}")),
            internal_call_id,
            &json!({"tool_name": tool_name, "args": {}}).to_string(),
        )
        .await,
    )
}

async fn wait_tool(hook: &DefraSessionHook, internal_call_id: &str, tool_call_id: &str) -> Value {
    skip_reason_json(
        PromptHook::<TestModel>::on_tool_call(
            hook,
            "wait_tool",
            Some(format!("model-{internal_call_id}")),
            internal_call_id,
            &json!({ "tool_call_id": tool_call_id }).to_string(),
        )
        .await,
    )
}

async fn read_tool_output(hook: &DefraSessionHook, internal_call_id: &str, args: Value) -> Value {
    skip_reason_json(
        PromptHook::<TestModel>::on_tool_call(
            hook,
            "read_tool_output",
            Some(format!("model-{internal_call_id}")),
            internal_call_id,
            &args.to_string(),
        )
        .await,
    )
}

fn skip_reason_json(action: ToolCallHookAction) -> Value {
    let ToolCallHookAction::Skip { reason } = action else {
        panic!("expected Skip action, got {action:?}");
    };
    serde_json::from_str(&reason).expect("skip reason should be JSON")
}

async fn create_foreground_tool_call(
    db: &support::TestDb,
    request_id: &str,
    session_id: &str,
) -> String {
    let tool_call_id = "foreground-call".to_string();
    let mut lifecycle = ToolCallLifecycle::new(
        db.node.clone(),
        request_id.to_string(),
        session_id.to_string(),
        tool_call_id.clone(),
        99,
        "foreground_tool".to_string(),
        "{}".to_string(),
        chrono::Utc::now() + chrono::Duration::minutes(5),
    );
    lifecycle.start_running().await.unwrap();
    lifecycle.complete("foreground result").await.unwrap();
    tool_call_id
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
            ) {{ _docID }}
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
        .and_then(|rows| rows.as_array())
        .map_or(0, Vec::len)
}

#[tokio::test]
async fn read_tool_output_running_returns_empty_live_stream_without_ring_buffer() {
    let (_db, hook, _session_id, _request_id) = setup_hook(
        "r4c-read-output-running",
        registry(vec![Box::new(PendingTool)], &["slow_tool"]),
    )
    .await;
    let handle = background_tool(&hook, "bg-running", "slow_tool").await;
    let tool_call_id = handle["tool_call_id"].as_str().unwrap();

    let result = read_tool_output(
        &hook,
        "read-running",
        json!({ "tool_call_id": tool_call_id }),
    )
    .await;
    assert_eq!(result["status"].as_str(), Some("running"));
    assert_eq!(result["tool_name"].as_str(), Some("slow_tool"));
    assert_eq!(result["stdout"]["bytes"].as_str(), Some(""));
    assert_eq!(result["stdout"]["truncated"].as_bool(), Some(false));
    assert_eq!(result["stdout"]["total_bytes_seen"].as_u64(), Some(0));
    assert!(result["exit_code"].is_null());
}

#[tokio::test]
async fn read_tool_output_terminal_reads_persisted_result() {
    let (_db, hook, _session_id, _request_id) = setup_hook(
        "r4c-read-output-terminal",
        registry(
            vec![Box::new(StaticTool {
                name: "complete_tool",
                result: "done\n".to_string(),
            })],
            &["complete_tool"],
        ),
    )
    .await;
    let handle = background_tool(&hook, "bg-terminal", "complete_tool").await;
    let tool_call_id = handle["tool_call_id"].as_str().unwrap();
    let waited = wait_tool(&hook, "wait-terminal", tool_call_id).await;
    assert_eq!(waited["status"].as_str(), Some("completed"));

    let result = read_tool_output(
        &hook,
        "read-terminal",
        json!({ "tool_call_id": tool_call_id }),
    )
    .await;
    assert_eq!(result["status"].as_str(), Some("completed"));
    assert_eq!(result["stdout"]["bytes"].as_str(), Some("done\n"));
    assert_eq!(result["stdout"]["total_bytes_seen"].as_u64(), Some(5));
    assert!(result["exit_code"].is_null());
}

#[tokio::test]
async fn read_tool_output_terminal_parses_native_command_streams() {
    let persisted = concat!(
        "defra_exec: {\"ok\":false,\"status\":\"exit_nonzero\",",
        "\"command\":\"grep -P foo README.md\",\"argv\":[\"grep\",\"-P\",\"foo\",\"README.md\"],",
        "\"cwd\":\".\",\"exit_code\":2,\"timed_out\":false,\"duration_ms\":4,",
        "\"timeout_ms\":10000,\"execution_mode\":\"read_only\",",
        "\"network_mode\":\"inherit\",\"sandbox\":\"policy_read_only\",",
        "\"stdout_truncation\":{\"returned_chars\":7,\"total_chars\":7,",
        "\"max_chars\":16000,\"truncated\":false},",
        "\"stderr_truncation\":{\"returned_chars\":25,\"total_chars\":25,",
        "\"max_chars\":16000,\"truncated\":false}}\n",
        "stdout:\n",
        "matches\n",
        "stderr:\n",
        "grep: invalid option -- P"
    );
    let (_db, hook, _session_id, _request_id) = setup_hook(
        "r4c-read-output-native",
        registry(
            vec![Box::new(StaticTool {
                name: "bash",
                result: persisted.to_string(),
            })],
            &["bash"],
        ),
    )
    .await;
    let handle = background_tool(&hook, "bg-native", "bash").await;
    let tool_call_id = handle["tool_call_id"].as_str().unwrap();
    let waited = wait_tool(&hook, "wait-native", tool_call_id).await;
    assert_eq!(waited["status"].as_str(), Some("completed"));

    let result = read_tool_output(
        &hook,
        "read-native",
        json!({ "tool_call_id": tool_call_id }),
    )
    .await;
    assert_eq!(result["stdout"]["bytes"].as_str(), Some("matches"));
    assert_eq!(result["stdout"]["total_bytes_seen"].as_u64(), Some(7));
    assert_eq!(
        result["stderr"]["bytes"].as_str(),
        Some("grep: invalid option -- P")
    );
    assert_eq!(result["stderr"]["total_bytes_seen"].as_u64(), Some(25));
    assert_eq!(result["exit_code"].as_i64(), Some(2));
}

#[tokio::test]
async fn read_tool_output_truncated_flag_on_overflow() {
    let large = format!("{}tail", "prefix".repeat(60));
    let (_db, hook, _session_id, _request_id) = setup_hook(
        "r4c-read-output-truncate",
        registry(
            vec![Box::new(StaticTool {
                name: "large_tool",
                result: large.clone(),
            })],
            &["large_tool"],
        ),
    )
    .await;
    let handle = background_tool(&hook, "bg-large", "large_tool").await;
    let tool_call_id = handle["tool_call_id"].as_str().unwrap();
    let waited = wait_tool(&hook, "wait-large", tool_call_id).await;
    assert_eq!(waited["status"].as_str(), Some("completed"));

    let result = read_tool_output(
        &hook,
        "read-large",
        json!({
            "tool_call_id": tool_call_id,
            "max_bytes_per_stream": 256
        }),
    )
    .await;
    assert_eq!(result["stdout"]["truncated"].as_bool(), Some(true));
    assert_eq!(
        result["stdout"]["total_bytes_seen"].as_u64(),
        Some(large.len() as u64)
    );
    assert_eq!(result["stdout"]["bytes"].as_str().unwrap().len(), 256);
    assert!(result["stdout"]["bytes"]
        .as_str()
        .unwrap()
        .ends_with("tail"));
    assert_ne!(result["stdout"]["bytes"].as_str().unwrap(), &large[..256]);
}

#[tokio::test]
async fn read_tool_output_rejects_non_backgrounded() {
    let (db, hook, session_id, request_id) = setup_hook(
        "r4c-read-output-foreground",
        registry(vec![Box::new(PendingTool)], &["slow_tool"]),
    )
    .await;
    let foreground_call_id = create_foreground_tool_call(&db, &request_id, &session_id).await;

    let result = read_tool_output(
        &hook,
        "read-foreground",
        json!({ "tool_call_id": foreground_call_id }),
    )
    .await;
    assert_eq!(result["ok"].as_bool(), Some(false));
    assert_eq!(result["failure_class"].as_str(), Some("argument_invalid"));
}

#[tokio::test]
async fn read_tool_output_rejects_unauthorized() {
    let db = test_db("r4c-read-output-unauthorized").await;
    let (hook_1, _session_1, _request_1) = setup_hook_on_db(
        &db,
        "parent-one",
        "session-one",
        registry(vec![Box::new(PendingTool)], &["slow_tool"]),
    )
    .await;
    let (hook_2, _session_2, _request_2) = setup_hook_on_db(
        &db,
        "parent-two",
        "session-two",
        registry(vec![Box::new(PendingTool)], &["slow_tool"]),
    )
    .await;
    let handle = background_tool(&hook_2, "sibling-bg", "slow_tool").await;
    let tool_call_id = handle["tool_call_id"].as_str().unwrap();

    let result = read_tool_output(
        &hook_1,
        "read-unauthorized",
        json!({ "tool_call_id": tool_call_id }),
    )
    .await;
    assert_eq!(result["ok"].as_bool(), Some(false));
    assert_eq!(result["failure_class"].as_str(), Some("tool_not_allowed"));
}

#[tokio::test]
async fn read_tool_output_no_parent_tool_call_row_written() {
    let (db, hook, session_id, _request_id) = setup_hook(
        "r4c-read-output-no-row",
        registry(vec![Box::new(PendingTool)], &["slow_tool"]),
    )
    .await;
    let handle = background_tool(&hook, "bg-no-row", "slow_tool").await;
    let tool_call_id = handle["tool_call_id"].as_str().unwrap();
    let _ = read_tool_output(
        &hook,
        "read-no-row",
        json!({ "tool_call_id": tool_call_id }),
    )
    .await;

    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "read_tool_output").await,
        0
    );
}
