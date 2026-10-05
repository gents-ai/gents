//! `list`, `show` and `trial`: reads of the report `gents::eval::report`
//! builds from a run's documents.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use gents::document_config::{EvalSplit, EvalTier};
use gents::eval::checks::verdict_detail;
use gents::eval::report::build::reason_code;
use gents::eval::report::{
    load_report, load_report_among, load_runs, report_refused, run_header, SlotCounts, SlotReport,
};
use gents::eval::{
    load_verdicts, Invalidation, OutcomeKind, ProviderReason, RunHeader, VerdictRecord,
};
use serde::Serialize;

use super::{render, write_json, EvalContext};
use crate::cli::{EvalListArgs, EvalShowArgs, EvalTrialArgs};

/// One run as `eval list` shows it.
#[derive(Debug, Serialize)]
pub(crate) struct ListRow {
    pub(crate) run_id: String,
    pub(crate) definition_id: String,
    pub(crate) comparability_version: i64,
    pub(crate) split: EvalSplit,
    pub(crate) purpose: String,
    pub(crate) created_at: String,
    pub(crate) invalidated: Option<Invalidation>,
    /// The run directory's size; `None` once `rm` or `gc` removed it, or
    /// when it could not be read (`size_error` then says why).
    pub(crate) size_bytes: Option<u64>,
    pub(crate) size_error: Option<String>,
    pub(crate) cells: Vec<ListCell>,
    /// Why the run's report could not be built, when it could not.
    pub(crate) report_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ListCell {
    pub(crate) cell_id: String,
    pub(crate) counts: SlotCounts,
}

pub(super) async fn list(
    ctx: &EvalContext,
    args: &EvalListArgs,
    out: &mut dyn Write,
) -> Result<()> {
    let runs = load_runs(&ctx.access, &ctx.owner).await?;
    let peers: Vec<RunHeader> = runs.iter().map(run_header).collect();
    let mut rows = Vec::new();
    for record in &runs {
        if args
            .definition
            .as_deref()
            .is_some_and(|id| id != record.origin.definition.definition_id)
        {
            continue;
        }
        if record.invalidated.is_some() && !args.all {
            continue;
        }
        // An unreportable run is still listed, with why; any other failure
        // (transport, database) fails the listing.
        let (cells, report_error) =
            match load_report_among(&ctx.access, &ctx.owner, &ctx.runs_dir(), record, &peers).await
            {
                Ok(report) => (
                    report
                        .cells
                        .iter()
                        .map(|cell| ListCell {
                            cell_id: cell.cell_id.clone(),
                            counts: cell.counts,
                        })
                        .collect(),
                    None,
                ),
                Err(error) => match report_refused(&error) {
                    Some(refusal) => (Vec::new(), Some(refusal.to_string())),
                    None => return Err(error),
                },
            };
        let (size_bytes, size_error) = run_size(ctx, &record.run_id);
        rows.push(ListRow {
            run_id: record.run_id.clone(),
            definition_id: record.origin.definition.definition_id.clone(),
            comparability_version: record.origin.definition.comparability_version,
            split: record.origin.split,
            purpose: record.origin.purpose.clone(),
            created_at: record.created_at.clone(),
            invalidated: record.invalidated.clone(),
            size_bytes,
            size_error,
            cells,
            report_error,
        });
    }
    if args.json {
        write_json(out, &rows)
    } else {
        Ok(render::list_table(&rows, out)?)
    }
}

/// The run directory's size: `(None, None)` when there is no directory,
/// `(None, Some(why))` when it could not be read.
fn run_size(ctx: &EvalContext, run_id: &str) -> (Option<u64>, Option<String>) {
    let Some(dir) = gents::eval::runner::run_dir(&ctx.runs_dir(), run_id)
        .ok()
        .filter(|dir| dir.is_dir())
    else {
        return (None, None);
    };
    match super::manage::dir_size(&dir) {
        Ok(size) => (Some(size), None),
        Err(error) => {
            tracing::warn!(
                run_id,
                dir = %dir.display(),
                error = %format!("{error:#}"),
                "eval list could not measure a run directory"
            );
            (None, Some(format!("{error:#}")))
        }
    }
}

pub(super) async fn show(
    ctx: &EvalContext,
    args: &EvalShowArgs,
    out: &mut dyn Write,
) -> Result<()> {
    let report = load_report(&ctx.access, &ctx.owner, &ctx.runs_dir(), &args.run_id).await?;
    if args.json {
        write_json(out, &report)
    } else {
        Ok(render::report_table(&report, out)?)
    }
}

/// One verdict row of the slot's latest attempt.
#[derive(Debug, Serialize)]
pub(crate) struct TrialVerdict {
    pub(crate) verdict_id: String,
    pub(crate) stage_id: String,
    pub(crate) check: String,
    pub(crate) tier: EvalTier,
    pub(crate) kind: OutcomeKind,
    pub(crate) provider_reason: Option<ProviderReason>,
    pub(crate) score_bp: Option<u32>,
    pub(crate) weight: u32,
    pub(crate) reason_code: Option<String>,
    /// What the check observed against what it expected.
    pub(crate) detail: Option<String>,
    pub(crate) regrade_of: Option<String>,
    pub(crate) raw: serde_json::Value,
    pub(crate) feedback: Option<String>,
}

impl From<VerdictRecord> for TrialVerdict {
    fn from(verdict: VerdictRecord) -> Self {
        Self {
            reason_code: reason_code(&verdict),
            detail: verdict_detail(&verdict.raw),
            verdict_id: verdict.verdict_id,
            stage_id: verdict.stage_id,
            check: verdict.check,
            tier: verdict.tier,
            kind: verdict.kind,
            provider_reason: verdict.provider_reason,
            score_bp: verdict.score_bp,
            weight: verdict.weight,
            regrade_of: verdict.regrade_of,
            raw: verdict.raw,
            feedback: verdict.feedback,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct TrialView {
    pub(crate) run_id: String,
    pub(crate) cell_id: String,
    /// The slot as the report counts it: its `verdicts` are the counted
    /// attempt's, after regrade supersession, which is what scoring reads.
    pub(crate) slot: SlotReport,
    /// The retained trial home: `<home>/eval/runs/<home_hint>`.
    pub(crate) home: Option<PathBuf>,
    /// Every verdict row of the latest attempt, regrades and the verdicts
    /// they superseded included. The latest attempt can differ from the
    /// counted one: the counted attempt is the latest completed one, so a
    /// retry in flight has no verdicts here yet.
    pub(crate) verdicts: Vec<TrialVerdict>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) requests: Option<Vec<gents::eval::runner::embedded::RetainedRequest>>,
}

pub(super) async fn trial(
    ctx: &EvalContext,
    args: &EvalTrialArgs,
    out: &mut dyn Write,
) -> Result<()> {
    let report = load_report(&ctx.access, &ctx.owner, &ctx.runs_dir(), &args.run_id).await?;
    let trial_index = args.trial_index.unwrap_or(0);
    let cell = report
        .cells
        .iter()
        .find(|cell| cell.cell_id == args.cell)
        .with_context(|| format!("run {} has no cell {:?}", args.run_id, args.cell))?;
    let slot = cell
        .slots
        .iter()
        .find(|slot| slot.case_id == args.case_id && slot.trial_index == trial_index)
        .with_context(|| {
            format!(
                "run {} cell {} has no slot for case {:?} trial {trial_index}",
                args.run_id, args.cell, args.case_id
            )
        })?;
    let latest = slot.latest.as_ref().with_context(|| {
        format!(
            "case {:?} trial {trial_index} of cell {} has no attempt yet: it is planned",
            args.case_id, args.cell
        )
    })?;
    let mut verdicts: Vec<TrialVerdict> = load_verdicts(&ctx.access, &ctx.owner, &args.run_id)
        .await?
        .into_iter()
        .filter(|verdict| verdict.trial_id == latest.trial_id)
        .map(TrialVerdict::from)
        .collect();
    verdicts.sort_by(|left, right| {
        (&left.stage_id, &left.check, &left.verdict_id).cmp(&(
            &right.stage_id,
            &right.check,
            &right.verdict_id,
        ))
    });
    let requests = if args.requests {
        let trials = gents::eval::load_trials(&ctx.access, &ctx.owner, &args.run_id).await?;
        let trial = trials
            .iter()
            .find(|trial| trial.identity.trial_id == latest.trial_id)
            .context("trial disappeared while inspecting the run")?;
        anyhow::ensure!(
            trial.completion.is_some(),
            "trial {} is still active or unfinished; request inspection requires a finished trial",
            latest.trial_id
        );
        Some(
            gents::eval::runner::embedded::inspect_retained_requests(
                &ctx.runs_dir(),
                &gents::eval::runner::TrialLocator {
                    trial_agent_did: latest.trial_agent_did.clone(),
                    session_id: latest.session_id.clone(),
                    home_hint: latest.home_hint.clone(),
                },
            )
            .await?,
        )
    } else {
        None
    };
    let view = TrialView {
        run_id: args.run_id.clone(),
        cell_id: args.cell.clone(),
        home: latest
            .home_hint
            .as_ref()
            .map(|hint| ctx.runs_dir().join(hint)),
        slot: slot.clone(),
        verdicts,
        requests,
    };
    if args.json {
        write_json(out, &view)
    } else {
        Ok(render::trial_text(&view, out)?)
    }
}

#[cfg(test)]
mod tests {
    use tokio_util::sync::CancellationToken;

    use super::super::testing::{eval, executor, row, Fixture, VALIDATION_CASES};

    #[tokio::test]
    async fn show_prints_six_slot_counts_per_cell_and_json_is_the_report() {
        let fixture = Fixture::new().await;
        fixture
            .scripted_run(
                "r1",
                &executor(&VALIDATION_CASES[..3]),
                CancellationToken::new(),
            )
            .await;

        let table = eval(&fixture, &["show", "r1"]).await.unwrap();
        assert_eq!(
            row(&table, "baseline", 10),
            vec!["baseline", "6", "6", "0", "0", "0", "0", "12", "50.00%", "-/-"]
        );
        assert_eq!(
            row(&table, "candidate", 10),
            vec![
                "candidate",
                "12",
                "0",
                "0",
                "0",
                "0",
                "0",
                "12",
                "100.00%",
                "-/-"
            ]
        );
        assert!(table.contains("exposure 1 "), "{table}");

        let json: serde_json::Value =
            serde_json::from_str(&eval(&fixture, &["show", "r1", "--json"]).await.unwrap())
                .unwrap();
        assert_eq!(json["report_version"], 1);
        assert_eq!(json["cells"][0]["cell_id"], "baseline");
        assert_eq!(json["cells"][0]["counts"]["fail"], 6);
        assert_eq!(json["cells"][0]["slots"][0]["class"], "fail");
        assert_eq!(json["cells"][1]["headline_bp"], 10_000);
    }

    #[tokio::test]
    async fn list_hides_invalidated_runs_unless_asked_and_counts_slots_per_cell() {
        let fixture = Fixture::new().await;
        let scripted = executor(&VALIDATION_CASES[..3]);
        fixture
            .scripted_run("r1", &scripted, CancellationToken::new())
            .await;
        fixture
            .scripted_run("r2", &scripted, CancellationToken::new())
            .await;
        gents::eval::invalidate_run(
            &fixture.ctx.access,
            &fixture.ctx.owner,
            "r1",
            &fixture.ctx.owner,
            "fixture was broken",
        )
        .await
        .unwrap();

        let listed = eval(&fixture, &["list"]).await.unwrap();
        assert!(
            !listed.lines().any(|line| line.starts_with("r1 ")),
            "{listed}"
        );
        let r2 = listed
            .lines()
            .find(|line| line.starts_with("r2 "))
            .unwrap_or_else(|| panic!("{listed}"));
        assert!(
            r2.contains("baseline[pass 6 fail 6 unknown 0 not_evidence 0 abandoned 0 planned 0]"),
            "{r2}"
        );

        let all = eval(&fixture, &["list", "--all"]).await.unwrap();
        let r1 = all
            .lines()
            .find(|line| line.starts_with("r1 "))
            .unwrap_or_else(|| panic!("{all}"));
        assert!(r1.contains(" yes "), "{r1}");

        let json: serde_json::Value =
            serde_json::from_str(&eval(&fixture, &["list", "--all", "--json"]).await.unwrap())
                .unwrap();
        assert_eq!(json.as_array().map(Vec::len), Some(2));
        assert_eq!(json[0]["run_id"], "r1");
        assert_eq!(json[0]["invalidated"]["reason"], "fixture was broken");

        let other = eval(&fixture, &["list", "--definition", "other"])
            .await
            .unwrap();
        assert_eq!(other.lines().count(), 1, "only the header: {other}");
    }

    #[tokio::test]
    async fn list_shows_an_unreportable_run_with_its_refusal_and_the_rest_normally() {
        let fixture = Fixture::new().await;
        let scripted = executor(&VALIDATION_CASES[..3]);
        fixture
            .scripted_run("r1", &scripted, CancellationToken::new())
            .await;
        fixture
            .scripted_run("r2", &scripted, CancellationToken::new())
            .await;
        let frozen = fixture.ctx.runs_dir().join("r1").join("definition.json");
        std::fs::write(&frozen, "not a definition").unwrap();

        let listed = eval(&fixture, &["list"]).await.unwrap();
        let r1 = listed
            .lines()
            .find(|line| line.starts_with("r1 "))
            .unwrap_or_else(|| panic!("{listed}"));
        assert!(r1.contains("report unavailable: parsing "), "{r1}");
        assert!(r1.contains("definition.json"), "{r1}");
        let r2 = listed
            .lines()
            .find(|line| line.starts_with("r2 "))
            .unwrap_or_else(|| panic!("{listed}"));
        assert!(
            r2.contains("baseline[pass 6 fail 6 unknown 0 not_evidence 0 abandoned 0 planned 0]"),
            "{r2}"
        );

        let json: serde_json::Value =
            serde_json::from_str(&eval(&fixture, &["list", "--json"]).await.unwrap()).unwrap();
        assert_eq!(json[0]["run_id"], "r1");
        assert_eq!(json[0]["cells"], serde_json::json!([]));
        assert!(
            json[0]["report_error"]
                .as_str()
                .is_some_and(|error| error.contains("definition.json")),
            "{json}"
        );
        assert_eq!(json[1]["run_id"], "r2");
        assert!(json[1]["report_error"].is_null(), "{json}");

        let error = eval(&fixture, &["show", "r1"]).await.unwrap_err();
        assert!(error.to_string().starts_with("parsing "), "{error:#}");
    }

    #[tokio::test]
    async fn trial_prints_the_latest_attempt_with_its_verdicts() {
        let fixture = Fixture::new().await;
        fixture
            .scripted_run(
                "r1",
                &executor(&VALIDATION_CASES[..3]),
                CancellationToken::new(),
            )
            .await;

        let text = eval(&fixture, &["trial", "r1", "baseline", "val-a", "1"])
            .await
            .unwrap();
        assert!(text.contains("class fail"), "{text}");
        assert!(text.contains("stage check terminal "), "{text}");
        assert!(
            text.contains(
                "verdict captured_rows_count acceptance model_acceptance score 0 reason below_min"
            ),
            "{text}"
        );

        let json: serde_json::Value = serde_json::from_str(
            &eval(&fixture, &["trial", "r1", "candidate", "val-a", "--json"])
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(json["slot"]["class"], "pass");
        assert_eq!(json["slot"]["trial_index"], 0);
        assert_eq!(json["verdicts"][0]["reason_code"], "in_range");
        assert_eq!(json["verdicts"][0]["raw"]["count"], 1);
        assert!(json.get("requests").is_none());

        let cancelled = CancellationToken::new();
        cancelled.cancel();
        fixture.scripted_run("r2", &executor(&[]), cancelled).await;
        let error = eval(&fixture, &["trial", "r2", "baseline", "val-a"])
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("has no attempt yet"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn trial_requests_read_original_content_without_changing_eval_records() {
        use gents::eval::runner::embedded::EmbeddedHome;
        use gents::eval::runner::{ScriptedExecutor, TrialLocator};
        use gents::graphql::escape_graphql_string;
        use gents::ConfigAccess;

        let fixture = Fixture::new().await;
        let path = fixture.ctx.runs_dir().join("retained/home");
        let home = EmbeddedHome::create_retained(&path).await.unwrap();
        let did = home.did().to_owned();
        let prompt = "Inspect the original \"request\"\nwithout another model turn.";
        ConfigAccess::write_local(&home.node, "eval.cli.test.retained_request", &format!(
            r#"mutation {{ create_AgentRequest(input: {{ request_id: "child", purpose: "normal", agent_did: "{}", session_id: "child-session", content: "{}", lifecycle_state: "completed", retry_root_request: "root", caused_by_parent_request_id: "parent", caused_by_parent_tool_call_id: "tool", caused_by_source_doc_id: "source", created_at: "2026-01-01T00:00:00Z" }}) {{ _docID }} }}"#,
            escape_graphql_string(&did), escape_graphql_string(prompt),
        )).await.unwrap();
        home.node.shutdown().await;
        drop(home);
        let mut evidence = ScriptedExecutor::passed_evidence(
            &did,
            "check",
            "findings",
            vec![serde_json::json!({})],
        );
        evidence.locator = TrialLocator {
            trial_agent_did: did.clone(),
            session_id: "session".into(),
            home_hint: Some("retained".into()),
        };
        let scripted = ScriptedExecutor::new().with_default(evidence);
        let mut request = fixture.request("r1");
        request.cells.truncate(1);
        request.case_ids = Some(vec!["val-a".into()]);
        request.trials_per_case = 1;
        gents::eval::runner::run(
            &fixture.ctx.access,
            &request,
            &scripted,
            &gents::eval::checks::CheckRegistry::builtin(),
            CancellationToken::new(),
            &super::super::testing::fast(),
        )
        .await
        .unwrap();
        let records_before =
            gents::eval::load_trials(&fixture.ctx.access, &fixture.ctx.owner, "r1")
                .await
                .unwrap();
        let verdicts_before =
            gents::eval::load_verdicts(&fixture.ctx.access, &fixture.ctx.owner, "r1")
                .await
                .unwrap();
        let json: serde_json::Value = serde_json::from_str(
            &eval(
                &fixture,
                &["trial", "r1", "baseline", "val-a", "--requests", "--json"],
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(json["requests"][0]["content"], prompt);
        assert_eq!(json["requests"][0]["agent_did"], did);
        assert_eq!(json["requests"][0]["retry_root_request"], "root");
        assert_eq!(json["requests"][0]["caused_by_parent_request_id"], "parent");
        assert_eq!(json["requests"][0]["caused_by_source_doc_id"], "source");
        assert!(json["requests"][0]["request_doc_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()));
        let text = eval(
            &fixture,
            &["trial", "r1", "baseline", "val-a", "--requests"],
        )
        .await
        .unwrap();
        assert!(text.contains(prompt), "{text}");
        assert!(text.contains("retry_root root"), "{text}");
        assert!(text.contains("parent_request parent"), "{text}");
        assert!(text.contains("source_document source"), "{text}");
        assert_eq!(
            gents::eval::load_trials(&fixture.ctx.access, &fixture.ctx.owner, "r1")
                .await
                .unwrap(),
            records_before
        );
        assert_eq!(
            gents::eval::load_verdicts(&fixture.ctx.access, &fixture.ctx.owner, "r1")
                .await
                .unwrap(),
            verdicts_before
        );

        let mut active = records_before[0].identity.clone();
        active.attempt += 1;
        active.trial_id = "unfinished-inspection".into();
        gents::eval::create_trial(&fixture.ctx.access, &fixture.ctx.owner, &active)
            .await
            .unwrap();
        let error = eval(
            &fixture,
            &["trial", "r1", "baseline", "val-a", "--requests"],
        )
        .await
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("still active or unfinished"),
            "{error:#}"
        );
    }
}
