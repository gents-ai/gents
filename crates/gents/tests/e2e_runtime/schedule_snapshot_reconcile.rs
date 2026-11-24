use chrono::{Duration as ChronoDuration, Utc};
use gents::{DocumentRuntimeOptions, Gents};
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use gents_tools::surface::ToolCeiling;

use crate::support::fixtures::bind_managed_agent_backend;
use crate::support::snapshots::{is_routed_ready_after, wait_for_runtime_snapshot_as};
use crate::support::{managed_agent_fixture, test_db, ManagedAgentFixture};
use gents_store::test_support::rows_when;
use gents_store::test_support::snapshot_reader::SnapshotReader;

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
        .transact_signed_conflict_rebuild("test.automation_configuration", |txn| {
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

/// The interval Trigger is seeded due at once, and a day passes before it is
/// due again; the cron Trigger is seeded at its next yearly run.
async fn create_scheduled_task(
    access: &ConfigAccess,
    owner: &str,
    interval_schedule_id: &str,
    cron_schedule_id: &str,
    task_id: &str,
    agent_did: &str,
    prompt_template: &str,
) {
    use gents_store::collection::Collection;
    apply_documents(
        access,
        vec![
            (
                Collection::Task,
                serde_json::json!({"node_did":owner,"task_id":task_id,"display_name":task_id,"agent_did":agent_did,"prompt_template":prompt_template}),
            ),
            (
                Collection::Schedule,
                serde_json::json!({"node_did":owner,"schedule_id":interval_schedule_id,"cadence":{"kind":"interval","interval_secs":86_400}}),
            ),
            (
                Collection::Trigger,
                schedule_trigger(owner, interval_schedule_id, task_id, true),
            ),
            (
                Collection::Schedule,
                serde_json::json!({"node_did":owner,"schedule_id":cron_schedule_id,"cadence":{"kind":"cron","expression":"0 0 1 1 *","timezone":"UTC"}}),
            ),
            (
                Collection::Trigger,
                schedule_trigger(owner, cron_schedule_id, task_id, true),
            ),
        ],
    )
    .await;
}

fn node_runtime(db: &crate::support::TestDb) -> ConfigAccess {
    ConfigAccess::local_as(
        db.node.clone(),
        db.node_identity.did().parse().expect("runtime DID"),
    )
}

/// Waits until the interval Trigger's seeded fire is recorded and the cron
/// Trigger's cursor is seeded: no fire admitted so far is still in flight,
/// and no seed write can land over a cursor the test writes later.
async fn await_seeded(
    db: &crate::support::TestDb,
    owner: &str,
    interval_trigger_id: &str,
    cron_trigger_id: &str,
) {
    let query = format!(
        r#"{{ Trigger(filter: {{ node_did: {{ _eq: "{}" }}, trigger_id: {{ _in: ["{}", "{}"] }} }}, limit: 3) {{ trigger_id next_run_at last_status fire_count }} }}"#,
        escape_graphql_string(owner),
        escape_graphql_string(interval_trigger_id),
        escape_graphql_string(cron_trigger_id),
    );
    rows_when(&node_runtime(db), "Trigger", &query, |data| {
        let rows = data["Trigger"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let row = |id: &str| rows.iter().find(|row| row["trigger_id"] == id);
        row(interval_trigger_id).is_some_and(|row| {
            row["last_status"] == "fired" && row["fire_count"].as_i64() == Some(1)
        }) && row(cron_trigger_id).is_some_and(|row| row["next_run_at"].is_string())
    })
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
    let response = node_runtime(db)
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

/// Scheduled requests belong to the Agent that runs them.
fn agent_reader(db: &crate::support::TestDb, fixture: &ManagedAgentFixture) -> ConfigAccess {
    ConfigAccess::local_managed_agent(
        db.node.clone(),
        fixture.agent.did().parse().expect("Agent DID"),
        fixture.agent.as_ref(),
    )
    .expect("working Agent reader")
}

fn schedule_requests_query(owner: &str, trigger_id: &str) -> String {
    format!(
        r#"{{ AgentRequest(filter: {{ node_did: {{ _eq: "{}" }}, caused_by_trigger_id: {{ _eq: "{}" }}, caused_by_trigger_kind: {{ _eq: "schedule" }} }}, limit: 3) {{ request_id }} }}"#,
        escape_graphql_string(owner),
        escape_graphql_string(trigger_id),
    )
}

async fn schedule_request_count(access: &ConfigAccess, owner: &str, trigger_id: &str) -> usize {
    access
        .read_data(&schedule_requests_query(owner, trigger_id))
        .await["AgentRequest"]
        .as_array()
        .expect("AgentRequest rows")
        .len()
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

    let startup = wait_for_runtime_snapshot_as(&fixture.access, &node_did, |snapshot| {
        is_routed_ready_after(snapshot, 0)
    })
    .await;
    let initial_generation = startup.active_generation;
    assert!(
        startup.last_reconcile_error.is_empty(),
        "startup reconcile should be clean, got error={:?}",
        startup.last_reconcile_error
    );

    create_scheduled_task(
        &fixture.access,
        &node_did,
        "schedule-reconcile-alpha",
        "schedule-reconcile-witness",
        "task-reconcile-alpha",
        &default_agent_did,
        "alpha prompt",
    )
    .await;

    let reconciled = wait_for_runtime_snapshot_as(&fixture.access, &node_did, |snapshot| {
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

    // The fire admitted under the enabling generation commits before the
    // disable, so the count below is final for that generation.
    await_seeded(
        &db,
        &node_did,
        "schedule-reconcile-alpha",
        "schedule-reconcile-witness",
    )
    .await;
    let reader = agent_reader(&db, &fixture);
    assert_eq!(
        schedule_request_count(&reader, &node_did, "schedule-reconcile-alpha").await,
        1,
        "an enabled interval Trigger fires once when first seen"
    );

    set_schedule_trigger_enabled(
        &fixture.access,
        &node_did,
        "schedule-reconcile-alpha",
        "task-reconcile-alpha",
        false,
    )
    .await;
    let disabled = wait_for_runtime_snapshot_as(&fixture.access, &node_did, |snapshot| {
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
    set_trigger_cursor(&db, &node_did, "schedule-reconcile-witness", &due).await;

    // The schedule source dispatches one intent at a time, so the witness
    // fire commits after every intent loaded from the disabled cursor.
    rows_when(
        &reader,
        "AgentRequest",
        &schedule_requests_query(&node_did, "schedule-reconcile-witness"),
        |data| {
            data["AgentRequest"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty())
        },
    )
    .await;
    assert_eq!(
        schedule_request_count(&reader, &node_did, "schedule-reconcile-alpha").await,
        1,
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

    let startup = wait_for_runtime_snapshot_as(&fixture.access, &node_did, |snapshot| {
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

    let reconciled = wait_for_runtime_snapshot_as(&fixture.access, &node_did, |snapshot| {
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
