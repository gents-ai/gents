//! `cancel`, `invalidate`, `rm` and `gc`: the operator's acts on a run.
//! Documents stay in every case: a marker is not a document, invalidation is
//! the run's one write-once mutation, and `rm` and `gc` delete only
//! directories.

use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gents::eval::report::load_runs;
use gents::eval::runner::{request_cancel, run_dir, run_finished, running_elsewhere, slots_owed};
use gents::eval::{invalidate_run, load_run, load_trials};
use gents::ConfigAccess;

use super::render::human_bytes;
use super::EvalContext;
use crate::cli::{EvalGcArgs, EvalInvalidateArgs, EvalRmArgs, EvalRunIdArgs};

/// Write the marker, then look up the run's purpose for the job note.
///
/// The marker needs no database, so it is written first: the process
/// hosting the run may hold the home's embedded node, and `purpose` (which
/// opens it) is awaited only afterwards. A failed lookup is logged and the
/// cancel still succeeds; only the job note is skipped. A job's run is
/// cancelled like any other, and the job stops at it.
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

/// Write the marker and say what it does: a live process stops launching at
/// its next check; with none, the next run or resume of the id clears it.
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

/// The run's origin purpose, `None` when the home has no such run.
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
        // Finished means nothing owed and no live process on it.
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
        // A job that still needs the run keeps it.
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
    // A process may have started on the run while the checks above read the
    // database and sized the directory: look again right before deleting.
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

/// Delete the directories of runs created before the threshold that no
/// optimization job references and that are finished (nothing owed, no live
/// process on them); with `--jobs`, also the directories of settled jobs last
/// modified before it. Each kept old run or job is printed with why. Nothing
/// is deleted on its own and no time-to-live is stored. A directory that
/// cannot be removed is reported and the rest still go; the command then
/// fails.
pub(super) async fn gc(ctx: &EvalContext, args: &EvalGcArgs, out: &mut dyn Write) -> Result<()> {
    // `parse_age` refuses an age this cannot represent; this is the backstop.
    let cutoff = chrono::Duration::from_std(args.older_than)
        .ok()
        .and_then(|age| chrono::Utc::now().checked_sub_signed(age))
        .context("--older-than reaches before the earliest representable time")?;
    // Runs first, references second: a job's driver journals RunStarted
    // before it freezes the run, so any run in this list that a job is
    // starting is already referenced when the references are read. The
    // other order could list a run whose reference landed in between.
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
                writeln!(out, "kept {}: {error:#}", record.run_id)?;
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
        // A run resumed since it was judged finished is not deleted from
        // under its loop.
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

/// A directory `gc` chose, with what it frees.
struct Doomed {
    /// The run id, or `job <id>`.
    label: String,
    dir: PathBuf,
    size: u64,
    run: bool,
}

/// The directories of this home's jobs that `gents::optimization::removable`
/// accepts, of `--definition`'s definition when given, last modified at or
/// before `cutoff`. A job document has no creation time, so its directory's
/// modification time stands in for its age.
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
        // The driver's job-id rule: one ordinary path component.
        if job.job_id.trim().is_empty() || run_dir(&job.origin.jobs_dir, &job.job_id).is_err() {
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
                writeln!(out, "kept job {}: {error:#}", job.job_id)?;
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
/// compared before anything under it is deleted: another home's job
/// directory is that home's to remove. The refusal reads `job <id>: its
/// directory is not under this home`.
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

/// Bytes under `path`, not following symbolic links. An entry that vanishes
/// during the walk (a live loop replacing `progress.json`, a trial home being
/// cleaned) counts as nothing; any other failure is the caller's.
pub(crate) fn dir_size(path: &Path) -> Result<u64> {
    let metadata =
        std::fs::symlink_metadata(path).with_context(|| format!("reading {}", path.display()))?;
    if !metadata.is_dir() {
        return Ok(metadata.len());
    }
    let mut total = 0;
    for entry in std::fs::read_dir(path).with_context(|| format!("listing {}", path.display()))? {
        total += below(entry.map(|entry| entry.path()))?;
    }
    Ok(total)
}

/// One entry's bytes for [`dir_size`]; `0` when it is already gone.
fn below(entry: std::io::Result<PathBuf>) -> Result<u64> {
    let gone = |error: &anyhow::Error| {
        error
            .chain()
            .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
            .any(|error| error.kind() == std::io::ErrorKind::NotFound)
    };
    match entry
        .map_err(anyhow::Error::from)
        .and_then(|path| dir_size(&path))
    {
        Err(error) if gone(&error) => Ok(0),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};

    use gents::eval::checks::CheckRegistry;
    use gents::eval::runner::{
        run, Capture, Isolation, ScriptedExecutor, TrialEvidence, TrialExecutor, TrialLocator,
        TrialSpec, CANCEL_MARKER,
    };
    use tokio_util::sync::CancellationToken;

    use super::super::testing::{eval, eval_command, executor, fast, row, Fixture};
    use super::{cancel, write_marker};
    use crate::commands::optimization::testing::accepted_job;

    /// Runs `gents eval cancel` for its run the first time it executes a
    /// trial: a second process cancelling a loop this test is running.
    struct CancelsFromAnotherProcess {
        inner: ScriptedExecutor,
        runs_dir: PathBuf,
        run_id: String,
        done: AtomicBool,
    }

    #[async_trait::async_trait]
    impl TrialExecutor for CancelsFromAnotherProcess {
        fn isolation(&self) -> Isolation {
            self.inner.isolation()
        }

        fn wants_script_key(&self) -> bool {
            self.inner.wants_script_key()
        }

        async fn provision(&self, spec: &TrialSpec) -> TrialLocator {
            self.inner.provision(spec).await
        }

        async fn execute(
            &self,
            spec: &TrialSpec,
            cancel_token: CancellationToken,
        ) -> TrialEvidence {
            if !self.done.swap(true, Ordering::SeqCst) {
                let mut confirmation = Vec::new();
                write_marker(&self.runs_dir, &self.run_id, &mut confirmation).unwrap();
                assert!(String::from_utf8(confirmation)
                    .unwrap()
                    .contains("stops launching at its next check"));
            }
            self.inner.execute(spec, cancel_token).await
        }

        async fn recollect(
            &self,
            at: &TrialLocator,
            captures: &[Capture],
        ) -> Option<TrialEvidence> {
            self.inner.recollect(at, captures).await
        }
    }

    #[tokio::test]
    async fn cancel_is_observed_by_a_loop_running_in_the_test() {
        let fixture = Fixture::new().await;
        let executor = CancelsFromAnotherProcess {
            inner: executor(&[]),
            runs_dir: fixture.ctx.runs_dir(),
            run_id: "r-cancel".into(),
            done: AtomicBool::new(false),
        };
        let outcome = run(
            &fixture.ctx.access,
            &fixture.request("r-cancel"),
            &executor,
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &fast(),
        )
        .await
        .unwrap();
        assert_eq!((outcome.completed, outcome.abandoned), (1, 0));
        assert!(fixture
            .ctx
            .runs_dir()
            .join("r-cancel")
            .join(CANCEL_MARKER)
            .exists());
        let table = eval(&fixture, &["show", "r-cancel"]).await.unwrap();
        assert_eq!(row(&table, "baseline", 10)[6], "11", "{table}");
        assert_eq!(row(&table, "candidate", 10)[6], "12", "{table}");

        let error = eval(&fixture, &["cancel", "absent"]).await.unwrap_err();
        assert!(
            error.to_string().starts_with("run absent has no directory"),
            "{error:#}"
        );
    }

    /// Cancelling a job's run proceeds and says the job stops there.
    #[tokio::test]
    async fn cancelling_a_jobs_run_says_the_job_stops_at_it() {
        let fixture = Fixture::new().await;
        let mut request = fixture.request("r-job");
        request.purpose = "optimization:job-9".into();
        let interrupted = CancellationToken::new();
        interrupted.cancel();
        run(
            &fixture.ctx.access,
            &request,
            &executor(&[]),
            &CheckRegistry::builtin(),
            interrupted,
            &fast(),
        )
        .await
        .unwrap();
        let said = eval(&fixture, &["cancel", "r-job"]).await.unwrap();
        assert!(
            said.contains("this run belongs to job job-9; the job stops at this run"),
            "{said}"
        );
        assert!(fixture
            .ctx
            .runs_dir()
            .join("r-job")
            .join(CANCEL_MARKER)
            .exists());
    }

    /// The home's node may be locked by the process hosting the run: the
    /// marker is written first, and a failed purpose lookup only skips the
    /// job note. A run no process is hosting says the next run clears it.
    #[tokio::test]
    async fn cancel_writes_the_marker_when_the_home_cannot_be_opened() {
        let fixture = Fixture::new().await;
        let interrupted = CancellationToken::new();
        interrupted.cancel();
        fixture
            .scripted_run("r-locked", &executor(&[]), interrupted)
            .await;
        let runs_dir = fixture.ctx.runs_dir();
        let marker = runs_dir.join("r-locked").join(CANCEL_MARKER);
        let mut out = Vec::new();
        cancel(
            &runs_dir,
            "r-locked",
            async {
                assert!(marker.exists(), "the marker is written before any lookup");
                Err(anyhow::anyhow!("the embedded store is locked"))
            },
            &mut out,
        )
        .await
        .unwrap();
        let said = String::from_utf8(out).unwrap();
        assert!(marker.exists());
        assert!(
            said.contains(
                "no process is running run r-locked now, and the next `gents eval run --run-id r-locked` or `gents eval resume r-locked` clears the marker"
            ),
            "{said}"
        );
        assert!(
            said.contains("could not read run r-locked's documents to check whether it belongs to an optimization job"),
            "{said}"
        );
    }

    #[tokio::test]
    async fn invalidate_confirms_once_and_then_prints_already_invalidated() {
        let fixture = Fixture::new().await;
        fixture
            .scripted_run("r1", &executor(&[]), CancellationToken::new())
            .await;
        let confirmed = eval(&fixture, &["invalidate", "r1", "--reason", "grader bug"])
            .await
            .unwrap();
        assert_eq!(
            confirmed.trim_end(),
            format!("invalidated run r1 as {}: grader bug", fixture.ctx.owner)
        );
        let again = eval(&fixture, &["invalidate", "r1", "--reason", "twice"])
            .await
            .unwrap_err();
        assert_eq!(
            again.to_string(),
            "eval run invalidation is write-once and is already set"
        );
    }

    #[tokio::test]
    async fn rm_refuses_an_unfinished_run_then_forces_and_keeps_the_documents() {
        let fixture = Fixture::new().await;
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        fixture
            .scripted_run("r-stopped", &executor(&[]), cancelled)
            .await;
        let dir = fixture.ctx.runs_dir().join("r-stopped");
        assert!(dir.is_dir());

        let refused = eval(&fixture, &["rm", "r-stopped"]).await.unwrap_err();
        assert!(
            refused
                .to_string()
                .contains("is unfinished: it still owes 24 slots"),
            "{refused:#}"
        );
        assert!(dir.is_dir(), "a refusal deletes nothing");

        let removed = eval(&fixture, &["rm", "r-stopped", "--force"])
            .await
            .unwrap();
        assert!(removed.contains("bytes reclaimed"), "{removed}");
        assert!(!dir.exists());
        assert!(
            gents::eval::load_run(&fixture.ctx.access, &fixture.ctx.owner, "r-stopped")
                .await
                .unwrap()
                .is_some(),
            "the run's documents stay"
        );

        fixture
            .scripted_run("r-done", &executor(&[]), CancellationToken::new())
            .await;
        // A finished run with a slot a live process is running is kept.
        let done = fixture.ctx.runs_dir().join("r-done");
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let held = serde_json::json!({"slots": {"trial-1": {
            "cell_id": "baseline",
            "case_id": "val-a",
            "trial_index": 0,
            "attempt": 2,
            "stage_id": null,
            "started_at": now,
            "pid": std::process::id(),
            "written_at": now,
        }}});
        let progress = done.join(gents::eval::runner::PROGRESS_FILE);
        std::fs::write(&progress, serde_json::to_vec(&held).unwrap()).unwrap();
        let refused = eval(&fixture, &["rm", "r-done"]).await.unwrap_err();
        assert!(
            refused
                .to_string()
                .contains("is being run by a live process right now"),
            "{refused:#}"
        );
        assert!(done.is_dir(), "a refusal deletes nothing");
        std::fs::remove_file(&progress).unwrap();

        let removed = eval(&fixture, &["rm", "r-done"]).await.unwrap();
        assert!(removed.starts_with("removed "), "{removed}");
    }

    #[tokio::test]
    async fn gc_removes_only_old_finished_unreferenced_run_directories() {
        let fixture = Fixture::new().await;
        fixture
            .scripted_run("r-done", &executor(&[]), CancellationToken::new())
            .await;
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        fixture
            .scripted_run("r-stopped", &executor(&[]), cancelled)
            .await;
        accepted_job(&fixture, "job-1").await;
        let referenced =
            gents::optimization::referenced_run_ids(&fixture.ctx.access, &fixture.ctx.owner)
                .await
                .unwrap();
        assert!(!referenced.is_empty());

        let young = eval(&fixture, &["gc"]).await.unwrap();
        assert_eq!(
            young.trim_end(),
            "reclaimed 0B from 0 runs; their documents stay"
        );

        let dry = eval(&fixture, &["gc", "--older-than", "0s", "--dry-run"])
            .await
            .unwrap();
        assert!(dry.contains("would remove r-done "), "{dry}");
        assert!(
            dry.contains("kept r-stopped: unfinished (it still owes 24 slots)"),
            "{dry}"
        );
        for run_id in &referenced {
            assert!(
                dry.contains(&format!("kept {run_id}: an optimization job references it")),
                "{dry}"
            );
        }
        assert!(dry.contains("dry run: 1 runs, "), "{dry}");
        assert!(fixture.ctx.runs_dir().join("r-done").is_dir());

        let other = eval(
            &fixture,
            &[
                "gc",
                "--older-than",
                "0s",
                "--definition",
                "other",
                "--dry-run",
            ],
        )
        .await
        .unwrap();
        assert!(other.starts_with("dry run: 0 runs"), "{other}");

        let removed = eval(&fixture, &["gc", "--older-than", "0s"]).await.unwrap();
        assert!(
            removed.contains(" from 1 runs; their documents stay"),
            "{removed}"
        );
        assert!(!fixture.ctx.runs_dir().join("r-done").exists());
        assert!(fixture.ctx.runs_dir().join("r-stopped").is_dir());
        for run_id in &referenced {
            assert!(fixture.ctx.runs_dir().join(run_id).is_dir(), "{run_id}");
        }
        assert!(
            gents::eval::load_run(&fixture.ctx.access, &fixture.ctx.owner, "r-done")
                .await
                .unwrap()
                .is_some(),
            "documents stay"
        );
        assert!(
            fixture.ctx.jobs_dir().join("job-1").is_dir(),
            "no --jobs, no job"
        );

        // A job waiting to be promoted keeps its directory.
        let kept = eval(
            &fixture,
            &["gc", "--older-than", "0s", "--jobs", "--dry-run"],
        )
        .await
        .unwrap();
        assert!(kept.contains("kept job job-1: ready_to_promote"), "{kept}");

        // --definition narrows the jobs as it narrows the runs.
        let narrowed = eval(
            &fixture,
            &[
                "gc",
                "--older-than",
                "0s",
                "--jobs",
                "--definition",
                "other",
                "--dry-run",
            ],
        )
        .await
        .unwrap();
        assert_eq!(
            narrowed.trim_end(),
            "dry run: 0 runs and 0 jobs, 0B would be reclaimed"
        );

        // Another home sharing the documents does not collect this home's
        // job directory.
        let gents::ConfigAccess::Local(node) = &*fixture.ctx.access else {
            panic!("the fixture is embedded");
        };
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(elsewhere.path().join("eval").join("jobs")).unwrap();
        let other_home = super::super::EvalContext {
            access: gents::ConfigAccess::Local(node.clone()).into(),
            home_dir: elsewhere.path().to_path_buf(),
            owner: fixture.ctx.owner.clone(),
        };
        let crate::cli::EvalCommand::Gc(args) =
            eval_command(&["gc", "--older-than", "0s", "--jobs", "--dry-run"])
        else {
            panic!("not gc");
        };
        let mut foreign = Vec::new();
        super::gc(&other_home, &args, &mut foreign).await.unwrap();
        let foreign = String::from_utf8(foreign).unwrap();
        assert!(
            foreign.contains("kept job job-1: its directory is not under this home"),
            "{foreign}"
        );

        // Promoted, then reverted: collected.
        let view = gents::optimization::show(&fixture.ctx.access, &fixture.ctx.owner, "job-1")
            .await
            .unwrap();
        let digest = view.checkpoint.expect("a retained checkpoint").pack_digest;
        let promotion = gents::optimization::promote(
            &fixture.ctx.access,
            &fixture.ctx.owner,
            "job-1",
            &digest,
            &fixture.ctx.owner,
        )
        .await
        .unwrap();
        gents::optimization::revert(
            &fixture.ctx.access,
            &fixture.ctx.owner,
            "job-1",
            &promotion.target_digest,
            &fixture.ctx.owner,
        )
        .await
        .unwrap();
        let jobs = eval(
            &fixture,
            &["gc", "--older-than", "0s", "--jobs", "--dry-run"],
        )
        .await
        .unwrap();
        assert!(jobs.contains("would remove job job-1 "), "{jobs}");
        assert!(jobs.contains("dry run: 0 runs and 1 jobs, "), "{jobs}");
        let collected = eval(&fixture, &["gc", "--older-than", "0s", "--jobs"])
            .await
            .unwrap();
        assert!(
            collected.contains(" from 0 runs and 1 jobs; their documents stay"),
            "{collected}"
        );
        assert!(!fixture.ctx.jobs_dir().join("job-1").exists());
        for run_id in &referenced {
            assert!(fixture.ctx.runs_dir().join(run_id).is_dir(), "{run_id}");
        }
    }

    /// Makes `dir` unreadable and unwritable until dropped.
    #[cfg(unix)]
    struct Sealed(PathBuf);

    #[cfg(unix)]
    impl Sealed {
        fn new(dir: PathBuf, mode: u32) -> Self {
            use std::os::unix::fs::PermissionsExt;
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("inner"), b"x").unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();
            Self(dir)
        }
    }

    #[cfg(unix)]
    impl Drop for Sealed {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }

    /// A directory that cannot be removed is reported, the others still go,
    /// and the command fails; a run whose size cannot be read is kept.
    #[cfg(unix)]
    #[tokio::test]
    async fn gc_continues_past_a_directory_it_cannot_remove_and_keeps_one_it_cannot_read() {
        // Root ignores directory permissions, so nothing here is sealed.
        // SAFETY: `geteuid` has no preconditions and cannot fail.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let fixture = Fixture::new().await;
        for run_id in ["r-a", "r-b", "r-c"] {
            fixture
                .scripted_run(run_id, &executor(&[]), CancellationToken::new())
                .await;
        }
        let runs_dir = fixture.ctx.runs_dir();
        // Readable, but its entry cannot be unlinked: r-a is measured and
        // chosen, and its removal fails.
        let _stuck = Sealed::new(runs_dir.join("r-a").join("stuck"), 0o555);
        // Unreadable: r-c cannot be measured, so it is kept.
        let _unreadable = Sealed::new(runs_dir.join("r-c").join("unreadable"), 0o000);

        let listed = eval(&fixture, &["list"]).await.unwrap();
        let r_c = listed
            .lines()
            .find(|line| line.starts_with("r-c "))
            .unwrap_or_else(|| panic!("{listed}"));
        assert_eq!(r_c.split_whitespace().nth(6), Some("?"), "{r_c}");

        let mut out = Vec::new();
        let crate::cli::EvalCommand::Gc(args) = eval_command(&["gc", "--older-than", "0s"]) else {
            panic!("not gc");
        };
        let error = super::gc(&fixture.ctx, &args, &mut out).await.unwrap_err();
        let said = String::from_utf8(out).unwrap();
        assert_eq!(
            error.to_string(),
            "eval gc could not remove 1 of the directories it chose"
        );
        assert!(said.contains("kept r-c: listing "), "{said}");
        assert!(
            said.contains(" from 1 runs; their documents stay"),
            "{said}"
        );
        assert!(
            said.contains(&format!(
                "could not remove {}: ",
                runs_dir.join("r-a").display()
            )),
            "{said}"
        );
        assert!(!runs_dir.join("r-b").exists(), "the rest still go");
        assert!(runs_dir.join("r-c").is_dir());
    }

    #[test]
    fn a_size_walk_counts_an_entry_that_vanished_as_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("kept"), b"12345").unwrap();
        let gone = dir.path().join("gone");
        assert_eq!(super::below(Ok(gone.clone())).unwrap(), 0);
        assert_eq!(
            super::below(Err(std::io::Error::from(std::io::ErrorKind::NotFound))).unwrap(),
            0
        );
        assert!(
            super::below(Err(std::io::Error::from(
                std::io::ErrorKind::PermissionDenied
            )))
            .is_err(),
            "any other failure is the caller's"
        );
        assert_eq!(super::dir_size(dir.path()).unwrap(), 5);
        assert!(
            super::dir_size(&gone).is_err(),
            "the root itself must exist"
        );
    }

    #[test]
    fn gc_older_than_refuses_an_age_no_cutoff_can_be_taken_from() {
        use clap::Parser;
        let parse = |age: &str| {
            crate::cli::Cli::try_parse_from(["gents", "eval", "gc", "--older-than", age])
        };
        assert!(parse("36500d").is_ok());
        let error = parse("200000000d").err().expect("refused");
        assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
        assert!(crate::cli::parse_age("9999999999999d").is_err());
    }

    #[test]
    fn gc_older_than_takes_zero() {
        use clap::Parser;
        let parse = |age: &str| {
            crate::cli::Cli::try_parse_from(["gents", "eval", "gc", "--older-than", age])
        };
        assert!(parse("0s").is_ok());
        assert!(parse("14d").is_ok());
        assert!(parse("soon").is_err());
        assert_eq!(
            crate::cli::parse_age("0s").unwrap(),
            std::time::Duration::ZERO
        );
    }

    #[tokio::test]
    async fn list_shows_each_runs_size_on_disk() {
        let fixture = Fixture::new().await;
        fixture
            .scripted_run("r1", &executor(&[]), CancellationToken::new())
            .await;
        let size = |listed: &str| {
            listed
                .lines()
                .find(|line| line.starts_with("r1 "))
                .and_then(|line| line.split_whitespace().nth(6))
                .map(str::to_owned)
                .unwrap_or_else(|| panic!("{listed}"))
        };
        let before = size(&eval(&fixture, &["list"]).await.unwrap());
        assert!(before.ends_with('B') && before != "-", "{before}");
        eval(&fixture, &["rm", "r1"]).await.unwrap();
        assert_eq!(size(&eval(&fixture, &["list"]).await.unwrap()), "-");
    }
}
