//! `run` and `resume`: the runner's loop, followed from the terminal. A line
//! per slot as it lands, then the `show` table. Ctrl-C cancels the token the
//! command was given (`cancel_on_ctrl_c`).

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::io::Write;

use anyhow::{anyhow, Result};
use gents::eval::documents::default_breaker_threshold;
use gents::eval::load_trials;
use gents::eval::report::load_report;
use gents::eval::runner::{self, CellRequest, RunOutcome, RunRequest, CANCEL_MARKER};
use gents::{default_behavior_id_for_agent, default_inference_profile_id_for_behavior};

use super::{
    default_id, follow_progress, render, source_commit, source_dirty, write_json, Deps,
    EvalContext, Progress,
};
use crate::cli::{EvalResumeArgs, EvalRunArgs};
use crate::commands::pack::{resolve_subject_pack, SubjectPack};

pub(super) async fn run(
    ctx: &EvalContext,
    args: &EvalRunArgs,
    deps: &Deps<'_>,
    out: &mut dyn Write,
) -> Result<()> {
    // The packs are held until the run ends: a cache entry stays locked.
    let (request, _packs) = run_request(ctx, args).await?;
    let run_id = request.run_id.clone();
    // A `--run-id` naming an existing run continues it, as `resume` does:
    // its landed attempts are not printed again, and the marker the runner
    // clears is reported once the run ran.
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
            &request,
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

/// The attempts of `run_id` that already landed; none for a new run.
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
    // `runner::resume` thaws the run before it touches the
    // marker, so a refused resume (an invalidated run, a changed pack) says
    // nothing about it; the notice is printed only once the resume ran.
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

/// Each `--profile` by the cell it names. Refused, from argv alone, when a
/// `--cell` id repeats, or a profile names a cell no `--cell` declares or
/// names a cell twice: the binary reports it as a usage error (exit 2)
/// before reading anything. The runner refuses a repeated cell id too.
pub(super) fn profiles_by_cell(args: &EvalRunArgs) -> Result<BTreeMap<&str, &str>, String> {
    let mut cells = BTreeSet::new();
    for cell in &args.cells {
        if !cells.insert(cell.cell_id.as_str()) {
            return Err(format!(
                "--cell declares cell {:?} more than once",
                cell.cell_id
            ));
        }
    }
    let mut profiles: BTreeMap<&str, &str> = BTreeMap::new();
    for (cell_id, profile_id) in &args.profiles {
        if !cells.contains(cell_id.as_str()) {
            return Err(format!(
                "--profile names cell {cell_id:?}, which no --cell declares"
            ));
        }
        if profiles.insert(cell_id, profile_id).is_some() {
            return Err(format!("--profile names cell {cell_id:?} more than once"));
        }
    }
    Ok(profiles)
}

async fn run_request(
    ctx: &EvalContext,
    args: &EvalRunArgs,
) -> Result<(RunRequest, Vec<SubjectPack>)> {
    let profiles = profiles_by_cell(args).map_err(anyhow::Error::msg)?;
    let default_profile =
        default_inference_profile_id_for_behavior(&default_behavior_id_for_agent(&ctx.owner));
    let mut cells = Vec::with_capacity(args.cells.len());
    let mut packs = Vec::with_capacity(args.cells.len());
    for cell in &args.cells {
        let pack = resolve_subject_pack(&ctx.home_dir, &cell.pack, args.registry.as_deref(), false)
            .await?;
        let behavior_id = match &cell.behavior {
            Some(behavior) => behavior.clone(),
            None => pack.default_behavior()?,
        };
        cells.push(CellRequest {
            cell_id: cell.cell_id.clone(),
            label: cell.cell_id.clone(),
            source: pack.source.clone(),
            behavior_id,
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
        // Each stage reads its captures from the definition; this run-level
        // list is only the fallback, and the command adds none.
        captures: Vec::new(),
        runs_dir: ctx.runs_dir(),
    };
    Ok((request, packs))
}

/// Drive `running` to its end, printing a line for each slot that lands.
async fn follow<F>(
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

/// The run's slots that landed, printed once each.
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

/// Print every completed attempt not printed yet. A failed read only delays
/// the lines; it never stops the run.
async fn report_landed(
    ctx: &EvalContext,
    run_id: &str,
    landed: &mut BTreeSet<String>,
    out: &mut dyn Write,
) {
    let trials = match load_trials(&ctx.access, &ctx.owner, run_id).await {
        Ok(trials) => trials,
        Err(error) => {
            tracing::warn!(run_id, error = %format!("{error:#}"), "could not read the run's trials for progress");
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
    // The pass says whether the token or a marker stopped it. A stopped run
    // is not a success: the table (or JSON) still renders, then the command
    // fails with the note, so a script never reads a partial run as done.
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
        Some(note) => Err(anyhow!(note)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use gents::eval::checks::CheckRegistry;
    use tokio_util::sync::CancellationToken;

    use super::super::testing::{
        deps, eval, eval_with, executor, row, Fixture, DEFINITION, VALIDATION_CASES,
    };

    fn run_argv<'a>(run_id: &'a str, cells: &'a [String]) -> Vec<&'a str> {
        let mut argv = vec!["run", DEFINITION, "--run-id", run_id];
        for cell in cells {
            argv.extend(["--cell", cell.as_str()]);
        }
        argv.extend([
            "--profile",
            "baseline=local",
            "--profile",
            "candidate=local",
        ]);
        argv
    }

    /// A refused resume prints nothing about the marker.
    #[tokio::test]
    async fn a_refused_resume_says_nothing_about_the_marker() {
        let fixture = Fixture::new().await;
        let interrupted = CancellationToken::new();
        interrupted.cancel();
        fixture
            .scripted_run("r1", &executor(&[]), interrupted)
            .await;
        eval(&fixture, &["cancel", "r1"]).await.unwrap();
        eval(
            &fixture,
            &["invalidate", "r1", "--reason", "broken fixture"],
        )
        .await
        .unwrap();
        let mut out = Vec::new();
        let scripted = executor(&[]);
        let registry = CheckRegistry::builtin();
        let error = super::super::execute(
            &fixture.ctx,
            super::super::testing::eval_command(&["resume", "r1"]),
            &deps(&scripted, &registry, CancellationToken::new()),
            &mut out,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("invalidated"), "{error:#}");
        assert!(out.is_empty(), "{}", String::from_utf8_lossy(&out));
        assert!(fixture
            .ctx
            .runs_dir()
            .join("r1")
            .join(gents::eval::runner::CANCEL_MARKER)
            .exists());
    }

    fn cells(pack: &str) -> Vec<String> {
        vec![
            format!("baseline={pack}"),
            format!("candidate={pack}:monitor"),
        ]
    }

    #[tokio::test]
    async fn run_prints_one_line_per_landed_slot_and_the_show_table() {
        let fixture = Fixture::new().await;
        let pack = fixture.pack_arg();
        let cells = cells(&pack);
        let scripted = executor(&VALIDATION_CASES[..3]);
        let registry = CheckRegistry::builtin();
        let output = eval_with(
            &fixture,
            &run_argv("r1", &cells),
            &deps(&scripted, &registry, CancellationToken::new()),
        )
        .await
        .unwrap();
        assert_eq!(
            output
                .lines()
                .filter(|line| line.starts_with("landed "))
                .count(),
            24,
            "{output}"
        );
        assert!(
            output.contains("landed baseline val-a #0 attempt 1"),
            "{output}"
        );
        assert_eq!(
            row(&output, "baseline", 10),
            vec!["baseline", "6", "6", "0", "0", "0", "0", "12", "50.00%", "-/-"]
        );
        assert!(!output.contains("stopped before it finished"), "{output}");

        let mut json_argv = run_argv("r2", &cells);
        json_argv.push("--json");
        let json: serde_json::Value = serde_json::from_str(
            &eval_with(
                &fixture,
                &json_argv,
                &deps(&scripted, &registry, CancellationToken::new()),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(json["run"]["run_id"], "r2");

        // Without --run-id, a fresh id every time.
        let fresh: Vec<&str> = run_argv("unused", &cells)
            .into_iter()
            .filter(|arg| *arg != "--run-id" && *arg != "unused")
            .collect();
        let first = eval_with(
            &fixture,
            &fresh,
            &deps(&scripted, &registry, CancellationToken::new()),
        )
        .await
        .unwrap();
        assert!(first.contains(&format!("run {DEFINITION}-")), "{first}");
        assert_eq!(json["run"]["source_commit"], super::super::source_commit());
    }

    #[tokio::test]
    async fn a_cancelled_run_says_how_to_resume_and_resume_clears_the_marker_and_finishes() {
        let fixture = Fixture::new().await;
        let pack = fixture.pack_arg();
        let cells = cells(&pack);
        let scripted = executor(&[]);
        let registry = CheckRegistry::builtin();
        // What Ctrl-C does: cancel the token the command was given.
        let interrupted = CancellationToken::new();
        interrupted.cancel();
        // A stopped run renders its table, then fails with the note once.
        let mut out = Vec::new();
        let error = super::super::execute(
            &fixture.ctx,
            super::super::testing::eval_command(&run_argv("r1", &cells)),
            &deps(&scripted, &registry, interrupted.clone()),
            &mut out,
        )
        .await
        .unwrap_err();
        assert_eq!(
            format!("{error:#}"),
            "run r1 stopped before it finished (0 completed, 0 abandoned this pass); `gents eval resume r1` continues it"
        );
        let stopped = String::from_utf8(out).unwrap();
        assert!(!stopped.contains("stopped before it finished"), "{stopped}");
        assert_eq!(row(&stopped, "baseline", 10)[6], "12");

        // With --json, stdout stays the report alone and the note is the error.
        let mut json_argv = run_argv("r1", &cells);
        json_argv.push("--json");
        let mut out = Vec::new();
        let error = super::super::execute(
            &fixture.ctx,
            super::super::testing::eval_command(&json_argv),
            &deps(&scripted, &registry, interrupted),
            &mut out,
        )
        .await
        .unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("run r1 stopped before it finished"),
            "{error:#}"
        );
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap_or_else(|error| {
            panic!(
                "stdout is not pure JSON ({error}): {}",
                String::from_utf8_lossy(&out)
            )
        });
        assert_eq!(json["run"]["run_id"], "r1");

        eval(&fixture, &["cancel", "r1"]).await.unwrap();
        let resumed = eval(&fixture, &["resume", "r1"]).await.unwrap();
        assert!(resumed.contains("cleared the cancel marker "), "{resumed}");
        assert_eq!(
            resumed
                .lines()
                .filter(|line| line.starts_with("landed "))
                .count(),
            24,
            "{resumed}"
        );
        assert_eq!(row(&resumed, "candidate", 10)[1], "12", "{resumed}");
        assert!(!fixture
            .ctx
            .runs_dir()
            .join("r1")
            .join(gents::eval::runner::CANCEL_MARKER)
            .exists());
    }

    /// Running an existing `--run-id` continues it like `resume`: attempts
    /// that already landed are not printed again, and the marker the runner
    /// clears is reported.
    #[tokio::test]
    async fn run_with_an_existing_run_id_prints_only_new_attempts_and_the_cleared_marker() {
        let fixture = Fixture::new().await;
        let pack = fixture.pack_arg();
        let cells = cells(&pack);
        let scripted = executor(&[]);
        let registry = CheckRegistry::builtin();
        let first = eval_with(
            &fixture,
            &run_argv("r1", &cells),
            &deps(&scripted, &registry, CancellationToken::new()),
        )
        .await
        .unwrap();
        let landed = |output: &str| {
            output
                .lines()
                .filter(|line| line.starts_with("landed "))
                .count()
        };
        assert_eq!(landed(&first), 24, "{first}");
        assert!(!first.contains("cleared the cancel marker"), "{first}");

        eval(&fixture, &["cancel", "r1"]).await.unwrap();
        let again = eval_with(
            &fixture,
            &run_argv("r1", &cells),
            &deps(&scripted, &registry, CancellationToken::new()),
        )
        .await
        .unwrap();
        assert_eq!(landed(&again), 0, "{again}");
        assert!(again.contains("cleared the cancel marker "), "{again}");
        assert_eq!(row(&again, "baseline", 10)[1], "12", "{again}");
        assert!(!fixture
            .ctx
            .runs_dir()
            .join("r1")
            .join(gents::eval::runner::CANCEL_MARKER)
            .exists());
    }

    #[tokio::test]
    async fn a_cell_without_a_profile_uses_the_homes_default_profile() {
        let fixture = Fixture::new().await;
        let pack = fixture.pack_arg();
        let cell = format!("baseline={pack}");
        let error = eval(
            &fixture,
            &["run", DEFINITION, "--cell", cell.as_str(), "--run-id", "r1"],
        )
        .await
        .unwrap_err();
        let profile = gents::default_inference_profile_id_for_behavior(
            &gents::default_behavior_id_for_agent(&fixture.ctx.owner),
        );
        assert_eq!(
            error.to_string(),
            format!("cell \"baseline\" names no inference profile {profile:?}")
        );

        let stray = eval(
            &fixture,
            &[
                "run",
                DEFINITION,
                "--cell",
                cell.as_str(),
                "--profile",
                "other=local",
            ],
        )
        .await
        .unwrap_err();
        assert_eq!(
            stray.to_string(),
            "--profile names cell \"other\", which no --cell declares"
        );

        let twice = eval(
            &fixture,
            &[
                "run",
                DEFINITION,
                "--cell",
                cell.as_str(),
                "--profile",
                "baseline=local",
                "--profile",
                "baseline=other",
            ],
        )
        .await
        .unwrap_err();
        assert_eq!(
            twice.to_string(),
            "--profile names cell \"baseline\" more than once"
        );
    }
}
