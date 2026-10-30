use crate::documents::EvalSplit;
use crate::optimization::job::JournalEntry;
use crate::optimization::policy::{Decision, Mode};

pub(super) fn runs_started(journal: &[JournalEntry], split: EvalSplit) -> Vec<String> {
    journal
        .iter()
        .filter_map(|entry| match entry {
            JournalEntry::RunStarted {
                run_id,
                split: started,
                ..
            } if *started == split => Some(run_id.clone()),
            _ => None,
        })
        .collect()
}

pub(super) fn decisions(journal: &[JournalEntry]) -> Vec<(Option<u32>, Decision)> {
    journal
        .iter()
        .filter_map(|entry| match entry {
            JournalEntry::Decided {
                round, decision, ..
            } => Some((*round, *decision)),
            _ => None,
        })
        .collect()
}

pub(super) fn structural_rejects(journal: &[JournalEntry]) -> usize {
    journal
        .iter()
        .filter(|entry| matches!(entry, JournalEntry::StructuralReject { .. }))
        .count()
}

pub(super) fn assert_held_out_only_at_finalize(journal: &[JournalEntry]) {
    let held_out = journal
        .iter()
        .position(|entry| {
            matches!(
                entry,
                JournalEntry::RunStarted {
                    split: EvalSplit::HeldOut,
                    ..
                }
            )
        })
        .unwrap_or_else(|| panic!("no held-out run: {journal:#?}"));
    let last_round_scoped = journal
        .iter()
        .rposition(|entry| {
            matches!(
                entry,
                JournalEntry::RunStarted { round: Some(_), .. }
                    | JournalEntry::Proposed { .. }
                    | JournalEntry::StructuralReject { .. }
                    | JournalEntry::Decided { round: Some(_), .. }
                    | JournalEntry::BudgetExhausted { round: Some(_), .. }
            )
        })
        .unwrap_or_else(|| panic!("no round: {journal:#?}"));
    assert!(held_out > last_round_scoped, "{journal:#?}");
    assert!(
        matches!(
            &journal[held_out + 1..],
            [
                JournalEntry::Decided {
                    round: None,
                    mode: Mode::Confirm,
                    ..
                },
                JournalEntry::Finalized { .. },
            ]
        ),
        "{journal:#?}"
    );
}

pub(super) fn normalized(journal: Vec<JournalEntry>, job_id: &str) -> Vec<String> {
    journal
        .iter()
        .map(|entry| serde_json::to_string(entry).unwrap().replace(job_id, "JOB"))
        .collect()
}
