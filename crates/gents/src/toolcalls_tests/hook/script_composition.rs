use super::*;
use gents_loop::SessionHook;
use gents_toolcalls::test_support::script_composition::*;
#[tokio::test]
async fn background_plugin_keeps_the_selected_tool_surface() {
    use gents_toolcalls::test_support::{host_call_plugin_wat, installed_plugin};
    let wat = host_call_plugin_wat(&json!({"tool_name":"probe", "arguments":{}}), 3);
    let (home, mut record) = installed_plugin(&wat, None);
    record.declaration.host_imports = vec![gents_pack::pack::HostImport::HostCall];
    gents_pack::plugin::store::write_record(home.path(), &record).unwrap();
    let executor = Arc::new(gents_toolcalls::plugin::executor::PluginExecutor::new(
        Some(home.path().to_owned()),
    ));
    let selected = gents_documents::PluginToolRef {
        plugin: "team/plugin".into(),
        digest: None,
        tool_calls: true,
        input_fields: Vec::new(),
        job: None,
    };
    let tool =
        gents_toolcalls::plugin::tool::PluginTool::resolve(executor, &selected, None).unwrap();
    let registry = BackgroundToolRegistry::from_tools(vec![Box::new(tool)], &["plugin".into()]);
    let fixture = gents_toolcalls::test_support::background_hook::budget_hook_fixture(
        "script-background",
        registry,
        Default::default(),
        gents_automation::lifecycle_hooks::composition(),
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
    let row = gents_toolcalls::test_support::background_hook::tool_row(
        &fixture.access,
        &fixture.session_id,
        id,
    )
    .await;
    assert_eq!(row["lifecycle_state"], "completed", "{reason}");
    let timeline = gents_observe::observe::run_timeline_fetch::load_session_tool_calls(
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
async fn composed_agent_new_keeps_its_asynchronous_receipt() {
    let fixture = gents_toolcalls::test_support::background_hook::budget_hook_fixture(
        "script-agent-new",
        Default::default(),
        Default::default(),
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    bind_admitted_deadline(&fixture).await;
    gents_toolcalls::test_support::enable_agents_tools_with_access(
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
    let row = gents_toolcalls::test_support::background_hook::tool_row(
        &fixture.access,
        &fixture.session_id,
        &child,
    )
    .await;
    assert_eq!(row["lifecycle_state"], "running");
    assert_eq!(row["await_mode"], "background");
    let result = gents_session::tool_call::query::result::load_tool_call_result(
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
            &gents_session::tool_call::query::result::render_tool_result(&result).unwrap()
        )
        .unwrap(),
        receipt
    );
    fixture.db.node.shutdown().await.unwrap();
}
#[tokio::test]
async fn cancelling_a_background_script_settles_its_in_flight_child() {
    use gents_toolcalls::test_support::host_call_plugin_wat;
    let wat = host_call_plugin_wat(&json!({"tool_name":"stall", "arguments":{}}), 1);
    let (_home, tool, digest) = host_call_tool(&wat);
    let registry = BackgroundToolRegistry::from_tools(vec![tool], &["plugin".into()]);
    let executions = BackgroundExecutionRegistry::default();
    let fixture = gents_toolcalls::test_support::background_hook::budget_hook_fixture(
        "script-cancel-child",
        registry,
        executions.clone(),
        gents_session::test_support::no_lifecycle_plugins(),
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
    let fixture = gents_toolcalls::test_support::background_hook::budget_hook_fixture(
        "script-timeout-child",
        Default::default(),
        Default::default(),
        gents_session::test_support::no_lifecycle_plugins(),
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
        &gents_loop::tool_call_lifecycle::ToolOutcome::TimedOut { deadline_at: None },
    )
    .await
    .unwrap();
    for (child, state) in [(&pending, "cancelled"), (&running, "timedOut")] {
        let row = gents_toolcalls::test_support::background_hook::tool_row(
            &fixture.access,
            &fixture.session_id,
            child,
        )
        .await;
        assert_eq!(row["lifecycle_state"], state, "{child}");
    }
    assert!(hook.effect_lifecycles.lock().await.is_empty());
    assert!(hook.effect_tool_calls.lock().await.is_empty());
    fixture.db.node.shutdown().await.unwrap();
}
#[tokio::test]
async fn cancelling_a_background_script_stops_a_guest_that_ignores_the_host() {
    // The guest keeps calling the host and never checks the answer, so only
    // the store's revoked allowance ends it.
    let (fixture, executions, id, digest, _home) = spawn_spinner(
        "script-cancel-guest",
        true,
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
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
    let row = gents_toolcalls::test_support::background_hook::tool_row(
        &fixture.access,
        &fixture.session_id,
        &id,
    )
    .await;
    assert_eq!(row["lifecycle_state"], "cancelled");
    fixture.db.node.shutdown().await.unwrap();
}
#[tokio::test]
async fn a_computing_background_guest_stops_at_its_declared_allowance() {
    // No host call ever reaches the store, so the declared wall clock (the
    // five second default here), not the admitted background lifetime of
    // hours, is what ends `while (true) {}`.
    let (fixture, executions, id, digest, _home) = spawn_spinner(
        "script-spin-guest",
        false,
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
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
#[tokio::test]
async fn a_background_request_on_a_script_child_is_refused() {
    let (fixture, [_completed, _pending, running, _cancelled]) = script_with_children(
        "script-child-background-control",
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let refusal = gents_toolcalls::background::status::request_foreground_backgrounding(
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
    let row = gents_toolcalls::test_support::background_hook::tool_row(
        &fixture.access,
        &fixture.session_id,
        &running,
    )
    .await;
    assert_eq!(row["await_mode"], "foreground");
    assert_eq!(row["lifecycle_state"], "running");
    fixture.db.node.shutdown().await.unwrap();
}

#[tokio::test]
async fn goal_tool_uses_its_owner_when_composed() {
    let fixture = gents_toolcalls::test_support::background_hook::budget_hook_fixture(
        "script-goal",
        Default::default(),
        Default::default(),
        gents_automation::lifecycle_hooks::composition(),
    )
    .await;
    bind_admitted_deadline(&fixture).await;
    let hook = fixture
        .hook
        .with_goal_tool_authority(true, false)
        .with_goal_tools(Arc::new(
            gents_automation::goal::goal_tools::GoalToolAnswers,
        ));
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
