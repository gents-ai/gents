use super::*;
use crate::inference::llm::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};
use gents_loop::SessionHook;

struct Probe(Arc<AtomicU64>);

async fn bind_admitted_deadline(fixture: &super::background_budget::BudgetHookFixture) {
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

#[tokio::test]
async fn background_plugin_keeps_the_selected_tool_surface() {
    use crate::plugin::tests::executor::{host_call_plugin_wat, installed_plugin};
    let wat = host_call_plugin_wat(&json!({"tool_name":"probe", "arguments":{}}), 3);
    let (home, mut record) = installed_plugin(&wat, None);
    record.declaration.host_imports = vec![crate::pack::HostImport::HostCall];
    crate::plugin::store::write_record(home.path(), &record).unwrap();
    let executor = Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
        home.path().to_owned(),
    )));
    let selected = crate::documents::PluginToolRef {
        plugin: "team/plugin".into(),
        digest: None,
        tool_calls: true,
        input_fields: Vec::new(),
        job: None,
    };
    let tool = crate::plugin::tool::PluginTool::resolve(executor, &selected, None).unwrap();
    let registry = BackgroundToolRegistry::from_tools(vec![Box::new(tool)], &["plugin".into()]);
    let fixture = super::background_budget::budget_hook_fixture(
        "script-background",
        registry,
        Default::default(),
    )
    .await;
    bind_admitted_deadline(&fixture).await;
    let count = Arc::new(AtomicU64::new(0));
    fixture
        .hook
        .bind_tool_surface(Arc::new(vec![Box::new(Probe(count.clone()))]))
        .await;
    let args = json!({"tool_name":"plugin", "args":{}}).to_string();
    accept_hook_tool_call(&fixture.hook, "start-script", "spawn_process", &args, None).await;
    let ToolCallHookAction::Skip { reason } = fixture
        .hook
        .on_tool_call("spawn_process", None, "start-script", &args)
        .await
    else {
        panic!("spawn did not return its receipt")
    };
    let receipt: serde_json::Value = serde_json::from_str(&reason).unwrap();
    let id = receipt["tool_call_id"].as_str().unwrap();
    let wait = json!({"tool_call_id":id, "timeout_secs":30}).to_string();
    accept_hook_tool_call(&fixture.hook, "wait-script", "wait_process", &wait, None).await;
    let ToolCallHookAction::Skip { reason } = fixture
        .hook
        .on_tool_call("wait_process", None, "wait-script", &wait)
        .await
    else {
        panic!("wait did not return")
    };
    assert_eq!(count.load(Ordering::SeqCst), 3, "{reason}");
    let row = super::background_budget::tool_row(&fixture.access, &fixture.session_id, id).await;
    assert_eq!(row["lifecycle_state"], "completed", "{reason}");
    let timeline = crate::observe::run_timeline_fetch::load_session_tool_calls(
        &fixture.access,
        fixture.db.node_identity.did(),
        &fixture.session_id,
        Some(fixture.user.did()),
    )
    .await
    .unwrap();
    let probes: Vec<_> = timeline
        .iter()
        .filter(|call| call.tool_name == "probe")
        .collect();
    assert_eq!(probes.len(), 3);
    assert!(probes.iter().all(|call| call.result.is_some()));
    fixture.db.node.shutdown().await.unwrap();
}

#[tokio::test]
async fn goal_tool_uses_its_owner_when_composed() {
    let fixture = super::background_budget::budget_hook_fixture(
        "script-goal",
        Default::default(),
        Default::default(),
    )
    .await;
    bind_admitted_deadline(&fixture).await;
    let hook = fixture.hook.with_goal_tool_authority(true, false);
    accept_hook_tool_call(&hook, "script", "plugin", "{}", None).await;
    assert!(matches!(
        hook.on_tool_call("plugin", None, "script", "{}").await,
        ToolCallHookAction::Continue
    ));
    let child = hook
        .admit_tool_effect("script", 1, crate::goal::GET_GOAL_TOOL_NAME, "{}")
        .await
        .unwrap();
    assert!(matches!(
        hook.on_tool_call(crate::goal::GET_GOAL_TOOL_NAME, None, &child, "{}")
            .await,
        ToolCallHookAction::Skip { .. }
    ));
    hook.finish_tool_effect(&child, None).await.unwrap();
    fixture.db.node.shutdown().await.unwrap();
}

#[tokio::test]
async fn composed_agent_new_keeps_its_asynchronous_receipt() {
    let fixture = super::background_budget::budget_hook_fixture(
        "script-agent-new",
        Default::default(),
        Default::default(),
    )
    .await;
    bind_admitted_deadline(&fixture).await;
    super::session_message_commit::enable_agents_tools_with_access(
        &fixture.owner,
        fixture.db.node_identity.did(),
        fixture.agent.did(),
    )
    .await;
    let hook = &fixture.hook;
    accept_hook_tool_call(hook, "script", "plugin", "{}", None).await;
    assert!(matches!(
        hook.on_tool_call("plugin", None, "script", "{}").await,
        ToolCallHookAction::Continue
    ));
    let name = crate::tools::native::AGENT_NEW_TOOL_NAME;
    let args = json!({"agent":"general", "prompt":"work"}).to_string();
    let child = hook
        .admit_tool_effect("script", 1, name, &args)
        .await
        .unwrap();
    let ToolCallHookAction::Skip { reason } = hook.on_tool_call(name, None, &child, &args).await
    else {
        panic!("agent_new did not return a receipt")
    };
    let receipt: serde_json::Value = serde_json::from_str(&reason).unwrap();
    assert_eq!(receipt["ok"], true, "{reason}");
    hook.finish_tool_effect(&child, None).await.unwrap();
    let row =
        super::background_budget::tool_row(&fixture.access, &fixture.session_id, &child).await;
    assert_eq!(row["lifecycle_state"], "running");
    assert_eq!(row["await_mode"], "background");
    let result = crate::tools::call::lifecycle::query::load_tool_call_result(
        &fixture.access,
        row["_docID"].as_str().unwrap(),
        fixture.db.node_identity.did(),
        &fixture.session_id,
        Some(fixture.user.did()),
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            &crate::tools::call::lifecycle::query::render_tool_result(&result).unwrap()
        )
        .unwrap(),
        receipt
    );
    fixture.db.node.shutdown().await.unwrap();
}
