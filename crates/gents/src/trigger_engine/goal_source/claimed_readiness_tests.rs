use super::*;
use crate::goal::{gate_claimed_goal_continuation, set_goal, GoalBehaviorObservation};
use crate::support::{test_db, TestDb};
use gents_protocol::row::{
    BehaviorReadinessEntry, BehaviorReadinessProcessState, BehaviorReadinessSnapshot,
    BehaviorReadinessState, BehaviorReadinessUnavailableReason, BEHAVIOR_READINESS_FORMAT_VERSION,
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

fn source(db: &TestDb) -> GoalSource {
    let (_, rx) = watch::channel(Arc::new(ActiveRuntimeSnapshot {
        generation: 1,
        principal: None,
        local_did: db.node_identity.did().to_owned(),
        default_behavior_id: BEHAVIOR.to_owned(),
        behaviors: Default::default(),
        tool_surfaces: Default::default(),
        backend_admission_configs: Default::default(),
        unavailable_behaviors: Default::default(),
        active_schedules: Default::default(),
        unavailable_schedules: Default::default(),
        active_event_triggers: Default::default(),
        unavailable_event_triggers: Default::default(),
        active_tasks: Default::default(),
        dispatchers: Default::default(),
        behavior_executor_capacities: Default::default(),
        behavior_executor_queue_capacities: Default::default(),
    }));
    GoalSource::new(rx, db.node.clone(), CancellationToken::new())
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
        &format!(r#"{{ AgentRequest(filter: {{agent_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, caused_by_trigger_kind: {{_eq: "goal"}}, caused_by_trigger_id: {{_eq: "{}"}}, caused_by_parent_request_id: {{_eq: "{}"}}}}) {{_docID}} }}"#,
            escape_graphql_string(&goal.agent_did), escape_graphql_string(&goal.session_id),
            escape_graphql_string(&goal.goal_id), escape_graphql_string(parent)),
        "count claimed readiness children",
    ).await.unwrap();
    response.data.unwrap()["AgentRequest"]
        .as_array()
        .unwrap()
        .len()
}

async fn install_claim(db: &TestDb, before: &Value) -> GoalDocument {
    crate::test_support::install_test_behavior(&db.node, db.node_identity.did(), BEHAVIOR).await;
    crate::session::ensure_session_with_behavior_id_and_requester_did(
        &db.node,
        SESSION,
        BEHAVIOR,
        db.node_identity.did(),
        BEHAVIOR,
        Some(db.node_identity.did()),
    )
    .await
    .unwrap();
    let parent = before["parent"].as_str().unwrap();
    let spec = crate::RequestSpec::new(
        gents_protocol::request_admission::RequestPurpose::Normal,
        crate::lifecycle::RequestIdentity {
            requester_did: Some(db.node_identity.did().to_owned()),
            request_id: parent.to_owned(),
            agent_did: db.node_identity.did().to_owned(),
            behavior_id: BEHAVIOR.to_owned(),
            session_id: SESSION.to_owned(),
            content: "original goal attempt".to_owned(),
            execution_origin: crate::lifecycle::ExecutionOrigin::Interactive,
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
        &format!(r#"mutation {{ update_AgentRequest(filter: {{request_id: {{_eq: "{}"}}, lifecycle_state: {{_eq: "pending"}}}}, input: {{lifecycle_state: "failed", terminalized_at: "2020-01-02T00:00:00Z"}}) {{_docID}} }}"#,
            escape_graphql_string(parent)),
    ).await.unwrap();
    let goal = set_goal(
        &db.node,
        db.node_identity.did(),
        SESSION,
        Some("Recover the saved attempt"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .unwrap();
    assert!(claim_retry_continuation(
        &db.node,
        &goal,
        parent,
        before["infrastructure_retries"].as_i64().unwrap(),
        before["last_failure"].as_str().unwrap(),
    )
    .await
    .unwrap());
    let claimed = load_goal(db).await;
    assert!(update_goal_fields_if_status(&db.node, &claimed, GoalStatus::Active, &format!(
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
    settled: bool,
    newer_than_terminal: bool,
) {
    let unavailable = match observation {
        "unavailable" => Some(BehaviorReadinessUnavailableReason::RuntimeConfigurationInvalid),
        "backend_recovering" => {
            Some(BehaviorReadinessUnavailableReason::BackendTemporarilyUnavailable)
        }
        _ => None,
    };
    let snapshot = BehaviorReadinessSnapshot {
        format_version: BEHAVIOR_READINESS_FORMAT_VERSION,
        process_state: if observation == "unknown" {
            BehaviorReadinessProcessState::Recovering
        } else {
            BehaviorReadinessProcessState::Ready
        },
        active_generation: 1,
        router_generation: 1,
        default_behavior_id: if observation == "unassigned" {
            "other-assigned-behavior".to_owned()
        } else {
            BEHAVIOR.to_owned()
        },
        behaviors: if observation == "unassigned" {
            vec![BehaviorReadinessEntry {
                behavior_id: "other-assigned-behavior".to_owned(),
                state: BehaviorReadinessState::Ready,
                reason: None,
            }]
        } else {
            vec![BehaviorReadinessEntry {
                behavior_id: BEHAVIOR.to_owned(),
                state: if unavailable.is_some() {
                    BehaviorReadinessState::Unavailable
                } else {
                    BehaviorReadinessState::Ready
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
            r#"mutation {{ upsert_AgentBehaviorReadiness(filter: {{agent_did: {{_eq: "{did}"}}}},
            add: {{agent_did: "{did}", snapshot_json: "{payload}", updated_at: "{at}"}},
            update: {{snapshot_json: "{payload}", updated_at: "{at}"}}) {{_docID}} }}"#,
        ),
    )
    .await
    .unwrap();
    let phase = if settled { "idle" } else { "applying" };
    crate::ConfigAccess::write_local(&db.node, "goal.claimed_readiness_test_reconcile", &format!(
        r#"mutation {{ upsert_AgentRuntime(filter: {{agent_did: {{_eq: "{did}"}}}},
            add: {{agent_did: "{did}", reconcile_phase: "{phase}", last_reconcile_result: "applied"}},
            update: {{reconcile_phase: "{phase}", last_reconcile_result: "applied"}}) {{_docID}} }}"#,
    )).await.unwrap();
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
        let controller = source(&db);
        let observation = case["observation"].as_str().unwrap();
        let settled = case["settled"].as_bool().unwrap();
        let child_exists = case["child_exists"].as_bool().unwrap();
        let native_observation = match observation {
            "ready" => GoalBehaviorObservation::Ready {
                newer_than_terminal: case["newer_than_terminal"].as_bool().unwrap(),
            },
            "backend_recovering" => GoalBehaviorObservation::BackendRecovering,
            "unavailable" => GoalBehaviorObservation::Unavailable,
            "unassigned" => GoalBehaviorObservation::Unassigned,
            "unknown" => GoalBehaviorObservation::Unknown,
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
            publish_readiness(&db, "ready", true, true).await;
            assert!(controller
                .build_intent(before.clone())
                .await
                .unwrap()
                .is_some());
        }
        publish_readiness(
            &db,
            observation,
            settled,
            case["newer_than_terminal"].as_bool().unwrap(),
        )
        .await;
        let observed = load_goal(&db).await;
        let intent = controller.build_intent(observed.clone()).await.unwrap();
        assert_eq!(
            intent.is_some(),
            decision == GoalClaimedDecision::Materialize,
            "{name}"
        );
        let after = load_goal(&db).await;
        assert_eq!(recovery_projection(&after), case["expected"], "{name}");
        assert_eq!(
            controller
                .continuation_child_exists(&after, parent)
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
            publish_readiness(&db, "ready", true, true).await;
            let recovered = controller
                .build_intent(after.clone())
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
            publish_readiness(&db, "ready", true, true).await;
            assert!(
                controller
                    .build_intent(after.clone())
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
                .build_intent(current.clone())
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
                !stop_claimed_continuation_for_unavailable_behavior(
                    &db.node,
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
        db.node.shutdown().await;
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
    assert!(claim_continuation(&db.node, &stale, parent).await.unwrap());
    let current = load_goal(&db).await;
    assert!(!stop_claimed_continuation_for_unavailable_behavior(
        &db.node,
        &stale,
        parent,
        "stale settled-unavailable observation",
    )
    .await
    .unwrap());
    assert_eq!(
        serde_json::to_value(load_goal(&db).await).unwrap(),
        serde_json::to_value(&current).unwrap()
    );
    db.node.shutdown().await;
}
