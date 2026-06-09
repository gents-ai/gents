use std::sync::Arc;
use std::time::Duration;

use defra_agent::{
    ensure_agent_principal, load_agent_behavior, upsert_agent_behavior, AgentIdentity, DefraAgent,
    DocumentRuntimeOptions, KeyIdentity, ToolCeiling,
};

mod support;

use support::mock_endpoint::MockModelEndpoint;
use support::snapshots::{fetch_runtime_snapshot, RuntimeSnapshot};
use support::test_db;

fn test_identity(name: &str) -> KeyIdentity {
    let path = std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4()));
    KeyIdentity::load_or_create(path, None).unwrap()
}

async fn bind_default_behavior_backend(
    node: &defra_agent::defra_node::EmbeddedNode,
    agent_did: &str,
    backend_id: &str,
    endpoint: &str,
) {
    let bootstrap = ensure_agent_principal(node, agent_did).await.unwrap();
    let escaped_backend_id = defra_agent::graphql::escape_graphql_string(backend_id);
    let escaped_endpoint = defra_agent::graphql::escape_graphql_string(endpoint);
    let mutation = format!(
        r#"mutation {{
            upsert_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{escaped_backend_id}" }} }},
                add: {{
                    backend_id: "{escaped_backend_id}",
                    name: "{escaped_backend_id}",
                    endpoint: "{escaped_endpoint}",
                    max_concurrent: 1,
                    enabled: true,
                    models: ["default"],
                    probe_status: "healthy"
                }},
                update: {{
                    name: "{escaped_backend_id}",
                    endpoint: "{escaped_endpoint}",
                    max_concurrent: 1,
                    enabled: true,
                    probe_status: "healthy"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "upsert InferenceBackend failed: {:?}",
        response.errors
    );

    let mut default_behavior = load_agent_behavior(node, &bootstrap.default_behavior.behavior_id)
        .await
        .unwrap()
        .expect("default behavior document");
    default_behavior.backend_id = Some(backend_id.to_string());
    upsert_agent_behavior(node, &default_behavior)
        .await
        .unwrap();
}

async fn wait_for_runtime_snapshot<F>(
    node: &defra_agent::defra_node::EmbeddedNode,
    agent_did: &str,
    predicate: F,
) -> RuntimeSnapshot
where
    F: Fn(&RuntimeSnapshot) -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(snapshot) = fetch_runtime_snapshot(node, agent_did).await {
            if predicate(&snapshot) {
                return snapshot;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for runtime snapshot for {agent_did}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_status_surfaces_startup_reconcile_and_shutdown() {
    let db = test_db("runtime-observability").await;
    let identity = Arc::new(test_identity("runtime-observability"));
    let mock_endpoint = MockModelEndpoint::start("default").unwrap();
    bind_default_behavior_backend(
        db.node.as_ref(),
        identity.did(),
        "backend-runtime-observability",
        mock_endpoint.endpoint(),
    )
    .await;
    let agent = DefraAgent::from_default_behavior_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let agent_did = agent.agent_did().to_string();
    let default_behavior_id = agent.default_behavior_id().to_string();

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));

    let startup = wait_for_runtime_snapshot(db.node.as_ref(), &agent_did, |snapshot| {
        snapshot.process_state == "ready"
            && snapshot.reconcile_phase == "idle"
            && snapshot.active_generation == 1
            && snapshot.router_generation == 1
            && snapshot.last_reconcile_result == "startup"
    })
    .await;
    assert_eq!(startup.default_behavior_id, default_behavior_id);
    assert!(startup.last_reconcile_error.is_empty());

    let mut behavior = load_agent_behavior(db.node.as_ref(), &default_behavior_id)
        .await
        .unwrap()
        .expect("default behavior document");
    behavior.system_prompt = Some("runtime observability update".to_string());
    upsert_agent_behavior(db.node.as_ref(), &behavior)
        .await
        .unwrap();

    let reconciled = wait_for_runtime_snapshot(db.node.as_ref(), &agent_did, |snapshot| {
        snapshot.process_state == "ready"
            && snapshot.reconcile_phase == "idle"
            && snapshot.active_generation == 2
            && snapshot.router_generation == 2
            && snapshot.last_reconcile_result == "applied"
    })
    .await;
    assert_eq!(reconciled.default_behavior_id, default_behavior_id);
    assert!(reconciled.last_reconcile_error.is_empty());

    let _ = shutdown_tx.send(true);
    handle.await.unwrap().unwrap();

    let shutdown = wait_for_runtime_snapshot(db.node.as_ref(), &agent_did, |snapshot| {
        snapshot.process_state == "shutdown"
    })
    .await;
    assert_eq!(shutdown.reconcile_phase, "idle");
    assert_eq!(shutdown.router_generation, 2);
}
