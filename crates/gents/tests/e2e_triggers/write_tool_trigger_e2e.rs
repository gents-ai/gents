//! Task B6 — capstone e2e for the `write_tools` primitive.
//!
//! This proves the two required properties of the steward emit->trigger loop:
//!
//!   1. **Multi-field templating** — a Task `prompt_template` that references
//!      SEVERAL fields of the triggering doc (`{{ doc.drift_sig }}`,
//!      `{{ doc.summary }}`, `{{ doc.target_paths }}`) renders ALL of them from
//!      the doc that fired the trigger. (Single-field render was already shown by
//!      `event_trigger_e2e.rs`; B6 must show multi-field — "verify report data
//!      gets pulled into the template properly".)
//!
//!   2. **A declared `BoundedWriteTool`'s output is a valid trigger source** — a
//!      `request_action -> ActionRequest` write tool, constructed exactly as the
//!      runtime constructs it (B3/B4), when its `.call()` writes an
//!      `ActionRequest` doc, fires an `EventTrigger` on `ActionRequest`. The
//!      tool's write is therefore a real, trigger-driving source-doc event.
//!
//! ## Why the deterministic-split shape (NO path), not a live tool call
//!
//! B6's plan asks whether the test harness can script a model completion that
//! returns an assistant message carrying a `tool_calls` entry, so a LIVE agent
//! could be driven to invoke `request_action` end to end. It cannot:
//!
//!   - `crate::support::mock_endpoint::MockModelEndpoint` only answers `GET /models`
//!     for the startup health probe; it drives no inference at all.
//!   - `crate::support::streaming_backend::MockStreamingBackend` streams scripted
//!     CONTENT chunks, but every SSE delta it emits hard-codes
//!     `"tool_calls": []` (see `write_sse_chunk`) — it has no way to script an
//!     assistant tool call.
//!   - The R4 subagent-tool tests (`tests/r4_subagent_tools.rs`) drive tool
//!     execution by hand-constructing a `rig` `ToolCall`/`ToolFunction` and
//!     feeding it through the session hook's `on_tool_call`, with a `TestModel`
//!     whose `completion`/`stream` both error — i.e. they bypass the model
//!     entirely rather than scripting one to emit a tool call.
//!
//! So there is no supported way to make a real model emit a tool call in Plan 1.
//! We therefore prove the integration deterministically in two parts: Test 1
//! drives the trigger via a direct source-doc write (multi-field render), and
//! Test 2 drives it via the write tool's own `.call()` against the same live
//! node (write-tool output fires the trigger). A LIVE model invoking the tool is
//! qualified in Plan 2 against d4f; B6 proves the tool's write fires triggers and
//! that multi-field templating renders, both deterministically.

use std::sync::Arc;
use std::time::Duration;

use defra_agent::defra_node::EmbeddedNode;
use defra_agent::defra_write::BoundedWriteTool;
use defra_agent::document_config::{WriteToolDecl, WriteToolField};
use defra_agent::graphql::escape_graphql_string;
use defra_agent::llm::tool::Tool;
use defra_agent::{AgentIdentity, DefraAgent, DocumentRuntimeOptions, ToolCeiling};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::support::fixtures::{bind_default_behavior_backend, test_identity};
use crate::support::mock_endpoint::MockModelEndpoint;
use crate::support::snapshots::{fetch_runtime_snapshot, RuntimeSnapshot};
use crate::support::test_db;

// Shared across both tests: an `ActionRequest` source collection (the steward
// loop's emit target) and a Task whose template pulls THREE of its fields.
const DRIFT_SIG: &str = "drift:host-doc:9f2c";
const SUMMARY: &str = "studio-1 host doc is stale vs runtime";
const TARGET_PATHS: &str = "infra/hosts/studio-1/host.md";
const PROMPT_TEMPLATE: &str =
    "drift={{ doc.drift_sig }} summary={{ doc.summary }} paths={{ doc.target_paths }}";

/// Register the `ActionRequest` source collection BEFORE seeding triggers — the
/// EventSource introspects its fields to hydrate `doc.*`, and the filter probe
/// needs the indexed field present. `status` is indexed so an operator filter on
/// it would pass DefraDB's limit-1 probe.
async fn register_action_request_schema(node: &EmbeddedNode) {
    let sdl = r#"
        type ActionRequest {
            drift_sig: String
            summary: String
            target_paths: String
            status: String @index
        }
    "#;
    node.add_schema(sdl)
        .await
        .expect("add_schema for ActionRequest");
}

async fn create_task(node: &EmbeddedNode, task_id: &str, behavior_id: &str, prompt_template: &str) {
    let escaped_task_id = escape_graphql_string(task_id);
    let escaped_behavior_id = escape_graphql_string(behavior_id);
    let escaped_prompt_template = escape_graphql_string(prompt_template);
    let mutation = format!(
        r#"mutation {{
            create_Task(input: {{
                task_id: "{escaped_task_id}",
                name: "{escaped_task_id}",
                behavior_id: "{escaped_behavior_id}",
                prompt_template: "{escaped_prompt_template}",
                enabled: true
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create Task failed: {:?}",
        response.errors
    );
}

async fn create_event_trigger(
    node: &EmbeddedNode,
    trigger_id: &str,
    task_id: &str,
    source_collection: &str,
    event_kind: &str,
) {
    let escaped_trigger_id = escape_graphql_string(trigger_id);
    let escaped_task_id = escape_graphql_string(task_id);
    let escaped_source_collection = escape_graphql_string(source_collection);
    let escaped_event_kind = escape_graphql_string(event_kind);
    let mutation = format!(
        r#"mutation {{
            create_EventTrigger(input: {{
                trigger_id: "{escaped_trigger_id}",
                task_id: "{escaped_task_id}",
                source_collection: "{escaped_source_collection}",
                event_kind: "{escaped_event_kind}",
                enabled: true,
                concurrency: "serial",
                fire_count: 0
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create EventTrigger failed: {:?}",
        response.errors
    );
}

async fn wait_for_runtime_snapshot<F>(
    node: &EmbeddedNode,
    agent_did: &str,
    predicate: F,
) -> RuntimeSnapshot
where
    F: Fn(&RuntimeSnapshot) -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(snapshot) = fetch_runtime_snapshot(node, agent_did).await {
            if predicate(&snapshot) {
                return snapshot;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for runtime snapshot for {agent_did}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[derive(Debug, Deserialize)]
struct AgentRequestRow {
    request_id: String,
    content: String,
    caused_by_trigger_id: Option<String>,
    caused_by_trigger_kind: Option<String>,
    execution_origin: Option<String>,
}

async fn query_agent_requests_for_trigger(
    node: &EmbeddedNode,
    trigger_id: &str,
) -> Vec<AgentRequestRow> {
    let escaped_trigger_id = escape_graphql_string(trigger_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ caused_by_trigger_id: {{ _eq: "{escaped_trigger_id}" }} }}
            ) {{
                request_id
                content
                caused_by_trigger_id
                caused_by_trigger_kind
                execution_origin
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "AgentRequest query failed: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    rows.into_iter()
        .map(|row| serde_json::from_value(row).expect("decode AgentRequest row"))
        .collect()
}

/// Poll until exactly one trigger-driven `AgentRequest` lands, then return it.
async fn wait_for_one_agent_request(node: &EmbeddedNode, trigger_id: &str) -> AgentRequestRow {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut requests = loop {
        let rows = query_agent_requests_for_trigger(node, trigger_id).await;
        if !rows.is_empty() {
            break rows;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for AgentRequest caused_by_trigger_id={trigger_id}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        requests.len(),
        1,
        "expected exactly one AgentRequest from a single matching source doc, got {}: {:?}",
        requests.len(),
        requests
    );
    requests.remove(0)
}

/// Assert the rendered prompt substituted ALL THREE referenced doc fields. This
/// is the multi-field requirement: not just one `{{ doc.* }}`, but the whole set.
fn assert_multi_field_render(request: &AgentRequestRow, trigger_id: &str) {
    assert_eq!(
        request.caused_by_trigger_id.as_deref(),
        Some(trigger_id),
        "request caused_by_trigger_id mismatch: {request:?}"
    );
    assert_eq!(
        request.caused_by_trigger_kind.as_deref(),
        Some("event"),
        "request caused_by_trigger_kind must be 'event': {request:?}"
    );
    assert_eq!(
        request.execution_origin.as_deref(),
        Some("scheduled"),
        "trigger-driven fires persist execution_origin=scheduled: {request:?}"
    );
    let expected = format!("drift={DRIFT_SIG} summary={SUMMARY} paths={TARGET_PATHS}");
    assert_eq!(
        request.content, expected,
        "rendered prompt must substitute ALL of drift_sig/summary/target_paths from the \
         triggering doc: {request:?}"
    );
    // Belt-and-suspenders: each field individually present in the render.
    assert!(
        request.content.contains(DRIFT_SIG),
        "render missing drift_sig: {request:?}"
    );
    assert!(
        request.content.contains(SUMMARY),
        "render missing summary: {request:?}"
    );
    assert!(
        request.content.contains(TARGET_PATHS),
        "render missing target_paths: {request:?}"
    );
    assert!(
        !request.request_id.is_empty(),
        "request_id must be populated: {request:?}"
    );
}

/// Boot a live `DefraAgent` with the `ActionRequest` Task + EventTrigger seeded
/// and reconciled into the active snapshot. Returns the running agent's handle,
/// its shutdown sender, and its DID. The source schema must already be
/// registered on the node before calling.
async fn boot_agent_with_action_trigger(
    db: &crate::support::TestDb,
    test_name: &str,
    task_id: &str,
    trigger_id: &str,
) -> (
    tokio::task::JoinHandle<anyhow::Result<()>>,
    tokio::sync::watch::Sender<bool>,
) {
    let identity = Arc::new(test_identity(test_name));
    let mock_endpoint = MockModelEndpoint::start("default").unwrap();
    bind_default_behavior_backend(
        db.node.as_ref(),
        identity.did(),
        &format!("backend-{test_name}"),
        mock_endpoint.endpoint(),
    )
    .await;

    let agent = DefraAgent::from_default_behavior_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let agent_did = agent.agent_did().to_string();
    let default_behavior_id = agent.default_behavior_id().to_string();

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));

    // Startup reconcile baseline.
    let startup = wait_for_runtime_snapshot(db.node.as_ref(), &agent_did, |snapshot| {
        snapshot.process_state == "ready"
            && snapshot.reconcile_phase == "idle"
            && snapshot.active_generation >= 1
            && snapshot.last_reconcile_result == "startup"
    })
    .await;
    let initial_generation = startup.active_generation;
    assert!(
        startup.last_reconcile_error.is_empty(),
        "startup reconcile should be clean, got error={:?}",
        startup.last_reconcile_error
    );

    // Seed Task + EventTrigger on ActionRequest with the multi-field template.
    create_task(
        db.node.as_ref(),
        task_id,
        &default_behavior_id,
        PROMPT_TEMPLATE,
    )
    .await;
    create_event_trigger(
        db.node.as_ref(),
        trigger_id,
        task_id,
        "ActionRequest",
        "created",
    )
    .await;

    // Wait for the control watcher to reconcile the new trigger into the active
    // snapshot and subscribe the EventSource to ActionRequest.
    let reconciled = wait_for_runtime_snapshot(db.node.as_ref(), &agent_did, |snapshot| {
        snapshot.process_state == "ready"
            && snapshot.reconcile_phase == "idle"
            && snapshot.active_generation > initial_generation
            && snapshot.last_reconcile_result == "applied"
    })
    .await;
    assert!(
        reconciled.last_reconcile_error.is_empty(),
        "post-insert reconcile should be clean, got error={:?}",
        reconciled.last_reconcile_error
    );

    (handle, shutdown_tx)
}

/// The `request_action -> ActionRequest` declaration the runtime would build a
/// `BoundedWriteTool` from (mirrors `src/defra_write/tests.rs::decl`, extended
/// with the `target_paths` field the multi-field template references).
fn request_action_decl() -> WriteToolDecl {
    WriteToolDecl {
        tool_name: "request_action".into(),
        collection: "ActionRequest".into(),
        description: "Emit one ActionRequest describing observed drift.".into(),
        fields: vec![
            WriteToolField {
                name: "drift_sig".into(),
                required: true,
            },
            WriteToolField {
                name: "summary".into(),
                required: true,
            },
            WriteToolField {
                name: "target_paths".into(),
                required: true,
            },
            WriteToolField {
                name: "status".into(),
                required: false,
            },
        ],
    }
}

/// Test 1 (multi-field templating, REQUIRED): a direct `ActionRequest` write
/// fires the EventTrigger and the rendered `AgentRequest.content` substitutes
/// ALL THREE referenced doc fields (drift_sig, summary, target_paths).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_field_template_renders_all_referenced_doc_fields() {
    let db = test_db("write-tool-trigger-multifield").await;
    register_action_request_schema(db.node.as_ref()).await;

    let task_id = "task-b6-multifield";
    let trigger_id = "trigger-b6-multifield";
    let (handle, shutdown_tx) =
        boot_agent_with_action_trigger(&db, "b6-multifield", task_id, trigger_id).await;

    // Direct source-doc write (no tool): drives the trigger so we can isolate
    // the multi-field render assertion.
    let escaped_drift = escape_graphql_string(DRIFT_SIG);
    let escaped_summary = escape_graphql_string(SUMMARY);
    let escaped_paths = escape_graphql_string(TARGET_PATHS);
    let mutation = format!(
        r#"mutation {{
            add_ActionRequest(input: {{
                drift_sig: "{escaped_drift}",
                summary: "{escaped_summary}",
                target_paths: "{escaped_paths}",
                status: "open"
            }}) {{ _docID }}
        }}"#
    );
    let resp = db.node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "add_ActionRequest failed: {:?}",
        resp.errors
    );

    let request = wait_for_one_agent_request(db.node.as_ref(), trigger_id).await;
    assert_multi_field_render(&request, trigger_id);

    let _ = shutdown_tx.send(true);
    handle.await.unwrap().unwrap();
}

/// Test 2 (write-tool output is a valid trigger source, REQUIRED): construct the
/// `request_action` `BoundedWriteTool` exactly as the runtime does, against the
/// SAME live node whose EventTrigger is subscribed to `ActionRequest`, call
/// `.call()` with concrete args, and assert the doc the tool wrote fires the
/// trigger — an `AgentRequest` materializes with the multi-field render. This
/// proves the write tool's output integrates with the trigger machinery.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declared_write_tool_call_fires_event_trigger() {
    let db = test_db("write-tool-trigger-tooldriven").await;
    register_action_request_schema(db.node.as_ref()).await;

    let task_id = "task-b6-tooldriven";
    let trigger_id = "trigger-b6-tooldriven";
    let (handle, shutdown_tx) =
        boot_agent_with_action_trigger(&db, "b6-tooldriven", task_id, trigger_id).await;

    // Build the write tool the runtime would build from the decl, bound to the
    // SAME node the EventTrigger is live on, and invoke it.
    let tool = BoundedWriteTool::new(db.node.clone(), request_action_decl());
    assert_eq!(
        Tool::name(&tool),
        "request_action",
        "tool advertises its declared name"
    );
    let out = Tool::call(
        &tool,
        serde_json::from_value(json!({
            "drift_sig": DRIFT_SIG,
            "summary": SUMMARY,
            "target_paths": TARGET_PATHS,
            "status": "open",
        }))
        .expect("decode write-tool args"),
    )
    .await
    .expect("request_action write tool call");
    assert!(
        out.contains("ActionRequest"),
        "tool should report the ActionRequest it created: {out}"
    );

    // The tool wrote exactly one ActionRequest doc with the supplied fields.
    let action_rows = db
        .node
        .execute("{ ActionRequest { drift_sig summary target_paths status } }")
        .await;
    let rows = action_rows
        .data
        .as_ref()
        .and_then(|d| d.get("ActionRequest"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        rows.len(),
        1,
        "write tool must have written exactly one ActionRequest doc: {rows:?}"
    );
    assert_eq!(
        rows[0].get("drift_sig").and_then(Value::as_str),
        Some(DRIFT_SIG)
    );
    assert_eq!(
        rows[0].get("summary").and_then(Value::as_str),
        Some(SUMMARY)
    );
    assert_eq!(
        rows[0].get("target_paths").and_then(Value::as_str),
        Some(TARGET_PATHS)
    );

    // The write-tool's doc fired the EventTrigger: one AgentRequest with the
    // multi-field render materializes.
    let request = wait_for_one_agent_request(db.node.as_ref(), trigger_id).await;
    assert_multi_field_render(&request, trigger_id);

    let _ = shutdown_tx.send(true);
    handle.await.unwrap().unwrap();
}
