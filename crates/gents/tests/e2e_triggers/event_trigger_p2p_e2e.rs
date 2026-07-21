//! Task A1 — two-node P2P event-trigger e2e test.
//!
//! This qualifies the ONE unproven assumption underpinning the on-host
//! Diagnose->Act->Verify loop: **does a document that arrives on a node via
//! P2P replication fire that node's `EventTrigger` as a `created` event?**
//!
//! The EventSource subscribes to DefraDB's global update bus and treats a
//! first-observed `doc_id` as a `created` fire, so a P2P-merged doc *should*
//! fire — but the existing-docs seed-cap and the subscription-vs-merge timing
//! could suppress it. This test answers that empirically.
//!
//! Topology:
//!   - Node A (`db_writer`): a bare writer node, P2P enabled, no agent.
//!   - Node B (`db_agent`): runs the `Gents` + an `EventTrigger` watching
//!     `ReplicatedEvent`.
//!
//! Ordering matters: the source doc is written on A only AFTER B's trigger is
//! reconciled into the active snapshot AND A->B replication is established, so
//! the merged doc is a fresh first-observation on B (the `created` path).
//!
//! This test owns its own source schema (`ReplicatedEvent`) so it does not
//! collide with the single-node `event_trigger_e2e.rs` `WebhookEvent` schema.
//!
//! Mirrors `event_trigger_e2e.rs` (the single-node vertical) for the agent /
//! trigger / assertion shape, and `state_machine_conformance/r5_cross_deployment.rs`
//! for the two-node P2P replicator plumbing (`install_one_way_replicator`,
//! `wait_for_listen_addr`, `wait_for_connected_peer`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::{AgentIdentity, Gents, DocumentRuntimeOptions, ToolCeiling};
use serde::Deserialize;
use serde_json::Value;

use crate::support::fixtures::{bind_default_behavior_backend, test_identity};
use crate::support::mock_endpoint::MockModelEndpoint;
use crate::support::snapshots::{fetch_runtime_snapshot, RuntimeSnapshot};
use crate::support::test_p2p_db;

const TRIGGER_ID: &str = "trigger-p2p-signup";
const TASK_ID: &str = "task-p2p-signup";
const EXTERNAL_ID: &str = "wh-p2p-1";
const PROMPT_TEMPLATE: &str = "fired for {{ doc.external_id }}";

async fn register_replicated_event_schema(node: &EmbeddedNode) {
    // The EventSource introspects the source collection's fields to hydrate
    // `doc.*`. `kind` is indexed so the operator-authored filter
    // (`{ kind: { _eq: "signup" } }`) passes DefraDB's limit-1 probe.
    let sdl = r#"
        type ReplicatedEvent {
            external_id: String
            payload: String
            kind: String @index
        }
    "#;
    node.add_schema(sdl)
        .await
        .expect("add_schema for ReplicatedEvent");
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
    // Generous deadline: the control watcher debounce is 5s plus settle.
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

async fn write_replicated_event(node: &EmbeddedNode, external_id: &str, kind: &str) -> String {
    let escaped_external_id = escape_graphql_string(external_id);
    let escaped_kind = escape_graphql_string(kind);
    // Dynamically-added collections on DefraDB expose `add_<Collection>` as
    // their insertion mutation alias (rather than `create_<Collection>`),
    // returning an array of rows with `_docID`.
    let mutation = format!(
        r#"mutation {{
            add_ReplicatedEvent(input: {{
                external_id: "{escaped_external_id}",
                payload: "{{}}",
                kind: "{escaped_kind}"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "add_ReplicatedEvent failed: {:?}",
        response.errors
    );
    let data = response
        .data
        .as_ref()
        .expect("add_ReplicatedEvent response missing data");
    let field = data
        .get("add_ReplicatedEvent")
        .or_else(|| data.get("create_ReplicatedEvent"))
        .unwrap_or_else(|| panic!("add_/create_ReplicatedEvent key missing; data={data:?}"));
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
        .unwrap_or_else(|| panic!("ReplicatedEvent mutation returned no _docID: {field}"))
}

/// Direct query of the source collection on a node — used as the
/// replication-vs-firing diagnostic when the AgentRequest poll times out.
async fn query_replicated_events(node: &EmbeddedNode) -> Vec<Value> {
    let response = node
        .execute(r#"{ ReplicatedEvent { external_id kind } }"#)
        .await;
    response
        .data
        .as_ref()
        .and_then(|data| data.get("ReplicatedEvent"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

// --- Two-node P2P replicator plumbing (copied verbatim from
// `state_machine_conformance/r5_cross_deployment.rs` — these helpers cannot be
// `use`d across the integration-test crates, so they are duplicated here). ---

async fn install_one_way_replicator(
    sender: &EmbeddedNode,
    receiver: &EmbeddedNode,
    collections: &[&str],
) {
    let sender_addr = wait_for_listen_addr(sender).await;
    let receiver_addr = wait_for_listen_addr(receiver).await;
    let sender_p2p = sender.p2p().expect("sender p2p");
    let receiver_p2p = receiver.p2p().expect("receiver p2p");

    sender_p2p
        .connect_peer(&receiver_addr)
        .await
        .expect("connect sender to receiver");
    wait_for_connected_peer(sender).await;
    wait_for_connected_peer(receiver).await;

    let collection_names = collections
        .iter()
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    sender_p2p
        .add_collections(collection_names.clone())
        .await
        .expect("add sender p2p collections");
    receiver_p2p
        .add_collections(collection_names.clone())
        .await
        .expect("add receiver p2p collections");
    // DefraDB needs both the sender-side push target and the receiver-side
    // authorization record. The data-flow under test remains sender -> receiver.
    receiver_p2p
        .add_replicator(
            collection_names.clone(),
            Some(&sender_addr),
            Default::default(),
            Vec::new(),
            None,
        )
        .await
        .expect("authorize sender as receiver-side replicator");
    sender_p2p
        .add_replicator(
            collection_names,
            Some(&receiver_addr),
            Default::default(),
            Vec::new(),
            None,
        )
        .await
        .expect("install sender to receiver replicator");
}

async fn wait_for_listen_addr(node: &EmbeddedNode) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let addrs = node
            .p2p()
            .expect("p2p should be enabled")
            .listen_addresses()
            .await
            .expect("listen addresses");
        if let Some(addr) = addrs.first() {
            return addr.clone();
        }
        if Instant::now() >= deadline {
            panic!("node never exposed a P2P listen address; last_addrs={addrs:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_connected_peer(node: &EmbeddedNode) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let peers = node
            .p2p()
            .expect("p2p should be enabled")
            .connected_peers()
            .await
            .expect("connected peers");
        if !peers.is_empty() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("node never reported a connected peer; last_peers={peers:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Two-node P2P: a `ReplicatedEvent` doc written on node A replicates to node B
/// and must fire B's `EventTrigger` as a `created` event, landing exactly one
/// `AgentRequest` on B with the rendered prompt and matching trigger lineage.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p2p_replicated_doc_fires_event_trigger() {
    // Node A: bare writer node (P2P enabled, no agent).
    let db_writer = test_p2p_db("event-trigger-p2p-writer").await;
    register_replicated_event_schema(db_writer.node.as_ref()).await;

    // Node B: the agent node (P2P enabled).
    let db_agent = test_p2p_db("event-trigger-p2p-agent").await;
    register_replicated_event_schema(db_agent.node.as_ref()).await;

    let identity = Arc::new(test_identity("event-trigger-p2p-agent"));
    let mock_endpoint = MockModelEndpoint::start("default").unwrap();
    bind_default_behavior_backend(
        db_agent.node.as_ref(),
        identity.did(),
        "backend-event-trigger-p2p",
        mock_endpoint.endpoint(),
    )
    .await;

    let agent = Gents::from_default_behavior_documents(
        db_agent.node.clone(),
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

    // Baseline reconcile — the generation our post-insert reconcile must exceed.
    let startup = wait_for_runtime_snapshot(db_agent.node.as_ref(), &agent_did, |s| {
        s.process_state == "ready" && s.reconcile_phase == "idle" && s.active_generation >= 1
    })
    .await;
    let initial_generation = startup.active_generation;

    // Task + EventTrigger on node B, watching ReplicatedEvent.
    create_task(
        db_agent.node.as_ref(),
        TASK_ID,
        &default_behavior_id,
        PROMPT_TEMPLATE,
    )
    .await;
    create_event_trigger_with_filter(
        db_agent.node.as_ref(),
        TRIGGER_ID,
        TASK_ID,
        "ReplicatedEvent",
        "created",
        r#"{ kind: { _eq: "signup" } }"#,
    )
    .await;
    wait_for_runtime_snapshot(db_agent.node.as_ref(), &agent_did, |s| {
        s.process_state == "ready"
            && s.reconcile_phase == "idle"
            && s.active_generation > initial_generation
            && s.last_reconcile_result == "applied"
    })
    .await;

    // Establish A -> B replication for ReplicatedEvent AFTER the trigger is live
    // so the merged doc is a fresh first-observation on B.
    install_one_way_replicator(
        db_writer.node.as_ref(),
        db_agent.node.as_ref(),
        &["ReplicatedEvent"],
    )
    .await;

    // Write the source doc on node A; it must replicate to B and fire B's trigger.
    let source_doc_id =
        write_replicated_event(db_writer.node.as_ref(), EXTERNAL_ID, "signup").await;

    // Assert B materialized exactly one event-driven AgentRequest with the
    // rendered template. If this poll times out, we run a replication-vs-firing
    // diagnostic to distinguish a plumbing gap from the real product gap.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let requests = loop {
        let rows = query_agent_requests_for_trigger(db_agent.node.as_ref(), TRIGGER_ID).await;
        if !rows.is_empty() {
            break rows;
        }
        if tokio::time::Instant::now() >= deadline {
            // Diagnostic: did the doc replicate to B at all?
            let replicated_on_b = query_replicated_events(db_agent.node.as_ref()).await;
            let replicated_on_a = query_replicated_events(db_writer.node.as_ref()).await;
            panic!(
                "timed out: P2P-replicated ReplicatedEvent did not fire B's trigger \
                 (source_doc_id={source_doc_id}).\n\
                 DIAGNOSTIC: ReplicatedEvent on node A (writer)={replicated_on_a:?}\n\
                 DIAGNOSTIC: ReplicatedEvent on node B (agent)={replicated_on_b:?}\n\
                 If B's list is NON-EMPTY, the doc REPLICATED but the trigger did NOT \
                 FIRE -> real product gap (Task A2 territory). If B's list is EMPTY, \
                 the doc never replicated -> test-plumbing gap."
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        requests.len(),
        1,
        "expected exactly one AgentRequest from a single replicated ReplicatedEvent, got {}: {:?}",
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

    // The runtime-owned bookkeeping writeback should also land on B's trigger,
    // referencing the replicated source doc's id.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let fired = loop {
        let row = fetch_event_trigger(db_agent.node.as_ref(), TRIGGER_ID).await;
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
        "last_fired_source_doc_id should match the replicated ReplicatedEvent docID: {fired:?}"
    );
    assert!(
        fired.last_error.as_deref().unwrap_or("").is_empty(),
        "last_error must be cleared on a successful fire: {fired:?}"
    );
    // Apply-owned fields must not be clobbered by the runtime writeback.
    assert_eq!(fired.task_id.as_deref(), Some(TASK_ID));
    assert_eq!(fired.source_collection.as_deref(), Some("ReplicatedEvent"));
    assert_eq!(fired.event_kind.as_deref(), Some("created"));
    assert_eq!(fired.enabled, Some(true));
    assert_eq!(fired.concurrency.as_deref(), Some("serial"));

    let _ = shutdown_tx.send(true);
    handle.await.unwrap().unwrap();
}
