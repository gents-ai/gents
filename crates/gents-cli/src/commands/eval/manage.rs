use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gents::eval::report::load_runs;
use gents::eval::runner::{request_cancel, run_dir, run_finished, running_elsewhere, slots_owed};
use gents::eval::{invalidate_run, load_run, load_trials};
use gents::ConfigAccess;
use gents_protocol::error_chain::ErrorChain;

use super::render::human_bytes;
use super::{EvalCommandError, EvalContext};
use crate::cli::{EvalGcArgs, EvalInvalidateArgs, EvalRmArgs, EvalRunIdArgs};

/// The marker needs no database, so it is written first: the process
/// hosting the run may hold the home's embedded node, and `purpose` (which
/// opens it) is awaited only afterwards.
pub(crate) async fn cancel<P>(
    runs_dir: &Path,
    run_id: &str,
    purpose: P,
    out: &mut dyn Write,
) -> Result<()>
where
    P: Future<Output = Result<Option<String>>>,
{
    write_marker(runs_dir, run_id, out)?;
    match purpose.await {
        Ok(purpose) => {
            if let Some(job_id) = purpose
                .as_deref()
                .and_then(|purpose| purpose.strip_prefix("optimization:"))
            {
                writeln!(
                    out,
                    "this run belongs to job {job_id}; the job stops at this run, and `gents optimization run … --job-id {job_id}` resumes it"
                )?;
            }
        }
        Err(error) => {
            tracing::warn!(
                run_id,
                error = %format!("{error:#}"),
                "the cancel marker is written, but the run's documents could not be read to check whether it belongs to an optimization job"
            );
            writeln!(
                out,
                "could not read run {run_id}'s documents to check whether it belongs to an optimization job"
            )?;
        }
    }
    Ok(())
}

pub(crate) fn write_marker(runs_dir: &Path, run_id: &str, out: &mut dyn Write) -> Result<()> {
    let marker = request_cancel(runs_dir, run_id)?;
    if marker.parent().is_some_and(running_elsewhere) {
        writeln!(
            out,
            "wrote {}; the process hosting run {run_id} stops launching at its next check, and `gents eval resume {run_id}` continues it",
            marker.display()
        )?;
    } else {
        writeln!(
            out,
            "wrote {}; no process is running run {run_id} now, and the next `gents eval run --run-id {run_id}` or `gents eval resume {run_id}` clears the marker",
            marker.display()
        )?;
    }
    Ok(())
}

pub(crate) async fn run_purpose(
    access: &ConfigAccess,
    owner: &str,
    run_id: &str,
) -> Result<Option<String>> {
    Ok(load_run(access, owner, run_id)
        .await?
        .map(|record| record.origin.purpose))
}

pub(super) async fn cancel_run(
    ctx: &EvalContext,
    args: &EvalRunIdArgs,
    out: &mut dyn Write,
) -> Result<()> {
    cancel(
        &ctx.runs_dir(),
        &args.run_id,
        run_purpose(&ctx.access, &ctx.owner, &args.run_id),
        out,
    )
    .await
}

pub(super) async fn invalidate(
    ctx: &EvalContext,
    args: &EvalInvalidateArgs,
    out: &mut dyn Write,
) -> Result<()> {
    invalidate_run(
        &ctx.access,
        &ctx.owner,
        &args.run_id,
        &ctx.owner,
        &args.reason,
    )
    .await?;
    writeln!(
        out,
        "invalidated run {} as {}: {}",
        args.run_id, ctx.owner, args.reason
    )?;
    Ok(())
}

pub(super) async fn rm(ctx: &EvalContext, args: &EvalRmArgs, out: &mut dyn Write) -> Result<()> {
    let dir = run_dir(&ctx.runs_dir(), &args.run_id)?;
    anyhow::ensure!(
        dir.is_dir(),
        "run {} has no directory {}",
        args.run_id,
        dir.display()
    );
    let live = || {
        anyhow::anyhow!(
            "run {} is being run by a live process right now (see `gents eval watch {}`); pass --force to delete its directory anyway",
            args.run_id,
            args.run_id
        )
    };
    if !args.force {
        let record = load_run(&ctx.access, &ctx.owner, &args.run_id)
            .await?
            .with_context(|| format!("no eval run {:?} for {}", args.run_id, ctx.owner))?;
        let trials = load_trials(&ctx.access, &ctx.owner, &args.run_id).await?;
        let owed = slots_owed(&record, &trials);
        anyhow::ensure!(
            owed == 0,
            "run {} is unfinished: it still owes {owed} slots; `gents eval resume {}` finishes it, or pass --force to delete its directory anyway",
            args.run_id,
            args.run_id
        );
        if running_elsewhere(&dir) {
            return Err(live());
        }
        if let Some((job_id, state)) = gents::optimization::held_runs(&ctx.access, &ctx.owner)
            .await?
            .get(&args.run_id)
        {
            anyhow::bail!(
                "run {} is evidence of optimization job {job_id}, which is {}; pass --force to delete its directory anyway",
                args.run_id,
                state.label()
            );
        }
    }
    let bytes = dir_size(&dir)?;
    if !args.force && running_elsewhere(&dir) {
        return Err(live());
    }
    std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    tracing::warn!(run_id = %args.run_id, bytes, "eval run directory removed; its documents stay");
    writeln!(
        out,
        "removed {} ({bytes} bytes reclaimed); the run's documents stay",
        dir.display()
    )?;
    Ok(())
}

pub(super) async fn gc(ctx: &EvalContext, args: &EvalGcArgs, out: &mut dyn Write) -> Result<()> {
    let cutoff = chrono::Duration::from_std(args.older_than)
        .ok()
        .and_then(|age| chrono::Utc::now().checked_sub_signed(age))
        .context("--older-than reaches before the earliest representable time")?;
    let records = load_runs(&ctx.access, &ctx.owner).await?;
    let referenced = gents::optimization::referenced_run_ids(&ctx.access, &ctx.owner).await?;
    let mut runs: Vec<Doomed> = Vec::new();
    for record in &records {
        if args
            .definition
            .as_deref()
            .is_some_and(|id| id != record.origin.definition.definition_id)
        {
            continue;
        }
        let dir = match run_dir(&ctx.runs_dir(), &record.run_id) {
            Ok(dir) => dir,
            Err(error) => {
                writeln!(out, "kept {}: {error}", record.run_id)?;
                continue;
            }
        };
        if !dir.is_dir() {
            continue;
        }
        let created = match chrono::DateTime::parse_from_rfc3339(&record.created_at) {
            Ok(created) => created.with_timezone(&chrono::Utc),
            Err(error) => {
                writeln!(
                    out,
                    "kept {}: its created_at {:?} does not parse ({error})",
                    record.run_id, record.created_at
                )?;
                continue;
            }
        };
        if created > cutoff {
            continue;
        }
        if referenced.contains(&record.run_id) {
            writeln!(
                out,
                "kept {}: an optimization job references it",
                record.run_id
            )?;
            continue;
        }
        let trials = match load_trials(&ctx.access, &ctx.owner, &record.run_id).await {
            Ok(trials) => trials,
            Err(error) => {
                writeln!(out, "kept {}: {error:#}", record.run_id)?;
                continue;
            }
        };
        if !run_finished(record, &trials, &dir) {
            let owed = slots_owed(record, &trials);
            if owed > 0 {
                writeln!(
                    out,
                    "kept {}: unfinished (it still owes {owed} slots)",
                    record.run_id
                )?;
            } else {
                writeln!(out, "kept {}: a live process is running it", record.run_id)?;
            }
            continue;
        }
        let size = match dir_size(&dir) {
            Ok(size) => size,
            Err(error) => {
                writeln!(out, "kept {}: {}", record.run_id, ErrorChain(&error))?;
                continue;
            }
        };
        writeln!(
            out,
            "{} {} {} created {}",
            if args.dry_run {
                "would remove"
            } else {
                "removing"
            },
            record.run_id,
            human_bytes(size),
            record.created_at
        )?;
        runs.push(Doomed {
            label: record.run_id.clone(),
            dir,
            size,
            run: true,
        });
    }
    let jobs = if args.jobs {
        gc_jobs(ctx, args, cutoff, out).await?
    } else {
        Vec::new()
    };
    let counted = |runs: usize, jobs: usize| {
        if args.jobs {
            format!("{runs} runs and {jobs} jobs")
        } else {
            format!("{runs} runs")
        }
    };
    if args.dry_run {
        let total: u64 = runs.iter().chain(&jobs).map(|doomed| doomed.size).sum();
        writeln!(
            out,
            "dry run: {}, {} would be reclaimed",
            counted(runs.len(), jobs.len()),
            human_bytes(total)
        )?;
        return Ok(());
    }
    let (mut removed_runs, mut removed_jobs, mut reclaimed) = (0, 0, 0);
    let mut failures = Vec::new();
    for doomed in runs.iter().chain(&jobs) {
        if doomed.run && running_elsewhere(&doomed.dir) {
            writeln!(out, "kept {}: a process is running it", doomed.label)?;
            continue;
        }
        match std::fs::remove_dir_all(&doomed.dir) {
            Ok(()) => {
                reclaimed += doomed.size;
                if doomed.run {
                    removed_runs += 1;
                } else {
                    removed_jobs += 1;
                }
            }
            Err(error) => failures.push(format!(
                "could not remove {}: {error}",
                doomed.dir.display()
            )),
        }
    }
    tracing::warn!(
        runs = removed_runs,
        jobs = removed_jobs,
        bytes = reclaimed,
        failed = failures.len(),
        "eval gc removed directories; their documents stay"
    );
    writeln!(
        out,
        "reclaimed {} from {}; their documents stay",
        human_bytes(reclaimed),
        counted(removed_runs, removed_jobs)
    )?;
    for failure in &failures {
        writeln!(out, "{failure}")?;
    }
    anyhow::ensure!(
        failures.is_empty(),
        "eval gc could not remove {} of the directories it chose",
        failures.len()
    );
    Ok(())
}

struct Doomed {
    label: String,
    dir: PathBuf,
    size: u64,
    run: bool,
}

async fn gc_jobs(
    ctx: &EvalContext,
    args: &EvalGcArgs,
    cutoff: chrono::DateTime<chrono::Utc>,
    out: &mut dyn Write,
) -> Result<Vec<Doomed>> {
    let mut doomed = Vec::new();
    for job_id in gents::optimization::job_ids(&ctx.access, &ctx.owner).await? {
        let Some(job) = gents::optimization::load_job(&ctx.access, &ctx.owner, &job_id).await?
        else {
            continue;
        };
        if args
            .definition
            .as_deref()
            .is_some_and(|id| id != job.origin.definition.definition_id)
        {
            continue;
        }
        if gents::optimization::validate_job_id(&job.job_id).is_err() {
            writeln!(
                out,
                "kept job {:?}: its id is not one ordinary path component",
                job.job_id
            )?;
            continue;
        }
        if !gents::optimization::job_dir(&job.origin.jobs_dir, &job.job_id).is_dir() {
            continue;
        }
        let dir = match job_dir_in_this_home(ctx, &job) {
            Ok(dir) => dir,
            Err(error) => {
                writeln!(out, "kept {error}")?;
                continue;
            }
        };
        let modified: chrono::DateTime<chrono::Utc> =
            match std::fs::metadata(&dir).and_then(|metadata| metadata.modified()) {
                Ok(modified) => modified.into(),
                Err(error) => {
                    writeln!(
                        out,
                        "kept job {}: reading {}: {error}",
                        job.job_id,
                        dir.display()
                    )?;
                    continue;
                }
            };
        if modified > cutoff {
            continue;
        }
        let state = gents::optimization::derive_state(&job.journal);
        if !gents::optimization::removable(&state) {
            writeln!(out, "kept job {}: {}", job.job_id, state.label())?;
            continue;
        }
        let size = match dir_size(&dir) {
            Ok(size) => size,
            Err(error) => {
                writeln!(out, "kept job {}: {}", job.job_id, ErrorChain(&error))?;
                continue;
            }
        };
        writeln!(
            out,
            "{} job {} {}",
            if args.dry_run {
                "would remove"
            } else {
                "removing"
            },
            job.job_id,
            human_bytes(size)
        )?;
        doomed.push(Doomed {
            label: format!("job {}", job.job_id),
            dir,
            size,
            run: false,
        });
    }
    Ok(doomed)
}

/// The job's directory, `<jobs_dir>/<job_id>` as its origin recorded it,
/// when that jobs directory is this home's. The path comes from a document
/// another home may have written, so both sides are canonicalized and
/// compared before anything under it is deleted.
pub(crate) fn job_dir_in_this_home(
    ctx: &EvalContext,
    job: &gents::optimization::JobRecord,
) -> Result<PathBuf> {
    let home_jobs = std::fs::canonicalize(ctx.jobs_dir()).ok();
    anyhow::ensure!(
        home_jobs.is_some() && std::fs::canonicalize(&job.origin.jobs_dir).ok() == home_jobs,
        "job {}: its directory is not under this home",
        job.job_id
    );
    Ok(gents::optimization::job_dir(
        &job.origin.jobs_dir,
        &job.job_id,
    ))
}

pub(crate) fn dir_size(path: &Path) -> Result<u64, EvalCommandError> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|source| EvalCommandError::ReadPath {
            path: path.to_owned(),
            source,
        })?;
    if !metadata.is_dir() {
        return Ok(metadata.len());
    }
    let entries = std::fs::read_dir(path).map_err(|source| EvalCommandError::ListPath {
        path: path.to_owned(),
        source,
    })?;
    let mut total = 0;
    for entry in entries {
        total += below(entry.map(|entry| entry.path()))?;
    }
    Ok(total)
}

fn below(entry: std::io::Result<PathBuf>) -> Result<u64, EvalCommandError> {
    let measured = match entry {
        Ok(path) => dir_size(&path),
        Err(error) => Err(EvalCommandError::Io(error)),
    };
    match measured {
        Err(error) if is_not_found(&error) => Ok(0),
        other => other,
    }
}

fn is_not_found(error: &EvalCommandError) -> bool {
    matches!(
        error,
        EvalCommandError::Io(source)
            | EvalCommandError::ReadPath { source, .. }
            | EvalCommandError::ListPath { source, .. }
            if source.kind() == std::io::ErrorKind::NotFound
    )
}
