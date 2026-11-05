use super::*;
use crate::NodeIdentity;
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

async fn durable_rows(access: &crate::ConfigAccess) -> Value {
    access.execute("{ Goal { _docID _version { cid } goal_id objective status token_budget continuation_sequence updated_at } GoalCreationClaim { _docID creation_key goal_id objective token_budget } }").await.unwrap()["data"].clone()
}

#[tokio::test]
async fn generated_goal_edit_cases_drive_real_transactions() {
    let contracts: Contracts = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(contracts.goal_edit_cases.len(), 66);
    for case in contracts.goal_edit_cases {
        let temp = tempfile::tempdir().unwrap();
        let signer =
            crate::KeyIdentity::load_or_create(temp.path().join("node.key"), None).unwrap();
        let user = crate::KeyIdentity::load_or_create(temp.path().join("user.key"), None).unwrap();
        let binding = crate::store::identity::provision_agent_identity(&signer).unwrap();
        let agent = crate::store::identity::resolve_agent_identity(&signer, &binding).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
        let node = std::sync::Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(signer.did())
                .with_http(defra_node::HttpConfig::with_listener(listener).unwrap())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let access = crate::ConfigAccess::Graphql(
            crate::store::config_client::GraphqlEndpoint::as_principal(&endpoint, user.did())
                .with_client_signing_capability(user.client_signing_capability().unwrap())
                .unwrap()
                .with_client_signed_agent_creates(),
        );
        let agent_access = crate::ConfigAccess::local_managed_agent(
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
                let doc = crate::store::graphql::created_doc_id(&created, "AgentSession")?;
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
            let case = &case;
            let doc = &doc;
            Box::pin(async move {
                txn.execute_client_signed_update(
                    &format!(r#"mutation($input: GoalMutationInputArg!) {{update_Goal(docID: "{doc}", input: $input) {{_docID}}}}"#),
                    &serde_json::json!({"input": {"status": case.current.status.as_str(),
                        "continuation_sequence": case.current.sequence, "objective": case.current.objective,
                        "token_budget": case.current.budget}}),
                    gents_protocol::schemas::GOAL,
                ).await
            })
        }).await.unwrap();
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
            case.current,
            "{} durable input",
            case.name
        );
        let before = durable_rows(&access).await;
        let edit = match case.origin {
            GoalEditOrigin::Agent => {
                edit_active_goal_from_access(&node, &agent_access, &observed, &case.fields).await
            }
            GoalEditOrigin::Client => {
                let observed = &observed;
                let fields = &case.fields;
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
                assert!(case.fields.validate().is_err(), "{}: {error:#}", case.name);
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
        if case.expected.is_none() || case.expected == Some(case.current) {
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
    let root = crate::KeyIdentity::load_or_create(directory.path().join("root.key"), None).unwrap();
    let binding = crate::store::identity::provision_agent_identity(&root).unwrap();
    let agent = crate::store::identity::resolve_agent_identity(&root, &binding).unwrap();
    let node = std::sync::Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(root.did())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let access = crate::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    for session in ["model", "operator"] {
        access
            .transact_write("test.goal_edit.session", |txn| {
                Box::pin(crate::session::ensure_session_in_txn(
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
    let edited = edit_active_goal_from_access(&node, &access, &initial, &fields)
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
    assert!(
        edit_active_goal_from_access(&node, &access, &foreign, &fields)
            .await
            .unwrap()
            .is_none()
    );
    let mut foreign = initial;
    foreign.goal_id = "other-goal".into();
    assert!(
        edit_active_goal_from_access(&node, &access, &foreign, &fields)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(edited.objective, "verify \"release\"");
    assert!(
        set_goal_from_access(&access, owner, "operator", Some("  "), None, None, None)
            .await
            .is_err()
    );
    node.shutdown().await.expect("node shutdown");
}
