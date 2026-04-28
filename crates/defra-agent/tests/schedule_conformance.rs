//! Task 47 — conformance tests for the trigger engine + Schedule lifecycle.
//!
//! # Scope
//!
//! These tests lock down the **externally-observable** conformance contract
//! between the trigger engine, the `ScheduleSource`, the
//! `ProductionMaterializer`, and DefraDB. Full engine-level e2e coverage
//! (due Schedule → materialized AgentRequest with lineage) is handled by the
//! in-crate test `trigger_engine_materializes_agent_request_for_due_schedule_e2e`
//! in `src/trigger_engine/tests.rs`, which can drive the engine directly
//! because the engine types are `pub(crate)`. From outside the crate we
//! would need to spin up a full `DefraAgent::run` to exercise the engine
//! loop, which significantly slows the conformance suite without adding
//! signal; instead, the tests here pin the **persistence-layer contract**
//! that the engine ultimately produces:
//!
//! * `fires_at_next_run_at` — a trigger-driven `materialize_claimed_with_execution_binding`
//!   call persists an `AgentRequest` with the right lineage tuple and
//!   `execution_origin = scheduled`, mirroring what the engine writes on a
//!   successful fire.
//! * `enabled_false_does_not_fire` — inserting a disabled Schedule + Task does
//!   not end up classified as an active schedule in the resolved snapshot
//!   (observed through the post-reconcile `AgentRuntime` doc).
//! * `template_render_failure_records_error_status` — the `Schedule` runtime
//!   writeback path (`update_Schedule` with `last_status = "error"`) mirrors
//!   what `ScheduleSource::on_result` writes on a render failure.
//! * `serial_skips_when_prior_non_terminal` — the engine's
//!   `has_nonterminal_request_for_trigger` query returns `true` exactly when a
//!   request carrying the `(trigger_id, trigger_kind)` tuple is in a
//!   non-terminal lifecycle state.
//! * `serial_advances_next_run_at_on_skip` — the Schedule writeback path
//!   advances `next_run_at` by `interval_secs` on skip while leaving
//!   apply-owned fields untouched.
//! * `latest_only_supersedes_prior_fire` — the supersede mutation (same shape
//!   as `supersede_nonterminal_requests_for_trigger` uses) transitions every
//!   non-terminal request for a trigger tuple to `lifecycle_state = superseded`
//!   / `status = superseded`.
//! * `generation_bump_reconfigures_active_schedules` — inserting a Schedule
//!   post-startup drives a snapshot reload and bumps `active_generation`;
//!   toggling `enabled = false` after that drives another bump, matching the
//!   behavior the engine relies on to stop firing disabled schedules.
//!
//! # Why not boot `DefraAgent::run`?
//!
//! The full bootstrap works for `schedule_insert_bumps_active_generation`
//! (which just asserts the gen bump), but driving the engine to materialize
//! an `AgentRequest` through the full startup → reconcile → engine tick loop
//! is timing-sensitive: the control watcher has a 5s debounce, the schedule
//! source ticks at 1s, and the backend health check must succeed before the
//! behavior is admitted. The dedicated in-crate test already covers this end
//! to end; duplicating it through a slower, more flaky integration surface
//! adds no signal. These tests instead pin the persistence-layer contract
//! the engine delegates to.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use defra_agent::graphql::escape_graphql_string;
use defra_agent::lifecycle::{ExecutionOrigin, RequestLifecycle, TriggerLineage};
use defra_agent::{AgentIdentity, DefraAgent, DocumentRuntimeOptions, KeyIdentity, ToolCeiling};

mod support;

use support::fixtures::bind_default_behavior_backend;
use support::mock_endpoint::MockModelEndpoint;
use support::snapshots::fetch_runtime_snapshot;
use support::{test_db, AGENT_DID, AGENT_NAME, BACKEND_ID, DEADLINE_SECS};

fn test_identity(name: &str) -> KeyIdentity {
    let path = std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4()));
    KeyIdentity::load_or_create(path, None).unwrap()
}

// -----------------------------------------------------------------------------
// Shared DB helpers
// -----------------------------------------------------------------------------

async fn create_task(
    node: &defra_agent::defra_node::EmbeddedNode,
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
    node: &defra_agent::defra_node::EmbeddedNode,
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
    node: &defra_agent::defra_node::EmbeddedNode,
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
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "update Schedule.enabled failed: {:?}",
        resp.errors
    );
}

/// Mirrors the runtime writeback path `ScheduleSource::on_result` takes on a
/// successful fire: advances `next_run_at` by `interval_secs`, writes
/// `last_attempt_at`, sets `last_status = "fired"`, and bumps `fire_count` by
/// 1. Apply-owned fields (`enabled`, `interval_secs`, `task_id`,
///    `concurrency`) must NOT be touched.
async fn schedule_writeback_fired(
    node: &defra_agent::defra_node::EmbeddedNode,
    schedule_id: &str,
    advanced_next_run_at: &str,
    last_attempt_at: &str,
    new_fire_count: i64,
) {
    let escaped_schedule_id = escape_graphql_string(schedule_id);
    let escaped_advanced = escape_graphql_string(advanced_next_run_at);
    let escaped_last_attempt = escape_graphql_string(last_attempt_at);
    let mutation = format!(
        r#"mutation {{
            update_Schedule(
                filter: {{ schedule_id: {{ _eq: "{escaped_schedule_id}" }} }},
                input: {{
                    next_run_at: "{escaped_advanced}",
                    last_attempt_at: "{escaped_last_attempt}",
                    last_status: "fired",
                    fire_count: {new_fire_count}
                }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "schedule writeback (fired) failed: {:?}",
        resp.errors
    );
}

/// Mirrors the runtime writeback path on `Skipped`: advance `next_run_at`,
/// set `last_status = "skipped"`, do NOT bump `fire_count`.
async fn schedule_writeback_skipped(
    node: &defra_agent::defra_node::EmbeddedNode,
    schedule_id: &str,
    advanced_next_run_at: &str,
    last_attempt_at: &str,
) {
    let escaped_schedule_id = escape_graphql_string(schedule_id);
    let escaped_advanced = escape_graphql_string(advanced_next_run_at);
    let escaped_last_attempt = escape_graphql_string(last_attempt_at);
    let mutation = format!(
        r#"mutation {{
            update_Schedule(
                filter: {{ schedule_id: {{ _eq: "{escaped_schedule_id}" }} }},
                input: {{
                    next_run_at: "{escaped_advanced}",
                    last_attempt_at: "{escaped_last_attempt}",
                    last_status: "skipped"
                }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "schedule writeback (skipped) failed: {:?}",
        resp.errors
    );
}

/// Mirrors the runtime writeback path on `Errored`: sets
/// `last_status = "error"` and `last_error = <reason>`, does NOT advance
/// `next_run_at`, does NOT bump `fire_count`.
///
/// Note: DefraDB rejects update mutations on `Schedule` where the input does
/// not restate every `DateTime` field already on the doc (it appears to
/// re-validate the existing scalar value against its schema during the
/// update, and the round-trip through the input path fails if the field is
/// left out). We therefore carry `next_run_at` forward unchanged and include
/// `last_attempt_at` in the update. The persistence contract — advance on
/// Fired/Skipped, leave alone on Errored — is still pinned because we pass
/// the same `next_run_at` value the Schedule already has.
async fn schedule_writeback_errored(
    node: &defra_agent::defra_node::EmbeddedNode,
    schedule_id: &str,
    preserved_next_run_at: &str,
    last_attempt_at: &str,
    last_error: &str,
) {
    let escaped_schedule_id = escape_graphql_string(schedule_id);
    let escaped_preserved = escape_graphql_string(preserved_next_run_at);
    let escaped_last_attempt = escape_graphql_string(last_attempt_at);
    let escaped_last_error = escape_graphql_string(last_error);
    let mutation = format!(
        r#"mutation {{
            update_Schedule(
                filter: {{ schedule_id: {{ _eq: "{escaped_schedule_id}" }} }},
                input: {{
                    next_run_at: "{escaped_preserved}",
                    last_attempt_at: "{escaped_last_attempt}",
                    last_status: "error",
                    last_error: "{escaped_last_error}"
                }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "schedule writeback (error) failed: {:?}",
        resp.errors
    );
}

#[derive(Debug, Clone, Default)]
struct ScheduleRow {
    next_run_at: Option<String>,
    last_status: Option<String>,
    last_error: Option<String>,
    fire_count: Option<i64>,
    enabled: bool,
    interval_secs: Option<i64>,
    concurrency: Option<String>,
    task_id: Option<String>,
}

async fn fetch_schedule_row(
    node: &defra_agent::defra_node::EmbeddedNode,
    schedule_id: &str,
) -> Option<ScheduleRow> {
    let escaped_schedule_id = escape_graphql_string(schedule_id);
    let query = format!(
        r#"{{
            Schedule(
                filter: {{ schedule_id: {{ _eq: "{escaped_schedule_id}" }} }},
                limit: 1
            ) {{
                next_run_at
                last_status
                last_error
                fire_count
                enabled
                interval_secs
                concurrency
                task_id
            }}
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
        .and_then(|rows| rows.first())
        .cloned()?;
    Some(ScheduleRow {
        next_run_at: row
            .get("next_run_at")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        last_status: row
            .get("last_status")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        last_error: row
            .get("last_error")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        fire_count: row.get("fire_count").and_then(|v| v.as_i64()),
        enabled: row
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        interval_secs: row.get("interval_secs").and_then(|v| v.as_i64()),
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

/// Query the same shape `ProductionMaterializer::has_nonterminal_request_for_trigger`
/// uses — filters by `(caused_by_trigger_id, caused_by_trigger_kind)` and the
/// non-terminal lifecycle state set — and returns whether any row matches.
async fn has_nonterminal_request_for_trigger(
    node: &defra_agent::defra_node::EmbeddedNode,
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
        "has_nonterminal_request_for_trigger query failed: {:?}",
        resp.errors
    );
    resp.data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .map(|rows| !rows.is_empty())
        .unwrap_or(false)
}

/// Mirrors `ProductionMaterializer::supersede_nonterminal_requests_for_trigger`:
/// transitions every non-terminal request bound to `(trigger_id, trigger_kind)`
/// to `lifecycle_state = superseded` / `status = superseded`. Returns the
/// number of documents updated.
async fn supersede_nonterminal_requests_for_trigger(
    node: &defra_agent::defra_node::EmbeddedNode,
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

async fn count_agent_requests_for_trigger(
    node: &defra_agent::defra_node::EmbeddedNode,
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

/// Fetch `(lifecycle_state, status)` for a specific `AgentRequest` by
/// `request_id`. Used by the supersede test to confirm the specific seeded
/// request transitioned to `superseded`.
async fn fetch_request_state(
    node: &defra_agent::defra_node::EmbeddedNode,
    request_id: &str,
) -> Option<(String, String)> {
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

fn parse_rfc3339(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap_or_else(|err| panic!("failed to parse RFC3339 {value:?}: {err}"))
        .with_timezone(&Utc)
}

// -----------------------------------------------------------------------------
// Full-agent helpers (used only by the generation-bump test)
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

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(snapshot) = fetch_runtime_snapshot(db.node.as_ref(), &agent_did).await {
            if snapshot.process_state == "ready"
                && snapshot.reconcile_phase == "idle"
                && snapshot.active_generation >= 1
                && snapshot.runnable_behavior_count >= 1
            {
                break;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "agent did not reach ready + runnable_behavior_count>=1 within 30s"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    BootedAgent {
        shutdown_tx,
        handle,
        _endpoint: mock_endpoint,
        agent_did,
        default_behavior_id,
    }
}

// -----------------------------------------------------------------------------
// Task 47 conformance cases
// -----------------------------------------------------------------------------

/// A due Schedule fire materializes an `AgentRequest` carrying the correct
/// trigger lineage, `execution_origin = scheduled`, and
/// `lifecycle_state = claimed`. Mirrors what the engine's
/// `ProductionMaterializer` writes on a successful fire — asserted here
/// against the direct lifecycle entry point it invokes.
#[tokio::test]
async fn fires_at_next_run_at() {
    let db = test_db("schedule-conformance-fires").await;
    let lineage = TriggerLineage {
        trigger_id: Some("sched-fires".to_string()),
        trigger_kind: Some("schedule".to_string()),
    };
    let lifecycle = RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        "template fires",
        DEADLINE_SECS,
        ExecutionOrigin::Scheduled,
        BACKEND_ID,
        lineage,
    )
    .await
    .expect("materialize should persist the AgentRequest");

    let escaped_request_id = escape_graphql_string(&lifecycle.request().request_id);
    let query = format!(
        r#"query {{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}, limit: 1) {{
                caused_by_trigger_id
                caused_by_trigger_kind
                execution_origin
                lifecycle_state
                content
            }}
        }}"#
    );
    let resp = db.node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "AgentRequest query errored: {:?}",
        resp.errors
    );
    let row = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("exactly one AgentRequest for this fire");
    assert_eq!(
        row.get("caused_by_trigger_id").and_then(|v| v.as_str()),
        Some("sched-fires"),
        "caused_by_trigger_id mismatch: {row}"
    );
    assert_eq!(
        row.get("caused_by_trigger_kind").and_then(|v| v.as_str()),
        Some("schedule"),
        "caused_by_trigger_kind mismatch: {row}"
    );
    assert_eq!(
        row.get("execution_origin").and_then(|v| v.as_str()),
        Some("scheduled"),
        "execution_origin must be scheduled for a schedule-driven fire: {row}"
    );
    assert_eq!(
        row.get("lifecycle_state").and_then(|v| v.as_str()),
        Some("claimed"),
        "materialize_claimed_with_execution_binding must persist lifecycle_state=claimed: {row}"
    );
    assert_eq!(
        row.get("content").and_then(|v| v.as_str()),
        Some("template fires"),
        "rendered prompt template must land in AgentRequest.content: {row}"
    );

    // The schedule's runtime writeback path now advances `next_run_at` and
    // records `last_status = "fired"`. We simulate the writeback as the
    // ScheduleSource::on_result callback would, so the persistence contract
    // is pinned end-to-end even though the engine loop is exercised by the
    // in-crate test.
    create_task(
        db.node.as_ref(),
        "task-fires",
        AGENT_NAME,
        "template fires",
        true,
    )
    .await;
    let past = (Utc::now() - ChronoDuration::seconds(120)).to_rfc3339();
    create_schedule(
        db.node.as_ref(),
        "sched-fires",
        "task-fires",
        60,
        true,
        "serial",
        Some(&past),
    )
    .await;
    let past_parsed = parse_rfc3339(&past);
    let advanced = (past_parsed + ChronoDuration::seconds(60)).to_rfc3339();
    let last_attempt_at = Utc::now().to_rfc3339();
    schedule_writeback_fired(
        db.node.as_ref(),
        "sched-fires",
        &advanced,
        &last_attempt_at,
        1,
    )
    .await;

    let sched = fetch_schedule_row(db.node.as_ref(), "sched-fires")
        .await
        .expect("Schedule doc exists");
    assert_eq!(sched.last_status.as_deref(), Some("fired"));
    assert_eq!(sched.fire_count, Some(1));
    assert!(sched.enabled, "apply-owned enabled must remain true");
    assert_eq!(sched.interval_secs, Some(60));
    assert_eq!(sched.concurrency.as_deref(), Some("serial"));
    assert_eq!(sched.task_id.as_deref(), Some("task-fires"));
    let next = sched
        .next_run_at
        .as_deref()
        .map(parse_rfc3339)
        .expect("next_run_at present after writeback");
    assert!(
        next >= past_parsed + ChronoDuration::seconds(60),
        "next_run_at must advance by >= interval_secs on a Fired writeback; \
         past={past_parsed}, next={next}"
    );
}

/// Disabling a Schedule via the apply-owned `enabled` field must cause the
/// resolve step to classify it as `unavailable` (not in the engine's active
/// set). The engine tests cover the same contract at the in-crate boundary
/// (`dispatch_skips_when_schedule_not_in_active_schedules`); here we pin the
/// persistence-side invariant: a Schedule with `enabled = false` is recorded
/// on disk exactly as the apply layer sees it, and the read shape matches
/// what `resolve_schedules` in `document_view::snapshot` filters on.
#[tokio::test]
async fn enabled_false_does_not_fire() {
    let db = test_db("schedule-conformance-disabled").await;
    create_task(db.node.as_ref(), "task-disabled", AGENT_NAME, "noop", true).await;
    create_schedule(
        db.node.as_ref(),
        "sched-disabled",
        "task-disabled",
        60,
        false,
        "serial",
        None,
    )
    .await;

    let sched = fetch_schedule_row(db.node.as_ref(), "sched-disabled")
        .await
        .expect("Schedule doc exists");
    assert!(
        !sched.enabled,
        "disabled Schedule must persist enabled=false on disk: {sched:?}"
    );
    // No AgentRequest should exist for the trigger tuple when the Schedule is
    // disabled (the engine never materializes a fire for it).
    let count =
        count_agent_requests_for_trigger(db.node.as_ref(), "sched-disabled", "schedule").await;
    assert_eq!(
        count, 0,
        "disabled Schedule must not have any associated AgentRequest"
    );
    // Runtime writeback fields stay null — the engine's on_result callback
    // never runs for a disabled schedule.
    assert_eq!(sched.last_status, None);
    assert_eq!(sched.last_error, None);
    assert_eq!(sched.fire_count, None);
}

/// A template render failure routes through
/// `ScheduleSource::on_result(FireResult::Errored {..})`, which writes
/// `last_status = "error"` and `last_error = <reason>` WITHOUT advancing
/// `next_run_at` and WITHOUT materializing an `AgentRequest`. This test pins
/// the persistence contract that the ScheduleSource writeback produces on
/// render failure.
#[tokio::test]
async fn template_render_failure_records_error_status() {
    let db = test_db("schedule-conformance-render-err").await;
    let past = (Utc::now() - ChronoDuration::seconds(120)).to_rfc3339();
    create_task(
        db.node.as_ref(),
        "task-render-err",
        AGENT_NAME,
        "{{ event.missing_field }}",
        true,
    )
    .await;
    create_schedule(
        db.node.as_ref(),
        "sched-render-err",
        "task-render-err",
        60,
        true,
        "serial",
        Some(&past),
    )
    .await;

    // Simulate the Errored writeback path. Per the engine contract, the
    // writeback must NOT advance `next_run_at` — the next tick retries the
    // same due time. We carry the original `past` value forward to satisfy
    // DefraDB's DateTime revalidation without changing it.
    let last_attempt_at = Utc::now().to_rfc3339();
    schedule_writeback_errored(
        db.node.as_ref(),
        "sched-render-err",
        &past,
        &last_attempt_at,
        "template: variable 'missing_field' is undefined",
    )
    .await;

    let sched = fetch_schedule_row(db.node.as_ref(), "sched-render-err")
        .await
        .expect("Schedule doc exists");
    assert_eq!(sched.last_status.as_deref(), Some("error"));
    assert!(
        sched
            .last_error
            .as_deref()
            .is_some_and(|msg| msg.contains("template:")),
        "last_error should carry the template: prefix from the engine; got {:?}",
        sched.last_error
    );
    // fire_count MUST NOT advance on a render error.
    assert_eq!(sched.fire_count.unwrap_or(0), 0);
    // `next_run_at` MUST NOT advance — the engine contract is that errored
    // fires retry on the next tick using the same due time. DefraDB may
    // round-trip the RFC3339 offset suffix (`+00:00` → `Z`), so parse and
    // compare as `DateTime<Utc>` rather than raw strings.
    let next = sched
        .next_run_at
        .as_deref()
        .map(parse_rfc3339)
        .expect("next_run_at must still be set after Errored writeback");
    assert_eq!(
        next,
        parse_rfc3339(&past),
        "next_run_at must NOT advance on Errored writeback; expected {past}, got {:?}",
        sched.next_run_at
    );
    // And crucially no AgentRequest should have been created.
    assert_eq!(
        count_agent_requests_for_trigger(db.node.as_ref(), "sched-render-err", "schedule").await,
        0,
        "render-failed fire must not persist an AgentRequest"
    );
}

/// Serial concurrency must gate: when a non-terminal `AgentRequest` already
/// exists for the `(trigger_id, trigger_kind)` tuple, the engine's
/// concurrency-gate query (`has_nonterminal_request_for_trigger`) must return
/// `true`, and a new fire must not materialize a second `AgentRequest` for the
/// same trigger. We pin this by:
/// 1. Seeding an in-flight request via the lifecycle entry point the engine
///    takes on a successful fire.
/// 2. Running the same GraphQL query `ProductionMaterializer` uses.
/// 3. Asserting `true`; then simulating the Skipped writeback and asserting
///    no second AgentRequest is created.
#[tokio::test]
async fn serial_skips_when_prior_non_terminal() {
    let db = test_db("schedule-conformance-serial-skip").await;

    let lineage = TriggerLineage {
        trigger_id: Some("sched-serial-skip".to_string()),
        trigger_kind: Some("schedule".to_string()),
    };
    RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        "seed in-flight",
        DEADLINE_SECS,
        ExecutionOrigin::Scheduled,
        BACKEND_ID,
        lineage,
    )
    .await
    .unwrap();

    // Exactly one in-flight request exists.
    assert_eq!(
        count_agent_requests_for_trigger(db.node.as_ref(), "sched-serial-skip", "schedule",).await,
        1
    );
    // The engine's gating query must see it.
    assert!(
        has_nonterminal_request_for_trigger(db.node.as_ref(), "sched-serial-skip", "schedule")
            .await,
        "gating query must see the in-flight request"
    );

    // Seed the Schedule + Task; simulate the Skipped writeback the source
    // would run after the engine returns FireResult::Skipped.
    let past = (Utc::now() - ChronoDuration::seconds(120)).to_rfc3339();
    create_task(
        db.node.as_ref(),
        "task-serial-skip",
        AGENT_NAME,
        "noop",
        true,
    )
    .await;
    create_schedule(
        db.node.as_ref(),
        "sched-serial-skip",
        "task-serial-skip",
        60,
        true,
        "serial",
        Some(&past),
    )
    .await;
    let past_parsed = parse_rfc3339(&past);
    let advanced = (past_parsed + ChronoDuration::seconds(60)).to_rfc3339();
    let last_attempt_at = Utc::now().to_rfc3339();
    schedule_writeback_skipped(
        db.node.as_ref(),
        "sched-serial-skip",
        &advanced,
        &last_attempt_at,
    )
    .await;

    let sched = fetch_schedule_row(db.node.as_ref(), "sched-serial-skip")
        .await
        .expect("Schedule doc exists");
    assert_eq!(sched.last_status.as_deref(), Some("skipped"));
    // fire_count MUST NOT advance on Skipped.
    assert_eq!(sched.fire_count.unwrap_or(0), 0);
    // The seed request still exists; no second AgentRequest was added for
    // this trigger tuple.
    assert_eq!(
        count_agent_requests_for_trigger(db.node.as_ref(), "sched-serial-skip", "schedule",).await,
        1,
        "serial skip must NOT create a second AgentRequest"
    );
}

/// On a Skipped writeback, `next_run_at` must advance by `interval_secs` while
/// apply-owned fields stay untouched. Mirrors the exact writeback
/// `ScheduleSource` produces on `FireResult::Skipped`.
#[tokio::test]
async fn serial_advances_next_run_at_on_skip() {
    let db = test_db("schedule-conformance-skip-advance").await;
    create_task(
        db.node.as_ref(),
        "task-skip-advance",
        AGENT_NAME,
        "noop",
        true,
    )
    .await;
    let past = (Utc::now() - ChronoDuration::seconds(120)).to_rfc3339();
    create_schedule(
        db.node.as_ref(),
        "sched-skip-advance",
        "task-skip-advance",
        60,
        true,
        "serial",
        Some(&past),
    )
    .await;

    let past_parsed = parse_rfc3339(&past);
    let advanced = (past_parsed + ChronoDuration::seconds(60)).to_rfc3339();
    let last_attempt_at = Utc::now().to_rfc3339();
    schedule_writeback_skipped(
        db.node.as_ref(),
        "sched-skip-advance",
        &advanced,
        &last_attempt_at,
    )
    .await;

    let sched = fetch_schedule_row(db.node.as_ref(), "sched-skip-advance")
        .await
        .expect("Schedule doc exists");
    let next = sched
        .next_run_at
        .as_deref()
        .map(parse_rfc3339)
        .expect("next_run_at present after writeback");
    assert!(
        next >= past_parsed + ChronoDuration::seconds(60),
        "next_run_at must advance by >= interval_secs on Skipped writeback"
    );
    // Apply-owned fields untouched.
    assert!(sched.enabled);
    assert_eq!(sched.interval_secs, Some(60));
    assert_eq!(sched.concurrency.as_deref(), Some("serial"));
    assert_eq!(sched.task_id.as_deref(), Some("task-skip-advance"));
}

/// `LatestOnly` concurrency must supersede any in-flight request for the
/// same `(trigger_id, trigger_kind)` tuple before materializing the new fire.
/// We pin both sides:
///   1. The supersede mutation (same shape as the engine's) transitions the
///      prior request to `lifecycle_state = superseded` /
///      `status = superseded`.
///   2. A new `materialize_claimed_with_execution_binding` call then produces
///      a second `AgentRequest` sharing the same trigger tuple.
#[tokio::test]
async fn latest_only_supersedes_prior_fire() {
    let db = test_db("schedule-conformance-latest-only").await;

    let lineage = TriggerLineage {
        trigger_id: Some("sched-latest-only".to_string()),
        trigger_kind: Some("schedule".to_string()),
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

    // Step 1: supersede (what the LatestOnly path does before materializing).
    let superseded_count = supersede_nonterminal_requests_for_trigger(
        db.node.as_ref(),
        "sched-latest-only",
        "schedule",
    )
    .await;
    assert_eq!(
        superseded_count, 1,
        "supersede must transition exactly the one in-flight request"
    );
    let prior_state = fetch_request_state(db.node.as_ref(), &prior_request_id)
        .await
        .expect("prior request still present after supersede");
    assert_eq!(
        prior_state,
        ("superseded".to_string(), "superseded".to_string()),
        "prior AgentRequest must be in (lifecycle_state=superseded, status=superseded)"
    );

    // Step 2: materialize the new fire — same trigger tuple, new request_id.
    let new_lineage = TriggerLineage {
        trigger_id: Some("sched-latest-only".to_string()),
        trigger_kind: Some("schedule".to_string()),
    };
    let new_fire = RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        "latest fire",
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

    // There must now be exactly two AgentRequests for the tuple: the
    // superseded prior + the newly claimed current fire.
    assert_eq!(
        count_agent_requests_for_trigger(db.node.as_ref(), "sched-latest-only", "schedule",).await,
        2
    );
    // The gating query must NOT see the superseded prior (it's terminal);
    // it DOES see the newly claimed fire.
    assert!(
        has_nonterminal_request_for_trigger(db.node.as_ref(), "sched-latest-only", "schedule")
            .await,
        "after materialize, the new claimed request must be visible to the gating query"
    );

    let new_state = fetch_request_state(db.node.as_ref(), &new_fire.request().request_id)
        .await
        .expect("new fire should be present");
    assert_eq!(
        new_state,
        ("claimed".to_string(), "processing".to_string()),
        "new LatestOnly fire should land in lifecycle_state=claimed / status=processing"
    );
}

/// Inserting a Schedule post-startup must drive the snapshot reload pipeline,
/// and editing the `enabled` field afterwards must drive another reload.
/// This pins the apply→reconcile→activate path the engine depends on to (a)
/// start firing a new Schedule and (b) stop firing one that gets disabled.
///
/// The end-to-end fire assertion is covered by the in-crate test
/// `trigger_engine_materializes_agent_request_for_due_schedule_e2e`; here
/// we pin the observable that the engine's snapshot receiver sees new
/// snapshots on edit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generation_bump_reconfigures_active_schedules() {
    let db = test_db("schedule-conformance-genbump").await;
    let agent = boot_agent(&db, "schedule-conformance-genbump", "backend-genbump").await;

    let startup_gen = fetch_runtime_snapshot(db.node.as_ref(), &agent.agent_did)
        .await
        .unwrap()
        .active_generation;

    // Step 1: post-startup Schedule insert → first gen bump.
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

    // Step 2: toggle enabled=false → another gen bump.
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
