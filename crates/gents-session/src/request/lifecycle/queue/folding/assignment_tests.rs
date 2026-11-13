use super::*;
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};
use gents_protocol::request_input::{GoalAssignmentInput, QueueDelivery};
use gents_store::identity::{KeyIdentity, NodeIdentity};
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
    crate::request::admission::sign_agent_request_create(identity, &mut create)
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
    gents_store::identity::request_receipt::verify_request_receipt_signature(&row).unwrap();
    row
}

fn reference_value(reference: Option<(&str, Option<&str>)>) -> Value {
    match reference {
        Some((goal_id, root_request_doc_id)) => json!({
            "goal_id": goal_id,
            "root_request_doc_id": root_request_doc_id,
        }),
        None => Value::Null,
    }
}

async fn head_and_candidate(
    identity: &KeyIdentity,
    head: &Value,
    candidate: &Value,
) -> (AgentRequest, AgentRequestRow) {
    let head = row(identity, "head-physical", head, None).await;
    let head = AgentRequest::try_from(head).unwrap();
    let candidate = row(
        identity,
        "candidate-physical",
        candidate,
        Some(QueueDelivery::Steer),
    )
    .await;
    (head, candidate)
}

#[tokio::test]
async fn goal_assignment_folding_follows_the_lean_model() {
    let cases = gents_lean_vocab::families::goal_queue_assignment_contracts_fold_decision::cases();
    let temp = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(temp.path().join("owner.key"), None).unwrap();
    for case in &cases {
        let (head, candidate) = head_and_candidate(
            &identity,
            &reference_value(case.input.head_reference.as_ref().map(|reference| {
                (
                    reference.goal_id.as_str(),
                    reference.root_request_doc_id.as_deref(),
                )
            })),
            &reference_value(case.input.candidate_reference.as_ref().map(|reference| {
                (
                    reference.goal_id.as_str(),
                    reference.root_request_doc_id.as_deref(),
                )
            })),
        )
        .await;
        case.check(
            gents_lean_vocab::families::goal_queue_assignment_contracts_fold_decision::FoldOutput {
                fold: folds_into(&head, &candidate, chrono::Utc::now()),
            },
        );
    }
}

#[tokio::test]
async fn goal_assignment_steering_follows_the_lean_model() {
    let cases = gents_lean_vocab::families::goal_queue_assignment_contracts_steer_decision::cases();
    let temp = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(temp.path().join("owner.key"), None).unwrap();
    for case in &cases {
        let (head, candidate) = head_and_candidate(
            &identity,
            &reference_value(case.input.head_reference.as_ref().map(|reference| {
                (
                    reference.goal_id.as_str(),
                    reference.root_request_doc_id.as_deref(),
                )
            })),
            &reference_value(case.input.candidate_reference.as_ref().map(|reference| {
                (
                    reference.goal_id.as_str(),
                    reference.root_request_doc_id.as_deref(),
                )
            })),
        )
        .await;
        case.check(
            gents_lean_vocab::families::goal_queue_assignment_contracts_steer_decision::SteerOutput {
                steer: super::super::intake::steering_compatible(
                    &head,
                    &candidate,
                    chrono::Utc::now(),
                ),
            },
        );
    }
}
