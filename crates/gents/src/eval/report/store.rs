//! Gathering a run's documents for a report: the module's one I/O boundary
//! (loading rows for an owner, and the run's frozen definition), alongside
//! `evidence::load_run_rows`. `build`, `compare` and the breakdowns are pure.
//!
//! `eval::documents` is frozen and has no list read, so listing asks
//! `EvalRun` for the owner's run ids and reads each through `load_run`,
//! keeping the row decoding in one place.

use std::path::Path;

use anyhow::Result;

use crate::config_client::ConfigAccess;
use crate::eval::report::build::{build, EvalReport};
use crate::eval::report::refused;
use crate::eval::runner::freeze::{definition_ref, load_definition, read_frozen_definition};
use crate::eval::runner::{freeze_refused, run_dir};
use crate::eval::{load_run, load_trials, load_verdicts, RunHeader, RunRecord};
use crate::graphql::escape_graphql_string;

/// Every run `owner` has, oldest first (`created_at`, then `run_id`).
pub async fn load_runs(access: &ConfigAccess, owner: &str) -> Result<Vec<RunRecord>> {
    let query = format!(
        r#"{{ EvalRun(filter: {{ owner_agent_did: {{ _eq: "{owner}" }} }}) {{ run_id }} }}"#,
        owner = escape_graphql_string(owner),
    );
    let response = access
        .transact("eval.report.load_runs", |txn| {
            let query = &query;
            Box::pin(async move { txn.execute(query).await })
        })
        .await?;
    let mut run_ids: Vec<String> = response["data"]["EvalRun"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["run_id"].as_str().map(str::to_owned))
        .collect();
    run_ids.sort();
    run_ids.dedup();
    let mut runs = Vec::with_capacity(run_ids.len());
    for run_id in run_ids {
        if let Some(record) = load_run(access, owner, &run_id).await? {
            runs.push(record);
        }
    }
    runs.sort_by(|left, right| {
        (&left.created_at, &left.run_id).cmp(&(&right.created_at, &right.run_id))
    });
    Ok(runs)
}

/// What `eval::scoring::exposure` counts about a run.
pub fn run_header(record: &RunRecord) -> RunHeader {
    RunHeader {
        definition_id: record.origin.definition.definition_id.clone(),
        comparability_version: record.origin.definition.comparability_version,
        split: record.origin.split,
        invalidated: record.invalidated.is_some(),
    }
}

/// `record`'s report, with exposure counted over `peers`. For a caller that
/// already listed the owner's runs. `runs_dir` is `<launching home>/eval/runs`.
///
/// The definition is the one the run froze beside itself; the installed one
/// is read only to say whether it has changed since. A run with no frozen
/// copy (frozen before the file existed, or whose directory was removed)
/// reads the installed definition, which `build` refuses unless it still
/// digests to the run's. A definition that cannot be resolved is a
/// [`ReportRefused`](super::ReportRefused), like every other unreportable run.
pub async fn load_report_among(
    access: &ConfigAccess,
    owner: &str,
    runs_dir: &Path,
    record: &RunRecord,
    peers: &[RunHeader],
) -> Result<EvalReport> {
    let frozen = &record.origin.definition;
    let run_dir = run_dir(runs_dir, &record.run_id).map_err(definition_refused)?;
    let live = load_definition(access, owner, &frozen.definition_id).await;
    let copy = read_frozen_definition(&run_dir, frozen).map_err(definition_refused)?;
    let (definition, definition_changed) = match copy {
        Some(definition) => {
            let changed = match live {
                Ok(live) => definition_ref(&live)?.digest != frozen.digest,
                // Missing or no longer valid: either way not what the run froze.
                Err(error) if unresolvable(&error) => true,
                Err(error) => return Err(error),
            };
            (definition, changed)
        }
        None => (live.map_err(definition_refused)?, false),
    };
    let trials = load_trials(access, owner, &record.run_id).await?;
    let verdicts = load_verdicts(access, owner, &record.run_id).await?;
    let mut report = build(record, &trials, &verdicts, &definition, peers)?;
    report.definition_changed = definition_changed;
    Ok(report)
}

/// A run whose definition cannot be resolved — its frozen copy tampered
/// with or unparseable, or, with no copy, the installed one missing or
/// invalid — is unreportable: one [`ReportRefused`](super::ReportRefused)
/// carrying the reason, so a caller skips it by checking one type.
/// Transport and database errors pass through unchanged.
fn definition_refused(error: anyhow::Error) -> anyhow::Error {
    if unresolvable(&error) {
        refused(format!("{error:#}"))
    } else {
        error
    }
}

/// A definition that is missing, invalid, tampered with or does not parse,
/// as opposed to one that could not be read at all.
fn unresolvable(error: &anyhow::Error) -> bool {
    freeze_refused(error).is_some() || error.chain().any(|cause| cause.is::<serde_json::Error>())
}

/// `run_id`'s report, read fresh from the documents.
pub async fn load_report(
    access: &ConfigAccess,
    owner: &str,
    runs_dir: &Path,
    run_id: &str,
) -> Result<EvalReport> {
    let record = load_run(access, owner, run_id)
        .await?
        .ok_or_else(|| refused(format!("no eval run {run_id:?} for {owner}")))?;
    let peers: Vec<RunHeader> = load_runs(access, owner)
        .await?
        .iter()
        .map(run_header)
        .collect();
    load_report_among(access, owner, runs_dir, &record, &peers).await
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::document_config::EvalSplit;
    use crate::eval::checks::CheckRegistry;
    use crate::eval::report::{report_refused, SlotClass};
    use crate::eval::runner::freeze::tests::{Launching, OWNER};
    use crate::eval::runner::{
        run, CellRequest, CellSource, RunOptions, RunRequest, ScriptedExecutor,
    };
    use crate::eval::{create_run, invalidate_run};

    fn request(launching: &Launching, pack: &Path, run_id: &str) -> RunRequest {
        RunRequest {
            run_id: run_id.into(),
            owner: OWNER.into(),
            evaluator_did: launching.evaluator_did(),
            definition_id: "monitor-findings".into(),
            split: EvalSplit::Validation,
            case_ids: None,
            cells: vec![CellRequest {
                cell_id: "baseline".into(),
                label: "baseline".into(),
                source: CellSource::Directory(pack.to_path_buf()),
                behavior_id: "monitor".into(),
                inference_profile_id: "local".into(),
            }],
            trials_per_case: 2,
            seed_base: 1_000,
            deadline_secs: Some(600),
            concurrency: 1,
            max_infra_retries: 0,
            breaker_threshold: 5,
            purpose: "eval".into(),
            source_commit: "store-test".into(),
            source_dirty: false,
            captures: Vec::new(),
            runs_dir: launching.runs_dir(),
        }
    }

    async fn scripted(launching: &Launching, request: &RunRequest) {
        let executor = ScriptedExecutor::new().with_default(ScriptedExecutor::passed_evidence(
            "did:key:trial",
            "check",
            "findings",
            vec![json!({})],
        ));
        run(
            &launching.access,
            request,
            &executor,
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &RunOptions {
                poll_backoff_base: Duration::from_millis(1),
                poll_backoff_cap: Duration::from_millis(2),
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_report_is_built_from_what_a_run_wrote() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "Off");
        scripted(&launching, &request(&launching, &pack, "run-1")).await;

        let report = load_report(&launching.access, OWNER, &launching.runs_dir(), "run-1")
            .await
            .unwrap();
        let cell = &report.cells[0];
        // Launching's check has no `min`, so every trial is graded by a
        // grader error: Unknown, never a failure of the subject.
        assert_eq!(
            cell.slots.iter().map(|slot| slot.class).collect::<Vec<_>>(),
            vec![SlotClass::Unknown; 2]
        );
        assert_eq!((cell.attempts, report.exposure), (2, 1));
        assert_eq!(report.run.source_commit, "store-test");
        assert!(!report.definition_changed);
    }

    #[tokio::test]
    async fn runs_are_listed_per_owner_and_exposure_skips_invalidated_runs() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "Off");
        scripted(&launching, &request(&launching, &pack, "run-1")).await;
        scripted(&launching, &request(&launching, &pack, "run-2")).await;
        invalidate_run(
            &launching.access,
            OWNER,
            "run-1",
            OWNER,
            "fixture was broken",
        )
        .await
        .unwrap();
        let first = load_run(&launching.access, OWNER, "run-1")
            .await
            .unwrap()
            .unwrap();
        create_run(
            &launching.access,
            "foreign",
            "did:key:someone-else",
            "did:key:someone-else",
            &first.origin,
        )
        .await
        .unwrap();

        let runs = load_runs(&launching.access, OWNER).await.unwrap();
        assert_eq!(
            runs.iter()
                .map(|run| run.run_id.as_str())
                .collect::<Vec<_>>(),
            vec!["run-1", "run-2"]
        );
        assert!(run_header(&runs[0]).invalidated);
        let report = load_report(&launching.access, OWNER, &launching.runs_dir(), "run-2")
            .await
            .unwrap();
        assert_eq!(report.exposure, 1, "the invalidated run is not exposure");
    }

    #[tokio::test]
    async fn a_report_reads_the_frozen_definition_after_the_installed_one_changes() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "Off");
        scripted(&launching, &request(&launching, &pack, "run-1")).await;
        let installed = load_definition(&launching.access, OWNER, "monitor-findings")
            .await
            .unwrap();
        let mut edited = serde_json::to_value(&installed).unwrap();
        edited["comparability_version"] = json!(2);
        launching
            .install(vec![(crate::Collection::EvalDefinition, edited)])
            .await;

        let report = load_report(&launching.access, OWNER, &launching.runs_dir(), "run-1")
            .await
            .unwrap();
        assert!(report.definition_changed);
        assert_eq!(report.run.definition.comparability_version, 1);
        assert_eq!(report.cells[0].slots.len(), 2);

        // Without the frozen copy, the installed definition no longer matches.
        std::fs::remove_file(
            launching
                .runs_dir()
                .join("run-1")
                .join(crate::eval::runner::DEFINITION_FILE),
        )
        .unwrap();
        let error = load_report(&launching.access, OWNER, &launching.runs_dir(), "run-1")
            .await
            .unwrap_err();
        assert!(
            report_refused(&error).is_some_and(|refusal| refusal.0.contains("no longer digests")),
            "{error:#}"
        );
    }

    /// A run frozen before `definition.json` existed still
    /// reports while the installed definition digests to the run's.
    #[tokio::test]
    async fn a_run_without_the_frozen_file_reports_from_a_matching_installed_definition() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "Off");
        scripted(&launching, &request(&launching, &pack, "run-old")).await;
        std::fs::remove_file(
            launching
                .runs_dir()
                .join("run-old")
                .join(crate::eval::runner::DEFINITION_FILE),
        )
        .unwrap();
        let report = load_report(&launching.access, OWNER, &launching.runs_dir(), "run-old")
            .await
            .unwrap();
        assert!(!report.definition_changed);
        assert_eq!(report.cells[0].slots.len(), 2);
    }

    fn definition_file(launching: &Launching, run_id: &str) -> std::path::PathBuf {
        launching
            .runs_dir()
            .join(run_id)
            .join(crate::eval::runner::DEFINITION_FILE)
    }

    /// Every way a run's definition fails to resolve is one refusal type,
    /// so a listing skips unreportable runs by checking `ReportRefused`.
    #[tokio::test]
    async fn a_tampered_or_unparseable_frozen_copy_is_a_report_refusal() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "Off");
        scripted(&launching, &request(&launching, &pack, "run-1")).await;
        let path = definition_file(&launching, "run-1");
        let mut tampered: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        tampered["comparability_version"] = json!(7);
        std::fs::write(&path, serde_json::to_vec(&tampered).unwrap()).unwrap();

        let error = load_report(&launching.access, OWNER, &launching.runs_dir(), "run-1")
            .await
            .unwrap_err();
        assert!(
            report_refused(&error).is_some_and(|refusal| refusal.0.contains("no longer digests")),
            "{error:#}"
        );

        std::fs::write(&path, b"{\"definition_id\": ").unwrap();
        let error = load_report(&launching.access, OWNER, &launching.runs_dir(), "run-1")
            .await
            .unwrap_err();
        assert!(
            report_refused(&error).is_some_and(|refusal| refusal.0.contains("parsing")),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn a_run_without_the_frozen_file_or_the_installed_definition_is_a_report_refusal() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "Off");
        scripted(&launching, &request(&launching, &pack, "run-old")).await;
        std::fs::remove_file(definition_file(&launching, "run-old")).unwrap();
        launching
            .delete(crate::Collection::EvalDefinition, "monitor-findings")
            .await;

        let error = load_report(&launching.access, OWNER, &launching.runs_dir(), "run-old")
            .await
            .unwrap_err();
        assert!(
            report_refused(&error).is_some_and(|refusal| refusal.0.contains("no eval definition")),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn a_frozen_copy_reports_after_the_installed_definition_is_deleted() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "Off");
        scripted(&launching, &request(&launching, &pack, "run-1")).await;
        launching
            .delete(crate::Collection::EvalDefinition, "monitor-findings")
            .await;

        let report = load_report(&launching.access, OWNER, &launching.runs_dir(), "run-1")
            .await
            .unwrap();
        assert!(report.definition_changed);
        assert_eq!(report.cells[0].slots.len(), 2);
    }

    #[tokio::test]
    async fn an_unknown_run_is_refused() {
        let launching = Launching::new().await;
        let error = load_report(&launching.access, OWNER, &launching.runs_dir(), "absent")
            .await
            .unwrap_err();
        let reason = &report_refused(&error).expect("a ReportRefused").0;
        assert_eq!(reason, &format!("no eval run \"absent\" for {OWNER}"));
    }
}
