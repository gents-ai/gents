use crate::workspace::{action_journal_prefix_legal, retry_allowed, ActionJournalEntry};

fn journal(states: &[String]) -> Vec<ActionJournalEntry> {
    states
        .iter()
        .enumerate()
        .map(|(index, state)| {
            ActionJournalEntry::new(
                u32::try_from(index).expect("journal index fits runtime"),
                serde_json::from_value(serde_json::Value::String(state.clone()))
                    .expect("runtime action journal state"),
            )
        })
        .collect()
}

#[test]
fn generated_retry_decisions_match_runtime_owner() {
    gents_lean_vocab::families::contracts_callback_retry::replay(|case| {
        retry_allowed(
            &case.state,
            &journal(&case.journal),
            u32::try_from(case.attempts).unwrap(),
            u32::try_from(case.max_attempts).unwrap(),
        )
    });
}

#[test]
fn generated_callback_journals_match_runtime_owner() {
    gents_lean_vocab::families::contracts_callback_journal::replay(|case| {
        action_journal_prefix_legal(&journal(&case.journal))
    });
}
