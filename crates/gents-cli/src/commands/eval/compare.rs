//! `compare`: the paired statistics between two cells, of one run or two,
//! and a policy verdict only when `--policy` asks for one.

use std::io::Write;

use anyhow::Result;
use gents::eval::report::{compare as compare_reports, load_report, EvalReport};

use super::{load_policy, render, write_json, EvalContext};
use crate::cli::EvalCompareArgs;

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
    let mut comparison = compare_reports(&baseline, &candidate, &baseline_cell, &candidate_cell)?;
    if let Some(policy) = &args.policy {
        comparison = comparison.with_policy(&load_policy(policy)?);
    }
    if args.json {
        write_json(out, &comparison)
    } else {
        Ok(render::comparison_table(&comparison, out)?)
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
    use tokio_util::sync::CancellationToken;

    use super::super::testing::{eval, executor, Fixture, VALIDATION_CASES};
    use super::super::UNCALIBRATED_BANNER;

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
        let mut comparison =
            gents::eval::report::compare(&report, &report, "baseline", "candidate")
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
}
