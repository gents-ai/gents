use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use gents::{DocumentRuntimeOptions, Gents};
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use gents_tools::surface::ToolCeiling;

use crate::support::fixtures::bind_managed_agent_backend;
use crate::support::snapshots::{
    fetch_runtime_snapshot_as, is_routed_ready_after, RuntimeSnapshot,
};
use crate::support::{managed_agent_fixture, test_db, ManagedAgentFixture};

const UNUSED_BACKEND_ENDPOINT: &str = "http://127.0.0.1:9/v1";

async fn apply_documents(
    access: &ConfigAccess,
    documents: Vec<(gents_store::collection::Collection, serde_json::Value)>,
) {
    use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
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
    access
        .transact_write("test.automation_configuration", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
}

async fn create_task(
    access: &ConfigAccess,
    owner: &str,
    task_id: &str,
    agent_did: &str,
    prompt_template: &str,
) {
    apply_documents(access, vec![(gents_store::collection::Collection::Task, serde_json::json!({"node_did":owner,"task_id":task_id,"display_name":task_id,"agent_did":agent_did,"prompt_template":prompt_template}))]).await;
}

fn schedule_trigger(
    owner: &str,
    schedule_id: &str,
    task_id: &str,
    enabled: bool,
) -> serde_json::Value {
    serde_json::json!({
        "node_did": owner,
        "trigger_id": schedule_id,
        "task_id": task_id,
        "source": {"kind": "schedule", "schedule_id": schedule_id},
        "enabled": enabled,
        "concurrency": "serial",
    })
}

async fn create_schedule(access: &ConfigAccess, owner: &str, schedule_id: &str, task_id: &str) {
    apply_documents(
        access,
        vec![
            (
                gents_store::collection::Collection::Schedule,
                serde_json::json!({"node_did":owner,"schedule_id":schedule_id,"cadence":{"kind":"interval","interval_secs":60}}),
            ),
            (
                gents_store::collection::Collection::Trigger,
                schedule_trigger(owner, schedule_id, task_id, true),
            ),
        ],
    )
    .await;
}

/// The configuring User edits the Trigger's desired state.
async fn set_schedule_trigger_enabled(
    access: &ConfigAccess,
    owner: &str,
    trigger_id: &str,
    task_id: &str,
    enabled: bool,
) {
    apply_documents(
        access,
        vec![(
            gents_store::collection::Collection::Trigger,
            schedule_trigger(owner, trigger_id, task_id, enabled),
        )],
    )
    .await;
}

/// The scheduling cursor is the runtime's own observation, written by the
/// Node runtime that owns the trigger cursor.
async fn set_trigger_cursor(
    db: &crate::support::TestDb,
    owner: &str,
    trigger_id: &str,
    next_run_at: &str,
) {
    let mutation = format!(
        r#"mutation {{
            update_Trigger(
                filter: {{
                    node_did: {{ _eq: "{}" }},
                    trigger_id: {{ _eq: "{}" }}
                }},
                input: {{ next_run_at: "{}" }}
            ) {{ _docID }}
        }}"#,
        escape_graphql_string(owner),
        escape_graphql_string(trigger_id),
        escape_graphql_string(next_run_at),
    );
    let runtime = ConfigAccess::local_as(
        db.node.clone(),
        db.node_identity.did().parse().expect("runtime DID"),
    );
    let response = runtime
        .write("test.schedule_trigger_cursor", &mutation)
        .await
        .unwrap();
    let updated = match response.pointer("/data/update_Trigger") {
        Some(serde_json::Value::Object(_)) => true,
        Some(serde_json::Value::Array(rows)) => rows.len() == 1,
        _ => false,
    };
    assert!(
        updated,
        "trigger cursor update must match exactly one owner-scoped document: {response}"
    );
}

async fn create_event_trigger(
    access: &ConfigAccess,
    owner: &str,
    trigger_id: &str,
    task_id: &str,
    source_collection: &str,
    event_kind: &str,
) {
    apply_documents(access, vec![
        (gents_store::collection::Collection::EventSource, serde_json::json!({"node_did":owner,"event_source_id":trigger_id,"source_collection":source_collection,"event_kind":event_kind})),
        (gents_store::collection::Collection::Trigger, serde_json::json!({"node_did":owner,"trigger_id":trigger_id,"task_id":task_id,"source":{"kind":"event","event_source_id":trigger_id},"concurrency":"serial"})),
    ]).await;
}

async fn wait_for_runtime_snapshot<F>(
    access: &ConfigAccess,
    node_did: &str,
    predicate: F,
) -> RuntimeSnapshot
where
    F: Fn(&RuntimeSnapshot) -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let snapshot = fetch_runtime_snapshot_as(access, node_did).await;
        if snapshot.as_ref().is_some_and(&predicate) {
            return snapshot.unwrap();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for runtime snapshot for {node_did}; last snapshot: {snapshot:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Scheduled requests belong to the Agent that runs them.
fn agent_reader(db: &crate::support::TestDb, fixture: &ManagedAgentFixture) -> ConfigAccess {
    ConfigAccess::local_managed_agent(
        db.node.clone(),
        fixture.agent.did().parse().expect("Agent DID"),
        fixture.agent.as_ref(),
    )
    .expect("working Agent reader")
}

async fn fetch_schedule_agent_requests(
    access: &ConfigAccess,
    owner: &str,
    trigger_id: &str,
) -> Vec<serde_json::Value> {
    let response = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ node_did: {{ _eq: "{}" }}, caused_by_trigger_id: {{ _eq: "{}" }}, caused_by_trigger_kind: {{ _eq: "schedule" }} }}, limit: 2) {{ content agent_did execution_origin caused_by_trigger_id caused_by_trigger_kind }} }}"#,
            escape_graphql_string(owner),
            escape_graphql_string(trigger_id),
        ))
        .await
        .unwrap();
    response["data"]["AgentRequest"].as_array().unwrap().clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schedule_insert_and_trigger_disable_bump_active_generation() {
    let db = test_db("schedule-snapshot-reconcile").await;
    let fixture = managed_agent_fixture(&db).await;
    let identity = db.node_identity.clone();
    bind_managed_agent_backend(
        &db,
        &fixture,
        "backend-schedule-snapshot-reconcile",
        UNUSED_BACKEND_ENDPOINT,
        "default",
    )
    .await;
    let agent = Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let node_did = agent.node_did().to_string();
    let default_agent_did = agent.default_agent_did().to_string();

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));

    let startup = wait_for_runtime_snapshot(&fixture.access, &node_did, |snapshot| {
        is_routed_ready_after(snapshot, 0)
    })
    .await;
    let initial_generation = startup.active_generation;
    assert!(
        startup.last_reconcile_error.is_empty(),
        "startup reconcile should be clean, got error={:?}",
        startup.last_reconcile_error
    );

    create_task(
        &fixture.access,
        &node_did,
        "task-reconcile-alpha",
        &default_agent_did,
        "alpha prompt",
    )
    .await;
    create_schedule(
        &fixture.access,
        &node_did,
        "schedule-reconcile-alpha",
        "task-reconcile-alpha",
    )
    .await;

    let reconciled = wait_for_runtime_snapshot(&fixture.access, &node_did, |snapshot| {
        is_routed_ready_after(snapshot, initial_generation)
    })
    .await;
    assert_eq!(reconciled.default_agent_did, default_agent_did);
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

    set_schedule_trigger_enabled(
        &fixture.access,
        &node_did,
        "schedule-reconcile-alpha",
        "task-reconcile-alpha",
        false,
    )
    .await;
    let disabled = wait_for_runtime_snapshot(&fixture.access, &node_did, |snapshot| {
        is_routed_ready_after(snapshot, reconciled.active_generation)
    })
    .await;
    assert!(
        disabled.last_reconcile_error.is_empty(),
        "post-disable reconcile should be clean, got error={:?}",
        disabled.last_reconcile_error
    );
    let due = (Utc::now() - ChronoDuration::seconds(60)).to_rfc3339();
    set_trigger_cursor(&db, &node_did, "schedule-reconcile-alpha", &due).await;

    let requests_after_reconcile = fetch_schedule_agent_requests(
        &agent_reader(&db, &fixture),
        &node_did,
        "schedule-reconcile-alpha",
    )
    .await
    .len();
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert_eq!(
        fetch_schedule_agent_requests(
            &agent_reader(&db, &fixture),
            &node_did,
            "schedule-reconcile-alpha"
        )
        .await
        .len(),
        requests_after_reconcile,
        "a disabled Trigger must stop firing after its generation is active"
    );

    let _ = shutdown_tx.send(true);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_source_trigger_insert_bumps_active_generation() {
    let db = test_db("event-trigger-snapshot-reconcile").await;
    let fixture = managed_agent_fixture(&db).await;
    let identity = db.node_identity.clone();
    bind_managed_agent_backend(
        &db,
        &fixture,
        "backend-event-trigger-snapshot-reconcile",
        UNUSED_BACKEND_ENDPOINT,
        "default",
    )
    .await;
    let agent = Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let node_did = agent.node_did().to_string();
    let default_agent_did = agent.default_agent_did().to_string();

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));

    let startup = wait_for_runtime_snapshot(&fixture.access, &node_did, |snapshot| {
        is_routed_ready_after(snapshot, 0)
    })
    .await;
    let initial_generation = startup.active_generation;
    assert!(
        startup.last_reconcile_error.is_empty(),
        "startup reconcile should be clean, got error={:?}",
        startup.last_reconcile_error
    );

    create_task(
        &fixture.access,
        &node_did,
        "task-event-trigger-alpha",
        &default_agent_did,
        "alpha prompt",
    )
    .await;
    create_event_trigger(
        &fixture.access,
        &node_did,
        "event-trigger-alpha",
        "task-event-trigger-alpha",
        "AgentMessage",
        "created",
    )
    .await;

    let reconciled = wait_for_runtime_snapshot(&fixture.access, &node_did, |snapshot| {
        is_routed_ready_after(snapshot, initial_generation)
    })
    .await;
    assert_eq!(reconciled.default_agent_did, default_agent_did);
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

    let _ = shutdown_tx.send(true);
    handle.await.unwrap().unwrap();
}
