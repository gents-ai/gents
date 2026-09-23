//! `cancel`, `invalidate` and `rm`: the operator's three acts on a run.
//! Documents stay in every case: a marker is not a document, invalidation is
//! the run's one write-once mutation, and `rm` deletes only the directory.

use std::future::Future;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use gents::eval::runner::{request_cancel, run_dir, running_elsewhere, slots_owed};
use gents::eval::{invalidate_run, load_run, load_trials};
use gents::ConfigAccess;

use super::EvalContext;
use crate::cli::{EvalInvalidateArgs, EvalRmArgs, EvalRunIdArgs};

/// Write the marker, then look up the run's purpose for the job note.
///
/// The marker needs no database, so it is written first: the process
/// hosting the run may hold the home's embedded node, and `purpose` (which
/// opens it) is awaited only afterwards. A failed lookup is logged and the
/// cancel still succeeds; only the job note is skipped. A job's run is
/// cancelled like any other, and the job stops at it (F1).
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
    if !args.force {
        let record = load_run(&ctx.access, &ctx.owner, &args.run_id)
            .await?
            .with_context(|| format!("no eval run {:?} for {}", args.run_id, ctx.owner))?;
        let trials = load_trials(&ctx.access, &ctx.owner, &args.run_id).await?;
        // Ruling F7: finished means nothing owed and no live process on it.
        let owed = slots_owed(&record, &trials);
        anyhow::ensure!(
            owed == 0,
            "run {} is unfinished: it still owes {owed} slots; `gents eval resume {}` finishes it, or pass --force to delete its directory anyway",
            args.run_id,
            args.run_id
        );
        anyhow::ensure!(
            !running_elsewhere(&dir),
            "run {} is being run by a live process right now (see `gents eval watch {}`); pass --force to delete its directory anyway",
            args.run_id,
            args.run_id
        );
        // Ruling F8: a job that still needs the run keeps it.
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
    std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    tracing::warn!(run_id = %args.run_id, bytes, "eval run directory removed; its documents stay");
    writeln!(
        out,
        "removed {} ({bytes} bytes reclaimed); the run's documents stay",
        dir.display()
    )?;
    Ok(())
}

/// Bytes under `path`, not following symbolic links.
pub(crate) fn dir_size(path: &Path) -> Result<u64> {
    let metadata =
        std::fs::symlink_metadata(path).with_context(|| format!("reading {}", path.display()))?;
    if !metadata.is_dir() {
        return Ok(metadata.len());
    }
    let mut total = 0;
    for entry in std::fs::read_dir(path).with_context(|| format!("listing {}", path.display()))? {
        total += dir_size(&entry?.path())?;
    }
    Ok(total)
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

    use super::super::testing::{eval, executor, fast, row, Fixture};
    use super::{cancel, write_marker};

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

    /// Ruling F1: cancelling a job's run proceeds and says the job stops there.
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
        let removed = eval(&fixture, &["rm", "r-done"]).await.unwrap();
        assert!(removed.starts_with("removed "), "{removed}");
    }
}
