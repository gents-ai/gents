use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::document_config::TaskHook;
use crate::hook::BackgroundExecutionRegistry;
use crate::identity::{KeyIdentity, NodeIdentity};
use crate::lean_vocab_test::{
    lean_recovery_sweep_cases, lean_task_hook_recovery_cases, LeanCommandResult, LeanTaskHook,
};
use crate::lifecycle::{ExecutionOrigin, RequestLifecycle, TriggerLineage};
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
        let node = Arc::new(
            defra_node::EmbeddedNode::builder()
                .data_path(data.path())
                .build()
                .await
                .expect("embedded node"),
        );
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let identity: Arc<dyn NodeIdentity> = Arc::new(
            KeyIdentity::load_or_create(data.path().join("agent.key"), None).expect("identity"),
        );
        crate::test_support::install_test_agent(node.as_ref(), identity.did(), AGENT_ID).await;
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

    /// A request left processing by an execution that is gone, with an
    /// expired lease: exactly what request recovery terminalizes.
    async fn abandoned_request(&self, interrupt_requested: bool) -> String {
        let mut lifecycle = self.claimed(Duration::from_secs(1)).await;
        let writer =
            crate::streaming::DefraStreamWriter::new(self.node.clone(), self.did(), Duration::ZERO);
        lifecycle.begin_owned_execution(&writer).await.unwrap();
        let doc_id = lifecycle.request().doc_id.clone();
        if interrupt_requested {
            crate::interrupt::interrupt_request_by_doc_id(
                self.node.as_ref(),
                &doc_id,
                self.did(),
                Some(self.did()),
            )
            .await
            .unwrap();
        }
        drop(lifecycle);
        tokio::time::sleep(Duration::from_millis(1_200)).await;
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

/// A real command that logs its own start and exits with `code`.
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
    }
}

/// An observed attempt of unknown outcome was recorded as started with no
/// result; every other observation is its recorded result.
fn recorded(result: &LeanCommandResult) -> Option<HookCommandResult> {
    (*result != LeanCommandResult::Interrupted).then(|| result.to_native())
}

/// Writes the durable record a crashed, started execution left behind.
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

/// Drives every generated recovery case through the production startup
/// sequence: request recovery decides the terminal from the latch it reads,
/// then task hook recovery runs remaining cleanup from a durable host record.
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

/// Drives the task hook sweep's generated witnesses through the production
/// sweep: a resolved request's record is released, one whose request is still
/// owned or whose executor is live is left untouched.
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
        effective_timeout_secs: 30,
    }
}

/// A restart while a hook command survives stops that command through its
/// recorded identity before running the remaining cleanup.
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

/// Runtime shutdown cancels a recovered cleanup command like a live one: the
/// launched attempt is recorded interrupted and never replayed, and cleanup
/// shutdown kept from launching stays recorded for the next start.
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

/// Recovered cleanup runs only from a root its owner still admits; otherwise
/// each remaining occurrence is recorded as refused and the record dropped.
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
