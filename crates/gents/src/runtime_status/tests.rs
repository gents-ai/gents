use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::Deserialize;
use tokio::sync::mpsc;

use gents_protocol::row::BehaviorReadinessUnavailableReason;

use super::*;
use crate::ensure_runtime_schemas;
use crate::lean_vocab_test::{
    assert_lean_contract_vocabulary_matches, assert_lean_transition_is_legal,
    assert_lifecycle_transition_cases_partition, assert_state_machine_contract_is_complete,
    lean_process_transition_cases, lean_runtime_reconcile_case, lean_vocabulary_values,
    LeanContractVocabulary, LeanLifecycleTransitionCase,
};

#[derive(Debug, Deserialize)]
struct AgentRuntimeRow {
    reconcile_phase: String,
    behavior_executor_capacity: i64,
    behavior_executor_queue_depth: i64,
    behavior_executor_status_json: String,
    last_reconcile_result: String,
    last_reconcile_error: String,
}

#[derive(Debug, Deserialize)]
struct AgentBehaviorReadinessRow {
    snapshot_json: String,
    updated_at: String,
}

async fn test_node() -> Arc<defra_node::EmbeddedNode> {
    Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap())
}

#[test]
fn agent_runtime_writer_source_cannot_serialize_readiness_authority() {
    let source = include_str!("../runtime_status.rs");
    for forbidden in [
        "row.process_state",
        "row.active_generation",
        "row.router_generation",
        "row.default_behavior_id",
        "process_state: \"{process_state}\"",
        "active_generation: {active_generation}",
        "router_generation: {router_generation}",
        "default_behavior_id: \"{default_behavior_id}\"",
        "runnable_behavior_count",
        "unavailable_behavior_count",
    ] {
        assert!(
            !source.contains(forbidden),
            "AgentRuntime writer regained readiness-owned source fragment {forbidden}"
        );
    }
}

fn status_test_request(request_id: &str) -> crate::watcher::AgentRequest {
    crate::watcher::AgentRequest {
        doc_id: format!("{request_id}-doc"),
        request_id: request_id.to_string(),
        agent_did: "did:test:status-test".to_string(),
        requester_did: None,
        behavior_id: Some("general".to_string()),
        session_id: format!("{request_id}-session"),
        content: "status test".to_string(),
        temperature: None,
        top_p: None,
        top_k: None,
        seed: None,
        max_tokens: None,
        max_total_tokens: None,
        metadata: None,
        execution_origin: Some("interactive".to_string()),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        deadline: None,
        execution_generation: None,
        execution_lease_expires_at: None,
        execution_progress_seq: 0,
        subagent_depth: 0,
        caused_by_parent_request_id: None,
        caused_by_parent_request_doc_id: None,
        caused_by_parent_tool_call_id: None,
        caused_by_parent_tool_call_doc_id: None,
        caused_by_trigger_id: None,
        caused_by_trigger_kind: None,
        caused_by_source_doc_id: None,
        caused_by_correlation: None,
        caused_by_trigger_context: None,
        workspace_id: None,
        workspace_authority: None,
        workspace_owner_deployment_id: None,
        workspace_seal_hash: None,
    }
}

fn unavailable_general() -> HashMap<String, crate::runtime_snapshot::UnavailableBehavior> {
    HashMap::from([(
        "general".to_string(),
        crate::runtime_snapshot::UnavailableBehavior::new(
            BehaviorReadinessUnavailableReason::RuntimeConfigurationInvalid,
            "test runtime is not configured",
        ),
    )])
}

async fn fetch_runtime_row(node: &defra_node::EmbeddedNode, agent_did: &str) -> AgentRuntimeRow {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentRuntime(filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }}, limit: 1) {{
                reconcile_phase
                behavior_executor_capacity
                behavior_executor_queue_depth
                behavior_executor_status_json
                last_reconcile_result
                last_reconcile_error
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "AgentRuntime query failed: {:?}",
        response.errors
    );
    let value = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRuntime"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("AgentRuntime row");
    serde_json::from_value(value).expect("decode AgentRuntime row")
}

async fn fetch_behavior_readiness_row(
    node: &defra_node::EmbeddedNode,
    agent_did: &str,
) -> AgentBehaviorReadinessRow {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let response = node
        .execute(&format!(
            r#"{{
                AgentBehaviorReadiness(
                    filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }},
                    limit: 1
                ) {{
                    snapshot_json
                    updated_at
                }}
            }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "AgentBehaviorReadiness query failed: {:?}",
        response.errors
    );
    let value = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentBehaviorReadiness"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("AgentBehaviorReadiness row");
    serde_json::from_value(value).expect("decode AgentBehaviorReadiness row")
}

#[tokio::test]
async fn restart_publishes_fail_closed_readiness_before_other_runtime_work() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let agent_did = "did:test:readiness-restart";

    let (first_owner, first) = RuntimeStatusHandle::start(node.clone(), agent_did);
    first.initialize_startup("general").await.unwrap();
    first
        .set_process_state_durable(ProcessLifecycleState::Ready)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<gents_protocol::row::BehaviorReadinessSnapshot>(
            &fetch_behavior_readiness_row(node.as_ref(), agent_did)
                .await
                .snapshot_json
        )
        .unwrap()
        .process_state,
        gents_protocol::row::BehaviorReadinessProcessState::Ready
    );
    first_owner.close().await.unwrap();

    let (second_owner, second) = RuntimeStatusHandle::start(node.clone(), agent_did);
    second.initialize_startup("general").await.unwrap();
    let restarted = serde_json::from_str::<gents_protocol::row::BehaviorReadinessSnapshot>(
        &fetch_behavior_readiness_row(node.as_ref(), agent_did)
            .await
            .snapshot_json,
    )
    .unwrap();
    assert_eq!(
        restarted.process_state,
        gents_protocol::row::BehaviorReadinessProcessState::Recovering,
        "a prior durable Ready must be overwritten before restart work can fail"
    );
    second_owner.close().await.unwrap();
}

#[test]
fn rust_process_state_vocabulary_matches_lean_model() {
    let rust_states = vec![
        ProcessLifecycleState::Uninitialized.as_str(),
        ProcessLifecycleState::Recovering.as_str(),
        ProcessLifecycleState::Ready.as_str(),
        ProcessLifecycleState::ShuttingDown.as_str(),
        ProcessLifecycleState::Shutdown.as_str(),
    ];

    assert_lean_contract_vocabulary_matches(LeanContractVocabulary {
        domain: "ProcessState",
        rust_source:
            "ProcessLifecycleState::{Uninitialized, Recovering, Ready, ShuttingDown, Shutdown}",
        rust_values: &rust_states,
    });
}

#[test]
fn rust_process_state_transitions_match_lean_contract() {
    assert_state_machine_contract_is_complete("Process");
    assert_lean_transition_is_legal("Process", "uninitialized", "recovering");
    assert_lean_transition_is_legal("Process", "uninitialized", "ready");
    assert_lean_transition_is_legal("Process", "recovering", "ready");
    assert_lean_transition_is_legal("Process", "ready", "shuttingDown");
    assert_lean_transition_is_legal("Process", "shuttingDown", "shutdown");
    assert_lifecycle_transition_cases_partition(
        "Process",
        &lean_vocabulary_values("ProcessState"),
        lean_process_transition_cases(),
    );
}

fn rust_process_transition_action(from: &str, to: &str) -> Option<&'static str> {
    match (from, to) {
        ("uninitialized", "recovering") => Some("startupRecover"),
        ("uninitialized", "ready") => Some("startupClean"),
        ("recovering", "ready") => Some("recoveryComplete"),
        ("ready", "shuttingDown") => Some("beginShutdown"),
        ("shuttingDown", "shutdown") => Some("finishShutdown"),
        _ => None,
    }
}

fn rust_process_transition_classification(from: &str, to: &str) -> &'static str {
    if rust_process_transition_action(from, to).is_some() {
        "legal"
    } else {
        "illegal"
    }
}

fn process_state_from_contract(state: &str) -> ProcessLifecycleState {
    match state {
        "uninitialized" => ProcessLifecycleState::Uninitialized,
        "recovering" => ProcessLifecycleState::Recovering,
        "ready" => ProcessLifecycleState::Ready,
        "shuttingDown" => ProcessLifecycleState::ShuttingDown,
        "shutdown" => ProcessLifecycleState::Shutdown,
        other => panic!("unknown generated Process state {other:?}"),
    }
}

async fn drive_generated_process_legal_case(case: &LeanLifecycleTransitionCase) {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let agent_did = format!("did:test:process-contract:{}", case.name);
    let (owner, status) = RuntimeStatusHandle::start(node.clone(), agent_did.clone());
    status.readiness().initialize("general").await.unwrap();
    let action = case
        .action
        .as_deref()
        .expect("legal Process transition case must carry an action");

    match action {
        "startupRecover" | "startupClean" => {
            status
                .set_process_state(process_state_from_contract(&case.to))
                .await;
        }
        "recoveryComplete" => {
            status
                .set_process_state(ProcessLifecycleState::Recovering)
                .await;
            status.set_process_state(ProcessLifecycleState::Ready).await;
        }
        "beginShutdown" => {
            status.set_process_state(ProcessLifecycleState::Ready).await;
            status
                .set_process_state(ProcessLifecycleState::ShuttingDown)
                .await;
        }
        "finishShutdown" => {
            status
                .set_process_state(ProcessLifecycleState::ShuttingDown)
                .await;
            status
                .set_process_state(ProcessLifecycleState::Shutdown)
                .await;
        }
        other => panic!(
            "generated Process transition {} has unsupported action {other:?}",
            case.name
        ),
    }

    let readiness = serde_json::from_str::<gents_protocol::row::BehaviorReadinessSnapshot>(
        &fetch_behavior_readiness_row(node.as_ref(), &agent_did)
            .await
            .snapshot_json,
    )
    .expect("decode process readiness");
    assert_eq!(
        readiness.process_state.as_str(), case.to,
        "generated Process transition {} expected {} -> {} classified as {} via {:?}, got authoritative process_state={}",
        case.name, case.from, case.to, case.classification, case.action, readiness.process_state.as_str()
    );
    owner.close().await.unwrap();
}

#[tokio::test]
async fn generated_process_transition_cases_match_runtime_status_policy() {
    let mut legal_count = 0;
    let mut illegal_count = 0;

    for case in lean_process_transition_cases() {
        let rust_classification = rust_process_transition_classification(&case.from, &case.to);
        assert_eq!(
            case.classification, rust_classification,
            "Process transition {} expected classification drift for {} -> {}; Lean action={:?} boundary={:?}",
            case.name, case.from, case.to, case.action, case.boundary
        );

        match case.classification.as_str() {
            "legal" => {
                legal_count += 1;
                assert_eq!(
                    case.action.as_deref(),
                    rust_process_transition_action(&case.from, &case.to),
                    "Process transition {} legal writer action drifted for {} -> {}",
                    case.name,
                    case.from,
                    case.to
                );
                drive_generated_process_legal_case(case).await;
            }
            "illegal" => {
                illegal_count += 1;
                assert!(
                    rust_process_transition_action(&case.from, &case.to).is_none(),
                    "Process transition {} is ordinary illegal but Rust has a writer path for {} -> {}",
                    case.name,
                    case.from,
                    case.to
                );
            }
            "productUnreachable" => {
                panic!(
                    "Process transition {} unexpectedly emitted product-unreachable classification",
                    case.name
                );
            }
            other => panic!(
                "generated Process transition {} has unknown classification {other:?}",
                case.name
            ),
        }
    }

    assert_eq!(legal_count, 5);
    assert_eq!(illegal_count, 20);
}

#[test]
fn rust_reconcile_phase_vocabulary_matches_lean_model() {
    let rust_phases = vec![
        ReconcilePhase::Idle.as_str(),
        ReconcilePhase::Debouncing.as_str(),
        ReconcilePhase::Resolving.as_str(),
        ReconcilePhase::Diffing.as_str(),
        ReconcilePhase::Applying.as_str(),
    ];

    assert_lean_contract_vocabulary_matches(LeanContractVocabulary {
        domain: "ReconcilePhase",
        rust_source: "ReconcilePhase::{Idle, Debouncing, Resolving, Diffing, Applying}",
        rust_values: &rust_phases,
    });
}

#[test]
fn runtime_reconcile_state_machine_contract_is_complete() {
    assert_state_machine_contract_is_complete("RuntimeReconcile");
    assert_lean_transition_is_legal("RuntimeReconcile", "applying", "idle");
}

#[tokio::test]
async fn runtime_status_persists_diagnostics_while_readiness_owns_lifecycle() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let status = RuntimeStatusHandle::new(node.clone(), "did:test:status-test");
    status
        .set_process_state(ProcessLifecycleState::Recovering)
        .await;
    status.set_reconcile_phase(ReconcilePhase::Resolving).await;
    status
        .publish_startup_snapshot(&ActiveRuntimeSnapshot {
            generation: 1,
            principal: None,
            local_did: String::new(),
            default_behavior_id: "code".to_string(),
            behaviors: HashMap::new(),
            tool_surfaces: HashMap::new(),
            backend_admission_configs: HashMap::new(),
            unavailable_behaviors: HashMap::from([(
                "code".to_string(),
                crate::runtime_snapshot::UnavailableBehavior::new(
                    BehaviorReadinessUnavailableReason::BehaviorDisabled,
                    "behavior code is disabled",
                ),
            )]),
            active_schedules: HashMap::new(),
            unavailable_schedules: HashSet::new(),
            active_event_triggers: HashMap::new(),
            unavailable_event_triggers: HashSet::new(),
            active_tasks: HashMap::new(),
            dispatchers: HashMap::new(),
            behavior_executor_capacities: HashMap::new(),
            behavior_executor_queue_capacities: HashMap::new(),
        })
        .await
        .unwrap();
    status.publish_router_generation(1).await.unwrap();
    status.set_process_state(ProcessLifecycleState::Ready).await;

    let row = fetch_runtime_row(node.as_ref(), "did:test:status-test").await;
    assert_eq!(row.reconcile_phase, "idle");
    assert_eq!(row.last_reconcile_result, "startup");
    assert!(row.last_reconcile_error.is_empty());
    let readiness = serde_json::from_str::<gents_protocol::row::BehaviorReadinessSnapshot>(
        &fetch_behavior_readiness_row(node.as_ref(), "did:test:status-test")
            .await
            .snapshot_json,
    )
    .expect("decode authoritative readiness");
    assert_eq!(
        readiness.process_state,
        gents_protocol::row::BehaviorReadinessProcessState::Ready
    );
    assert_eq!(readiness.active_generation, 1);
    assert_eq!(readiness.router_generation, 1);
    assert_eq!(readiness.default_behavior_id, "code");
}

#[tokio::test]
async fn runtime_status_persists_behavior_executor_capacity_and_queue_depth() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let (tx, _rx) = mpsc::channel(4);
    tx.try_send(status_test_request("queued-1")).unwrap();
    tx.try_send(status_test_request("queued-2")).unwrap();

    let status = RuntimeStatusHandle::new(node.clone(), "did:test:executor-status");
    status
        .publish_startup_snapshot(&ActiveRuntimeSnapshot {
            generation: 1,
            principal: None,
            local_did: String::new(),
            default_behavior_id: "general".to_string(),
            behaviors: HashMap::new(),
            tool_surfaces: HashMap::new(),
            backend_admission_configs: HashMap::new(),
            unavailable_behaviors: HashMap::new(),
            active_schedules: HashMap::new(),
            unavailable_schedules: HashSet::new(),
            active_event_triggers: HashMap::new(),
            unavailable_event_triggers: HashSet::new(),
            active_tasks: HashMap::new(),
            dispatchers: HashMap::from([("general".to_string(), tx)]),
            behavior_executor_capacities: HashMap::from([("general".to_string(), 3)]),
            behavior_executor_queue_capacities: HashMap::from([("general".to_string(), 4)]),
        })
        .await
        .unwrap();

    let row = fetch_runtime_row(node.as_ref(), "did:test:executor-status").await;
    assert_eq!(row.behavior_executor_capacity, 3);
    assert_eq!(row.behavior_executor_queue_depth, 2);

    let executor_status: serde_json::Value =
        serde_json::from_str(&row.behavior_executor_status_json).unwrap();
    assert_eq!(
        executor_status,
        serde_json::json!({
            "general": {
                "worker_capacity": 3,
                "queue_depth": 2,
                "queue_capacity": 4
            }
        })
    );
}

#[tokio::test]
async fn executor_metrics_do_not_republish_unchanged_behavior_readiness() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let (tx, _rx) = mpsc::channel(2);
    let snapshot = ActiveRuntimeSnapshot {
        generation: 1,
        principal: None,
        local_did: String::new(),
        default_behavior_id: "general".to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_behaviors: HashMap::new(),
        active_schedules: HashMap::new(),
        unavailable_schedules: HashSet::new(),
        active_event_triggers: HashMap::new(),
        unavailable_event_triggers: HashSet::new(),
        active_tasks: HashMap::new(),
        dispatchers: HashMap::from([("general".to_string(), tx.clone())]),
        behavior_executor_capacities: HashMap::from([("general".to_string(), 1)]),
        behavior_executor_queue_capacities: HashMap::from([("general".to_string(), 2)]),
    };
    let agent_did = "did:test:readiness-metric-owner";
    let status = RuntimeStatusHandle::new(node.clone(), agent_did);
    status.publish_startup_snapshot(&snapshot).await.unwrap();
    let before = fetch_behavior_readiness_row(node.as_ref(), agent_did).await;

    tx.try_send(status_test_request("metric-only-change"))
        .unwrap();
    status.publish_executor_snapshot(&snapshot).await;
    let after = fetch_behavior_readiness_row(node.as_ref(), agent_did).await;

    assert_eq!(after.snapshot_json, before.snapshot_json);
    assert_eq!(
        after.updated_at, before.updated_at,
        "executor polling must not become an incidental readiness writer"
    );
}

#[tokio::test]
async fn runtime_status_serializes_persisted_generation_updates() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let status = RuntimeStatusHandle::new(node.clone(), "did:test:status-serialize");
    let startup = ActiveRuntimeSnapshot {
        generation: 1,
        principal: None,
        local_did: String::new(),
        default_behavior_id: "general".to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_behaviors: unavailable_general(),
        active_schedules: HashMap::new(),
        unavailable_schedules: HashSet::new(),
        active_event_triggers: HashMap::new(),
        unavailable_event_triggers: HashSet::new(),
        active_tasks: HashMap::new(),
        dispatchers: HashMap::new(),
        behavior_executor_capacities: HashMap::new(),
        behavior_executor_queue_capacities: HashMap::new(),
    };
    let applied = ActiveRuntimeSnapshot {
        generation: 2,
        principal: None,
        local_did: String::new(),
        default_behavior_id: "general".to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_behaviors: unavailable_general(),
        active_schedules: HashMap::new(),
        unavailable_schedules: HashSet::new(),
        active_event_triggers: HashMap::new(),
        unavailable_event_triggers: HashSet::new(),
        active_tasks: HashMap::new(),
        dispatchers: HashMap::new(),
        behavior_executor_capacities: HashMap::new(),
        behavior_executor_queue_capacities: HashMap::new(),
    };

    status.publish_startup_snapshot(&startup).await.unwrap();
    let status_for_snapshot = status.clone();
    let status_for_router = status.clone();
    let publish_snapshot = tokio::spawn(async move {
        status_for_snapshot.publish_applied(&applied).await;
    });
    let publish_router = tokio::spawn(async move {
        status_for_router
            .publish_router_generation(2)
            .await
            .unwrap();
    });
    publish_snapshot.await.unwrap();
    publish_router.await.unwrap();

    let row = fetch_runtime_row(node.as_ref(), "did:test:status-serialize").await;
    assert_eq!(row.last_reconcile_result, "applied");
    let readiness = fetch_behavior_readiness_row(node.as_ref(), "did:test:status-serialize").await;
    let readiness: gents_protocol::row::BehaviorReadinessSnapshot =
        serde_json::from_str(&readiness.snapshot_json).expect("decode serialized readiness");
    assert_eq!(readiness.active_generation, 2);
    assert_eq!(readiness.router_generation, 2);
}

#[tokio::test]
async fn runtime_status_generation_updates_match_lean_runtime_reconcile_cases() {
    let publish = lean_runtime_reconcile_case("publish_changed_snapshot");
    let router = lean_runtime_reconcile_case("router_observe_published_generation");
    assert!(publish.legal);
    assert!(router.legal);

    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let status = RuntimeStatusHandle::new(node.clone(), "did:test:runtime-contract");
    let startup = ActiveRuntimeSnapshot {
        generation: publish.pre_active_generation as u64,
        principal: None,
        local_did: String::new(),
        default_behavior_id: "general".to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_behaviors: unavailable_general(),
        active_schedules: HashMap::new(),
        unavailable_schedules: HashSet::new(),
        active_event_triggers: HashMap::new(),
        unavailable_event_triggers: HashSet::new(),
        active_tasks: HashMap::new(),
        dispatchers: HashMap::new(),
        behavior_executor_capacities: HashMap::new(),
        behavior_executor_queue_capacities: HashMap::new(),
    };
    let applied = ActiveRuntimeSnapshot {
        generation: publish.post_active_generation as u64,
        principal: None,
        local_did: String::new(),
        default_behavior_id: "general".to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_behaviors: unavailable_general(),
        active_schedules: HashMap::new(),
        unavailable_schedules: HashSet::new(),
        active_event_triggers: HashMap::new(),
        unavailable_event_triggers: HashSet::new(),
        active_tasks: HashMap::new(),
        dispatchers: HashMap::new(),
        behavior_executor_capacities: HashMap::new(),
        behavior_executor_queue_capacities: HashMap::new(),
    };

    status.publish_startup_snapshot(&startup).await.unwrap();
    status
        .publish_router_generation(publish.pre_router_generation as u64)
        .await
        .unwrap();
    status.set_reconcile_phase(ReconcilePhase::Applying).await;
    status.publish_applied(&applied).await;
    let row = fetch_runtime_row(node.as_ref(), "did:test:runtime-contract").await;
    assert_eq!(row.reconcile_phase, publish.post_phase.as_str());
    assert_eq!(row.last_reconcile_result, "applied");
    let readiness = serde_json::from_str::<gents_protocol::row::BehaviorReadinessSnapshot>(
        &fetch_behavior_readiness_row(node.as_ref(), "did:test:runtime-contract")
            .await
            .snapshot_json,
    )
    .expect("decode published runtime contract readiness");
    assert_eq!(
        readiness.active_generation,
        publish.post_active_generation as u64
    );
    assert_eq!(
        readiness.router_generation,
        publish.post_router_generation as u64
    );

    status
        .publish_router_generation(router.post_router_generation as u64)
        .await
        .unwrap();
    let row = fetch_runtime_row(node.as_ref(), "did:test:runtime-contract").await;
    assert_eq!(row.reconcile_phase, router.post_phase.as_str());
    let readiness = serde_json::from_str::<gents_protocol::row::BehaviorReadinessSnapshot>(
        &fetch_behavior_readiness_row(node.as_ref(), "did:test:runtime-contract")
            .await
            .snapshot_json,
    )
    .expect("decode router-observed readiness");
    assert_eq!(
        readiness.active_generation,
        router.post_active_generation as u64
    );
    assert_eq!(
        readiness.router_generation,
        router.post_router_generation as u64
    );
}
