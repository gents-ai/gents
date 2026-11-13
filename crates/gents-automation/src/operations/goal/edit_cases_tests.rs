use crate::goal::{
    create_goal_for_session_from_access, load_canonical_goal_in_txn, CreateGoalForSessionOutcome,
    GoalDocument,
};
use gents_protocol::goal_status::GoalStatus;
use gents_lean_vocab::families::goal_edit_contracts_edit as edit_family;
use gents_protocol::operations::goal::{GoalExpected, GoalOperationError, GoalSetInput};
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Deserialize)]
struct Contracts {
    goal_objective_whitespace_codepoints: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Origin {
    Agent,
    Client,
}

type WireAssignment = edit_family::Assignment<GoalStatus>;
type WireEdited = edit_family::Edited<GoalStatus>;

fn wire_assignment(goal: &GoalDocument) -> WireAssignment {
    WireAssignment {
        status: goal.parsed_status().unwrap(),
        sequence: u64::try_from(goal.continuation_sequence()).unwrap(),
        objective: goal.objective.clone(),
        budget: goal.token_budget,
    }
}

fn wire_edited(goal: &GoalDocument) -> WireEdited {
    WireEdited {
        assignment: wire_assignment(goal),
        auto_resume: goal.auto_resume_at_reset,
    }
}

fn same(left: &WireAssignment, right: &WireAssignment) -> bool {
    serde_json::to_value(left).unwrap() == serde_json::to_value(right).unwrap()
}

async fn durable_rows(access: &gents_store::config_client::ConfigAccess) -> Value {
    access.execute("{ Goal { _docID _version { cid } goal_id objective status token_budget continuation_sequence updated_at } GoalCreationClaim { _docID creation_key goal_id objective token_budget } }").await.unwrap()["data"].clone()
}

#[tokio::test]
async fn generated_goal_edit_cases_drive_real_transactions() {
    let cases = gents_lean_vocab::families::goal_edit_contracts_edit::cases::<Origin, GoalStatus>();
    for case in &cases {
        let row = &case.input;
        let input = GoalSetInput {
            objective: row.fields.objective.clone(),
            token_budget: row.fields.budget.map(Some),
            auto_resume: row.auto_resume,
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
                    &serde_json::json!({"input": {"status": row.current.status,
                        "continuation_sequence": row.current.sequence, "objective": row.current.objective.as_str(),
                        "token_budget": row.current.budget,
                        "auto_resume_at_reset": row.stored_auto_resume}}),
                    gents_protocol::schemas::GOAL,
                ).await
            })
        }).await.unwrap();
        observed.status = serde_json::to_value(row.observed.status)
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned();
        observed.continuation_sequence = Some(i64::try_from(row.observed.sequence).unwrap());
        observed.objective = row.observed.objective.clone();
        observed.token_budget = row.observed.budget;
        assert!(
            same(&wire_assignment(&observed), &row.observed),
            "{} observation",
            row.name
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
        assert!(
            same(&wire_assignment(&current), &row.current),
            "{} durable input",
            row.name
        );
        assert_eq!(current.auto_resume_at_reset, row.stored_auto_resume, "{}", row.name);
        let before = durable_rows(&access).await;
        let edit = match row.origin {
            Origin::Agent => {
                super::set_as_working_agent(&agent_access, &observed, &input, None, "request").await
            }
            Origin::Client => {
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
            Err(GoalOperationError::Passthrough(error)) => panic!("{}: {error:#}", row.name),
            Err(_) => None,
        };
        let edited = result.as_ref().map(wire_edited);
        case.check(edited.clone());
        if let Some(updated) = &result {
            let prompt =
                crate::triggers::engine::goal_source::continuation_prompt(updated, None, false);
            let objective = serde_json::to_string(&updated.objective).unwrap();
            assert!(
                prompt.contains(&format!(
                    "<goal-objective-json>{objective}</goal-objective-json>"
                )),
                "{}",
                row.name
            );
        }
        let after = durable_rows(&access).await;
        assert_eq!(
            before["GoalCreationClaim"], after["GoalCreationClaim"],
            "{}",
            row.name
        );
        if edited.as_ref().is_none_or(|edited| {
            same(&edited.assignment, &wire_assignment(&current))
                && edited.auto_resume == current.auto_resume_at_reset
        }) {
            assert_eq!(before, after, "{} must not write", row.name);
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
