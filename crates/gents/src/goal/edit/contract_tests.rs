use super::*;
use gents_protocol::error_chain::ErrorChain;
use serde_json::Value;

#[derive(Deserialize)]
struct Contracts {
    goal_edit_cases: Vec<Case>,
    goal_objective_whitespace_codepoints: Vec<u32>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    origin: GoalEditOrigin,
    observed: Assignment,
    current: Assignment,
    fields: GoalEdit,
    expected: Option<Assignment>,
}

#[derive(Debug, Deserialize, PartialEq)]
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

async fn durable_rows(node: &EmbeddedNode) -> Value {
    graphql_with_transaction_retry(node, "{ Goal { _docID _version { cid } goal_id objective status token_budget continuation_sequence updated_at } GoalCreationClaim { _docID creation_key goal_id objective token_budget } }", "inspect edited goal").await.unwrap().data.unwrap()
}

#[tokio::test]
async fn generated_goal_edit_cases_drive_real_transactions() {
    let contracts: Contracts = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(contracts.goal_edit_cases.len(), 66);
    for case in contracts.goal_edit_cases {
        let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let created =
            create_goal_for_session(&node, "did:test:goal-edit", "session", "ship", Some(100))
                .await
                .unwrap();
        let mut observed = match created {
            CreateGoalForSessionOutcome::Created(goal)
            | CreateGoalForSessionOutcome::Idempotent(goal) => goal,
        };
        let access = crate::ConfigAccess::local(node.clone());
        access.write("test.goal_edit_state", &format!(
            "mutation {{ update_Goal(filter: {{ _docID: {{ _eq: \"{}\" }} }}, input: {{ status: \"{}\", continuation_sequence: {}, objective: \"{}\", {} }}) {{ _docID }} }}",
            escape_graphql_string(&observed.doc_id), escape_graphql_string(case.current.status.as_str()), case.current.sequence,
            escape_graphql_string(&case.current.objective), optional_int_graphql_field("token_budget", case.current.budget),
        )).await.unwrap();
        observed.status = case.observed.status.as_str().to_owned();
        observed.continuation_sequence = Some(case.observed.sequence);
        observed.objective = case.observed.objective.clone();
        observed.token_budget = case.observed.budget;
        assert_eq!(
            assignment(&observed),
            case.observed,
            "{} observation",
            case.name
        );
        let current = load_canonical_goal(&node, &observed.node_did, &observed.session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            assignment(&current),
            case.current,
            "{} durable input",
            case.name
        );
        let before = durable_rows(&node).await;
        let edit = match case.origin {
            GoalEditOrigin::Agent => edit_active_goal(&node, &observed, &case.fields).await,
            GoalEditOrigin::Client => {
                let observed = &observed;
                let fields = &case.fields;
                crate::ConfigAccess::transact_local_write(
                    &node,
                    None,
                    "test.goal_client_edit",
                    |txn| {
                        Box::pin(async move {
                            stage_goal_edit(txn, observed, fields, GoalEditOrigin::Client).await
                        })
                    },
                )
                .await
            }
        };
        let result = match edit {
            Ok(result) => result,
            Err(error) => {
                assert!(
                    case.fields.validate().is_err(),
                    "{}: {}",
                    case.name,
                    ErrorChain(&error)
                );
                None
            }
        };
        assert_eq!(
            result.as_ref().map(assignment),
            case.expected,
            "{}",
            case.name
        );
        if let (Some(updated), Some(expected)) = (&result, &case.expected) {
            let prompt =
                crate::trigger_engine::goal_source::continuation_prompt(updated, None, false);
            let objective = serde_json::to_string(&expected.objective).unwrap();
            assert!(
                prompt.contains(&format!(
                    "<goal-objective-json>{objective}</goal-objective-json>"
                )),
                "{}",
                case.name
            );
        }
        let after = durable_rows(&node).await;
        assert_eq!(
            before["GoalCreationClaim"], after["GoalCreationClaim"],
            "{}",
            case.name
        );
        if case.expected.is_none() || case.expected == Some(case.current) {
            assert_eq!(before, after, "{} must not write", case.name);
        }
        assert_eq!(after["Goal"][0]["goal_id"], before["Goal"][0]["goal_id"]);
        node.shutdown().await;
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
    let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let owner = "did:test:goal-edit";
    let initial = set_goal(&node, owner, "model", Some("ship"), None, Some(Some(100)))
        .await
        .unwrap();
    let fields = GoalEdit {
        objective: Some("  verify \"release\"  ".into()),
        budget: Some(200),
    };
    let edited = edit_active_goal(&node, &initial, &fields)
        .await
        .unwrap()
        .unwrap();
    set_goal(
        &node,
        owner,
        "operator",
        Some("ship"),
        None,
        Some(Some(100)),
    )
    .await
    .unwrap();
    let operator = set_goal(
        &node,
        owner,
        "operator",
        fields.objective.as_deref(),
        None,
        Some(fields.budget),
    )
    .await
    .unwrap();
    assert_eq!(assignment(&edited), assignment(&operator));
    assert_eq!(edited.goal_id, initial.goal_id);
    let mut foreign = initial.clone();
    foreign.doc_id = "other-document".into();
    assert!(edit_active_goal(&node, &foreign, &fields)
        .await
        .unwrap()
        .is_none());
    let mut foreign = initial;
    foreign.goal_id = "other-goal".into();
    assert!(edit_active_goal(&node, &foreign, &fields)
        .await
        .unwrap()
        .is_none());
    assert_eq!(edited.objective, "verify \"release\"");
    assert!(set_goal(&node, owner, "operator", Some("  "), None, None)
        .await
        .is_err());
    node.shutdown().await;
}
