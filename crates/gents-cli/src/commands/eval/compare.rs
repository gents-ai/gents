//! `compare`: the paired statistics between two cells, of one run or two,
//! and a policy verdict only when `--policy` asks for one; `--by` adds a
//! breakdown by check or stage and `--case` one case's trials side by side.

use std::io::Write;

use anyhow::Result;
use gents::eval::report::{
    by_check, by_stage, case_view, compare as compare_reports, load_report, CompareOptions,
    EvalReport, PolicyOutcome,
};
use serde::Serialize;

use super::{load_policy, render, write_json, EvalContext};
use crate::cli::{BreakdownArg, EvalCompareArgs};

pub(super) async fn compare(
    ctx: &EvalContext,
    args: &EvalCompareArgs,
    out: &mut dyn Write,
) -> Result<()> {
    let baseline =
        load_report(&ctx.access, &ctx.owner, &ctx.runs_dir(), &args.baseline_run).await?;
    let candidate = if args.candidate_run == args.baseline_run {
        baseline.clone()
    } else {
        load_report(
            &ctx.access,
            &ctx.owner,
            &ctx.runs_dir(),
            &args.candidate_run,
        )
        .await?
    };
    let baseline_cell = pick_cell(&baseline, args.baseline_cell.as_deref(), "--baseline-cell")?;
    let candidate_cell = pick_cell(
        &candidate,
        args.candidate_cell.as_deref(),
        "--candidate-cell",
    )?;
    let options = CompareOptions {
        include_pilot: args.include_pilot,
    };
    let mut comparison = compare_reports(
        &baseline,
        &candidate,
        &baseline_cell,
        &candidate_cell,
        &options,
    )?;
    if let Some(policy) = &args.policy {
        comparison = comparison.with_policy(&load_policy(policy)?);
    }
    if let Some(by) = args.by {
        let breakdown = match by {
            BreakdownArg::Check => render::Breakdown::Check(by_check(&comparison)),
            BreakdownArg::Stage => render::Breakdown::Stage(by_stage(&comparison)),
        };
        if args.json {
            return write_json(out, &with_policy(&breakdown, &comparison));
        }
        render::comparison_table(&comparison, out)?;
        return Ok(render::breakdown_table(&breakdown, out)?);
    }
    if let Some(case_id) = &args.case_id {
        let view = case_view(&comparison, case_id)?;
        if args.json {
            return write_json(out, &with_policy(&view, &comparison));
        }
        render::comparison_table(&comparison, out)?;
        return Ok(render::case_view_text(&view, out)?);
    }
    if args.json {
        write_json(out, &comparison)
    } else {
        Ok(render::comparison_table(&comparison, out)?)
    }
}

/// A breakdown's or a case's JSON with the comparison's `"policy"` outcome
/// beside it: the `PolicyOutcome` under `--policy`, else null.
#[derive(Serialize)]
struct WithPolicy<'a, T: Serialize> {
    #[serde(flatten)]
    body: &'a T,
    policy: Option<&'a PolicyOutcome>,
}

fn with_policy<'a, T: Serialize>(
    body: &'a T,
    comparison: &'a gents::eval::report::Comparison,
) -> WithPolicy<'a, T> {
    WithPolicy {
        body,
        policy: comparison.policy.as_ref(),
    }
}

/// The named cell, or the run's only one.
fn pick_cell(report: &EvalReport, named: Option<&str>, flag: &str) -> Result<String> {
    match (named, report.cells.as_slice()) {
        (Some(cell), _) => Ok(cell.to_owned()),
        (None, [only]) => Ok(only.cell_id.clone()),
        (None, cells) => anyhow::bail!(
            "run {} has {} cells; name one with {flag}",
            report.run.run_id,
            cells.len()
        ),
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use tokio_util::sync::CancellationToken;

    use super::super::testing::{eval, executor, row, Fixture, VALIDATION_CASES};
    use super::super::UNCALIBRATED_BANNER;
    use crate::cli::Cli;

    const CELLS: [&str; 4] = [
        "--baseline-cell",
        "baseline",
        "--candidate-cell",
        "candidate",
    ];

    #[tokio::test]
    async fn compare_prints_paired_statistics_and_the_banner_with_default_policy() {
        let fixture = Fixture::new().await;
        fixture
            .scripted_run(
                "r1",
                &executor(&VALIDATION_CASES[..3]),
                CancellationToken::new(),
            )
            .await;

        let mut argv = vec!["compare", "r1", "r1"];
        argv.extend(CELLS);
        let plain = eval(&fixture, &argv).await.unwrap();
        assert!(!plain.contains(UNCALIBRATED_BANNER), "{plain}");
        assert!(
            plain.contains(
                "improved 3 tied 3 worsened 0 mean_diff +50.00% pairs 12 dropped_baseline 0 dropped_candidate 0 imputed 0 p 125000ppm"
            ),
            "{plain}"
        );
        assert!(plain.contains("val-a"), "{plain}");

        argv.extend(["--policy", "defaults"]);
        let decided = eval(&fixture, &argv).await.unwrap();
        assert_eq!(decided.lines().next(), Some(UNCALIBRATED_BANNER));
        assert!(
            decided.contains("policy v2: reject(no_improvement) (alpha 16666ppm, calibrated no)"),
            "{decided}"
        );
        assert!(
            decided.contains(
                "gates sufficient yes no_case_regression yes cost_ok skipped significant no min_effect yes"
            ),
            "{decided}"
        );

        argv.push("--json");
        let json: serde_json::Value =
            serde_json::from_str(&eval(&fixture, &argv).await.unwrap()).unwrap();
        assert_eq!(json["p_ppm"], 125_000);
        assert_eq!(json["policy"]["calibrated"], false);
        assert_eq!(json["policy"]["report"]["decision"]["decision"], "reject");
    }

    #[tokio::test]
    async fn compare_needs_a_cell_for_a_run_with_several_and_reads_a_policy_file() {
        let fixture = Fixture::new().await;
        fixture
            .scripted_run(
                "r1",
                &executor(&VALIDATION_CASES[..3]),
                CancellationToken::new(),
            )
            .await;
        let error = eval(&fixture, &["compare", "r1", "r1"]).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "run r1 has 2 cells; name one with --baseline-cell"
        );

        let path = fixture.ctx.home_dir.join("policy.json");
        std::fs::create_dir_all(&fixture.ctx.home_dir).unwrap();
        let policy = gents::optimization::PolicyV2 {
            min_pairs: 1,
            ..gents::optimization::PolicyV2::uncalibrated()
        };
        std::fs::write(&path, serde_json::to_vec(&policy).unwrap()).unwrap();
        let path_arg = path.display().to_string();
        let mut argv = vec!["compare", "r1", "r1", "--policy", path_arg.as_str()];
        argv.extend(CELLS);
        let decided = eval(&fixture, &argv).await.unwrap();
        assert!(!decided.contains(UNCALIBRATED_BANNER), "{decided}");
        assert!(decided.contains("calibrated yes"), "{decided}");
    }

    /// `comparison_table` reads each gate from its own field: an
    /// undetermined `min_effect` and a skipped `cost_ok` print their own words.
    #[tokio::test]
    async fn the_gates_line_prints_an_undetermined_min_effect_and_a_skipped_cost_gate() {
        let fixture = Fixture::new().await;
        fixture
            .scripted_run(
                "r1",
                &executor(&VALIDATION_CASES[..3]),
                CancellationToken::new(),
            )
            .await;
        let ctx = &fixture.ctx;
        let report =
            gents::eval::report::load_report(&ctx.access, &ctx.owner, &ctx.runs_dir(), "r1")
                .await
                .unwrap();
        let mut comparison = gents::eval::report::compare(
            &report,
            &report,
            "baseline",
            "candidate",
            &gents::eval::report::CompareOptions::default(),
        )
        .unwrap()
        .with_policy(&gents::optimization::PolicyV2::uncalibrated());
        let gates = &mut comparison.policy.as_mut().unwrap().gates;
        gates.min_effect = None;
        gates.cost_ok = None;
        let mut out = Vec::new();
        super::super::render::comparison_table(&comparison, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        let gates_line = text
            .lines()
            .find(|line| line.starts_with("gates "))
            .unwrap_or_else(|| panic!("no gates line in\n{text}"));
        assert!(gates_line.contains("cost_ok skipped"), "{gates_line}");
        assert!(
            gates_line.ends_with("min_effect undetermined"),
            "{gates_line}"
        );

        comparison.policy.as_mut().unwrap().gates.min_effect = Some(false);
        let mut out = Vec::new();
        super::super::render::comparison_table(&comparison, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("cost_ok skipped significant no min_effect no"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn by_check_and_by_stage_aggregate_and_case_prints_both_sides() {
        let fixture = Fixture::new().await;
        fixture
            .scripted_run(
                "r1",
                &executor(&VALIDATION_CASES[..3]),
                CancellationToken::new(),
            )
            .await;
        let with = |extra: &[&'static str]| {
            let mut argv = vec!["compare", "r1", "r1"];
            argv.extend(CELLS);
            argv.extend_from_slice(extra);
            argv
        };

        let checks = eval(&fixture, &with(&["--by", "check"])).await.unwrap();
        assert_eq!(
            row(&checks, "captured_rows_count", 7),
            vec!["captured_rows_count", "6", "12", "+50.00%", "3", "3", "0"]
        );
        let stages = eval(&fixture, &with(&["--by", "stage"])).await.unwrap();
        assert_eq!(
            row(&stages, "check", 7),
            vec!["check", "6", "12", "+50.00%", "3", "3", "0"]
        );

        let json: serde_json::Value = serde_json::from_str(
            &eval(&fixture, &with(&["--by", "check", "--json"]))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(json["by"], "check");
        assert_eq!(json["rows"][0]["mean_diff_bp"], 5_000);
        assert!(json["policy"].is_null(), "{json}");

        // With --policy, the breakdown and the case carry its outcome.
        for extra in [
            &["--by", "stage", "--json", "--policy", "defaults"][..],
            &["--case", "val-a", "--json", "--policy", "defaults"],
        ] {
            let json: serde_json::Value =
                serde_json::from_str(&eval(&fixture, &with(extra)).await.unwrap()).unwrap();
            assert_eq!(json["policy"]["calibrated"], false, "{json}");
            assert_eq!(
                json["policy"]["report"]["decision"]["decision"], "reject",
                "{json}"
            );
        }
        let json: serde_json::Value = serde_json::from_str(
            &eval(&fixture, &with(&["--case", "val-a", "--json"]))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(json["case_id"], "val-a");
        assert!(json["policy"].is_null(), "{json}");

        let case = eval(&fixture, &with(&["--case", "val-a"])).await.unwrap();
        for expected in [
            "case val-a pairs 2 baseline 0.00% candidate 100.00% diff +100.00%",
            "#0 baseline fail 0.00% candidate pass 100.00%",
            "  baseline check/captured_rows_count model_acceptance score 0 reason below_min",
            "  candidate check/captured_rows_count passed score 10000 reason in_range",
        ] {
            assert!(
                case.lines().any(|line| line == expected),
                "{expected:?} in\n{case}"
            );
        }

        // A breakdown with no row says so instead of printing a bare header.
        for (breakdown, expected) in [
            (
                super::super::render::Breakdown::Check(Vec::new()),
                "no check scored on both sides of any pair",
            ),
            (
                super::super::render::Breakdown::Stage(Vec::new()),
                "no stage scored on both sides of any pair",
            ),
        ] {
            let mut out = Vec::new();
            super::super::render::breakdown_table(&breakdown, &mut out).unwrap();
            assert_eq!(String::from_utf8(out).unwrap(), format!("\n{expected}\n"));
        }
        let help = Cli::try_parse_from(["gents", "eval", "--help"])
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(
            help.contains("--by check|stage breaks it down, --case shows one case"),
            "{help}"
        );

        let missing = eval(&fixture, &with(&["--case", "zzz"])).await.unwrap_err();
        assert_eq!(missing.to_string(), "the comparison has no case \"zzz\"");
        assert!(Cli::try_parse_from([
            "gents", "eval", "compare", "r1", "r1", "--by", "check", "--case", "val-a"
        ])
        .is_err());
    }
}
