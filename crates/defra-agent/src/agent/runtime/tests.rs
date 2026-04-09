use super::*;
use crate::document_config::ToolSelectionDocument;
use crate::ensure_runtime_schemas;
use crate::graphql::escape_graphql_string;
use crate::identity::{AgentIdentity, SimpleIdentity};
use crate::runtime_status::RuntimeStatusHandle;
use crate::tool_surface::ToolCeiling;
use crate::watcher::AgentRequest;
use serde_json::Value;
use std::collections::HashMap;

async fn test_node() -> Arc<defra_node::EmbeddedNode> {
    Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap())
}

fn test_identity(name: &str) -> SimpleIdentity {
    let path = std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4()));
    SimpleIdentity::new(name, path, None)
}

fn request(behavior_id: Option<&str>, session_id: &str) -> AgentRequest {
    AgentRequest {
        doc_id: "doc-1".to_string(),
        request_id: "req-1".to_string(),
        agent_did: "did:defra-agent:test".to_string(),
        behavior_id: behavior_id.map(ToOwned::to_owned),
        session_id: session_id.to_string(),
        content: "hello".to_string(),
        created_at: "2026-04-09T00:00:00Z".to_string(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct RuntimeStatusRow {
    reconcile_phase: String,
    active_generation: i64,
    last_reconcile_result: String,
    last_reconcile_error: String,
}

async fn fetch_runtime_status(
    node: &defra_node::EmbeddedNode,
    agent_did: &str,
) -> RuntimeStatusRow {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentRuntime(filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }}, limit: 1) {{
                reconcile_phase
                active_generation
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

async fn bind_default_behavior_backend(
    node: &defra_node::EmbeddedNode,
    agent_did: &str,
    backend_id: &str,
    endpoint: &str,
) {
    let bootstrap = crate::ensure_agent_principal(node, agent_did)
        .await
        .unwrap();
    let escaped_backend_id = escape_graphql_string(backend_id);
    let escaped_endpoint = escape_graphql_string(endpoint);
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
                    last_probe: "2026-04-09T00:00:00Z",
                    probe_status: "healthy"
                }},
                update: {{
                    name: "{escaped_backend_id}",
                    endpoint: "{escaped_endpoint}",
                    max_concurrent: 1,
                    enabled: true,
                    last_probe: "2026-04-09T00:00:00Z",
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

    let mut default_behavior =
        crate::load_agent_behavior(node, &bootstrap.default_behavior.behavior_id)
            .await
            .unwrap()
            .expect("default behavior document");
    default_behavior.backend_id = Some(backend_id.to_string());
    crate::upsert_agent_behavior(node, &default_behavior)
        .await
        .unwrap();
}

struct ScriptedWatcher {
    rx: mpsc::Receiver<anyhow::Result<AgentRequest>>,
}

impl Watcher for ScriptedWatcher {
    async fn next_request(&mut self) -> Option<anyhow::Result<AgentRequest>> {
        self.rx.recv().await
    }
}

#[tokio::test]
async fn resolve_behavior_uses_default_when_session_is_unbound() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let resolved =
        resolve_behavior_for_request(node.as_ref(), &request(None, "session-default"), "general")
            .await
            .unwrap();

    assert_eq!(resolved.behavior_id, "general");
    assert!(resolved.rejection_reason.is_none());
}

#[tokio::test]
async fn resolve_behavior_prefers_existing_session_binding() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    crate::session::create_session_with_behavior_id(
        node.as_ref(),
        "session-bound",
        "general",
        "code",
    )
    .await
    .unwrap();

    let resolved =
        resolve_behavior_for_request(node.as_ref(), &request(None, "session-bound"), "general")
            .await
            .unwrap();

    assert_eq!(resolved.behavior_id, "code");
    assert!(resolved.rejection_reason.is_none());
}

#[tokio::test]
async fn resolve_behavior_rejects_session_switches() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    crate::session::create_session_with_behavior_id(
        node.as_ref(),
        "session-pinned",
        "general",
        "general",
    )
    .await
    .unwrap();

    let resolved = resolve_behavior_for_request(
        node.as_ref(),
        &request(Some("code"), "session-pinned"),
        "general",
    )
    .await
    .unwrap();

    assert_eq!(resolved.behavior_id, "code");
    assert_eq!(
        resolved.rejection_reason.as_deref(),
        Some("session session-pinned is pinned to behavior general and cannot switch to code")
    );
}

#[tokio::test]
async fn router_dispatches_first_request_after_snapshot_change_to_latest_generation() {
    let agent_did = "did:defra-agent:router-latest-snapshot";
    let initial_snapshot = Arc::new(crate::runtime_snapshot::ActiveRuntimeSnapshot {
        generation: 1,
        default_behavior_id: "general".to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        unavailable_behaviors: HashMap::new(),
        dispatchers: HashMap::new(),
    });
    let updated_snapshot = Arc::new(crate::runtime_snapshot::ActiveRuntimeSnapshot {
        generation: 2,
        default_behavior_id: "code".to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        unavailable_behaviors: HashMap::new(),
        dispatchers: HashMap::new(),
    });
    let (active_tx, mut active_rx) = watch::channel(initial_snapshot);
    let (_shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let (watcher_tx, watcher_rx) = mpsc::channel(1);
    let mut watcher = ScriptedWatcher { rx: watcher_rx };
    let mut active_snapshot = active_rx.borrow().clone();

    active_tx.send(updated_snapshot).unwrap();
    watcher_tx
        .send(Ok(AgentRequest {
            doc_id: "doc-router".to_string(),
            request_id: "req-router".to_string(),
            agent_did: agent_did.to_string(),
            behavior_id: None,
            session_id: "session-router".to_string(),
            content: "hello".to_string(),
            created_at: "2026-04-09T00:00:00Z".to_string(),
        }))
        .await
        .unwrap();
    let request = wait_for_next_request_with_latest_snapshot(
        agent_did,
        &mut watcher,
        &mut active_snapshot,
        &mut active_rx,
        &mut shutdown_rx,
    )
    .await
    .expect("router wait should succeed")
    .expect("request should be returned");

    assert_eq!(request.request_id, "req-router");
    assert_eq!(active_snapshot.generation, 2);
    assert_eq!(active_snapshot.default_behavior_id, "code");
}

#[tokio::test(start_paused = true)]
async fn router_publishes_observed_generation_without_waiting_for_request() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let agent_did = "did:defra-agent:router-observed-generation";
    let initial_snapshot = Arc::new(crate::runtime_snapshot::ActiveRuntimeSnapshot {
        generation: 1,
        default_behavior_id: "general".to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        unavailable_behaviors: HashMap::new(),
        dispatchers: HashMap::new(),
    });
    let updated_snapshot = Arc::new(crate::runtime_snapshot::ActiveRuntimeSnapshot {
        generation: 2,
        default_behavior_id: "general".to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        unavailable_behaviors: HashMap::new(),
        dispatchers: HashMap::new(),
    });
    let (active_tx, active_rx) = watch::channel(initial_snapshot.clone());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let runtime_status = RuntimeStatusHandle::new(node.clone(), agent_did.to_string());
    runtime_status
        .publish_startup_snapshot(initial_snapshot.as_ref())
        .await;

    let observer_task = tokio::spawn(run_router_generation_observer(
        active_rx,
        runtime_status.clone(),
        shutdown_rx,
    ));

    tokio::task::yield_now().await;
    active_tx.send(updated_snapshot).unwrap();
    tokio::task::yield_now().await;

    let row = fetch_runtime_status(node.as_ref(), agent_did).await;
    assert_eq!(row.active_generation, 1);
    assert_eq!(row.last_reconcile_result, "startup");

    let query = format!(
        r#"{{
            AgentRuntime(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}, limit: 1) {{
                router_generation
            }}
        }}"#,
        agent_did = escape_graphql_string(agent_did),
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        let response = node.execute(&query).await;
        assert!(
            !response.has_errors(),
            "AgentRuntime router query failed: {:?}",
            response.errors
        );
        let router_generation = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRuntime"))
            .and_then(|rows| rows.as_array())
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("router_generation"))
            .and_then(Value::as_i64)
            .unwrap_or_default();
        if router_generation == 2 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "router generation did not advance to 2; last value={router_generation}"
        );
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(10)).await;
    }

    let _ = shutdown_tx.send(true);
    observer_task.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn control_watcher_publishes_reconciled_snapshot_after_relevant_update() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("control-watcher"));
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-control",
        "http://127.0.0.1:8111/v1",
    )
    .await;
    let agent = crate::DefraAgent::from_default_behavior_documents(
        node.clone(),
        identity,
        crate::agent::DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let resolve_context = agent
        .document_runtime_context()
        .cloned()
        .expect("document-backed agent");
    let runtime_status = RuntimeStatusHandle::new(node.clone(), agent.agent_did().to_string());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposal_tx, mut proposal_rx) = mpsc::channel(4);

    let watcher_task = tokio::spawn(run_control_watcher(
        node.clone(),
        agent.agent_did().to_string(),
        resolve_context,
        proposal_tx,
        runtime_status.clone(),
        shutdown_rx,
    ));

    tokio::task::yield_now().await;

    let mut default_behavior =
        crate::load_agent_behavior(node.as_ref(), agent.default_behavior_id())
            .await
            .unwrap()
            .expect("default behavior document");
    default_behavior.system_prompt = Some("updated prompt".to_string());
    crate::upsert_agent_behavior(node.as_ref(), &default_behavior)
        .await
        .unwrap();

    tokio::task::yield_now().await;
    let debouncing = fetch_runtime_status(node.as_ref(), agent.agent_did()).await;
    assert_eq!(debouncing.reconcile_phase, "debouncing");
    tokio::time::advance(CONTROL_RECONCILE_DEBOUNCE + Duration::from_millis(1)).await;
    tokio::task::yield_now().await;

    let snapshot = proposal_rx.recv().await.expect("reconciled snapshot");
    assert_eq!(
        snapshot
            .behaviors
            .get(agent.default_behavior_id())
            .expect("default behavior in snapshot")
            .system_prompt,
        "updated prompt"
    );
    let resolving = fetch_runtime_status(node.as_ref(), agent.agent_did()).await;
    assert_eq!(resolving.reconcile_phase, "resolving");

    let _ = shutdown_tx.send(true);
    watcher_task.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn control_watcher_recovers_after_resolve_error() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("control-watcher-recover"));
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-control-recover",
        "http://127.0.0.1:8112/v1",
    )
    .await;
    let agent = crate::DefraAgent::from_default_behavior_documents(
        node.clone(),
        identity,
        crate::agent::DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let resolve_context = agent
        .document_runtime_context()
        .cloned()
        .expect("document-backed agent");
    let runtime_status = RuntimeStatusHandle::new(node.clone(), agent.agent_did().to_string());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposal_tx, mut proposal_rx) = mpsc::channel(4);

    let watcher_task = tokio::spawn(run_control_watcher(
        node.clone(),
        agent.agent_did().to_string(),
        resolve_context,
        proposal_tx,
        runtime_status.clone(),
        shutdown_rx,
    ));

    tokio::task::yield_now().await;
    update_agent_principal_enabled(node.as_ref(), agent.agent_did(), false).await;

    tokio::task::yield_now().await;
    tokio::time::advance(CONTROL_RECONCILE_DEBOUNCE + Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert!(proposal_rx.try_recv().is_err());
    let failed_status = fetch_runtime_status(node.as_ref(), agent.agent_did()).await;
    assert_eq!(failed_status.reconcile_phase, "idle");
    assert_eq!(failed_status.active_generation, 0);
    assert_eq!(failed_status.last_reconcile_result, "error");
    assert!(!failed_status.last_reconcile_error.is_empty());

    update_agent_principal_enabled(node.as_ref(), agent.agent_did(), true).await;

    tokio::task::yield_now().await;
    tokio::time::advance(CONTROL_RECONCILE_DEBOUNCE + Duration::from_millis(1)).await;
    tokio::task::yield_now().await;

    let snapshot = proposal_rx.recv().await.expect("recovered snapshot");
    assert_eq!(snapshot.default_behavior_id, agent.default_behavior_id());
    let recovered_status = fetch_runtime_status(node.as_ref(), agent.agent_did()).await;
    assert_eq!(recovered_status.reconcile_phase, "resolving");

    let _ = shutdown_tx.send(true);
    watcher_task.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn control_watcher_resolves_tool_selection_into_reconciled_tool_surface() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("control-watcher-tools"));
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-control-tools",
        "http://127.0.0.1:8113/v1",
    )
    .await;
    let agent = crate::DefraAgent::from_default_behavior_documents(
        node.clone(),
        identity,
        crate::agent::DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let resolve_context = agent
        .document_runtime_context()
        .cloned()
        .expect("document-backed agent");
    let runtime_status = RuntimeStatusHandle::new(node.clone(), agent.agent_did().to_string());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposal_tx, mut proposal_rx) = mpsc::channel(4);

    let watcher_task = tokio::spawn(run_control_watcher(
        node.clone(),
        agent.agent_did().to_string(),
        resolve_context,
        proposal_tx,
        runtime_status.clone(),
        shutdown_rx,
    ));

    tokio::task::yield_now().await;

    let selection_id = format!("{}:tools", agent.default_behavior_id());
    crate::upsert_tool_selection(
        node.as_ref(),
        &ToolSelectionDocument {
            selection_id: selection_id.clone(),
            agent_did: agent.agent_did().to_string(),
            display_name: Some("Read tools".to_string()),
            enable_file_tools: Some(true),
            file_tools_mode: Some("ReadOnly".to_string()),
            enable_bash: Some(false),
            bash_mode: Some("Off".to_string()),
            cli_tool_names: Some(Vec::new()),
            enable_meta_tools: Some(false),
            delegate_to: Some(Vec::new()),
        },
    )
    .await
    .unwrap();

    let mut default_behavior =
        crate::load_agent_behavior(node.as_ref(), agent.default_behavior_id())
            .await
            .unwrap()
            .expect("default behavior document");
    default_behavior.tool_selection_id = Some(selection_id);
    crate::upsert_agent_behavior(node.as_ref(), &default_behavior)
        .await
        .unwrap();

    tokio::task::yield_now().await;
    tokio::time::advance(CONTROL_RECONCILE_DEBOUNCE + Duration::from_millis(1)).await;
    tokio::task::yield_now().await;

    let snapshot = proposal_rx.recv().await.expect("reconciled snapshot");
    let tool_surface = snapshot
        .tool_surfaces
        .get(agent.default_behavior_id())
        .expect("default behavior tool surface");
    let tool_names = tool_surface.tool_names();
    assert!(tool_names.contains(&"read_file".to_string()));
    assert!(tool_names.contains(&"list_files".to_string()));
    assert!(!tool_names.contains(&"discover_tools".to_string()));

    let _ = shutdown_tx.send(true);
    watcher_task.await.unwrap().unwrap();
}

async fn update_agent_principal_enabled(
    node: &defra_node::EmbeddedNode,
    agent_did: &str,
    enabled: bool,
) {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let mutation = format!(
        r#"mutation {{
            update_AgentPrincipal(
                filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }},
                input: {{ enabled: {enabled} }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "update_AgentPrincipal failed: {:?}",
        response.errors
    );
}
