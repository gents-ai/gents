use std::sync::Arc;

use gents_inference::llm::ToolCallHookAction;
use gents_store::identity::NodeIdentity;
use serde_json::json;

use gents_toolcalls::call::hook::fixtures::*;
use gents_toolcalls::call::hook::*;

use gents_loop::SessionHook;
use gents_toolcalls::test_support::script_composition::*;
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
    assert_eq!(
        guest_allowances(&digest).await,
        [std::time::Duration::from_secs(5)],
        "the guest computes under its declared allowance"
    );
    let cancelled = control(
        &fixture,
        "cancel-script",
        "cancel_process",
        json!({"tool_call_id": id}),
    )
    .await;
    // Nothing orders the cancel before the allowance (TLA
    // `MCGuestAllowanceCancelLate`): a late cancel reports the ended process.
    assert!(
        cancelled["ok"] == true
            || matches!(cancelled["status"].as_str(), Some(status) if status != "running" && status != "pending"),
        "{cancelled}"
    );
    executions.wait_for_completion(&id).await;
    // The allowance is counted in wake-ups of the WASM host's epoch ticker,
    // which a loaded host delays without bound (Lean `allowanceTicks`), so
    // the stop is awaited, not timed.
    guest_released(&digest).await;
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
