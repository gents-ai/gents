//! `watch`: the derived report for finished slots plus `progress.json` for
//! what is in flight (spec 4b §6). The loop writes the file; this reads it
//! without writing; no daemon, and the trial homes are never opened. No interrupt handler is
//! installed: Ctrl-C ends it.
//!
//! The process running the loop holds the home's embedded node, which opens
//! exclusively, so while a live process holds the run the watch reads the
//! files it leaves beside the run: `progress.json` for the slots in flight
//! with their live snapshots, and `report.json` for the report and the
//! finished slots (`gents::eval::runner::view`). The node is opened only
//! when no live process holds the run, or when a runner too old to write
//! `report.json` does; then it is tried again at every render.

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

/// The home's documents at one render.
pub(crate) enum Documents<'a> {
    /// Opened once, for the whole watch.
    Open(&'a EvalContext),
    /// Opened for this render only; dropped before the watch waits, so the
    /// watch never keeps the node from a process that wants it.
    Reopened(EvalContext),
    /// Another process holds the home's node: why.
    Locked(String),
}

/// Whether `error` is the store refusing a second opener: another process
/// (or this one) holds it.
///
/// A refusal reaches here typed when the home's own store claim caught it
/// first. The texts are regolith's (`env/db_lock.rs`) and cover the openers
/// that reach the backend without that claim; they arrive only as text
/// through the node builder's error.
pub(crate) fn store_locked(error: &anyhow::Error) -> bool {
    if error.downcast_ref::<gents::home::StoreLockHeld>().is_some() {
        return true;
    }
    const LOCKED: [&str; 2] = [
        "database directory is already locked for read-write access",
        "database directory is already open in this process",
    ];
    let text = format!("{error:#}");
    LOCKED.iter().any(|locked| text.contains(locked))
}

/// The home's documents as `resolve` opens them: `Locked` only when the
/// store's lock is held; any other failure is the watch's.
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

/// A delayed heartbeat makes the displayed snapshot stale, not the process
/// dead. Watching never takes ownership, so it can wait for a live local PID
/// without changing the runner's freshness rule for admission and cleanup.
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

/// Render until no live process holds the run, then say whether it finished
/// or how many slots it still owes. Only `finished` returns `Ok`: a run that
/// stopped owing slots, or whose report cannot be read, is an error, so the
/// command exits 1 and a script never reads a cancelled run as a success.
/// With `--once`, render once and return. `open` yields the home's documents
/// at each render.
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
        // While a live process holds the run, the view it leaves beside the
        // run is what the node would say, and the node is left alone.
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
                // The reason is said once: every later frame would repeat it.
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
            // The runner let go since this frame: its ending needs the node.
            None => open().await?,
        };
        if !runner_alive(&dir) {
            // The holder can lapse for a moment, as when one call over the
            // run hands it to the next: with the documents open, look once
            // more before ending, after an interval or the stale window,
            // whichever is shorter, and keep watching a run held again.
            if let Documents::Open(_) = &documents {
                tokio::time::sleep(args.interval.min(STALE_WINDOW)).await;
                if runner_alive(&dir) {
                    continue;
                }
            }
            // The host lets go of the run before it lets go of the node: it
            // still renders its own report and shuts the node down. A
            // locked node is tried again for a grace period, and a run held
            // again meanwhile is watched again.
            let mut documents = match documents {
                Documents::Locked(_) => open().await?,
                opened => opened,
            };
            // `parse_interval` caps the interval; saturating here is the
            // backstop, and a deadline past what an instant can hold ends
            // the grace at once rather than panicking.
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

/// The screen a terminal watch draws on.
pub(crate) fn frame_screen() -> Screen {
    Screen::of_stdout()
}

/// The definition id and version the run directory froze, read for its
/// heading only: the report verifies it, a frame does not need to.
fn frozen_definition(dir: &Path) -> Option<(String, i64)> {
    let bytes = std::fs::read(dir.join(gents::eval::runner::DEFINITION_FILE)).ok()?;
    let definition: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    Some((
        definition.get("definition_id")?.as_str()?.to_owned(),
        definition.get("comparability_version")?.as_i64()?,
    ))
}

/// How many finished slots a frame shows, latest first.
const FINISHED_SHOWN: usize = 10;

/// The run's view from the documents, or why its report cannot be built: a
/// refusal is shown, not fatal, since the in-flight slots are still worth
/// watching.
async fn view(
    ctx: &EvalContext,
    runs_dir: &Path,
    run_id: &str,
    dir: &Path,
) -> Result<(Option<RunView>, Option<String>)> {
    match load_report(&ctx.access, &ctx.owner, runs_dir, run_id).await {
        Ok(report) => Ok((Some(run_view(report, dir)), None)),
        Err(error) => match report_refused(&error) {
            Some(refusal) => Ok((None, Some(refusal.to_string()))),
            None => Err(error),
        },
    }
}

/// `finished`, or the error `stopped: N slots owed (…)`.
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use clap::Parser;
    use gents::eval::runner::{Holder, InFlight, Progress, PROGRESS_FILE};
    use tokio_util::sync::CancellationToken;

    use super::super::testing::{eval, eval_command, executor, row, Fixture, VALIDATION_CASES};
    use super::{reopen, store_locked, watch, Documents};
    use crate::cli::EvalWatchArgs;

    const LOCKED: &str = "building embedded DefraDB node from /h/data: database directory is already locked for read-write access: /h/data/LOCK";

    fn now_millis(seconds_ago: i64) -> String {
        (chrono::Utc::now() - chrono::Duration::seconds(seconds_ago))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    fn slot(case_id: &str, pid: u32, written_at: String) -> InFlight {
        InFlight {
            cell_id: "baseline".into(),
            case_id: case_id.into(),
            trial_index: 0,
            attempt: 2,
            stage_id: Some("check".into()),
            started_at: now_millis(5),
            pid,
            written_at,
            live: None,
            goal: Vec::new(),
        }
    }

    fn read_progress_file(run_dir: &std::path::Path) -> Option<Progress> {
        gents::eval::runner::read_progress(run_dir)
    }

    /// A run a runner too old to write `report.json` left: the watch has
    /// only the node to build the report from.
    fn without_report_file(run_dir: &std::path::Path) {
        std::fs::remove_file(run_dir.join(gents::eval::runner::REPORT_FILE)).unwrap();
    }

    /// What a live loop in this process would have written just now: it
    /// holds the run and has one slot in flight.
    fn hosted() -> Progress {
        Progress {
            holder: Some(Holder {
                pid: std::process::id(),
                written_at: now_millis(0),
            }),
            slots: BTreeMap::from([(
                "trial-1".to_owned(),
                slot("val-a", std::process::id(), now_millis(0)),
            )]),
        }
    }

    fn write_progress(run_dir: &std::path::Path, progress: &Progress) {
        std::fs::write(
            run_dir.join(PROGRESS_FILE),
            serde_json::to_vec(progress).unwrap(),
        )
        .unwrap();
    }

    /// Captures a watch's output and plays the host between renders: after
    /// each render before `last` it rewrites a fresh `progress.json`, so no
    /// entry ages however slow the test runs; after render `last` it removes
    /// the file, as a loop that exited leaves no holder.
    struct SimulatedHost {
        run_dir: PathBuf,
        last: usize,
        renders: usize,
        written: Vec<u8>,
    }

    impl SimulatedHost {
        fn new(run_dir: PathBuf, last: usize) -> Self {
            write_progress(&run_dir, &hosted());
            Self {
                run_dir,
                last,
                renders: 0,
                written: Vec::new(),
            }
        }

        fn output(&self) -> String {
            String::from_utf8(self.written.clone()).unwrap()
        }
    }

    impl std::io::Write for SimulatedHost {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.written.write(bytes)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.renders += 1;
            if self.renders < self.last {
                write_progress(&self.run_dir, &hosted());
            } else if self.renders == self.last {
                std::fs::remove_file(self.run_dir.join(PROGRESS_FILE))?;
            }
            Ok(())
        }
    }

    fn watch_args(argv: &[&str]) -> EvalWatchArgs {
        match eval_command(argv) {
            crate::cli::EvalCommand::Watch(args) => args,
            _ => panic!("not watch"),
        }
    }

    #[tokio::test]
    async fn watch_renders_the_report_and_what_is_in_flight() {
        let fixture = Fixture::new().await;
        fixture
            .scripted_run("r1", &executor(&[]), CancellationToken::new())
            .await;
        let run_dir = fixture.ctx.runs_dir().join("r1");

        // A finished run returns after one render even without --once.
        let finished = eval(&fixture, &["watch", "r1", "--interval", "1s"])
            .await
            .unwrap();
        assert!(finished.starts_with("--- "), "{finished}");
        assert_eq!(row(&finished, "baseline", 10)[1], "12");
        assert!(finished.contains("in flight: none"), "{finished}");
        assert!(!finished.contains("held by"), "{finished}");
        assert_eq!(finished.lines().last(), Some("finished"), "{finished}");
        assert_eq!(finished.matches("--- ").count(), 1, "{finished}");

        // Written right before the render: this process holds the run and
        // one slot; another slot was left by a host that stopped.
        let mut progress = hosted();
        progress
            .slots
            .insert("trial-2".to_owned(), slot("val-b", 4242, now_millis(60)));
        write_progress(&run_dir, &progress);
        let live = eval(&fixture, &["watch", "r1", "--once"]).await.unwrap();
        assert!(
            live.lines()
                .any(|line| line == format!("held by pid {}", std::process::id())),
            "{live}"
        );
        let line = live
            .lines()
            .find(|line| {
                line.trim_start()
                    .starts_with("baseline val-a #0 attempt 2 stage check for ")
            })
            .unwrap_or_else(|| panic!("{live}"));
        assert!(line.ends_with("(trial-1)"), "{line}");
        let stale = live
            .lines()
            .find(|line| line.trim_start().starts_with("baseline val-b #0"))
            .unwrap_or_else(|| panic!("{live}"));
        assert!(stale.contains("(trial-2) stale: pid 4242 "), "{stale}");
        assert!(
            !live.lines().any(|line| line == "finished") && !live.contains("stopped"),
            "--once renders and returns: {live}"
        );

        // While a live process holds the run the watch renders again; the
        // render after which it let go is the last.
        let mut host = SimulatedHost::new(run_dir.clone(), 2);
        watch(
            &fixture.ctx.runs_dir(),
            &watch_args(&["watch", "r1", "--interval", "250ms"]),
            None,
            || async { Ok(Documents::Open(&fixture.ctx)) },
            &mut host,
        )
        .await
        .unwrap();
        let followed = host.output();
        assert_eq!(followed.matches("--- ").count(), 2, "{followed}");
        assert_eq!(followed.lines().last(), Some("finished"), "{followed}");

        let absent = eval(&fixture, &["watch", "r1", "--once"]).await.unwrap();
        assert!(
            absent.contains("no progress file: showing finished slots only"),
            "{absent}"
        );
    }

    /// A live runner holds the home's node: the watch never opens it, and
    /// renders the whole frame from the files the runner leaves, with each
    /// slot's live snapshot against its goal and each finished slot's final
    /// snapshot and verdict detail.
    #[tokio::test]
    async fn a_held_home_still_renders_the_full_frame_from_the_runners_files() {
        use gents::eval::runner::{
            read_evidence_record, read_run_view, run_view, GoalEntry, LiveSnapshot, ToolTally,
            EVIDENCE_FILE, REPORT_FILE,
        };

        let fixture = Fixture::new().await;
        fixture
            .scripted_run(
                "r1",
                &executor(&VALIDATION_CASES[..1]),
                CancellationToken::new(),
            )
            .await;
        let run_dir = fixture.ctx.runs_dir().join("r1");
        let goal = vec![
            GoalEntry {
                collection: "AgentBehavior".into(),
                capture: None,
                min: 9,
                max: None,
            },
            GoalEntry {
                collection: "Task".into(),
                capture: Some("tasks".into()),
                min: 20,
                max: Some(22),
            },
            GoalEntry {
                collection: "SubagentTarget".into(),
                capture: None,
                min: 2,
                max: None,
            },
            GoalEntry {
                collection: gents::eval::runner::SCHEMAS_GOAL.into(),
                capture: None,
                min: 9,
                max: None,
            },
        ];
        let live = LiveSnapshot {
            stages: Default::default(),
            observed_at: now_millis(1),
            elapsed_secs: 400,
            requests: 3,
            model_turns: 57,
            input_tokens: Some(812_345),
            output_tokens: Some(41_234),
            reported_input_tokens: None,
            reported_output_tokens: None,
            session_contexts: None,
            tool_calls: 149,
            failed_tool_calls: 6,
            tools: [
                ("config", 98, 4),
                ("read_file", 20, 0),
                ("file_mailbox_item", 12, 2),
                ("write_file", 8, 0),
                ("shell", 6, 0),
                ("agent_message", 5, 0),
            ]
            .into_iter()
            .map(|(name, calls, failed)| (name.to_owned(), ToolTally { calls, failed }))
            .collect(),
            last_tool: Some(gents::eval::runner::LastToolCall {
                tool_name: "config".into(),
                state: Some("failed".into()),
                result: Some("task create refused: trigger review-shard names no Task".into()),
            }),
            documents: [
                ("AgentBehavior", 7),
                ("AgentContext", 7),
                ("Tools", 7),
                ("Task", 12),
                ("Trigger", 9),
                ("EventSource", 9),
                ("SubagentTarget", 2),
            ]
            .into_iter()
            .map(|(collection, rows)| (collection.to_owned(), rows))
            .collect(),
            captures: BTreeMap::from([("tasks".to_owned(), 12)]),
            schemas: ["RunStart", "ShardResult", "GateResult"]
                .map(str::to_owned)
                .to_vec(),
        };

        // A finished slot ended on this snapshot; the runner refreshed its
        // view after it.
        let report = gents::eval::report::load_report(
            &fixture.ctx.access,
            &fixture.ctx.owner,
            &fixture.ctx.runs_dir(),
            "r1",
        )
        .await
        .unwrap();
        let finished = report.cells[0].slots[0].latest.clone().unwrap().trial_id;
        let trial_dir = run_dir.join("trials").join(&finished);
        let mut record = read_evidence_record(&trial_dir).expect("an evidence record");
        record.live = Some(live.clone());
        record.goal = goal.clone();
        // The latest to finish, so it heads the finished list.
        record.ended_at = Some("2099-01-01T00:00:00Z".into());
        std::fs::write(
            trial_dir.join(EVIDENCE_FILE),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        std::fs::write(
            run_dir.join(REPORT_FILE),
            serde_json::to_vec(&run_view(report, &run_dir)).unwrap(),
        )
        .unwrap();
        let runner = read_run_view(&run_dir).expect("the view reads back");

        let mut progress = hosted();
        let slot = progress.slots.get_mut("trial-1").unwrap();
        slot.live = Some(live);
        slot.goal = goal;
        write_progress(&run_dir, &progress);

        let mut out = Vec::new();
        watch(
            &fixture.ctx.runs_dir(),
            &watch_args(&["watch", "r1", "--once"]),
            None,
            || async { panic!("a held home's node is never opened") },
            &mut out,
        )
        .await
        .unwrap();
        let frame = String::from_utf8(out).unwrap();
        println!("{frame}");
        let lines: Vec<&str> = frame.lines().collect();
        assert!(
            lines.contains(&format!("live from runner pid {}", runner.pid).as_str()),
            "{frame}"
        );
        assert_eq!(row(&frame, "baseline", 10)[1], "10", "the summary: {frame}");
        assert!(!frame.contains("report unavailable"), "{frame}");
        for said in [
            "in flight: 1",
            "    6m40s tokens 812.3k/41.2k requests 3 turns 57 tools 143 ok 6 failed",
            "    top tools config 98 (4 failed)  read_file 20  file_mailbox_item 12 (2 failed)  write_file 8  shell 6  +1 more",
            "    goal AgentBehavior 7/9  Task 12/20..22  SubagentTarget 2/2 ✓  schemas 3/9",
            "    docs AgentContext 7  EventSource 9  Tools 7  Trigger 9",
        ] {
            assert!(lines.contains(&said), "{said:?} in\n{frame}");
        }
        assert!(lines.contains(&"finished: 24 (latest 10 shown)"), "{frame}");
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("    check/captured_rows_count ")
                    && line.contains("findings observed ")),
            "verdict detail: {frame}"
        );
        assert_eq!(
            frame.matches("    goal ").count(),
            2,
            "in flight and finished: {frame}"
        );
        let held_at = lines
            .iter()
            .position(|line| line.starts_with("held by pid "))
            .unwrap();
        assert!(
            lines[held_at..]
                .iter()
                .all(|line| line.chars().count() <= 120),
            "the live sections fit 120 columns: {frame}"
        );

        // On a terminal the same files make one compact frame, redrawn in
        // place: at most 40 rows of 120 columns, no color under NO_COLOR.
        let screen = super::frame::Screen {
            width: 120,
            height: 40,
            color: false,
        };
        let heading = super::frame::Heading {
            run_id: "r1".into(),
            ..Default::default()
        };
        let lines = super::frame::frame(
            &heading,
            Some(&runner),
            read_progress_file(&run_dir).as_ref(),
            chrono::Utc::now(),
            screen,
        );
        println!("{}", lines.join("\n"));
        assert!(lines.len() <= 40, "{lines:#?}");
        assert!(
            lines.iter().all(|line| line.chars().count() <= 120),
            "{lines:#?}"
        );
        assert!(
            lines[0].starts_with("r1  cli-def v1  elapsed "),
            "{}",
            lines[0]
        );
        assert!(lines[0].ends_with(&format!("runner pid {}", std::process::id())));
        assert!(
            lines[1].contains(" 24/24 trials done  1 running  0 queued"),
            "{}",
            lines[1]
        );
        let flight = lines
            .iter()
            .find(|line| line.starts_with("  baseline   val-a"))
            .unwrap_or_else(|| panic!("{lines:#?}"));
        assert!(
            flight.contains("6m40s")
                && flight.contains("812.3k/41.2k")
                && flight.contains("143/6")
                && flight.ends_with("beh 7/9 task 12/20..22 sub 2/2 schema 3/9"),
            "{flight}"
        );
        assert!(
            lines.contains(
                &"    └ config err: task create refused: trigger review-shard names no Task"
                    .to_owned()
            ),
            "{lines:#?}"
        );
        assert!(
            lines.contains(&"finished (latest 4 of 24)".to_owned()),
            "{lines:#?}"
        );
        let latest = lines
            .iter()
            .position(|line| line.contains(" FAIL "))
            .unwrap_or_else(|| panic!("{lines:#?}"));
        assert!(lines[latest].ends_with("1 failing"), "{}", lines[latest]);
        assert_eq!(
            lines[latest + 1],
            "    └ captured_rows_count findings observed 0 expected ≥1"
        );
        let colored = super::frame::frame(
            &heading,
            Some(&runner),
            read_progress_file(&run_dir).as_ref(),
            chrono::Utc::now(),
            super::frame::Screen {
                color: true,
                ..screen
            },
        );
        assert!(colored
            .iter()
            .any(|line| line.contains("\x1b[31mFAIL\x1b[0m")));

        // `--json` is one object per render with the same view.
        let mut out = Vec::new();
        watch(
            &fixture.ctx.runs_dir(),
            &watch_args(&["watch", "r1", "--once", "--json"]),
            None,
            || async { panic!("a held home's node is never opened") },
            &mut out,
        )
        .await
        .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(json["source"], "runner");
        assert_eq!(
            json["progress"]["slots"]["trial-1"]["live"]["model_turns"],
            57
        );
        assert_eq!(json["view"]["trials"][&finished]["live"]["requests"], 3);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_delayed_heartbeat_keeps_watching_until_the_live_runner_releases() {
        let fixture = Fixture::new().await;
        fixture
            .scripted_run("r1", &executor(&[]), CancellationToken::new())
            .await;
        let run_dir = fixture.ctx.runs_dir().join("r1");
        let mut host = SimulatedHost::new(run_dir.clone(), 2);
        let mut progress = hosted();
        progress.holder.as_mut().unwrap().written_at = "2000-01-01T00:00:00Z".into();
        for slot in progress.slots.values_mut() {
            slot.written_at = "2000-01-01T00:00:00Z".into();
        }
        write_progress(&run_dir, &progress);
        assert!(!gents::eval::runner::running_elsewhere(&run_dir));
        let opens = Cell::new(0);
        watch(
            &fixture.ctx.runs_dir(),
            &watch_args(&["watch", "r1", "--interval", "250ms"]),
            None,
            || {
                opens.set(opens.get() + 1);
                async { Ok(Documents::Open(&fixture.ctx)) }
            },
            &mut host,
        )
        .await
        .unwrap();
        let output = host.output();
        assert_eq!(output.matches("--- ").count(), 2, "{output}");
        assert!(output.contains("has not refreshed it"), "{output}");
        assert_eq!(output.lines().last(), Some("finished"));
        assert_eq!(
            opens.get(),
            1,
            "open documents only after the runner releases"
        );

        progress.holder.as_mut().unwrap().pid = u32::MAX;
        for slot in progress.slots.values_mut() {
            slot.pid = u32::MAX;
        }
        write_progress(&run_dir, &progress);
        assert!(!super::runner_alive(&run_dir));
    }

    /// A run no live process holds ends the watch, whether or not it owes
    /// slots: a cancelled run would otherwise be watched forever. Ending
    /// stopped is a failure, so a script never reads it as a success.
    #[tokio::test]
    async fn watch_of_a_stopped_run_says_how_many_slots_it_owes_and_fails() {
        let fixture = Fixture::new().await;
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        fixture
            .scripted_run("r-stopped", &executor(&[]), cancelled)
            .await;
        let mut out = Vec::new();
        let error = watch(
            &fixture.ctx.runs_dir(),
            &watch_args(&["watch", "r-stopped", "--interval", "250ms"]),
            None,
            || async { Ok(Documents::Open(&fixture.ctx)) },
            &mut out,
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "stopped: 24 slots owed (gents eval resume r-stopped)"
        );
        let stopped = String::from_utf8(out).unwrap();
        assert_eq!(stopped.matches("--- ").count(), 1, "{stopped}");
        assert!(!stopped.lines().any(|line| line == "finished"), "{stopped}");
    }

    #[test]
    fn watch_help_states_the_exit_statuses() {
        use clap::CommandFactory;
        let mut cli = crate::cli::Cli::command();
        let help = cli
            .find_subcommand_mut("eval")
            .and_then(|eval| eval.find_subcommand_mut("watch"))
            .expect("eval watch")
            .render_long_help()
            .to_string();
        for said in [
            "0 when the run ends finished",
            "1 when it ends stopped",
            "2 on a usage error",
        ] {
            assert!(help.contains(said), "{said}: {help}");
        }
    }

    /// The hosting process holds the home's node: the watch shows what is
    /// in flight, tries the node at every render, and ends with the report's
    /// verdict once it opens.
    #[tokio::test]
    async fn watch_without_the_node_shows_in_flight_slots_and_ends_once_it_opens() {
        let fixture = Fixture::new().await;
        fixture
            .scripted_run("r1", &executor(&[]), CancellationToken::new())
            .await;
        let run_dir = fixture.ctx.runs_dir().join("r1");
        without_report_file(&run_dir);
        let args = watch_args(&["watch", "r1", "--interval", "250ms"]);

        // Locked for both renders; open at the ending.
        let opens = Cell::new(0);
        let mut host = SimulatedHost::new(run_dir.clone(), 2);
        watch(
            &fixture.ctx.runs_dir(),
            &args,
            None,
            || {
                opens.set(opens.get() + 1);
                let locked = opens.get() <= 2;
                let ctx = &fixture.ctx;
                async move {
                    Ok(if locked {
                        Documents::Locked(LOCKED.to_owned())
                    } else {
                        Documents::Open(ctx)
                    })
                }
            },
            &mut host,
        )
        .await
        .unwrap();
        let degraded = host.output();
        assert_eq!(
            degraded
                .matches(&format!(
                    "report unavailable: {LOCKED}; showing in-flight slots only"
                ))
                .count(),
            1,
            "the reason is said once: {degraded}"
        );
        assert_eq!(
            degraded
                .matches("report unavailable; showing in-flight slots only")
                .count(),
            1,
            "{degraded}"
        );
        assert!(!degraded.contains("exposure"), "no report: {degraded}");
        assert!(degraded.contains("(trial-1)"), "{degraded}");
        assert_eq!(degraded.lines().last(), Some("finished"), "{degraded}");
        assert_eq!(opens.get(), 3);

        // Still locked through the whole grace: the watch stops and fails,
        // having tried the node more than once.
        let opens = Cell::new(0);
        let mut host = SimulatedHost::new(run_dir.clone(), 1);
        let error = watch(
            &fixture.ctx.runs_dir(),
            &args,
            None,
            || {
                opens.set(opens.get() + 1);
                async { Ok(Documents::Locked(LOCKED.to_owned())) }
            },
            &mut host,
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("stopped: report unavailable: {LOCKED}")
        );
        assert!(opens.get() > 2, "retried within the grace: {}", opens.get());

        // Without the database a run id with no directory is refused.
        let error = watch(
            &fixture.ctx.runs_dir(),
            &watch_args(&["watch", "absent", "--once"]),
            None,
            || async { Ok(Documents::Locked(LOCKED.to_owned())) },
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("run absent has no directory "),
            "{error:#}"
        );
    }

    /// The host released the run but still holds the node while it renders
    /// its report and shuts down: the watch waits for the node and ends with
    /// the normal verdict.
    #[tokio::test]
    async fn the_ending_waits_for_a_node_the_host_is_still_letting_go_of() {
        let fixture = Fixture::new().await;
        fixture
            .scripted_run("r1", &executor(&[]), CancellationToken::new())
            .await;
        let run_dir = fixture.ctx.runs_dir().join("r1");
        without_report_file(&run_dir);
        let args = watch_args(&["watch", "r1", "--interval", "250ms"]);

        // One render; the holder is gone after it; three more tries find
        // the node locked; the fifth opens it.
        let opens = Cell::new(0);
        let mut host = SimulatedHost::new(run_dir.clone(), 1);
        watch(
            &fixture.ctx.runs_dir(),
            &args,
            None,
            || {
                opens.set(opens.get() + 1);
                let locked = opens.get() <= 4;
                let ctx = &fixture.ctx;
                async move {
                    Ok(if locked {
                        Documents::Locked(LOCKED.to_owned())
                    } else {
                        Documents::Open(ctx)
                    })
                }
            },
            &mut host,
        )
        .await
        .unwrap();
        let ended = host.output();
        assert_eq!(ended.matches("--- ").count(), 1, "{ended}");
        assert_eq!(ended.lines().last(), Some("finished"), "{ended}");
        assert_eq!(opens.get(), 5);

        // A run held again during the grace is watched again: the second
        // try finds a fresh holder, the next render finds it gone, and the
        // node then opens.
        let opens = Cell::new(0);
        let mut host = SimulatedHost::new(run_dir.clone(), 1);
        watch(
            &fixture.ctx.runs_dir(),
            &args,
            None,
            || {
                opens.set(opens.get() + 1);
                match opens.get() {
                    2 => write_progress(&run_dir, &hosted()),
                    3 => std::fs::remove_file(run_dir.join(PROGRESS_FILE)).unwrap(),
                    _ => {}
                }
                let locked = opens.get() <= 3;
                let ctx = &fixture.ctx;
                async move {
                    Ok(if locked {
                        Documents::Locked(LOCKED.to_owned())
                    } else {
                        Documents::Open(ctx)
                    })
                }
            },
            &mut host,
        )
        .await
        .unwrap();
        let rewatched = host.output();
        assert_eq!(rewatched.matches("--- ").count(), 2, "{rewatched}");
        assert_eq!(rewatched.lines().last(), Some("finished"), "{rewatched}");
    }

    /// Removes `progress.json` after every render; after the first it also
    /// has a host take the run again a moment later, while the watch waits.
    struct Relapsing {
        run_dir: PathBuf,
        renders: usize,
        written: Vec<u8>,
    }

    impl std::io::Write for Relapsing {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.written.write(bytes)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.renders += 1;
            std::fs::remove_file(self.run_dir.join(PROGRESS_FILE))?;
            if self.renders == 1 {
                let run_dir = self.run_dir.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    write_progress(&run_dir, &hosted());
                });
            }
            Ok(())
        }
    }

    /// A holder that lapses for a moment does not end the watch: it looks
    /// again after an interval (at most the stale window) and keeps
    /// watching a run held again.
    #[tokio::test]
    async fn a_holder_that_lapses_for_a_moment_is_watched_again() {
        let fixture = Fixture::new().await;
        fixture
            .scripted_run("r1", &executor(&[]), CancellationToken::new())
            .await;
        let run_dir = fixture.ctx.runs_dir().join("r1");
        write_progress(&run_dir, &hosted());
        let mut host = Relapsing {
            run_dir,
            renders: 0,
            written: Vec::new(),
        };
        watch(
            &fixture.ctx.runs_dir(),
            &watch_args(&["watch", "r1", "--interval", "1s"]),
            None,
            || async { Ok(Documents::Open(&fixture.ctx)) },
            &mut host,
        )
        .await
        .unwrap();
        let watched = String::from_utf8(host.written).unwrap();
        assert_eq!(watched.matches("--- ").count(), 2, "{watched}");
        assert_eq!(watched.lines().last(), Some("finished"), "{watched}");
    }

    /// Pins the text `store_locked` matches to what the pinned store says
    /// when its directory is opened a second time.
    #[tokio::test]
    async fn a_second_open_of_a_home_is_recognised_as_the_stores_lock() {
        let home = gents::eval::runner::embedded::EmbeddedHome::create_temp("lock")
            .await
            .unwrap();
        let second = gents::eval::runner::embedded::EmbeddedHome::open_retained(home.path())
            .await
            .err()
            .expect("the directory is locked by the first open");
        assert!(store_locked(&second), "{second:#}");
    }

    #[tokio::test]
    async fn an_unreportable_run_is_watched_without_its_report() {
        let fixture = Fixture::new().await;
        fixture
            .scripted_run("r1", &executor(&[]), CancellationToken::new())
            .await;
        let run_dir = fixture.ctx.runs_dir().join("r1");
        std::fs::write(run_dir.join("definition.json"), "not a definition").unwrap();
        without_report_file(&run_dir);
        write_progress(&run_dir, &hosted());
        let degraded = eval(&fixture, &["watch", "r1", "--once"]).await.unwrap();
        let line = degraded
            .lines()
            .find(|line| line.starts_with("report unavailable: parsing "))
            .unwrap_or_else(|| panic!("{degraded}"));
        assert!(line.ends_with("; showing in-flight slots only"), "{line}");
        assert!(degraded.contains("(trial-1)"), "{degraded}");
    }

    #[tokio::test]
    async fn only_the_stores_lock_degrades_the_watch() {
        let locked =
            anyhow::anyhow!("database directory is already locked for read-write access: /h/LOCK")
                .context("building embedded DefraDB node from /h");
        assert!(store_locked(&locked));
        assert!(store_locked(&anyhow::anyhow!(
            "database directory is already open in this process: /h"
        )));
        let uninitialized = anyhow::anyhow!("gents home /h is not initialized");
        assert!(!store_locked(&uninitialized));

        let degraded = reopen(|| async { Err(locked) }).await.unwrap();
        assert!(
            matches!(&degraded, Documents::Locked(reason) if reason.contains("already locked")),
            "the reason carries the lock"
        );
        let failed = reopen(|| async { Err(uninitialized) }).await;
        assert!(failed.is_err(), "any other failure is the watch's");
    }

    #[tokio::test]
    async fn the_homes_own_store_claim_degrades_the_watch() {
        // The claim refuses before the backend does, so the refusal reaches
        // the watch typed rather than as regolith's text.
        let held = anyhow::Error::new(gents::home::StoreLockHeld {
            home: std::path::PathBuf::from("/h"),
            holder_pid: Some(4321),
        });
        assert!(store_locked(&held));
        assert!(
            !format!("{held:#}").contains("already locked"),
            "the typed refusal is not recognized by regolith's text"
        );

        let degraded = reopen(|| async { Err(held) }).await.unwrap();
        assert!(
            matches!(&degraded, Documents::Locked(reason) if reason.contains("process 4321")),
            "the reason names the holder"
        );
    }

    #[test]
    fn an_interval_over_an_hour_is_refused_as_a_usage_error() {
        let parse = |interval: &str| {
            crate::cli::Cli::try_parse_from([
                "gents",
                "eval",
                "watch",
                "r1",
                "--interval",
                interval,
            ])
        };
        assert!(parse("1h").is_ok());
        assert!(parse("60m").is_ok());
        for interval in ["61m", "2h", "18446744073709551615s"] {
            let error = parse(interval).err().expect("refused");
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::ValueValidation,
                "{interval}"
            );
            assert_eq!(error.exit_code(), 2, "{interval}");
        }
        assert_eq!(
            crate::cli::parse_interval("2h").unwrap_err(),
            "--interval \"2h\" is over 1h"
        );
    }

    #[test]
    fn an_interval_under_250ms_is_refused() {
        let parse = |interval: &str| {
            crate::cli::Cli::try_parse_from([
                "gents",
                "eval",
                "watch",
                "r1",
                "--interval",
                interval,
            ])
        };
        assert!(parse("0s").is_err(), "zero is refused");
        assert!(parse("100ms").is_err(), "under 250ms is refused");
        assert!(parse("250ms").is_ok());
        assert!(parse("1s").is_ok());
        assert!(parse("soon").is_err());
        assert_eq!(
            crate::cli::parse_interval("100ms").unwrap_err(),
            "--interval \"100ms\" is under 250ms"
        );
        assert_eq!(
            crate::cli::parse_interval("500ms").unwrap(),
            std::time::Duration::from_millis(500)
        );
    }
}
