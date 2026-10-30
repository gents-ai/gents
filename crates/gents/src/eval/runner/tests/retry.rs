use super::*;

struct BreakerExecutor {
    gate: CancellationToken,
}

#[async_trait::async_trait]
impl TrialExecutor for BreakerExecutor {
    fn isolation(&self) -> Isolation {
        Isolation::Embedded
    }

    fn wants_script_key(&self) -> bool {
        true
    }

    async fn provision(&self, _spec: &TrialSpec) -> TrialLocator {
        TrialLocator {
            trial_node_did: "did:key:trial".into(),
            session_id: "session".into(),
            home_hint: None,
        }
    }

    async fn execute(&self, spec: &TrialSpec, _cancel: CancellationToken) -> TrialEvidence {
        let index = spec.script_key.as_ref().map_or(0, |key| key.trial_index);
        if index == 0 {
            return ScriptedExecutor::not_evidence("did:key:x");
        }
        self.gate.cancelled().await;
        passed()
    }

    async fn recollect(&self, _at: &TrialLocator, _captures: &[Capture]) -> Option<TrialEvidence> {
        None
    }
}

struct GatingRecorder<'a> {
    inner: DocumentRecorder<'a>,
    completions: Mutex<u32>,
    open_after: u32,
    gate: CancellationToken,
}

#[async_trait::async_trait]
impl Recorder for GatingRecorder<'_> {
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
        let mut completions = self.completions.lock().expect("the completion count");
        *completions += 1;
        if *completions >= self.open_after {
            self.gate.cancel();
        }
        Ok(())
    }

    async fn load_trials(&self, owner: &str, run_id: &str) -> Result<Vec<TrialRecord>> {
        self.inner.load_trials(owner, run_id).await
    }
}

#[tokio::test]
async fn not_evidence_retries_up_to_the_cap_then_the_slot_is_left_as_not_evidence() {
    let (launching, pack) = launching("captured_rows_count").await;
    let registry = CheckRegistry::builtin();

    let mut retrying = request(&launching, &pack, "run-1");
    one_slot(&mut retrying);
    retrying.max_infra_retries = 3;
    let executor = ScriptedExecutor::new()
        .with(
            key("case-a", 1),
            ScriptedExecutor::not_evidence("did:key:x"),
        )
        .with(
            key("case-a", 2),
            ScriptedExecutor::not_evidence("did:key:x"),
        )
        .with(key("case-a", 3), passed());
    let outcome = run(
        &launching.access,
        &retrying,
        &executor,
        &registry,
        CancellationToken::new(),
        &options(),
    )
    .await
    .unwrap();
    assert_eq!((outcome.completed, outcome.not_evidence), (3, 0));
    let trials = load_trials(&launching.access, OWNER, "run-1")
        .await
        .unwrap();
    assert_eq!(
        trial_keys(&trials),
        [
            ("base".to_string(), "case-a".to_string(), 0, 1, true),
            ("base".to_string(), "case-a".to_string(), 0, 2, true),
            ("base".to_string(), "case-a".to_string(), 0, 3, true),
        ],
        "an attempt that learned nothing still finished, so it is completed"
    );
    let empty = completion_of(&trials, 1);
    assert!(
        completion_is_not_evidence(&empty),
        "the attempt that never reached its stage is not evidence"
    );
    assert_eq!(empty.anchor.requests, 0);
    assert!(
        !completion_is_not_evidence(&completion_of(&trials, 3)),
        "the attempt that ran its stage is"
    );

    let mut exhausting = request(&launching, &pack, "run-2");
    one_slot(&mut exhausting);
    exhausting.max_infra_retries = 3;
    let executor =
        ScriptedExecutor::new().with_default(ScriptedExecutor::not_evidence("did:key:x"));
    let outcome = run(
        &launching.access,
        &exhausting,
        &executor,
        &registry,
        CancellationToken::new(),
        &options(),
    )
    .await
    .unwrap();
    assert_eq!((outcome.completed, outcome.not_evidence), (4, 1));
    let trials = load_trials(&launching.access, OWNER, "run-2")
        .await
        .unwrap();
    assert_eq!(
        trials.iter().map(|t| t.identity.attempt).max(),
        Some(4),
        "the slot stops at max_infra_retries + 1 attempts"
    );
    assert!(trials.iter().all(|trial| trial.completion.is_some()));
    assert!(
        completion_is_not_evidence(&completion_of(&trials, 4)),
        "the slot's latest completed attempt is still not evidence"
    );
    assert_eq!(not_evidence_slots(&trials), 1);
}

#[tokio::test]
async fn a_stage_that_failed_on_the_harness_re_plans_the_slot() {
    let (launching, pack) = launching("captured_rows_count").await;
    let mut request = request(&launching, &pack, "run-1");
    one_slot(&mut request);
    request.max_infra_retries = 1;

    let executor = ScriptedExecutor::new()
        .with(
            key("case-a", 1),
            ScriptedExecutor::failed_evidence(
                "did:key:trial",
                "check",
                OutcomeKind::Infrastructure,
                None,
            ),
        )
        .with(key("case-a", 2), passed());
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

    assert_eq!((outcome.completed, outcome.not_evidence), (2, 0));
    let trials = load_trials(&launching.access, OWNER, "run-1")
        .await
        .unwrap();
    assert_eq!(
        trial_keys(&trials),
        [
            ("base".to_string(), "case-a".to_string(), 0, 1, true),
            ("base".to_string(), "case-a".to_string(), 0, 2, true),
        ],
        "the first attempt learned nothing, so the slot owed another"
    );
    assert!(completion_is_not_evidence(&completion_of(&trials, 1)));

    let verdicts = load_verdicts(&launching.access, OWNER, "run-1")
        .await
        .unwrap();
    let first = trials
        .iter()
        .find(|trial| trial.identity.attempt == 1)
        .expect("the first attempt");
    let rows = verdicts
        .iter()
        .filter(|verdict| verdict.trial_id == first.identity.trial_id)
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(
        (rows[0].kind, rows[0].score_bp),
        (OutcomeKind::Infrastructure, None)
    );
}

#[tokio::test]
async fn the_breaker_trips_on_consecutive_not_evidence_and_resume_continues() {
    let (launching, pack) = launching("captured_rows_count").await;
    let registry = CheckRegistry::builtin();
    let mut request = request(&launching, &pack, "run-1");
    request.cells.truncate(1);
    request.trials_per_case = 1;
    request.max_infra_retries = 3;
    request.breaker_threshold = 2;

    let executor =
        ScriptedExecutor::new().with_default(ScriptedExecutor::not_evidence("did:key:x"));
    let error = run(
        &launching.access,
        &request,
        &executor,
        &registry,
        CancellationToken::new(),
        &options(),
    )
    .await
    .unwrap_err();
    let down = provider_down(&error).unwrap_or_else(|| panic!("expected ProviderDown: {error:#}"));
    assert_eq!(
        (down.outcome.run_id.as_str(), down.consecutive_not_evidence),
        ("run-1", 2)
    );
    assert!(down.outcome.breaker_tripped);
    let trials = load_trials(&launching.access, OWNER, "run-1")
        .await
        .unwrap();
    assert_eq!(trials.len(), 2);
    assert!(trials.iter().all(|trial| trial
        .completion
        .as_ref()
        .is_some_and(completion_is_not_evidence)));

    let recovered = ScriptedExecutor::new().with_default(passed());
    let outcome = resume(
        &launching.access,
        OWNER,
        "run-1",
        &launching.runs_dir(),
        &recovered,
        &registry,
        CancellationToken::new(),
        &options(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.completed, 2);
    let trials = load_trials(&launching.access, OWNER, "run-1")
        .await
        .unwrap();
    assert_eq!(
        trial_keys(&trials),
        [
            ("base".to_string(), "case-a".to_string(), 0, 1, true),
            ("base".to_string(), "case-a".to_string(), 0, 2, true),
            ("base".to_string(), "case-b".to_string(), 0, 1, true),
            ("base".to_string(), "case-b".to_string(), 0, 2, true),
        ]
    );
    assert_eq!(
        not_evidence_slots(&trials),
        0,
        "both slots learned something"
    );
}

#[tokio::test]
async fn resume_refuses_an_embedded_executor_for_a_pack_that_grants_host_bash() {
    let (launching, _) = launching("captured_rows_count").await;
    let pack = launching.pack("unrestricted", "Unrestricted");
    let mut request = request(&launching, &pack, "run-1");
    one_slot(&mut request);
    freeze(&launching.access, &request, Isolation::Process)
        .await
        .unwrap();

    let error = resume(
        &launching.access,
        OWNER,
        "run-1",
        &launching.runs_dir(),
        &ScriptedExecutor::new().with_default(passed()),
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &options(),
    )
    .await
    .unwrap_err();
    let reason =
        freeze_refused(&error).unwrap_or_else(|| panic!("expected a FreezeRefused, got {error:#}"));
    assert!(
        reason.0.contains("monitor-tools") && reason.0.contains("Unrestricted"),
        "{reason}"
    );
}

#[tokio::test]
async fn the_breaker_reports_the_count_it_tripped_on_not_the_one_it_ended_with() {
    let (launching, pack) = launching("captured_rows_count").await;
    let mut request = request(&launching, &pack, "run-1");
    request.cells.truncate(1);
    request.concurrency = 4;
    request.breaker_threshold = 2;
    let gate = CancellationToken::new();
    let recorder = GatingRecorder {
        inner: DocumentRecorder::new(&launching.access),
        completions: Mutex::new(0),
        open_after: 2,
        gate: gate.clone(),
    };

    let frozen = freeze(&launching.access, &request, Isolation::Embedded)
        .await
        .unwrap();
    let error = execute_frozen(
        &frozen,
        &recorder,
        &BreakerExecutor { gate },
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &options(),
    )
    .await
    .unwrap_err();

    let down =
        provider_down(&error).unwrap_or_else(|| panic!("expected a ProviderDown, got {error:#}"));
    assert_eq!(down.consecutive_not_evidence, 2);
    assert!(down.outcome.breaker_tripped);
    assert_eq!(
        down.outcome.completed, 4,
        "the trials already in flight at the trip are recorded like any other"
    );
    assert!(format!("{down}").contains("2 trials in a row"), "{down}");
    let trials = load_trials(&launching.access, OWNER, "run-1")
        .await
        .unwrap();
    assert_eq!(
        not_evidence_slots(&trials),
        2,
        "the two empty slots, once each"
    );
}

#[tokio::test]
async fn an_invalidated_run_refuses_resume() {
    let (launching, pack) = launching("captured_rows_count").await;
    let mut request = request(&launching, &pack, "run-1");
    one_slot(&mut request);
    freeze(&launching.access, &request, Isolation::Embedded)
        .await
        .unwrap();
    invalidate_run(&launching.access, OWNER, "run-1", OWNER, "grader bug")
        .await
        .unwrap();

    let error = resume(
        &launching.access,
        OWNER,
        "run-1",
        &launching.runs_dir(),
        &ScriptedExecutor::new().with_default(passed()),
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &options(),
    )
    .await
    .unwrap_err();
    let reason = format!("{error:#}");
    assert!(
        reason.contains("run-1") && reason.contains("invalidated"),
        "{reason}"
    );
}
