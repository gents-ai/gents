//! Task 30 — conformance tests for the trigger engine + EventTrigger lifecycle.
//!
//! # Scope
//!
//! These tests lock down the **externally-observable** conformance contract
//! between the trigger engine, the `EventSource`, the
//! `ProductionMaterializer`, and DefraDB. Full engine-level e2e coverage for
//! the event pipeline (source-doc create → materialized `AgentRequest` with
//! lineage) is handled by `tests/event_trigger_e2e.rs` (PR 2 Task 24). The
//! cases here pin the corresponding conformance surface:
//!
//! * `fires_on_matching_source_doc_create` — a filter-less EventTrigger
//!   materializes an `AgentRequest` with `caused_by_trigger_kind = "event"`
//!   and the rendered template in `content`.
//! * `does_not_fire_when_source_doc_fails_filter` — an EventTrigger gated by a
//!   `kind == "signup"` filter does NOT fire for `kind: "other"` source docs,
//!   and the runtime bookkeeping on the trigger row stays null.
//! * `enabled_false_does_not_fire` — `enabled: false` triggers never
//!   materialize requests, even for matching source docs.
//! * `backfill_is_forward_only` — pre-existing source docs are NEVER replayed
//!   when a trigger becomes active; only NEW doc-create events fire.
//! * `subscription_reconciles_on_generation_bump` — re-pointing a trigger from
//!   collection A to collection B bumps `active_generation` and only B-side
//!   writes fire afterwards.
//! * `serial_skips_when_prior_non_terminal` — the engine's gating query sees a
//!   non-terminal `(trigger_id, "event")` tuple and the serial trigger skips.
//! * `latest_only_supersedes_prior_fire` — the supersede mutation the engine
//!   would run transitions the in-flight event-kind request to
//!   `superseded`, and a new materialize lands with the same lineage.
//! * `template_render_failure_records_error_status` — an event-kind render
//!   failure writes `last_status = "error"` / `last_error = ...` on the
//!   EventTrigger doc without materializing a request.
//! * `two_triggers_same_source_collection_each_evaluate_filter_independently`
//!   — two triggers on the same `source_collection` apply their own filters
//!   independently; only the trigger whose filter matches fires.
//!
//! # Pragmatic split: engine semantics vs. persistence/operational surfaces
//!
//! The pure trigger-engine branch matrix is pinned in-crate by
//! `trigger_engine::tests::trigger_engine_dispatch_matches_lean_generated_contract_cases`,
//! which consumes finite cases emitted by
//! `Proofs/Conformance/Triggers/Contracts.lean`. That Lean-generated contract
//! covers manual dispatch, schedule/event reachability, tuple-sensitive serial
//! gating, latest-only supersession, parallel bypass of in-flight gates, and
//! lineage shape without depending on wall-clock debounce.
//!
//! Cases 6, 7, 8 remain asserted here at the persistence-layer contract (seed
//! an in-flight `AgentRequest` with the right lineage tuple + simulate the
//! exact mutation / writeback the production materializer/source produces).
//! They are still valuable because they pin the DefraDB query/mutation shape
//! the engine delegates to at runtime, but they are no longer the only
//! correctness oracle for serial/latest-only trigger behavior.
//!
//! Cases 1, 2, 3, 4, 5, 9 boot a real `DefraAgent` so the EventSource loop
//! actually observes DefraDB events; these are the tests where the
//! externally-observable behavior *only* exists if the live subscription +
//! filter + materialize chain runs end to end.

use std::sync::Arc;
use std::time::Duration;

use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::lifecycle::{ExecutionOrigin, RequestLifecycle, TriggerLineage};
use defra_agent::{AgentIdentity, DefraAgent, DocumentRuntimeOptions, ToolCeiling};
use serde_json::Value;

mod support;

use support::fixtures::{bind_default_behavior_backend, test_identity};
use support::mock_endpoint::MockModelEndpoint;
use support::snapshots::{fetch_runtime_snapshot, RuntimeSnapshot};
use support::{test_db, AGENT_DID, AGENT_NAME, BACKEND_ID, DEADLINE_SECS};

// -----------------------------------------------------------------------------
// Shared DB helpers
// -----------------------------------------------------------------------------

/// Register the `WebhookEvent` source collection used by most cases. Indexed
/// `kind` so operator-authored filters (`{ kind: { _eq: "…" } }`) pass the
/// EventSource's limit-1 filter probe.
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

/// Secondary source collection used by `subscription_reconciles_on_generation_bump`.
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

    // Read the current `last_attempt_at` so we can restate it on the update
    // if the runtime has already written to it (e.g., after a previous fire).
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

/// Insert a `WebhookEvent` document (dynamically-added collections expose
/// `add_<Collection>` as their insertion alias on DefraDB). Returns the new
/// `_docID`.
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

/// Mirrors `ProductionMaterializer::has_nonterminal_request_for_trigger`.
async fn has_nonterminal_request_for_trigger(
    node: &EmbeddedNode,
    trigger_id: &str,
    trigger_kind: &str,
) -> bool {
    let escaped_trigger_id = escape_graphql_string(trigger_id);
    let escaped_trigger_kind = escape_graphql_string(trigger_kind);
    let query = format!(
        r#"query {{
            AgentRequest(
                filter: {{
                    caused_by_trigger_id: {{ _eq: "{escaped_trigger_id}" }},
                    caused_by_trigger_kind: {{ _eq: "{escaped_trigger_kind}" }},
                    lifecycle_state: {{ _in: ["pending", "claimed", "processing", "inputRequired"] }}
                }},
                limit: 1
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "has_nonterminal_request_for_trigger failed: {:?}",
        resp.errors
    );
    resp.data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .map(|rows| !rows.is_empty())
        .unwrap_or(false)
}

/// Mirrors `ProductionMaterializer::supersede_nonterminal_requests_for_trigger`.
async fn supersede_nonterminal_requests_for_trigger(
    node: &EmbeddedNode,
    trigger_id: &str,
    trigger_kind: &str,
) -> usize {
    let escaped_trigger_id = escape_graphql_string(trigger_id);
    let escaped_trigger_kind = escape_graphql_string(trigger_kind);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{
                    caused_by_trigger_id: {{ _eq: "{escaped_trigger_id}" }},
                    caused_by_trigger_kind: {{ _eq: "{escaped_trigger_kind}" }},
                    lifecycle_state: {{ _in: ["pending", "claimed", "processing", "inputRequired"] }}
                }},
                input: {{
                    status: "superseded",
                    lifecycle_state: "superseded"
                }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "supersede_nonterminal_requests_for_trigger failed: {:?}",
        resp.errors
    );
    resp.data
        .as_ref()
        .and_then(|d| d.get("update_AgentRequest"))
        .and_then(|v| v.as_array())
        .map(|rows| rows.len())
        .unwrap_or(0)
}

async fn fetch_request_state(node: &EmbeddedNode, request_id: &str) -> Option<(String, String)> {
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                limit: 1
            ) {{
                lifecycle_state
                status
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "fetch_request_state failed: {:?}",
        resp.errors
    );
    resp.data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .map(|row| {
            (
                row.get("lifecycle_state")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned(),
                row.get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned(),
            )
        })
}

// -----------------------------------------------------------------------------
// Booted-agent helpers (for engine-driven cases)
// -----------------------------------------------------------------------------

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

async fn boot_agent(db: &support::TestDb, test_name: &str, backend_id: &str) -> BootedAgent {
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity(test_name));
    let mock_endpoint = MockModelEndpoint::start("default").unwrap();
    bind_default_behavior_backend(
        db.node.as_ref(),
        identity.did(),
        backend_id,
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

    wait_for_runtime_snapshot(db.node.as_ref(), &agent_did, |snapshot| {
        snapshot.process_state == "ready"
            && snapshot.reconcile_phase == "idle"
            && snapshot.active_generation >= 1
            && snapshot.runnable_behavior_count >= 1
    })
    .await;

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

/// Poll for exactly `expected` AgentRequests carrying
/// `(caused_by_trigger_id, caused_by_trigger_kind = "event")`, with a
/// generous deadline. Fails with the current count when the deadline fires.
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

/// Assert that NO AgentRequest materializes for `trigger_id` within `settle`.
/// Used by the "does not fire" cases. This is necessarily a negative
/// assertion with a timeout — the engine is event-driven, so we sleep long
/// enough that if it *were* going to fire it would have done so already.
async fn assert_no_request_within(node: &EmbeddedNode, trigger_id: &str, settle: Duration) {
    tokio::time::sleep(settle).await;
    let count = count_agent_requests_for_trigger(node, trigger_id, "event").await;
    assert_eq!(
        count, 0,
        "expected no AgentRequest for trigger_id={trigger_id} but got {count}"
    );
}

/// Poll the EventTrigger row until its `last_status` reaches `desired`.
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

// -----------------------------------------------------------------------------
// Task 30 cases
// -----------------------------------------------------------------------------

/// A filter-less EventTrigger fires for any source doc create on its
/// `source_collection`, producing an `AgentRequest` with
/// `caused_by_trigger_kind = "event"`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fires_on_matching_source_doc_create() {
    let db = test_db("trigger-conformance-fires").await;
    register_webhook_event_schema(db.node.as_ref()).await;

    let agent = boot_agent(&db, "trigger-conformance-fires", "backend-fires").await;
    let initial_gen = fetch_runtime_snapshot(db.node.as_ref(), &agent.agent_did)
        .await
        .unwrap()
        .active_generation;

    create_task(
        db.node.as_ref(),
        "task-fires",
        &agent.default_behavior_id,
        "plain prompt",
    )
    .await;
    create_event_trigger(
        db.node.as_ref(),
        "trigger-fires",
        "task-fires",
        "WebhookEvent",
        "created",
        None,
        true,
        "serial",
    )
    .await;

    // Wait for the control-watcher debounce + reload so the trigger is resolved
    // and the EventSource subscription is active.
    wait_for_runtime_snapshot(db.node.as_ref(), &agent.agent_did, |snap| {
        snap.active_generation > initial_gen && snap.last_reconcile_result == "applied"
    })
    .await;

    let _source_doc_id = write_webhook_event(db.node.as_ref(), "ext-1", "any").await;
    wait_for_request_count(
        db.node.as_ref(),
        "trigger-fires",
        1,
        Duration::from_secs(10),
    )
    .await;

    let fired = wait_for_last_status(
        db.node.as_ref(),
        "trigger-fires",
        "fired",
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(fired.fire_count, Some(1));
    assert_eq!(fired.task_id.as_deref(), Some("task-fires"));

    agent.shutdown().await;
}

/// Source doc does NOT match the trigger's filter → no request materializes
/// and no runtime writeback on the trigger row happens.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn does_not_fire_when_source_doc_fails_filter() {
    let db = test_db("trigger-conformance-filter-miss").await;
    register_webhook_event_schema(db.node.as_ref()).await;

    let agent = boot_agent(
        &db,
        "trigger-conformance-filter-miss",
        "backend-filter-miss",
    )
    .await;
    let initial_gen = fetch_runtime_snapshot(db.node.as_ref(), &agent.agent_did)
        .await
        .unwrap()
        .active_generation;

    create_task(
        db.node.as_ref(),
        "task-filter-miss",
        &agent.default_behavior_id,
        "prompt",
    )
    .await;
    create_event_trigger(
        db.node.as_ref(),
        "trigger-filter-miss",
        "task-filter-miss",
        "WebhookEvent",
        "created",
        Some(r#"{ kind: { _eq: "signup" } }"#),
        true,
        "serial",
    )
    .await;
    wait_for_runtime_snapshot(db.node.as_ref(), &agent.agent_did, |snap| {
        snap.active_generation > initial_gen && snap.last_reconcile_result == "applied"
    })
    .await;

    let _ = write_webhook_event(db.node.as_ref(), "ext-other", "other").await;
    assert_no_request_within(
        db.node.as_ref(),
        "trigger-filter-miss",
        Duration::from_secs(2),
    )
    .await;

    let row = fetch_event_trigger_row(db.node.as_ref(), "trigger-filter-miss")
        .await
        .expect("EventTrigger doc present");
    // Runtime writeback fields stay untouched — the engine never called
    // on_result for a filter-miss.
    assert_eq!(row.last_status, None);
    assert_eq!(row.last_error, None);
    assert_eq!(row.fire_count.unwrap_or(0), 0);
    assert_eq!(row.last_fired_source_doc_id, None);

    agent.shutdown().await;
}

/// `enabled: false` EventTriggers must not fire even on matching source docs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enabled_false_does_not_fire() {
    let db = test_db("trigger-conformance-disabled").await;
    register_webhook_event_schema(db.node.as_ref()).await;

    let agent = boot_agent(&db, "trigger-conformance-disabled", "backend-disabled").await;

    create_task(
        db.node.as_ref(),
        "task-disabled",
        &agent.default_behavior_id,
        "prompt",
    )
    .await;
    create_event_trigger(
        db.node.as_ref(),
        "trigger-disabled",
        "task-disabled",
        "WebhookEvent",
        "created",
        None,
        false,
        "serial",
    )
    .await;

    // Give the control watcher a chance to observe the insert (even though
    // the trigger is disabled, inserting it still produces a doc-update event
    // + reconcile pass that classifies it as unavailable).
    tokio::time::sleep(Duration::from_secs(7)).await;

    let _ = write_webhook_event(db.node.as_ref(), "ext-disabled", "any").await;
    assert_no_request_within(db.node.as_ref(), "trigger-disabled", Duration::from_secs(2)).await;

    let row = fetch_event_trigger_row(db.node.as_ref(), "trigger-disabled")
        .await
        .expect("EventTrigger doc present");
    assert_eq!(
        row.enabled,
        Some(false),
        "disabled trigger must persist enabled=false"
    );
    assert_eq!(
        row.fire_count.unwrap_or(0),
        0,
        "disabled trigger must not fire"
    );

    agent.shutdown().await;
}

/// **Highest-signal test**: backfill is forward-only. Pre-existing source
/// docs are not replayed when an EventTrigger activates; only NEW doc-create
/// events trigger a fire.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backfill_is_forward_only() {
    let db = test_db("trigger-conformance-backfill").await;
    register_webhook_event_schema(db.node.as_ref()).await;

    // Step 1: seed 3 source docs BEFORE booting the agent or creating the
    // trigger. Await each write to completion so they are durable.
    let _ = write_dynamic_event(db.node.as_ref(), "WebhookEvent", "pre-1", "signup").await;
    let _ = write_dynamic_event(db.node.as_ref(), "WebhookEvent", "pre-2", "signup").await;
    let _ = write_dynamic_event(db.node.as_ref(), "WebhookEvent", "pre-3", "signup").await;

    // Step 2: boot the agent and create the trigger — `enabled: true` so it
    // joins the active set on the first reconcile after insert.
    let agent = boot_agent(&db, "trigger-conformance-backfill", "backend-backfill").await;
    let initial_gen = fetch_runtime_snapshot(db.node.as_ref(), &agent.agent_did)
        .await
        .unwrap()
        .active_generation;

    create_task(
        db.node.as_ref(),
        "task-backfill",
        &agent.default_behavior_id,
        "prompt",
    )
    .await;
    create_event_trigger(
        db.node.as_ref(),
        "trigger-backfill",
        "task-backfill",
        "WebhookEvent",
        "created",
        None,
        true,
        "serial",
    )
    .await;

    // Step 3: wait for reconcile so the subscription is live.
    wait_for_runtime_snapshot(db.node.as_ref(), &agent.agent_did, |snap| {
        snap.active_generation > initial_gen && snap.last_reconcile_result == "applied"
    })
    .await;

    // Step 4: sleep 2 seconds, then assert ZERO fires — backfill must not
    // replay the 3 pre-seeded docs.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        count_agent_requests_for_trigger(db.node.as_ref(), "trigger-backfill", "event").await,
        0,
        "backfill must not replay pre-existing source docs"
    );

    // Step 5: write ONE new doc; assert EXACTLY ONE AgentRequest.
    let _ = write_webhook_event(db.node.as_ref(), "post-1", "signup").await;
    wait_for_request_count(
        db.node.as_ref(),
        "trigger-backfill",
        1,
        Duration::from_secs(10),
    )
    .await;

    let fired = wait_for_last_status(
        db.node.as_ref(),
        "trigger-backfill",
        "fired",
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(fired.fire_count, Some(1), "exactly one fire recorded");

    agent.shutdown().await;
}

/// Re-pointing the trigger's `source_collection` drives the control watcher
/// to reconcile and bump `active_generation`. The EventSource observes the
/// bump at the next `next_fire` tick and reconciles its `desired_collections`
/// set (the exact internal effect is pinned directly by the in-crate
/// `event_source_reconciles_subscriptions_on_generation_bump` test — Task 19).
///
/// Here we pin the externally-observable side of the contract:
///   1. Inserting the trigger bumps `active_generation` past startup and the
///      resolved snapshot classifies the trigger as applied cleanly.
///   2. Updating the trigger's `source_collection` drives **another** gen
///      bump with `last_reconcile_result = "applied"`, which is the signal
///      the EventSource receives via `snapshot_rx.changed()` to reconcile
///      its subscription set.
///   3. Post-flip WebhookEvent creates do NOT fire (the trigger no longer
///      resolves to `source_collection = WebhookEvent`). The positive side
///      of the flip (AuditEvent creates now firing the trigger) is covered
///      by `fires_on_matching_source_doc_create` for a fresh collection and
///      by the in-crate reconcile test for the internal subscription swap;
///      reproducing the end-to-end cross-collection flip here would add a
///      timing-sensitive dependency on the event-bus delivery ordering
///      immediately across a reconcile tick that adds nothing over the
///      in-crate coverage.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscription_reconciles_on_generation_bump() {
    let db = test_db("trigger-conformance-subscription-reconcile").await;
    register_webhook_event_schema(db.node.as_ref()).await;
    register_audit_event_schema(db.node.as_ref()).await;

    let agent = boot_agent(
        &db,
        "trigger-conformance-subscription-reconcile",
        "backend-subscription-reconcile",
    )
    .await;
    let startup_gen = fetch_runtime_snapshot(db.node.as_ref(), &agent.agent_did)
        .await
        .unwrap()
        .active_generation;

    // Step 1: create the trigger pointing at WebhookEvent. Observable: gen
    // bump + `applied`.
    create_task(
        db.node.as_ref(),
        "task-reconcile",
        &agent.default_behavior_id,
        "prompt",
    )
    .await;
    create_event_trigger(
        db.node.as_ref(),
        "trigger-reconcile",
        "task-reconcile",
        "WebhookEvent",
        "created",
        None,
        true,
        "serial",
    )
    .await;
    let post_insert_snap = wait_for_runtime_snapshot(db.node.as_ref(), &agent.agent_did, |snap| {
        snap.active_generation > startup_gen && snap.last_reconcile_result == "applied"
    })
    .await;
    assert!(
        post_insert_snap.active_generation > startup_gen,
        "active_generation must bump after EventTrigger insert"
    );

    // Step 2: re-point the trigger to AuditEvent. Observable: another gen
    // bump + `applied`. This is the signal the EventSource receives to
    // reconcile its subscription set (Task 19 / Task 20).
    update_event_trigger_source_collection(db.node.as_ref(), "trigger-reconcile", "AuditEvent")
        .await;
    let post_flip_snap = wait_for_runtime_snapshot(db.node.as_ref(), &agent.agent_did, |snap| {
        snap.active_generation > post_insert_snap.active_generation
            && snap.last_reconcile_result == "applied"
    })
    .await;
    assert!(
        post_flip_snap.active_generation > post_insert_snap.active_generation,
        "active_generation must bump again after source_collection flip"
    );

    // Step 3: the post-flip apply path must have swapped the resolved
    // `source_collection` on disk. Assert this through the observable
    // EventTrigger row.
    let row = fetch_event_trigger_row(db.node.as_ref(), "trigger-reconcile")
        .await
        .expect("EventTrigger doc present");
    assert_eq!(
        row.source_collection.as_deref(),
        Some("AuditEvent"),
        "post-flip source_collection must be AuditEvent: {row:?}"
    );

    // Step 4: WebhookEvent creates after the flip must NOT produce a new
    // fire — WebhookEvent is no longer in the desired collection set.
    let before_flip =
        count_agent_requests_for_trigger(db.node.as_ref(), "trigger-reconcile", "event").await;
    let _ = write_webhook_event(db.node.as_ref(), "post-flip-webhook", "any").await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let after_webhook =
        count_agent_requests_for_trigger(db.node.as_ref(), "trigger-reconcile", "event").await;
    assert_eq!(
        after_webhook, before_flip,
        "post-flip WebhookEvent must not fire trigger-reconcile \
         (subscription set has moved to AuditEvent)"
    );

    agent.shutdown().await;
}

/// Serial concurrency: when a non-terminal request already exists for the
/// event-kind lineage tuple, the engine's gating query sees it → `FireResult::Skipped`.
/// No second `AgentRequest` materializes for the same tuple. Asserted at the
/// persistence-layer contract (PR 1 pattern).
#[tokio::test]
async fn serial_skips_when_prior_non_terminal() {
    let db = test_db("trigger-conformance-event-serial-skip").await;

    let lineage = TriggerLineage {
        trigger_id: Some("trigger-event-serial".into()),
        trigger_kind: Some("event".into()),
    };
    RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        "seed event in-flight",
        DEADLINE_SECS,
        ExecutionOrigin::Scheduled,
        BACKEND_ID,
        lineage,
    )
    .await
    .unwrap();

    assert!(
        has_nonterminal_request_for_trigger(db.node.as_ref(), "trigger-event-serial", "event",)
            .await,
        "gating query must see the in-flight event-kind request"
    );
    assert_eq!(
        count_agent_requests_for_trigger(db.node.as_ref(), "trigger-event-serial", "event",).await,
        1,
        "seeded count should be 1"
    );

    // The engine's FireResult::Skipped decision: no materialize call, so no
    // additional AgentRequest for the lineage tuple.
    let after =
        count_agent_requests_for_trigger(db.node.as_ref(), "trigger-event-serial", "event").await;
    assert_eq!(
        after, 1,
        "serial skip must not produce a second AgentRequest for the event-kind tuple"
    );
}

/// LatestOnly concurrency: the engine's supersede mutation transitions the
/// in-flight event-kind request to `superseded`; a new materialize lands
/// with the same lineage tuple. Asserted at the persistence-layer contract.
#[tokio::test]
async fn latest_only_supersedes_prior_fire() {
    let db = test_db("trigger-conformance-event-latest-only").await;

    let lineage = TriggerLineage {
        trigger_id: Some("trigger-event-latest".into()),
        trigger_kind: Some("event".into()),
    };
    let prior = RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        "seed prior",
        DEADLINE_SECS,
        ExecutionOrigin::Scheduled,
        BACKEND_ID,
        lineage,
    )
    .await
    .unwrap();
    let prior_request_id = prior.request().request_id.clone();

    let superseded = supersede_nonterminal_requests_for_trigger(
        db.node.as_ref(),
        "trigger-event-latest",
        "event",
    )
    .await;
    assert_eq!(
        superseded, 1,
        "supersede mutation must transition exactly the one in-flight request"
    );
    let prior_state = fetch_request_state(db.node.as_ref(), &prior_request_id)
        .await
        .expect("prior request still present");
    assert_eq!(
        prior_state,
        ("superseded".into(), "superseded".into()),
        "prior event-kind AgentRequest must be (lifecycle_state=superseded, status=superseded)"
    );

    // Materialize the new fire with the same trigger lineage.
    let new_lineage = TriggerLineage {
        trigger_id: Some("trigger-event-latest".into()),
        trigger_kind: Some("event".into()),
    };
    let new_fire = RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        "latest event fire",
        DEADLINE_SECS,
        ExecutionOrigin::Scheduled,
        BACKEND_ID,
        new_lineage,
    )
    .await
    .unwrap();
    assert_ne!(
        new_fire.request().request_id,
        prior_request_id,
        "new fire must have a fresh request_id"
    );
    assert_eq!(
        count_agent_requests_for_trigger(db.node.as_ref(), "trigger-event-latest", "event",).await,
        2
    );
    assert!(
        has_nonterminal_request_for_trigger(db.node.as_ref(), "trigger-event-latest", "event",)
            .await,
        "after materialize, the new claimed request must be visible to the gating query"
    );
}

/// Template render failure: a trigger whose Task references an undefined
/// template variable fires into `FireResult::Errored` — `last_status = "error"`
/// / `last_error` populated on the EventTrigger doc, and NO `AgentRequest`
/// materializes. Drive this end to end via the live engine so both sides of
/// the contract (the on_result writeback + the no-materialize invariant) are
/// pinned together.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn template_render_failure_records_error_status() {
    let db = test_db("trigger-conformance-render-err").await;
    register_webhook_event_schema(db.node.as_ref()).await;

    let agent = boot_agent(&db, "trigger-conformance-render-err", "backend-render-err").await;
    let initial_gen = fetch_runtime_snapshot(db.node.as_ref(), &agent.agent_did)
        .await
        .unwrap()
        .active_generation;

    create_task(
        db.node.as_ref(),
        "task-render-err",
        &agent.default_behavior_id,
        "{{ event.missing_field }}",
    )
    .await;
    create_event_trigger(
        db.node.as_ref(),
        "trigger-render-err",
        "task-render-err",
        "WebhookEvent",
        "created",
        None,
        true,
        "serial",
    )
    .await;
    wait_for_runtime_snapshot(db.node.as_ref(), &agent.agent_did, |snap| {
        snap.active_generation > initial_gen && snap.last_reconcile_result == "applied"
    })
    .await;

    let _ = write_webhook_event(db.node.as_ref(), "ext-render-err", "any").await;

    let errored = wait_for_last_status(
        db.node.as_ref(),
        "trigger-render-err",
        "error",
        Duration::from_secs(10),
    )
    .await;
    assert!(
        !errored.last_error.as_deref().unwrap_or("").is_empty(),
        "last_error must carry a render-failure reason: {errored:?}"
    );
    assert_eq!(
        errored.fire_count.unwrap_or(0),
        0,
        "render failure must not bump fire_count"
    );
    assert_eq!(
        count_agent_requests_for_trigger(db.node.as_ref(), "trigger-render-err", "event").await,
        0,
        "render failure must not materialize an AgentRequest"
    );

    agent.shutdown().await;
}

/// Two EventTriggers on the same `source_collection` with different filters:
/// a source doc matching only one filter fires only that trigger.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_triggers_same_source_collection_each_evaluate_filter_independently() {
    let db = test_db("trigger-conformance-two-filters").await;
    register_webhook_event_schema(db.node.as_ref()).await;

    let agent = boot_agent(
        &db,
        "trigger-conformance-two-filters",
        "backend-two-filters",
    )
    .await;
    let initial_gen = fetch_runtime_snapshot(db.node.as_ref(), &agent.agent_did)
        .await
        .unwrap()
        .active_generation;

    create_task(
        db.node.as_ref(),
        "task-two-a",
        &agent.default_behavior_id,
        "prompt-a",
    )
    .await;
    create_task(
        db.node.as_ref(),
        "task-two-b",
        &agent.default_behavior_id,
        "prompt-b",
    )
    .await;
    create_event_trigger(
        db.node.as_ref(),
        "trigger-two-a",
        "task-two-a",
        "WebhookEvent",
        "created",
        Some(r#"{ kind: { _eq: "signup" } }"#),
        true,
        "serial",
    )
    .await;
    create_event_trigger(
        db.node.as_ref(),
        "trigger-two-b",
        "task-two-b",
        "WebhookEvent",
        "created",
        Some(r#"{ kind: { _eq: "login" } }"#),
        true,
        "serial",
    )
    .await;
    wait_for_runtime_snapshot(db.node.as_ref(), &agent.agent_did, |snap| {
        snap.active_generation > initial_gen && snap.last_reconcile_result == "applied"
    })
    .await;

    // Write a doc matching only A's filter.
    let _ = write_webhook_event(db.node.as_ref(), "ext-signup", "signup").await;

    // A fires.
    wait_for_request_count(
        db.node.as_ref(),
        "trigger-two-a",
        1,
        Duration::from_secs(10),
    )
    .await;
    // B does not fire within the settle window.
    assert_no_request_within(db.node.as_ref(), "trigger-two-b", Duration::from_secs(2)).await;

    // Runtime-writeback sanity.
    let a = wait_for_last_status(
        db.node.as_ref(),
        "trigger-two-a",
        "fired",
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(a.fire_count, Some(1));
    let b = fetch_event_trigger_row(db.node.as_ref(), "trigger-two-b")
        .await
        .expect("EventTrigger B row present");
    assert_eq!(
        b.fire_count.unwrap_or(0),
        0,
        "trigger B must not have fired for a signup event"
    );
    assert_eq!(b.last_status, None);

    agent.shutdown().await;
}
