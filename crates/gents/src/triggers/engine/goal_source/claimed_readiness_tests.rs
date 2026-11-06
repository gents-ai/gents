use super::*;
use crate::goal::{
    claim_continuation_from_access, claim_retry_continuation_from_access,
    gate_claimed_goal_continuation, set_goal_from_access,
    stop_claimed_continuation_for_unavailable_agent_from_access,
    update_goal_fields_if_status_from_access, GoalAgentObservation,
};
use crate::store::graphql::graphql_with_transaction_retry;
use crate::support::{test_db, TestDb};
use gents_protocol::node_readiness::{
    AgentReadinessEntry, AgentReadinessState, AgentReadinessUnavailableReason,
    NodeReadinessProcessState, NodeReadinessSnapshot, NODE_READINESS_FORMAT_VERSION,
};
use serde_json::{json, Value};

const SESSION: &str = "claimed-readiness-session";
const BEHAVIOR: &str = "claimed-readiness-behavior";

fn recovery_projection(goal: &GoalDocument) -> Value {
    json!({
        "status": goal.status,
        "blocked_audits": goal.consecutive_blocked_audits.unwrap_or_default(),
        "wrapup_requested": goal.wrapup_requested.unwrap_or(false),
        "wrapup_completed": goal.wrapup_completed.unwrap_or(false),
        "infrastructure_retries": goal.infrastructure_retry_count.unwrap_or_default(),
        "last_failure": goal.last_failure,
        "continuation_sequence": goal.continuation_sequence(),
        "parent": goal.last_continued_from_request_id,
    })
}

async fn source(
    db: &TestDb,
) -> (
    GoalSource,
    crate::runtime::status::RuntimeStatusHandle,
    crate::runtime::status::RuntimeStatusOwner,
) {
    let agent_identity = selected_identity(db).await;
    let agent_did = agent_identity.did().to_owned();
    let unavailable = crate::runtime::snapshot::UnavailableAgent::new(
        AgentReadinessUnavailableReason::RuntimeConfigurationInvalid,
        "test fixture identity",
    )
    .with_watcher_identity(agent_identity);
    let snapshot = Arc::new(ActiveRuntimeSnapshot {
        generation: 1,
        node: None,
        local_did: db.node_identity.did().to_owned(),
        default_agent_did: agent_did.clone(),
        agents: Default::default(),
        tool_surfaces: Default::default(),
        backend_admission_configs: Default::default(),
        unavailable_agents: [(agent_did, unavailable)].into_iter().collect(),
        active_schedules: Default::default(),
        unavailable_schedules: Default::default(),
        active_event_triggers: Default::default(),
        unavailable_event_triggers: Default::default(),
        active_tasks: Default::default(),
        dispatchers: Default::default(),
        agent_executor_capacities: Default::default(),
        agent_executor_queue_capacities: Default::default(),
    });
    let (owner, status) =
        crate::runtime::status::RuntimeStatusHandle::start(db.node.clone(), db.node_identity.did());
    status.publish_applied(&snapshot).await;
    let (_, rx) = watch::channel(snapshot);
    (
        GoalSource::new(rx, db.node.clone(), CancellationToken::new())
            .with_runtime_status(status.clone()),
        status,
        owner,
    )
}

async fn selected_agent(db: &TestDb) -> String {
    let query = format!(
        r#"{{ Agent(filter: {{ node_did: {{ _eq: "{}" }}, display_name: {{ _eq: "{}" }} }}, limit: 2) {{ agent_did }} }}"#,
        escape_graphql_string(db.node_identity.did()),
        escape_graphql_string(BEHAVIOR)
    );
    let rows = ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(db.node_identity.did()).unwrap(),
    )
    .execute(&query)
    .await
    .unwrap();
    rows["data"]["Agent"][0]["agent_did"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn selected_identity(db: &TestDb) -> Arc<dyn crate::store::identity::NodeIdentity> {
    let agent_did = selected_agent(db).await;
    let agent = crate::load_agent(&db.node, &agent_did)
        .await
        .unwrap()
        .unwrap();
    let binding = gents_protocol::agent_identity::AgentIdentityBinding {
        agent_did: agent.agent_did,
        creation_nonce: agent.creation_nonce,
    };
    crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding).unwrap()
}

async fn agent_access(db: &TestDb) -> ConfigAccess {
    let agent = selected_identity(db).await;
    ConfigAccess::local_managed_agent(
        db.node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap()
}

async fn load_goal(db: &TestDb) -> GoalDocument {
    crate::goal::load_canonical_goal(&db.node, db.node_identity.did(), SESSION)
        .await
        .unwrap()
        .unwrap()
}

async fn child_count(db: &TestDb, goal: &GoalDocument, parent: &str) -> usize {
    let response = graphql_with_transaction_retry(
        &db.node,
        &format!(r#"{{ AgentRequest(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, caused_by_trigger_kind: {{_eq: "goal"}}, caused_by_trigger_id: {{_eq: "{}"}}, caused_by_parent_request_id: {{_eq: "{}"}}}}) {{_docID}} }}"#,
            escape_graphql_string(&goal.node_did), escape_graphql_string(&goal.session_id),
            escape_graphql_string(&goal.goal_id), escape_graphql_string(parent)),
        "count claimed readiness children",
    ).await.unwrap();
    response.data.unwrap()["AgentRequest"]
        .as_array()
        .unwrap()
        .len()
}

async fn install_claim(db: &TestDb, before: &Value) -> GoalDocument {
    install_claim_with_terminal(db, before, "failed").await
}

async fn install_claim_with_terminal(db: &TestDb, before: &Value, terminal: &str) -> GoalDocument {
    let agent = crate::test_support::install_test_agent(&db.node, db.node_identity.did(), BEHAVIOR)
        .await
        .agent_did;
    crate::session::ensure_session_with_agent_did_and_requester_did(
        &db.node,
        SESSION,
        db.node_identity.did(),
        &agent,
        Some(db.node_identity.did()),
    )
    .await
    .unwrap();
    let parent = before["parent"].as_str().unwrap();
    let spec = crate::RequestSpec::new(
        gents_protocol::request_admission::RequestPurpose::Normal,
        crate::request::lifecycle::RequestIdentity {
            requester_did: Some(db.node_identity.did().to_owned()),
            request_id: parent.to_owned(),
            node_did: db.node_identity.did().to_owned(),
            agent_did: agent.clone(),
            session_id: SESSION.to_owned(),
            content: "original goal attempt".to_owned(),
            execution_origin: crate::request::lifecycle::ExecutionOrigin::Interactive,
            created_at: "2020-01-01T00:00:00Z".to_owned(),
        },
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
            db.node_identity.did(),
        ),
    );
    let request = crate::build_signed_request(
        spec,
        crate::RequestSigner::Identity(db.node_identity.as_ref()),
    )
    .await
    .unwrap();
    crate::ConfigAccess::write_local(
        &db.node,
        "goal.claimed_readiness_test_parent",
        &request.graphql_mutation().unwrap(),
    )
    .await
    .unwrap();
    crate::ConfigAccess::write_local(
        &db.node,
        "goal.claimed_readiness_test_terminal",
        &format!(r#"mutation {{ update_AgentRequest(filter: {{request_id: {{_eq: "{}"}}, lifecycle_state: {{_eq: "pending"}}}}, input: {{lifecycle_state: "{}", terminalized_at: "2020-01-02T00:00:00Z"}}) {{_docID}} }}"#,
            escape_graphql_string(parent), escape_graphql_string(terminal)),
    ).await.unwrap();
    let access = agent_access(db).await;
    let goal = set_goal_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        Some("Recover the saved attempt"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .unwrap();
    if let Some(failure) = before["last_failure"].as_str() {
        assert!(claim_retry_continuation_from_access(
            &access,
            &goal,
            parent,
            before["infrastructure_retries"].as_i64().unwrap(),
            failure,
        )
        .await
        .unwrap());
    } else {
        assert!(claim_continuation_from_access(&access, &goal, parent)
            .await
            .unwrap());
    }
    let claimed = load_goal(db).await;
    assert!(update_goal_fields_if_status_from_access(&access, &claimed, GoalStatus::Active, &format!(
        r#"status: "{}", wrapup_requested: {}, wrapup_completed: {}, consecutive_blocked_audits: {}"#,
        escape_graphql_string(before["status"].as_str().unwrap()),
        before["wrapup_requested"], before["wrapup_completed"], before["blocked_audits"],
    )).await.unwrap());
    let installed = load_goal(db).await;
    assert_eq!(recovery_projection(&installed), *before);
    installed
}

async fn publish_readiness(
    db: &TestDb,
    observation: &str,
    status: &crate::runtime::status::RuntimeStatusHandle,
    settled: bool,
    newer_than_terminal: bool,
) {
    status
        .set_reconcile_phase(if settled {
            crate::runtime::status::ReconcilePhase::Idle
        } else {
            crate::runtime::status::ReconcilePhase::Applying
        })
        .await;
    let agent = selected_agent(db).await;
    let unavailable = match observation {
        "unavailable" => Some(AgentReadinessUnavailableReason::RuntimeConfigurationInvalid),
        "backend_recovering" => {
            Some(AgentReadinessUnavailableReason::BackendTemporarilyUnavailable)
        }
        _ => None,
    };
    let snapshot = NodeReadinessSnapshot {
        format_version: NODE_READINESS_FORMAT_VERSION,
        process_state: if observation == "unknown" {
            NodeReadinessProcessState::Recovering
        } else {
            NodeReadinessProcessState::Ready
        },
        active_generation: 1,
        router_generation: 1,
        default_agent_did: if observation == "unassigned" {
            "other-assigned-behavior".to_owned()
        } else {
            agent.clone()
        },
        agents: if observation == "unassigned" {
            vec![AgentReadinessEntry {
                agent_did: "other-assigned-behavior".to_owned(),
                state: AgentReadinessState::Ready,
                reason: None,
            }]
        } else {
            vec![AgentReadinessEntry {
                agent_did: agent.clone(),
                state: if unavailable.is_some() {
                    AgentReadinessState::Unavailable
                } else {
                    AgentReadinessState::Ready
                },
                reason: unavailable,
            }]
        },
    };
    let did = escape_graphql_string(db.node_identity.did());
    let payload = escape_graphql_string(&serde_json::to_string(&snapshot).unwrap());
    let at = if newer_than_terminal {
        Utc::now().to_rfc3339()
    } else {
        "2019-01-01T00:00:00Z".to_owned()
    };
    let at = escape_graphql_string(&at);
    crate::ConfigAccess::write_local(
        &db.node,
        "goal.claimed_readiness_test_observation",
        &format!(
            r#"mutation {{ upsert_NodeReadiness(filter: {{node_did: {{_eq: "{did}"}}}},
            add: {{node_did: "{did}", snapshot_json: "{payload}", updated_at: "{at}"}},
            update: {{snapshot_json: "{payload}", updated_at: "{at}"}}) {{_docID}} }}"#,
        ),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn required_question_holds_the_claim_until_its_native_item_is_acted() {
    let case = crate::lean_vocab_test::lean_contract_snapshot()
        .goal_claimed_readiness_cases
        .iter()
        .find(|case| case["name"] == "claimed_ready_materializes_retry")
        .unwrap();
    let mut before = case["before"].clone();
    before["infrastructure_retries"] = json!(0);
    before["last_failure"] = Value::Null;
    let db = test_db("required-question-continuation").await;
    let goal = install_claim_with_terminal(&db, &before, "completed").await;
    let parent = goal.last_continued_from_request_id.as_deref().unwrap();
    let (controller, status, _status_owner) = source(&db).await;
    publish_readiness(&db, "ready", &status, true, true).await;
    let payload = json!({"version":2,"prompt":"Choose the next step","options":[
        {"id":"small","label":"Small change"},{"id":"broad","label":"Broader change"}
    ]})
    .to_string();
    let did = escape_graphql_string(db.node_identity.did());
    let session = escape_graphql_string(SESSION);
    let behavior = escape_graphql_string(&selected_agent(&db).await);
    let response = crate::ConfigAccess::write_local(
        &db.node,
        "test.goal.required_question",
        &format!(
            r#"mutation {{ create_MailboxItem(input: {{
            item_key: "required-question", requester_did: "{did}", node_did: "{did}",
            status: "open", kind: "ask", action: "start_request", title: "Next step",
            payload: "{}", source_kind: "agent", source_id: "required-question-source",
            session_id: "{session}", request_id: "{}", target_node_did: "{did}",
            target_agent_did: "{behavior}", created_at: "2020-01-02T00:00:00Z"
        }}) {{ _docID }} }}"#,
            escape_graphql_string(&payload),
            escape_graphql_string(parent),
        ),
    )
    .await
    .unwrap();
    let item_id = crate::store::graphql::created_doc_id(&response, "MailboxItem").unwrap();
    for _ in 0..2 {
        assert!(controller
            .build_intent(
                &identity::Did::new(selected_agent(&db).await).unwrap(),
                load_goal(&db).await
            )
            .await
            .unwrap()
            .is_none());
        assert_eq!(child_count(&db, &goal, parent).await, 0);
        assert_eq!(recovery_projection(&load_goal(&db).await), before);
    }
    crate::ConfigAccess::write_local(&db.node, "test.goal.acted_question", &format!(
        r#"mutation {{ update_MailboxItem(filter: {{_docID: {{_eq: "{}"}}}},
            input: {{status: "acted", resolved_doc_id: "accepted-answer", resolved_at: "2030-01-01T00:00:00Z"}}) {{_docID}} }}"#,
        escape_graphql_string(&item_id),
    )).await.unwrap();
    assert!(controller
        .build_intent(
            &identity::Did::new(selected_agent(&db).await).unwrap(),
            load_goal(&db).await
        )
        .await
        .unwrap()
        .is_some());
    assert_eq!(child_count(&db, &goal, parent).await, 1);
    assert!(controller
        .build_intent(
            &identity::Did::new(selected_agent(&db).await).unwrap(),
            load_goal(&db).await
        )
        .await
        .unwrap()
        .is_none());
    assert_eq!(child_count(&db, &goal, parent).await, 1);
}

#[tokio::test]
async fn generated_claimed_readiness_cases_drive_goal_source() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().goal_claimed_readiness_cases;
    assert!(!cases.is_empty());
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let db = test_db(name).await;
        let before = install_claim(&db, &case["before"]).await;
        let parent = before.last_continued_from_request_id.as_deref().unwrap();
        let (controller, status, _status_owner) = source(&db).await;
        let observation = case["observation"].as_str().unwrap();
        let settled = case["settled"].as_bool().unwrap();
        let child_exists = case["child_exists"].as_bool().unwrap();
        let native_observation = match observation {
            "ready" => GoalAgentObservation::Ready {
                newer_than_terminal: case["newer_than_terminal"].as_bool().unwrap(),
            },
            "backend_recovering" => GoalAgentObservation::BackendRecovering,
            "unavailable" => GoalAgentObservation::Unavailable,
            "unassigned" => GoalAgentObservation::Unassigned,
            "unknown" => GoalAgentObservation::Unknown,
            _ => unreachable!(),
        };
        let decision = gate_claimed_goal_continuation(
            native_observation,
            settled,
            child_exists,
            &before.state().unwrap(),
        );
        let decision_name = match decision {
            GoalClaimedDecision::Materialize => "materialize",
            GoalClaimedDecision::AwaitReadiness => "await_readiness",
            GoalClaimedDecision::Stop => "stop",
            GoalClaimedDecision::Inactive => "inactive",
        };
        assert_eq!(decision_name, case["decision"].as_str().unwrap(), "{name}");
        if child_exists {
            publish_readiness(&db, "ready", &status, true, true).await;
            assert!(controller
                .build_intent(
                    &identity::Did::new(selected_agent(&db).await).unwrap(),
                    before.clone()
                )
                .await
                .unwrap()
                .is_some());
        }
        publish_readiness(
            &db,
            observation,
            &status,
            settled,
            case["newer_than_terminal"].as_bool().unwrap(),
        )
        .await;
        let observed = load_goal(&db).await;
        let intent = controller
            .build_intent(
                &identity::Did::new(selected_agent(&db).await).unwrap(),
                observed.clone(),
            )
            .await
            .unwrap();
        assert_eq!(
            intent.is_some(),
            decision == GoalClaimedDecision::Materialize,
            "{name}"
        );
        let after = load_goal(&db).await;
        assert_eq!(recovery_projection(&after), case["expected"], "{name}");
        assert_eq!(
            controller
                .continuation_child_exists(
                    &identity::Did::new(selected_agent(&db).await).unwrap(),
                    &after,
                    parent
                )
                .await
                .unwrap(),
            child_exists || intent.is_some(),
            "{name}"
        );
        if decision == GoalClaimedDecision::AwaitReadiness {
            assert_eq!(
                serde_json::to_value(&after).unwrap(),
                serde_json::to_value(&observed).unwrap(),
                "{name}: waiting writes nothing"
            );
            publish_readiness(&db, "ready", &status, true, true).await;
            let recovered = controller
                .build_intent(
                    &identity::Did::new(selected_agent(&db).await).unwrap(),
                    after.clone(),
                )
                .await
                .unwrap()
                .expect("ready claim recovers");
            let retries = case["before"]["infrastructure_retries"].as_i64().unwrap();
            let retry_prompt =
                format!("recovery attempt {retries} of {MAX_INFRASTRUCTURE_RETRIES}");
            assert!(
                recovered.task.prompt_template.contains(&retry_prompt),
                "{name}: {}",
                recovered.task.prompt_template
            );
            assert_eq!(
                recovery_projection(&load_goal(&db).await),
                case["expected"],
                "{name}: recovery preserves retry charge and prompt"
            );
        } else if decision == GoalClaimedDecision::Stop {
            publish_readiness(&db, "ready", &status, true, true).await;
            assert!(
                controller
                    .build_intent(
                        &identity::Did::new(selected_agent(&db).await).unwrap(),
                        after.clone()
                    )
                    .await
                    .unwrap()
                    .is_none(),
                "{name}: readiness cannot resume a stopped claim"
            );
            assert_eq!(
                serde_json::to_value(&load_goal(&db).await).unwrap(),
                serde_json::to_value(&after).unwrap(),
                "{name}: stopped claim stays unchanged"
            );
        }
        let current = load_goal(&db).await;
        assert!(
            controller
                .build_intent(
                    &identity::Did::new(selected_agent(&db).await).unwrap(),
                    current.clone()
                )
                .await
                .unwrap()
                .is_none(),
            "{name}: repeated reconciliation creates no second child"
        );
        assert_eq!(
            child_count(&db, &current, parent).await,
            usize::from(
                child_exists
                    || matches!(
                        decision,
                        GoalClaimedDecision::Materialize | GoalClaimedDecision::AwaitReadiness
                    )
            ),
            "{name}: exactly one child after recovery, none after stop",
        );
        if child_exists {
            assert!(
                !stop_claimed_continuation_for_unavailable_agent_from_access(
                    &agent_access(&db).await,
                    &before,
                    parent,
                    "stale unavailable observation"
                )
                .await
                .unwrap(),
                "{name}: published child fences stale stop"
            );
            assert_eq!(
                serde_json::to_value(&load_goal(&db).await).unwrap(),
                serde_json::to_value(&current).unwrap()
            );
        }
        db.node.shutdown().await.expect("node shutdown");
    }
}

#[tokio::test]
async fn readiness_stop_rejects_a_replaced_claim_epoch() {
    let case = crate::lean_vocab_test::lean_contract_snapshot()
        .goal_claimed_readiness_cases
        .iter()
        .find(|case| case["name"] == "claimed_settled_invalid_pauses")
        .unwrap();
    let db = test_db("claimed-readiness-stale-epoch").await;
    let stale = install_claim(&db, &case["before"]).await;
    let parent = stale.last_continued_from_request_id.as_deref().unwrap();
    assert!(
        claim_continuation_from_access(&agent_access(&db).await, &stale, parent)
            .await
            .unwrap()
    );
    let current = load_goal(&db).await;
    assert!(
        !stop_claimed_continuation_for_unavailable_agent_from_access(
            &agent_access(&db).await,
            &stale,
            parent,
            "stale settled-unavailable observation",
        )
        .await
        .unwrap()
    );
    assert_eq!(
        serde_json::to_value(load_goal(&db).await).unwrap(),
        serde_json::to_value(&current).unwrap()
    );
    db.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn unclaimed_goal_follows_live_reconcile_status_and_modeled_readiness() {
    use crate::request::lifecycle::{
        build_signed_client_request, submit_client_request, ExecutionOrigin, RequestIdentity,
        RequestSigner, RequestSpec,
    };
    use crate::runtime::status::{ReconcilePhase, RuntimeStatusHandle};
    use crate::NodeIdentity;
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};

    let db = test_db("goal-unclaimed-live-readiness").await;
    let fixture = crate::support::managed_agent_fixture(&db).await;
    let agent = fixture.agent.clone();
    let actor = identity::Did::new(agent.did()).unwrap();
    let agent_access =
        ConfigAccess::local_managed_agent(db.node.clone(), actor.clone(), agent.as_ref()).unwrap();
    let request_id = "readiness-parent";
    let spec = RequestSpec::new(
        RequestPurpose::Normal,
        RequestIdentity {
            node_did: db.node_identity.did().into(),
            requester_did: Some(fixture.user.did().into()),
            agent_did: agent.did().into(),
            session_id: SESSION.into(),
            request_id: request_id.into(),
            content: "continue work".into(),
            execution_origin: ExecutionOrigin::Interactive,
            created_at: "2026-07-15T00:00:00Z".into(),
        },
        AgentRequestAdmissionRecord::client(fixture.user.did()),
    );
    let request = build_signed_client_request(
        &fixture.access,
        spec,
        RequestSigner::Identity(&fixture.user),
    )
    .await
    .unwrap();
    submit_client_request(&fixture.access, &request)
        .await
        .unwrap();
    let found = agent_access
        .execute(
            "{AgentRequest(filter: {request_id: {_eq: \"readiness-parent\"}}, limit: 2) {_docID}}",
        )
        .await
        .unwrap();
    let rows = found["data"]["AgentRequest"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    let request_doc = rows[0]["_docID"].as_str().unwrap();
    let session_doc = request.session_doc_id.as_deref().unwrap();
    let changed = agent_access.write("test.goal_readiness_parent", &format!(r#"mutation {{update_AgentRequest(docID: "{}", input: {{lifecycle_state: "completed", terminalized_at: "2026-07-15T00:00:01Z"}}) {{_docID}}}}"#, escape_graphql_string(request_doc))).await.unwrap();
    assert_eq!(
        changed["data"]["update_AgentRequest"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let node_did = db.node_identity.did();
    let agent_did = agent.did();
    agent_access.transact_write("test.goal_readiness_activity", |txn| {
        Box::pin(async move {
            let input = json!({"call_id":"readiness-activity", "node_did":node_did,
                "agent_did":agent_did, "session_id":SESSION, "request_id":request_id,
                "request_doc_id":request_doc, "call_seq":1, "call_kind":"inference",
                "attempt":1, "call_state":"completed"});
            let response = txn.execute_with_variables(
                "mutation($input: InferenceCallMutationInputArg!) {create_InferenceCall(input: $input) {_docID}}",
                &json!({"input":input}),
            ).await?;
            let doc = crate::store::graphql::created_doc_id(&response, "InferenceCall")?;
            txn.add_resource_relationship("InferenceCall", &doc, "parent", "sessions", session_doc).await?;
            Ok::<_, anyhow::Error>(())
        })
    }).await.unwrap();
    let goal = set_goal_from_access(
        &agent_access,
        db.node_identity.did(),
        SESSION,
        Some("Wait for an available Agent"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .unwrap();

    let unavailable = crate::runtime::snapshot::UnavailableAgent::new(
        AgentReadinessUnavailableReason::RuntimeConfigurationInvalid,
        "test fixture identity",
    )
    .with_watcher_identity(agent.clone());
    let snapshot = Arc::new(ActiveRuntimeSnapshot {
        generation: 1,
        node: None,
        local_did: db.node_identity.did().to_owned(),
        default_agent_did: agent.did().to_owned(),
        agents: Default::default(),
        tool_surfaces: Default::default(),
        backend_admission_configs: Default::default(),
        unavailable_agents: [(agent.did().to_owned(), unavailable)]
            .into_iter()
            .collect(),
        active_schedules: Default::default(),
        unavailable_schedules: Default::default(),
        active_event_triggers: Default::default(),
        unavailable_event_triggers: Default::default(),
        active_tasks: Default::default(),
        dispatchers: Default::default(),
        agent_executor_capacities: Default::default(),
        agent_executor_queue_capacities: Default::default(),
    });
    let (owner, status) = RuntimeStatusHandle::start(db.node.clone(), db.node_identity.did());
    status.publish_applied(&snapshot).await;
    assert!(status.goal_reconcile_settled().await);
    let (_tx, rx) = watch::channel(snapshot);
    let controller = GoalSource::new(rx, db.node.clone(), CancellationToken::new())
        .with_runtime_status(status.clone());
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().goal_readiness_gate_cases;
    for (name, process, reason, phase) in [
        (
            "missing_readiness_defers_continuation",
            NodeReadinessProcessState::Recovering,
            None,
            ReconcilePhase::Idle,
        ),
        (
            "backend_recovery_defers_even_when_settled",
            NodeReadinessProcessState::Ready,
            Some(AgentReadinessUnavailableReason::BackendTemporarilyUnavailable),
            ReconcilePhase::Idle,
        ),
        (
            "unsettled_invalid_agent_defers",
            NodeReadinessProcessState::Ready,
            Some(AgentReadinessUnavailableReason::RuntimeConfigurationInvalid),
            ReconcilePhase::Applying,
        ),
        (
            "settled_invalid_agent_stops_active_goal",
            NodeReadinessProcessState::Ready,
            Some(AgentReadinessUnavailableReason::RuntimeConfigurationInvalid),
            ReconcilePhase::Idle,
        ),
    ] {
        let modeled = cases
            .iter()
            .find(|case| case.name == name)
            .expect("modeled readiness case");
        status.set_reconcile_phase(phase).await;
        assert_eq!(
            status.goal_reconcile_settled().await,
            modeled.settled,
            "{name}"
        );
        let readiness = NodeReadinessSnapshot {
            format_version: NODE_READINESS_FORMAT_VERSION,
            process_state: process,
            active_generation: 1,
            router_generation: 1,
            default_agent_did: agent.did().into(),
            agents: vec![AgentReadinessEntry {
                agent_did: agent.did().into(),
                state: if reason.is_some() {
                    AgentReadinessState::Unavailable
                } else {
                    AgentReadinessState::Ready
                },
                reason,
            }],
        };
        crate::runtime::node_readiness_publisher::upsert_node_readiness(
            &db.node,
            db.node_identity.did(),
            &readiness,
            &Utc::now().to_rfc3339(),
        )
        .await
        .unwrap();
        let current = agent_access
            .transact_readonly("test.goal_readiness.current", |txn| {
                Box::pin(crate::goal::load_canonical_goal_in_txn(
                    txn,
                    db.node_identity.did(),
                    SESSION,
                ))
            })
            .await
            .unwrap()
            .unwrap();
        assert!(
            controller
                .build_intent(&actor, current)
                .await
                .unwrap()
                .is_none(),
            "{name}"
        );
        let current = agent_access
            .transact_readonly("test.goal_readiness.after", |txn| {
                Box::pin(crate::goal::load_canonical_goal_in_txn(
                    txn,
                    db.node_identity.did(),
                    SESSION,
                ))
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            current.infrastructure_retry_count.unwrap_or_default(),
            modeled.expected_retries,
            "{name}"
        );
        match modeled.expected_gate.as_str() {
            "await_readiness" => {
                assert_eq!(current.parsed_status(), Some(GoalStatus::Active), "{name}");
                assert!(
                    current
                        .last_failure
                        .as_deref()
                        .is_some_and(|v| v.starts_with(crate::goal::GOAL_READINESS_WAIT_PREFIX)),
                    "{name}"
                );
                assert_eq!(
                    current.continuation_sequence(),
                    goal.continuation_sequence(),
                    "{name}"
                );
                assert!(controller
                    .build_intent(&actor, current.clone())
                    .await
                    .unwrap()
                    .is_none());
                let repeated = agent_access
                    .transact_readonly("test.goal_readiness.repeated", |txn| {
                        Box::pin(crate::goal::load_canonical_goal_in_txn(
                            txn,
                            db.node_identity.did(),
                            SESSION,
                        ))
                    })
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    serde_json::to_value(repeated).unwrap(),
                    serde_json::to_value(current).unwrap(),
                    "{name}: unchanged readiness must not rewrite the Goal"
                );
            }
            "agent_unavailable" => {
                assert_eq!(current.parsed_status(), Some(GoalStatus::Paused), "{name}");
                assert_eq!(
                    current.last_failure.as_deref(),
                    Some(
                        format!(
                            "agent {} is unavailable: {}",
                            agent.did(),
                            AgentReadinessUnavailableReason::RuntimeConfigurationInvalid
                                .public_message()
                        )
                        .as_str()
                    ),
                    "{name}"
                );
            }
            other => panic!("unexpected modeled readiness decision {other}"),
        }
    }
    owner.close().await.unwrap();
    db.node.shutdown().await.unwrap();
}
