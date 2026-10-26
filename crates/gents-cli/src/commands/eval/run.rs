use gents_protocol::error_chain::ErrorChain;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::io::Write;

use anyhow::Result;
use gents::eval::documents::default_breaker_threshold;
use gents::eval::load_trials;
use gents::eval::report::load_report;
use gents::eval::runner::{self, CellRequest, RunOutcome, RunRequest, CANCEL_MARKER};
use gents::{default_agent_id_for_node, default_inference_profile_id_for_agent};

use super::{
    default_id, follow_progress, render, source_commit, source_dirty, write_json, Deps,
    EvalCommandError, EvalContext, Progress,
};
use crate::cli::{EvalResumeArgs, EvalRunArgs};
use crate::commands::pack::{resolve_subject_pack, SubjectPack};

pub(super) async fn run(
    ctx: &EvalContext,
    args: &EvalRunArgs,
    deps: &Deps<'_>,
    out: &mut dyn Write,
) -> Result<()> {
    let (request, _packs) = run_request(ctx, args).await?;
    run_prepared(ctx, args, &request, deps, out).await
}

pub(super) async fn run_prepared(
    ctx: &EvalContext,
    args: &EvalRunArgs,
    request: &RunRequest,
    deps: &Deps<'_>,
    out: &mut dyn Write,
) -> Result<()> {
    let run_id = request.run_id.clone();
    let marker = runner::run_dir(&ctx.runs_dir(), &run_id)?.join(CANCEL_MARKER);
    let had_marker = marker.exists();
    let landed = landed_attempts(ctx, &run_id).await?;
    let result = follow(
        ctx,
        &run_id,
        landed,
        !args.json,
        out,
        runner::run(
            &ctx.access,
            request,
            deps.executor,
            deps.registry,
            deps.cancel.clone(),
            &deps.options,
        ),
    )
    .await;
    if result.is_ok() && had_marker && !args.json {
        writeln!(out, "cleared the cancel marker {}", marker.display())?;
    }
    finish(ctx, &run_id, args.json, result, out).await
}

async fn landed_attempts(ctx: &EvalContext, run_id: &str) -> Result<BTreeSet<String>> {
    Ok(load_trials(&ctx.access, &ctx.owner, run_id)
        .await?
        .into_iter()
        .filter(|trial| trial.completion.is_some())
        .map(|trial| trial.identity.trial_id)
        .collect())
}

pub(super) async fn resume(
    ctx: &EvalContext,
    args: &EvalResumeArgs,
    deps: &Deps<'_>,
    out: &mut dyn Write,
) -> Result<()> {
    let marker = runner::run_dir(&ctx.runs_dir(), &args.run_id)?.join(CANCEL_MARKER);
    let had_marker = marker.exists();
    let landed = landed_attempts(ctx, &args.run_id).await?;
    let result = follow(
        ctx,
        &args.run_id,
        landed,
        !args.json,
        out,
        runner::resume(
            &ctx.access,
            &ctx.owner,
            &args.run_id,
            &ctx.runs_dir(),
            deps.executor,
            deps.registry,
            deps.cancel.clone(),
            &deps.options,
        ),
    )
    .await;
    if result.is_ok() && had_marker && !args.json {
        writeln!(out, "cleared the cancel marker {}", marker.display())?;
    }
    finish(ctx, &args.run_id, args.json, result, out).await
}

pub(super) fn profiles_by_cell(
    args: &EvalRunArgs,
) -> Result<BTreeMap<&str, &str>, EvalCommandError> {
    let mut cells = BTreeSet::new();
    for cell in &args.cells {
        if !cells.insert(cell.cell_id.as_str()) {
            return Err(EvalCommandError::DuplicateCell {
                cell_id: cell.cell_id.clone(),
            });
        }
    }
    let mut profiles: BTreeMap<&str, &str> = BTreeMap::new();
    for (cell_id, profile_id) in &args.profiles {
        if !cells.contains(cell_id.as_str()) {
            return Err(EvalCommandError::ProfileForUndeclaredCell {
                cell_id: cell_id.clone(),
            });
        }
        if profiles.insert(cell_id, profile_id).is_some() {
            return Err(EvalCommandError::DuplicateProfile {
                cell_id: cell_id.clone(),
            });
        }
    }
    Ok(profiles)
}

pub(super) async fn run_request(
    ctx: &EvalContext,
    args: &EvalRunArgs,
) -> Result<(RunRequest, Vec<SubjectPack>)> {
    let profiles = profiles_by_cell(args)?;
    let default_profile =
        default_inference_profile_id_for_agent(&default_agent_id_for_node(&ctx.owner));
    let mut cells = Vec::with_capacity(args.cells.len());
    let mut packs = Vec::with_capacity(args.cells.len());
    for cell in &args.cells {
        let pack =
            resolve_subject_pack(&ctx.home_dir, &cell.pack, args.registry.as_deref()).await?;
        let agent_id = match &cell.agent {
            Some(agent) => agent.clone(),
            None => pack.default_agent()?,
        };
        cells.push(CellRequest {
            cell_id: cell.cell_id.clone(),
            label: cell.cell_id.clone(),
            source: pack.source.clone(),
            agent_id,
            inference_profile_id: profiles
                .get(cell.cell_id.as_str())
                .map_or_else(|| default_profile.clone(), |profile| (*profile).to_owned()),
        });
        packs.push(pack);
    }
    let request = RunRequest {
        run_id: args
            .run_id
            .clone()
            .unwrap_or_else(|| default_id(&args.definition_id)),
        owner: ctx.owner.clone(),
        evaluator_did: ctx.owner.clone(),
        definition_id: args.definition_id.clone(),
        split: args.split,
        case_ids: None,
        cells,
        trials_per_case: args.trials,
        seed_base: args.seed_base,
        deadline_secs: None,
        concurrency: args.concurrency,
        max_infra_retries: args.max_infra_retries,
        breaker_threshold: default_breaker_threshold(),
        purpose: args.purpose.clone(),
        source_commit: source_commit(),
        source_dirty: source_dirty(),
        captures: Vec::new(),
        runs_dir: ctx.runs_dir(),
    };
    Ok((request, packs))
}

pub(super) async fn follow<F>(
    ctx: &EvalContext,
    run_id: &str,
    landed: BTreeSet<String>,
    print: bool,
    out: &mut dyn Write,
    running: F,
) -> Result<RunOutcome>
where
    F: Future<Output = Result<RunOutcome>>,
{
    let mut progress = LandedTrials {
        ctx,
        run_id,
        landed,
        out,
    };
    follow_progress(print, &mut progress, running).await
}

struct LandedTrials<'a> {
    ctx: &'a EvalContext,
    run_id: &'a str,
    landed: BTreeSet<String>,
    out: &'a mut dyn Write,
}

impl Progress for LandedTrials<'_> {
    async fn report(&mut self) {
        report_landed(self.ctx, self.run_id, &mut self.landed, &mut *self.out).await;
    }
}

async fn report_landed(
    ctx: &EvalContext,
    run_id: &str,
    landed: &mut BTreeSet<String>,
    out: &mut dyn Write,
) {
    let trials = match load_trials(&ctx.access, &ctx.owner, run_id).await {
        Ok(trials) => trials,
        Err(error) => {
            tracing::warn!(run_id, error = %ErrorChain(&error), "could not read the run's trials for progress");
            return;
        }
    };
    let mut new: Vec<_> = trials
        .into_iter()
        .filter(|trial| trial.completion.is_some() && !landed.contains(&trial.identity.trial_id))
        .collect();
    new.sort_by(|left, right| {
        let key = |trial: &gents::eval::TrialRecord| {
            (
                trial.identity.cell_id.clone(),
                trial.identity.case_id.clone(),
                trial.identity.trial_index,
                trial.identity.attempt,
            )
        };
        key(left).cmp(&key(right))
    });
    for trial in new {
        let identity = &trial.identity;
        if let Err(error) = writeln!(
            out,
            "landed {} {} #{} attempt {}",
            identity.cell_id, identity.case_id, identity.trial_index, identity.attempt
        ) {
            tracing::warn!(error = %error, "could not write a progress line");
            return;
        }
        landed.insert(identity.trial_id.clone());
    }
}

async fn finish(
    ctx: &EvalContext,
    run_id: &str,
    json: bool,
    result: Result<RunOutcome>,
    out: &mut dyn Write,
) -> Result<()> {
    let outcome = result?;
    let stopped = (outcome.cancelled || outcome.abandoned > 0).then(|| {
        format!(
            "run {run_id} stopped before it finished ({} completed, {} abandoned this pass); `gents eval resume {run_id}` continues it",
            outcome.completed, outcome.abandoned
        )
    });
    let report = load_report(&ctx.access, &ctx.owner, &ctx.runs_dir(), run_id).await?;
    if json {
        write_json(out, &report)?;
    } else {
        render::report_table(&report, out)?;
    }
    match stopped {
        Some(note) => Err(EvalCommandError::Stopped { note }.into()),
        None => Ok(()),
    }
}
