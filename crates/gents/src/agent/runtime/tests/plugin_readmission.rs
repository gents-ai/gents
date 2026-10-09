use super::support::*;
use super::*;

use std::sync::Arc;

// Detects a deadlock; not a latency assertion.
const READINESS_DEADLOCK_GUARD: Duration = Duration::from_secs(30);

async fn wait_for_agent_state(
    node: &defra_node::EmbeddedNode,
    node_did: &str,
    agent_id: &str,
    state: AgentReadinessState,
    reason: Option<AgentReadinessUnavailableReason>,
) -> NodeReadinessSnapshot {
    let deadline = tokio::time::Instant::now() + READINESS_DEADLOCK_GUARD;
    loop {
        let readiness = fetch_node_readiness(node, node_did).await;
        if readiness.agents.iter().any(|entry| {
            entry.agent_id == agent_id && entry.state == state && entry.reason == reason
        }) {
            return readiness;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {agent_id} to reach {state:?}/{reason:?}; last readiness: {readiness:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Installs the fixture pack's `fixture/list_files` into the plugin store
/// only, returning the identity an install records.
fn install_fixture_plugin(plugin_home: &std::path::Path) -> crate::pack::PackIdentity {
    let (_pack_guard, pack_root) =
        crate::test_support::fixture_pack_copy("bind_plugin_fixture", &serde_json::json!({}));
    let (pack_bytes, _) = crate::pack_archive::pack_dir(&pack_root).unwrap();
    let archive = crate::pack_archive::PackArchive::from_bytes(&pack_bytes).unwrap();
    let installed = crate::plugin::install::install_pack_plugins(
        plugin_home,
        archive.manifest(),
        archive.digest(),
        |path| archive.asset(path),
        false,
    )
    .unwrap();
    crate::pack::PackIdentity::new(
        archive.manifest(),
        archive.digest(),
        installed
            .iter()
            .map(|plugin| crate::pack::InstalledPackPlugin {
                name: plugin.name.clone(),
                digest: plugin.digest.clone(),
            })
            .collect(),
    )
}

/// #2338 end to end: a agent_config whose Tools document names a missing plugin
/// burns its build budget and is demoted; installing that plugin mid-run —
/// the plugin store, then the plugin-store record every install path writes
/// — re-admits it on the next reconcile.
#[tokio::test]
async fn demoted_agent_is_readmitted_when_its_named_plugin_installs_midrun() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("plugin-readmission"));
    let endpoint = MockModelEndpoint::start("default").unwrap();
    bind_default_agent_backend(
        node.as_ref(),
        identity.did(),
        "backend-plugin-readmit",
        endpoint.endpoint(),
    )
    .await;
    let plugin_home = tempfile::tempdir().unwrap();
    let agent = crate::Gents::from_default_agent_documents(
        node.clone(),
        identity.clone(),
        crate::agent::DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            plugin_home: Some(plugin_home.path().to_path_buf()),
            retry_policy: crate::retry::RetryPolicy {
                max_retries: 3,
                base_delay_ms: 5,
                max_delay_ms: 10,
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let node_did = identity.did().to_string();
    let agent_id = agent.default_agent_id().to_string();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let run = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_process_state(node.as_ref(), &node_did, "ready").await;

    let tools: Tools = serde_json::from_value(serde_json::json!({
        "tools_id": format!("{agent_id}:tools"),
        "node_did": node_did,
        "integrations": {"plugins": [{"plugin": "fixture/list_files"}]},
    }))
    .unwrap();
    crate::config_client::write_tools_document(
        &crate::config_client::ConfigAccess::Local(node.clone()),
        &tools,
    )
    .await
    .unwrap();
    let demoted = wait_for_agent_state(
        node.as_ref(),
        &node_did,
        &agent_id,
        AgentReadinessState::Unavailable,
        Some(AgentReadinessUnavailableReason::ExecutorStartFailed),
    )
    .await;
    assert_eq!(
        demoted.process_state,
        NodeReadinessProcessState::Ready,
        "a demoted Agent degrades readiness without stopping the process"
    );

    let pack = install_fixture_plugin(plugin_home.path());
    crate::pack::record_plugin_store_change(
        &crate::config_client::ConfigAccess::Local(node.clone()),
        &node_did,
        plugin_home.path(),
        &pack.coordinate,
        Some(&pack),
    )
    .await
    .unwrap();

    let readmitted = wait_for_agent_state(
        node.as_ref(),
        &node_did,
        &agent_id,
        AgentReadinessState::Ready,
        None,
    )
    .await;
    assert!(
        readmitted.active_generation >= 3,
        "the Tools write and the install each applied a generation: {readmitted:?}"
    );

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(READINESS_DEADLOCK_GUARD, run)
        .await
        .expect("agent task should join")
        .expect("run task should join")
        .expect("agent run should return ok");
}
