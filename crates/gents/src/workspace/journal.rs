use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ActionJournalState {
    Validated,
    Executing,
    EffectObserved,
    ResultDocsWritten,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionJournalEntry {
    pub index: u32,
    pub state: ActionJournalState,
}

impl ActionJournalEntry {
    pub fn new(index: u32, state: ActionJournalState) -> Self {
        Self { index, state }
    }
}

/// Action N+1 must not be Executing-or-later unless action N is ResultDocsWritten.
pub fn action_journal_prefix_legal(entries: &[ActionJournalEntry]) -> bool {
    let mut by_index: Vec<Option<ActionJournalState>> = Vec::new();
    for entry in entries {
        let idx = entry.index as usize;
        if by_index.len() <= idx {
            by_index.resize(idx + 1, None);
        }
        by_index[idx] = Some(entry.state);
    }
    for (idx, state) in by_index.iter().enumerate().skip(1) {
        let Some(state) = state else {
            continue;
        };
        if matches!(
            state,
            ActionJournalState::Executing
                | ActionJournalState::EffectObserved
                | ActionJournalState::ResultDocsWritten
        ) {
            if !matches!(
                by_index.get(idx - 1).copied().flatten(),
                Some(ActionJournalState::ResultDocsWritten)
            ) {
                return false;
            }
        }
    }
    true
}

/// Whether a failed invocation may run again: it has attempts left, and no
/// action observed its effect or wrote results, so running it again cannot
/// repeat anything.
pub fn retry_allowed(
    state: &str,
    journal: &[ActionJournalEntry],
    attempts: u32,
    max_attempts: u32,
) -> bool {
    state == "failed"
        && attempts < max_attempts
        && journal.iter().all(|entry| {
            !matches!(
                entry.state,
                ActionJournalState::EffectObserved | ActionJournalState::ResultDocsWritten
            )
        })
}

pub(crate) fn current_state(
    journal: &[ActionJournalEntry],
    index: u32,
) -> Option<ActionJournalState> {
    journal
        .iter()
        .rev()
        .find(|entry| entry.index == index)
        .map(|entry| entry.state)
}

pub(crate) fn advance(
    journal: &mut Vec<ActionJournalEntry>,
    index: u32,
    state: ActionJournalState,
) {
    journal.retain(|entry| entry.index != index);
    journal.push(ActionJournalEntry::new(index, state));
}
