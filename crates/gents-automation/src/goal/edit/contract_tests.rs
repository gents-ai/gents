use super::*;
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
        let fields = GoalEdit {
            objective: case.fields.objective.clone(),
            budget: case.fields.budget,
        };
        let db = gents_inference::test_support::test_db(
            "goal-edit-contract",
            crate::lifecycle_hooks::composition(),
        )
        .await;
        let node = db.node.clone();
        let signer = gents_store::test_support::held_runtime_identity(db.node_identity.did());
        let temp = tempfile::tempdir().unwrap();
        let user =
            gents_store::identity::KeyIdentity::load_or_create(temp.path().join("user.key"), None)
                .unwrap();
        let binding = gents_store::identity::provision_agent_identity(&signer).unwrap();
        let agent = gents_store::identity::resolve_agent_identity(&signer, &binding).unwrap();
        let access = db.client_access(&user);
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
        let origin: GoalEditOrigin =
            serde_json::from_value(Value::String(case.origin.clone())).unwrap();
        let edit = match origin {
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
                assert!(fields.validate().is_err(), "{}: {error:#}", case.name);
                None
            }
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

#[tokio::test]
async fn edited_fields_match_operator_updates_without_changing_goal_identity() {
    let directory = tempfile::tempdir().unwrap();
    let root =
        gents_store::identity::KeyIdentity::load_or_create(directory.path().join("root.key"), None)
            .unwrap();
    let binding = gents_store::identity::provision_agent_identity(&root).unwrap();
    let agent = gents_store::identity::resolve_agent_identity(&root, &binding).unwrap();
    let node_db = gents_inference::test_support::test_db(
        "edited-fields-match-operator-updates-wit",
        crate::lifecycle_hooks::composition(),
    )
    .await;
    let node = node_db.node.clone();
    let root = gents_store::test_support::held_runtime_identity(node_db.node_identity.did());
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
