use super::*;

struct StageReporting {
    run_dir: PathBuf,
    seen: Mutex<Vec<Option<Progress>>>,
}

#[async_trait::async_trait]
impl TrialExecutor for StageReporting {
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
        spec.progress.stage_started("check");
        let during = read_progress(&self.run_dir);
        spec.progress.stage_ended("check");
        self.seen.lock().unwrap().push(during);
        passed()
    }

    async fn recollect(&self, _at: &TrialLocator, _captures: &[Capture]) -> Option<TrialEvidence> {
        None
    }
}

#[tokio::test]
async fn progress_names_the_stage_an_in_flight_slot_is_in_and_forgets_it_at_the_end() {
    let (launching, pack) = launching("captured_rows_count").await;
    let mut request = request(&launching, &pack, "run-progress");
    one_slot(&mut request);
    let run_dir = launching.runs_dir().join("run-progress");
    let executor = StageReporting {
        run_dir: run_dir.clone(),
        seen: Mutex::new(Vec::new()),
    };
    run(
        &launching.access,
        &request,
        &executor,
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &options(),
    )
    .await
    .unwrap();

    let seen = executor.seen.lock().unwrap().clone();
    let during = seen[0]
        .as_ref()
        .expect("progress.json exists while the trial runs");
    let (trial_id, slot) = during.slots.iter().next().expect("one slot in flight");
    assert_eq!(
        trial_id,
        &trial_id_for("run-progress", "base", "case-a", 0, 1)
    );
    assert_eq!(
        (
            slot.cell_id.as_str(),
            slot.case_id.as_str(),
            slot.trial_index,
            slot.attempt,
            slot.stage_id.as_deref()
        ),
        ("base", "case-a", 0, 1, Some("check"))
    );
    let holder = during.holder.as_ref().expect("the loop holds the run");
    assert_eq!(holder.pid, std::process::id());
    let after = read_progress(&run_dir).expect("the file stays after the run");
    assert!(after.slots.is_empty(), "{after:?}");
    assert_eq!(after.holder, None, "the loop released the run: {after:?}");
    assert!(!running_elsewhere(&run_dir));
}

struct ReportsLive {
    run_dir: PathBuf,
    seen: Mutex<Option<InFlight>>,
}

fn snapshot(requests: u64) -> LiveSnapshot {
    LiveSnapshot {
        stages: Default::default(),
        observed_at: "2026-09-29T00:00:00Z".into(),
        elapsed_secs: requests * 10,
        requests,
        model_turns: 4 * requests,
        input_tokens: Some(1_200 * requests),
        output_tokens: Some(300 * requests),
        reported_input_tokens: None,
        reported_output_tokens: None,
        session_contexts: None,
        tool_calls: 3,
        failed_tool_calls: 1,
        tools: std::collections::BTreeMap::from([(
            "config".to_owned(),
            ToolTally {
                calls: 3,
                failed: 1,
            },
        )]),
        documents: std::collections::BTreeMap::from([("Finding".to_owned(), requests)]),
        captures: std::collections::BTreeMap::from([("findings".to_owned(), requests)]),
        schemas: vec!["Finding".into()],
        last_tool: None,
    }
}

#[async_trait::async_trait]
impl TrialExecutor for ReportsLive {
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
        assert!(spec.progress.attached(), "the loop watches every trial");
        spec.progress.live(snapshot(1));
        *self.seen.lock().unwrap() = read_progress(&self.run_dir)
            .and_then(|progress| progress.slots.values().next().cloned());
        spec.progress.live(snapshot(2));
        passed()
    }

    async fn recollect(&self, _at: &TrialLocator, _captures: &[Capture]) -> Option<TrialEvidence> {
        None
    }
}

#[tokio::test]
async fn a_slots_live_snapshot_is_in_progress_and_its_last_is_kept_with_its_evidence() {
    let (launching, pack) = launching("captured_rows_count").await;
    let mut request = request(&launching, &pack, "run-live");
    one_slot(&mut request);
    request.captures = vec![Capture::Documents {
        name: "findings".into(),
        collection: "Finding".into(),
        filter: json!({}),
        fields: Vec::new(),
    }];
    let run_dir = launching.runs_dir().join("run-live");
    let executor = ReportsLive {
        run_dir: run_dir.clone(),
        seen: Mutex::new(None),
    };
    run(
        &launching.access,
        &request,
        &executor,
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &options(),
    )
    .await
    .unwrap();

    let goal = vec![GoalEntry {
        collection: "Finding".into(),
        capture: Some("findings".into()),
        min: 1,
        max: None,
    }];
    let in_flight = executor.seen.lock().unwrap().clone().expect("in flight");
    assert_eq!(in_flight.live, Some(snapshot(1)));
    assert_eq!(
        in_flight.goal, goal,
        "derived from the case's row-count check"
    );

    let trials = load_trials(&launching.access, OWNER, "run-live")
        .await
        .unwrap();
    let trial_id = &trials[0].identity.trial_id;
    let record =
        read_evidence_record(&run_dir.join("trials").join(trial_id)).expect("an evidence record");
    assert_eq!(record.live, Some(snapshot(2)), "the last snapshot reported");
    assert_eq!(record.goal, goal);
    assert!(record.ended_at.is_some());

    let view = read_run_view(&run_dir).expect("report.json");
    assert_eq!(view.pid, std::process::id());
    assert_eq!(view.trials.get(trial_id), Some(&record));
    let slot = &view.report.cells[0].slots[0];
    assert_eq!(slot.class, crate::eval::report::SlotClass::Pass);
    assert_eq!(
        slot.verdicts[0].detail.as_deref(),
        Some("findings observed 1 expected ≥1")
    );
    assert_eq!(
        view.report,
        crate::eval::report::load_report(
            &launching.access,
            OWNER,
            &launching.runs_dir(),
            "run-live"
        )
        .await
        .unwrap(),
        "the file is the report the node derives"
    );
}
