//! `list`, `show` and `trial`: reads of the report `gents::eval::report`
//! builds from a run's documents.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use gents::document_config::{EvalSplit, EvalTier};
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
        rows.push(ListRow {
            run_id: record.run_id.clone(),
            definition_id: record.origin.definition.definition_id.clone(),
            comparability_version: record.origin.definition.comparability_version,
            split: record.origin.split,
            purpose: record.origin.purpose.clone(),
            created_at: record.created_at.clone(),
            invalidated: record.invalidated.clone(),
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
    pub(crate) regrade_of: Option<String>,
}

impl From<VerdictRecord> for TrialVerdict {
    fn from(verdict: VerdictRecord) -> Self {
        Self {
            reason_code: verdict
                .raw
                .get("reason_code")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            verdict_id: verdict.verdict_id,
            stage_id: verdict.stage_id,
            check: verdict.check,
            tier: verdict.tier,
            kind: verdict.kind,
            provider_reason: verdict.provider_reason,
            score_bp: verdict.score_bp,
            weight: verdict.weight,
            regrade_of: verdict.regrade_of,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct TrialView {
    pub(crate) run_id: String,
    pub(crate) cell_id: String,
    pub(crate) slot: SlotReport,
    /// The retained trial home: `<home>/eval/runs/<home_hint>`.
    pub(crate) home: Option<PathBuf>,
    /// Every verdict row of the latest attempt, regrades included.
    pub(crate) verdicts: Vec<TrialVerdict>,
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
    let view = TrialView {
        run_id: args.run_id.clone(),
        cell_id: args.cell.clone(),
        home: latest
            .home_hint
            .as_ref()
            .map(|hint| ctx.runs_dir().join(hint)),
        slot: slot.clone(),
        verdicts,
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
}
