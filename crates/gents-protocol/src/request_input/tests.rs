use super::*;
use serde_json::{json, Value};

fn reference(value: &Value) -> Option<GoalAssignmentInput> {
    (!value.is_null()).then(|| GoalAssignmentInput {
        goal_id: format!("goal:{}", value["goal"].as_u64().unwrap()),
        root_request_doc_id: value["root"].as_u64().map(|root| format!("request:{root}")),
    })
}

#[test]
fn physical_steer_target_is_signed_and_removed_from_fresh_sessions() {
    use crate::request_admission::{
        AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
    };
    let mut request = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "request",
        "node",
        "node",
        "agent",
        "session",
        "message",
        "interactive",
        "2026-10-29T00:00:00Z",
        AgentRequestAdmissionRecord::local_self("node"),
    );
    let original = request.signing_payload();
    request.input.steer_target_request_doc_id = Some("document-a".to_owned());
    let signed = request.signing_payload();
    assert_ne!(signed, original);
    request.input.steer_target_request_doc_id = Some("document-b".to_owned());
    assert_ne!(request.signing_payload(), signed);
    request.input = request.input.for_fresh_session();
    assert!(request.input.steer_target_request_doc_id.is_none());
    assert_eq!(request.signing_payload(), original);
}

#[test]
fn generated_fresh_session_resubmissions_drop_invocation_lineage() {
    let snapshot: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = snapshot["goal_fresh_session_input_cases"]
        .as_array()
        .unwrap();
    assert_eq!(cases.len(), 12);
    for case in cases {
        let modeled = &case["input"];
        let payload = modeled["authored"].as_u64().unwrap();
        let mut input: RequestInput = serde_json::from_value(json!({
            "cwd":format!("/workspace/{payload}"), "selected_skill_ids":[format!("skill:{payload}")],
            "initial_title":{"text":format!("Resend {payload}"),"source":"user"},
            "captured_context":[{"kind":"selection","source":"editor","capturedAt":"2026-10-21T00:00:00Z",
                "uri":"file:///workspace/main.rs","terminalName":null,"workspaceUri":"file:///workspace",
                "version":4,"unsaved":true,"range":{"start":{"line":1,"character":0},"end":{"line":2,"character":0}},
                "content":format!("private unsaved payload {payload}")}]
        })).unwrap();
        input.goal_assignment = reference(&modeled["reference"]);
        input.queue = modeled["queued"].as_bool().unwrap().then(|| RequestQueue {
            source: QueueSource::BackgroundCompletion,
            policy: QueuePolicy::Coalesce,
            delivery: QueueDelivery::Steer,
            position: Some(QueuePosition {
                slot_request_doc_id: "old-slot".into(),
                replaces_request_doc_id: "old-request".into(),
            }),
            key: Some("old-queue-key".into()),
            queued_after_request_id: Some("old-active".into()),
            interrupted_request_id: Some("old-interrupted".into()),
            background_completion_wake_version: Some(1),
        });
        input.goal_continuation =
            modeled["continuation"]
                .as_bool()
                .unwrap()
                .then_some(GoalContinuationInput {
                    sequence: 7,
                    wrapup: true,
                });
        let actual = input.for_fresh_session();
        let expected = &case["expected"];
        assert_eq!(
            actual.goal_assignment,
            reference(&expected["reference"]),
            "{case}"
        );
        assert_eq!(
            actual.queue.is_some(),
            expected["queued"].as_bool().unwrap(),
            "{case}"
        );
        assert_eq!(
            actual.goal_continuation.is_some(),
            expected["continuation"].as_bool().unwrap(),
            "{case}"
        );
        assert_eq!(expected["authored"], modeled["authored"], "{case}");
        assert_eq!(
            actual.selected_skill_ids, input.selected_skill_ids,
            "{case}"
        );
        assert_eq!(actual.cwd, input.cwd, "{case}");
        assert_eq!(actual.initial_title, input.initial_title, "{case}");
        assert_eq!(actual.captured_context, input.captured_context, "{case}");
        assert_eq!(actual.for_fresh_session(), actual, "{case}");
    }
}
