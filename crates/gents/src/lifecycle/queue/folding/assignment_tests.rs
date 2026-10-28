use super::*;
use crate::identity::{KeyIdentity, NodeIdentity};
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};
use gents_protocol::request_input::{GoalAssignmentInput, QueueDelivery};
use serde_json::{json, Value};

async fn row(
    identity: &KeyIdentity,
    doc: &str,
    reference: &Value,
    queued: bool,
    steer_target: Option<&str>,
    target_node: Option<&str>,
) -> AgentRequestRow {
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        doc,
        target_node.unwrap_or(identity.did()),
        identity.did(),
        "agent",
        "session",
        "authored message",
        "interactive",
        "2026-10-21T00:00:00Z",
        if target_node.is_some() {
            AgentRequestAdmissionRecord::peer(identity.did())
        } else {
            AgentRequestAdmissionRecord::local_self(identity.did())
        },
    );
    create.input.goal_assignment =
        serde_json::from_value::<Option<GoalAssignmentInput>>(reference.clone()).unwrap();
    create.input.steer_target_request_doc_id = steer_target.map(str::to_owned);
    if steer_target.is_some() {
        create.caused_by_parent_request_id = Some("caller".to_owned());
        create.caused_by_parent_request_doc_id = Some("caller-doc".to_owned());
        create.caused_by_parent_tool_call_id = Some("tool".to_owned());
        create.caused_by_parent_tool_call_doc_id = Some("tool-doc".to_owned());
        create.request_hop = 1;
    }
    if queued {
        create.input.queue = Some(RequestQueue {
            source: if steer_target.is_some() {
                QueueSource::Steering
            } else {
                QueueSource::User
            },
            policy: QueuePolicy::Append,
            delivery: QueueDelivery::Steer,
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
    crate::request_admission::verify_request_receipt_signature(&row).unwrap();
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
        let head = row(
            &identity,
            "head-physical",
            &case["head_reference"],
            false,
            None,
            None,
        )
        .await;
        let head = AgentRequest::try_from(head).unwrap();
        let candidate = row(
            &identity,
            "candidate-physical",
            &case["candidate_reference"],
            true,
            None,
            None,
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
async fn generated_agent_steer_targets_match_the_signed_physical_request() {
    let output = tokio::task::spawn_blocking(|| {
        gents_lean_contract::run_prepared_generator(
            &gents_lean_contract::proofs_dir().unwrap(),
            "Proofs.Conformance.AgentSteerTarget:olean",
            "Proofs/Conformance/AgentSteerTarget.lean",
        )
        .unwrap()
    })
    .await
    .unwrap();
    let cases: Vec<Value> = serde_json::from_str(&output).unwrap();
    assert_eq!(cases.len(), 3);
    let temp = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(temp.path().join("owner.key"), None).unwrap();
    let peer = KeyIdentity::load_or_create(temp.path().join("peer.key"), None).unwrap();
    for case in cases {
        let target = case["targetDocument"].as_u64().map(|id| id.to_string());
        for is_peer in [false, true] {
            let signer = if is_peer { &peer } else { &identity };
            let target_node = is_peer.then_some(identity.did());
            let head = AgentRequest::try_from(
                row(signer, "20", &Value::Null, false, None, target_node).await,
            )
            .unwrap();
            let candidate = row(
                signer,
                "candidate",
                &Value::Null,
                true,
                target.as_deref(),
                target_node,
            )
            .await;
            assert_eq!(
                super::super::intake::steering_compatible(&head, &candidate, chrono::Utc::now()),
                case["accepted"].as_bool().unwrap(),
                "{case}"
            );
            if target.is_some() {
                let mut tampered = candidate;
                tampered.input.as_mut().unwrap().steer_target_request_doc_id =
                    Some("other".to_owned());
                assert!(
                    crate::request_admission::verify_request_receipt_signature(&tampered).is_err()
                );
            }
        }
    }
}
