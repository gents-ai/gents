use super::*;
use gents_lean_vocab::families::goal_edit_contracts_edit as family;
use gents_store::identity::NodeIdentity;
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

type WireAssignment = family::Assignment<GoalStatus>;

fn wire_assignment(goal: &GoalDocument) -> WireAssignment {
    WireAssignment {
        status: goal.parsed_status().unwrap(),
        sequence: u64::try_from(goal.continuation_sequence()).unwrap(),
        objective: goal.objective.clone(),
        budget: goal.token_budget,
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
    let cases = family::cases::<GoalEditOrigin, GoalStatus>();
    assert_eq!(cases.len(), 66);
    for case in &cases {
        let input = &case.input;
        let fields = GoalEdit {
            objective: input.fields.objective.clone(),
            budget: input.fields.budget,
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
                    &serde_json::json!({"input": {"status": input.current.status,
                        "continuation_sequence": input.current.sequence, "objective": input.current.objective.as_str(),
                        "token_budget": input.current.budget}}),
                    gents_protocol::schemas::GOAL,
                ).await
            })
        }).await.unwrap();
        observed.status = serde_json::to_value(input.observed.status)
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned();
        observed.continuation_sequence = Some(i64::try_from(input.observed.sequence).unwrap());
        observed.objective = input.observed.objective.clone();
        observed.token_budget = input.observed.budget;
        assert!(
            same(&wire_assignment(&observed), &input.observed),
            "{} observation",
            input.name
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
            same(&wire_assignment(&current), &input.current),
            "{} durable input",
            input.name
        );
        let before = durable_rows(&access).await;
        let edit = match input.origin {
            GoalEditOrigin::Agent => {
                edit_active_goal_from_access(&agent_access, &observed, &fields).await
            }
            GoalEditOrigin::Client => {
                let observed = &observed;
                let fields = &fields;
                access
                    .transact_write("test.goal_client_edit", |txn| {
                        Box::pin(async move {
                            stage_goal_edit(txn, observed, fields, GoalEditOrigin::Client).await
                        })
                    })
                    .await
            }
        };
        let result = match edit {
            Ok(result) => result,
            Err(error) => {
                assert!(fields.validate().is_err(), "{}: {error:#}", input.name);
                None
            }
        };
        let edited = result.as_ref().map(wire_assignment);
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
                input.name
            );
        }
        let after = durable_rows(&access).await;
        assert_eq!(
            before["GoalCreationClaim"], after["GoalCreationClaim"],
            "{}",
            input.name
        );
        if edited
            .as_ref()
            .is_none_or(|edited| same(edited, &wire_assignment(&current)))
        {
            assert_eq!(before, after, "{} must not write", input.name);
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

#[tokio::test]
async fn edited_fields_match_operator_updates_without_changing_goal_identity() {
    let (node_db, _user, _user_access, binding) = gents_inference::test_support::signed_test_node(
        "edited-fields-match-operator-updates-wit",
        crate::lifecycle_hooks::composition(),
    )
    .await;
    let node = node_db.node.clone();
    let root = gents_store::test_support::held_runtime_identity(node_db.node_identity.did());
    let agent = gents_store::identity::resolve_agent_identity(&root, &binding).unwrap();
    let access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    for session in ["model", "operator"] {
        access
            .transact_write("test.goal_edit.session", |txn| {
                Box::pin(gents_session::session::ensure_session_in_txn(
                    txn,
                    session,
                    root.did(),
                    agent.did(),
                    Some(agent.did()),
                    None,
                    None,
                    "2026-11-04T00:00:00Z",
                ))
            })
            .await
            .unwrap();
    }
    let owner = root.did();
    let initial = set_goal_from_access(
        &access,
        owner,
        "model",
        Some("ship"),
        None,
        Some(Some(100)),
        None,
    )
    .await
    .unwrap();
    let fields = GoalEdit {
        objective: Some("  verify \"release\"  ".into()),
        budget: Some(200),
    };
    let edited = edit_active_goal_from_access(&access, &initial, &fields)
        .await
        .unwrap()
        .unwrap();
    set_goal_from_access(
        &access,
        owner,
        "operator",
        Some("ship"),
        None,
        Some(Some(100)),
        None,
    )
    .await
    .unwrap();
    let operator = set_goal_from_access(
        &access,
        owner,
        "operator",
        fields.objective.as_deref(),
        None,
        Some(fields.budget),
        None,
    )
    .await
    .unwrap();
    assert_eq!(assignment(&edited), assignment(&operator));
    assert_eq!(edited.goal_id, initial.goal_id);
    let mut foreign = initial.clone();
    foreign.doc_id = "other-document".into();
    assert!(edit_active_goal_from_access(&access, &foreign, &fields)
        .await
        .unwrap()
        .is_none());
    let mut foreign = initial;
    foreign.goal_id = "other-goal".into();
    assert!(edit_active_goal_from_access(&access, &foreign, &fields)
        .await
        .unwrap()
        .is_none());
    assert_eq!(edited.objective, "verify \"release\"");
    assert!(
        set_goal_from_access(&access, owner, "operator", Some("  "), None, None, None)
            .await
            .is_err()
    );
    node.shutdown().await.expect("node shutdown");
}
