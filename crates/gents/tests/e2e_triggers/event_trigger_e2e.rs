//! Task 24 — end-to-end integration test for the full EventTrigger pipeline.
//!
//! This test exercises the complete vertical from DefraDB event bus all the
//! way to a materialized `AgentRequest` row. It:
//!
//!   1. Boots a real `Gents` against an embedded DefraDB node with a
//!      `MockModelEndpoint` so the backend probes healthy without reaching
//!      a live LLM.
//!   2. Registers a custom `WebhookEvent` schema *before* seeding the
//!      Task / EventTrigger docs — EventTrigger runtime resolution (and the
//!      EventSource filter probe) both need the source collection already
//!      present on the node.
//!   3. Seeds one Task + one EventTrigger bound to the default behavior,
//!      gated by `filter: { kind: { _eq: "signup" } }` with a `{{ doc.external_id }}`
//!      prompt template.
//!   4. Waits for the control-watcher reconcile to pick up the new trigger and
//!      bump `active_generation` past the startup baseline (Task 21 pipeline).
//!   5. Writes a matching `WebhookEvent` source doc and polls the
//!      `AgentRequest` collection until exactly one request lands with the
//!      expected trigger lineage.
//!   6. Asserts the rendered prompt made it onto the request's `content` and
//!      that the EventTrigger's runtime-owned bookkeeping fields
//!      (`fire_count`, `last_status`, `last_fired_source_doc_id`) were
//!      written through by the EventSource `on_result` callback (Task 22).
//!
//! Mirrors `tests/trigger_engine_e2e.rs` (the schedule-side PR 1 e2e) but
//! exercises the event-driven source end-to-end: a real control-plane
//! document write drives a real source-doc event, which drives a real fire
//! through the TriggerEngine, through the ProductionMaterializer, and onto
//! a persisted AgentRequest row.

use std::sync::Arc;
use std::time::Duration;

use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::{AgentIdentity, DocumentRuntimeOptions, Gents, ToolCeiling};
use serde::Deserialize;
use serde_json::Value;

use crate::support::fixtures::{bind_default_behavior_backend, test_identity};
use crate::support::mock_endpoint::MockModelEndpoint;
use crate::support::snapshots::{fetch_runtime_snapshot, RuntimeSnapshot};
use crate::support::test_db;

const TRIGGER_ID: &str = "trigger-e2e-signup";
const TASK_ID: &str = "task-e2e-signup";
const EXTERNAL_ID: &str = "wh-1";
const PROMPT_TEMPLATE: &str = "fired for {{ doc.external_id }}";

async fn register_webhook_event_schema(node: &EmbeddedNode) {
    let sdl = r#"
        type WebhookEvent {
            external_id: String
            payload: String
            kind: String @index
        }
    "#;
    node.add_schema(sdl)
        .await
        .expect("add_schema for WebhookEvent");
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

async fn create_event_trigger_with_filter(
    node: &EmbeddedNode,
    trigger_id: &str,
    task_id: &str,
    source_collection: &str,
    event_kind: &str,
    filter: &str,
) {
    let escaped_trigger_id = escape_graphql_string(trigger_id);
    let escaped_task_id = escape_graphql_string(task_id);
    let escaped_source_collection = escape_graphql_string(source_collection);
    let escaped_event_kind = escape_graphql_string(event_kind);
    let escaped_filter = escape_graphql_string(filter);
    let mutation = format!(
        r#"mutation {{
            create_EventTrigger(input: {{
                trigger_id: "{escaped_trigger_id}",
                task_id: "{escaped_task_id}",
                source_collection: "{escaped_source_collection}",
                event_kind: "{escaped_event_kind}",
                filter: "{escaped_filter}",
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
    #[serde(rename = "_docID")]
    _doc_id: String,
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
                _docID
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

#[derive(Debug, Deserialize)]
struct EventTriggerRow {
    fire_count: Option<i64>,
    last_status: Option<String>,
    last_fired_source_doc_id: Option<String>,
    last_error: Option<String>,
    task_id: Option<String>,
    source_collection: Option<String>,
    event_kind: Option<String>,
    enabled: Option<bool>,
    concurrency: Option<String>,
}

async fn fetch_event_trigger(node: &EmbeddedNode, trigger_id: &str) -> EventTriggerRow {
    let escaped_trigger_id = escape_graphql_string(trigger_id);
    let query = format!(
        r#"{{
            EventTrigger(
                filter: {{ trigger_id: {{ _eq: "{escaped_trigger_id}" }} }},
                limit: 1
            ) {{
                fire_count
                last_status
                last_fired_source_doc_id
                last_error
                task_id
                source_collection
                event_kind
                enabled
                concurrency
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "EventTrigger query failed: {:?}",
        response.errors
    );
    let row = response
        .data
        .as_ref()
        .and_then(|data| data.get("EventTrigger"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .expect("EventTrigger row missing");
    serde_json::from_value(row).expect("decode EventTrigger row")
}

async fn write_webhook_event(node: &EmbeddedNode, external_id: &str, kind: &str) -> String {
    let escaped_external_id = escape_graphql_string(external_id);
    let escaped_kind = escape_graphql_string(kind);
    let mutation = format!(
        r#"mutation {{
            add_WebhookEvent(input: {{
                external_id: "{escaped_external_id}",
                payload: "{{}}",
                kind: "{escaped_kind}"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "add_WebhookEvent failed: {:?}",
        response.errors
    );
    let data = response
        .data
        .as_ref()
        .expect("add_WebhookEvent response missing data");
    let field = data
        .get("add_WebhookEvent")
        .or_else(|| data.get("create_WebhookEvent"))
        .unwrap_or_else(|| panic!("add_/create_WebhookEvent key missing; data={data:?}"));
    field
        .get("_docID")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            field
                .as_array()
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("_docID"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| panic!("WebhookEvent mutation returned no _docID: {field}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_trigger_fires_on_source_doc_create_end_to_end() {
    let db = test_db("event-trigger-e2e").await;

    register_webhook_event_schema(db.node.as_ref()).await;

    let identity = Arc::new(test_identity("event-trigger-e2e"));
    let mock_endpoint = MockModelEndpoint::start("default").unwrap();
    bind_default_behavior_backend(
        db.node.as_ref(),
        identity.did(),
        "backend-event-trigger-e2e",
        mock_endpoint.endpoint(),
    )
    .await;

    let agent = Gents::from_default_behavior_documents(
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

    create_task(
        db.node.as_ref(),
        TASK_ID,
        &default_behavior_id,
        PROMPT_TEMPLATE,
    )
    .await;
    create_event_trigger_with_filter(
        db.node.as_ref(),
        TRIGGER_ID,
        TASK_ID,
        "WebhookEvent",
        "created",
        r#"{ kind: { _eq: "signup" } }"#,
    )
    .await;

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

    let source_doc_id = write_webhook_event(db.node.as_ref(), EXTERNAL_ID, "signup").await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let requests = loop {
        let rows = query_agent_requests_for_trigger(db.node.as_ref(), TRIGGER_ID).await;
        if !rows.is_empty() {
            break rows;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for AgentRequest caused_by_trigger_id={TRIGGER_ID}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        requests.len(),
        1,
        "expected exactly one AgentRequest from a single matching WebhookEvent, got {}: {:?}",
        requests.len(),
        requests
    );
    let request = &requests[0];
    assert_eq!(
        request.caused_by_trigger_id.as_deref(),
        Some(TRIGGER_ID),
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
    let expected_content = format!("fired for {EXTERNAL_ID}");
    assert_eq!(
        request.content, expected_content,
        "rendered prompt should substitute doc.external_id: {request:?}"
    );
    assert!(
        !request.request_id.is_empty(),
        "request_id must be populated: {request:?}"
    );

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let fired = loop {
        let row = fetch_event_trigger(db.node.as_ref(), TRIGGER_ID).await;
        if row.last_status.as_deref() == Some("fired") {
            break row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for EventTrigger.last_status=\"fired\" (last row: {row:?})"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(
        fired.fire_count,
        Some(1),
        "fire_count must be 1 after one fire: {fired:?}"
    );
    assert_eq!(
        fired.last_fired_source_doc_id.as_deref(),
        Some(source_doc_id.as_str()),
        "last_fired_source_doc_id should match the WebhookEvent docID: {fired:?}"
    );
    assert!(
        fired.last_error.as_deref().unwrap_or("").is_empty(),
        "last_error must be cleared on a successful fire: {fired:?}"
    );
    assert_eq!(fired.task_id.as_deref(), Some(TASK_ID));
    assert_eq!(fired.source_collection.as_deref(), Some("WebhookEvent"));
    assert_eq!(fired.event_kind.as_deref(), Some("created"));
    assert_eq!(fired.enabled, Some(true));
    assert_eq!(fired.concurrency.as_deref(), Some("serial"));

    let _ = shutdown_tx.send(true);
    handle.await.unwrap().unwrap();
}
