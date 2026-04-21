use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::Deserialize;

use super::*;
use crate::ensure_runtime_schemas;

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
        default_behavior_id: "general".to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_behaviors: HashMap::new(),
        active_schedules: HashMap::new(),
        unavailable_schedules: HashSet::new(),
        active_event_triggers: HashMap::new(),
        dispatchers: HashMap::new(),
    };
    let applied = ActiveRuntimeSnapshot {
        generation: 2,
        default_behavior_id: "general".to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_behaviors: HashMap::new(),
        active_schedules: HashMap::new(),
        unavailable_schedules: HashSet::new(),
        active_event_triggers: HashMap::new(),
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
