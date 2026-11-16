use super::*;
use gents_lean_vocab::families::goal_request_assignment_contracts_input_binding as family;
use gents_protocol::request_input::{GoalAssignmentInput, RequestInput};
use serde_json::json;

fn reference(value: Option<&family::Reference>) -> Option<GoalAssignmentInput> {
    value.map(|value| GoalAssignmentInput {
        goal_id: format!("goal:{}", value.goal),
        root_request_doc_id: value.root.map(|root| format!("request-document:{root}")),
    })
}

fn modeled(value: Option<&GoalAssignmentInput>) -> Option<family::Reference> {
    value.map(|value| family::Reference {
        goal: value
            .goal_id
            .strip_prefix("goal:")
            .and_then(|goal| goal.parse().ok())
            .expect("a modeled goal id"),
        root: value.root_request_doc_id.as_deref().map(|root| {
            root.strip_prefix("request-document:")
                .and_then(|root| root.parse().ok())
                .expect("a modeled root request")
        }),
    })
}

fn observation(reference: Option<GoalAssignmentInput>) -> SessionGoalObservation {
    SessionGoalObservation {
        version: SESSION_GOAL_CONTROL_VERSION,
        node_did: "did:key:source".into(),
        agent_did: "did:key:agent".into(),
        session_id: "chat".into(),
        session_doc_id: "session-document".into(),
        requester_did: Some("did:key:source".into()),
        observed_at: "2026-10-21T00:00:00Z".into(),
        goal: reference.map(|reference| {
            let goal: crate::goal::GoalDocument = serde_json::from_value(json!({
                "_docID": "goal-document", "node_did": "did:key:source",
                "session_id": "chat", "goal_id": reference.goal_id,
                "objective": "Preserve this assignment", "status": "active"
            }))
            .unwrap();
            ObservedGoal {
                revision: GoalRevision {
                    document: goal.doc_id.clone(),
                    composite: "source-composite".into(),
                },
                assignment_root_request_doc_id: reference.root_request_doc_id,
                snapshot: crate::goal::snapshot(&goal, chrono::Utc::now()),
            }
        }),
        signature: Vec::new(),
    }
}

#[test]
fn generated_goal_input_bindings_wait_for_verified_source_state() {
    family::replay(|case| {
        let mut input: RequestInput = serde_json::from_value(json!({
            "captured_context": [{"kind":"file", "source":"editor",
                "capturedAt":"2026-10-21T00:00:00Z", "uri":"file:///workspace/main.rs",
                "terminalName":null, "workspaceUri":"file:///workspace", "version":4,
                "unsaved":true, "range":null, "content":"private unsaved capture"}]
        }))
        .unwrap();
        input.goal_assignment = reference(case.captured.as_ref());
        let original = input.clone();
        let state = if !case.session_readable {
            SessionGoalObservationState::NoReadableSession
        } else if !case.observation_verified {
            SessionGoalObservationState::Unknown(SessionGoalUnknownCause::ProjectionUnverified)
        } else {
            SessionGoalObservationState::Observed(Box::new(observation(reference(
                case.observed.as_ref(),
            ))))
        };
        match bind_goal_input(input, state) {
            Err(refused) => {
                assert_eq!(refused, GoalInputRefused::ProjectionUnverified, "{case:?}");
                family::InputBindingOutput {
                    kind: "pending".to_owned(),
                    reference: None,
                }
            }
            Ok(mut bound) => {
                let reference = modeled(bound.goal_assignment.as_ref());
                bound.goal_assignment = original.goal_assignment.clone();
                assert_eq!(bound, original, "{case:?}");
                family::InputBindingOutput {
                    kind: "prepared".to_owned(),
                    reference,
                }
            }
        }
    });
}
