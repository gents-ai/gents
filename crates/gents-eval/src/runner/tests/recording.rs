use super::*;
use crate::test_support::doubles::{
    DiscardingExecutor, FaultingRecorder, FeedbackCheck, SettlesOnCancel,
};

#[tokio::test]
async fn a_two_cell_run_completes_every_slot_and_writes_verdicts_before_completion() {
    let (launching, pack) = launching("captured_rows_count").await;
    let request = request(&launching, &pack, "run-1");
    let executor = ScriptedExecutor::new().with_default(passed());
    let recorder = FaultingRecorder::new(&launching.access);

    let frozen = freeze(&launching.access, &request, Isolation::Embedded)
        .await
        .unwrap();
    let outcome = execute_frozen(
        &frozen,
        &recorder,
        &executor,
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &options(),
    )
    .await
    .unwrap();

    assert_eq!(
        outcome,
        RunOutcome {
            run_id: "run-1".into(),
            completed: 8,
            abandoned: 0,
            not_evidence: 0,
            breaker_tripped: false,
            cancelled: false,
        }
    );

    let trials = load_trials(&launching.access, launching.owner(), "run-1")
        .await
        .unwrap();
    assert_eq!(trials.len(), 8);
    assert!(trials.iter().all(|trial| trial.completion.is_some()));
    assert!(
        trials
            .iter()
            .all(|trial| trial.identity.seed == 1000 + trial.identity.trial_index as i64),
        "each trial index is seeded from the run's seed base"
    );
    let completion = trials[0].completion.as_ref().expect("a completion");
    assert_eq!(completion.stages.len(), 1);
    assert_eq!(completion.stages[0].stage_id, "check");
    assert_eq!(completion.stages[0].failure_kind, None);

    let verdicts = load_verdicts(&launching.access, launching.owner(), "run-1")
        .await
        .unwrap();
    assert_eq!(verdicts.len(), 8);
    assert!(verdicts
        .iter()
        .all(|verdict| verdict.kind == OutcomeKind::Passed && verdict.feedback.is_none()));

    for trial in &trials {
        let path = frozen
            .run_dir
            .join("trials")
            .join(&trial.identity.trial_id)
            .join("evidence.json");
        let recorded: Value =
            serde_json::from_slice(&std::fs::read(&path).expect("an evidence record"))
                .expect("the evidence record parses");
        assert_eq!(recorded["evidence_digest"], passed().evidence_digest);
        assert_eq!(recorded["anchor"]["requests"], 1, "{recorded}");
    }

    let calls = recorder.log();
    for trial in &trials {
        let id = &trial.identity.trial_id;
        let position = |method: &str| {
            calls
                .iter()
                .position(|(name, subject)| *name == method && subject == id)
                .unwrap_or_else(|| panic!("no {method} call for {id}"))
        };
        assert!(
            position("create_trial") < position("append_verdict"),
            "the row exists before its verdicts"
        );
        assert!(
            position("append_verdict") < position("complete_trial"),
            "a trial's verdicts are written before its completion"
        );
    }
}

#[tokio::test]
async fn a_crash_after_create_trial_is_repaired_by_resume() {
    let (launching, pack) = launching("captured_rows_count").await;
    let mut request = request(&launching, &pack, "run-1");
    one_slot(&mut request);
    let executor = ScriptedExecutor::new().with_default(passed());
    let registry = CheckRegistry::builtin();
    let recorder = FaultingRecorder::new(&launching.access).failing("complete_trial", 1);

    let frozen = freeze(&launching.access, &request, Isolation::Embedded)
        .await
        .unwrap();
    let error = execute_frozen(
        &frozen,
        &recorder,
        &executor,
        &registry,
        CancellationToken::new(),
        &options(),
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("injected"), "{error:#}");

    let trials = load_trials(&launching.access, launching.owner(), "run-1")
        .await
        .unwrap();
    assert_eq!(
        trial_keys(&trials),
        [("base".into(), "case-a".into(), 0, 1, false)]
    );
    let verdicts = load_verdicts(&launching.access, launching.owner(), "run-1")
        .await
        .unwrap();
    assert_eq!(verdicts.len(), 1);
    assert_eq!(verdicts[0].trial_id, trials[0].identity.trial_id);

    let outcome = resume(
        &launching.access,
        RunLocation {
            owner: launching.owner(),
            run_id: "run-1",
            runs_dir: &launching.runs_dir(),
        },
        &executor,
        &registry,
        CancellationToken::new(),
        &options(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.completed, 1);

    let trials = load_trials(&launching.access, launching.owner(), "run-1")
        .await
        .unwrap();
    assert_eq!(
        trial_keys(&trials),
        [
            ("base".to_string(), "case-a".to_string(), 0, 1, false),
            ("base".to_string(), "case-a".to_string(), 0, 2, true),
        ],
        "the abandoned row stays, and the repaired attempt completes"
    );
}

#[tokio::test]
async fn a_trial_whose_row_cannot_be_written_is_handed_back_to_the_executor() {
    let (launching, pack) = launching("captured_rows_count").await;
    let mut request = request(&launching, &pack, "run-1");
    one_slot(&mut request);
    let executor = DiscardingExecutor {
        inner: ScriptedExecutor::new().with_default(passed()),
        discarded: Mutex::new(Vec::new()),
    };
    let recorder = FaultingRecorder::new(&launching.access).failing("create_trial", 1);

    let frozen = freeze(&launching.access, &request, Isolation::Embedded)
        .await
        .unwrap();
    let error = execute_frozen(
        &frozen,
        &recorder,
        &executor,
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &options(),
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("injected"), "{error:#}");

    let owed = plan(&frozen.record.origin, "run-1", &[], 1);
    assert_eq!(owed.len(), 1);
    assert_eq!(
        executor
            .discarded
            .lock()
            .expect("the discard log")
            .as_slice(),
        [owed[0].trial_id.clone()],
        "the provisioned trial was released"
    );
    assert!(
        executor
            .inner
            .calls
            .lock()
            .expect("the execute log")
            .is_empty(),
        "a trial whose row was never written is not run"
    );
    assert!(
        load_trials(&launching.access, launching.owner(), "run-1")
            .await
            .unwrap()
            .is_empty(),
        "the row the loop failed to write is not there"
    );
}

#[tokio::test]
async fn an_error_in_one_trial_drains_the_trials_in_flight() {
    let (launching, pack) = launching("captured_rows_count").await;
    let mut request = request(&launching, &pack, "run-drain");
    request.concurrency = 2;
    let frozen = freeze(&launching.access, &request, Isolation::Embedded)
        .await
        .unwrap();
    let executor = SettlesOnCancel::default();
    let recorder = FaultingRecorder::new(&launching.access).failing("create_trial", 2);
    let error = execute_frozen(
        &frozen,
        &recorder,
        &executor,
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &options(),
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("injected"), "{error:#}");
    assert_eq!(
        executor.settled.load(Ordering::SeqCst),
        1,
        "the trial in flight saw the cancellation and wound down"
    );
}

#[tokio::test]
async fn feedback_is_dropped_off_train() {
    let (launching, pack) = launching("feedback_check").await;
    let registry = CheckRegistry::builtin().with(Box::new(FeedbackCheck {
        feedback: "more rows next time",
    }));
    let executor = ScriptedExecutor::new().with_default(passed());

    let mut off_train = request(&launching, &pack, "run-1");
    one_slot(&mut off_train);
    run(
        &launching.access,
        &off_train,
        &executor,
        &registry,
        CancellationToken::new(),
        &options(),
    )
    .await
    .unwrap();
    let verdicts = load_verdicts(&launching.access, launching.owner(), "run-1")
        .await
        .unwrap();
    assert_eq!(verdicts.len(), 1);
    assert_eq!(verdicts[0].check, "feedback_check");
    assert_eq!(verdicts[0].feedback, None, "validation keeps no feedback");

    let mut on_train = request(&launching, &pack, "run-2");
    on_train.cells.truncate(1);
    on_train.split = EvalSplit::Train;
    on_train.case_ids = Some(vec!["train-case".into()]);
    on_train.trials_per_case = 1;
    run(
        &launching.access,
        &on_train,
        &executor,
        &registry,
        CancellationToken::new(),
        &options(),
    )
    .await
    .unwrap();
    let verdicts = load_verdicts(&launching.access, launching.owner(), "run-2")
        .await
        .unwrap();
    assert_eq!(verdicts.len(), 1);
    assert_eq!(verdicts[0].feedback.as_deref(), Some("more rows next time"));
}

#[tokio::test]
async fn concurrency_32_writes_the_same_documents_as_one() {
    async fn run_at(
        concurrency: u32,
    ) -> (
        Vec<(String, String, u32, u32, bool)>,
        Vec<(String, String, String, &'static str, Option<u32>)>,
    ) {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-1");
        request.concurrency = concurrency;
        request.trials_per_case = 8;
        let outcome = run(
            &launching.access,
            &request,
            &ScriptedExecutor::new().with_default(passed()),
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();
        assert_eq!(outcome.completed, 32);
        let trials = load_trials(&launching.access, launching.owner(), "run-1")
            .await
            .unwrap();
        let verdicts = load_verdicts(&launching.access, launching.owner(), "run-1")
            .await
            .unwrap();
        (trial_keys(&trials), verdict_keys(&verdicts))
    }

    let (serial_trials, serial_verdicts) = run_at(1).await;
    let (parallel_trials, parallel_verdicts) = run_at(32).await;
    assert_eq!(serial_trials, parallel_trials);
    assert_eq!(serial_verdicts, parallel_verdicts);
}
