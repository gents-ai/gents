use super::*;
use crate::test_support::doubles::{
    CancelOnFirstExecute, MarkAfterComplete, MarkOnFirstExecute, MarksThenWaits,
};

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

    let trials = load_trials(&launching.access, launching.owner(), "run-1")
        .await
        .unwrap();
    assert_eq!(trials.len(), 1, "the second slot is never launched");
    assert!(trials[0].completion.is_none());
    assert!(load_verdicts(&launching.access, launching.owner(), "run-1")
        .await
        .unwrap()
        .is_empty());
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
        load_trials(&launching.access, launching.owner(), "run-marker")
            .await
            .unwrap()
            .len(),
        1
    );

    let resumed = resume(
        &launching.access,
        RunLocation {
            owner: launching.owner(),
            run_id: "run-marker",
            runs_dir: &launching.runs_dir(),
        },
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
        load_trials(&launching.access, launching.owner(), "run-marker")
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
    assert!(
        load_trials(&launching.access, launching.owner(), "run-early")
            .await
            .unwrap()
            .is_empty()
    );
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
        RunLocation {
            owner: launching.owner(),
            run_id: "run-abandoned",
            runs_dir: &launching.runs_dir(),
        },
        &ScriptedExecutor::new().with_default(passed()),
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &options(),
    )
    .await
    .unwrap();
    assert_eq!(resumed.completed, 1, "the abandoned attempt spent no retry");
    let mut attempts: Vec<(u32, bool)> =
        load_trials(&launching.access, launching.owner(), "run-abandoned")
            .await
            .unwrap()
            .iter()
            .map(|trial| (trial.identity.attempt, trial.completion.is_some()))
            .collect();
    attempts.sort();
    assert_eq!(attempts, vec![(1, false), (2, true)]);
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
        "the trial saw its token cancelled"
    );
    assert_eq!((outcome.completed, outcome.abandoned), (0, 1));
    let trials = load_trials(&launching.access, launching.owner(), "run-mid")
        .await
        .unwrap();
    assert_eq!(trials.len(), 1);
    assert!(trials[0].completion.is_none(), "left open for a resume");
}
