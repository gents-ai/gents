//! Live ScheduleSource materialization, writeback, and runtime reconfiguration.
//! Source callback persistence and dispatch branches are tested through their
//! actual owners in trigger_engine/tests/{schedule_source,dispatch}.rs.
//! Canonical Schedule -> Trigger -> Task resolution still awaits runtime migration.

use std::sync::Arc;
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use gents::config_client::ConfigAccess;
use gents::graphql::escape_graphql_string;
use gents::{AgentIdentity, DocumentRuntimeOptions, Gents, ToolCeiling};
use serde_json::Value;

use crate::support::fixtures::{bind_default_behavior_backend, test_identity};
use crate::support::interrupt::wait_for_runtime_ready;
use crate::support::mock_endpoint::MockModelEndpoint;
use crate::support::snapshots::fetch_runtime_snapshot;
use crate::support::{test_db, AGENT_NAME};

async fn create_task(
    node: &gents::defra_node::EmbeddedNode,
    task_id: &str,
    behavior_id: &str,
    prompt_template: &str,
    enabled: bool,
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
                enabled: {enabled}
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(!resp.has_errors(), "create Task failed: {:?}", resp.errors);
}

#[allow(clippy::too_many_arguments)]
async fn create_schedule(
    node: &gents::defra_node::EmbeddedNode,
    schedule_id: &str,
    task_id: &str,
    interval_secs: i64,
    enabled: bool,
    concurrency: &str,
    next_run_at: Option<&str>,
) {
    let escaped_schedule_id = escape_graphql_string(schedule_id);
    let escaped_task_id = escape_graphql_string(task_id);
    let escaped_concurrency = escape_graphql_string(concurrency);
    let next_run_at_entry = match next_run_at {
        Some(value) => format!(", next_run_at: \"{}\"", escape_graphql_string(value)),
        None => String::new(),
    };
    let mutation = format!(
        r#"mutation {{
            create_Schedule(input: {{
                schedule_id: "{escaped_schedule_id}",
                task_id: "{escaped_task_id}",
                interval_secs: {interval_secs},
                enabled: {enabled},
                concurrency: "{escaped_concurrency}"{next_run_at_entry}
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create Schedule failed: {:?}",
        resp.errors
    );
}

async fn set_schedule_enabled(
    node: &gents::defra_node::EmbeddedNode,
    schedule_id: &str,
    enabled: bool,
) {
    let escaped_schedule_id = escape_graphql_string(schedule_id);
    let mutation = format!(
        r#"mutation {{
            update_Schedule(
                filter: {{ schedule_id: {{ _eq: "{escaped_schedule_id}" }} }},
                input: {{ enabled: {enabled} }}
            ) {{ _docID }}
        }}"#
    );
    let resp = ConfigAccess::write_local(node, "test.update_schedule_enabled", &mutation)
        .await
        .expect("update Schedule.enabled");
    let updated = match resp.pointer("/data/update_Schedule") {
        Some(value @ Value::Object(_)) => Some(value),
        Some(Value::Array(rows)) if rows.len() == 1 => rows.first(),
        _ => None,
    };
    assert!(
        updated.is_some(),
        "update Schedule.enabled must match exactly one document for {schedule_id}; response: {resp:?}",
    );
}

#[derive(Debug)]
struct ScheduleRow {
    last_status: Option<String>,
    task_id: Option<String>,
}

async fn fetch_schedule_row(
    node: &gents::defra_node::EmbeddedNode,
    schedule_id: &str,
) -> Option<ScheduleRow> {
    let escaped_schedule_id = escape_graphql_string(schedule_id);
    let query = format!(
        r#"{{
            Schedule(
                filter: {{ schedule_id: {{ _eq: "{escaped_schedule_id}" }} }},
                limit: 1
            ) {{ last_status task_id }}
        }}"#
    );
    let resp = node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "fetch Schedule row failed: {:?}",
        resp.errors
    );
    let row = resp
        .data
        .as_ref()
        .and_then(|d| d.get("Schedule"))
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())?;
    Some(ScheduleRow {
        last_status: row
            .get("last_status")
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generation_bump_reconfigures_active_schedules() {
    let db = test_db("schedule-conformance-genbump").await;
    let agent = boot_agent(&db, "schedule-conformance-genbump", "backend-genbump").await;

    let startup_gen = fetch_runtime_snapshot(db.node.as_ref(), &agent.agent_did)
        .await
        .unwrap()
        .active_generation;

    create_task(
        db.node.as_ref(),
        "task-genbump",
        &agent.default_behavior_id,
        "noop",
        true,
    )
    .await;
    create_schedule(
        db.node.as_ref(),
        "sched-genbump",
        "task-genbump",
        60,
        true,
        "serial",
        None,
    )
    .await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let post_insert_gen = loop {
        let snap = fetch_runtime_snapshot(db.node.as_ref(), &agent.agent_did)
            .await
            .unwrap();
        if snap.active_generation > startup_gen && snap.last_reconcile_result == "applied" {
            break snap.active_generation;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "snapshot never re-resolved after Schedule insert; stuck at {startup_gen}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(
        post_insert_gen > startup_gen,
        "post-insert active_generation must exceed startup generation"
    );

    set_schedule_enabled(db.node.as_ref(), "sched-genbump", false).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let post_disable_gen = loop {
        let snap = fetch_runtime_snapshot(db.node.as_ref(), &agent.agent_did)
            .await
            .unwrap();
        if snap.active_generation > post_insert_gen && snap.last_reconcile_result == "applied" {
            break snap.active_generation;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "snapshot never re-resolved after Schedule disable; stuck at {post_insert_gen}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(
        post_disable_gen > post_insert_gen,
        "disabling an active Schedule must bump active_generation again"
    );

    agent.shutdown().await;
}

/// Fetch the materialized AgentRequest for one schedule fire with the fields
/// that expose rendered content, the current behavior binding, and
/// dispatch lineage.
async fn fetch_schedule_agent_request(
    node: &gents::defra_node::EmbeddedNode,
    schedule_id: &str,
) -> Option<serde_json::Value> {
    let escaped = escape_graphql_string(schedule_id);
    let query = format!(
        r#"query {{
            AgentRequest(
                filter: {{
                    caused_by_trigger_id: {{ _eq: "{escaped}" }},
                    caused_by_trigger_kind: {{ _eq: "schedule" }}
                }},
                limit: 1
            ) {{
                content
                behavior_id
                execution_origin
                caused_by_trigger_id
                caused_by_trigger_kind
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "AgentRequest schedule-fire query errored: {:?}",
        resp.errors
    );
    resp.data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())
        .cloned()
}

/// Exercises the current live ScheduleSource and persisted request/writeback
/// owners. The fixture uses the old Schedule->Task SDL; it does not establish
/// the new Schedule->Trigger->Task resolution contract or alternate selection.
#[tokio::test]
async fn live_schedule_fire_persists_task_content_and_writeback() {
    let db = test_db("schedule-conformance-task-selection").await;
    let agent = boot_agent(
        &db,
        "schedule-conformance-task-selection",
        "backend-schedule-task-selection",
    )
    .await;

    const TASK_PROMPT: &str = "task-selection prompt: render through the configured Task";
    create_task(
        db.node.as_ref(),
        "task-schedule-selection",
        &agent.default_behavior_id,
        TASK_PROMPT,
        true,
    )
    .await;
    let past = (Utc::now() - ChronoDuration::seconds(120)).to_rfc3339();
    create_schedule(
        db.node.as_ref(),
        "sched-task-selection",
        "task-schedule-selection",
        60,
        true,
        "serial",
        Some(&past),
    )
    .await;

    // The live ScheduleSource ticks ~1s; the due schedule fires and
    // materializes the request through the configured Task.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let row = loop {
        if let Some(row) =
            fetch_schedule_agent_request(db.node.as_ref(), "sched-task-selection").await
        {
            break row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for the schedule fire to materialize a request"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    assert_eq!(
        row.get("caused_by_trigger_id").and_then(|v| v.as_str()),
        Some("sched-task-selection"),
        "schedule lineage must record the schedule id: {row}"
    );
    assert_eq!(
        row.get("caused_by_trigger_kind").and_then(|v| v.as_str()),
        Some("schedule"),
        "schedule lineage must record the trigger kind: {row}"
    );
    assert_eq!(
        row.get("execution_origin").and_then(|v| v.as_str()),
        Some("scheduled"),
        "schedule-driven fires must be scheduled: {row}"
    );
    assert_eq!(
        row.get("content").and_then(|v| v.as_str()),
        Some(TASK_PROMPT),
        "materialized content must be the configured Task's prompt_template: {row}"
    );
    assert_eq!(
        row.get("behavior_id").and_then(|v| v.as_str()),
        Some(agent.default_behavior_id.as_str()),
        "the request retains the configured default behavior binding: {row}"
    );

    // The live ScheduleSource owns the writeback: firing must persist the
    // applied state, not leave the due row untouched.
    let writeback_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let sched = loop {
        let sched = fetch_schedule_row(db.node.as_ref(), "sched-task-selection")
            .await
            .expect("Schedule doc exists");
        if sched.last_status.as_deref() == Some("fired") {
            break sched;
        }
        assert!(
            tokio::time::Instant::now() < writeback_deadline,
            "schedule request was created but fired writeback did not arrive: {sched:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        sched.last_status.as_deref(),
        Some("fired"),
        "live ScheduleSource must persist the fired writeback; got {sched:?}"
    );
    assert_eq!(
        sched.task_id.as_deref(),
        Some("task-schedule-selection"),
        "apply-owned task_id must remain the configured task"
    );

    agent.shutdown().await;
}
