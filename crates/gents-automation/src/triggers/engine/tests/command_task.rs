use std::path::PathBuf;

use gents_documents::{CommandRun, CommandRunState, TaskCommand};
use gents_store::test_support::STALL_CEILING;

use super::*;
use crate::triggers::engine::command::{admit_command, CommandHost};

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Records every run write and budget stop, waking waiters on each.
struct SpyCommandHost {
    root: tempfile::TempDir,
    runs: Mutex<Vec<(String, CommandRun)>>,
    stops: Mutex<Vec<(String, String)>>,
    changed: Notify,
}

impl SpyCommandHost {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            root: tempfile::tempdir().unwrap(),
            runs: Mutex::new(Vec::new()),
            stops: Mutex::new(Vec::new()),
            changed: Notify::new(),
        })
    }

    /// Waits for the recorded runs to satisfy `done`, failing with the last
    /// records seen once none arrive for [`STALL_CEILING`].
    async fn runs_when(&self, done: impl Fn(&[(String, CommandRun)]) -> bool) -> Vec<CommandRun> {
        loop {
            let changed = self.changed.notified();
            {
                let runs = self.runs.lock().unwrap();
                if done(&runs) {
                    return runs.iter().map(|(_, run)| run.clone()).collect();
                }
            }
            if tokio::time::timeout(STALL_CEILING, changed).await.is_err() {
                panic!(
                    "command runs stalled: {:#?}",
                    self.runs.lock().unwrap().clone()
                );
            }
        }
    }

    fn finished(runs: &[(String, CommandRun)], run_id: &str) -> bool {
        runs.iter()
            .any(|(_, run)| run.run_id == run_id && run.finished_at.is_some())
    }
}

impl CommandHost for SpyCommandHost {
    fn command_root(&self) -> BoxFuture<'_, anyhow::Result<PathBuf>> {
        let root = self.root.path().to_path_buf();
        Box::pin(async move { Ok(root) })
    }

    fn record_run<'a>(
        &'a self,
        _node_did: &'a str,
        trigger_id: &'a str,
        run: &'a CommandRun,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        self.runs
            .lock()
            .unwrap()
            .push((trigger_id.to_owned(), run.clone()));
        self.changed.notify_waiters();
        Box::pin(async { Ok(()) })
    }

    fn stop_for_budget<'a>(
        &'a self,
        _node_did: &'a str,
        trigger_id: &'a str,
        reason: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        self.stops
            .lock()
            .unwrap()
            .push((trigger_id.to_owned(), reason.to_owned()));
        Box::pin(async { Ok(()) })
    }
}

fn command_task(argv: &[&str], timeout_secs: Option<i64>) -> ResolvedTask {
    ResolvedTask {
        emit_outcome: false,
        task_id: "tick".into(),
        name: None,
        agent_did: String::new(),
        prompt_template: String::new(),
        goal_objective_template: None,
        goal_token_budget: None,
        goal_max_infrastructure_retries: None,
        output_schema_ref: None,
        hooks: Vec::new(),
        command: Some(TaskCommand {
            argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
            timeout_secs,
        }),
    }
}

struct CommandFixture {
    engine: TriggerEngine,
    host: Arc<SpyCommandHost>,
    materializer: Arc<SpyMaterializer>,
    _snapshot: watch::Sender<Arc<ActiveRuntimeSnapshot>>,
}

impl Drop for CommandFixture {
    fn drop(&mut self) {
        self.engine.command_shutdown.cancel();
    }
}

fn command_fixture(
    task: ResolvedTask,
    concurrency: ConcurrencyMode,
    budget: u32,
) -> CommandFixture {
    let mut schedule = resolved_schedule_with_concurrency("tick-trigger", task, concurrency);
    schedule.max_fires_per_hour = budget;
    let (tx, rx) = watch::channel(snapshot_with_schedules(RapidHashMap::from_iter([(
        "tick-trigger".to_owned(),
        schedule,
    )])));
    let host = SpyCommandHost::new();
    let materializer = SpyMaterializer::new();
    let engine = TriggerEngine::new(rx, materializer.clone()).with_command_host(host.clone());
    CommandFixture {
        engine,
        host,
        materializer,
        _snapshot: tx,
    }
}

fn schedule_fire(fixture: &CommandFixture, fire_key: &str) -> FireIntent {
    let snapshot = fixture.engine.snapshot_rx.borrow().clone();
    let schedule = snapshot.active_schedules().get("tick-trigger").unwrap();
    FireIntent {
        trigger_id: Some("tick-trigger".into()),
        trigger_kind: TriggerKind::Schedule,
        task: schedule.task.clone(),
        concurrency: schedule.concurrency,
        event_vars: serde_json::json!({"trigger_kind": "schedule"}),
        doc_vars: None,
        correlation: None,
        group_vars: None,
        trigger_context: None,
        args_vars: None,
        durable_fire_key: fire_key.into(),
        pre_materialized_request_id: None,
        event_source_generation: None,
        on_result: Box::new(|_| {}),
    }
}

fn started(result: FireResult) -> String {
    match result {
        FireResult::CommandStarted { run_id } => run_id,
        other => panic!("expected the command to start, got {other:?}"),
    }
}

fn record_of(runs: &[CommandRun], run_id: &str) -> CommandRun {
    runs.iter()
        .rev()
        .find(|run| run.run_id == run_id)
        .cloned()
        .unwrap_or_else(|| panic!("no record of run {run_id}: {runs:#?}"))
}

#[tokio::test]
async fn command_admission_matches_lean() {
    gents_lean_vocab::families::triggers_command_run_admit::replay(|input| {
        admit_command(input.concurrency, input.running)
    });
}

#[tokio::test]
async fn a_command_fire_runs_its_command_and_records_exit_and_output_without_a_request() {
    let fixture = command_fixture(
        command_task(&["sh", "-c", "pwd; echo to-stderr >&2; exit 3"], None),
        ConcurrencyMode::Parallel,
        gents_documents::DEFAULT_MAX_FIRES_PER_HOUR,
    );
    let run_id = started(
        fixture
            .engine
            .dispatch(schedule_fire(&fixture, "schedule:tick:1"))
            .await,
    );
    let runs = fixture
        .host
        .runs_when(|runs| SpyCommandHost::finished(runs, &run_id))
        .await;
    assert_eq!(runs[0].state, CommandRunState::Running, "{runs:#?}");
    let finished = record_of(&runs, &run_id);
    assert_eq!(finished.state, CommandRunState::Failed);
    assert_eq!(finished.exit_code, Some(3));
    let tail = finished.output_tail.unwrap();
    let root = fixture.host.root.path().canonicalize().unwrap();
    assert!(
        tail.contains(root.to_str().unwrap()) && tail.contains("to-stderr"),
        "the run ran in the command root and kept both streams: {tail}"
    );
    match fixture
        .engine
        .dispatch(schedule_fire(&fixture, "schedule:tick:1"))
        .await
    {
        FireResult::Duplicate { request_id } => assert_eq!(request_id, run_id),
        other => panic!("a source re-emitting a finished fire is a duplicate, got {other:?}"),
    }
    assert!(
        fixture.materializer.calls().is_empty(),
        "a command fire writes no AgentRequest"
    );
}

#[tokio::test]
async fn a_command_past_its_timeout_is_recorded_as_timed_out() {
    let fixture = command_fixture(
        command_task(&["sleep", "30"], Some(1)),
        ConcurrencyMode::Parallel,
        gents_documents::DEFAULT_MAX_FIRES_PER_HOUR,
    );
    let run_id = started(
        fixture
            .engine
            .dispatch(schedule_fire(&fixture, "schedule:tick:1"))
            .await,
    );
    let runs = fixture
        .host
        .runs_when(|runs| SpyCommandHost::finished(runs, &run_id))
        .await;
    assert_eq!(record_of(&runs, &run_id).state, CommandRunState::TimedOut);
}

#[tokio::test]
async fn a_serial_trigger_skips_a_fire_while_its_command_runs() {
    let fixture = command_fixture(
        command_task(&["sleep", "30"], None),
        ConcurrencyMode::Serial,
        gents_documents::DEFAULT_MAX_FIRES_PER_HOUR,
    );
    let first = started(
        fixture
            .engine
            .dispatch(schedule_fire(&fixture, "schedule:tick:1"))
            .await,
    );
    match fixture
        .engine
        .dispatch(schedule_fire(&fixture, "schedule:tick:2"))
        .await
    {
        FireResult::Skipped { reason } => assert_eq!(reason, SERIAL_BUSY),
        other => panic!("a serial fire must skip while a run is going, got {other:?}"),
    }
    match fixture
        .engine
        .dispatch(schedule_fire(&fixture, "schedule:tick:1"))
        .await
    {
        FireResult::Duplicate { request_id } => assert_eq!(request_id, first),
        other => panic!("the running fire's own key is a duplicate, got {other:?}"),
    }
    fixture.engine.command_shutdown.cancel();
    let runs = fixture
        .host
        .runs_when(|runs| SpyCommandHost::finished(runs, &first))
        .await;
    assert_eq!(record_of(&runs, &first).state, CommandRunState::Interrupted);
    assert_eq!(
        runs.iter()
            .filter(|run| run.state == CommandRunState::Running)
            .count(),
        1,
        "only one command ever started: {runs:#?}"
    );
}

#[tokio::test]
async fn a_latest_only_fire_interrupts_the_running_command_before_its_own_starts() {
    let fixture = command_fixture(
        command_task(&["sleep", "30"], None),
        ConcurrencyMode::LatestOnly,
        gents_documents::DEFAULT_MAX_FIRES_PER_HOUR,
    );
    let first = started(
        fixture
            .engine
            .dispatch(schedule_fire(&fixture, "schedule:tick:1"))
            .await,
    );
    fixture
        .host
        .runs_when(|runs| runs.iter().any(|(_, run)| run.run_id == first))
        .await;
    let second = started(
        fixture
            .engine
            .dispatch(schedule_fire(&fixture, "schedule:tick:2"))
            .await,
    );
    let runs = fixture
        .host
        .runs_when(|runs| {
            SpyCommandHost::finished(runs, &first)
                && runs.iter().any(|(_, run)| run.run_id == second)
        })
        .await;
    let order = runs
        .iter()
        .map(|run| (run.run_id == first, run.state))
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec![
            (true, CommandRunState::Running),
            (true, CommandRunState::Interrupted),
            (false, CommandRunState::Running),
        ],
        "the superseded run ends before the latest one starts, so last_run ends on the latest"
    );
}

#[tokio::test]
async fn a_parallel_trigger_runs_overlapping_commands() {
    let fixture = command_fixture(
        command_task(&["sleep", "30"], None),
        ConcurrencyMode::Parallel,
        gents_documents::DEFAULT_MAX_FIRES_PER_HOUR,
    );
    let first = started(
        fixture
            .engine
            .dispatch(schedule_fire(&fixture, "schedule:tick:1"))
            .await,
    );
    let second = started(
        fixture
            .engine
            .dispatch(schedule_fire(&fixture, "schedule:tick:2"))
            .await,
    );
    let runs = fixture
        .host
        .runs_when(|runs| {
            [&first, &second]
                .iter()
                .all(|id| runs.iter().any(|(_, run)| &run.run_id == *id))
        })
        .await;
    assert!(
        runs.iter().all(|run| run.state == CommandRunState::Running),
        "both runs are going at once: {runs:#?}"
    );
}

#[tokio::test]
async fn a_queued_serial_command_fire_is_refused() {
    let fixture = command_fixture(
        command_task(&["true"], None),
        ConcurrencyMode::QueuedSerial,
        gents_documents::DEFAULT_MAX_FIRES_PER_HOUR,
    );
    match fixture
        .engine
        .dispatch(schedule_fire(&fixture, "schedule:tick:1"))
        .await
    {
        FireResult::Rejected { error } => assert!(error.contains("queued_serial"), "{error}"),
        other => panic!("queued_serial has no queue for a command, got {other:?}"),
    }
    assert!(fixture.host.runs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_command_trigger_past_its_fire_budget_stops_without_running() {
    let fixture = command_fixture(command_task(&["true"], None), ConcurrencyMode::Parallel, 1);
    let first = started(
        fixture
            .engine
            .dispatch(schedule_fire(&fixture, "schedule:tick:1"))
            .await,
    );
    match fixture
        .engine
        .dispatch(schedule_fire(&fixture, "schedule:tick:2"))
        .await
    {
        FireResult::BudgetExhausted { reason } => {
            assert!(reason.contains("budget of 1 fires"), "{reason}")
        }
        other => panic!("the second fire in the hour exceeds a budget of one, got {other:?}"),
    }
    let stops = fixture.host.stops.lock().unwrap().clone();
    assert_eq!(stops.len(), 1, "the Trigger is stopped once: {stops:?}");
    assert_eq!(stops[0].0, "tick-trigger");
    let runs = fixture
        .host
        .runs_when(|runs| SpyCommandHost::finished(runs, &first))
        .await;
    assert!(
        runs.iter().all(|run| run.run_id == first),
        "only the admitted fire ran: {runs:#?}"
    );
}

#[tokio::test]
async fn a_latest_only_fire_past_its_budget_leaves_the_running_command_going() {
    let fixture = command_fixture(
        command_task(&["sleep", "1"], None),
        ConcurrencyMode::LatestOnly,
        1,
    );
    let first = started(
        fixture
            .engine
            .dispatch(schedule_fire(&fixture, "schedule:tick:1"))
            .await,
    );
    match fixture
        .engine
        .dispatch(schedule_fire(&fixture, "schedule:tick:2"))
        .await
    {
        FireResult::BudgetExhausted { .. } => {}
        other => panic!("the second fire in the hour exceeds a budget of one, got {other:?}"),
    }
    let runs = fixture
        .host
        .runs_when(|runs| SpyCommandHost::finished(runs, &first))
        .await;
    assert_eq!(
        record_of(&runs, &first).state,
        CommandRunState::Succeeded,
        "a fire the budget refused supersedes nothing: {runs:#?}"
    );
}

#[tokio::test]
async fn an_event_fire_of_a_command_task_is_refused() {
    let fixture = command_fixture(
        command_task(&["true"], None),
        ConcurrencyMode::Parallel,
        gents_documents::DEFAULT_MAX_FIRES_PER_HOUR,
    );
    let mut intent = schedule_fire(&fixture, "event:tick:1");
    intent.trigger_kind = TriggerKind::Event;
    match fixture.engine.dispatch(intent).await {
        FireResult::Rejected { error } => {
            assert!(error.contains("not an event Trigger"), "{error}")
        }
        other => panic!("event sources read as an Agent, got {other:?}"),
    }
}

/// A schedule fires a command Task through the production engine: the Node
/// runs the command in its tool root, records the run on the Trigger, and no
/// AgentRequest is written.
#[tokio::test]
async fn a_schedule_fires_a_command_task_with_no_agent_request_e2e() {
    let (db, _user, client, behavior) = signed_materializer_agent("command-task-e2e").await;
    let node = db.node.clone();
    let owner = behavior.node_did().to_owned();
    let config = serde_json::from_value(serde_json::json!({
        "node":{"node_did":owner},
        "default_agent_selection":{"node_did":owner,"default_agent_did":behavior.agent_did},
        "tasks":[{"node_did":owner,"task_id":"tick","command":{"argv":["sh","-c","echo ran > marker; echo tick-output"]}}],
        "schedules":[{"node_did":owner,"schedule_id":"every-minute","cadence":{"kind":"interval","interval_secs":60}}],
        "triggers":[{"node_did":owner,"trigger_id":"tick-trigger","task_id":"tick","source":{"kind":"schedule","schedule_id":"every-minute"},"concurrency":"serial"}]
    }))
    .unwrap();
    let trigger_doc_id = apply_user_config(&client, &config, "tick-trigger").await;
    let schedule_doc = node
        .execute(r#"{ Schedule(filter: { schedule_id: { _eq: "every-minute" } }) { _docID } }"#)
        .await;
    let schedule_doc_id =
        gents_store::graphql::rows::<serde_json::Value>(&schedule_doc, "Schedule").unwrap()[0]
            ["_docID"]
            .as_str()
            .unwrap()
            .to_owned();
    gents_documents::update_trigger_runtime_fields(
        &node,
        &owner,
        "tick-trigger",
        gents_documents::TriggerRuntimeUpdate {
            next_run_at: Some((Utc::now() - ChronoDuration::seconds(1)).to_rfc3339()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let task = command_task(&["sh", "-c", "echo ran > marker; echo tick-output"], None);
    let schedule = ResolvedSchedule {
        session_id_template: None,
        create_session: false,
        max_fires_per_hour: gents_documents::DEFAULT_MAX_FIRES_PER_HOUR,
        trigger_doc_id,
        schedule_doc_id,
        schedule_id: "every-minute".into(),
        task,
        cadence: ScheduleCadence::Interval { interval_secs: 60 },
        concurrency: ConcurrencyMode::Serial,
    };
    let snapshot = snapshot_with_agent_and_schedules(
        behavior.clone(),
        RapidHashMap::from_iter([("tick-trigger".to_owned(), schedule)]),
    );
    let (_tx, rx) = watch::channel(snapshot);
    let root = tempfile::tempdir().unwrap();
    gents_tools::surface::set_workspace_root_with_readers(
        &client,
        &owner,
        &root.path().canonicalize().unwrap().to_string_lossy(),
        None,
        true,
        &[],
    )
    .await
    .expect("the operator publishes the tool root");
    let cancel = CancellationToken::new();
    let source: Box<dyn TriggerSource> = Box::new(
        ScheduleSource::new(rx.clone(), node.clone(), cancel.clone())
            .with_tick_every(Duration::from_millis(50)),
    );
    let engine = TriggerEngine::new(
        rx.clone(),
        Arc::new(ProductionMaterializer::new(node.clone(), rx)),
    )
    .with_command_host(Arc::new(
        crate::triggers::engine::command::ProductionCommandHost::new(
            node.clone(),
            Some(root.path().to_path_buf()),
        ),
    ));
    let engine_cancel = cancel.clone();
    let engine_handle = tokio::spawn(async move { engine.run(vec![source], engine_cancel).await });

    let query = r#"{ Trigger(filter: { trigger_id: { _eq: "tick-trigger" } }) { last_status fire_count last_run } }"#;
    let trigger = gents_store::test_support::read_when_changed(
        &node,
        "Trigger",
        "the scheduled command finishes and its fire is counted",
        async || {
            let response = node.execute(query).await;
            let rows =
                gents_store::graphql::rows::<serde_json::Value>(&response, "Trigger").unwrap();
            let row = rows[0].clone();
            if row["last_run"]["finished_at"].is_string() && row["fire_count"] == 1 {
                Ok(row)
            } else {
                Err(row)
            }
        },
    )
    .await;
    cancel.cancel();
    let _ = engine_handle.await;

    let run: CommandRun = serde_json::from_value(trigger["last_run"].clone()).unwrap();
    assert_eq!(run.state, CommandRunState::Succeeded, "{trigger}");
    assert_eq!(run.exit_code, Some(0));
    assert_eq!(run.output_tail.as_deref(), Some("tick-output"));
    assert_eq!(trigger["last_status"], "fired");
    assert_eq!(trigger["fire_count"], 1);
    assert_eq!(
        std::fs::read_to_string(root.path().join("marker")).unwrap(),
        "ran\n",
        "the command ran in the node's tool root"
    );
    let explanation = client
        .transact_readonly("trigger.explain", |txn| {
            let owner = owner.as_str();
            Box::pin(gents_documents::trigger_explain::explain_trigger_in_txn(
                txn,
                owner,
                "tick-trigger",
                None,
            ))
        })
        .await
        .expect("trigger explain reads a command Task's Trigger");
    let steps = explanation.metadata["steps"].as_array().unwrap();
    let task_step = steps.iter().find(|step| step["name"] == "task").unwrap();
    assert_eq!(task_step["status"], "passed", "{steps:#?}");
    for access in [&client, &agent_access(&node, &behavior)] {
        let requests = access.execute("{ AgentRequest { _docID } }").await.unwrap();
        assert_eq!(
            requests["data"]["AgentRequest"],
            serde_json::json!([]),
            "a command Task's fire writes no AgentRequest"
        );
    }
}
