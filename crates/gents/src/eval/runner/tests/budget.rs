use super::*;

struct BudgetExecutor {
    started: tokio::sync::mpsc::UnboundedSender<(String, tokio::sync::oneshot::Sender<()>)>,
    active: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl TrialExecutor for BudgetExecutor {
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
    async fn execute(&self, spec: &TrialSpec, _cancel: CancellationToken) -> TrialEvidence {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        let (send, receive) = tokio::sync::oneshot::channel();
        self.started.send((spec.trial_id.clone(), send)).unwrap();
        receive.await.unwrap();
        self.active.fetch_sub(1, Ordering::SeqCst);
        passed()
    }
    async fn recollect(&self, _at: &TrialLocator, _captures: &[Capture]) -> Option<TrialEvidence> {
        None
    }
}

#[tokio::test]
async fn shared_trial_budget_reuses_capacity_across_runs_without_exceeding_ceiling() {
    let (launching, pack) = launching("captured_rows_count").await;
    let mut a = request(&launching, &pack, "budget-a");
    one_slot(&mut a);
    a.trials_per_case = 2;
    let mut b = a.clone();
    b.run_id = "budget-b".into();
    let a = prepare_run(&launching.access, &a, Isolation::Embedded)
        .await
        .unwrap();
    let b = prepare_run(&launching.access, &b, Isolation::Embedded)
        .await
        .unwrap();
    let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let executor = BudgetExecutor {
        started,
        active: 0.into(),
        peak: 0.into(),
    };
    let recorder = DocumentRecorder::new(&launching.access);
    let registry = CheckRegistry::builtin();
    let mut opts = options();
    opts.trial_budget = Some(TrialBudget::new(2).unwrap());
    let observe = async {
        let first = starts.recv().await.unwrap();
        let second = starts.recv().await.unwrap();
        assert_eq!(executor.active.load(Ordering::SeqCst), 2);
        first.1.send(()).unwrap();
        let third = starts.recv().await.unwrap();
        assert_eq!(
            executor.active.load(Ordering::SeqCst),
            2,
            "freed capacity is used while another run remains active"
        );
        third.1.send(()).unwrap();
        second.1.send(()).unwrap();
        starts.recv().await.unwrap().1.send(()).unwrap();
    };
    let (a_result, b_result, ()) = tokio::join!(
        execute_frozen(
            &a,
            &recorder,
            &executor,
            &registry,
            CancellationToken::new(),
            &opts
        ),
        execute_frozen(
            &b,
            &recorder,
            &executor,
            &registry,
            CancellationToken::new(),
            &opts
        ),
        observe,
    );
    assert_eq!(a_result.unwrap().completed, 2);
    assert_eq!(b_result.unwrap().completed, 2);
    assert_eq!(executor.peak.load(Ordering::SeqCst), 2);
    assert_eq!(opts.trial_budget.as_ref().unwrap().0.available_permits(), 2);
}

#[tokio::test]
async fn cancelled_budget_wait_creates_no_attempt_and_does_not_leak_capacity() {
    assert!(TrialBudget::new(0).is_err());
    let (launching, pack) = launching("captured_rows_count").await;
    let mut request = request(&launching, &pack, "budget-cancelled");
    one_slot(&mut request);
    let frozen = prepare_run(&launching.access, &request, Isolation::Embedded)
        .await
        .unwrap();
    let budget = TrialBudget::new(1).unwrap();
    let held = budget.0.acquire().await.unwrap();
    let cancel = CancellationToken::new();
    let executor = ScriptedExecutor::new().with_default(passed());
    let recorder = DocumentRecorder::new(&launching.access);
    let registry = CheckRegistry::builtin();
    let progress = ProgressWriter::new(&frozen.run_dir);
    let views = ViewWriter::default();
    let stop = AtomicBool::new(false);
    let admission_stopped = CancellationToken::new();
    let planned = plan(
        &frozen.record.origin,
        &request.run_id,
        &[],
        request.max_infra_retries,
    );
    let trial = execute_trial(
        &frozen,
        &planned[0],
        &recorder,
        &executor,
        &registry,
        &cancel,
        &stop,
        &progress,
        &views,
        Some(&budget),
        &admission_stopped,
    );
    tokio::pin!(trial);
    assert!(futures::poll!(&mut trial).is_pending());
    cancel.cancel();
    assert!(matches!(trial.await.unwrap(), Slot::Skipped));
    assert!(recorder
        .load_trials(OWNER, &request.run_id)
        .await
        .unwrap()
        .is_empty());
    drop(held);
    assert_eq!(budget.0.available_permits(), 1);
    let mut opts = options();
    opts.trial_budget = Some(budget.clone());
    let outcome = execute_frozen(
        &frozen,
        &recorder,
        &executor,
        &registry,
        CancellationToken::new(),
        &opts,
    )
    .await
    .unwrap();
    assert_eq!(outcome.completed, 1);
    assert_eq!(outcome.abandoned, 0);
    assert_eq!(budget.0.available_permits(), 1);
}

#[tokio::test]
async fn stopped_budget_wait_finishes_without_waiting_for_unrelated_capacity() {
    assert!(TrialBudget::new(0).is_err());
    let (launching, pack) = launching("captured_rows_count").await;
    let mut request = request(&launching, &pack, "budget-cancelled");
    one_slot(&mut request);
    let frozen = prepare_run(&launching.access, &request, Isolation::Embedded)
        .await
        .unwrap();
    let budget = TrialBudget::new(1).unwrap();
    let held = budget.0.acquire().await.unwrap();
    let cancel = CancellationToken::new();
    let executor = ScriptedExecutor::new().with_default(passed());
    let recorder = DocumentRecorder::new(&launching.access);
    let registry = CheckRegistry::builtin();
    let progress = ProgressWriter::new(&frozen.run_dir);
    let views = ViewWriter::default();
    let stop = AtomicBool::new(false);
    let admission_stopped = CancellationToken::new();
    let planned = plan(
        &frozen.record.origin,
        &request.run_id,
        &[],
        request.max_infra_retries,
    );
    let trial = execute_trial(
        &frozen,
        &planned[0],
        &recorder,
        &executor,
        &registry,
        &cancel,
        &stop,
        &progress,
        &views,
        Some(&budget),
        &admission_stopped,
    );
    tokio::pin!(trial);
    assert!(futures::poll!(&mut trial).is_pending());
    stop.store(true, Ordering::Relaxed);
    admission_stopped.cancel();
    assert!(matches!(
        futures::poll!(&mut trial),
        std::task::Poll::Ready(Ok(Slot::Skipped))
    ));
    assert!(recorder
        .load_trials(OWNER, &request.run_id)
        .await
        .unwrap()
        .is_empty());
    drop(held);
    assert_eq!(budget.0.available_permits(), 1);
    let mut opts = options();
    opts.trial_budget = Some(budget.clone());
    let outcome = execute_frozen(
        &frozen,
        &recorder,
        &executor,
        &registry,
        CancellationToken::new(),
        &opts,
    )
    .await
    .unwrap();
    assert_eq!(outcome.completed, 1);
    assert_eq!(outcome.abandoned, 0);
    assert_eq!(budget.0.available_permits(), 1);
}
