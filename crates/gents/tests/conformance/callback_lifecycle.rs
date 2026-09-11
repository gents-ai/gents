//! Generated journal observations exercise the existing action-journal owner.
//! Invocation state, result emission and transactional rejection are separate
//! callback execution obligations; no test-local invocation model lives here.

use crate::lean_vocab_test::lean_callback_cases;
use gents::workspace::{action_journal_prefix_legal, ActionJournalEntry};

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
