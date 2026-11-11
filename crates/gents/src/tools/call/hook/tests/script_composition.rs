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
        .admit_tool_effect(
            "script",
            1,
            gents_protocol::goal_names::GET_GOAL_TOOL_NAME,
            "{}",
        )
        .await
        .unwrap();
    assert!(matches!(
        hook.on_tool_call(
            gents_protocol::goal_names::GET_GOAL_TOOL_NAME,
            None,
            &child,
            "{}"
        )
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
    let name = gents_protocol::tool_names::AGENT_NEW_TOOL_NAME;
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

struct Stall(Arc<tokio::sync::Notify>);

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

/// A selected `team/plugin` tool over a `host_call` guest, with its artifact digest.
fn host_call_tool(wat: &str) -> (tempfile::TempDir, Box<dyn ToolDyn>, String) {
    use crate::plugin::tests::executor::installed_plugin;
    let (home, mut record) = installed_plugin(wat, None);
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
    (home, Box::new(tool), record.digest)
}

async fn control(
    fixture: &super::background_budget::BudgetHookFixture,
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

async fn session_tool_rows(
    fixture: &super::background_budget::BudgetHookFixture,
) -> Vec<serde_json::Value> {
    let session = crate::store::graphql::escape_graphql_string(&fixture.session_id);
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

async fn guest_stopped(digest: &str, within: std::time::Duration) {
    tokio::time::timeout(within, async {
        while crate::plugin::live_guests::count(digest) != 0 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the guest still occupies its worker thread");
}

#[tokio::test]
async fn cancelling_a_background_script_settles_its_in_flight_child() {
    use crate::plugin::tests::executor::host_call_plugin_wat;
    let wat = host_call_plugin_wat(&json!({"tool_name":"stall", "arguments":{}}), 1);
    let (_home, tool, digest) = host_call_tool(&wat);
    let registry = BackgroundToolRegistry::from_tools(vec![tool], &["plugin".into()]);
    let executions = BackgroundExecutionRegistry::default();
    let fixture = super::background_budget::budget_hook_fixture(
        "script-cancel-child",
        registry,
        executions.clone(),
    )
    .await;
    bind_admitted_deadline(&fixture).await;
    let started = Arc::new(tokio::sync::Notify::new());
    fixture
        .hook
        .bind_tool_surface(Arc::new(vec![Box::new(Stall(started.clone()))]))
        .await;
    let receipt = control(
        &fixture,
        "start-script",
        "spawn_process",
        json!({"tool_name":"plugin", "args":{}}),
    )
    .await;
    let id = receipt["tool_call_id"].as_str().unwrap().to_owned();
    tokio::time::timeout(std::time::Duration::from_secs(30), started.notified())
        .await
        .expect("the script reached its child");
    let rows = session_tool_rows(&fixture).await;
    let child = rows.iter().find(|row| row["tool_name"] == "stall").unwrap();
    assert_eq!(child["lifecycle_state"], "running", "{rows:?}");

    let cancelled = control(
        &fixture,
        "cancel-script",
        "cancel_process",
        json!({"tool_call_id": id}),
    )
    .await;
    assert_eq!(cancelled["ok"], true, "{cancelled}");
    executions.wait_for_completion(&id).await;
    guest_stopped(&digest, std::time::Duration::from_secs(2)).await;

    let rows = session_tool_rows(&fixture).await;
    let child = rows.iter().find(|row| row["tool_name"] == "stall").unwrap();
    assert_eq!(child["lifecycle_state"], "cancelled", "{rows:?}");
    assert!(
        rows.iter()
            .all(|row| !matches!(row["lifecycle_state"].as_str(), Some("pending" | "running"))),
        "every row of the stopped script is terminal: {rows:?}"
    );
    fixture.db.node.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_stopped_invocation_times_out_the_child_it_left_running() {
    let fixture = super::background_budget::budget_hook_fixture(
        "script-timeout-child",
        Default::default(),
        Default::default(),
    )
    .await;
    bind_admitted_deadline(&fixture).await;
    let hook = &fixture.hook;
    accept_hook_tool_call(hook, "script", "plugin", "{}", None).await;
    assert!(matches!(
        hook.on_tool_call("plugin", None, "script", "{}").await,
        ToolCallHookAction::Continue
    ));
    let pending = hook
        .admit_tool_effect("script", 1, "stall", "{}")
        .await
        .unwrap();
    let running = hook
        .admit_tool_effect("script", 2, "stall", "{}")
        .await
        .unwrap();
    assert!(matches!(
        hook.on_tool_call("stall", None, &running, "{}").await,
        ToolCallHookAction::Continue
    ));
    hook.settle_tool_effects(
        "script",
        &crate::tools::call::lifecycle::ToolOutcome::TimedOut { deadline_at: None },
    )
    .await
    .unwrap();
    for (child, state) in [(&pending, "cancelled"), (&running, "timedOut")] {
        let row =
            super::background_budget::tool_row(&fixture.access, &fixture.session_id, child).await;
        assert_eq!(row["lifecycle_state"], state, "{child}");
    }
    assert!(hook.effect_lifecycles.lock().await.is_empty());
    assert!(hook.effect_tool_calls.lock().await.is_empty());
    fixture.db.node.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_script_spawned_by_a_script_cannot_call_tools() {
    use crate::plugin::tests::executor::host_call_plugin_wat;
    // Every generation runs the same guest: it asks to spawn itself again.
    let wat = host_call_plugin_wat(
        &json!({"tool_name":"spawn_process", "arguments":{"tool_name":"plugin", "args":{}}}),
        1,
    );
    let (_home, tool, _digest) = host_call_tool(&wat);
    let registry = BackgroundToolRegistry::from_tools(vec![tool], &["plugin".into()]);
    let executions = BackgroundExecutionRegistry::default();
    let fixture = super::background_budget::budget_hook_fixture(
        "script-recursion",
        registry,
        executions.clone(),
    )
    .await;
    bind_admitted_deadline(&fixture).await;
    fixture
        .hook
        .bind_tool_surface(Arc::new(crate::tools::native::build_background_tools(
            crate::tools::surface::BackgroundToolConfig {
                allowlist: vec!["plugin".into()],
                timeouts: Default::default(),
            },
        )))
        .await;
    let receipt = control(
        &fixture,
        "start-script",
        "spawn_process",
        json!({"tool_name":"plugin", "args":{}}),
    )
    .await;
    let outer = receipt["tool_call_id"].as_str().unwrap().to_owned();
    executions.wait_for_completion(&outer).await;
    let rows = session_tool_rows(&fixture).await;
    let outer_doc = rows
        .iter()
        .find(|row| row["tool_call_id"] == outer.as_str())
        .unwrap()["_docID"]
        .clone();
    let inner = rows
        .iter()
        .find(|row| row["tool_name"] == "plugin" && row["tool_call_id"] != outer.as_str())
        .unwrap_or_else(|| panic!("the script spawned its own kind once: {rows:?}"));
    let inner_id = inner["tool_call_id"].as_str().unwrap().to_owned();
    executions.wait_for_completion(&inner_id).await;

    let rows = session_tool_rows(&fixture).await;
    assert_eq!(
        rows.iter()
            .filter(|row| row["tool_name"] == "plugin")
            .count(),
        2,
        "the spawned script spawned nothing: {rows:?}"
    );
    assert!(
        rows.iter()
            .filter(|row| !row["plugin_parent_tool_call_doc_id"].is_null())
            .all(|row| row["plugin_parent_tool_call_doc_id"] == outer_doc),
        "only the model-started script composed: {rows:?}"
    );
    let inner = rows
        .iter()
        .find(|row| row["tool_call_id"] == inner_id.as_str())
        .unwrap();
    assert_eq!(inner["lifecycle_state"], "completed", "{rows:?}");
    let timeline = crate::observe::run_timeline_fetch::load_session_tool_calls(
        &fixture.access,
        fixture.db.node_identity.did(),
        &fixture.session_id,
        Some(fixture.user.did()),
    )
    .await
    .unwrap();
    let rendered = timeline
        .iter()
        .find(|call| call.tool_call_id == inner_id)
        .and_then(|call| call.result.clone())
        .unwrap_or_default();
    assert!(
        rendered.contains("tool access is disabled for this invocation"),
        "{rendered}"
    );
    fixture.db.node.shutdown().await.unwrap();
}

fn spinning_wat(name: &str, host_calls: bool) -> String {
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

async fn spawn_spinner(
    name: &str,
    host_calls: bool,
) -> (
    super::background_budget::BudgetHookFixture,
    BackgroundExecutionRegistry,
    String,
    String,
    tempfile::TempDir,
) {
    let (home, tool, digest) = host_call_tool(&spinning_wat(name, host_calls));
    let registry = BackgroundToolRegistry::from_tools(vec![tool], &["plugin".into()]);
    let executions = BackgroundExecutionRegistry::default();
    let fixture =
        super::background_budget::budget_hook_fixture(name, registry, executions.clone()).await;
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
        while crate::plugin::live_guests::count(&digest) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the guest started");
    (fixture, executions, id, digest, home)
}

#[tokio::test]
async fn cancelling_a_background_script_stops_a_guest_that_ignores_the_host() {
    // The guest keeps calling the host and never checks the answer, so only
    // the store's revoked allowance ends it.
    let (fixture, executions, id, digest, _home) = spawn_spinner("script-cancel-guest", true).await;
    let cancelled = control(
        &fixture,
        "cancel-script",
        "cancel_process",
        json!({"tool_call_id": id}),
    )
    .await;
    assert_eq!(cancelled["ok"], true, "{cancelled}");
    executions.wait_for_completion(&id).await;
    guest_stopped(&digest, std::time::Duration::from_secs(2)).await;
    let row = super::background_budget::tool_row(&fixture.access, &fixture.session_id, &id).await;
    assert_eq!(row["lifecycle_state"], "cancelled");
    fixture.db.node.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_computing_background_guest_stops_at_its_declared_allowance() {
    // No host call ever reaches the store, so the declared wall clock (the
    // five second default here), not the admitted background lifetime of
    // hours, is what ends `while (true) {}`.
    let (fixture, executions, id, digest, _home) = spawn_spinner("script-spin-guest", false).await;
    let cancelled = control(
        &fixture,
        "cancel-script",
        "cancel_process",
        json!({"tool_call_id": id}),
    )
    .await;
    assert_eq!(cancelled["ok"], true, "{cancelled}");
    executions.wait_for_completion(&id).await;
    guest_stopped(&digest, std::time::Duration::from_secs(20)).await;
    fixture.db.node.shutdown().await.unwrap();
}

/// A foreground script with one child in each state a stopped script can leave.
async fn script_with_children(
    name: &str,
) -> (super::background_budget::BudgetHookFixture, [String; 4]) {
    let fixture =
        super::background_budget::budget_hook_fixture(name, Default::default(), Default::default())
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
        &crate::tools::call::lifecycle::ToolOutcome::Completed("{\"ordinal\":1}".into()),
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
        Some(&crate::tools::call::lifecycle::ToolOutcome::Cancelled),
    )
    .await
    .unwrap();
    (fixture, [completed, pending, running, cancelled])
}

async fn timeline(
    fixture: &super::background_budget::BudgetHookFixture,
) -> Vec<crate::observe::run_timeline::TimelineToolCallRow> {
    crate::observe::run_timeline_fetch::load_session_tool_calls(
        &fixture.access,
        fixture.db.node_identity.did(),
        &fixture.session_id,
        Some(fixture.user.did()),
    )
    .await
    .expect("one script call never fails the session timeline")
}

#[tokio::test]
async fn the_timeline_reads_a_script_child_in_every_state() {
    let (fixture, [completed, pending, running, cancelled]) =
        script_with_children("script-timeline-states").await;
    let rows = timeline(&fixture).await;
    for (child, status, has_result) in [
        (&completed, "completed", true),
        (&pending, "pending", false),
        (&running, "running", false),
        (&cancelled, "cancelled", true),
    ] {
        let row = rows
            .iter()
            .find(|row| &row.tool_call_id == child)
            .unwrap_or_else(|| panic!("{status} child is on the timeline"));
        assert_eq!(row.status, status);
        assert_eq!(row.tool_name, "probe");
        assert_eq!(row.args, "{}", "{status}");
        assert_eq!(
            row.result.is_some(),
            has_result,
            "{status}: {:?}",
            row.result
        );
        assert_eq!(row.payload_unavailable, None, "{status}");
    }
    // A script that stopped with children in flight: they settle and still read.
    fixture
        .hook
        .settle_tool_effects(
            "script",
            &crate::tools::call::lifecycle::ToolOutcome::Cancelled,
        )
        .await
        .unwrap();
    let rows = timeline(&fixture).await;
    for child in [&pending, &running] {
        let row = rows.iter().find(|row| &row.tool_call_id == child).unwrap();
        assert_eq!(row.status, "cancelled");
        assert_eq!(row.payload_unavailable, None);
    }
    fixture.db.node.shutdown().await.unwrap();
}

#[tokio::test]
async fn one_unreadable_script_child_is_marked_and_the_timeline_still_loads() {
    let (fixture, [completed, pending, _running, _cancelled]) =
        script_with_children("script-timeline-unreadable").await;
    // The child's recorded identity no longer matches its canonical arguments.
    let row =
        super::background_budget::tool_row(&fixture.access, &fixture.session_id, &pending).await;
    let changed = fixture
        .access
        .write(
            "test.corrupt_script_child",
            &format!(
                r#"mutation {{ update_AgentToolCall(docID: "{}", input: {{ tool_name: "renamed" }}) {{ _docID }} }}"#,
                crate::store::graphql::escape_graphql_string(row["_docID"].as_str().unwrap()),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        changed["data"]["update_AgentToolCall"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    let rows = timeline(&fixture).await;
    let unreadable = rows.iter().find(|row| row.tool_call_id == pending).unwrap();
    assert_eq!(unreadable.status, "pending");
    assert!(unreadable.payload_unavailable.is_some());
    assert!(unreadable.args.is_empty() && unreadable.result.is_none());
    let readable = rows
        .iter()
        .find(|row| row.tool_call_id == completed)
        .unwrap();
    assert_eq!(readable.payload_unavailable, None);
    assert!(readable.result.is_some());
    fixture.db.node.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_background_request_on_a_script_child_is_refused() {
    let (fixture, [_completed, _pending, running, _cancelled]) =
        script_with_children("script-child-background-control").await;
    let refusal = crate::tools::background::status::request_foreground_backgrounding(
        &fixture.access,
        fixture.db.node_identity.did(),
        &fixture.session_id,
        &fixture.request_id,
        &running,
    )
    .await
    .expect_err("only the model's own Bash call can move to the background");
    assert!(
        format!("{refusal:#}").contains("only a direct native Bash command can be backgrounded"),
        "{refusal:#}"
    );
    let row =
        super::background_budget::tool_row(&fixture.access, &fixture.session_id, &running).await;
    assert_eq!(row["await_mode"], "foreground");
    assert_eq!(row["lifecycle_state"], "running");
    fixture.db.node.shutdown().await.unwrap();
}
