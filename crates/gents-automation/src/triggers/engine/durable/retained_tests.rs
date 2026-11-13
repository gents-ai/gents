use super::*;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::GoalAssignmentInput;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::{KeyIdentity, NodeIdentity};
use serde_json::{json, Value};
use std::sync::Arc;

async fn signed_request(
    access: &ConfigAccess,
    owner: &str,
    identity: &dyn NodeIdentity,
    id: &str,
    session: &str,
    goal_id: &str,
) -> String {
    let mut request = AgentRequestCreate::base(
        RequestPurpose::Normal,
        id,
        owner,
        identity.did(),
        identity.did(),
        session,
        "Task assignment input",
        "interactive",
        "2026-10-21T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(identity.did()),
    );
    request.input.goal_assignment = Some(GoalAssignmentInput {
        goal_id: goal_id.into(),
        root_request_doc_id: None,
    });
    gents_session::request::admission::sign_agent_request_create(identity, &mut request)
        .await
        .unwrap();
    let response = access
        .write(
            "test.retained_fire_request",
            &request.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
    let doc = gents_store::graphql::created_doc_id(&response, "AgentRequest").unwrap();
    access
        .write(
            "test.retained_fire_terminal",
            &format!(
                r#"mutation {{update_AgentRequest(docID: "{}", input: {{lifecycle_state: "completed"}}) {{_docID}}}}"#,
                escape_graphql_string(&doc)
            ),
        )
        .await
        .unwrap();
    doc
}

/// The Task's Agent opens the session its fires and outcomes belong to.
async fn task_session(access: &ConfigAccess, owner: &str, agent: &str, session: &str) {
    access
        .transact_write("test.retained_task_session", |txn| {
            Box::pin(gents_session::session::ensure_session_in_txn(
                txn,
                session,
                owner,
                agent,
                None,
                None,
                None,
                "2026-10-21T00:00:00Z",
            ))
        })
        .await
        .unwrap();
}

/// Goals bind their physical session when one exists for their owner/session.
async fn goal_document(access: &ConfigAccess, input: Value) -> String {
    let result = access
        .transact_write("test.retained_fire_goal", |txn| {
            let mut input = input.clone();
            Box::pin(async move {
                if let Some(session) =
                    gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                        txn,
                        input["node_did"].as_str().unwrap(),
                        input["session_id"].as_str().unwrap(),
                    )
                    .await?
                {
                    input["session_doc_id"] = session.doc_id.into();
                }
                let response = txn.execute_with_variables(
                    "mutation($input: GoalMutationInputArg!) {create_Goal(input: $input) {_docID}}",
                    &json!({"input":input}),
                ).await?;
                Ok::<_, anyhow::Error>(gents_store::graphql::created_doc_id(&response, "Goal")?)
            })
        })
        .await
        .unwrap();
    result
}

async fn outcome_rows(access: &ConfigAccess) -> Vec<Value> {
    access
        .execute("{FireOutcome {handoff_id fire_key request_id session_id goal_id terminal_state reason source_handoff_id created_at}}")
        .await.unwrap()["data"]["FireOutcome"].as_array().unwrap().clone()
}

#[tokio::test]
async fn retained_task_outcomes_follow_modeled_assignment_history() {
    for case in gents_lean_vocab::families::trigger_delivery_retained_outcome_recovery::cases::<String>() {
        let name = case.input.name.as_str();
        let directory = tempfile::tempdir().unwrap();
        let other = KeyIdentity::load_or_create(directory.path().join("other.key"), None).unwrap();
        let node_db = gents_inference::test_support::test_db_with_tool_policy(
            "retained-task-outcomes-follow-modeled-as",
            crate::lifecycle_hooks::composition(),
            Arc::new(gents_tools::surface::ToolCeiling::meta_only()),
        )
        .await;
        let (mut node_db, _user, user_access, binding) =
            gents_inference::test_support::signed_test_node_from_db(
                node_db,
                "retained-task-outcomes-follow-modeled-as",
            )
            .await;
        let node = node_db.node.clone();
        let identity =
            gents_store::test_support::held_runtime_identity(node_db.node_identity.did());
        // Task requests, receipts, Goals and outcomes belong to the Task's
        // Agent; it writes and recovers them with its managed signer, and
        // retires the Goals it created through its selected Goal-creation tools.
        let agent = gents_store::identity::resolve_agent_identity(&identity, &binding).unwrap();
        let tools = serde_json::json!({
            "node_did": identity.did(),
            "tools_id": format!("{}:tools", binding.agent_did),
            "built_ins": {"enable_goal_tools": true, "enable_goal_creation": true}
        });
        let plan = gents_documents::DesiredStateApplyPlan::new(vec![
            gents_documents::DesiredStateApplyDocument {
                collection: gents_store::collection::Collection::Tools,
                add: tools.clone(),
                update: tools,
            },
        ])
        .unwrap();
        user_access
            .transact_write("test.retained_task_agent_tools", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    gents_documents::apply_desired_state_plan(
                        txn,
                        plan,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await
                })
            })
            .await
            .unwrap();
        let agent_access = |node: &Arc<defra_node::Node>| {
            ConfigAccess::local_managed_agent(
                node.clone(),
                ::identity::Did::new(agent.did()).unwrap(),
                agent.as_ref(),
            )
            .unwrap()
        };
        let access = agent_access(&node);
        let session = case.input.request.fire.session.as_str();
        task_session(&access, identity.did(), agent.did(), session).await;
        let mut fire_identity: FireIdentity =
            gents_lean_vocab::family::recode(&case.input.request.fire.identity);
        fire_identity.owner_did = identity.did().into();
        let fire = TriggerFire {
            fire_key: fire_identity.fire_key(),
            request_id: fire_identity.request_id(),
            identity: fire_identity,
            task_id: "retained-task".into(),
            session_id: session.into(),
            goal_id: Some("task-goal".into()),
            goal_objective: Some("Original Task assignment".into()),
            goal_token_budget: None,
            goal_assignment_applied: true,
            emit_outcome: true,
            queued_serial: true,
            source_handoff_id: Some("retained-source-handoff".into()),
            reply_session_id: None,
            shard_id: None,
            attempt: None,
            created_at: "2026-10-21T00:00:00Z".into(),
        };
        let root = signed_request(
            &access,
            identity.did(),
            agent.as_ref(),
            &fire.request_id,
            session,
            "task-goal",
        )
        .await;
        let reused = case.input.reassigned_in_place;
        let next_root = signed_request(
            &access,
            identity.did(),
            agent.as_ref(),
            "fresh-request",
            session,
            if reused { "task-goal" } else { "fresh-goal" },
        )
        .await;
        access
            .transact_write("test.retained_fire_receipt", |txn| {
                let fire = &fire;
                Box::pin(async move { stage_fire_receipt(txn, fire).await })
            })
            .await
            .unwrap();
        let old_doc = if let Some(retained) = &case.input.retained {
            Some(goal_document(&access, json!({
                "goal_id":"task-goal",
                "node_did": if retained.owner == "owner-a" { identity.did() } else { other.did() },
                "session_id": retained.session,
                "objective":"Original Task assignment",
                "status":retained.status,
                "assignment_root_request_doc_id": if retained.assignment.source_doc_id == "retained" { &root } else { &next_root },
                "tokens_used":317, "token_usage_baseline":100, "active_time_seconds":23
            })).await)
        } else {
            None
        };
        let settled = case.input.settled;
        if settled {
            let binding = GoalOutcomeBinding {
                owner_did: identity.did().into(),
                session_id: session.into(),
                assignment_request_id: fire.request_id.clone(),
                status: case.input.retained.as_ref().unwrap().status.clone(),
            };
            access
                .transact_write("test.retained_outcome_settled", |txn| {
                    let fire = &fire;
                    let binding = &binding;
                    Box::pin(async move {
                        stage_outcome(
                            txn,
                            fire,
                            OutcomeStage {
                                goal: Some(binding),
                                assignment_replaced: false,
                                request_terminal: true,
                                terminal_state: "completed",
                                reason: &binding.status,
                            },
                            "2026-10-21T00:00:01Z",
                        )
                        .await
                    })
                })
                .await
                .unwrap();
        }
        let before = outcome_rows(&access).await;
        if reused {
            access.write("test.reuse_task_goal_assignment", &format!(
                r#"mutation {{update_Goal(docID: "{}", input: {{assignment_root_request_doc_id: "{}", status: "active", objective: "New Task assignment"}}) {{_docID}}}}"#,
                escape_graphql_string(old_doc.as_ref().unwrap()), escape_graphql_string(&next_root)
            )).await.unwrap();
        } else {
            if let Some(doc) = &old_doc {
                access
                    .write(
                        "test.retire_task_goal",
                        &format!(
                            r#"mutation {{delete_Goal(docID: "{}") {{_docID}}}}"#,
                            escape_graphql_string(doc)
                        ),
                    )
                    .await
                    .unwrap();
            }
            if let Some(current) = &case.input.current {
                goal_document(&access, json!({
                    "goal_id":"fresh-goal",
                    "node_did":if current.owner == "owner-a" { identity.did() } else { other.did() },
                    "session_id":current.session, "objective":"Fresh assignment", "status":current.status,
                    "assignment_root_request_doc_id":next_root,
                    "tokens_used":0, "token_usage_baseline":417, "active_time_seconds":0
                })).await;
            }
        }
        let signed = access
            .execute(&format!(
                "{{AgentRequest {{{}}}}}",
                gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
            ))
            .await
            .unwrap();
        let requests: Vec<gents_protocol::row::AgentRequestRow> =
            serde_json::from_value(signed["data"]["AgentRequest"].clone()).unwrap();
        for request in requests {
            gents_store::identity::request_receipt::verify_request_receipt_signature(&request)
                .unwrap();
        }
        if settled {
            access
                .write(
                    "test.retire_settled_request",
                    &format!(
                        r#"mutation {{delete_AgentRequest(docID: "{}") {{_docID}}}}"#,
                        escape_graphql_string(&root)
                    ),
                )
                .await
                .unwrap();
        }
        let rollback: Result<()> = access
            .transact_write("test.retained_outcome_rollback", |txn| {
                let owner = identity.did();
                Box::pin(async move {
                    recover_outcomes_in_txn(txn, owner).await?;
                    anyhow::bail!("injected outcome publication rollback")
                })
            })
            .await;
        let rollback = rollback.unwrap_err();
        assert!(
            rollback
                .to_string()
                .contains("injected outcome publication rollback"),
            "{name}: {rollback:#}"
        );
        assert_eq!(outcome_rows(&access).await, before, "{name}: rollback");
        drop(access);
        drop(node);
        node_db.simulate_process_crash().await.unwrap();
        let node = node_db.node.clone();
        let access = agent_access(&node);
        let recovered = recover_outcomes(&node, identity.did(), agent.as_ref())
            .await
            .unwrap();
        assert_eq!(
            recover_outcomes(&node, identity.did(), agent.as_ref())
                .await
                .unwrap(),
            0,
            "{name}: replay"
        );
        let after = outcome_rows(&access).await;
        assert_eq!(after.len(), before.len() + recovered, "{name}");
        let recovered_row = (recovered > 0).then(|| &after[0]);
        case.check(
            gents_lean_vocab::families::trigger_delivery_retained_outcome_recovery::RetainedRecovery {
                recovered: recovered as u64,
                rows_changed: after != before,
                recover_reason: recovered_row
                    .map(|row| row["terminal_state"].as_str().unwrap().to_owned()),
                request_matches: recovered_row
                    .is_some_and(|row| row["request_id"] == fire.request_id),
            },
        );
        node.shutdown().await.expect("node shutdown");
    }
}
