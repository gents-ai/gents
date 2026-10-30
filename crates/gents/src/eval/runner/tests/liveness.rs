use super::*;

#[tokio::test]
async fn run_and_resume_refuse_a_run_held_by_a_live_process() {
    let (launching, pack) = launching("captured_rows_count").await;
    let mut request = request(&launching, &pack, "run-held");
    one_slot(&mut request);
    let frozen = freeze(&launching.access, &request, Isolation::Embedded)
        .await
        .unwrap();
    let holder = ProgressWriter::new(&frozen.run_dir);
    let _held = holder.hold();
    let marker = request_cancel(&launching.runs_dir(), "run-held").unwrap();
    let definition = frozen.run_dir.join(freeze::snapshot::DEFINITION_FILE);
    let long_ago = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    std::fs::File::options()
        .write(true)
        .open(&definition)
        .unwrap()
        .set_modified(long_ago)
        .unwrap();

    holder.heartbeat(Duration::ZERO);
    let ran = run(
        &launching.access,
        &request,
        &ScriptedExecutor::new().with_default(passed()),
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &options(),
    )
    .await
    .unwrap_err();
    holder.heartbeat(Duration::ZERO);
    let resumed = resume(
        &launching.access,
        OWNER,
        "run-held",
        &launching.runs_dir(),
        &ScriptedExecutor::new().with_default(passed()),
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &options(),
    )
    .await
    .unwrap_err();
    for error in [ran, resumed] {
        assert!(
            freeze_refused(&error).is_some_and(|refusal| refusal.0.contains("still running")),
            "{error:#}"
        );
    }
    holder.heartbeat(Duration::ZERO);
    assert!(marker.exists(), "the holder's cancel request survives");
    assert_eq!(
        std::fs::metadata(&definition).unwrap().modified().unwrap(),
        long_ago,
        "run refuses before freezing rewrites the run's files"
    );
    assert!(running_elsewhere(&frozen.run_dir));
    assert!(load_trials(&launching.access, OWNER, "run-held")
        .await
        .unwrap()
        .is_empty());
}

#[test]
fn request_cancel_refuses_a_missing_run_directory_and_a_path_for_an_id() {
    let dir = tempfile::tempdir().unwrap();
    let missing = request_cancel(dir.path(), "absent").unwrap_err();
    assert!(
        freeze_refused(&missing).is_some_and(|refusal| refusal.0.contains("has no directory")),
        "{missing:#}"
    );
    let escaping = request_cancel(dir.path(), "../elsewhere").unwrap_err();
    assert!(
        freeze_refused(&escaping)
            .is_some_and(|refusal| refusal.0.contains("one ordinary path component")),
        "{escaping:#}"
    );
    assert_eq!(
        run_dir(dir.path(), "run-1").unwrap(),
        dir.path().join("run-1")
    );
}

struct DrainsSlowly {
    runs_dir: PathBuf,
    run_id: String,
    seen: Mutex<Vec<Option<String>>>,
}

#[async_trait::async_trait]
impl TrialExecutor for DrainsSlowly {
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
        tokio::time::timeout(Duration::from_secs(10), cancel.cancelled())
            .await
            .expect("the loop saw the marker");
        let holder = || {
            read_progress(&self.runs_dir.join(&self.run_id))
                .and_then(|progress| progress.holder)
                .map(|holder| holder.written_at)
        };
        let first = holder();
        tokio::time::sleep(Duration::from_millis(1_000)).await;
        let second = holder();
        self.seen.lock().unwrap().extend([first, second]);
        passed()
    }

    async fn recollect(&self, _at: &TrialLocator, _captures: &[Capture]) -> Option<TrialEvidence> {
        None
    }
}

#[tokio::test]
async fn the_holder_stays_fresh_while_a_cancelled_pass_drains() {
    let (launching, pack) = launching("captured_rows_count").await;
    let mut request = request(&launching, &pack, "run-drain");
    one_slot(&mut request);
    let executor = DrainsSlowly {
        runs_dir: launching.runs_dir(),
        run_id: "run-drain".into(),
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
    let (Some(first), Some(second)) = (&seen[0], &seen[1]) else {
        panic!("the loop held the run while it drained: {seen:?}");
    };
    assert!(
        second > first,
        "the 250 ms heartbeat refreshed the holder during the drain: {seen:?}"
    );
    let after = read_progress(&launching.runs_dir().join("run-drain")).unwrap();
    assert_eq!(after.holder, None);
}

struct WatchesHeartbeat {
    run_dir: PathBuf,
    seen: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl TrialExecutor for WatchesHeartbeat {
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

    async fn execute(&self, _spec: &TrialSpec, _cancel: CancellationToken) -> TrialEvidence {
        let written_at = || {
            read_progress(&self.run_dir)
                .and_then(|progress| progress.slots.values().next().cloned())
                .map(|slot| slot.written_at)
                .unwrap_or_default()
        };
        let first = written_at();
        tokio::time::sleep(Duration::from_millis(600)).await;
        let second = written_at();
        self.seen.lock().unwrap().extend([first, second]);
        passed()
    }

    async fn recollect(&self, _at: &TrialLocator, _captures: &[Capture]) -> Option<TrialEvidence> {
        None
    }
}

#[tokio::test]
async fn the_loop_refreshes_in_flight_entries_while_a_trial_runs() {
    let (launching, pack) = launching("captured_rows_count").await;
    let mut request = request(&launching, &pack, "run-beat");
    one_slot(&mut request);
    let executor = WatchesHeartbeat {
        run_dir: launching.runs_dir().join("run-beat"),
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
    assert!(!seen[0].is_empty(), "{seen:?}");
    assert!(
        seen[1] > seen[0],
        "written_at advanced while the trial ran: {seen:?}"
    );
    assert_eq!(marker_poll(&options()), Duration::from_millis(1));
    assert_eq!(marker_poll(&RunOptions::default()), Duration::from_secs(1));
}

#[tokio::test]
async fn a_run_is_finished_when_it_owes_nothing_and_no_live_process_holds_a_slot() {
    let (launching, pack) = launching("captured_rows_count").await;
    let mut request = request(&launching, &pack, "run-fin");
    one_slot(&mut request);
    let frozen = freeze(&launching.access, &request, Isolation::Embedded)
        .await
        .unwrap();
    let record = load_run(&launching.access, OWNER, "run-fin")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(slots_owed(&record, &[]), 1);
    assert!(!run_finished(&record, &[], &frozen.run_dir));

    run(
        &launching.access,
        &request,
        &ScriptedExecutor::new().with_default(passed()),
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &options(),
    )
    .await
    .unwrap();
    let trials = load_trials(&launching.access, OWNER, "run-fin")
        .await
        .unwrap();
    assert_eq!(slots_owed(&record, &trials), 0);
    assert!(run_finished(&record, &trials, &frozen.run_dir));

    let writer = ProgressWriter::new(&frozen.run_dir);
    writer.slot_started(
        "held",
        InFlight {
            cell_id: "base".into(),
            case_id: "case-a".into(),
            trial_index: 0,
            attempt: 2,
            stage_id: None,
            started_at: String::new(),
            pid: 0,
            written_at: String::new(),
            live: None,
            goal: Vec::new(),
        },
    );
    assert!(running_elsewhere(&frozen.run_dir));
    assert!(!run_finished(&record, &trials, &frozen.run_dir));

    let rewrite = |change: &dyn Fn(&mut InFlight)| {
        let mut progress = read_progress(&frozen.run_dir).unwrap();
        change(progress.slots.get_mut("held").unwrap());
        std::fs::write(
            frozen.run_dir.join(PROGRESS_FILE),
            serde_json::to_vec(&progress).unwrap(),
        )
        .unwrap();
    };
    rewrite(&|slot| slot.written_at = "2026-01-01T00:00:00.000Z".into());
    assert!(!running_elsewhere(&frozen.run_dir));

    rewrite(&|slot| {
        slot.written_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        slot.pid = u32::MAX;
    });
    assert!(!running_elsewhere(&frozen.run_dir));
    assert!(run_finished(&record, &trials, &frozen.run_dir));

    let idle = ProgressWriter::new(&frozen.run_dir);
    let held = idle.hold();
    assert!(running_elsewhere(&frozen.run_dir));
    assert!(!run_finished(&record, &trials, &frozen.run_dir));
    let mut progress = read_progress(&frozen.run_dir).unwrap();
    progress.holder.as_mut().unwrap().written_at = "2026-01-01T00:00:00.000Z".into();
    std::fs::write(
        frozen.run_dir.join(PROGRESS_FILE),
        serde_json::to_vec(&progress).unwrap(),
    )
    .unwrap();
    assert!(!running_elsewhere(&frozen.run_dir));
    drop(held);
    assert!(!running_elsewhere(&frozen.run_dir));

    assert_eq!(STALE_WINDOW, marker_poll(&RunOptions::default()) * 3);
    assert!(
        [0, 1, 250, 999, 1_000, 5_000, u64::MAX].iter().all(|ms| {
            let options = RunOptions {
                trial_budget: None,
                poll_backoff_base: Duration::from_millis(*ms),
                poll_backoff_cap: Duration::from_secs(60),
            };
            marker_poll(&options) * 3 <= STALE_WINDOW
        }),
        "one window covers a runner started with any options"
    );
}
