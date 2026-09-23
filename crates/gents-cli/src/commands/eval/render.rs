//! Plain-text tables for `gents eval`. The `--json` form is the library's
//! own structure; these are for a terminal.

use std::io::{self, Write};

use gents::eval::report::{Comparison, EvalReport, SlotCounts};
use gents::eval::runner::{is_fresh, Progress, STALE_WINDOW};
use gents::eval::TrialUsage;
use gents::optimization::Decision;
use serde::Serialize;

use super::inspect::{ListRow, TrialView};

/// Basis points as a percentage with two decimals, or `-`.
pub(crate) fn percent(bp: Option<u32>) -> String {
    bp.map_or_else(
        || "-".to_owned(),
        |bp| format!("{}.{:02}%", bp / 100, bp % 100),
    )
}

/// A value's serde form: an enum's wire name, a string's text.
pub(crate) fn wire<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => text,
        Ok(other) => other.to_string(),
        Err(_) => "?".to_owned(),
    }
}

fn count(value: Option<u64>) -> String {
    value.map_or_else(|| "-".to_owned(), |value| value.to_string())
}

fn tokens(usage: &TrialUsage) -> String {
    format!(
        "{}/{}",
        count(usage.input_tokens),
        count(usage.output_tokens)
    )
}

pub(crate) fn counts_inline(counts: &SlotCounts) -> String {
    format!(
        "pass {} fail {} unknown {} not_evidence {} abandoned {} planned {}",
        counts.pass,
        counts.fail,
        counts.unknown,
        counts.not_evidence,
        counts.abandoned,
        counts.planned
    )
}

pub(crate) fn report_table(report: &EvalReport, out: &mut dyn Write) -> io::Result<()> {
    let run = &report.run;
    writeln!(
        out,
        "run {} definition {} v{} split {} purpose {}",
        run.run_id,
        run.definition.definition_id,
        run.definition.comparability_version,
        wire(&run.split),
        run.purpose
    )?;
    writeln!(
        out,
        "created {} trials_per_case {} seed_base {} concurrency {} source {}{}",
        run.created_at,
        run.trials_per_case,
        run.seed_base,
        run.concurrency,
        run.source_commit,
        if run.source_dirty { " (dirty)" } else { "" }
    )?;
    if let Some(invalidation) = &run.invalidated {
        writeln!(
            out,
            "invalidated {} by {}: {}",
            invalidation.at, invalidation.by, invalidation.reason
        )?;
    }
    if report.definition_changed {
        writeln!(
            out,
            "the installed eval definition has changed since this run froze it; this report reads the run's frozen copy"
        )?;
    }
    writeln!(out)?;
    writeln!(
        out,
        "{:<16} {:>5} {:>5} {:>7} {:>12} {:>9} {:>7} {:>8} {:>9} {:>13}",
        "cell",
        "pass",
        "fail",
        "unknown",
        "not_evidence",
        "abandoned",
        "planned",
        "attempts",
        "headline",
        "tokens_in/out"
    )?;
    for cell in &report.cells {
        let counts = &cell.counts;
        writeln!(
            out,
            "{:<16} {:>5} {:>5} {:>7} {:>12} {:>9} {:>7} {:>8} {:>9} {:>13}",
            cell.cell_id,
            counts.pass,
            counts.fail,
            counts.unknown,
            counts.not_evidence,
            counts.abandoned,
            counts.planned,
            cell.attempts,
            percent(cell.headline_bp),
            tokens(&cell.usage)
        )?;
    }
    writeln!(out)?;
    writeln!(
        out,
        "{:<16} {:<24} {:<14} {:>9}  counts",
        "cell", "case", "reducer", "mean"
    )?;
    for cell in &report.cells {
        for case in &cell.cases {
            writeln!(
                out,
                "{:<16} {:<24} {:<14} {:>9}  {}",
                cell.cell_id,
                case.case_id,
                wire(&case.reducer),
                percent(case.mean_bp),
                counts_inline(&case.counts)
            )?;
        }
    }
    writeln!(out)?;
    writeln!(
        out,
        "exposure {} (non-invalidated runs of {} v{} on the {} split)",
        report.exposure,
        run.definition.definition_id,
        run.definition.comparability_version,
        wire(&run.split)
    )
}

/// A byte count for a person: `512B`, `1.5KiB`, `3.0MiB`.
pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes}B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1}{}", UNITS[unit])
}

pub(crate) fn list_table(rows: &[ListRow], out: &mut dyn Write) -> io::Result<()> {
    writeln!(
        out,
        "{:<28} {:<24} {:<11} {:<24} {:<21} {:<11} {:<9} cells",
        "run_id", "definition", "split", "purpose", "created", "invalidated", "size"
    )?;
    for row in rows {
        let cells = match &row.report_error {
            Some(error) => format!("report unavailable: {error}"),
            None => row
                .cells
                .iter()
                .map(|cell| format!("{}[{}]", cell.cell_id, counts_inline(&cell.counts)))
                .collect::<Vec<_>>()
                .join(" "),
        };
        writeln!(
            out,
            "{:<28} {:<24} {:<11} {:<24} {:<21} {:<11} {:<9} {}",
            row.run_id,
            format!("{}@v{}", row.definition_id, row.comparability_version),
            wire(&row.split),
            row.purpose,
            row.created_at,
            if row.invalidated.is_some() {
                "yes"
            } else {
                "no"
            },
            match (row.size_bytes, &row.size_error) {
                (Some(bytes), _) => human_bytes(bytes),
                (None, Some(_)) => "?".to_owned(),
                (None, None) => "-".to_owned(),
            },
            cells
        )?;
    }
    Ok(())
}

pub(crate) fn trial_text(view: &TrialView, out: &mut dyn Write) -> io::Result<()> {
    let slot = &view.slot;
    writeln!(
        out,
        "run {} cell {} case {} trial {} class {} attempts {}",
        view.run_id,
        view.cell_id,
        slot.case_id,
        slot.trial_index,
        wire(&slot.class),
        slot.attempts
    )?;
    if let Some(latest) = &slot.latest {
        writeln!(
            out,
            "attempt {} trial_id {} agent {} session {}",
            latest.attempt, latest.trial_id, latest.trial_agent_did, latest.session_id
        )?;
        writeln!(
            out,
            "home {}",
            view.home
                .as_ref()
                .map_or_else(|| "-".to_owned(), |home| home.display().to_string())
        )?;
        writeln!(
            out,
            "evidence_digest {}",
            latest.evidence_digest.as_deref().unwrap_or("-")
        )?;
        for stage in &latest.stages {
            writeln!(
                out,
                "stage {} terminal {} kind {}",
                stage.stage_id,
                stage
                    .terminal_state
                    .as_ref()
                    .map_or_else(|| "-".to_owned(), |state| wire(state)),
                stage
                    .failure_kind
                    .map_or_else(|| "-".to_owned(), |kind| kind.as_str().to_owned())
            )?;
        }
    }
    for verdict in &view.verdicts {
        writeln!(
            out,
            "verdict {} {} {} score {} reason {}{}",
            verdict.check,
            wire(&verdict.tier),
            verdict.kind.as_str(),
            verdict
                .score_bp
                .map_or_else(|| "-".to_owned(), |score| score.to_string()),
            verdict.reason_code.as_deref().unwrap_or("-"),
            verdict
                .regrade_of
                .as_ref()
                .map_or_else(String::new, |id| format!(" (regrades {id})"))
        )?;
    }
    Ok(())
}

/// Signed basis points as a percentage, or `-`.
pub(crate) fn signed_percent(bp: Option<i64>) -> String {
    bp.map_or_else(
        || "-".to_owned(),
        |bp| {
            let sign = if bp < 0 { "-" } else { "+" };
            let magnitude = bp.unsigned_abs();
            format!("{sign}{}.{:02}%", magnitude / 100, magnitude % 100)
        },
    )
}

pub(crate) fn decision_label(decision: &Decision) -> String {
    match decision {
        Decision::Accept => "accept".to_owned(),
        Decision::Reject(reason) => format!("reject({})", wire(reason)),
        Decision::Inconclusive(reason) => format!("inconclusive({})", wire(reason)),
    }
}

fn yes(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

/// A gate that may be unread: `yes`, `no`, or what its absence means.
fn yes_or(value: Option<bool>, absent: &'static str) -> &'static str {
    value.map_or(absent, yes)
}

pub(crate) fn comparison_table(comparison: &Comparison, out: &mut dyn Write) -> io::Result<()> {
    if comparison
        .policy
        .as_ref()
        .is_some_and(|policy| !policy.calibrated)
    {
        writeln!(out, "{}", super::UNCALIBRATED_BANNER)?;
    }
    writeln!(
        out,
        "compare {} v{}: baseline {}/{} candidate {}/{}",
        comparison.comparability.definition_id,
        comparison.comparability.comparability_version,
        comparison.baseline_run,
        comparison.baseline_cell,
        comparison.candidate_run,
        comparison.candidate_cell
    )?;
    writeln!(
        out,
        "{:<24} {:>5} {:>9} {:>9} {:>9}",
        "case", "pairs", "baseline", "candidate", "diff"
    )?;
    for case in &comparison.cases {
        writeln!(
            out,
            "{:<24} {:>5} {:>9} {:>9} {:>9}",
            case.case_id,
            case.pairs,
            percent(case.baseline_mean_bp),
            percent(case.candidate_mean_bp),
            signed_percent(case.diff_bp)
        )?;
    }
    writeln!(
        out,
        "improved {} tied {} worsened {} mean_diff {} pairs {} dropped_baseline {} dropped_candidate {} imputed {} p {}",
        comparison.improved,
        comparison.tied,
        comparison.worsened,
        signed_percent(comparison.mean_diff_bp),
        comparison.pairs,
        comparison.dropped_baseline,
        comparison.dropped_candidate,
        comparison.imputed,
        comparison
            .p_ppm
            .map_or_else(|| "-".to_owned(), |p| format!("{p}ppm"))
    )?;
    if let Some(policy) = &comparison.policy {
        let gates = &policy.gates;
        writeln!(
            out,
            "policy {}: {} (alpha {}ppm, calibrated {})",
            policy.report.policy_version,
            decision_label(&policy.report.decision),
            policy.report.alpha_effective_ppm,
            yes(policy.calibrated)
        )?;
        writeln!(
            out,
            "gates sufficient {} no_case_regression {} cost_ok {} significant {} min_effect {}",
            yes(gates.sufficient),
            yes(gates.no_case_regression),
            yes_or(gates.cost_ok, "skipped"),
            yes(gates.significant),
            yes_or(gates.min_effect, "undetermined")
        )?;
    }
    Ok(())
}

/// Seconds since `timestamp`, or `None` when it does not parse.
fn age(timestamp: &str, now: chrono::DateTime<chrono::Utc>) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|then| {
            (now - then.with_timezone(&chrono::Utc))
                .num_seconds()
                .max(0)
        })
}

/// Why an entry written by `pid` at `written_at` is stale, or `None` while
/// it is fresh: one window, the runner's (`STALE_WINDOW`, via `is_fresh`'s
/// rule).
fn staleness(
    fresh: bool,
    pid: u32,
    written_at: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    if fresh {
        String::new()
    } else if !gents::eval::runner::host_alive(pid) {
        format!(" stale: pid {pid} is not running")
    } else {
        format!(
            " stale: pid {pid} has not refreshed it for {}",
            age(written_at, now).map_or_else(
                || "an unknown time".to_owned(),
                |seconds| format!("{seconds}s")
            )
        )
    }
}

/// The in-flight lines under a watched report: which process holds the run,
/// and each slot in flight.
pub(crate) fn in_flight(
    progress: Option<&Progress>,
    now: chrono::DateTime<chrono::Utc>,
    out: &mut dyn Write,
) -> io::Result<()> {
    let Some(progress) = progress else {
        return writeln!(out, "no progress file: showing finished slots only");
    };
    if let Some(holder) = &progress.holder {
        writeln!(
            out,
            "held by pid {}{}",
            holder.pid,
            staleness(
                holder.is_fresh(STALE_WINDOW),
                holder.pid,
                &holder.written_at,
                now
            )
        )?;
    }
    if progress.slots.is_empty() {
        return writeln!(out, "in flight: none");
    }
    writeln!(out, "in flight:")?;
    for (trial_id, slot) in &progress.slots {
        let elapsed = age(&slot.started_at, now)
            .map_or_else(|| "?".to_owned(), |seconds| format!("{seconds}s"));
        writeln!(
            out,
            "  {} {} #{} attempt {} stage {} for {} ({trial_id}){}",
            slot.cell_id,
            slot.case_id,
            slot.trial_index,
            slot.attempt,
            slot.stage_id.as_deref().unwrap_or("-"),
            elapsed,
            staleness(
                is_fresh(slot, STALE_WINDOW),
                slot.pid,
                &slot.written_at,
                now
            )
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use gents::optimization::{Decision, InconclusiveReason, RejectReason};

    use super::{decision_label, human_bytes, signed_percent, yes_or};

    #[test]
    fn human_bytes_reads_in_binary_units() {
        assert_eq!(human_bytes(0), "0B");
        assert_eq!(human_bytes(512), "512B");
        assert_eq!(human_bytes(1_536), "1.5KiB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0MiB");
        assert_eq!(human_bytes(u64::MAX), "16777216.0TiB");
    }

    #[test]
    fn an_undetermined_min_effect_gate_reads_undetermined() {
        assert_eq!(yes_or(None, "undetermined"), "undetermined");
        assert_eq!(yes_or(Some(true), "undetermined"), "yes");
        assert_eq!(yes_or(Some(false), "undetermined"), "no");
        assert_eq!(yes_or(None, "skipped"), "skipped");
    }

    #[test]
    fn signed_percent_and_decision_labels_read_as_the_wire() {
        assert_eq!(signed_percent(Some(5_000)), "+50.00%");
        assert_eq!(signed_percent(Some(-1_234)), "-12.34%");
        assert_eq!(signed_percent(Some(0)), "+0.00%");
        assert_eq!(signed_percent(None), "-");
        assert_eq!(decision_label(&Decision::Accept), "accept");
        assert_eq!(
            decision_label(&Decision::Reject(RejectReason::NoImprovement)),
            "reject(no_improvement)"
        );
        assert_eq!(
            decision_label(&Decision::Inconclusive(InconclusiveReason::TooFewCases)),
            "inconclusive(too_few_cases)"
        );
    }
}
