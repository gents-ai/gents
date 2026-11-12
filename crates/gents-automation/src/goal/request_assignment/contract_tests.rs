use super::*;
use gents_lean_vocab::families::goal_request_assignment_contracts_signed_binding as signed_binding;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::GoalAssignmentInput;
use gents_store::identity::{KeyIdentity, NodeIdentity};
use serde_json::{json, Value};

#[tokio::test]
async fn physical_goal_caller_reuses_the_signed_session_observation() {
    let temp = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(temp.path().join("node.key"), None).unwrap();
    let row = entry(
        &json!({"owner":1,"session":1,"doc":1,"reference":null,"signature_valid":true}),
        std::slice::from_ref(&identity),
    )
    .await;
    let assignments =
        request_assignments(identity.did(), "session-1", vec![row.clone()], vec![]).unwrap();
    let (observed, assignment) = assignments
        .observe_physical_request("physical-1", "request-1")
        .unwrap();
    assert_eq!(observed.doc_id, row.doc_id);
    assert!(matches!(assignment, RequestAssignmentObservation::None));
    assert!(assignments
        .observe_physical_request("physical-1", "other")
        .is_err());
    assert!(assignments
        .observe_physical_request("missing", "request-1")
        .is_err());
    assert!(
        request_assignments(identity.did(), "session-1", vec![row.clone(), row], vec![]).is_err()
    );
}

pub(super) fn goal(value: &Value, identities: &[KeyIdentity]) -> GoalDocument {
    serde_json::from_value(json!({
        "_docID":format!("goal-doc-{}-{}",value["goal"],value["root"]),
        "goal_id":format!("goal-{}",value["goal"]),
        "node_did":identities[value["owner"].as_u64().unwrap() as usize - 1].did(),
        "session_id":format!("session-{}",value["session"]),
        "assignment_root_request_doc_id":value["root"].as_u64().map(|root|format!("physical-{root}")),
        "status":"active"
    })).unwrap()
}

pub(super) async fn entry(value: &Value, identities: &[KeyIdentity]) -> AgentRequestRow {
    let identity = &identities[value["owner"].as_u64().unwrap() as usize - 1];
    let mut request = AgentRequestCreate::base(
        RequestPurpose::Normal,
        format!("request-{}", value["doc"]),
        identity.did(),
        identity.did(),
        identity.did(),
        format!("session-{}", value["session"]),
        "follow up",
        "interactive",
        "2026-10-21T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(identity.did()),
    );
    request.input.goal_assignment = value["reference"].as_object().map(|_| GoalAssignmentInput {
        goal_id: format!("goal-{}", value["reference"]["goal"]),
        root_request_doc_id: value["reference"]["root"]
            .as_u64()
            .map(|root| format!("physical-{root}")),
    });
    request.admission.signature = identity.sign(&request.signing_payload()).await.unwrap();
    let mut row = serde_json::to_value(&request).unwrap();
    row.as_object_mut().unwrap().remove("admission");
    row["_docID"] = json!(format!("physical-{}", value["doc"]));
    row["admission_kind"] = json!(request.admission.kind.as_str());
    row["admission_signer_did"] = json!(request.admission.signer_did);
    row["admission_signature"] = json!(bs58::encode(request.admission.signature).into_string());
    if value["signature_valid"] == false {
        row["content"] = json!("changed after signing");
    }
    serde_json::from_value(row).unwrap()
}

fn label(value: &str, prefix: &str) -> u64 {
    value.strip_prefix(prefix).unwrap().parse::<u64>().unwrap()
}

pub(super) fn queue_scope_wire(scope: RequestAssignmentScope) -> signed_binding::QueueScopeWire {
    match scope {
        RequestAssignmentScope::Unassigned => signed_binding::QueueScopeWire {
            kind: "unassigned".into(),
            entry: None,
            goal: None,
            root: None,
        },
        RequestAssignmentScope::Unresolved { entry_doc_id } => signed_binding::QueueScopeWire {
            kind: "unresolved".into(),
            entry: Some(label(&entry_doc_id, "physical-")),
            goal: None,
            root: None,
        },
        RequestAssignmentScope::Referenced(reference) => signed_binding::QueueScopeWire {
            kind: "referenced".into(),
            entry: None,
            goal: Some(label(&reference.goal_id, "goal-")),
            root: Some(label(
                reference.root_request_doc_id.as_deref().unwrap(),
                "physical-",
            )),
        },
    }
}

#[tokio::test]
async fn generated_goal_request_assignments_drive_signed_binding() {
    let temp = tempfile::tempdir().unwrap();
    let identities = [
        KeyIdentity::load_or_create(temp.path().join("one.key"), None).unwrap(),
        KeyIdentity::load_or_create(temp.path().join("two.key"), None).unwrap(),
    ];
    for case in
        gents_lean_vocab::families::goal_request_assignment_contracts_signed_binding::cases()
    {
        let input = &case.input;
        let entry = entry(&serde_json::to_value(&input.entry).unwrap(), &identities).await;
        let assignment = goal(
            &serde_json::to_value(&input.assignment).unwrap(),
            &identities,
        );
        let member = request_belongs_to_goal(&assignment, &entry);
        if assignment.assignment_root_request_doc_id.is_some() {
            assert_eq!(
                super::super::request_head::latest_goal_request(
                    &assignment,
                    std::slice::from_ref(&entry)
                )
                .map(|row| row.doc_id.as_ref()),
                member.then_some(entry.doc_id.as_ref()),
                "{}: signed assignment head",
                input.name,
            );
        }
        let goals = input
            .observed
            .iter()
            .map(|value| goal(&serde_json::to_value(value).unwrap(), &identities))
            .collect::<Vec<_>>();
        let observation = observe_request_assignment(&goals, &entry);
        let queue_scope = request_assignment_scope(&entry, &observation)
            .ok()
            .map(queue_scope_wire);
        let observation = match observation {
            RequestAssignmentObservation::None => signed_binding::AssignmentObservationWire {
                kind: "unassigned".into(),
                goal: None,
                root: None,
            },
            RequestAssignmentObservation::Unknown => signed_binding::AssignmentObservationWire {
                kind: "unknown".into(),
                goal: None,
                root: None,
            },
            RequestAssignmentObservation::Assigned(goal) => {
                signed_binding::AssignmentObservationWire {
                    kind: "assigned".into(),
                    goal: Some(label(&goal.goal_id, "goal-")),
                    root: Some(label(
                        goal.assignment_root_request_doc_id.as_deref().unwrap(),
                        "physical-",
                    )),
                }
            }
        };
        case.check(signed_binding::SignedBindingOutput {
            member,
            observation,
            queue_scope,
        });
    }
}
