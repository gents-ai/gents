use std::future::Future;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use gents::eval::report::{load_report, report_refused};
use gents::eval::runner::{
    read_progress, read_run_view, run_dir, run_finished, run_view, slots_owed, RunView,
    STALE_WINDOW,
};
use gents::eval::{load_run, load_trials};

use super::frame::{self, Heading, Screen};
use super::{render, EvalContext};
use crate::cli::EvalWatchArgs;

pub(crate) enum Documents<'a> {
    Open(&'a EvalContext),
    Reopened(EvalContext),
    Locked(String),
}

/// Whether `error` is the store refusing a second opener: another process
/// (or this one) holds it. The texts are regolith's (`env/db_lock.rs`);
/// they arrive only as text through the node builder's error.
pub(crate) fn store_locked(error: &anyhow::Error) -> bool {
    if error
        .downcast_ref::<gents::home::HomeError>()
        .and_then(gents::home::HomeError::lock_held)
        .is_some()
    {
        return true;
    }
    const LOCKED: [&str; 2] = [
        "database directory is already locked for read-write access",
        "database directory is already open in this process",
    ];
    let text = format!("{error:#}");
    LOCKED.iter().any(|locked| text.contains(locked))
}

pub(crate) async fn reopen<F, Fut>(resolve: F) -> Result<Documents<'static>>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<EvalContext>>,
{
    match resolve().await {
        Ok(ctx) => Ok(Documents::Reopened(ctx)),
        Err(error) if store_locked(&error) => Ok(Documents::Locked(format!("{error:#}"))),
        Err(error) => Err(error),
    }
}

fn runner_alive(dir: &Path) -> bool {
    #[cfg(unix)]
    {
        use gents::eval::runner::host_alive;
        read_progress(dir).is_some_and(|progress| {
            progress
                .holder
                .as_ref()
                .is_some_and(|holder| host_alive(holder.pid))
                || progress.slots.values().any(|slot| host_alive(slot.pid))
        })
    }
    #[cfg(not(unix))]
    gents::eval::runner::running_elsewhere(dir)
}

pub(crate) async fn watch<'a, F, Fut>(
    runs_dir: &Path,
    args: &EvalWatchArgs,
    screen: Option<Screen>,
    mut open: F,
    out: &mut dyn Write,
) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Documents<'a>>>,
{
    let dir = run_dir(runs_dir, &args.run_id)?;
    let mut said_unavailable = false;
    let mut said_held = false;
    loop {
        let from_runner = runner_alive(&dir).then(|| read_run_view(&dir)).flatten();
        let documents = match from_runner {
            Some(_) => {
                if !said_held {
                    tracing::debug!(run_id = %args.run_id, "node held, reading runner files");
                    said_held = true;
                }
                None
            }
            None => Some(open().await?),
        };
        if let Some(Documents::Locked(reason)) = &documents {
            anyhow::ensure!(
                dir.is_dir(),
                "run {} has no directory {} and the home's documents cannot be read: {reason}",
                args.run_id,
                dir.display()
            );
        }
        let progress = read_progress(&dir);
        let now = chrono::Utc::now();
        let (view, unavailable) = match (&from_runner, &documents) {
            (Some(view), _) => (Some(view.clone()), None),
            (None, Some(Documents::Open(ctx))) => view(ctx, runs_dir, &args.run_id, &dir).await?,
            (None, Some(Documents::Reopened(ctx))) => {
                view(ctx, runs_dir, &args.run_id, &dir).await?
            }
            (None, Some(Documents::Locked(reason))) => (None, Some(reason.clone())),
            (None, None) => (None, None),
        };
        if let Some(screen) = screen {
            let heading = Heading {
                run_id: args.run_id.clone(),
                definition: frozen_definition(&dir),
                note: unavailable
                    .as_ref()
                    .map(|_| "report unavailable while the runner holds the home".to_owned()),
            };
            let lines = frame::frame(&heading, view.as_ref(), progress.as_ref(), now, screen);
            frame::draw(&lines, out)?;
        } else if args.json {
            let frame = serde_json::json!({
                "at": now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                "source": if from_runner.is_some() { "runner" } else if view.is_some() { "node" } else { "progress" },
                "report_unavailable": unavailable,
                "progress": progress,
                "view": view,
            });
            writeln!(out, "{frame}")?;
        } else {
            writeln!(
                out,
                "--- {}",
                now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            )?;
            if let Some(runner) = &from_runner {
                writeln!(out, "live from runner pid {}", runner.pid)?;
            }
            if let Some(view) = &view {
                render::report_table(&view.report, out)?;
            }
            if let Some(reason) = &unavailable {
                if said_unavailable {
                    writeln!(out, "report unavailable; showing in-flight slots only")?;
                } else {
                    writeln!(
                        out,
                        "report unavailable: {reason}; showing in-flight slots only"
                    )?;
                    said_unavailable = true;
                }
            }
            render::in_flight(progress.as_ref(), now, out)?;
            if let Some(view) = &view {
                render::finished(view, FINISHED_SHOWN, out)?;
            }
        }
        out.flush()?;
        if args.once {
            return Ok(());
        }
        let documents = match documents {
            Some(documents) => documents,
            None if runner_alive(&dir) => {
                tokio::time::sleep(args.interval).await;
                continue;
            }
            None => open().await?,
        };
        if !runner_alive(&dir) {
            if let Documents::Open(_) = &documents {
                tokio::time::sleep(args.interval.min(STALE_WINDOW)).await;
                if runner_alive(&dir) {
                    continue;
                }
            }
            let mut documents = match documents {
                Documents::Locked(_) => open().await?,
                opened => opened,
            };
            let grace = STALE_WINDOW.max(args.interval.saturating_mul(3));
            let deadline = tokio::time::Instant::now().checked_add(grace);
            let mut held_again = false;
            while let Documents::Locked(reason) = &documents {
                if runner_alive(&dir) {
                    held_again = true;
                    break;
                }
                if deadline.is_none_or(|deadline| tokio::time::Instant::now() >= deadline) {
                    anyhow::bail!("stopped: report unavailable: {reason}");
                }
                tokio::time::sleep(args.interval).await;
                documents = open().await?;
            }
            if held_again {
                continue;
            }
            let ctx = match &documents {
                Documents::Open(ctx) => *ctx,
                Documents::Reopened(ctx) => ctx,
                Documents::Locked(reason) => {
                    anyhow::bail!("stopped: report unavailable: {reason}")
                }
            };
            return ending(ctx, &args.run_id, &dir, args.json, out).await;
        }
        drop(documents);
        tokio::time::sleep(args.interval).await;
    }
}

pub(crate) fn frame_screen() -> Screen {
    Screen::of_stdout()
}

fn frozen_definition(dir: &Path) -> Option<(String, i64)> {
    let bytes = std::fs::read(dir.join(gents::eval::runner::DEFINITION_FILE)).ok()?;
    let definition: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    Some((
        definition.get("definition_id")?.as_str()?.to_owned(),
        definition.get("comparability_version")?.as_i64()?,
    ))
}

const FINISHED_SHOWN: usize = 10;

async fn view(
    ctx: &EvalContext,
    runs_dir: &Path,
    run_id: &str,
    dir: &Path,
) -> Result<(Option<RunView>, Option<String>)> {
    match load_report(&ctx.access, &ctx.owner, runs_dir, run_id).await {
        Ok(report) => Ok((Some(run_view(report, dir)), None)),
        Err(error) => match report_refused(error.as_ref()) {
            Some(refusal) => Ok((None, Some(refusal.to_string()))),
            None => Err(error),
        },
    }
}

async fn ending(
    ctx: &EvalContext,
    run_id: &str,
    dir: &Path,
    json: bool,
    out: &mut dyn Write,
) -> Result<()> {
    let record = load_run(&ctx.access, &ctx.owner, run_id)
        .await?
        .with_context(|| format!("no eval run {run_id:?} for {}", ctx.owner))?;
    let trials = load_trials(&ctx.access, &ctx.owner, run_id).await?;
    anyhow::ensure!(
        run_finished(&record, &trials, dir),
        "stopped: {} slots owed (gents eval resume {run_id})",
        slots_owed(&record, &trials)
    );
    if !json {
        writeln!(out, "finished")?;
    }
    Ok(())
}
