use super::*;
use gents_lean_vocab::families::goal_request_assignment_contracts_fresh_session::{
    FreshSessionResult, Reference, ResubmissionInput,
};
use serde_json::json;

fn modeled_reference(reference: &Reference) -> GoalAssignmentInput {
    GoalAssignmentInput {
        goal_id: format!("goal:{}", reference.goal),
        root_request_doc_id: reference.root.map(|root| format!("request:{root}")),
    }
}

fn observed_reference(reference: &GoalAssignmentInput) -> Reference {
    Reference {
        goal: reference
            .goal_id
            .strip_prefix("goal:")
            .and_then(|goal| goal.parse().ok())
            .unwrap_or_else(|| panic!("unmodeled goal id {}", reference.goal_id)),
        root: reference.root_request_doc_id.as_ref().map(|root| {
            root.strip_prefix("request:")
                .and_then(|root| root.parse().ok())
                .unwrap_or_else(|| panic!("unmodeled root {root}"))
        }),
    }
}

#[test]
fn generated_fresh_session_resubmissions_drop_invocation_lineage() {
    gents_lean_vocab::families::goal_request_assignment_contracts_fresh_session::replay::<
        SessionCreationIntent,
    >(|case| {
        let modeled = &case.input;
        let payload = modeled.authored;
        let mut input: RequestInput = serde_json::from_value(json!({
            "cwd":format!("/workspace/{payload}"), "selected_skill_ids":[format!("skill:{payload}")],
            "initial_title":{"text":format!("Resend {payload}"),"source":"user"},
            "captured_context":[{"kind":"selection","source":"editor","capturedAt":"2026-10-21T00:00:00Z",
                "uri":"file:///workspace/main.rs","terminalName":null,"workspaceUri":"file:///workspace",
                "version":4,"unsaved":true,"range":{"start":{"line":1,"character":0},"end":{"line":2,"character":0}},
                "content":format!("private unsaved payload {payload}")}]
        })).unwrap();
        input.session_creation = modeled.creation;
        assert_eq!(
            input.reissued("source-doc").session_creation,
            input.session_creation,
            "{modeled:?}"
        );
        input.goal_assignment = modeled.reference.as_ref().map(modeled_reference);
        input.queue = modeled.queued.then(|| RequestQueue {
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
        input.goal_continuation = modeled.continuation.then_some(GoalContinuationInput {
            sequence: 7,
            wrapup: true,
        });
        let actual = input.for_fresh_session();
        assert_eq!(
            actual.selected_skill_ids, input.selected_skill_ids,
            "{modeled:?}"
        );
        assert_eq!(actual.cwd, input.cwd, "{modeled:?}");
        assert_eq!(actual.initial_title, input.initial_title, "{modeled:?}");
        assert_eq!(
            actual.captured_context, input.captured_context,
            "{modeled:?}"
        );
        assert_eq!(actual.for_fresh_session(), actual, "{modeled:?}");
        FreshSessionResult {
            expected: ResubmissionInput {
                creation: actual.session_creation,
                reference: actual.goal_assignment.as_ref().map(observed_reference),
                queued: actual.queue.is_some(),
                continuation: actual.goal_continuation.is_some(),
                authored: actual
                    .cwd
                    .as_deref()
                    .and_then(|cwd| cwd.strip_prefix("/workspace/"))
                    .and_then(|authored| authored.parse().ok())
                    .unwrap_or_else(|| panic!("{modeled:?}: authored payload lost")),
            },
        }
    });
}

#[test]
fn agent_new_directory_inheritance_matches_the_model() {
    let cases = &gents_lean_vocab::lean_contract_snapshot().request_input_cases;
    assert!(cases.iter().any(|case| case["input"]["cwd"].is_string()));
    for case in cases {
        let input: RequestInput = serde_json::from_value(case["input"].clone()).unwrap();
        for (same_node, expected) in [
            (true, &case["expected_agent_new_local_cwd"]),
            (false, &case["expected_agent_new_remote_cwd"]),
        ] {
            assert_eq!(
                serde_json::to_value(input.agent_new_cwd(same_node)).unwrap(),
                *expected,
                "{}: same node = {same_node}",
                case["name"]
            );
        }
    }
}

#[test]
fn captured_input_compatibility_matches_the_executable_owner() {
    for case in &gents_lean_vocab::lean_contract_snapshot().request_input_cases {
        let input: RequestInput = serde_json::from_value(case["input"].clone()).unwrap();
        assert_eq!(
            input.captured_input_compatible(case["runtime_input_version"].as_u64()),
            case["expected_captured_input_compatible"]
                .as_bool()
                .unwrap(),
            "{}: runtime input compatibility",
            case["name"],
        );
    }
}

#[test]
fn goal_assignment_compatibility_matches_the_executable_owner() {
    for case in &gents_lean_vocab::lean_contract_snapshot().request_input_cases {
        let input: RequestInput = serde_json::from_value(case["input"].clone()).unwrap();
        assert_eq!(
            input.goal_assignment_compatible(case["runtime_goal_version"].as_u64()),
            case["expected_goal_assignment_compatible"]
                .as_bool()
                .unwrap(),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn runtime_continuation_inputs_match_the_lean_owner() {
    for case in &gents_lean_vocab::lean_contract_snapshot().request_input_cases {
        let name = case["name"].as_str().unwrap();
        let parent: RequestInput = serde_json::from_value(case["input"].clone()).unwrap();
        let queue = serde_json::from_value(case["continuation_queue"].clone()).unwrap();
        let goal = serde_json::from_value(case["continuation_goal"].clone()).unwrap();
        let expected: RequestInput =
            serde_json::from_value(case["expected_continuation"].clone()).unwrap();
        assert_eq!(
            parent.continuation(
                queue,
                goal,
                case["continuation_parent_doc_id"].as_str().unwrap()
            ),
            expected,
            "{name}"
        );
        let expected_fresh: RequestInput =
            serde_json::from_value(case["expected_fresh_input"].clone()).unwrap();
        assert_eq!(
            parent.for_fresh_session(),
            expected_fresh,
            "{name}: fresh-session input"
        );
        let expected_reissued: RequestInput =
            serde_json::from_value(case["expected_reissued_input"].clone()).unwrap();
        assert_eq!(
            parent.reissued(case["reissued_source_doc_id"].as_str().unwrap()),
            expected_reissued,
            "{name}: same-session reissuance"
        );
    }
}

#[test]
fn lean_request_inputs_decode_without_losing_explicit_issuance_facts() {
    let cases = &gents_lean_vocab::lean_contract_snapshot().request_input_cases;
    assert!(!cases.is_empty());
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let input: RequestInput = serde_json::from_value(case["input"].clone())
            .unwrap_or_else(|error| panic!("{name}: typed input: {error}"));
        let encoded = serde_json::to_value(&input).unwrap();
        for (field, value) in case["input"].as_object().unwrap() {
            if !value.is_null() && value.as_array().is_none_or(|xs| !xs.is_empty()) {
                if field == "queue" {
                    let modeled: crate::request_input::RequestQueue =
                        serde_json::from_value(value.clone()).unwrap();
                    let decoded = input.queue.as_ref().expect("modeled queue survives decode");
                    assert_eq!(decoded.delivery, modeled.delivery, "{name}: queue.delivery");
                    assert_eq!(decoded.position, modeled.position, "{name}: queue.position");
                    for (key, value) in value.as_object().unwrap() {
                        if key == "delivery" && modeled.delivery.is_queue() {
                            assert!(
                                encoded[field].get(key).is_none(),
                                "{name}: compact default delivery"
                            );
                        } else if !value.is_null() {
                            assert_eq!(&encoded[field][key], value, "{name}: queue.{key}");
                        }
                    }
                } else {
                    assert_eq!(&encoded[field], value, "{name}: {field}");
                }
            }
        }
        if let Some(goal) = input.goal_continuation {
            assert_eq!(
                encoded["goal_continuation"]["wrapup"], goal.wrapup,
                "{name}: false is an explicit original issuance fact"
            );
        }
    }
}

#[test]
fn request_input_rejects_legacy_controls_and_incomplete_goal_facts() {
    for value in [
        json!({"temperature": 0.7}),
        json!({"metadata": {"queue": {"source": "goal"}}}),
        json!({"skill_excludes": ["configured-skill"]}),
        json!({"goal_continuation": {"sequence": 1}}),
        json!({"goal_continuation": {"wrapup": false}}),
        json!({"goal_continuation": {"sequence": 1, "wrapup": false, "goal_id": "forged"}}),
    ] {
        assert!(
            serde_json::from_value::<RequestInput>(value.clone()).is_err(),
            "{value}"
        );
    }
    let compact = serde_json::to_value(RequestInput::default()).unwrap();
    assert_eq!(compact, json!({}));
}

#[test]
fn signed_request_input_fields_match_lean_bytes() {
    use crate::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};

    fn read_length(cursor: &mut &[u8]) -> usize {
        let length = cursor.iter().position(|byte| *byte != 0).unwrap();
        assert_eq!(cursor[length], 255, "invalid canonical length delimiter");
        *cursor = &cursor[length + 1..];
        length
    }

    for case in &gents_lean_vocab::lean_contract_snapshot().request_input_cases {
        let name = case["name"].as_str().unwrap();
        let mut request = AgentRequestCreate::base(
            crate::request_admission::RequestPurpose::Normal,
            "request-1",
            "did:key:agent",
            "did:key:agent",
            "default",
            "session-1",
            "input-fields-follow-this-content",
            "interactive",
            "2026-08-30T00:00:00Z",
            AgentRequestAdmissionRecord::local_self("did:key:agent"),
        );
        request.input = serde_json::from_value(case["input"].clone()).unwrap();
        let expected: Vec<Vec<u8>> =
            serde_json::from_value(case["canonical_input_fields"].clone()).unwrap();
        let payload = request.signing_payload();
        let mut cursor = payload.as_slice();
        let count = read_length(&mut cursor);
        let mut fields = Vec::new();
        for _ in 0..count {
            let length = read_length(&mut cursor);
            fields.push(cursor[..length].to_vec());
            cursor = &cursor[length..];
        }
        assert!(
            cursor.is_empty(),
            "{name}: trailing signature payload bytes"
        );
        let start = fields
            .iter()
            .position(|field| field == b"input-fields-follow-this-content")
            .unwrap()
            + 1;
        assert_eq!(
            &fields[start..start + expected.len()],
            expected.as_slice(),
            "{name}: real signing owner must use Lean's typed-input framing"
        );
        assert_eq!(fields[start + expected.len()], vec![1], "{name}");
        assert_eq!(fields[start + expected.len() + 1], b"interactive", "{name}");
    }
}

#[test]
fn captured_context_structure_matches_the_executable_input_owner() {
    let cases = &gents_lean_vocab::lean_contract_snapshot().request_input_cases;
    assert!(cases
        .iter()
        .any(|case| case["name"] == "captured-selection"));
    for case in cases {
        let input: RequestInput = serde_json::from_value(case["input"].clone()).unwrap();
        assert_eq!(
            input
                .captured_context
                .iter()
                .all(|context| context.is_well_formed()),
            case["expected_captures_well_formed"].as_bool().unwrap(),
            "{}: actual captured-context structural owner",
            case["name"],
        );
    }
}

#[test]
fn generated_queue_sources_parse_and_key_through_the_protocol_owner() {
    gents_lean_vocab::families::contracts_r6_queue_source::replay(|i| {
        let parsed = serde_json::from_value::<QueueSource>(serde_json::json!(i.queue_source)).ok();
        let enqueue = i.action == "enqueue_background_completion";
        gents_lean_vocab::families::contracts_r6_queue_source::QueueSourceOutput {
            legal: parsed.is_some() == enqueue,
            queue_key: parsed
                .filter(|_| enqueue)
                .map(|source| source.queue_key(&i.session_key)),
        }
    });
}
