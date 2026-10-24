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
use gents_protocol::error_chain::ErrorChain;
use serde::Serialize;

use super::{render, write_json, EvalContext};
use crate::cli::{EvalListArgs, EvalShowArgs, EvalTrialArgs};

#[derive(Debug, Serialize)]
pub(crate) struct ListRow {
    pub(crate) run_id: String,
    pub(crate) definition_id: String,
    pub(crate) comparability_version: i64,
    pub(crate) split: EvalSplit,
    pub(crate) purpose: String,
    pub(crate) created_at: String,
    pub(crate) invalidated: Option<Invalidation>,
    pub(crate) size_bytes: Option<u64>,
    pub(crate) size_error: Option<String>,
    pub(crate) cells: Vec<ListCell>,
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
                Err(error) => match report_refused(error.as_ref()) {
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
        Ok(write_json(out, &rows)?)
    } else {
        Ok(render::list_table(&rows, out)?)
    }
}

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
                error = %ErrorChain(&error),
                "eval list could not measure a run directory"
            );
            (None, Some(ErrorChain(&error).to_string()))
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
        Ok(write_json(out, &report)?)
    } else {
        Ok(render::report_table(&report, out)?)
    }
}

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
    pub(crate) slot: SlotReport,
    pub(crate) home: Option<PathBuf>,
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
                &ctx.runtime,
                &ctx.runs_dir(),
                &gents::eval::runner::TrialLocator {
                    trial_node_did: latest.trial_node_did.clone(),
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
        Ok(write_json(out, &view)?)
    } else {
        Ok(render::trial_text(&view, out)?)
    }
}
