use crate::goal::{
    create_goal_for_session_from_access, load_canonical_goal_in_txn, CreateGoalForSessionOutcome,
    GoalDocument,
};
use gents_protocol::goal_status::GoalStatus;
use gents_protocol::operations::goal::{GoalExpected, GoalOperationError, GoalSetInput};
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct Contracts {
    goal_objective_whitespace_codepoints: Vec<u32>,
}

#[derive(Debug, PartialEq)]
struct Assignment {
    status: GoalStatus,
    sequence: i64,
    objective: String,
    budget: Option<i64>,
}

fn assignment(goal: &GoalDocument) -> Assignment {
    Assignment {
        status: goal.parsed_status().unwrap(),
        sequence: goal.continuation_sequence(),
        objective: goal.objective.clone(),
        budget: goal.token_budget,
    }
}

fn case_assignment(status: &str, sequence: u64, objective: &str, budget: u64) -> Assignment {
    Assignment {
        status: serde_json::from_value(Value::String(status.to_owned())).unwrap(),
        sequence: sequence as i64,
        objective: objective.to_owned(),
        budget: Some(budget as i64),
    }
}

async fn durable_rows(access: &gents_store::config_client::ConfigAccess) -> Value {
    access.execute("{ Goal { _docID _version { cid } goal_id objective status token_budget continuation_sequence updated_at } GoalCreationClaim { _docID creation_key goal_id objective token_budget } }").await.unwrap()["data"].clone()
}

#[tokio::test]
async fn generated_goal_edit_cases_drive_real_transactions() {
    let cases = &gents_lean_vocab::lean_contract_snapshot().goal_edit_cases;
    assert_eq!(cases.len(), 66);
    for case in cases {
        let observed_case = case_assignment(
            &case.observed.status,
            case.observed.sequence,
            &case.observed.objective,
            case.observed.budget,
        );
        let current_case = case_assignment(
            &case.current.status,
            case.current.sequence,
            &case.current.objective,
            case.current.budget,
        );
        let expected = case.expected.as_ref().map(|expected| {
            case_assignment(
                &expected.status,
                expected.sequence,
                &expected.objective,
                expected.budget,
            )
        });
        let input = GoalSetInput {
            objective: case.fields.objective.clone(),
            token_budget: case.fields.budget.map(Some),
            ..Default::default()
        };
        let (db, user, access, binding) = gents_inference::test_support::signed_test_node(
            "goal-edit-contract",
            crate::lifecycle_hooks::composition(),
        )
        .await;
        let node = db.node.clone();
        let signer = gents_store::test_support::held_runtime_identity(db.node_identity.did());
        let agent = gents_store::identity::resolve_agent_identity(&signer, &binding).unwrap();
        let agent_access = gents_store::config_client::ConfigAccess::local_managed_agent(
            node.clone(),
            identity::Did::new(agent.did()).unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        access.transact_write("test.goal_edit_session", |txn| {
            Box::pin(async {
                let created = txn.execute_client_signed_create(
                    "mutation($input: AgentSessionMutationInputArg!) {create_AgentSession(input: $input) {_docID}}",
                    &serde_json::json!({"input": {"node_did": signer.did(), "session_id": "session",
                        "requester_did": user.did(), "agent_did": agent.did(),
                        "created_at": "2026-11-04T00:00:00Z"}}),
                    gents_protocol::schemas::AGENT_SESSION,
                ).await?;
                let doc = gents_store::graphql::created_doc_id(&created, "AgentSession")?;
                txn.add_actor_relationship("AgentSession", &doc, "working_agent", &identity::Did::new(agent.did())?).await?;
                Ok::<_, anyhow::Error>(())
            })
        }).await.unwrap();
        let created = create_goal_for_session_from_access(
            &access,
            signer.did(),
            "session",
            "ship",
            Some(100),
        )
        .await
        .unwrap();
        let mut observed = match created {
            CreateGoalForSessionOutcome::Created(goal)
            | CreateGoalForSessionOutcome::Idempotent(goal) => goal,
        };
        let doc = escape_graphql_string(&observed.doc_id);
        access.transact_write("test.goal_edit_state", |txn| {
            let doc = &doc;
            Box::pin(async move {
                txn.execute_client_signed_update(
                    &format!(r#"mutation($input: GoalMutationInputArg!) {{update_Goal(docID: "{doc}", input: $input) {{_docID}}}}"#),
                    &serde_json::json!({"input": {"status": case.current.status.as_str(),
                        "continuation_sequence": case.current.sequence, "objective": case.current.objective.as_str(),
                        "token_budget": case.current.budget}}),
                    gents_protocol::schemas::GOAL,
                ).await
            })
        }).await.unwrap();
        observed.status = case.observed.status.clone();
        observed.continuation_sequence = Some(case.observed.sequence as i64);
        observed.objective = case.observed.objective.clone();
        observed.token_budget = Some(case.observed.budget as i64);
        assert_eq!(
            assignment(&observed),
            observed_case,
            "{} observation",
            case.name
        );
        let current = access
            .transact_readonly("test.goal_edit.current", |txn| {
                Box::pin(load_canonical_goal_in_txn(
                    txn,
                    &observed.node_did,
                    &observed.session_id,
                ))
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            assignment(&current),
            current_case,
            "{} durable input",
            case.name
        );
        let before = durable_rows(&access).await;
        let edit = match case.origin.as_str() {
            "agent" => {
                super::set_as_working_agent(&agent_access, &observed, &input, None, "request").await
            }
            _ => {
                let input = GoalSetInput {
                    expected: Some(GoalExpected::Assignment {
                        goal_doc_id: observed.doc_id.clone(),
                        goal_id: observed.goal_id.clone(),
                        assignment_root_request_doc_id: observed
                            .assignment_root_request_doc_id
                            .clone(),
                        status: observed.parsed_status().unwrap(),
                        continuation_sequence: observed.continuation_sequence(),
                    }),
                    ..input.clone()
                };
                super::set(&access, signer.did(), "session", &input).await
            }
        };
        let result = match edit {
            Ok(result) => Some(result),
            Err(GoalOperationError::Passthrough(error)) => panic!("{}: {error:#}", case.name),
            Err(_) => None,
        };
        assert_eq!(result.as_ref().map(assignment), expected, "{}", case.name);
        if let (Some(updated), Some(expected)) = (&result, &expected) {
            let prompt =
                crate::triggers::engine::goal_source::continuation_prompt(updated, None, false);
            let objective = serde_json::to_string(&expected.objective).unwrap();
            assert!(
                prompt.contains(&format!(
                    "<goal-objective-json>{objective}</goal-objective-json>"
                )),
                "{}",
                case.name
            );
        }
        let after = durable_rows(&access).await;
        assert_eq!(
            before["GoalCreationClaim"], after["GoalCreationClaim"],
            "{}",
            case.name
        );
        if expected.is_none() || expected == Some(current_case) {
            assert_eq!(before, after, "{} must not write", case.name);
        }
        assert_eq!(after["Goal"][0]["goal_id"], before["Goal"][0]["goal_id"]);
        node.shutdown().await.expect("node shutdown");
    }
}

#[test]
fn generated_goal_objective_whitespace_matches_native_character_classification() {
    let contracts: Contracts = gents_lean_contract::load_contract_snapshot().unwrap();
    let native: Vec<u32> = (0..=0x10ffff)
        .filter_map(char::from_u32)
        .filter(|character| character.is_whitespace())
        .map(u32::from)
        .collect();
    assert_eq!(native, contracts.goal_objective_whitespace_codepoints);
}
