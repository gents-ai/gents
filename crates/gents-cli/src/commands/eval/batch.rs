//! A batch shares only trial admission; each run keeps its existing frozen
//! identity, report, retry policy, cancellation marker and resume semantics.

use std::collections::BTreeSet;
use std::io::Write;

use anyhow::{Context, Result};
use clap::Parser;
use futures_util::{stream::FuturesUnordered, StreamExt};
use gents::eval::report::load_report;
use gents::eval::runner::{self, TrialBudget};
use serde::{Deserialize, Serialize};

use super::{run, write_json, Deps, EvalContext};
use crate::cli::{EvalBatchArgs, EvalRunArgs};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    runs: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    args: Vec<String>,
}

#[derive(Parser)]
struct RunParser {
    #[command(flatten)]
    args: EvalRunArgs,
}

fn parse_plan(bytes: &[u8]) -> Result<Vec<EvalRunArgs>> {
    let plan: Plan = serde_json::from_slice(bytes).context("parsing eval batch plan")?;
    anyhow::ensure!(
        !plan.runs.is_empty(),
        "eval batch plan must contain at least one run"
    );
    let mut ids = BTreeSet::new();
    plan.runs
        .into_iter()
        .enumerate()
        .map(|(index, entry)| {
            let mut args = RunParser::try_parse_from(
                std::iter::once("eval batch run".to_owned()).chain(entry.args),
            )
            .with_context(|| format!("parsing batch run {}", index + 1))?
            .args;
            anyhow::ensure!(
                args.scope.home.is_none() && args.scope.graphql.is_none(),
                "batch run {} must use the batch's --home/--graphql scope",
                index + 1
            );
            anyhow::ensure!(
                !args.json,
                "batch run {} must not override --json",
                index + 1
            );
            run::profiles_by_cell(&args).map_err(anyhow::Error::msg)?;
            let id = args
                .run_id
                .as_deref()
                .context("each batch run requires an explicit --run-id")?;
            // The existing run owner validates path-safe IDs; a batch adds only uniqueness.
            runner::run_dir(std::path::Path::new("."), id)?;
            anyhow::ensure!(ids.insert(id.to_owned()), "duplicate batch run ID {id:?}");
            args.json = true;
            Ok(args)
        })
        .collect()
}

#[derive(Serialize)]
struct RunResult {
    run_id: String,
    report: Option<serde_json::Value>,
    error: Option<String>,
}

#[derive(Serialize)]
struct BatchResult {
    runs: Vec<RunResult>,
}

pub(super) async fn run(
    ctx: &EvalContext,
    args: &EvalBatchArgs,
    deps: &Deps<'_>,
    out: &mut dyn Write,
) -> Result<()> {
    let budget = TrialBudget::new(args.concurrency as usize)?;
    let parsed = parse_plan(
        &std::fs::read(&args.plan)
            .with_context(|| format!("reading batch plan {}", args.plan.display()))?,
    )?;
    let mut prepared = Vec::with_capacity(parsed.len());
    for mut run_args in parsed {
        run_args.json = args.json;
        let args = run_args;
        let (request, packs) = run::run_request(ctx, &args).await?;
        prepared.push((args, request, packs));
    }
    // Complete the existing owner's validation/freeze for every entry before
    // any executor starts. Packs stay guarded until every run has drained.
    for (_, request, _) in &prepared {
        runner::prepare_run(&ctx.access, request, deps.executor.isolation()).await?;
    }
    let mut options = deps.options.clone();
    options.trial_budget = Some(budget);
    let shared = Deps {
        executor: deps.executor,
        registry: deps.registry,
        cancel: deps.cancel.clone(),
        options,
    };
    let mut futures = FuturesUnordered::new();
    for (index, (run_args, request, _)) in prepared.iter().enumerate() {
        let shared = &shared;
        futures.push(async move {
            let mut buffered = Vec::new();
            let result = run::run_prepared(ctx, run_args, request, shared, &mut buffered).await;
            let mut error = result.err().map(|e| format!("{e:#}"));
            // Even a cancelled/failed run may have a useful durable report.
            let report = match load_report(
                &ctx.access,
                &ctx.owner,
                &ctx.runs_dir(),
                &request.run_id,
            )
            .await
            {
                Ok(report) => Some(report),
                Err(e) => {
                    let message = format!("loading report: {e:#}");
                    error = Some(error.map_or(message.clone(), |e| format!("{e}; {message}")));
                    None
                }
            };
            (index, request.run_id.clone(), report, error, buffered)
        });
    }
    let mut results = Vec::with_capacity(prepared.len());
    // A failed run never drops another started run's future. Output errors are
    // also delayed until all run futures have drained.
    let mut output_error = None;
    while let Some((index, run_id, report, error, buffered)) = futures.next().await {
        if !args.json && output_error.is_none() {
            let written = (|| -> Result<()> {
                writeln!(out, "run {run_id}")?;
                if !buffered.is_empty() {
                    out.write_all(&buffered)?;
                } else if let Some(report) = &report {
                    super::render::report_table(report, out)?;
                }
                if let Some(error) = &error {
                    writeln!(out, "error: {error}")?;
                }
                Ok(())
            })();
            output_error = written.err();
        }
        let (report, error) = match report.map(serde_json::to_value).transpose() {
            Ok(report) => (report, error),
            Err(e) => (
                None,
                Some(error.map_or_else(|| e.to_string(), |previous| format!("{previous}; {e}"))),
            ),
        };
        results.push((
            index,
            RunResult {
                run_id,
                report,
                error,
            },
        ));
    }
    results.sort_by_key(|(index, _)| *index);
    let result = BatchResult {
        runs: results.into_iter().map(|(_, run)| run).collect(),
    };
    if args.json {
        write_json(out, &result)?;
    }
    if let Some(error) = output_error {
        return Err(error);
    }
    anyhow::ensure!(
        result.runs.iter().all(|run| run.error.is_none()),
        "one or more batch runs failed or stopped; rerun the saved plan to resume"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(id: &str) -> serde_json::Value {
        json!({"args":["definition", "--cell", "engineer=/subject:engineer", "--run-id", id]})
    }

    #[test]
    fn plan_uses_run_parser_and_requires_unique_explicit_ids_and_common_scope() {
        let args =
            parse_plan(&serde_json::to_vec(&json!({"runs":[entry("one"), entry("two")]})).unwrap())
                .unwrap();
        assert_eq!(args[0].definition_id, "definition");
        assert_eq!(args[0].cells[0].agent.as_deref(), Some("engineer"));
        for plan in [
            json!({"runs":[]}),
            json!({"runs":[entry("same"),entry("same")]}),
            json!({"runs":[{"args":["d","--cell","a=/p"]}]}),
            json!({"runs":[{"args":["d","--cell","a=/p","--run-id","x","--home","/other"]}]}),
            json!({"runs":[{"args":["d","--cell","a=/p","--run-id","x","--graphql","http://other"]}]}),
            json!({"runs":[{"args":["d","--cell","a=/p","--run-id","x","--json"]}]}),
            json!({"runs":[{"args":["d","--cell","a=/p","--run-id","x","--unknown"]}]}),
        ] {
            assert!(
                parse_plan(&serde_json::to_vec(&plan).unwrap()).is_err(),
                "{plan}"
            );
        }
    }
    fn fixture_entry(fixture: &super::super::testing::Fixture, id: &str) -> serde_json::Value {
        json!({"args":[super::super::testing::DEFINITION, "--cell", format!("baseline={}", fixture.pack_arg()),
            "--profile", "baseline=local", "--split", "train", "--trials", "1", "--run-id", id]})
    }

    fn fixture_args(
        fixture: &super::super::testing::Fixture,
        runs: serde_json::Value,
    ) -> EvalBatchArgs {
        let plan = fixture.ctx.home_dir.join("batch-plan.json");
        std::fs::create_dir_all(&fixture.ctx.home_dir).unwrap();
        std::fs::write(&plan, serde_json::to_vec(&json!({"runs":runs})).unwrap()).unwrap();
        EvalBatchArgs {
            plan,
            concurrency: 2,
            json: true,
            scope: Default::default(),
        }
    }

    #[tokio::test]
    async fn every_request_is_preflighted_before_any_trial_starts() {
        use super::super::testing::{deps, executor, Fixture};
        let fixture = Fixture::new().await;
        let mut invalid = fixture_entry(&fixture, "invalid");
        invalid["args"][0] = json!("missing-definition");
        let args = fixture_args(&fixture, json!([fixture_entry(&fixture, "valid"), invalid]));
        let executor = executor(&[]);
        let registry = gents::eval::checks::CheckRegistry::builtin();
        let deps = deps(
            &executor,
            &registry,
            tokio_util::sync::CancellationToken::new(),
        );
        let error = run(&fixture.ctx, &args, &deps, &mut Vec::new())
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("missing-definition"),
            "{error:#}"
        );
        assert!(executor.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn reports_remain_separate_and_saved_plan_resumes_without_repeating_trials() {
        use super::super::testing::{deps, executor, Fixture};
        let fixture = Fixture::new().await;
        let args = fixture_args(
            &fixture,
            json!([
                fixture_entry(&fixture, "first"),
                fixture_entry(&fixture, "second")
            ]),
        );
        let executor = executor(&[]);
        let registry = gents::eval::checks::CheckRegistry::builtin();
        let deps = deps(
            &executor,
            &registry,
            tokio_util::sync::CancellationToken::new(),
        );
        for _ in 0..2 {
            let mut out = Vec::new();
            run(&fixture.ctx, &args, &deps, &mut out).await.unwrap();
            let result: serde_json::Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(result["runs"][0]["report"]["run"]["run_id"], "first");
            assert_eq!(result["runs"][1]["report"]["run"]["run_id"], "second");
            assert!(result["runs"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["error"].is_null()));
            assert_eq!(executor.calls.lock().unwrap().len(), 2);
        }
    }

    #[tokio::test]
    async fn cancellation_reports_all_runs_and_saved_plan_can_finish_them() {
        use super::super::testing::{deps, executor, Fixture};
        let fixture = Fixture::new().await;
        let args = fixture_args(
            &fixture,
            json!([
                fixture_entry(&fixture, "first"),
                fixture_entry(&fixture, "second")
            ]),
        );
        let executor = executor(&[]);
        let registry = gents::eval::checks::CheckRegistry::builtin();
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        let cancelled = deps(&executor, &registry, cancel);
        let mut out = Vec::new();
        assert!(run(&fixture.ctx, &args, &cancelled, &mut out)
            .await
            .is_err());
        let result: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(result["runs"].as_array().unwrap().len(), 2);
        assert!(result["runs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["error"].is_string()));
        assert!(executor.calls.lock().unwrap().is_empty());
        let resumed = deps(
            &executor,
            &registry,
            tokio_util::sync::CancellationToken::new(),
        );
        run(&fixture.ctx, &args, &resumed, &mut Vec::new())
            .await
            .unwrap();
        assert_eq!(executor.calls.lock().unwrap().len(), 2);
    }
    #[test]
    fn batch_requires_a_positive_global_trial_ceiling() {
        #[derive(clap::Parser)]
        struct Probe {
            #[command(flatten)]
            args: EvalBatchArgs,
        }
        assert!(Probe::try_parse_from(["probe", "--plan", "p", "--concurrency", "0"]).is_err());
        assert!(Probe::try_parse_from(["probe", "--plan", "p", "--concurrency", "-1"]).is_err());
        let parsed = Probe::try_parse_from(["probe", "--plan", "p", "--concurrency", "3"]).unwrap();
        assert_eq!(parsed.args.concurrency, 3);
    }

    #[tokio::test]
    async fn output_failure_still_drains_every_started_run() {
        use super::super::testing::{deps, executor, Fixture};
        struct BrokenOutput;
        impl Write for BrokenOutput {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("closed output"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let fixture = Fixture::new().await;
        let mut args = fixture_args(
            &fixture,
            json!([
                fixture_entry(&fixture, "first"),
                fixture_entry(&fixture, "second")
            ]),
        );
        args.json = false;
        let executor = executor(&[]);
        let registry = gents::eval::checks::CheckRegistry::builtin();
        let deps = deps(
            &executor,
            &registry,
            tokio_util::sync::CancellationToken::new(),
        );
        assert!(run(&fixture.ctx, &args, &deps, &mut BrokenOutput)
            .await
            .is_err());
        assert_eq!(executor.calls.lock().unwrap().len(), 2);
        for id in ["first", "second"] {
            let report = load_report(
                &fixture.ctx.access,
                &fixture.ctx.owner,
                &fixture.ctx.runs_dir(),
                id,
            )
            .await
            .unwrap();
            assert_eq!(report.cells[0].counts.pass, 1);
        }
    }
}
