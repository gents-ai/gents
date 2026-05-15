use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::Deserialize;

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
    process_state: String,
    reconcile_phase: String,
    active_generation: i64,
    router_generation: i64,
    default_behavior_id: String,
    runnable_behavior_count: i64,
    unavailable_behavior_count: i64,
    last_reconcile_result: String,
    last_reconcile_error: String,
}

async fn test_node() -> Arc<defra_node::EmbeddedNode> {
    Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap())
}

async fn fetch_runtime_row(node: &defra_node::EmbeddedNode, agent_did: &str) -> AgentRuntimeRow {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentRuntime(filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }}, limit: 1) {{
                process_state
                reconcile_phase
                active_generation
                router_generation
                default_behavior_id
                runnable_behavior_count
                unavailable_behavior_count
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
    let agent_did = format!("did:defra-agent:process-contract:{}", case.name);
    let status = RuntimeStatusHandle::new(node.clone(), agent_did.clone());
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

    let row = fetch_runtime_row(node.as_ref(), &agent_did).await;
    assert_eq!(
        row.process_state, case.to,
        "generated Process transition {} expected {} -> {} classified as {} via {:?}, got persisted process_state={}",
        case.name, case.from, case.to, case.classification, case.action, row.process_state
    );
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
    assert_state_machine_contract_is_complete("RuntimeReconcile");
    assert_lean_transition_is_legal("RuntimeReconcile", "applying", "idle");
}

#[tokio::test]
async fn runtime_status_persists_process_and_reconcile_state() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let status = RuntimeStatusHandle::new(node.clone(), "did:defra-agent:status-test");
    status
        .set_process_state(ProcessLifecycleState::Recovering)
        .await;
    status.set_reconcile_phase(ReconcilePhase::Resolving).await;
    status
        .publish_startup_snapshot(&ActiveRuntimeSnapshot {
            generation: 1,
            local_did: String::new(),
            paired_peer_dids: HashSet::new(),
            default_behavior_id: "general".to_string(),
            behaviors: HashMap::new(),
            tool_surfaces: HashMap::new(),
            backend_admission_configs: HashMap::new(),
            unavailable_behaviors: HashMap::from([(
                "code".to_string(),
                "behavior code is disabled".to_string(),
            )]),
            active_schedules: HashMap::new(),
            unavailable_schedules: HashSet::new(),
            active_event_triggers: HashMap::new(),
            unavailable_event_triggers: HashSet::new(),
            active_tasks: HashMap::new(),
            dispatchers: HashMap::new(),
        })
        .await;
    status.publish_router_generation(1).await;
    status.set_process_state(ProcessLifecycleState::Ready).await;

    let row = fetch_runtime_row(node.as_ref(), "did:defra-agent:status-test").await;
    assert_eq!(row.process_state, "ready");
    assert_eq!(row.reconcile_phase, "idle");
    assert_eq!(row.active_generation, 1);
    assert_eq!(row.router_generation, 1);
    assert_eq!(row.default_behavior_id, "general");
    assert_eq!(row.runnable_behavior_count, 0);
    assert_eq!(row.unavailable_behavior_count, 1);
    assert_eq!(row.last_reconcile_result, "startup");
    assert!(row.last_reconcile_error.is_empty());
}

#[tokio::test]
async fn runtime_status_serializes_persisted_generation_updates() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let status = RuntimeStatusHandle::new(node.clone(), "did:defra-agent:status-serialize");
    let startup = ActiveRuntimeSnapshot {
        generation: 1,
        local_did: String::new(),
        paired_peer_dids: HashSet::new(),
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
        dispatchers: HashMap::new(),
    };
    let applied = ActiveRuntimeSnapshot {
        generation: 2,
        local_did: String::new(),
        paired_peer_dids: HashSet::new(),
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
        dispatchers: HashMap::new(),
    };

    status.publish_startup_snapshot(&startup).await;
    let status_for_snapshot = status.clone();
    let status_for_router = status.clone();
    let publish_snapshot = tokio::spawn(async move {
        status_for_snapshot.publish_applied(&applied).await;
    });
    let publish_router = tokio::spawn(async move {
        status_for_router.publish_router_generation(2).await;
    });
    publish_snapshot.await.unwrap();
    publish_router.await.unwrap();

    let row = fetch_runtime_row(node.as_ref(), "did:defra-agent:status-serialize").await;
    assert_eq!(row.active_generation, 2);
    assert_eq!(row.router_generation, 2);
    assert_eq!(row.last_reconcile_result, "applied");
}

#[tokio::test]
async fn runtime_status_generation_updates_match_lean_runtime_reconcile_cases() {
    let publish = lean_runtime_reconcile_case("publish_changed_snapshot");
    let router = lean_runtime_reconcile_case("router_observe_published_generation");
    assert!(publish.legal);
    assert!(router.legal);

    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let status = RuntimeStatusHandle::new(node.clone(), "did:defra-agent:runtime-contract");
    let startup = ActiveRuntimeSnapshot {
        generation: publish.pre_active_generation as u64,
        local_did: String::new(),
        paired_peer_dids: HashSet::new(),
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
        dispatchers: HashMap::new(),
    };
    let applied = ActiveRuntimeSnapshot {
        generation: publish.post_active_generation as u64,
        local_did: String::new(),
        paired_peer_dids: HashSet::new(),
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
        dispatchers: HashMap::new(),
    };

    status.publish_startup_snapshot(&startup).await;
    status.set_reconcile_phase(ReconcilePhase::Applying).await;
    status.publish_applied(&applied).await;
    let row = fetch_runtime_row(node.as_ref(), "did:defra-agent:runtime-contract").await;
    assert_eq!(row.reconcile_phase, publish.post_phase.as_str());
    assert_eq!(row.active_generation, publish.post_active_generation as i64);
    assert_eq!(row.router_generation, publish.post_router_generation as i64);
    assert_eq!(row.last_reconcile_result, "applied");

    status
        .publish_router_generation(router.post_router_generation as u64)
        .await;
    let row = fetch_runtime_row(node.as_ref(), "did:defra-agent:runtime-contract").await;
    assert_eq!(row.reconcile_phase, router.post_phase.as_str());
    assert_eq!(row.active_generation, router.post_active_generation as i64);
    assert_eq!(row.router_generation, router.post_router_generation as i64);
}
