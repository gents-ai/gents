use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::llm::tool::BoxFuture;
use gents::llm::tool::ToolDefinition;
use gents::llm::tool::{ToolDyn, ToolError};
use gents::llm::ToolCallHookAction;
use gents::{BackgroundToolRegistry, DefraSessionHook, FailurePolicy};
use serde::Deserialize;
use serde_json::Value;

use crate::support::{first_row, test_db};

struct StaticTool {
    name: &'static str,
    result: &'static str,
}

impl ToolDyn for StaticTool {
    fn name(&self) -> String {
        self.name.to_string()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async move {
            ToolDefinition {
                name: self.name.to_string(),
                description: "test tool".to_string(),
                parameters: serde_json::json!({"type":"object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move { Ok(self.result.to_string()) })
    }
}

struct LargeOutputTool {
    name: &'static str,
    output: String,
}

impl ToolDyn for LargeOutputTool {
    fn name(&self) -> String {
        self.name.to_string()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async move {
            ToolDefinition {
                name: self.name.to_string(),
                description: "test tool".to_string(),
                parameters: serde_json::json!({"type":"object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        let output = self.output.clone();
        Box::pin(async move { Ok(output) })
    }
}

struct PendingTool;

impl ToolDyn for PendingTool {
    fn name(&self) -> String {
        "slow_tool".to_string()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "slow_tool".to_string(),
                description: "test tool".to_string(),
                parameters: serde_json::json!({"type":"object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(std::future::pending())
    }
}

#[derive(Debug, Deserialize)]
struct ToolCallRow {
    tool_name: Option<String>,
    result: Option<String>,
    lifecycle_state: Option<String>,
    cancel_cause: Option<String>,
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
) -> (crate::support::TestDb, DefraSessionHook, String, String) {
    let db = test_db(test_name).await;
    let session_id = format!("{test_name}-session");
    let request_id = format!("{test_name}-request");
    crate::support::create_request(
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
        crate::support::AGENT_DID,
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

async fn wait_for_background_wakes(
    node: &EmbeddedNode,
    session_id: &str,
    expected_count: usize,
) -> Vec<WakeRequestRow> {
    for _ in 0..20 {
        let wakes = fetch_background_wakes(node, session_id).await;
        if wakes.len() >= expected_count {
            return wakes;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("expected at least {expected_count} background wake rows for session {session_id}");
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
                cancel_cause
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
        hook.on_tool_call(
            "spawn_process",
            None,
            "meta-bg-1",
            r#"{"tool_name":"test_tool","args":{"x":1}}"#,
        )
        .await,
    );
    assert_eq!(receipt["status"], "running");
    let tool_call_id = receipt["tool_call_id"].as_str().unwrap().to_string();

    let waited = skip_reason_json(
        hook.on_tool_call(
            "wait_process",
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
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "wait_process").await,
        0
    );

    let message =
        wait_for_tool_completion_message(db.node.as_ref(), &session_id, &tool_call_id).await;
    assert!(message.content.contains(r#"tool_name="test_tool""#));
    assert!(message.content.contains(r#"status="completed""#));
    assert!(message.content.contains("<result>done</result>"));

    let wakes = wait_for_background_wakes(db.node.as_ref(), &session_id, 1).await;
    assert_eq!(wakes.len(), 1);
    let metadata: serde_json::Value =
        serde_json::from_str(wakes[0].metadata.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["queue"]["source"], "background_completion");
    assert_eq!(
        metadata["queue"]["key"],
        format!("background_completion:{session_id}")
    );
}

// #985: a backgrounded bash run's lifetime budget is decoupled from both the
// parent request deadline and the foreground command ceiling — the execution
// must complete (and notify) even though the parent deadline expires while it
// is still running.
#[tokio::test]
async fn background_tool_execution_survives_parent_request_deadline() {
    let bash_tools = gents::ToolSet::builder()
        .bash_read_only_with_policy_and_timeout(
            gents::CommandExecutionPolicy::read_only(vec!["sleep".to_string()]),
            std::time::Duration::from_secs(120),
        )
        .build()
        .build_native_tools()
        .unwrap();
    let (db, hook, session_id, _request_id) = setup_hook(
        "r6-background-outlives-deadline",
        registry(bash_tools, &["bash"]),
    )
    .await;
    hook.set_request_deadline_at(Some(
        chrono::Utc::now() + chrono::Duration::milliseconds(200),
    ))
    .await;

    let receipt = skip_reason_json(
        hook.on_tool_call(
            "spawn_process",
            None,
            "meta-bg-outlive",
            r#"{"tool_name":"bash","args":{"command":"sleep","args":["0.7"]}}"#,
        )
        .await,
    );
    assert_eq!(receipt["status"], "running");
    let tool_call_id = receipt["tool_call_id"].as_str().unwrap().to_string();

    // The tool outlives both the parent deadline (200ms) and the shared
    // 500ms wait helper; poll long enough for the 700ms sleep to finish.
    let marker = format!(r#"<tool-completion tool_call_id="{tool_call_id}""#);
    let mut message = None;
    for _ in 0..60 {
        if let Some(found) = fetch_messages(db.node.as_ref(), &session_id)
            .await
            .into_iter()
            .find(|message| message.content.contains(&marker))
        {
            message = Some(found);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let message = message.expect("background tool completion message was not appended");
    assert!(
        message.content.contains(r#"status="completed""#),
        "background tool must outlive the parent request deadline; got: {}",
        message.content
    );

    let row = load_tool_call(db.node.as_ref(), &session_id, &tool_call_id).await;
    assert_eq!(row.lifecycle_state.as_deref(), Some("completed"));
}

// #985: wait_process is a bounded wait — on timeout it reports the process
// as still running without cancelling it, so a model that waits cannot pin
// the session (or kill the job) until the parent request deadline.
#[tokio::test]
async fn wait_process_bounded_wait_returns_still_running_without_cancelling() {
    let (db, hook, session_id, _request_id) = setup_hook(
        "r6-background-wait-bounded",
        registry(vec![Box::new(PendingTool)], &["slow_tool"]),
    )
    .await;

    let receipt = skip_reason_json(
        hook.on_tool_call(
            "spawn_process",
            None,
            "meta-bg-bounded",
            r#"{"tool_name":"slow_tool","args":{}}"#,
        )
        .await,
    );
    assert_eq!(receipt["status"], "running");
    let tool_call_id = receipt["tool_call_id"].as_str().unwrap().to_string();

    let started = std::time::Instant::now();
    let waited = skip_reason_json(
        hook.on_tool_call(
            "wait_process",
            None,
            "meta-wait-bounded",
            &serde_json::json!({ "tool_call_id": tool_call_id, "timeout_secs": 1 }).to_string(),
        )
        .await,
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(4),
        "bounded wait must return promptly, took {:?}",
        started.elapsed()
    );
    assert_eq!(waited["status"], "running");
    assert_eq!(waited["error"]["reason"], "wait_timeout");

    let row = load_tool_call(db.node.as_ref(), &session_id, &tool_call_id).await;
    assert_eq!(
        row.lifecycle_state.as_deref(),
        Some("running"),
        "wait timeout must not cancel the background process"
    );
    assert_eq!(row.cancel_cause.as_deref(), None);
}

#[tokio::test]
async fn wait_envelope_bounds_oversized_background_tool_result() {
    let big_line = "x".repeat(200);
    let big_output = std::iter::repeat(big_line)
        .take(5_000)
        .collect::<Vec<_>>()
        .join("\n");
    let full_len = big_output.len();
    let (db, hook, session_id, _request_id) = setup_hook(
        "r6-background-bounded",
        registry(
            vec![Box::new(LargeOutputTool {
                name: "big_tool",
                output: big_output,
            })],
            &["big_tool"],
        ),
    )
    .await;

    let receipt = skip_reason_json(
        hook.on_tool_call(
            "spawn_process",
            None,
            "meta-bg-big",
            r#"{"tool_name":"big_tool","args":{}}"#,
        )
        .await,
    );
    assert_eq!(receipt["status"], "running");
    let tool_call_id = receipt["tool_call_id"].as_str().unwrap().to_string();

    let waited = skip_reason_json(
        hook.on_tool_call(
            "wait_process",
            None,
            "meta-wait-big",
            &serde_json::json!({ "tool_call_id": tool_call_id }).to_string(),
        )
        .await,
    );
    assert_eq!(waited["status"], "completed");
    let envelope_result = waited["result"].as_str().expect("envelope result string");
    assert!(
        envelope_result.len() < full_len,
        "wait envelope must bound the model-facing result: envelope={} full={}",
        envelope_result.len(),
        full_len
    );
    assert!(
        !envelope_result.is_empty(),
        "bounded result must be non-empty"
    );

    let row = load_tool_call(db.node.as_ref(), &session_id, &tool_call_id).await;
    assert_eq!(row.lifecycle_state.as_deref(), Some("completed"));
    assert_eq!(
        row.result.as_deref().map(str::len),
        Some(full_len),
        "the AgentToolCall row must keep the full output"
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
        hook.on_tool_call(
            "spawn_process",
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
            hook.on_tool_call(
                "spawn_process",
                None,
                &format!("meta-bg-budget-{index}"),
                r#"{"tool_name":"slow_tool","args":{}}"#,
            )
            .await,
        );
        assert_eq!(receipt["status"], "running");
    }

    let denied = skip_reason_json(
        hook.on_tool_call(
            "spawn_process",
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
        hook.on_tool_call(
            "spawn_process",
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
        hook.on_tool_call(
            "wait_process",
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
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "wait_process").await,
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
        hook.on_tool_call(
            "spawn_process",
            None,
            "meta-bg-slow",
            r#"{"tool_name":"slow_tool","args":{}}"#,
        )
        .await,
    );
    let tool_call_id = receipt["tool_call_id"].as_str().unwrap().to_string();

    let cancelled = skip_reason_json(
        hook.on_tool_call(
            "cancel_process",
            None,
            "meta-cancel-slow",
            &serde_json::json!({ "tool_call_id": tool_call_id }).to_string(),
        )
        .await,
    );
    assert_eq!(cancelled["status"], "cancelled");

    let row = load_tool_call(db.node.as_ref(), &session_id, &tool_call_id).await;
    assert_eq!(row.lifecycle_state.as_deref(), Some("cancelled"));
    assert_eq!(row.cancel_cause.as_deref(), Some("userCancelled"));
    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "cancel_process").await,
        0
    );

    let message =
        wait_for_tool_completion_message(db.node.as_ref(), &session_id, &tool_call_id).await;
    assert!(message.content.contains(r#"status="cancelled""#));
    assert!(message.content.contains("<reason>explicit_cancel</reason>"));

    let wakes = wait_for_background_wakes(db.node.as_ref(), &session_id, 1).await;
    assert_eq!(wakes.len(), 1);
}

#[tokio::test]
async fn cancel_tool_unknown_handle_returns_tool_error_instead_of_failing_turn() {
    let (_db, hook, _session_id, _request_id) =
        setup_hook("r6-background-cancel-missing", registry(Vec::new(), &[])).await;

    let cancelled = skip_reason_json(
        hook.on_tool_call(
            "cancel_process",
            None,
            "meta-cancel-missing",
            r#"{"tool_call_id":"missing-background-handle"}"#,
        )
        .await,
    );

    assert_eq!(cancelled["ok"], false);
    assert_eq!(cancelled["tool_name"], "cancel_process");
    assert!(cancelled["message"]
        .as_str()
        .unwrap()
        .contains("missing-background-handle"));
}

#[tokio::test]
async fn wait_tool_unknown_handle_returns_tool_error_instead_of_failing_turn() {
    let (_db, hook, _session_id, _request_id) =
        setup_hook("r6-background-wait-missing", registry(Vec::new(), &[])).await;

    let waited = skip_reason_json(
        hook.on_tool_call(
            "wait_process",
            None,
            "meta-wait-missing",
            r#"{"tool_call_id":"missing-background-handle"}"#,
        )
        .await,
    );

    assert_eq!(waited["ok"], false);
    assert_eq!(waited["tool_name"], "wait_process");
    assert!(waited["message"]
        .as_str()
        .unwrap()
        .contains("missing-background-handle"));
}
