//! Plain text for `gents optimization`: one line per journal entry, and the
//! job table `run` and `show` end with.

use std::io::{self, Write};

use gents::eval::report::is_placeholder;
use gents::optimization::{Decision, JobState, JobView, JournalEntry, PolicyV2};

use crate::commands::eval::render::{decision_label, signed_percent, wire};
use crate::commands::eval::UNCALIBRATED_BANNER;

/// Shown in place of a recomputed decision when `show` could not recompute
/// one: the job's definition was deleted or no longer validates.
const NOT_RECOMPUTABLE: &str = "not recomputable: definition changed or runs invalidated";

/// [`UNCALIBRATED_BANNER`] when `policy` is the placeholder defaults.
pub(crate) fn uncalibrated_banner(policy: &PolicyV2, out: &mut dyn Write) -> io::Result<()> {
    if is_placeholder(policy) {
        writeln!(out, "{UNCALIBRATED_BANNER}")?;
    }
    Ok(())
}

fn round_label(round: Option<u32>) -> String {
    round.map_or_else(|| "final".to_owned(), |round| round.to_string())
}

fn state_label(state: &JobState) -> String {
    match state {
        JobState::Failed { reason } => format!("failed ({reason})"),
        other => other.label().to_owned(),
    }
}

fn recomputed_label(recomputed: Option<&Decision>) -> String {
    recomputed.map_or_else(|| NOT_RECOMPUTABLE.to_owned(), decision_label)
}

pub(crate) fn journal_line(entry: &JournalEntry) -> String {
    match entry {
        JournalEntry::Frozen => "frozen".to_owned(),
        JournalEntry::RunStarted {
            run_id,
            round,
            split,
        } => format!(
            "round {} run started {run_id} split {}",
            round_label(*round),
            wire(split)
        ),
        JournalEntry::Proposed {
            round,
            rationale,
            candidate_digest,
            ..
        } => format!("round {round} proposed {candidate_digest}: {rationale}"),
        JournalEntry::StructuralReject { round, diagnostics } => {
            format!("round {round} structurally rejected: {diagnostics}")
        }
        JournalEntry::Decided {
            round,
            attempt,
            run_ids,
            mode,
            decision,
            summary,
            ..
        } => format!(
            "round {} attempt {attempt} {} {}: improved {} tied {} worsened {} mean_diff {} p {} alpha {}ppm{} runs {}",
            round_label(*round),
            wire(mode),
            decision_label(decision),
            summary.improved,
            summary.tied,
            summary.worsened,
            signed_percent(summary.mean_diff_bp),
            summary
                .p_ppm
                .map_or_else(|| "-".to_owned(), |p| format!("{p}ppm")),
            summary.alpha_effective_ppm,
            if summary.cost_skipped { " cost skipped" } else { "" },
            run_ids.join(",")
        ),
        JournalEntry::BudgetExhausted { round, reason } => {
            format!("round {} budget exhausted: {reason}", round_label(*round))
        }
        JournalEntry::Finalized { state } => format!("finalized {}", state_label(state)),
        JournalEntry::Promoted {
            by,
            target_digest,
            previous_digest,
            ..
        } => format!("promoted by {by}: target {target_digest} (was {previous_digest})"),
        JournalEntry::PromotionRefused { drifted } => format!(
            "promotion refused: {} documents drifted since the freeze",
            drifted.len()
        ),
        JournalEntry::Reverted { by } => format!("reverted by {by}"),
    }
}

pub(crate) fn job_table(view: &JobView, out: &mut dyn Write) -> io::Result<()> {
    uncalibrated_banner(&view.job.origin.policy, out)?;
    writeln!(
        out,
        "job {} state {} rounds_used {}",
        view.job.job_id,
        state_label(&view.state),
        view.rounds_used
    )?;
    match &view.checkpoint {
        Some(checkpoint) => writeln!(
            out,
            "checkpoint round {} pack {}",
            checkpoint.round, checkpoint.pack_digest
        )?,
        None => writeln!(out, "checkpoint none")?,
    }
    if view.definition_changed {
        writeln!(
            out,
            "the eval definition changed since the job froze it: the recomputations below read a different instrument, or none was possible"
        )?;
    }
    writeln!(out, "journal:")?;
    for (index, entry) in view.job.journal.iter().enumerate() {
        writeln!(out, "{:>4}. {}", index + 1, journal_line(entry))?;
    }
    writeln!(out, "decisions:")?;
    for decision in &view.decisions {
        writeln!(
            out,
            "  round {} attempt {} {} journaled {} recomputed {}{}{} runs {}",
            round_label(decision.round),
            decision.attempt,
            wire(&decision.mode),
            decision_label(&decision.journaled),
            recomputed_label(decision.recomputed.as_ref()),
            if decision.mismatch { " MISMATCH" } else { "" },
            if decision.invalidated {
                " (a run it read is invalidated)"
            } else {
                ""
            },
            decision.run_ids.join(",")
        )?;
    }
    Ok(())
}
