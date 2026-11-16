use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use gents_store::identity::NodeIdentity;
use serde_json::json;

use gents_toolcalls::call::hook::fixtures::accept_hook_tool_call;
use gents_toolcalls::call::hook::*;

use gents_loop::SessionHook;
use gents_observe::test_support::script_composition::timeline;
use gents_toolcalls::test_support::script_composition::*;
#[tokio::test]
async fn a_script_spawned_by_a_script_cannot_call_tools() {
    use gents_toolcalls::test_support::host_call_plugin_wat;
    // Every generation runs the same guest: it asks to spawn itself again.
    let wat = host_call_plugin_wat(
        &json!({"tool_name":"spawn_process", "arguments":{"tool_name":"plugin", "args":{}}}),
        1,
    );
    let (_home, tool, _digest) = host_call_tool(&wat);
    let registry = BackgroundToolRegistry::from_tools(vec![tool], &["plugin".into()]);
    let executions = BackgroundExecutionRegistry::default();
    let fixture = gents_toolcalls::test_support::background_hook::budget_hook_fixture(
        "script-recursion",
        registry,
        executions.clone(),
        gents_automation::lifecycle_hooks::composition(),
    )
    .await;
    bind_admitted_deadline(&fixture).await;
    fixture
        .hook
        .bind_tool_surface(Arc::new(gents_tools::native::build_background_tools(
            gents_tools::surface::BackgroundToolConfig {
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
    let timeline = gents_observe::observe::run_timeline_fetch::load_session_tool_calls(
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

#[tokio::test]
async fn the_timeline_reads_a_script_child_in_every_state() {
    let (fixture, [completed, pending, running, cancelled]) = script_with_children(
        "script-timeline-states",
        gents_automation::lifecycle_hooks::composition(),
    )
    .await;
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
            &gents_loop::tool_call_lifecycle::ToolOutcome::Cancelled,
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
    let (fixture, [completed, pending, _running, _cancelled]) = script_with_children(
        "script-timeline-unreadable",
        gents_automation::lifecycle_hooks::composition(),
    )
    .await;
    // The child's recorded identity no longer matches its canonical arguments.
    let row = gents_toolcalls::test_support::background_hook::tool_row(
        &fixture.access,
        &fixture.session_id,
        &pending,
    )
    .await;
    let changed = fixture
        .access
        .write(
            "test.corrupt_script_child",
            &format!(
                r#"mutation {{ update_AgentToolCall(docID: "{}", input: {{ tool_name: "renamed" }}) {{ _docID }} }}"#,
                gents_store::graphql::escape_graphql_string(row["_docID"].as_str().unwrap()),
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
