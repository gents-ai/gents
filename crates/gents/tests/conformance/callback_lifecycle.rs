//! Generated journal observations exercise the existing action-journal owner.
//! Invocation state, result emission and transactional rejection are separate
//! callback execution obligations; no test-local invocation model lives here.

use crate::lean_vocab_test::{lean_callback_cases, lean_callback_retry_cases};
use gents::workspace::{action_journal_prefix_legal, retry_allowed, ActionJournalEntry};

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
    let cases = lean_callback_retry_cases();
    assert_eq!(cases.len(), 192, "Lean must emit the whole retry matrix");
    assert!(cases.iter().any(|case| case.allowed));
    for case in cases {
        assert_eq!(
            retry_allowed(
                &case.state,
                &journal(&case.journal),
                case.attempts,
                case.max_attempts
            ),
            case.allowed,
            "{}: production retry decision",
            case.name,
        );
    }
}

#[test]
fn generated_callback_journals_match_runtime_owner() {
    let cases = lean_callback_cases();
    assert!(!cases.is_empty(), "Lean must emit callback journal cases");
    for case in cases {
        let journal = case
            .journal
            .iter()
            .enumerate()
            .map(|(index, state)| {
                ActionJournalEntry::new(
                    u32::try_from(index).expect("journal index fits runtime"),
                    serde_json::from_value(serde_json::Value::String(state.clone()))
                        .expect("runtime action journal state"),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            action_journal_prefix_legal(&journal),
            case.journal_prefix_legal,
            "{}: production journal prefix",
            case.name,
        );
    }
}
