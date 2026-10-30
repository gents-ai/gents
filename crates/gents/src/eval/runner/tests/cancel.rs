use super::*;

struct CancelOnFirstExecute {
    run: CancellationToken,
}

#[async_trait::async_trait]
impl TrialExecutor for CancelOnFirstExecute {
    fn isolation(&self) -> Isolation {
        Isolation::Embedded
    }

    async fn provision(&self, _spec: &TrialSpec) -> TrialLocator {
        TrialLocator {
            trial_node_did: "did:key:trial".into(),
            session_id: "session".into(),
            home_hint: None,
        }
    }

    async fn execute(&self, _spec: &TrialSpec, cancel: CancellationToken) -> TrialEvidence {
        self.run.cancel();
        cancel.cancelled().await;
        passed()
    }

    async fn recollect(&self, _at: &TrialLocator, _captures: &[Capture]) -> Option<TrialEvidence> {
        None
    }
}

#[tokio::test]
async fn cancel_stops_launching_and_leaves_in_flight_rows_null() {
    let (launching, pack) = launching("captured_rows_count").await;
    let mut request = request(&launching, &pack, "run-1");
    request.cells.truncate(1);
    request.trials_per_case = 1;

    let cancel = CancellationToken::new();
    let executor = CancelOnFirstExecute {
        run: cancel.clone(),
    };
    let outcome = run(
        &launching.access,
        &request,
        &executor,
        &CheckRegistry::builtin(),
        cancel,
        &options(),
    )
    .await
    .unwrap();
    assert_eq!((outcome.completed, outcome.abandoned), (0, 1));

    let trials = load_trials(&launching.access, OWNER, "run-1")
        .await
        .unwrap();
    assert_eq!(trials.len(), 1, "the second slot is never launched");
    assert!(trials[0].completion.is_none());
    assert!(load_verdicts(&launching.access, OWNER, "run-1")
        .await
        .unwrap()
        .is_empty());
}

struct MarkOnFirstExecute {
    inner: ScriptedExecutor,
    runs_dir: PathBuf,
    run_id: String,
    marked: AtomicBool,
}

#[async_trait::async_trait]
impl TrialExecutor for MarkOnFirstExecute {
    fn isolation(&self) -> Isolation {
        self.inner.isolation()
    }

    fn wants_script_key(&self) -> bool {
        self.inner.wants_script_key()
    }

    async fn provision(&self, spec: &TrialSpec) -> TrialLocator {
        self.inner.provision(spec).await
    }

    async fn execute(&self, spec: &TrialSpec, cancel: CancellationToken) -> TrialEvidence {
        if !self.marked.swap(true, Ordering::SeqCst) {
            request_cancel(&self.runs_dir, &self.run_id).unwrap();
        }
        self.inner.execute(spec, cancel).await
    }

    async fn recollect(&self, at: &TrialLocator, captures: &[Capture]) -> Option<TrialEvidence> {
        self.inner.recollect(at, captures).await
    }
}

#[tokio::test]
async fn a_cancel_marker_stops_the_loop_at_its_next_launch_and_resume_clears_it() {
    let (launching, pack) = launching("captured_rows_count").await;
    let mut request = request(&launching, &pack, "run-marker");
    request.cells.truncate(1);
    request.trials_per_case = 1;
    let caller = CancellationToken::new();
    let executor = MarkOnFirstExecute {
        inner: ScriptedExecutor::new().with_default(passed()),
        runs_dir: launching.runs_dir(),
        run_id: "run-marker".into(),
        marked: AtomicBool::new(false),
    };
    let outcome = run(
        &launching.access,
        &request,
        &executor,
        &CheckRegistry::builtin(),
        caller.clone(),
        &options(),
    )
    .await
    .unwrap();
    assert_eq!(
        (outcome.completed, outcome.abandoned),
        (1, 0),
        "the trial that was running finishes; the next is never launched"
    );
    assert!(outcome.cancelled, "the pass says why it stopped");
    assert!(
        !caller.is_cancelled(),
        "the marker stops this run, never the caller's other work"
    );
    let marker = launching.runs_dir().join("run-marker").join(CANCEL_MARKER);
    assert!(marker.exists());
    assert_eq!(
        load_trials(&launching.access, OWNER, "run-marker")
            .await
            .unwrap()
            .len(),
        1
    );

    let resumed = resume(
        &launching.access,
        OWNER,
        "run-marker",
        &launching.runs_dir(),
        &ScriptedExecutor::new().with_default(passed()),
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &options(),
    )
    .await
    .unwrap();
    assert_eq!(resumed.completed, 1);
    assert!(
        !marker.exists(),
        "resume removes the marker before it plans"
    );
    assert_eq!(
        load_trials(&launching.access, OWNER, "run-marker")
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn a_marker_written_before_the_loop_starts_launches_nothing() {
    let (launching, pack) = launching("captured_rows_count").await;
    let mut request = request(&launching, &pack, "run-early");
    one_slot(&mut request);
    let frozen = freeze(&launching.access, &request, Isolation::Embedded)
        .await
        .unwrap();
    request_cancel(&launching.runs_dir(), "run-early").unwrap();
    let outcome = execute_frozen(
        &frozen,
        &DocumentRecorder::new(&launching.access),
        &ScriptedExecutor::new().with_default(passed()),
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &options(),
    )
    .await
    .unwrap();
    assert_eq!(
        outcome,
        RunOutcome {
            run_id: "run-early".into(),
            cancelled: true,
            ..RunOutcome::default()
        }
    );
    assert!(load_trials(&launching.access, OWNER, "run-early")
        .await
        .unwrap()
        .is_empty());
}

struct MarkAfterComplete<'a> {
    inner: DocumentRecorder<'a>,
    runs_dir: PathBuf,
}

#[async_trait::async_trait]
impl Recorder for MarkAfterComplete<'_> {
    async fn create_trial(&self, owner: &str, identity: &TrialIdentity) -> Result<()> {
        self.inner.create_trial(owner, identity).await
    }

    async fn append_verdict(
        &self,
        owner: &str,
        split: EvalSplit,
        draft: &VerdictDraft,
    ) -> Result<()> {
        self.inner.append_verdict(owner, split, draft).await
    }

    async fn complete_trial(
        &self,
        owner: &str,
        trial_id: &str,
        completion: &TrialCompletion,
    ) -> Result<()> {
        self.inner
            .complete_trial(owner, trial_id, completion)
            .await?;
        request_cancel(&self.runs_dir, "run-late").map(|_| ())
    }

    async fn load_trials(&self, owner: &str, run_id: &str) -> Result<Vec<TrialRecord>> {
        self.inner.load_trials(owner, run_id).await
    }
}

#[tokio::test]
async fn a_marker_written_after_the_last_completion_is_not_a_cancelled_run() {
    let (launching, pack) = launching("captured_rows_count").await;
    let mut request = request(&launching, &pack, "run-late");
    one_slot(&mut request);
    let frozen = freeze(&launching.access, &request, Isolation::Embedded)
        .await
        .unwrap();
    let recorder = MarkAfterComplete {
        inner: DocumentRecorder::new(&launching.access),
        runs_dir: launching.runs_dir(),
    };
    let outcome = execute_frozen(
        &frozen,
        &recorder,
        &ScriptedExecutor::new().with_default(passed()),
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &options(),
    )
    .await
    .unwrap();
    assert!(
        launching
            .runs_dir()
            .join("run-late")
            .join(CANCEL_MARKER)
            .exists(),
        "the marker landed"
    );
    assert_eq!(outcome.completed, 1);
    assert!(!outcome.cancelled, "the run owes nothing: {outcome:?}");
}

#[tokio::test]
async fn run_on_an_existing_run_id_clears_the_marker_like_resume() {
    let (launching, pack) = launching("captured_rows_count").await;
    let mut request = request(&launching, &pack, "run-again");
    one_slot(&mut request);
    freeze(&launching.access, &request, Isolation::Embedded)
        .await
        .unwrap();
    let marker = request_cancel(&launching.runs_dir(), "run-again").unwrap();
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
    assert_eq!(outcome.completed, 1);
    assert!(!outcome.cancelled);
    assert!(!marker.exists());
}

#[tokio::test]
async fn a_cancelled_slot_is_planned_again_under_no_infrastructure_retries() {
    let (launching, pack) = launching("captured_rows_count").await;
    let mut request = request(&launching, &pack, "run-abandoned");
    one_slot(&mut request);
    request.max_infra_retries = 0;
    let cancel = CancellationToken::new();
    let outcome = run(
        &launching.access,
        &request,
        &CancelOnFirstExecute {
            run: cancel.clone(),
        },
        &CheckRegistry::builtin(),
        cancel,
        &options(),
    )
    .await
    .unwrap();
    assert_eq!((outcome.completed, outcome.abandoned), (0, 1));

    let resumed = resume(
        &launching.access,
        OWNER,
        "run-abandoned",
        &launching.runs_dir(),
        &ScriptedExecutor::new().with_default(passed()),
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &options(),
    )
    .await
    .unwrap();
    assert_eq!(resumed.completed, 1, "the abandoned attempt spent no retry");
    let mut attempts: Vec<(u32, bool)> = load_trials(&launching.access, OWNER, "run-abandoned")
        .await
        .unwrap()
        .iter()
        .map(|trial| (trial.identity.attempt, trial.completion.is_some()))
        .collect();
    attempts.sort();
    assert_eq!(attempts, vec![(1, false), (2, true)]);
}

struct MarksThenWaits {
    runs_dir: PathBuf,
    run_id: String,
    interrupted: AtomicBool,
}

#[async_trait::async_trait]
impl TrialExecutor for MarksThenWaits {
    fn isolation(&self) -> Isolation {
        Isolation::Embedded
    }

    async fn provision(&self, _spec: &TrialSpec) -> TrialLocator {
        TrialLocator {
            trial_node_did: "did:key:trial".into(),
            session_id: "session".into(),
            home_hint: None,
        }
    }

    async fn execute(&self, _spec: &TrialSpec, cancel: CancellationToken) -> TrialEvidence {
        request_cancel(&self.runs_dir, &self.run_id).unwrap();
        tokio::select! {
            _ = cancel.cancelled() => self.interrupted.store(true, Ordering::SeqCst),
            _ = tokio::time::sleep(Duration::from_secs(30)) => {}
        }
        passed()
    }

    async fn recollect(&self, _at: &TrialLocator, _captures: &[Capture]) -> Option<TrialEvidence> {
        None
    }
}

#[tokio::test]
async fn a_marker_written_mid_trial_interrupts_the_trial_in_flight() {
    let (launching, pack) = launching("captured_rows_count").await;
    let mut request = request(&launching, &pack, "run-mid");
    one_slot(&mut request);
    let executor = MarksThenWaits {
        runs_dir: launching.runs_dir(),
        run_id: "run-mid".into(),
        interrupted: AtomicBool::new(false),
    };
    let outcome = run(
        &launching.access,
        &request,
        &executor,
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &options(),
    )
    .await
    .unwrap();
    assert!(
        executor.interrupted.load(Ordering::SeqCst),
        "the trial saw its token cancelled, not its 30 s timeout"
    );
    assert_eq!((outcome.completed, outcome.abandoned), (0, 1));
    let trials = load_trials(&launching.access, OWNER, "run-mid")
        .await
        .unwrap();
    assert_eq!(trials.len(), 1);
    assert!(trials[0].completion.is_none(), "left open for a resume");
}
