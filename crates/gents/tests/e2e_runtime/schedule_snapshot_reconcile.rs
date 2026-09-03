use std::sync::Arc;
use std::time::Duration;

use gents::{
    ensure_agent_principal, graphql::escape_graphql_string, load_agent_behavior,
    upsert_agent_behavior, AgentIdentity, DocumentRuntimeOptions, Gents, KeyIdentity, ToolCeiling,
};

use crate::support::snapshots::{fetch_runtime_snapshot, RuntimeSnapshot};
use crate::support::test_db;

const UNUSED_BACKEND_ENDPOINT: &str = "http://127.0.0.1:9/v1";

fn test_identity(name: &str) -> KeyIdentity {
    let path = std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4()));
    KeyIdentity::load_or_create(path, None).unwrap()
}

async fn bind_default_behavior_backend(
    node: &gents::defra_node::EmbeddedNode,
    agent_did: &str,
    backend_id: &str,
    endpoint: &str,
) {
    let bootstrap = ensure_agent_principal(node, agent_did).await.unwrap();
    let escaped_backend_id = escape_graphql_string(backend_id);
    let escaped_endpoint = escape_graphql_string(endpoint);
    let mutation = format!(
        r#"mutation {{
            upsert_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{escaped_backend_id}" }} }},
                add: {{
                    backend_id: "{escaped_backend_id}",
                    name: "{escaped_backend_id}",
                    provider_kind: "OpenAiCompatible",
                    endpoint: "{escaped_endpoint}",
                    max_concurrent: 1,
                    enabled: true,
                    models: ["default"],
                    probe_status: "healthy"
                }},
                update: {{
                    name: "{escaped_backend_id}",
                    endpoint: "{escaped_endpoint}",
                    max_concurrent: 1,
                    enabled: true,
                    probe_status: "healthy"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "upsert InferenceBackend failed: {:?}",
        response.errors
    );

    let mut default_behavior = load_agent_behavior(node, &bootstrap.default_behavior.behavior_id)
        .await
        .unwrap()
        .expect("default behavior document");
    default_behavior.backend_id = Some(backend_id.to_string());
    upsert_agent_behavior(node, &default_behavior)
        .await
        .unwrap();
}

async fn create_task(
    node: &gents::defra_node::EmbeddedNode,
    task_id: &str,
    behavior_id: &str,
    prompt_template: &str,
) {
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

async fn create_schedule(node: &gents::defra_node::EmbeddedNode, schedule_id: &str, task_id: &str) {
    let escaped_schedule_id = escape_graphql_string(schedule_id);
    let escaped_task_id = escape_graphql_string(task_id);
    let mutation = format!(
        r#"mutation {{
            create_Schedule(input: {{
                schedule_id: "{escaped_schedule_id}",
                task_id: "{escaped_task_id}",
                interval_secs: 60,
                enabled: true,
                concurrency: "serial"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create Schedule failed: {:?}",
        response.errors
    );
}

async fn create_event_trigger(
    node: &gents::defra_node::EmbeddedNode,
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
        "task-reconcile-alpha",
        &default_behavior_id,
        "alpha prompt",
    )
    .await;
    create_schedule(
        db.node.as_ref(),
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
        "task-event-trigger-alpha",
        &default_behavior_id,
        "alpha prompt",
    )
    .await;
    create_event_trigger(
        db.node.as_ref(),
        "event-trigger-alpha",
        "task-event-trigger-alpha",
        "AgentMessage",
        "create",
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
