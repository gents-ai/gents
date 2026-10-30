use super::{visible_context, ReviewInput, MAX_INPUT_BYTES};
use crate::inference::llm::message::Message;

fn bounded(history: &[Message]) -> ReviewInput {
    ReviewInput::bounded(
        "action-1".into(),
        "Ask before changing files outside this workspace.".into(),
        serde_json::json!({"command": ["cargo", "test"], "workspace": "/project"}),
        "Fix the failing test.",
        history.iter(),
    )
    .unwrap()
}

#[test]
fn long_review_keeps_recent_whole_entries_and_complete_required_fields() {
    let history: Vec<_> = (0..200)
        .map(|index| Message::user(format!("{index}: {}", "\"\\\n🦀".repeat(200))))
        .collect();
    let input = bounded(&history);
    let wire = serde_json::to_vec(&input).unwrap();
    assert!(wire.len() <= MAX_INPUT_BYTES);
    assert!(input.omitted_history_entries > 0);
    let expected: Vec<_> = history[input.omitted_history_entries..]
        .iter()
        .filter_map(visible_context)
        .chain(std::iter::once(
            "Current request:\nFix the failing test.".into(),
        ))
        .collect();
    assert_eq!(input.conversation, expected);
    let decoded: ReviewInput = serde_json::from_slice(&wire).unwrap();
    assert_eq!(decoded.action_id, "action-1");
    assert_eq!(
        decoded.policy,
        "Ask before changing files outside this workspace."
    );
    assert_eq!(
        decoded.action,
        serde_json::json!({"command": ["cargo", "test"], "workspace": "/project"})
    );
}

#[test]
fn oversized_required_fields_are_rejected_without_truncation() {
    for field in 0..3 {
        let oversized = "x".repeat(MAX_INPUT_BYTES);
        let (policy, action, current) = match field {
            0 => (oversized, serde_json::Value::Null, "request".into()),
            1 => (
                "policy".into(),
                serde_json::json!({"payload": oversized}),
                "request".into(),
            ),
            _ => ("policy".into(), serde_json::Value::Null, oversized),
        };
        let result = ReviewInput::bounded("id".into(), policy, action, &current, [].iter());
        assert!(result.is_err());
    }
}

#[test]
fn oversized_recent_entry_does_not_substitute_older_context() {
    let input = bounded(&[
        Message::user("Older small entry"),
        Message::user("x".repeat(MAX_INPUT_BYTES)),
        Message::user("Most recent entry"),
    ]);
    assert_eq!(input.omitted_history_entries, 2);
    assert_eq!(
        input.conversation,
        [
            "Context:\nMost recent entry",
            "Current request:\nFix the failing test."
        ]
    );
}

#[test]
fn omitted_count_includes_hidden_rows_without_exposing_them() {
    let input = bounded(&[
        Message::system("Private system instructions"),
        Message::user(""),
        Message::user("Visible entry"),
    ]);
    assert_eq!(input.omitted_history_entries, 2);
    assert_eq!(
        input.conversation,
        [
            "Context:\nVisible entry",
            "Current request:\nFix the failing test."
        ]
    );
}
