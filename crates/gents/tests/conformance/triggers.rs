//! Live event-source delivery, filtering, writeback, and reconfiguration.
//! Concurrency branches and persistence are covered through the existing
//! trigger dispatch and ProductionMaterializer owners, without copied SQL here.

use std::sync::Arc;
use std::time::Duration;

use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::{AgentIdentity, DocumentRuntimeOptions, Gents, ToolCeiling};
use serde_json::Value;

use crate::support::fixtures::{bind_default_behavior_backend, test_identity};
use crate::support::interrupt::{wait_for_runtime_ready, TEST_RUNTIME_READY_TIMEOUT};
use crate::support::mock_endpoint::MockModelEndpoint;
use crate::support::snapshots::{fetch_runtime_snapshot, RuntimeSnapshot};
use crate::support::{set_request_lifecycle_state, test_db, AGENT_NAME};

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

async fn register_audit_event_schema(node: &EmbeddedNode) {
    let sdl = r#"
        type AuditEvent {
            external_id: String
            payload: String
            kind: String @index
        }
    "#;
    node.add_schema(sdl)
        .await
        .expect("add_schema for AuditEvent");
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
    let resp = node.execute(&mutation).await;
    assert!(!resp.has_errors(), "create Task failed: {:?}", resp.errors);
}

#[allow(clippy::too_many_arguments)]
async fn create_event_trigger(
    node: &EmbeddedNode,
    trigger_id: &str,
    task_id: &str,
    source_collection: &str,
    event_kind: &str,
    filter: Option<&str>,
    enabled: bool,
    concurrency: &str,
) {
    let escaped_trigger_id = escape_graphql_string(trigger_id);
    let escaped_task_id = escape_graphql_string(task_id);
    let escaped_source_collection = escape_graphql_string(source_collection);
    let escaped_event_kind = escape_graphql_string(event_kind);
    let escaped_concurrency = escape_graphql_string(concurrency);
    let filter_entry = match filter {
        Some(f) => format!(", filter: \"{}\"", escape_graphql_string(f)),
        None => String::new(),
    };
    let mutation = format!(
        r#"mutation {{
            create_EventTrigger(input: {{
                trigger_id: "{escaped_trigger_id}",
                task_id: "{escaped_task_id}",
                source_collection: "{escaped_source_collection}",
                event_kind: "{escaped_event_kind}",
                enabled: {enabled},
                concurrency: "{escaped_concurrency}",
                fire_count: 0{filter_entry}
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create EventTrigger failed: {:?}",
        resp.errors
    );
}

/// Update the apply-owned `source_collection` field on an EventTrigger (used
/// by the generation-bump reconciliation test).
///
/// DefraDB rejects `update_EventTrigger` mutations on a doc whose existing
/// `last_attempt_at` (a `DateTime` scalar) is not restated in the input — it
/// appears to re-validate scalar DateTime fields during the round-trip and
/// trips on the `String(...)` vs `Scalar(DateTime)` mismatch. Mirror the
/// workaround PR 1's `schedule_writeback_errored` helper takes: read the
/// current `last_attempt_at` and restate it in the update input if present.
async fn update_event_trigger_source_collection(
    node: &EmbeddedNode,
    trigger_id: &str,
    new_source_collection: &str,
) {
    let escaped_trigger_id = escape_graphql_string(trigger_id);
    let escaped_new = escape_graphql_string(new_source_collection);

    let read_query = format!(
        r#"{{
            EventTrigger(
                filter: {{ trigger_id: {{ _eq: "{escaped_trigger_id}" }} }},
                limit: 1
            ) {{ last_attempt_at }}
        }}"#
    );
    let read_resp = node.execute(&read_query).await;
    assert!(
        !read_resp.has_errors(),
        "read EventTrigger last_attempt_at failed: {:?}",
        read_resp.errors
    );
    let last_attempt_at = read_resp
        .data
        .as_ref()
        .and_then(|d| d.get("EventTrigger"))
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("last_attempt_at"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);

    let last_attempt_entry = match last_attempt_at.as_deref() {
        Some(v) => format!(", last_attempt_at: \"{}\"", escape_graphql_string(v)),
        None => String::new(),
    };
    let mutation = format!(
        r#"mutation {{
            update_EventTrigger(
                filter: {{ trigger_id: {{ _eq: "{escaped_trigger_id}" }} }},
                input: {{ source_collection: "{escaped_new}"{last_attempt_entry} }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "update EventTrigger source_collection failed: {:?}",
        resp.errors
    );
}

async fn write_webhook_event(node: &EmbeddedNode, external_id: &str, kind: &str) -> String {
    write_dynamic_event(node, "WebhookEvent", external_id, kind).await
}

async fn write_dynamic_event(
    node: &EmbeddedNode,
    collection: &str,
    external_id: &str,
    kind: &str,
) -> String {
    let escaped_external_id = escape_graphql_string(external_id);
    let escaped_kind = escape_graphql_string(kind);
    let mutation = format!(
        r#"mutation {{
            add_{collection}(input: {{
                external_id: "{escaped_external_id}",
                payload: "{{}}",
                kind: "{escaped_kind}"
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "add_{collection} failed: {:?}",
        resp.errors
    );
    let data = resp
        .data
        .as_ref()
        .unwrap_or_else(|| panic!("add_{collection} response missing data"));
    let field = data
        .get(format!("add_{collection}"))
        .or_else(|| data.get(format!("create_{collection}")))
        .unwrap_or_else(|| panic!("add_/create_{collection} key missing; data={data:?}"));
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
        .unwrap_or_else(|| panic!("{collection} mutation returned no _docID: {field}"))
}

async fn count_agent_requests_for_trigger(
    node: &EmbeddedNode,
    trigger_id: &str,
    trigger_kind: &str,
) -> usize {
    let escaped_trigger_id = escape_graphql_string(trigger_id);
    let escaped_trigger_kind = escape_graphql_string(trigger_kind);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    caused_by_trigger_id: {{ _eq: "{escaped_trigger_id}" }},
                    caused_by_trigger_kind: {{ _eq: "{escaped_trigger_kind}" }}
                }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "count AgentRequest by trigger failed: {:?}",
        resp.errors
    );
    resp.data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .map(|rows| rows.len())
        .unwrap_or(0)
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
struct EventTriggerRow {
    fire_count: Option<i64>,
    last_status: Option<String>,
    last_error: Option<String>,
    last_fired_source_doc_id: Option<String>,
    enabled: Option<bool>,
    source_collection: Option<String>,
    event_kind: Option<String>,
    concurrency: Option<String>,
    task_id: Option<String>,
}

async fn fetch_event_trigger_row(node: &EmbeddedNode, trigger_id: &str) -> Option<EventTriggerRow> {
    let escaped_trigger_id = escape_graphql_string(trigger_id);
    let query = format!(
        r#"{{
            EventTrigger(
                filter: {{ trigger_id: {{ _eq: "{escaped_trigger_id}" }} }},
                limit: 1
            ) {{
                fire_count
                last_status
                last_error
                last_fired_source_doc_id
                enabled
                source_collection
                event_kind
                concurrency
                task_id
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "fetch EventTrigger row failed: {:?}",
        resp.errors
    );
    let row = resp
        .data
        .as_ref()
        .and_then(|d| d.get("EventTrigger"))
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())
        .cloned()?;
    Some(EventTriggerRow {
        fire_count: row.get("fire_count").and_then(|v| v.as_i64()),
        last_status: row
            .get("last_status")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        last_error: row
            .get("last_error")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        last_fired_source_doc_id: row
            .get("last_fired_source_doc_id")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        enabled: row.get("enabled").and_then(|v| v.as_bool()),
        source_collection: row
            .get("source_collection")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        event_kind: row
            .get("event_kind")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        concurrency: row
            .get("concurrency")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        task_id: row
            .get("task_id")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
    })
}

struct BootedAgent {
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    handle: tokio::task::JoinHandle<anyhow::Result<()>>,
    _endpoint: MockModelEndpoint,
    agent_did: String,
    default_behavior_id: String,
}

impl BootedAgent {
    async fn shutdown(self) {
        let _ = self.shutdown_tx.send(true);
        match tokio::time::timeout(Duration::from_secs(5), self.handle).await {
            Ok(join_result) => {
                let _ = join_result;
            }
            Err(_) => panic!("agent did not shut down within 5s"),
        }
    }
}

async fn boot_agent(db: &crate::support::TestDb, test_name: &str, backend_id: &str) -> BootedAgent {
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity(test_name));
    let mock_endpoint = MockModelEndpoint::start("default").unwrap();
    bind_default_behavior_backend(
        db.node.as_ref(),
        identity.did(),
        backend_id,
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

    wait_for_runtime_ready(db.node.as_ref(), &agent_did).await;

    BootedAgent {
        shutdown_tx,
        handle,
        _endpoint: mock_endpoint,
        agent_did,
        default_behavior_id,
    }
}

async fn wait_for_runtime_snapshot<F>(
    node: &EmbeddedNode,
    agent_did: &str,
    predicate: F,
) -> RuntimeSnapshot
where
    F: Fn(&RuntimeSnapshot) -> bool,
{
    let deadline = tokio::time::Instant::now() + TEST_RUNTIME_READY_TIMEOUT;
    let mut last_snapshot = None;
    let mut sleep = Duration::from_millis(50);
    loop {
        if let Some(snapshot) = fetch_runtime_snapshot(node, agent_did).await {
            if predicate(&snapshot) {
                return snapshot;
            }
            last_snapshot = Some(snapshot);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out after {TEST_RUNTIME_READY_TIMEOUT:?} waiting for runtime snapshot for \
             {agent_did}; last_snapshot={last_snapshot:?}",
        );
        tokio::time::sleep(sleep).await;
        sleep = (sleep * 2).min(Duration::from_millis(250));
    }
}

async fn wait_for_request_count(
    node: &EmbeddedNode,
    trigger_id: &str,
    expected: usize,
    timeout: Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let count = count_agent_requests_for_trigger(node, trigger_id, "event").await;
        if count == expected {
            return;
        }
        if count > expected {
            panic!("over-fire for trigger_id={trigger_id}: expected {expected}, got {count}");
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for request_count({trigger_id}) == {expected}; got {count}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_last_status(
    node: &EmbeddedNode,
    trigger_id: &str,
    desired: &str,
    timeout: Duration,
) -> EventTriggerRow {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(row) = fetch_event_trigger_row(node, trigger_id).await {
            if row.last_status.as_deref() == Some(desired) {
                return row;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for EventTrigger({trigger_id}).last_status = {desired:?}; \
                 got {:?}",
                fetch_event_trigger_row(node, trigger_id).await
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Register a GroupMember source schema for live per-group delivery tests.
/// `run_id` is indexed because `EventTrigger.correlation_field` resolves
/// against it in the filter probe and recovery pages.
async fn register_group_member_schema(node: &EmbeddedNode) {
    let sdl = r#"
        type GroupMember {
            run_id: String @index
            value: String
        }
    "#;
    node.add_schema(sdl)
        .await
        .expect("add_schema for GroupMember");
}

async fn write_group_member(node: &EmbeddedNode, run_id: &str, value: &str) {
    let escaped_run_id = escape_graphql_string(run_id);
    let escaped_value = escape_graphql_string(value);
    let mutation = format!(
        r#"mutation {{
            create_GroupMember(input: {{
                run_id: "{escaped_run_id}",
                value: "{escaped_value}"
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create_GroupMember failed: {:?}",
        resp.errors
    );
}

/// Create a `per_group` EventTrigger with a fixed expected cardinality and a
/// 60s collection timeout. Both knobs are required by the runtime resolve-time
/// quarantine (a per_group trigger needs correlation + expected count or
/// timeout), so this fixture exercises the existing resolver. These legacy EventTrigger
/// fields migrate to Trigger -> EventSource grouping; they are not pack authoring targets.
async fn create_per_group_event_trigger(
    node: &EmbeddedNode,
    trigger_id: &str,
    task_id: &str,
    source_collection: &str,
    expected_count: i64,
) {
    let escaped_trigger_id = escape_graphql_string(trigger_id);
    let escaped_task_id = escape_graphql_string(task_id);
    let escaped_source_collection = escape_graphql_string(source_collection);
    let mutation = format!(
        r#"mutation {{
            create_EventTrigger(input: {{
                trigger_id: "{escaped_trigger_id}",
                task_id: "{escaped_task_id}",
                source_collection: "{escaped_source_collection}",
                event_kind: "created",
                enabled: true,
                concurrency: "serial",
                fire_mode: "per_group",
                correlation_field: "run_id",
                expected_count: {expected_count},
                group_timeout_secs: 60,
                fire_count: 0
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create per-group EventTrigger failed: {:?}",
        resp.errors
    );
}

/// Create an `AgentRequest` row carrying exact trigger lineage so tests can
/// seed the durable input of `ProductionMaterializer::
/// has_active_runtime_request_for_trigger` through the real store. The lineage
/// tuple is `@immutable` in the schema, so it must be authored at create time;
/// `lifecycle_state` is authored directly, matching what the runtime's
/// completion/terminal owners persist.
///
async fn create_request_with_trigger_lineage(
    node: &EmbeddedNode,
    agent_did: &str,
    request_id: &str,
    lifecycle_state: &str,
    trigger_id: &str,
    trigger_kind: &str,
) -> String {
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_lifecycle_state = escape_graphql_string(lifecycle_state);
    let escaped_trigger_id = escape_graphql_string(trigger_id);
    let escaped_trigger_kind = escape_graphql_string(trigger_kind);
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                agent_did: "{escaped_agent_did}",
                behavior_id: "{AGENT_NAME}",
                session_id: "{escaped_request_id}",
                retry_parent_request: "",
                retry_root_request: "{escaped_request_id}",
                superseded_by_request: "",
                content: "serial gate seed",
                lifecycle_state: "{escaped_lifecycle_state}",
                caused_by_trigger_id: "{escaped_trigger_id}",
                caused_by_trigger_kind: "{escaped_trigger_kind}",
                backend_id: "",
                execution_origin: "scheduled",
                created_at: "2026-01-01T00:00:00Z",
                retry_count: 0,
                max_retries: {max_retries},
                subagent_depth: 0
            }}) {{ _docID }}
        }}"#,
        max_retries = gents::lifecycle::DEFAULT_REQUEST_MAX_RETRIES,
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create serial-gate seed request failed: {:?}",
        resp.errors
    );
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}) {{ _docID }}
        }}"#
    );
    let lookup = node.execute(&query).await;
    assert!(
        !lookup.has_errors(),
        "seed lookup failed: {:?}",
        lookup.errors
    );
    lookup
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(|v| v.as_str())
        .expect("seeded AgentRequest _docID")
        .to_string()
}

#[path = "triggers_cases/event_source_cases.rs"]
mod event_source_cases;
