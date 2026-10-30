use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::documents::TaskHook;
use crate::hook::BackgroundExecutionRegistry;
use crate::lean_vocab_test::{
    lean_recovery_sweep_cases, lean_task_hook_recovery_cases, LeanCommandResult, LeanTaskHook,
};
use crate::lifecycle::{ExecutionOrigin, RequestLifecycle, TriggerLineage};
use crate::store::identity::{KeyIdentity, NodeIdentity};
use crate::task_hooks::{
    recover_task_hook_records, HookCommandResult, RecordedHookAttempt, TaskHookRecord,
    TaskHookRecordStore,
};

const AGENT_ID: &str = "general";

pub(super) struct Fixture {
    pub(super) node: Arc<defra_node::EmbeddedNode>,
    identity: Arc<dyn NodeIdentity>,
    _data: tempfile::TempDir,
}

impl Fixture {
    pub(super) async fn new() -> Self {
        let data = tempfile::tempdir().expect("node data directory");
        let identity: Arc<dyn NodeIdentity> = Arc::new(
            KeyIdentity::load_or_create(data.path().join("agent.key"), None).expect("identity"),
        );
        let node = Arc::new(
            defra_node::EmbeddedNode::builder()
                .with_node_identity_did(identity.did())
                .data_path(data.path())
                .build()
                .await
                .expect("embedded node"),
        );
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        crate::test_support::install_test_agent(&node, identity.did(), AGENT_ID).await;
        Self {
            node,
            identity,
            _data: data,
        }
    }

    fn did(&self) -> &str {
        self.identity.did()
    }

    pub(super) async fn claimed(&self, lease: Duration) -> RequestLifecycle {
        let mut lifecycle = RequestLifecycle::materialize_pending_with_execution_binding(
            self.node.clone(),
            AGENT_ID,
            self.identity.clone(),
            "run the task",
            60,
            ExecutionOrigin::Interactive,
            "task-hook-recovery",
            TriggerLineage::default(),
        )
        .await
        .unwrap();
        lifecycle.set_execution_lease_duration(lease);
        lifecycle.claim_with_identity().await.unwrap();
        lifecycle
    }

    /// POST-failure diagnostic: read-after-the-fact snapshot of selected
    /// persisted request lifecycle fields for the failing request. Uses the
    /// existing safe bounded query pattern (single-row filter on _docID,
    /// narrow serde_json::Value projection following the request_state
    /// lookup in tool_call_lifecycle/query.rs) and emits ONLY the explicit
    /// selected fields — never a full row dump. Sanitized by construction:
    /// lifecycle/generation/deadline tokens and wall time only — no message
    /// content, identities, secrets, or full row dumps. NOT atomic guard
    /// evidence: the row is read after the failure and may have changed.
    /// The whole snapshot future is capped at 1s (best-effort diagnostic cap
    /// only; no lease/test budget increase, no retries): on timeout a marker
    /// is returned and the original begin error stays in the panic.
    async fn post_failure_request_snapshot(&self, request_doc_id: &str) -> String {
        let snapshot_started = chrono::Utc::now();
        let escaped = crate::graphql::escape_graphql_string(request_doc_id);
        let query_future = async {
            let response = self.node.execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{ lifecycle_state execution_generation execution_lease_expires_at execution_lease_secs }} }}"#
            )).await;
            if response.has_errors() {
                return Err(());
            }
            let rows = response
                .data
                .as_ref()
                .and_then(|data| data.get("AgentRequest"))
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default();
            Ok(rows.first().and_then(project_request_row))
        };
        let snapshot = render_snapshot(
            snapshot_started,
            tokio::time::timeout(std::time::Duration::from_secs(1), query_future)
                .await
                .map_err(|_| ()),
        );
        snapshot
    }

    async fn abandoned_request(&self, interrupt_requested: bool) -> String {
        let claim_started = std::time::Instant::now();
        let mut lifecycle = self.claimed(Duration::from_secs(60)).await;
        let writer =
            crate::streaming::DefraStreamWriter::new(self.node.clone(), self.did(), Duration::ZERO);
        if let Err(begin_error) = lifecycle.begin_owned_execution(&writer).await {
            let expected_generation = lifecycle
                .execution_generation()
                .unwrap_or("<none>")
                .to_string();
            let elapsed = claim_started.elapsed();
            let request_doc_id = lifecycle.request().doc_id.clone();
            let post_failure_snapshot = self.post_failure_request_snapshot(&request_doc_id).await;
            panic!(
                "begin_owned_execution failed in abandoned_request fixture: {begin_error}; \
                 expected execution generation: {expected_generation}; \
                 monotonic claim-through-begin elapsed: {elapsed:?}; \
                 POST-failure request snapshot (not atomic guard evidence): {post_failure_snapshot}"
            );
        }
        let doc_id = lifecycle.request().doc_id.clone();
        if interrupt_requested {
            crate::interrupt::interrupt_request_by_doc_id(
                &self.node,
                &doc_id,
                self.did(),
                Some(self.did()),
            )
            .await
            .unwrap();
        }
        drop(lifecycle);
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let response = crate::graphql::graphql_with_transaction_retry(
                    &self.node,
                    &format!(r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ execution_lease_expires_at }} }}"#,
                        crate::graphql::escape_graphql_string(&doc_id)),
                    "test.task_hook_relinquishment",
                ).await.unwrap();
                let row: serde_json::Value = crate::graphql::first_row(&response, "AgentRequest")
                    .unwrap().expect("abandoned request");
                let expiry = chrono::DateTime::parse_from_rfc3339(
                    row["execution_lease_expires_at"].as_str().unwrap(),
                ).unwrap();
                if expiry < chrono::Utc::now() { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("dropping the owner must relinquish its execution lease");
        doc_id
    }

    async fn request_state(&self, doc_id: &str) -> String {
        let response = crate::graphql::graphql_with_transaction_retry(
            self.node.as_ref(),
            &format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ lifecycle_state }} }}"#,
                crate::graphql::escape_graphql_string(doc_id),
            ),
            "test.task_hook_recovery_request",
        )
        .await
        .unwrap();
        let row: serde_json::Value = crate::graphql::first_row(&response, "AgentRequest")
            .unwrap()
            .expect("request row");
        row["lifecycle_state"].as_str().unwrap().to_owned()
    }
}

fn logging_hook(generated: &LeanTaskHook, log: &Path, code: i64) -> TaskHook {
    TaskHook {
        hook_id: generated.hook_id.clone(),
        phase: crate::lean_vocab_test::lean_hook_phase(&generated.phase),
        command: vec![
            "sh".into(),
            "-c".into(),
            format!(
                "printf '%s\\n' {} >> {}; exit {code}",
                generated.hook_id,
                log.display()
            ),
        ],
        timeout_secs: generated.timeout_secs,
        skip_exit_codes: generated.skip_exit_codes.clone(),
    }
}

fn recorded(result: &LeanCommandResult) -> Option<HookCommandResult> {
    (*result != LeanCommandResult::Interrupted).then(|| result.to_native())
}

async fn persist_crashed(store: &TaskHookRecordStore, record: TaskHookRecord) {
    let handle = store.begin(record).expect("claim the record");
    handle
        .work_started()
        .await
        .expect("write the crashed execution's record");
}

fn record(
    doc_id: &str,
    name: &str,
    did: &str,
    cwd: &Path,
    hooks: Vec<TaskHook>,
    attempts: Vec<RecordedHookAttempt>,
) -> TaskHookRecord {
    TaskHookRecord {
        request_doc_id: doc_id.to_owned(),
        request_id: name.to_owned(),
        node_did: did.to_owned(),
        cwd: cwd.to_path_buf(),
        root_guard: None,
        hooks,
        attempts,
    }
}

fn log_lines(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn generated_task_hook_recovery_cases_drive_startup_recovery_and_host_records() {
    let cases = lean_task_hook_recovery_cases();
    assert!(!cases.is_empty());
    let fixture = Fixture::new().await;
    for case in cases {
        let dir = tempfile::tempdir().expect("case directory");
        let log = dir.path().join("cleanup.log");
        let doc_id = fixture.abandoned_request(case.interrupt_requested).await;
        let store = TaskHookRecordStore::durable(dir.path().join("task-hooks"));
        if case.started {
            let hooks = case
                .hooks
                .iter()
                .map(|generated| {
                    let code = case
                        .script
                        .iter()
                        .find(|entry| entry.hook_id == generated.hook_id)
                        .map(|entry| match entry.result {
                            LeanCommandResult::Exited { code } => code,
                            ref other => panic!("{}: unsupported script {other:?}", case.name),
                        })
                        .unwrap_or(0);
                    logging_hook(generated, &log, code)
                })
                .collect();
            let attempts = case
                .observed
                .iter()
                .map(|observed| RecordedHookAttempt {
                    hook_id: observed.hook_id.clone(),
                    process: None,
                    result: recorded(&observed.result),
                })
                .collect();
            persist_crashed(
                &store,
                record(
                    &doc_id,
                    &case.name,
                    fixture.did(),
                    dir.path(),
                    hooks,
                    attempts,
                ),
            )
            .await;
        }
        let restarted = BackgroundExecutionRegistry::default()
            .with_task_hook_records(dir.path().join("task-hooks"));
        let outcome = crate::startup_recovery::run_startup_recovery_with_executions(
            &fixture.node,
            fixture.did(),
            &restarted,
        )
        .await;
        let report = outcome.task_hooks.expect("task hook recovery");
        restarted.task_hook_records().wait_for_recoveries().await;
        assert_eq!(report, usize::from(case.started));
        assert_eq!(
            fixture.request_state(&doc_id).await,
            case.expected_request_state,
            "{}: request recovery decided a different terminal than the model",
            case.name
        );
        assert_eq!(
            log_lines(&log),
            case.expected_remaining,
            "{}: recovery ran different cleanup than the model selected",
            case.name
        );
        assert_eq!(
            case.attempted
                .iter()
                .map(|attempt| attempt.hook_id.clone())
                .collect::<Vec<_>>(),
            case.expected_remaining,
            "{}",
            case.name
        );
        assert!(
            restarted.task_hook_records().list().is_empty(),
            "{}: a recovered record must be forgotten",
            case.name
        );

        let again = crate::startup_recovery::run_startup_recovery_with_executions(
            &fixture.node,
            fixture.did(),
            &restarted,
        )
        .await;
        assert!(again.task_hooks.expect("second pass") == 0);
        restarted.task_hook_records().wait_for_recoveries().await;
        assert_eq!(
            log_lines(&log),
            case.expected_remaining,
            "{}: cleanup ran twice",
            case.name
        );
    }
}

#[tokio::test]
async fn generated_task_hook_sweep_cases_drive_the_record_sweep() {
    let cases = lean_recovery_sweep_cases()
        .iter()
        .filter(|case| case.collection == "TaskHookRecord")
        .collect::<Vec<_>>();
    assert!(!cases.is_empty());
    let fixture = Fixture::new().await;
    for case in cases {
        let resolved = case.parent_terminal.expect("resolved premise");
        let live = case.execution_registered.expect("live premise");
        let dir = tempfile::tempdir().expect("case directory");
        let log = dir.path().join("cleanup.log");
        let (doc_id, owner) = if resolved {
            (fixture.abandoned_request(false).await, None)
        } else {
            let owner = fixture.claimed(Duration::from_secs(60)).await;
            (owner.request().doc_id.clone(), Some(owner))
        };
        if resolved {
            RequestLifecycle::recover_all(&fixture.node, fixture.did())
                .await
                .unwrap();
        }
        let store = TaskHookRecordStore::default();
        let handle = store
            .begin(record(
                &doc_id,
                &case.name,
                fixture.did(),
                dir.path(),
                vec![logging_hook(&generated("sweep", "finally"), &log, 0)],
                Vec::new(),
            ))
            .unwrap();
        handle.work_started().await.unwrap();
        let held = live.then_some(handle);
        let report = recover_task_hook_records(&fixture.node, fixture.did(), &store)
            .await
            .unwrap();
        store.wait_for_recoveries().await;
        let recorded = !store.list().is_empty();
        assert_eq!(
            if recorded { "running" } else { "released" },
            case.terminal_state,
            "{}",
            case.name
        );
        assert_eq!(
            report,
            case.measure_before - case.measure_after,
            "{}",
            case.name
        );
        assert_eq!(log_lines(&log).len(), report, "{}", case.name);
        drop(held);
        drop(owner);
    }
}

fn generated(hook_id: &str, phase: &str) -> LeanTaskHook {
    LeanTaskHook {
        hook_id: hook_id.into(),
        phase: phase.into(),
        command: Vec::new(),
        timeout_secs: Some(30),
        skip_exit_codes: Vec::new(),
        effective_timeout_secs: 30,
    }
}

#[cfg(unix)]
#[tokio::test]
async fn recovery_stops_a_surviving_hook_command_before_cleanup() {
    use crate::managed_exec::ownership::test_support::OwnedTestProcess;
    use crate::managed_exec::ownership::ProcessObservation;

    let fixture = Fixture::new().await;
    let dir = tempfile::tempdir().expect("case directory");
    let log = dir.path().join("cleanup.log");
    let doc_id = fixture.abandoned_request(false).await;
    let survivor = OwnedTestProcess::spawn(None).await;
    let records = dir.path().join("task-hooks");
    persist_crashed(
        &TaskHookRecordStore::durable(records.clone()),
        record(
            &doc_id,
            "surviving",
            fixture.did(),
            dir.path(),
            vec![
                logging_hook(&generated("prepare", "before"), &log, 0),
                logging_hook(&generated("sweep", "finally"), &log, 0),
            ],
            vec![RecordedHookAttempt {
                hook_id: "prepare".into(),
                process: Some(survivor.identity.clone()),
                result: None,
            }],
        ),
    )
    .await;
    assert_eq!(survivor.identity.observe(), ProcessObservation::Running);

    let restarted = BackgroundExecutionRegistry::default().with_task_hook_records(records);
    let outcome = crate::startup_recovery::run_startup_recovery_with_executions(
        &fixture.node,
        fixture.did(),
        &restarted,
    )
    .await;
    assert_eq!(outcome.task_hooks.unwrap(), 1);
    restarted.task_hook_records().wait_for_recoveries().await;
    assert_ne!(survivor.identity.observe(), ProcessObservation::Running);
    assert_eq!(log_lines(&log), vec!["sweep".to_string()]);
    assert!(restarted.task_hook_records().list().is_empty());
}

#[tokio::test]
async fn shutdown_during_recovered_cleanup_keeps_only_the_unlaunched_cleanup() {
    let fixture = Fixture::new().await;
    let dir = tempfile::tempdir().expect("case directory");
    let log = dir.path().join("cleanup.log");
    let started = dir.path().join("first-started");
    let doc_id = fixture.abandoned_request(false).await;
    let records = dir.path().join("task-hooks");
    let mut held = logging_hook(&generated("first", "finally"), &log, 0);
    held.command = vec![
        "sh".into(),
        "-c".into(),
        format!(
            "printf 'first\\n' >> {}; : > {}; exec sleep 30",
            log.display(),
            started.display()
        ),
    ];
    persist_crashed(
        &TaskHookRecordStore::durable(records.clone()),
        record(
            &doc_id,
            "shutdown",
            fixture.did(),
            dir.path(),
            vec![held, logging_hook(&generated("second", "finally"), &log, 0)],
            Vec::new(),
        ),
    )
    .await;

    let running = BackgroundExecutionRegistry::default().with_task_hook_records(records.clone());
    let outcome = crate::startup_recovery::run_startup_recovery_with_executions(
        &fixture.node,
        fixture.did(),
        &running,
    )
    .await;
    assert_eq!(outcome.task_hooks.unwrap(), 1);
    tokio::time::timeout(Duration::from_secs(20), async {
        while !started.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("recovered cleanup started");
    tokio::time::timeout(
        Duration::from_secs(20),
        running.task_hook_records().shutdown(),
    )
    .await
    .expect("shutdown awaits recovered cleanup");

    let left = running.task_hook_records().list();
    assert_eq!(left.len(), 1, "unlaunched cleanup stays recorded");
    assert_eq!(
        left[0]
            .attempts
            .iter()
            .map(|attempt| (attempt.hook_id.as_str(), attempt.result.clone()))
            .collect::<Vec<_>>(),
        vec![("first", Some(HookCommandResult::Interrupted))]
    );

    let restarted = BackgroundExecutionRegistry::default().with_task_hook_records(records);
    crate::startup_recovery::run_startup_recovery_with_executions(
        &fixture.node,
        fixture.did(),
        &restarted,
    )
    .await;
    restarted.task_hook_records().wait_for_recoveries().await;
    assert_eq!(log_lines(&log), vec!["first", "second"]);
    assert!(restarted.task_hook_records().list().is_empty());
}

#[tokio::test]
async fn recovered_cleanup_outside_an_admitted_root_is_refused() {
    let fixture = Fixture::new().await;
    let dir = tempfile::tempdir().expect("case directory");
    let log = dir.path().join("cleanup.log");
    let ceiling = tempfile::tempdir().expect("admitted ceiling");
    let cases = [
        ("removed-cwd", dir.path().join("removed"), None),
        (
            "root-outside-ceiling",
            dir.path().to_path_buf(),
            Some(crate::tool_surface::RootExecutionGuard {
                agent_id: AGENT_ID.into(),
                selected_root: Some(dir.path().to_path_buf()),
                ceiling_root: Some(ceiling.path().to_path_buf()),
            }),
        ),
    ];
    for (name, cwd, root_guard) in cases {
        if let Some(guard) = &root_guard {
            assert!(
                guard.validate(fixture.node.as_ref()).await.is_err(),
                "{name}: the root owner must refuse this root"
            );
        }
        let doc_id = fixture.abandoned_request(false).await;
        let store = TaskHookRecordStore::durable(dir.path().join(format!("records-{name}")));
        persist_crashed(
            &store,
            TaskHookRecord {
                root_guard,
                ..record(
                    &doc_id,
                    name,
                    fixture.did(),
                    &cwd,
                    vec![logging_hook(&generated("sweep", "finally"), &log, 0)],
                    Vec::new(),
                )
            },
        )
        .await;
        RequestLifecycle::recover_all(&fixture.node, fixture.did())
            .await
            .unwrap();
        let report = recover_task_hook_records(&fixture.node, fixture.did(), &store)
            .await
            .unwrap();
        assert_eq!(report, 1, "{name}");
        store.wait_for_recoveries().await;
        assert!(
            log_lines(&log).is_empty(),
            "{name}: cleanup ran outside its root"
        );
        assert!(
            store.list().is_empty(),
            "{name}: the refused record is dropped"
        );
    }
}

/// Pure projection of one persisted AgentRequest row onto the snapshot's
/// selected fields. Used by the live query path AND the projection tests.
/// Emits ONLY the explicitly selected fields; any other keys present in the
/// row (identities, secrets, message content, unknown future fields) are
/// ignored and never rendered.
type RequestRowProjection = (Option<String>, Option<String>, Option<String>, Option<i64>);

fn project_request_row(row: &serde_json::Value) -> Option<RequestRowProjection> {
    Some((
        row.get("lifecycle_state")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        row.get("execution_generation")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        row.get("execution_lease_expires_at")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        row.get("execution_lease_secs")
            .and_then(serde_json::Value::as_i64),
    ))
}

/// Pure rendering of the POST-failure snapshot result, separated so the
/// formatting branches (selected fields / missing row / query failure / 1s
/// timeout marker) are testable without a node process. `result` is the
/// outer timeout result (Err = 1s cap hit); the inner value is the query
/// outcome. Emits only the explicitly selected fields — never a full row.
fn render_snapshot(
    snapshot_started: chrono::DateTime<chrono::Utc>,
    result: Result<Result<Option<RequestRowProjection>, ()>, ()>,
) -> String {
    match result {
        Ok(Ok(Some((lifecycle_state, generation, expires_at, lease_secs)))) => format!(
            "snapshot_started_at={}; lifecycle_state={}; execution_generation={}; \
             execution_lease_expires_at={}; execution_lease_secs={}",
            snapshot_started.to_rfc3339(),
            lifecycle_state.unwrap_or_else(|| "<missing>".to_string()),
            generation.unwrap_or_else(|| "<missing>".to_string()),
            expires_at.unwrap_or_else(|| "<missing>".to_string()),
            lease_secs
                .map(|secs| secs.to_string())
                .unwrap_or_else(|| "<missing>".to_string()),
        ),
        Ok(Ok(None)) => "request row missing at snapshot time".to_string(),
        Ok(Err(())) => "<snapshot query failed>".to_string(),
        Err(()) => "<snapshot timed out after 1s>".to_string(),
    }
}

#[cfg(test)]
mod render_snapshot_tests {
    use super::{project_request_row, render_snapshot};
    use chrono::TimeZone;

    fn ts() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.with_ymd_and_hms(2026, 5, 20, 12, 0, 0).unwrap()
    }

    #[test]
    fn renders_selected_fields_only() {
        let rendered = render_snapshot(
            ts(),
            Ok(Ok(Some((
                Some("running".to_string()),
                Some("gen-7".to_string()),
                Some("2026-05-20T12:00:01Z".to_string()),
                Some(1),
            )))),
        );
        assert!(rendered.contains("snapshot_started_at=2026-05-20T12:00:00"));
        assert!(rendered.contains("lifecycle_state=running"));
        assert!(rendered.contains("execution_generation=gen-7"));
        assert!(rendered.contains("execution_lease_expires_at=2026-05-20T12:00:01Z"));
        assert!(rendered.contains("execution_lease_secs=1"));
        assert!(!rendered.contains("request_id"));
    }

    #[test]
    fn projects_realistic_row_without_request_id_and_ignores_sensitive_keys() {
        // Realistic full AgentRequest row shape WITHOUT request_id in the
        // projection, plus extra sensitive/unknown keys that must never be
        // rendered.
        let row = serde_json::json!({
            "_docID": "agent-request-doc-1",
            "lifecycle_state": "claimed",
            "execution_generation": "gen-9",
            "execution_lease_expires_at": "2026-05-20T12:00:01.5Z",
            "execution_lease_secs": 1,
            "requester_did": "did:key:secret-owner",
            "session_id": "session-secret",
            "messages": [{"role": "user", "content": "sensitive"}],
            "future_unknown_field": {"nested": "payload"}
        });
        let (lifecycle_state, generation, expires_at, lease_secs) =
            project_request_row(&row).expect("row projects");
        assert_eq!(lifecycle_state.as_deref(), Some("claimed"));
        assert_eq!(generation.as_deref(), Some("gen-9"));
        assert_eq!(expires_at.as_deref(), Some("2026-05-20T12:00:01.5Z"));
        assert_eq!(lease_secs, Some(1));
        // Rendered output must contain none of the ignored sensitive keys.
        let rendered = render_snapshot(
            ts(),
            Ok(Ok(Some((
                lifecycle_state,
                generation,
                expires_at,
                lease_secs,
            )))),
        );
        assert!(!rendered.contains("requester_did"));
        assert!(!rendered.contains("session-secret"));
        assert!(!rendered.contains("sensitive"));
        assert!(!rendered.contains("future_unknown_field"));
        assert!(!rendered.contains("agent-request-doc-1"));
    }

    #[test]
    fn projects_partial_row_with_malformed_types_as_missing() {
        // Wrong-typed / missing selected fields render as <missing>, and
        // malformed types are simply ignored rather than panicking.
        let row = serde_json::json!({
            "lifecycle_state": 17,
            "execution_generation": {"nested": true},
            "execution_lease_secs": "not-a-number"
        });
        let (lifecycle_state, generation, expires_at, lease_secs) =
            project_request_row(&row).expect("row projects even when typed wrong");
        assert_eq!(lifecycle_state, None);
        assert_eq!(generation, None);
        assert_eq!(expires_at, None);
        assert_eq!(lease_secs, None);
    }

    #[test]
    fn empty_array_and_absent_key_project_to_missing_row() {
        // Empty array: query succeeded but no row matched.
        let empty: Vec<serde_json::Value> = Vec::new();
        assert!(empty.first().and_then(project_request_row).is_none());

        // Absent key: data object present without AgentRequest.
        let data = serde_json::json!({ "SomethingElse": [] });
        let rows = data
            .get("AgentRequest")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(rows.first().and_then(project_request_row).is_none());
    }

    #[test]
    fn renders_missing_fields_and_missing_row() {
        let partial = render_snapshot(ts(), Ok(Ok(Some((None, None, None, None)))));
        assert!(partial.contains("lifecycle_state=<missing>"));
        assert!(partial.contains("execution_lease_secs=<missing>"));

        let absent = render_snapshot(ts(), Ok(Ok(None)));
        assert_eq!(absent, "request row missing at snapshot time");
    }

    #[test]
    fn renders_query_failure_and_timeout_markers() {
        let failed = render_snapshot(ts(), Ok(Err(())));
        assert_eq!(failed, "<snapshot query failed>");

        let timed_out = render_snapshot(ts(), Err(()));
        assert_eq!(timed_out, "<snapshot timed out after 1s>");
    }
}
