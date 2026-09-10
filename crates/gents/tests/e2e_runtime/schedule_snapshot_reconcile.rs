use std::sync::Arc;
use std::time::Duration;

use gents::{AgentIdentity, DocumentRuntimeOptions, Gents, KeyIdentity, ToolCeiling};

use crate::support::fixtures::bind_default_behavior_backend;
use crate::support::snapshots::{RuntimeSnapshot, fetch_runtime_snapshot};
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
    apply_documents(node, vec![
        (gents::Collection::Schedule, serde_json::json!({"agent_did":owner,"schedule_id":schedule_id,"cadence":{"kind":"interval","interval_secs":60}})),
        (gents::Collection::Trigger, serde_json::json!({"agent_did":owner,"trigger_id":schedule_id,"task_id":task_id,"source":{"kind":"schedule","schedule_id":schedule_id},"concurrency":"serial"})),
    ]).await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schedule_insert_bumps_active_generation() {
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

    let _ = shutdown_tx.send(true);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_trigger_insert_bumps_active_generation() {
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
        "active_generation should bump after Task+EventTrigger insert (initial={initial_generation}, observed={})",
        reconciled.active_generation
    );
    assert_eq!(
        reconciled.last_reconcile_result, "applied",
        "last_reconcile_result should be 'applied' after EventTrigger insert"
    );

    let _ = shutdown_tx.send(true);
    handle.await.unwrap().unwrap();
}
