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

fn production<T>(wire: &T) -> RequestInput
where
    T: serde::Serialize,
{
    gents_lean_vocab::production_request_input(wire)
}

fn authored(input: &RequestInput) -> AuthoredRequestInput {
    AuthoredRequestInput {
        selected_skill_ids: input.selected_skill_ids.clone(),
        captured_context: input.captured_context.clone(),
        attachments: input.attachments.clone(),
    }
}

#[test]
fn request_input_derivations_follow_the_lean_owner() {
    use gents_lean_vocab::families::request_input_contracts_derivation as derivation;
    derivation::replay(|case: &derivation::DerivationInput| {
        let input = production(&case.input);
        let has_authored_input = input.has_authored_input(&case.authored_text);
        assert_eq!(
            authored(&input).has_authored_input(&case.authored_text),
            has_authored_input,
            "{}: replacement input presence",
            case.name
        );
        let native = input.authored_message(&case.authored_text).unwrap();
        let captures_well_formed = input
            .captured_context
            .iter()
            .all(CapturedContext::is_well_formed);
        assert_eq!(
            CapturedContext::from_message(&native).ok(),
            captures_well_formed.then(|| input.captured_context.clone()),
            "{}: immutable document codec",
            case.name
        );
        let queue: RequestQueue = gents_lean_vocab::family::recode(&case.continuation_queue);
        let goal = case
            .continuation_goal
            .as_ref()
            .map(gents_lean_vocab::family::recode);
        let edit_queue: RequestQueue = gents_lean_vocab::family::recode(&case.edit_queue);
        let edit_authored = authored(&production(&case.edit_authored_input));
        derivation::Derived {
            captured_input_compatible: input.captured_input_compatible(case.runtime_input_version),
            goal_assignment_compatible: input.goal_assignment_compatible(case.runtime_goal_version),
            captures_well_formed,
            attachments_well_formed: input.attachments_well_formed(),
            has_authored_input,
            native_input: gents_lean_vocab::family::Json(serde_json::to_value(&native).unwrap()),
            agent_new_local_cwd: input.agent_new_cwd(true),
            agent_new_remote_cwd: input.agent_new_cwd(false),
            continuation: gents_lean_vocab::request_input_wire(&input.continuation(
                queue,
                goal,
                &case.continuation_parent_doc_id,
            )),
            fresh_input: gents_lean_vocab::request_input_wire(&input.for_fresh_session()),
            reissued_input: gents_lean_vocab::request_input_wire(
                &input.reissued(&case.reissued_source_doc_id),
            ),
            edited_input: gents_lean_vocab::request_input_wire(&input.edited(
                &edit_authored,
                &case.reissued_source_doc_id,
                edit_queue.clone(),
            )),
            edited_from_empty: gents_lean_vocab::request_input_wire(
                &RequestInput::default().edited(
                    &authored(&input),
                    &case.reissued_source_doc_id,
                    edit_queue,
                ),
            ),
        }
    });
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
fn signed_request_input_fields_follow_the_lean_encoding() {
    use crate::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};
    use gents_lean_vocab::families::request_input_contracts_signed_fields as signed;

    fn read_length(cursor: &mut &[u8]) -> usize {
        let length = cursor.iter().position(|byte| *byte != 0).unwrap();
        assert_eq!(cursor[length], 255, "invalid canonical length delimiter");
        *cursor = &cursor[length + 1..];
        length
    }

    signed::replay(|case: &signed::SigningInput| {
        let decoded = production(&case.input);
        let encoded = serde_json::to_value(&decoded).unwrap();
        if decoded
            .queue
            .as_ref()
            .is_some_and(|queue| queue.delivery.is_queue())
        {
            assert!(
                encoded["queue"].get("delivery").is_none(),
                "{}: compact default delivery",
                case.name
            );
        }
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
        request.input = serde_json::from_value(encoded)
            .unwrap_or_else(|error| panic!("{}: encoded input decodes: {error}", case.name));
        assert_eq!(request.input, decoded, "{}: lossless wire", case.name);
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
            "{}: trailing signature payload bytes",
            case.name
        );
        let start = fields
            .iter()
            .position(|field| field == b"input-fields-follow-this-content")
            .unwrap()
            + 1;
        let origin = fields
            .windows(2)
            .rposition(|pair| pair[0] == [1] && pair[1] == b"interactive")
            .unwrap_or_else(|| panic!("{}: execution origin follows the input", case.name));
        signed::Signed {
            canonical_input_fields: fields[start..origin]
                .iter()
                .map(|field| field.iter().map(|byte| u64::from(*byte)).collect())
                .collect(),
        }
    });
}
