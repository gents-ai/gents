use super::plan::PlannedWrite;
use super::*;
use crate::store::identity::{KeyIdentity, NodeIdentity};
use gents_protocol::goal_control::SessionGoalObservation;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::GoalAssignmentInput;
use serde_json::Value;

async fn write(node: &Arc<EmbeddedNode>, mutation: &str) -> Value {
    ConfigAccess::write_local(node, "test.session_message_goal_input", mutation)
        .await
        .unwrap()
}

#[tokio::test]
async fn generated_session_messages_capture_target_goal_before_signing() {
    let contracts: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = contracts["session_message_goal_input_cases"]
        .as_array()
        .unwrap();
    assert_eq!(cases.len(), 11);
    let temp = tempfile::tempdir().unwrap();
    let identities = [
        Arc::new(KeyIdentity::load_or_create(temp.path().join("caller.key"), None).unwrap()),
        Arc::new(KeyIdentity::load_or_create(temp.path().join("target.key"), None).unwrap()),
    ];
    let caller = &identities[0];
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(caller.did())
            .build()
            .await
            .unwrap(),
    );
    crate::store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    for identity in &identities {
        crate::test_support::install_test_agent(&node, identity.did(), "agent").await;
    }
    let mut calling = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "caller",
        caller.did(),
        caller.did(),
        "agent",
        "caller-chat",
        "Send to another chat",
        "interactive",
        "2026-10-21T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(caller.did()),
    );
    calling.input.goal_assignment = Some(GoalAssignmentInput {
        goal_id: "caller-goal".into(),
        root_request_doc_id: Some("caller-root".into()),
    });
    crate::sign_agent_request_create(caller.as_ref(), &mut calling)
        .await
        .unwrap();
    let calling = write(&node, &calling.graphql_mutation().unwrap()).await;
    let calling_doc = crate::graphql::created_doc_id(&calling, "AgentRequest").unwrap();
    for (index, case) in cases.iter().enumerate() {
        let name = case["name"].as_str().unwrap();
        let identity = &identities[case["owner"].as_u64().unwrap() as usize - 1];
        let session_id = format!("target-{index}");
        let target = SessionMessageTarget {
            node_did: identity.did().into(),
            agent_id: "agent".into(),
            session_id: session_id.clone(),
        };
        if case["session_readable"] == true {
            crate::session::ensure_session_with_agent_id_and_requester_did(
                &node,
                &session_id,
                identity.did(),
                "agent",
                Some(caller.did()),
            )
            .await
            .unwrap();
        }
        let mut goal_id = None;
        let mut root = None;
        if case["observed"].is_object() {
            let mut opening = AgentRequestCreate::base(
                RequestPurpose::Normal,
                format!("opening-{index}"),
                identity.did(),
                identity.did(),
                "agent",
                &session_id,
                "Original goal",
                "interactive",
                "2026-10-21T00:00:00Z",
                AgentRequestAdmissionRecord::local_self(identity.did()),
            );
            crate::sign_agent_request_create(identity.as_ref(), &mut opening)
                .await
                .unwrap();
            let created = write(&node, &opening.graphql_mutation().unwrap()).await;
            let opening_doc = crate::graphql::created_doc_id(&created, "AgentRequest").unwrap();
            let goal = crate::goal::set_goal(
                &node,
                identity.did(),
                &session_id,
                Some("Target goal"),
                None,
                None,
            )
            .await
            .unwrap();
            goal_id = Some(goal.goal_id);
            if !case["observed"]["root"].is_null() {
                write(&node, &format!(
                    r#"mutation {{ update_Goal(docID: "{}", input: {{ assignment_root_request_doc_id: "{}" }}) {{ _docID }} }}"#,
                    escape_graphql_string(&goal.doc_id), escape_graphql_string(&opening_doc),
                )).await;
                root = Some(opening_doc);
            }
        }
        if case["owner"] == 2
            && (case["observation_verified"] == true || case["observed"].is_object())
        {
            let source: Arc<dyn NodeIdentity> = identity.clone();
            crate::goal::reconcile_goal_observations(&node, &source, true, &[])
                .await
                .unwrap();
            let access = ConfigAccess::local(node.clone());
            let rows = access.execute(&format!(
                r#"{{ GoalObservation(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }} }}) {{ _docID observation_json }} }}"#,
                escape_graphql_string(identity.did()), escape_graphql_string(&session_id),
            )).await.unwrap();
            let row = &rows["data"]["GoalObservation"][0];
            let mut observation: SessionGoalObservation =
                serde_json::from_str(row["observation_json"].as_str().unwrap()).unwrap();
            if let Some(goal) = &mut observation.goal {
                if case["observed"]["owner"] != case["owner"] {
                    goal.snapshot.node_did = caller.did().into();
                }
                if case["observed"]["session"] != case["session"] {
                    goal.snapshot.session_id = "foreign-chat".into();
                }
            }
            observation.signature = identity
                .sign(&observation.signing_payload().unwrap())
                .await
                .unwrap();
            if case["observation_verified"] == false {
                observation.signature[0] ^= 1;
            }
            write(&node, &format!(
                r#"mutation {{ update_GoalObservation(docID: "{}", input: {{ observation_json: "{}" }}) {{ _docID }} }}"#,
                escape_graphql_string(row["_docID"].as_str().unwrap()),
                escape_graphql_string(&serde_json::to_string(&observation).unwrap()),
            )).await;
        }
        let cause = SessionMessageCause {
            caller_node_did: caller.did().into(),
            caller_agent_id: "caller-agent".to_owned(),
            caller_session_id: "caller-session".to_owned(),
            caller_requester_did: None,
            caller_request_id: "caller".into(),
            caller_request_doc_id: calling_doc.clone(),
            caller_hop: 0,
            tool_call_id: format!("call-{index}"),
            tool_call_doc_id: format!("call-doc-{index}"),
            correlation: None,
        };
        let planned = plan(
            &node,
            &cause,
            &target,
            RenderedBody {
                content: "Follow up".into(),
                goal: None,
            },
            None,
            false,
        )
        .await;
        if case["expected"]["kind"] == "pending" {
            assert!(
                planned.as_ref().map_or(true, |planned| planned.is_err()),
                "{name}: unknown source state must not be signed as unassigned"
            );
        } else {
            let planned = planned.unwrap().unwrap();
            let PlannedWrite::Request { create, .. } = &planned.write else {
                panic!("{name}")
            };
            let expected = case["expected"]["reference"]
                .is_object()
                .then(|| GoalAssignmentInput {
                    goal_id: goal_id.unwrap(),
                    root_request_doc_id: root,
                });
            assert_eq!(create.input.goal_assignment, expected, "{name}");
            if name == "local_target_uses_own_goal" {
                write(&node, &format!(
                    r#"mutation {{ update_Goal(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }} }}, input: {{ assignment_root_request_doc_id: "later-opening" }}) {{ _docID }} }}"#,
                    escape_graphql_string(&target.node_did), escape_graphql_string(&target.session_id),
                )).await;
            }
            let staged = ConfigAccess::transact_local_write(
                &node,
                None,
                "test.session_message_stage_goal_input",
                |txn| Box::pin(async { stage_planned_write_in_txn(txn, &cause, &planned).await }),
            )
            .await
            .unwrap();
            let stored = ConfigAccess::local(node.clone())
                .execute(&format!(
                    r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                    escape_graphql_string(&staged.doc_id),
                    crate::request_admission::SIGNED_REQUEST_FIELDS,
                ))
                .await
                .unwrap();
            let stored: gents_protocol::row::AgentRequestRow =
                serde_json::from_value(stored["data"]["AgentRequest"][0].clone()).unwrap();
            crate::request_admission::verify_request_receipt_signature(&stored).unwrap();
            assert_eq!(
                stored
                    .input
                    .as_ref()
                    .and_then(|input| input.goal_assignment.clone()),
                expected,
                "{name}: persisted capture"
            );
            if let Some(expected) = &expected {
                let crate::goal::RequestAssignmentObservation::Assigned(assigned) =
                    crate::goal::load_request_assignment(&node, &stored)
                        .await
                        .unwrap()
                else {
                    panic!("{name}: persisted signed message lost its retained assignment")
                };
                assert_eq!(assigned.goal_id, expected.goal_id, "{name}");
                assert_eq!(
                    assigned.assignment_root_request_doc_id, expected.root_request_doc_id,
                    "{name}"
                );
            }
            assert!(
                caller
                    .verify(
                        caller.did(),
                        &create.signing_payload(),
                        &create.admission.signature
                    )
                    .await
                    .unwrap(),
                "{name}: input must be signed"
            );
            assert_ne!(
                create
                    .input
                    .goal_assignment
                    .as_ref()
                    .map(|r| r.goal_id.as_str()),
                Some("caller-goal"),
                "{name}"
            );
        }
    }
    node.shutdown().await;
}
