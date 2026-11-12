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

#[tokio::test]
async fn a_stopped_invocation_times_out_the_child_it_left_running() {
    let fixture = gents_toolcalls::test_support::background_hook::budget_hook_fixture(
        "script-timeout-child",
        Default::default(),
        Default::default(),
        gents_automation::lifecycle_hooks::composition(),
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
