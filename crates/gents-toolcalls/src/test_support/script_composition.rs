use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use gents_inference::llm::ToolCallHookAction;
use serde_json::json;

use crate::call::hook::fixtures::*;
use crate::call::hook::*;

use gents_loop::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};
use gents_loop::SessionHook;

pub struct Probe(pub Arc<AtomicU64>);

pub async fn bind_admitted_deadline(
    fixture: &crate::test_support::background_hook::BudgetHookFixture,
) {
    let deadline = hook_execution_fixtures().lock().await
        [&hook_execution_fixture_key(&fixture.hook, &fixture.request_id)]
        .lifecycle
        .claimed_deadline_at()
        .unwrap();
    fixture.hook.state.lock().await.request_deadline_at = Some(deadline);
}

impl ToolDyn for Probe {
    fn name(&self) -> String {
        "probe".into()
    }
    fn definition<'a>(&'a self, _: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "probe".into(),
                description: "probe".into(),
                parameters: json!({"type":"object"}),
            }
        })
    }
    fn call<'a>(&'a self, _: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            Ok(json!({"ordinal": self.0.fetch_add(1, Ordering::SeqCst) + 1}).to_string())
        })
    }
}

pub struct Stall(pub Arc<tokio::sync::Notify>);

impl ToolDyn for Stall {
    fn name(&self) -> String {
        "stall".into()
    }
    fn definition<'a>(&'a self, _: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "stall".into(),
                description: "never answers".into(),
                parameters: json!({"type":"object"}),
            }
        })
    }
    fn call<'a>(&'a self, _: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            self.0.notify_one();
            std::future::pending().await
        })
    }
}

pub fn host_call_tool(wat: &str) -> (tempfile::TempDir, Box<dyn ToolDyn>, String) {
    use crate::test_support::installed_plugin;
    let (home, mut record) = installed_plugin(wat, None);
    record.declaration.host_imports = vec![gents_pack::pack::HostImport::HostCall];
    gents_pack::plugin::store::write_record(home.path(), &record).unwrap();
    let executor = Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
        home.path().to_owned(),
    )));
    let selected = gents_documents::PluginToolRef {
        plugin: "team/plugin".into(),
        digest: None,
        tool_calls: true,
        input_fields: Vec::new(),
        job: None,
    };
    let tool = crate::plugin::tool::PluginTool::resolve(executor, &selected, None).unwrap();
    (home, Box::new(tool), record.digest)
}

pub async fn control(
    fixture: &crate::test_support::background_hook::BudgetHookFixture,
    internal_id: &str,
    tool: &str,
    args: serde_json::Value,
) -> serde_json::Value {
    let args = args.to_string();
    accept_hook_tool_call(&fixture.hook, internal_id, tool, &args, None).await;
    let ToolCallHookAction::Skip { reason } = fixture
        .hook
        .on_tool_call(tool, None, internal_id, &args)
        .await
    else {
        panic!("{tool} did not return its result")
    };
    serde_json::from_str(&reason).unwrap_or(serde_json::Value::String(reason))
}

pub async fn session_tool_rows(
    fixture: &crate::test_support::background_hook::BudgetHookFixture,
) -> Vec<serde_json::Value> {
    let session = gents_store::graphql::escape_graphql_string(&fixture.session_id);
    let response = fixture
        .access
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session}" }} }}) {{
                _docID tool_call_id tool_name lifecycle_state await_mode
                plugin_parent_tool_call_doc_id spawned_by_tool_call_doc_id }} }}"#
        ))
        .await
        .unwrap();
    response["data"]["AgentToolCall"]
        .as_array()
        .unwrap()
        .clone()
}

pub async fn guest_stopped(digest: &str, within: std::time::Duration) {
    tokio::time::timeout(within, async {
        while crate::plugin::host_calls::live::count(digest) != 0 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the guest still occupies its worker thread");
}

pub fn spinning_wat(name: &str, host_calls: bool) -> String {
    let request = json!({"sleep_ms": 1}).to_string();
    let escaped = request
        .bytes()
        .map(|b| format!("\\{b:02x}"))
        .collect::<String>();
    let body = if host_calls {
        // The inner call is a function entry, where compiled code rereads
        // the store's deadline; a script interpreter enters functions constantly.
        format!(
            "(loop $again (drop (call $call (i32.const 32) (i32.const {}))) (call $enter) (br $again))",
            request.len()
        )
    } else {
        "(loop $again (br $again))".to_owned()
    };
    format!(
        r#"(module
      (import "gents" "host_call" (func $call (param i32 i32) (result i32)))
      (memory (export "memory") 2)
      (data (i32.const 32) "{escaped}")
      (data (i32.const 1024) "{name}")
      (func $enter)
      (func (export "_start") {body}))"#
    )
}

pub async fn spawn_spinner(
    name: &str,
    host_calls: bool,
    composition: gents_store::node_extension::Extensions,
) -> (
    crate::test_support::background_hook::BudgetHookFixture,
    BackgroundExecutionRegistry,
    String,
    String,
    tempfile::TempDir,
) {
    let (home, tool, digest) = host_call_tool(&spinning_wat(name, host_calls));
    let registry = BackgroundToolRegistry::from_tools(vec![tool], &["plugin".into()]);
    let executions = BackgroundExecutionRegistry::default();
    let fixture = crate::test_support::background_hook::budget_hook_fixture(
        name,
        registry,
        executions.clone(),
        composition,
    )
    .await;
    bind_admitted_deadline(&fixture).await;
    fixture.hook.bind_tool_surface(Arc::new(Vec::new())).await;
    let receipt = control(
        &fixture,
        "start-script",
        "spawn_process",
        json!({"tool_name":"plugin", "args":{}}),
    )
    .await;
    let id = receipt["tool_call_id"].as_str().unwrap().to_owned();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while crate::plugin::host_calls::live::count(&digest) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the guest started");
    (fixture, executions, id, digest, home)
}

pub async fn script_with_children(
    name: &str,
    composition: gents_store::node_extension::Extensions,
) -> (
    crate::test_support::background_hook::BudgetHookFixture,
    [String; 4],
) {
    let fixture = crate::test_support::background_hook::budget_hook_fixture(
        name,
        Default::default(),
        Default::default(),
        composition,
    )
    .await;
    bind_admitted_deadline(&fixture).await;
    let hook = &fixture.hook;
    accept_hook_tool_call(hook, "script", "plugin", "{}", None).await;
    assert!(matches!(
        hook.on_tool_call("plugin", None, "script", "{}").await,
        ToolCallHookAction::Continue
    ));
    let completed = hook
        .admit_tool_effect("script", 1, "probe", "{}")
        .await
        .unwrap();
    assert!(matches!(
        hook.on_tool_call("probe", None, &completed, "{}").await,
        ToolCallHookAction::Continue
    ));
    hook.on_tool_result(
        "probe",
        None,
        &completed,
        "{}",
        &gents_loop::tool_call_lifecycle::ToolOutcome::Completed("{\"ordinal\":1}".into()),
    )
    .await;
    hook.finish_tool_effect(&completed, None).await.unwrap();
    let pending = hook
        .admit_tool_effect("script", 2, "probe", "{}")
        .await
        .unwrap();
    let running = hook
        .admit_tool_effect("script", 3, "probe", "{}")
        .await
        .unwrap();
    assert!(matches!(
        hook.on_tool_call("probe", None, &running, "{}").await,
        ToolCallHookAction::Continue
    ));
    let cancelled = hook
        .admit_tool_effect("script", 4, "probe", "{}")
        .await
        .unwrap();
    assert!(matches!(
        hook.on_tool_call("probe", None, &cancelled, "{}").await,
        ToolCallHookAction::Continue
    ));
    hook.finish_tool_effect(
        &cancelled,
        Some(&gents_loop::tool_call_lifecycle::ToolOutcome::Cancelled),
    )
    .await
    .unwrap();
    (fixture, [completed, pending, running, cancelled])
}
