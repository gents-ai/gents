use super::*;
use crate::identity::{KeyIdentity, NodeIdentity};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::GoalAssignmentInput;
use serde_json::{json, Value};

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
        "agent",
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

#[tokio::test]
async fn generated_goal_request_assignments_drive_signed_binding() {
    #[derive(serde::Deserialize)]
    struct Contracts {
        goal_request_assignment_cases: Vec<Value>,
    }
    let contracts: Contracts = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(contracts.goal_request_assignment_cases.len(), 18);
    let temp = tempfile::tempdir().unwrap();
    let identities = [
        KeyIdentity::load_or_create(temp.path().join("one.key"), None).unwrap(),
        KeyIdentity::load_or_create(temp.path().join("two.key"), None).unwrap(),
    ];
    for case in contracts.goal_request_assignment_cases {
        let entry = entry(&case["entry"], &identities).await;
        assert_eq!(
            request_belongs_to_goal(&goal(&case["assignment"], &identities), &entry),
            case["expected_member"].as_bool().unwrap(),
            "{}: membership",
            case["name"]
        );
        let assignment = goal(&case["assignment"], &identities);
        if assignment.assignment_root_request_doc_id.is_some() {
            assert_eq!(
                super::super::request_head::latest_goal_request(
                    &assignment,
                    std::slice::from_ref(&entry)
                )
                .map(|row| row.doc_id.as_ref()),
                case["expected_member"]
                    .as_bool()
                    .unwrap()
                    .then_some(entry.doc_id.as_ref()),
                "{}: signed assignment head",
                case["name"],
            );
        }
        let goals = case["observed"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| goal(value, &identities))
            .collect::<Vec<_>>();
        let observation = observe_request_assignment(&goals, &entry);
        let scope = request_assignment_scope(&entry, &observation).ok().map(|scope| match scope {
            RequestAssignmentScope::Unassigned => json!({"kind":"unassigned"}),
            RequestAssignmentScope::Unresolved { entry_doc_id } => json!({"kind":"unresolved",
                "entry":entry_doc_id.strip_prefix("physical-").unwrap().parse::<u64>().unwrap()}),
            RequestAssignmentScope::Referenced(reference) => json!({"kind":"referenced",
                "goal":reference.goal_id.strip_prefix("goal-").unwrap().parse::<u64>().unwrap(),
                "root":reference.root_request_doc_id.unwrap().strip_prefix("physical-").unwrap().parse::<u64>().unwrap()}),
        });
        assert_eq!(
            scope.unwrap_or(Value::Null),
            case["expected_queue_scope"],
            "{}: queue scope",
            case["name"]
        );
        let actual = match observation {
            RequestAssignmentObservation::None => json!({"kind":"unassigned"}),
            RequestAssignmentObservation::Unknown => json!({"kind":"unknown"}),
            RequestAssignmentObservation::Assigned(goal) => json!({"kind":"assigned",
                "goal":goal.goal_id.strip_prefix("goal-").unwrap().parse::<u64>().unwrap(),
                "root":goal.assignment_root_request_doc_id.as_deref().unwrap().strip_prefix("physical-").unwrap().parse::<u64>().unwrap()}),
        };
        assert_eq!(
            actual, case["expected_observation"],
            "{}: assignment",
            case["name"]
        );
    }
}
