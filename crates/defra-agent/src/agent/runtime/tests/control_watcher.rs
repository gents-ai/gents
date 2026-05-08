use super::support::*;
use super::*;

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

    let selection_id = crate::default_tool_selection_id_for_behavior(agent.default_behavior_id());
    crate::upsert_tool_selection(
        node.as_ref(),
        &ToolSelectionDocument {
            selection_id: selection_id.clone(),
            agent_did: agent.agent_did().to_string(),
            display_name: Some("Read tools".to_string()),
            enable_file_tools: Some(true),
            file_tools_mode: Some("ReadOnly".to_string()),
            file_tool_root: None,
            enable_bash: Some(false),
            bash_mode: Some("Off".to_string()),
            command_execution_policy: None,
            command_allowed_argv_prefixes: Some(Vec::new()),
            command_forbidden_argv_prefixes: Some(Vec::new()),
            command_network_mode: None,
            cli_tool_names: Some(Vec::new()),
            enable_meta_tools: Some(false),
            allowed_mcp_service_ids: Some(Vec::new()),
            delegate_to: Some(Vec::new()),
            ..Default::default()
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
