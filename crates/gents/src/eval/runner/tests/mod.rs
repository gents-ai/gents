use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::anyhow;
use serde_json::{json, Value};

use super::*;
use crate::document_config::EvalFixtureDocument;
use crate::document_config::EvalFixtureFile;
use crate::document_config::{EvalCase, EvalFixtures, EvalSplit};
use crate::eval::checks::{Check, CheckDescription, CheckVerdict};
use crate::eval::runner::freeze::tests::{Launching, OWNER};
use crate::eval::{
    invalidate_run, load_run, load_trials, load_verdicts, OutcomeKind, TrialCompletion,
    TrialIdentity, TrialRecord, VerdictDraft, VerdictRecord,
};
use crate::Collection;

mod budget;
mod cancel;
mod liveness;
mod progress;
mod recording;
mod retry;
mod spec;

fn definition(check: &str) -> Value {
    json!({
        "definition_id": "loop-def",
        "node_did": OWNER,
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
        .install(vec![(Collection::EvalDefinition, definition(check))])
        .await;
    let pack = launching.pack("pack", "Off");
    (launching, pack)
}

fn request(launching: &Launching, pack: &Path, run_id: &str) -> RunRequest {
    RunRequest {
        run_id: run_id.into(),
        owner: OWNER.into(),
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
        agent_id: "monitor".into(),
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

struct BackoffBegan(Arc<tokio::sync::Notify>);

impl<S> tracing_subscriber::Layer<S> for BackoffBegan
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if event.metadata().target() == module_path!().trim_end_matches("::tests")
            && event.metadata().fields().field("backoff").is_some()
        {
            self.0.notify_one();
        }
    }
}

#[tokio::test]
async fn a_marker_written_during_the_backoff_ends_the_wait() {
    use tracing_subscriber::layer::SubscriberExt;

    let began = Arc::new(tokio::sync::Notify::new());
    let subscriber = tracing::Dispatch::new(
        tracing_subscriber::Registry::default().with(BackoffBegan(began.clone())),
    );
    let _subscriber = tracing::dispatcher::set_default(&subscriber);

    let (launching, pack) = launching("captured_rows_count").await;
    let mut request = request(&launching, &pack, "run-backoff");
    one_slot(&mut request);
    let slow = RunOptions {
        trial_budget: None,
        poll_backoff_base: Duration::from_secs(30),
        poll_backoff_cap: Duration::from_secs(30),
    };
    let runs_dir = launching.runs_dir();
    let run_dir = runs_dir.join("run-backoff");
    let held = || {
        read_progress(&run_dir)
            .and_then(|progress| progress.holder)
            .map(|holder| holder.written_at)
    };
    let operator = async {
        tokio::time::timeout(Duration::from_secs(20), began.notified())
            .await
            .expect("the loop began its backoff after the not-evidence attempt");
        let first = held().expect("the loop holds the run during its backoff");
        assert!(running_elsewhere(&run_dir));
        tokio::time::sleep(Duration::from_millis(2_500)).await;
        let second = held().expect("still held");
        assert!(
            second > first,
            "refreshed during the backoff: {first} {second}"
        );
        request_cancel(&runs_dir, "run-backoff").unwrap();
        std::time::Instant::now()
    };
    let executor =
        ScriptedExecutor::new().with_default(ScriptedExecutor::not_evidence("did:key:trial"));
    let registry = CheckRegistry::builtin();
    let (outcome, marked) = tokio::join!(
        run(
            &launching.access,
            &request,
            &executor,
            &registry,
            CancellationToken::new(),
            &slow,
        ),
        operator,
    );
    let outcome = outcome.unwrap();
    assert!(
        marked.elapsed() < Duration::from_secs(5),
        "the 30 s backoff was cut short at the next tick: {:?}",
        marked.elapsed()
    );
    assert!(outcome.cancelled);
    assert_eq!((outcome.completed, outcome.abandoned), (1, 0));
    assert_eq!(
        load_trials(&launching.access, OWNER, "run-backoff")
            .await
            .unwrap()
            .len(),
        1,
        "the retry the backoff waited for never launched"
    );
    assert_eq!(held(), None, "a cancelled loop releases the run");
    assert!(!running_elsewhere(&run_dir));
}
