use std::sync::Arc;
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use gents::graphql::escape_graphql_string;
use gents::{AgentIdentity, DocumentRuntimeOptions, Gents, KeyIdentity, ToolCeiling};

use crate::support::fixtures::bind_default_behavior_backend;
use crate::support::mock_endpoint::MockModelEndpoint;
use crate::support::snapshots::{fetch_runtime_snapshot, RuntimeSnapshot};
use crate::support::test_db;

const UNUSED_BACKEND_ENDPOINT: &str = "http://127.0.0.1:9/v1";

fn test_identity(name: &str) -> KeyIdentity {
    let path = std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4()));
    KeyIdentity::load_or_create(path, None).unwrap()
}

async fn apply_documents(
    node: &gents::defra_node::EmbeddedNode,
    documents: Vec<(gents::Collection, serde_json::Value)>,
) {
    use gents::config_client::{ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan};
    let plan = DesiredStateApplyPlan::new(
        documents
            .into_iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
    )
    .unwrap();
    ConfigAccess::transact_local(node, None, "test.automation_configuration", |txn| {
        let plan = &plan;
        Box::pin(async move { gents::config_client::apply_desired_state_plan(txn, plan).await })
    })
    .await
    .unwrap();
}

async fn create_task(
    node: &gents::defra_node::EmbeddedNode,
    owner: &str,
    task_id: &str,
    behavior_id: &str,
    prompt_template: &str,
) {
    apply_documents(node, vec![(gents::Collection::Task, serde_json::json!({"agent_did":owner,"task_id":task_id,"display_name":task_id,"behavior_id":behavior_id,"prompt_template":prompt_template}))]).await;
}

async fn create_schedule(
    node: &gents::defra_node::EmbeddedNode,
    owner: &str,
    schedule_id: &str,
    task_id: &str,
) {
    let trigger = serde_json::json!({
        "agent_did": owner,
        "trigger_id": schedule_id,
        "task_id": task_id,
        "source": {"kind": "schedule", "schedule_id": schedule_id},
        "enabled": true,
        "concurrency": "serial",
    });
    apply_documents(
        node,
        vec![
            (
                gents::Collection::Schedule,
                serde_json::json!({"agent_did":owner,"schedule_id":schedule_id,"cadence":{"kind":"interval","interval_secs":60}}),
            ),
            (gents::Collection::Trigger, trigger),
        ],
    )
    .await;
}

async fn set_trigger_enabled_and_cursor(
    node: &gents::defra_node::EmbeddedNode,
    owner: &str,
    trigger_id: &str,
    enabled: bool,
    next_run_at: Option<&str>,
) {
    let next_run_at = next_run_at
        .map(escape_graphql_string)
        .map(|value| format!(r#", next_run_at: "{value}""#))
        .unwrap_or_default();
    let mutation = format!(
        r#"mutation {{
            update_Trigger(
                filter: {{
                    agent_did: {{ _eq: "{}" }},
                    trigger_id: {{ _eq: "{}" }}
                }},
                input: {{ enabled: {enabled}{next_run_at} }}
            ) {{ _docID }}
        }}"#,
        escape_graphql_string(owner),
        escape_graphql_string(trigger_id),
    );
    let response =
        gents::ConfigAccess::write_local(node, "test.disable_schedule_trigger", &mutation)
            .await
            .unwrap();
    let updated = match response.pointer("/data/update_Trigger") {
        Some(serde_json::Value::Object(_)) => true,
        Some(serde_json::Value::Array(rows)) => rows.len() == 1,
        _ => false,
    };
    assert!(
        updated,
        "trigger update must match exactly one owner-scoped document: {response}"
    );
}

async fn create_event_trigger(
    node: &gents::defra_node::EmbeddedNode,
    owner: &str,
    trigger_id: &str,
    task_id: &str,
    source_collection: &str,
    event_kind: &str,
) {
    apply_documents(node, vec![
        (gents::Collection::EventSource, serde_json::json!({"agent_did":owner,"event_source_id":trigger_id,"source_collection":source_collection,"event_kind":event_kind})),
        (gents::Collection::Trigger, serde_json::json!({"agent_did":owner,"trigger_id":trigger_id,"task_id":task_id,"source":{"kind":"event","event_source_id":trigger_id},"concurrency":"serial"})),
    ]).await;
}

async fn wait_for_runtime_snapshot<F>(
    node: &gents::defra_node::EmbeddedNode,
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

async fn fetch_schedule_agent_requests(
    node: &gents::defra_node::EmbeddedNode,
    owner: &str,
    trigger_id: &str,
) -> Vec<serde_json::Value> {
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ agent_did: {{ _eq: "{}" }}, caused_by_trigger_id: {{ _eq: "{}" }}, caused_by_trigger_kind: {{ _eq: "schedule" }} }}, limit: 2) {{ content behavior_id execution_origin caused_by_trigger_id caused_by_trigger_kind }} }}"#,
            escape_graphql_string(owner),
            escape_graphql_string(trigger_id),
        ))
        .await;
    gents::graphql::rows::<serde_json::Value>(&response, "AgentRequest").unwrap()
}

async fn fetch_trigger_observation(
    node: &gents::defra_node::EmbeddedNode,
    owner: &str,
    trigger_id: &str,
) -> serde_json::Value {
    let response = node
        .execute(&format!(
            r#"{{ Trigger(filter: {{ agent_did: {{ _eq: "{}" }}, trigger_id: {{ _eq: "{}" }} }}, limit: 2) {{ task_id source enabled next_run_at last_status last_error fire_count }} }}"#,
            escape_graphql_string(owner),
            escape_graphql_string(trigger_id),
        ))
        .await;
    let rows = gents::graphql::rows::<serde_json::Value>(&response, "Trigger").unwrap();
    assert_eq!(rows.len(), 1, "owner-scoped Trigger must be unique");
    rows.into_iter().next().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schedule_insert_and_trigger_disable_bump_active_generation() {
    let db = test_db("schedule-snapshot-reconcile").await;
    let identity = Arc::new(test_identity("schedule-snapshot-reconcile"));
    bind_default_behavior_backend(
        db.node.as_ref(),
        identity.did(),
        "backend-schedule-snapshot-reconcile",
        UNUSED_BACKEND_ENDPOINT,
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
        &agent_did,
        "task-reconcile-alpha",
        &default_behavior_id,
        "alpha prompt",
    )
    .await;
    create_schedule(
        db.node.as_ref(),
        &agent_did,
        "schedule-reconcile-alpha",
        "task-reconcile-alpha",
    )
    .await;

    let reconciled = wait_for_runtime_snapshot(db.node.as_ref(), &agent_did, |snapshot| {
        snapshot.process_state == "ready"
            && snapshot.reconcile_phase == "idle"
            && snapshot.active_generation > initial_generation
            && snapshot.last_reconcile_result == "applied"
    })
    .await;
    assert_eq!(reconciled.default_behavior_id, default_behavior_id);
    assert!(
        reconciled.last_reconcile_error.is_empty(),
        "post-insert reconcile should be clean, got error={:?}",
        reconciled.last_reconcile_error
    );
    assert!(
        reconciled.active_generation > initial_generation,
        "active_generation should bump after Task+Schedule insert (initial={initial_generation}, observed={})",
        reconciled.active_generation
    );

    let due = (Utc::now() - ChronoDuration::seconds(60)).to_rfc3339();
    set_trigger_enabled_and_cursor(
        db.node.as_ref(),
        &agent_did,
        "schedule-reconcile-alpha",
        false,
        Some(&due),
    )
    .await;
    let disabled = wait_for_runtime_snapshot(db.node.as_ref(), &agent_did, |snapshot| {
        snapshot.process_state == "ready"
            && snapshot.reconcile_phase == "idle"
            && snapshot.active_generation > reconciled.active_generation
            && snapshot.last_reconcile_result == "applied"
    })
    .await;
    assert!(
        disabled.last_reconcile_error.is_empty(),
        "post-disable reconcile should be clean, got error={:?}",
        disabled.last_reconcile_error
    );

    // A due cursor can race the configuration reconcile. Once the disabled
    // generation is published, however, the active schedule set must stop
    // producing requests.
    let requests_after_reconcile =
        fetch_schedule_agent_requests(db.node.as_ref(), &agent_did, "schedule-reconcile-alpha")
            .await
            .len();
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert_eq!(
        fetch_schedule_agent_requests(db.node.as_ref(), &agent_did, "schedule-reconcile-alpha")
            .await
            .len(),
        requests_after_reconcile,
        "a disabled Trigger must stop firing after its generation is active"
    );

    let _ = shutdown_tx.send(true);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduled_fire_persists_task_content_and_trigger_writeback() {
    let db = test_db("scheduled-fire-writeback").await;
    let identity = Arc::new(test_identity("scheduled-fire-writeback"));
    let endpoint = MockModelEndpoint::start("default").unwrap();
    bind_default_behavior_backend(
        db.node.as_ref(),
        identity.did(),
        "backend-scheduled-fire-writeback",
        endpoint.endpoint(),
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

    const TASK_ID: &str = "task-scheduled-fire";
    const TRIGGER_ID: &str = "trigger-scheduled-fire";
    const TASK_PROMPT: &str = "render this exact configured schedule task";
    create_task(
        db.node.as_ref(),
        &agent_did,
        TASK_ID,
        &default_behavior_id,
        TASK_PROMPT,
    )
    .await;
    let due = (Utc::now() - ChronoDuration::seconds(2)).to_rfc3339();
    create_schedule(db.node.as_ref(), &agent_did, TRIGGER_ID, TASK_ID).await;

    let reconciled = wait_for_runtime_snapshot(db.node.as_ref(), &agent_did, |snapshot| {
        snapshot.process_state == "ready"
            && snapshot.reconcile_phase == "idle"
            && snapshot.active_generation > startup.active_generation
            && snapshot.last_reconcile_result == "applied"
    })
    .await;
    assert!(
        reconciled.last_reconcile_error.is_empty(),
        "schedule reconcile should be clean: {:?}",
        reconciled.last_reconcile_error
    );

    set_trigger_enabled_and_cursor(db.node.as_ref(), &agent_did, TRIGGER_ID, true, Some(&due))
        .await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let request = loop {
        let requests =
            fetch_schedule_agent_requests(db.node.as_ref(), &agent_did, TRIGGER_ID).await;
        if let Some(request) = requests.into_iter().next() {
            break request;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for the canonical schedule fire"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(request["content"], TASK_PROMPT);
    assert_eq!(request["behavior_id"], default_behavior_id);
    assert_eq!(request["execution_origin"], "scheduled");
    assert_eq!(request["caused_by_trigger_id"], TRIGGER_ID);
    assert_eq!(request["caused_by_trigger_kind"], "schedule");

    let writeback_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let trigger = loop {
        let trigger = fetch_trigger_observation(db.node.as_ref(), &agent_did, TRIGGER_ID).await;
        if trigger["last_status"] == "fired" {
            break trigger;
        }
        assert!(
            tokio::time::Instant::now() < writeback_deadline,
            "scheduled request persisted without Trigger writeback: {trigger}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(trigger["task_id"], TASK_ID);
    assert_eq!(trigger["source"]["kind"], "schedule");
    assert_eq!(trigger["source"]["schedule_id"], TRIGGER_ID);
    assert_eq!(trigger["enabled"], true);
    assert_eq!(trigger["fire_count"], 1);
    assert!(trigger["last_error"]
        .as_str()
        .unwrap_or_default()
        .is_empty());
    assert_ne!(trigger["next_run_at"], due);

    let _ = shutdown_tx.send(true);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_source_trigger_insert_bumps_active_generation() {
    let db = test_db("event-trigger-snapshot-reconcile").await;
    let identity = Arc::new(test_identity("event-trigger-snapshot-reconcile"));
    bind_default_behavior_backend(
        db.node.as_ref(),
        identity.did(),
        "backend-event-trigger-snapshot-reconcile",
        UNUSED_BACKEND_ENDPOINT,
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
        &agent_did,
        "task-event-trigger-alpha",
        &default_behavior_id,
        "alpha prompt",
    )
    .await;
    create_event_trigger(
        db.node.as_ref(),
        &agent_did,
        "event-trigger-alpha",
        "task-event-trigger-alpha",
        "AgentMessage",
        "created",
    )
    .await;

    let reconciled = wait_for_runtime_snapshot(db.node.as_ref(), &agent_did, |snapshot| {
        snapshot.process_state == "ready"
            && snapshot.reconcile_phase == "idle"
            && snapshot.active_generation > initial_generation
            && snapshot.last_reconcile_result == "applied"
    })
    .await;
    assert_eq!(reconciled.default_behavior_id, default_behavior_id);
    assert!(
        reconciled.last_reconcile_error.is_empty(),
        "post-insert reconcile should be clean, got error={:?}",
        reconciled.last_reconcile_error
    );
    assert!(
        reconciled.active_generation > initial_generation,
        "active_generation should bump after Task+EventSource+Trigger insert (initial={initial_generation}, observed={})",
        reconciled.active_generation
    );
    assert_eq!(
        reconciled.last_reconcile_result, "applied",
        "last_reconcile_result should be 'applied' after EventSource+Trigger insert"
    );

    let _ = shutdown_tx.send(true);
    handle.await.unwrap().unwrap();
}
