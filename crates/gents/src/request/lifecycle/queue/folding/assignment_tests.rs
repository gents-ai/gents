use super::*;
use crate::store::identity::{KeyIdentity, NodeIdentity};
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};
use gents_protocol::request_input::{GoalAssignmentInput, QueueDelivery};
use serde_json::{json, Value};

async fn row(
    identity: &KeyIdentity,
    doc: &str,
    reference: &Value,
    delivery: Option<QueueDelivery>,
) -> AgentRequestRow {
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        doc,
        identity.did(),
        identity.did(),
        "agent",
        "session",
        "authored message",
        "interactive",
        "2026-10-21T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(identity.did()),
    );
    create.input.goal_assignment =
        serde_json::from_value::<Option<GoalAssignmentInput>>(reference.clone()).unwrap();
    if let Some(delivery) = delivery {
        create.input.queue = Some(RequestQueue {
            source: QueueSource::User,
            policy: QueuePolicy::Append,
            delivery,
            position: None,
            key: None,
            queued_after_request_id: Some("head-physical".into()),
            interrupted_request_id: None,
            background_completion_wake_version: None,
        });
    }
    crate::sign_agent_request_create(identity, &mut create)
        .await
        .unwrap();
    let mut value = serde_json::to_value(&create).unwrap();
    value.as_object_mut().unwrap().remove("admission");
    value["_docID"] = json!(doc);
    value["admission_kind"] = json!(create.admission.kind.as_str());
    value["admission_signer_did"] = json!(create.admission.signer_did);
    value["admission_signature"] = json!(bs58::encode(create.admission.signature).into_string());
    value["lifecycle_state"] = json!("pending");
    let row: AgentRequestRow = serde_json::from_value(value).unwrap();
    crate::request::admission::verify_request_receipt_signature(&row).unwrap();
    row
}

#[tokio::test]
async fn generated_goal_assignments_bound_fold_and_steering() {
    #[derive(serde::Deserialize)]
    struct Contracts {
        goal_queue_assignment_cases: Vec<Value>,
    }
    let contracts: Contracts = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(contracts.goal_queue_assignment_cases.len(), 10);
    let temp = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(temp.path().join("owner.key"), None).unwrap();
    for case in contracts.goal_queue_assignment_cases {
        let head = row(&identity, "head-physical", &case["head_reference"], None).await;
        let head = AgentRequest::try_from(head).unwrap();
        let candidate = row(
            &identity,
            "candidate-physical",
            &case["candidate_reference"],
            Some(QueueDelivery::Steer),
        )
        .await;
        let now = chrono::Utc::now();
        assert_eq!(
            folds_into(&head, &candidate, now),
            case["expected_fold"].as_bool().unwrap(),
            "{}: claim folding",
            case["name"]
        );
        assert_eq!(
            super::super::intake::steering_compatible(&head, &candidate, now),
            case["expected_steer"].as_bool().unwrap(),
            "{}: live steering",
            case["name"]
        );
    }
}

#[tokio::test]
async fn ordinary_queued_input_does_not_fold_but_explicit_steer_can() {
    let temp = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(temp.path().join("owner.key"), None).unwrap();
    let head =
        AgentRequest::try_from(row(&identity, "head-physical", &Value::Null, None).await).unwrap();
    let queued = row(
        &identity,
        "queued-physical",
        &Value::Null,
        Some(QueueDelivery::Queue),
    )
    .await;
    let steer = row(
        &identity,
        "steer-physical",
        &Value::Null,
        Some(QueueDelivery::Steer),
    )
    .await;
    let now = chrono::Utc::now();
    assert!(!folds_into(&head, &queued, now));
    assert!(folds_into(&head, &steer, now));
}
