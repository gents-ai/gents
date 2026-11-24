use std::io::Write;

use anyhow::{Context, Result};
use gents::eval::{load_run, load_trials, StageCompletion, TrialUsage};
use gents_documents::EvalSplit;
use gents_eval::report::{
    load_report, AttemptSummary, RunSummary, SlotClass, SlotReport, SlotScore, SlotVerdict,
};
use gents_eval::runner::embedded::{
    export_retained_transcript, StageTranscript, TokenUsage, TranscriptCalls, TranscriptOptions,
};
use gents_eval::runner::TrialLocator;
use gents_observe::observe::adapter_projection::ProjectionRedactionMode;
use serde::Serialize;

use super::EvalContext;
use crate::cli::{EvalExportArgs, EvalExportCallsArg};

#[derive(Debug, Serialize)]
pub(crate) struct ExportRecord {
    pub(crate) run_id: String,
    pub(crate) definition_id: String,
    pub(crate) comparability_version: i64,
    pub(crate) split: EvalSplit,
    pub(crate) cell_id: String,
    pub(crate) case_id: String,
    pub(crate) trial_index: u32,
    pub(crate) trial_id: String,
    pub(crate) attempt: u32,
    pub(crate) class: SlotClass,
    pub(crate) score_bp: Option<u32>,
    pub(crate) counted: SlotScore,
    /// The run report's usage for this trial.
    pub(crate) usage: TrialUsage,
    /// The same calls summed from the trial home, with reasoning and cache counts.
    pub(crate) transcript_usage: Option<TokenUsage>,
    pub(crate) redaction: ProjectionRedactionMode,
    pub(crate) stages: Vec<ExportStage>,
    /// Why the retained home yielded no transcript; the record keeps the rest.
    pub(crate) transcript_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ExportStage {
    #[serde(flatten)]
    pub(crate) completion: StageCompletion,
    pub(crate) verdicts: Vec<SlotVerdict>,
    pub(crate) transcript: Option<StageTranscript>,
}

pub(super) async fn export(
    ctx: &EvalContext,
    args: &EvalExportArgs,
    out: &mut dyn Write,
) -> Result<()> {
    let report = load_report(&ctx.access, &ctx.owner, &ctx.runs_dir(), &args.run_id).await?;
    if let Some(cell) = args.cell.as_deref() {
        anyhow::ensure!(
            report
                .cells
                .iter()
                .any(|candidate| candidate.cell_id == cell),
            "run {} has no cell {cell:?}",
            args.run_id
        );
    }
    let run = load_run(&ctx.access, &ctx.owner, &args.run_id)
        .await?
        .context("run disappeared while exporting it")?;
    anyhow::ensure!(
        ctx.evaluator.did() == run.evaluator_did,
        "retained trial export requires the evaluator identity that created the run"
    );
    let trials = load_trials(&ctx.access, &ctx.owner, &args.run_id).await?;
    let options = TranscriptOptions {
        calls: match args.calls {
            EvalExportCallsArg::Final => TranscriptCalls::Final,
            EvalExportCallsArg::All => TranscriptCalls::All,
        },
        redaction: crate::commands::trace::projection_redaction_mode(args.redaction),
    };
    let mut file = match args.output_file.as_deref() {
        Some(path) => {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating output directory {}", parent.display()))?;
            }
            Some(std::io::BufWriter::new(
                std::fs::File::create(path)
                    .with_context(|| format!("creating {}", path.display()))?,
            ))
        }
        None => None,
    };
    let sink: &mut dyn Write = match file.as_mut() {
        Some(file) => file,
        None => out,
    };
    let mut written = 0usize;
    for cell in report
        .cells
        .iter()
        .filter(|cell| args.cell.as_deref().is_none_or(|id| id == cell.cell_id))
    {
        for slot in cell.slots.iter().filter(|slot| {
            (args.cases.is_empty() || args.cases.contains(&slot.case_id))
                && args.trial.is_none_or(|index| index == slot.trial_index)
        }) {
            let Some(latest) = slot.latest.as_ref() else {
                continue;
            };
            let finished = trials
                .iter()
                .find(|trial| trial.identity.trial_id == latest.trial_id)
                .is_some_and(|trial| trial.completion.is_some());
            if !finished {
                tracing::warn!(
                    case_id = %slot.case_id,
                    trial_index = slot.trial_index,
                    trial_id = %latest.trial_id,
                    "skipping a trial that is still active or unfinished"
                );
                continue;
            }
            let record = export_slot(ctx, &report.run, &cell.cell_id, slot, latest, options).await;
            serde_json::to_writer(&mut *sink, &record)?;
            writeln!(sink)?;
            written += 1;
        }
    }
    sink.flush()?;
    anyhow::ensure!(
        written > 0,
        "run {} has no finished trial matching the selection",
        args.run_id
    );
    Ok(())
}

async fn export_slot(
    ctx: &EvalContext,
    run: &RunSummary,
    cell_id: &str,
    slot: &SlotReport,
    latest: &AttemptSummary,
    options: TranscriptOptions,
) -> ExportRecord {
    let transcript = export_retained_transcript(
        &ctx.runtime,
        &ctx.runs_dir(),
        &TrialLocator {
            trial_node_did: latest.trial_node_did.clone(),
            session_id: latest.session_id.clone(),
            home_hint: latest.home_hint.clone(),
        },
        ctx.evaluator.as_ref(),
        latest
            .stages
            .iter()
            .filter_map(|stage| stage.request_id.clone())
            .collect(),
        options,
    )
    .await;
    let (mut transcripts, transcript_usage, transcript_error) = match transcript {
        Ok(transcript) => (transcript.stages, Some(transcript.usage), None),
        Err(error) => (Vec::new(), None, Some(format!("{error:#}"))),
    };
    let stages = latest
        .stages
        .iter()
        .map(|stage| ExportStage {
            completion: stage.clone(),
            verdicts: slot
                .verdicts
                .iter()
                .filter(|verdict| verdict.stage_id == stage.stage_id)
                .cloned()
                .collect(),
            transcript: stage.request_id.as_deref().and_then(|request_id| {
                transcripts
                    .iter()
                    .position(|transcript| transcript.request_id == request_id)
                    .map(|index| transcripts.remove(index))
            }),
        })
        .collect();
    ExportRecord {
        run_id: run.run_id.clone(),
        definition_id: run.definition.definition_id.clone(),
        comparability_version: run.definition.comparability_version,
        split: run.split,
        cell_id: cell_id.to_owned(),
        case_id: slot.case_id.clone(),
        trial_index: slot.trial_index,
        trial_id: latest.trial_id.clone(),
        attempt: latest.attempt,
        class: slot.class,
        score_bp: slot.score_bp,
        counted: slot.counted,
        usage: latest.usage.clone(),
        transcript_usage,
        redaction: options.redaction,
        stages,
        transcript_error,
    }
}
