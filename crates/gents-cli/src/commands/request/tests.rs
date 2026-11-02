use serde_json::Value;

use super::show_text::{push_request_output_text, render_request_show_text};
use super::show_views::{
    request_cancel_cause_view, request_header_view, terminal_source, terminal_timestamp,
    transition_history, RequestShowSnapshot,
};
use super::*;
use crate::request_helpers::CliOutputObservation;

fn output_envelope(output: CliOutputObservation) -> RequestOutputEnvelope {
    RequestOutputEnvelope {
        request: crate::request_helpers::CliRequestMetadata {
            request_doc_id: "request-doc".into(),
            request_id: "request".into(),
            node_did: "did:key:agent".into(),
            requester_did: Some("did:key:requester".into()),
            agent_id: Some("agent".into()),
            session_id: "session".into(),
            lifecycle_state: RequestLifecycleState::Completed,
            failure_reason: None,
            terminalized_at: Some("2026-09-22T00:00:00Z".into()),
        },
        output,
    }
}

fn show_snapshot(blocked: Option<gents::goal::blocked_turn::BlockedTurn>) -> RequestShowSnapshot {
    let row = json!({
        "request_id": "request", "lifecycle_state": "failed",
        "failure_reason": "provider usage limit reached (reset time not reported)",
    });
    RequestShowSnapshot {
        request: request_header_view(&row, None, Vec::new()).unwrap(),
        output: CliOutputObservation::Absent,
        cancel_cause: None,
        tool_calls: Vec::new(),
        backgrounded_tools: Vec::new(),
        native_executors_available: false,
        native_executors: Vec::new(),
        child_requests: Vec::new(),
        blocked,
    }
}

fn usage_limit(
    resets_at: Option<chrono::DateTime<chrono::Utc>>,
) -> gents::goal::blocked_turn::BlockedTurn {
    gents::goal::blocked_turn::BlockedTurn {
        reason: gents::goal::blocked_turn::BlockedReason::UsageLimit,
        account: Some(gents::goal::blocked_turn::BlockedAccount {
            label: "label-b".into(),
            provider: "claude-subscription".into(),
        }),
        profile: Some("main".into()),
        agents_on_profile: vec!["x".into(), "y".into()],
        resets_at,
        switch_command: Some("gents config profile set-account main".into()),
    }
}

fn show_now() -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    chrono::Utc.with_ymd_and_hms(2026, 9, 25, 16, 0, 0).unwrap()
}

#[test]
fn blocked_usage_limit_prints_account_reset_and_switch() {
    let resets_at = show_now() + chrono::Duration::minutes(133);
    let snapshot = show_snapshot(Some(usage_limit(Some(resets_at))));
    let text = render_request_show_text(&snapshot, show_now());
    assert!(
        text.contains(
            r#"blocked: usage limit on account "label-b", resets at 2026-09-25T18:13:00Z (in 2h13m)"#
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "profile main (used by 2 agents); switch: gents config profile set-account main"
        ),
        "{text}"
    );
    let value = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(
        value.pointer("/blocked/reason"),
        Some(&json!("usage_limit"))
    );
    let value = serde_json::to_value(show_snapshot(None)).unwrap();
    assert_eq!(value.get("blocked"), Some(&Value::Null));
}

#[test]
fn blocked_after_the_reset_prints_no_countdown() {
    let resets_at = show_now() - chrono::Duration::days(1);
    let text = render_request_show_text(
        &show_snapshot(Some(usage_limit(Some(resets_at)))),
        show_now(),
    );
    assert!(
        text.contains(
            r#"blocked: usage limit on account "label-b", reset at 2026-09-24T16:00:00Z (passed)"#
        ),
        "{text}"
    );
}

#[test]
fn blocked_without_a_reset_prints_reset_not_reported() {
    let text = render_request_show_text(&show_snapshot(Some(usage_limit(None))), show_now());
    assert!(
        text.contains(r#"blocked: usage limit on account "label-b", reset not reported"#),
        "{text}"
    );
    let text = render_request_show_text(&show_snapshot(None), show_now());
    assert!(!text.contains("blocked:"), "{text}");
}

#[test]
fn waited_request_json_has_one_output_observation_layer() {
    let result = wait_result(
        json!({"request_id": "request", "input": {"prompt": "not an answer"}}),
        &output_envelope(CliOutputObservation::TerminalNoMessage),
    )
    .unwrap();

    assert_eq!(
        result.pointer("/output/kind"),
        Some(&json!("terminal_no_message"))
    );
    assert!(result.pointer("/output/request").is_none());
    assert_eq!(
        result.pointer("/input/prompt"),
        Some(&json!("not an answer"))
    );
}

#[test]
fn request_text_renders_only_canonical_presentation_bytes() {
    let mut lines = Vec::new();
    push_request_output_text(
        &mut lines,
        &CliOutputObservation::Live {
            presentation: crate::CliOutputPresentation {
                body_markdown: "canonical answer".into(),
                reasoning_markdown: None,
            },
        },
    );
    let rendered = lines.join("\n");
    assert!(rendered.contains("state: live"));
    assert!(rendered.contains("canonical answer"));
}

#[test]
fn request_show_json_retains_typed_input_and_aggregate_budget() {
    let request = json!({
        "request_id": "request-one",
        "node_did": "did:key:agent",
        "agent_id": "default",
        "session_id": "session-one",
        "lifecycle_state": "pending",
        "input": {"selected_skill_ids":["review"]},
        "max_total_tokens": 250000,
    });
    let header = request_header_view(&request, None, Vec::new()).unwrap();
    let value = serde_json::to_value(header).unwrap();

    assert_eq!(value["input"]["selected_skill_ids"], json!(["review"]));
    assert_eq!(value["max_total_tokens"], 250000);
}

#[test]
fn request_show_json_retains_null_aggregate_budget_key() {
    let request = json!({
        "request_id": "request-one",
        "node_did": "did:key:agent",
        "agent_id": "default",
        "session_id": "session-one",
        "lifecycle_state": "pending",
    });
    let header = request_header_view(&request, None, Vec::new()).unwrap();
    let value = serde_json::to_value(header).unwrap();

    assert!(value.get("max_total_tokens").is_some());
    assert!(value["max_total_tokens"].is_null());
}

#[test]
fn transition_history_does_not_emit_processing_self_loop() {
    let request = json!({
        "lifecycle_state": "processing",
        "claimed_at": "2026-05-20T10:00:01Z",
    });

    let transitions = transition_history(&request, None);

    assert!(transitions
        .iter()
        .any(|transition| transition.action == "claim"));
    assert!(transitions
        .iter()
        .any(|transition| transition.action == "begin_inference"));
    assert!(!transitions
        .iter()
        .any(|transition| transition.action == "advance"));
    assert!(!transitions.iter().any(|transition| transition
        .from
        .as_deref()
        .is_some_and(|from| from == transition.to)));
}

#[test]
fn transition_history_keeps_interrupt_attempt_on_terminal_completion() {
    let request = json!({
        "lifecycle_state": "completed",
        "claimed_at": "2026-05-20T10:00:01Z",
        "interrupt_requested_at": "2026-05-20T10:00:02Z",
        "terminalized_at": "2026-05-20T10:00:03Z",
    });

    let transitions = transition_history(&request, Some("finish"));
    let interrupt = transitions
        .iter()
        .find(|transition| transition.action == "interrupt_requested")
        .expect("interrupt request transition should be present");
    assert_eq!(interrupt.from, None);
    assert_eq!(interrupt.to, "completed");
    assert_eq!(interrupt.at.as_deref(), Some("2026-05-20T10:00:02Z"));
    assert!(interrupt
        .note
        .as_deref()
        .is_some_and(|note| note.contains("terminal snapshot")));

    let finish = transitions
        .iter()
        .find(|transition| transition.action == "finish")
        .expect("finish transition should be present");
    assert_eq!(finish.from, None);
    assert_eq!(finish.at.as_deref(), Some("2026-05-20T10:00:03Z"));
}

#[test]
fn terminal_timestamp_uses_canonical_request_fields() {
    let request = json!({
        "lifecycle_state": "completed",
        "terminalized_at": "2026-05-20T10:00:03Z",
        "valid_until": "2026-05-20T10:00:05Z",
        "interrupt_requested_at": "2026-05-20T10:00:02Z",
    });

    assert_eq!(
        terminal_timestamp(&request, "finish").as_deref(),
        Some("2026-05-20T10:00:03Z")
    );
    assert_eq!(
        terminal_timestamp(&request, "fail").as_deref(),
        Some("2026-05-20T10:00:03Z")
    );
    assert_eq!(
        terminal_timestamp(&request, "expire").as_deref(),
        Some("2026-05-20T10:00:03Z")
    );
    assert_eq!(
        terminal_timestamp(&request, "interrupt").as_deref(),
        Some("2026-05-20T10:00:02Z")
    );

    let expiring = json!({
        "lifecycle_state": "dead",
        "valid_until": "2026-05-20T10:00:05Z",
    });
    assert_eq!(
        terminal_timestamp(&expiring, "expire").as_deref(),
        Some("2026-05-20T10:00:05Z")
    );

    assert_eq!(terminal_source("finish"), "AgentRequest.terminalized_at");
    assert_eq!(terminal_source("fail"), "AgentRequest.terminalized_at");
    assert_eq!(terminal_source("expire"), "AgentRequest.terminalized_at");
    assert_eq!(
        terminal_source("interrupt"),
        "AgentRequest.interrupt_requested_at"
    );
}

#[test]
fn terminal_transitions_do_not_guess_prior_state() {
    let cases = [
        ("completed", "finish"),
        ("failed", "fail"),
        ("dead", "expire"),
        ("interrupted", "interrupt"),
        ("superseded", "supersede"),
    ];

    for (state, action) in cases {
        let request = json!({
            "lifecycle_state": state,
            "claimed_at": "2026-05-20T10:00:01Z",
            "interrupt_requested_at": "2026-05-20T10:00:02Z",
            "valid_until": "2026-05-20T10:00:03Z",
            "deadline": "2026-05-20T10:00:04Z",
        });
        let transitions = transition_history(&request, Some(action));
        let terminal = transitions
            .iter()
            .find(|transition| transition.action == action)
            .expect("terminal transition should be present");

        assert_eq!(
            terminal.from, None,
            "{state} should not infer a prior state"
        );
    }
}

#[test]
fn request_cancel_cause_is_absent_for_uncancelled_requests() {
    let request = json!({
        "lifecycle_state": "processing",
    });

    assert!(request_cancel_cause_view(&request).is_none());
}

#[test]
fn request_cancel_cause_uses_request_fields_on_interrupted_requests() {
    let request = json!({
        "lifecycle_state": "interrupted",
    });
    let cancel = request_cancel_cause_view(&request)
        .expect("interrupted requests should render CancelCause");
    assert_eq!(cancel.cause, "unknown");
    assert_eq!(cancel.interrupt_requested_at, None);

    let request = json!({
        "lifecycle_state": "interrupted",
        "cancel_cause": "operator_interrupt",
        "interrupt_requested_at": "2026-05-20T10:00:03Z",
    });
    let cancel = request_cancel_cause_view(&request)
        .expect("interrupted requests should render CancelCause");
    assert_eq!(cancel.cause, "operator_interrupt");
    assert_eq!(
        cancel.interrupt_requested_at.as_deref(),
        Some("2026-05-20T10:00:03Z")
    );
}
