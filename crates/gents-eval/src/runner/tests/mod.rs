use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::anyhow;
use serde_json::{json, Value};

use super::*;
use crate::checks::{Check, CheckDescription, CheckVerdict};
use crate::runner::freeze::tests::Launching;
use crate::{
    invalidate_run, load_run, load_trials, load_verdicts, OutcomeKind, TrialCompletion,
    TrialIdentity, TrialRecord, VerdictDraft, VerdictRecord,
};
use gents_documents::EvalFixtureDocument;
use gents_documents::EvalFixtureFile;
use gents_documents::{EvalCase, EvalFixtures, EvalSplit};
use gents_store::collection::Collection;
use gents_store::test_support::{STALL_CEILING, UNREACHED_DEADLINE};

mod budget;
mod cancel;
mod liveness;
mod progress;
mod recording;
mod retry;
mod spec;

fn definition(owner: &str, check: &str) -> Value {
    json!({
        "definition_id": "loop-def",
        "node_did": owner,
        "comparability_version": 1,
        "subject": {"kind": "agent", "inference_slots": ["primary"]},
        "cases": [
            case("case-a", "validation", check),
            case("case-b", "validation", check),
            case("train-case", "train", check),
        ],
    })
}

fn case(case_id: &str, split: &str, check: &str) -> Value {
    json!({
        "case_id": case_id,
        "split": split,
        "stages": [{
            "stage_id": "check",
            "prompt": "Run the monitor.",
            "deadline_secs": 600,
            "checks": [{
                "check": check,
                "params": {"name": "findings", "min": 1},
                "tier": "acceptance",
            }],
        }],
    })
}

async fn launching(check: &str) -> (Launching, PathBuf) {
    let launching = Launching::new().await;
    launching
        .install(vec![(
            Collection::EvalDefinition,
            definition(launching.owner(), check),
        )])
        .await;
    let pack = launching.pack("pack", "Off");
    (launching, pack)
}

fn request(launching: &Launching, pack: &Path, run_id: &str) -> RunRequest {
    RunRequest {
        run_id: run_id.into(),
        owner: launching.owner().into(),
        evaluator_did: launching.evaluator_did(),
        definition_id: "loop-def".into(),
        split: EvalSplit::Validation,
        case_ids: None,
        cells: vec![cell("base", pack), cell("cand", pack)],
        trials_per_case: 2,
        seed_base: 1000,
        deadline_secs: Some(600),
        concurrency: 1,
        max_infra_retries: 1,
        breaker_threshold: 5,
        purpose: "eval".into(),
        source_commit: "0deb7659c".into(),
        source_dirty: false,
        runs_dir: launching.runs_dir(),
        divergences: None,
        captures: Vec::new(),
    }
}

fn one_slot(request: &mut RunRequest) {
    request.cells.truncate(1);
    request.case_ids = Some(vec!["case-a".into()]);
    request.trials_per_case = 1;
}

fn cell(cell_id: &str, pack: &Path) -> CellRequest {
    CellRequest {
        cell_id: cell_id.into(),
        label: cell_id.into(),
        source: CellSource::Directory(pack.to_path_buf()),
        agent_slot: "monitor".into(),
        inference_profile_id: "local".into(),
    }
}

fn options() -> RunOptions {
    RunOptions {
        trial_budget: None,
        poll_backoff_base: Duration::from_millis(1),
        poll_backoff_cap: Duration::from_millis(2),
    }
}

fn passed() -> TrialEvidence {
    ScriptedExecutor::passed_evidence("did:key:trial", "check", "findings", vec![json!({})])
}

fn key(case_id: &str, attempt: u32) -> ScriptKey {
    ScriptKey {
        cell_label: "base".into(),
        case_id: case_id.into(),
        trial_index: 0,
        attempt,
    }
}

fn completion_of(trials: &[TrialRecord], attempt: u32) -> TrialCompletion {
    trials
        .iter()
        .find(|trial| trial.identity.attempt == attempt)
        .and_then(|trial| trial.completion.clone())
        .unwrap_or_else(|| panic!("a completion for attempt {attempt}"))
}

fn trial_keys(trials: &[TrialRecord]) -> Vec<(String, String, u32, u32, bool)> {
    let mut keys: Vec<_> = trials
        .iter()
        .map(|trial| {
            (
                trial.identity.cell_id.clone(),
                trial.identity.case_id.clone(),
                trial.identity.trial_index,
                trial.identity.attempt,
                trial.completion.is_some(),
            )
        })
        .collect();
    keys.sort();
    keys
}

fn verdict_keys(
    verdicts: &[VerdictRecord],
) -> Vec<(String, String, String, &'static str, Option<u32>)> {
    let mut keys: Vec<_> = verdicts
        .iter()
        .map(|verdict| {
            (
                verdict.trial_id.clone(),
                verdict.stage_id.clone(),
                verdict.check.clone(),
                verdict.kind.as_str(),
                verdict.score_bp,
            )
        })
        .collect();
    keys.sort();
    keys
}

/// Re-reads the run's progress file at the loop's marker poll until `found`
/// answers, and fails with the last progress read once the file has not
/// changed for [`STALL_CEILING`]. The file has no change notification.
async fn progress_when<T>(run_dir: &Path, what: &str, found: impl Fn(&Progress) -> Option<T>) -> T {
    let mut reread = tokio::time::interval(marker_poll(&RunOptions::default()));
    let mut last = None;
    let mut changed = std::time::Instant::now();
    loop {
        reread.tick().await;
        let progress = read_progress(run_dir);
        if let Some(answer) = progress.as_ref().and_then(&found) {
            return answer;
        }
        if progress != last {
            last = progress;
            changed = std::time::Instant::now();
        } else if changed.elapsed() >= STALL_CEILING {
            panic!("{what}: the progress stayed {last:?} for {STALL_CEILING:?}");
        }
    }
}

const RUNNER_TARGET: &str = "gents_eval::runner";

fn runner_target(target: &str) -> bool {
    target.starts_with(RUNNER_TARGET) || target == "gents::eval"
}

/// Makes the run loop's events visible to a scoped subscriber. tracing caches
/// one interest per callsite for the whole process, and while a single
/// dispatcher is registered it computes that interest from the default of
/// whichever thread reaches the callsite first; another test's thread with no
/// default caches `never`, which then hides the event from this test's
/// subscriber. A global default that enables the target is every such
/// thread's default (TLA+ `EvalBackoffCancel`).
fn enable_scoped_event_capture() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        tracing::subscriber::set_global_default(RunnerTargetSubscriber)
            .expect("no other global tracing default in this test binary");
    });
}

struct RunnerTargetSubscriber;

impl tracing::Subscriber for RunnerTargetSubscriber {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        runner_target(metadata.target())
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::Id {
        tracing::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::Id, _follows: &tracing::Id) {}

    fn event(&self, _event: &tracing::Event<'_>) {}

    fn enter(&self, _span: &tracing::Id) {}

    fn exit(&self, _span: &tracing::Id) {}
}

#[derive(Clone, Debug, Default)]
struct RunnerEvents {
    last: Option<String>,
    backoff_began: bool,
}

struct ObservesRunner(tokio::sync::watch::Sender<RunnerEvents>);

impl<S> tracing_subscriber::Layer<S> for ObservesRunner
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        let metadata = event.metadata();
        if !runner_target(metadata.target()) {
            return;
        }
        let mut message = Message(String::new());
        event.record(&mut message);
        let backoff =
            metadata.target() == RUNNER_TARGET && metadata.fields().field("backoff").is_some();
        self.0.send_modify(|events| {
            events.last = Some(format!("{}: {}", metadata.target(), message.0));
            events.backoff_began |= backoff;
        });
    }
}

/// Waits until `done` holds for the run's events, failing with the last one
/// once none has arrived for [`STALL_CEILING`].
async fn runner_when(
    events: &mut tokio::sync::watch::Receiver<RunnerEvents>,
    what: &str,
    done: impl Fn(&RunnerEvents) -> bool,
) {
    loop {
        if done(&events.borrow_and_update()) {
            return;
        }
        match tokio::time::timeout(STALL_CEILING, events.changed()).await {
            Ok(changed) => changed.expect("the test's subscriber outlives the run"),
            Err(_) => panic!(
                "{what}: no runner event for {STALL_CEILING:?}; the last was {:?}",
                events.borrow().last
            ),
        }
    }
}

#[tokio::test]
async fn a_marker_written_during_the_backoff_ends_the_wait() {
    use tracing_subscriber::layer::SubscriberExt;

    enable_scoped_event_capture();
    let (observed, mut events) = tokio::sync::watch::channel(RunnerEvents::default());
    let subscriber = tracing::Dispatch::new(
        tracing_subscriber::Registry::default().with(ObservesRunner(observed)),
    );
    let _subscriber = tracing::dispatcher::set_default(&subscriber);

    let (launching, pack) = launching("captured_rows_count").await;
    let mut request = request(&launching, &pack, "run-backoff");
    one_slot(&mut request);
    let unreached = RunOptions {
        trial_budget: None,
        poll_backoff_base: UNREACHED_DEADLINE,
        poll_backoff_cap: UNREACHED_DEADLINE,
    };
    let runs_dir = launching.runs_dir();
    let run_dir = runs_dir.join("run-backoff");
    let held = || read_progress(&run_dir).and_then(|progress| progress.holder);
    let operator = async {
        runner_when(
            &mut events,
            "the loop begins its backoff after the not-evidence attempt",
            |events| events.backoff_began,
        )
        .await;
        let first = held().expect("the loop holds the run during its backoff");
        assert_eq!(first.pid, std::process::id());
        progress_when(
            &run_dir,
            "the loop refreshes its hold during the backoff",
            |progress| {
                progress
                    .holder
                    .as_ref()
                    .filter(|holder| holder.written_at > first.written_at)
                    .map(|_| ())
            },
        )
        .await;
        request_cancel(&runs_dir, "run-backoff").unwrap();
        runner_when(&mut events, "the marker ends the backoff", |_| false).await;
    };
    let executor =
        ScriptedExecutor::new().with_default(ScriptedExecutor::not_evidence("did:key:trial"));
    let registry = CheckRegistry::builtin();
    let outcome = tokio::select! {
        outcome = run(
            &launching.access,
            &request,
            &executor,
            &registry,
            CancellationToken::new(),
            &unreached,
        ) => outcome.unwrap(),
        () = operator => unreachable!("the operator returns only by failing"),
    };
    assert!(outcome.cancelled);
    assert_eq!((outcome.completed, outcome.abandoned), (1, 0));
    assert_eq!(
        load_trials(&launching.access, launching.owner(), "run-backoff")
            .await
            .unwrap()
            .len(),
        1,
        "the retry the backoff waited for never launched"
    );
    assert_eq!(held(), None, "a cancelled loop releases the run");
    assert!(!running_elsewhere(&run_dir));
}
