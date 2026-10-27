use gents::{KeyIdentity, NodeIdentity};
use gents_protocol::{
    goal_control::{GoalRevision, ObservedGoal, SessionGoalObservation},
    request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose},
    request_input::GoalAssignmentInput,
    request_lifecycle::RequestLifecycleState,
    row::{AgentRequestRow, MailboxItemRow},
    session::SessionRequestObservation,
};
use serde_json::json;

#[tokio::test]
async fn question_wait_requires_the_authenticated_current_goal_assignment() {
    let home = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(home.path().join("node.key"), None).unwrap();
    let did = identity.did();
    let mut request = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "request",
        did,
        did,
        "agent",
        "chat",
        "Start the goal",
        "interactive",
        "2026-10-21T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(did),
    );
    request.input.goal_assignment = Some(GoalAssignmentInput {
        goal_id: "goal".into(),
        root_request_doc_id: None,
    });
    request.admission.signature = identity.sign(&request.signing_payload()).await.unwrap();
    let mut row = serde_json::to_value(&request).unwrap();
    row.as_object_mut().unwrap().remove("admission");
    row["_docID"] = json!("physical-root");
    row["admission_kind"] = json!(request.admission.kind.as_str());
    row["admission_signer_did"] = json!(request.admission.signer_did);
    row["admission_signature"] = json!(bs58::encode(&request.admission.signature).into_string());
    row["lifecycle_state"] = json!("completed");
    let root: AgentRequestRow = serde_json::from_value(row).unwrap();
    let goal: gents::goal::GoalDocument = serde_json::from_value(json!({
        "_docID":"goal-document", "goal_id":"goal", "node_did":did, "session_id":"chat",
        "assignment_root_request_doc_id":"physical-root", "status":"active"
    }))
    .unwrap();
    let now = chrono::Utc::now();
    let observed = SessionGoalObservation {
        version: 1,
        node_did: did.into(),
        session_id: "chat".into(),
        session_doc_id: "session-document".into(),
        requester_did: Some(did.into()),
        observed_at: now.to_rfc3339(),
        signature: Vec::new(),
        goal: Some(ObservedGoal {
            revision: GoalRevision {
                document: goal.doc_id.clone(),
                composite: "goal-commit".into(),
            },
            assignment_root_request_doc_id: goal.assignment_root_request_doc_id.clone(),
            snapshot: gents::goal::snapshot(&goal, now),
        }),
    };
    let latest = SessionRequestObservation {
        request_doc_id: "physical-root".into(),
        request_id: "request".into(),
        lifecycle_state: RequestLifecycleState::Completed,
    };
    let item: MailboxItemRow = serde_json::from_value(json!({
        "_docID":"question", "item_key":"question", "requester_did":did, "node_did":did,
        "target_node_did":did, "target_agent_id":"agent", "session_id":"chat", "request_id":"request",
        "status":"open", "kind":"ask", "action":"start_request", "title":"Next step",
        "payload":json!({"version":2,"prompt":"Next step?","options":[],"allow_free_text":true}).to_string(),
        "source_kind":"agent", "source_id":"question-source", "created_at":now.to_rfc3339(),
    })).unwrap();
    for fault in [
        "none",
        "created-in-turn",
        "wrong-physical-reference",
        "foreign-row",
        "old-assignment",
        "other-goal",
        "other-requester",
        "changed-receipt",
        "missing-parent",
        "duplicate-parent",
        "interrupted",
        "wrong-current-request",
    ] {
        let mut observation = observed.clone();
        let mut latest = latest.clone();
        let mut requests = vec![root.clone()];
        match fault {
            "created-in-turn" | "wrong-physical-reference" => {
                let mut signed = request.clone();
                signed.input.goal_assignment = if fault == "created-in-turn" {
                    None
                } else {
                    Some(GoalAssignmentInput {
                        goal_id: "goal".into(),
                        root_request_doc_id: Some("other-root".into()),
                    })
                };
                signed.admission.signature =
                    identity.sign(&signed.signing_payload()).await.unwrap();
                let mut row = serde_json::to_value(&requests[0]).unwrap();
                row["_docID"] = json!(requests[0].doc_id);
                row["input"] = serde_json::to_value(&signed.input).unwrap();
                row["admission_signature"] =
                    json!(bs58::encode(signed.admission.signature).into_string());
                requests[0] = serde_json::from_value(row).unwrap();
            }
            "foreign-row" => {
                let mut foreign = root.clone();
                foreign.node_did = Some("other-node".into());
                foreign.session_id = Some("other-chat".into());
                requests.insert(0, foreign);
            }
            "old-assignment" => {
                observation
                    .goal
                    .as_mut()
                    .unwrap()
                    .assignment_root_request_doc_id = Some("other-root".into())
            }
            "other-goal" => {
                observation.goal.as_mut().unwrap().snapshot.goal_id = "other-goal".into()
            }
            "other-requester" => observation.requester_did = Some("other-person".into()),
            "changed-receipt" => requests[0].content = Some("changed after signing".into()),
            "missing-parent" => {
                requests.clear();
            }
            "duplicate-parent" => requests.push(root.clone()),
            "interrupted" => {
                requests[0].lifecycle_state = Some(RequestLifecycleState::Interrupted);
                latest.lifecycle_state = RequestLifecycleState::Interrupted;
            }
            "wrong-current-request" => latest.request_id = "different-logical-request".into(),
            _ => {}
        }
        let actual = gents::goal::awaiting_goal_question_ids(
            &observation,
            &latest,
            &requests,
            std::slice::from_ref(&item),
            now,
        );
        assert_eq!(
            actual,
            if matches!(fault, "none" | "foreign-row" | "created-in-turn") {
                vec!["question".to_string()]
            } else {
                Vec::new()
            },
            "{fault}"
        );
    }
}
