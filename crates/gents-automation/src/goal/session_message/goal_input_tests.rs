use super::SessionGoals;
use crate::goal::operator_resume_test_support::Fixture;
use gents_lean_vocab::families::goal_request_assignment_contracts_message_input as message_input;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::GoalAssignmentInput;
use gents_protocol::request_lifecycle::{SessionMessageCause, SessionMessageTarget};
use gents_session::session::message::plan::{plan, target_goal_assignment};
use gents_session::session::message::MessageContent;
use gents_session::session::message::{stage_planned_write_in_txn, RenderedBody};
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use serde_json::json;

fn label(id: &str, known: &[(&str, Option<u64>)]) -> u64 {
    known
        .iter()
        .find(|(known, _)| *known == id)
        .and_then(|(_, label)| *label)
        .unwrap_or(u64::MAX)
}

fn prepared(
    selected: &anyhow::Result<Option<GoalAssignmentInput>>,
    goals: &[(&str, Option<u64>)],
    roots: &[(&str, Option<u64>)],
) -> message_input::PreparedWire {
    match selected {
        Err(_) => message_input::PreparedWire {
            kind: "pending".into(),
            reference: None,
        },
        Ok(selected) => message_input::PreparedWire {
            kind: "prepared".into(),
            reference: selected.as_ref().map(|goal| message_input::Reference {
                goal: label(&goal.goal_id, goals),
                root: goal
                    .root_request_doc_id
                    .as_deref()
                    .map(|root| label(root, roots)),
            }),
        },
    }
}

#[tokio::test]
async fn generated_session_messages_capture_target_goal_before_signing() {
    let cases =
        gents_lean_vocab::families::goal_request_assignment_contracts_message_input::cases();
    let before = json!({"status":"active","sequence":0,"last_continued_from":null,
        "latest_request":10,"children":[],"blocked_audits":0,"wrapup_requested":false,
        "wrapup_completed":false,"tokens_used":37,"token_budget":1000});
    for (index, case) in cases.iter().enumerate() {
        let input = &case.input;
        let name = input.name.as_str();
        let f = Fixture::new(&before).await;
        let caller_agent = &f.agent;
        let session_id = format!("goal-input-{index}");
        let target = SessionMessageTarget {
            node_did: f.identity.did().into(),
            agent_did: f.agent.did().into(),
            session_id: session_id.clone(),
        };
        let agent_access = f.agent_access();
        if input.session_readable {
            agent_access
                .transact_write("test.goal_input.session", |txn| {
                    Box::pin(async {
                        gents_session::session::ensure_session_in_txn(
                            txn,
                            &session_id,
                            f.identity.did(),
                            f.agent.did(),
                            Some(caller_agent.did()),
                            None,
                            None,
                            "2026-01-01T00:00:00Z",
                        )
                        .await?;
                        Ok::<_, anyhow::Error>(())
                    })
                })
                .await
                .unwrap();
            gents_session::session::recover_agent_session_relationships_with_access(
                &agent_access,
                f.identity.did(),
                &identity::Did::new(f.agent.did()).unwrap(),
            )
            .await
            .unwrap();
        }
        let mut goal_id = None;
        let mut root_doc = None;
        if input.observed.is_some() && !input.session_readable {
            agent_access.write("test.goal_input.orphan", &format!(r#"mutation {{ create_Goal(input: {{ goal_id: "orphan", node_did: "{}", agent_did: "{}", session_id: "{}", objective: "Orphan", status: "active" }}) {{ _docID }} }}"#,
                escape_graphql_string(f.identity.did()), escape_graphql_string(f.agent.did()), escape_graphql_string(&session_id))).await.unwrap();
        }
        if input.observed.is_some() && input.session_readable {
            let mut opening = AgentRequestCreate::base(
                RequestPurpose::Normal,
                format!("opening-{index}"),
                f.identity.did(),
                f.agent.did(),
                f.agent.did(),
                &session_id,
                "Original goal",
                "interactive",
                "2026-10-21T00:00:00Z",
                AgentRequestAdmissionRecord::local_self(f.agent.did()),
            );
            gents_session::request::admission::sign_agent_request_create(
                f.agent.as_ref(),
                &mut opening,
            )
            .await
            .unwrap();
            let created = agent_access
                .write(
                    "test.goal_input.opening",
                    &opening.graphql_mutation().unwrap(),
                )
                .await
                .unwrap();
            let doc = gents_store::graphql::created_doc_id(&created, "AgentRequest").unwrap();
            let goal = crate::goal::seed_goal_from_access(
                &agent_access,
                f.identity.did(),
                &session_id,
                Some("Target goal"),
                None,
                None,
                None,
            )
            .await
            .unwrap();
            goal_id = Some(goal.goal_id);
            if input.observed.as_ref().unwrap().root.is_some() {
                agent_access.write("test.goal_input.root", &format!(r#"mutation {{ update_Goal(docID: "{}", input: {{ assignment_root_request_doc_id: "{}" }}) {{ _docID }} }}"#,
                    escape_graphql_string(&goal.doc_id), escape_graphql_string(&doc))).await.unwrap();
                root_doc = Some(doc);
            }
        }
        // A local message is caused by an accepted tool call on the caller's
        // own request, which staging verifies before it publishes.
        let tool_call_doc_id = {
            let created = agent_access
                .write(
                    "test.goal_input.tool_call",
                    &format!(
                        r#"mutation {{ create_AgentToolCall(input: {{ tool_call_key: "goal-input-call-{index}", request_id: "{}", request_doc_id: "{}", session_id: "{}", agent_did: "{}", node_did: "{}", requester_did: "{}", tool_name: "agent_message", tool_call_id: "call-{index}", status: "pending", lifecycle_state: "pending" }}) {{ _docID }} }}"#,
                        escape_graphql_string(&f.parent.request_id),
                        escape_graphql_string(&f.parent.doc_id),
                        escape_graphql_string(&f.parent.session_id),
                        escape_graphql_string(f.agent.did()),
                        escape_graphql_string(f.identity.did()),
                        escape_graphql_string(f.parent.requester_did.as_deref().unwrap()),
                    ),
                )
                .await
                .unwrap();
            gents_store::graphql::created_doc_id(&created, "AgentToolCall").unwrap()
        };
        let cause = SessionMessageCause {
            caller_node_did: f.identity.did().into(),
            caller_agent_did: caller_agent.did().into(),
            caller_session_id: f.parent.session_id.clone(),
            caller_requester_did: f.parent.requester_did.clone(),
            caller_request_id: f.parent.request_id.clone(),
            caller_request_doc_id: f.parent.doc_id.clone(),
            caller_hop: 0,
            tool_call_id: format!("call-{index}"),
            tool_call_doc_id,
            correlation: None,
        };
        let selected = target_goal_assignment(&f.node, &SessionGoals, &cause, &target).await;
        let observed = input.observed.as_ref();
        let caller = input.caller_reference.as_ref();
        let goals = [
            (
                goal_id.as_deref().unwrap_or_default(),
                observed.map(|a| a.goal),
            ),
            (f.goal.goal_id.as_str(), caller.map(|r| r.goal)),
        ];
        let caller_root = f
            .goal
            .assignment_root_request_doc_id
            .clone()
            .unwrap_or_default();
        let roots = [
            (
                root_doc.as_deref().unwrap_or_default(),
                observed.and_then(|a| a.root),
            ),
            (caller_root.as_str(), caller.and_then(|r| r.root)),
        ];
        let output = prepared(&selected, &goals, &roots);
        if selected.is_err() {
            let signer = caller_agent.managed_agent_signer().unwrap();
            let planned = plan(
                &f.node,
                &SessionGoals,
                &cause,
                &target,
                MessageContent {
                    rendered: RenderedBody {
                        content: "Follow up".into(),
                        goal: None,
                    },
                    title: None,
                },
                false,
                &signer,
            )
            .await;
            assert!(
                planned.as_ref().map_or(true, |result| result.is_err()),
                "{name}: unknown destination input must not be signed as unassigned"
            );
        } else {
            let expected = selected.as_ref().unwrap().clone();
            let signer = caller_agent.managed_agent_signer().unwrap();
            let source_access = ConfigAccess::local_managed_agent(
                f.node.clone(),
                identity::Did::new(caller_agent.did()).unwrap(),
                caller_agent.as_ref(),
            )
            .unwrap();
            let planned = plan(
                &f.node,
                &SessionGoals,
                &cause,
                &target,
                MessageContent {
                    rendered: RenderedBody {
                        content: "Follow up".into(),
                        goal: None,
                    },
                    title: None,
                },
                false,
                &signer,
            )
            .await
            .unwrap()
            .unwrap_or_else(|error| panic!("{name}: local delivery must plan: {error}"));
            let Some(create) = planned.planned_request() else {
                panic!("{name}: expected a session message request")
            };
            assert_eq!(
                create.input.goal_assignment, expected,
                "{name}: signed capture"
            );
            assert!(
                caller_agent
                    .verify(
                        caller_agent.did(),
                        &create.signing_payload(),
                        &create.admission.signature
                    )
                    .await
                    .unwrap(),
                "{name}: input must be signed by the selected Agent"
            );
            assert_ne!(
                create
                    .input
                    .goal_assignment
                    .as_ref()
                    .map(|goal| goal.goal_id.as_str()),
                Some("caller-goal"),
                "{name}"
            );
            if name == "local_target_uses_own_goal" {
                agent_access.write("test.goal_input.later_root", &format!(
                    r#"mutation {{ update_Goal(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }} }}, input: {{ assignment_root_request_doc_id: "later-opening" }}) {{ _docID }} }}"#,
                    escape_graphql_string(&target.node_did), escape_graphql_string(&target.session_id),
                )).await.unwrap();
            }
            let staged = source_access
                .transact_write("test.session_message_stage_goal_input", |txn| {
                    Box::pin(async {
                        stage_planned_write_in_txn(txn, &SessionGoals, &cause, &planned).await
                    })
                })
                .await;
            if !input.session_readable {
                // Session messages publish only into an existing session the
                // caller can see; a fresh session is opened by its own flow.
                let error = staged.err().unwrap_or_else(|| {
                    panic!("{name}: an unseen session must not receive a message")
                });
                assert!(
                    format!("{error:#}").contains("client request session is unavailable"),
                    "{name}: {error:#}"
                );
                f.node.shutdown().await.expect("shut down test node");
                case.check(output);
                continue;
            }
            let staged = staged.unwrap_or_else(|error| panic!("{name}: {error:#}"));
            let stored = source_access
                .execute(&format!(
                    r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                    escape_graphql_string(&staged.doc_id),
                    gents_protocol::request_admission::SIGNED_REQUEST_FIELDS,
                ))
                .await
                .unwrap();
            let stored: gents_protocol::row::AgentRequestRow =
                serde_json::from_value(stored["data"]["AgentRequest"][0].clone()).unwrap();
            gents_store::identity::request_receipt::verify_request_receipt_signature(&stored)
                .unwrap();
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
                    crate::goal::load_request_assignment(&source_access, &stored)
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
        }
        f.node.shutdown().await.expect("shut down test node");
        case.check(output);
    }
}
