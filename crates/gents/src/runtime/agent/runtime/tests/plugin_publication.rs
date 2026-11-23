use super::support::*;
use super::*;
use gents_store::test_support::STALL_CEILING;

async fn configured_publication_agent(
    fixture: &PluginRuntimeFixture,
    home: &std::path::Path,
) -> crate::Gents {
    super::plugin_readmission::install_fixture_plugin(home);
    let record = gents_pack::plugin::store::read_record(home, "fixture", "list_files").unwrap();
    let agent_did = fixture.node_binding.default_agent_did.as_ref().unwrap();
    let tools: gents_documents::Tools = serde_json::from_value(serde_json::json!({
        "tools_id": format!("{agent_did}:tools"),
        "node_did": fixture.runtime.did(),
        "integrations": {"plugins": [{"plugin": "fixture/list_files", "digest": record.digest}]}
    }))
    .unwrap();
    gents_documents::write_tools_document(
        &fixture.user_access,
        &tools,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await
    .unwrap();
    crate::Gents::from_default_agent_documents(
        fixture.node.clone(),
        fixture.runtime.clone(),
        crate::runtime::agent::DocumentRuntimeOptions {
            backend_admission: Some(fixture.admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            plugin_home: Some(home.to_owned()),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

enum PublicationChange {
    None,
    RevokePlugin,
}

/// The watcher meets an installer that never releases inside the epoch: the
/// change waits out the epoch, then publishes with only the busy plugin agent
/// demoted, and a revocation applies while the store is still held. Once the
/// installer releases, the demoted agent comes back without another event.
async fn bounded_publication_case(change: PublicationChange) {
    let endpoint = crate::support::mock_endpoint::MockModelEndpoint::start("default").unwrap();
    let fixture = plugin_runtime_fixture(
        "publication-epoch",
        "backend-publication-epoch",
        endpoint.endpoint(),
    )
    .await
    .unwrap();
    let home = tempfile::tempdir().unwrap();
    let agent = configured_publication_agent(&fixture, home.path()).await;
    let context = agent.document_runtime_context().cloned().unwrap();
    let node_did = fixture.runtime.did().to_owned();
    let agent_did = agent.default_agent_did().to_owned();
    let runtime_status = RuntimeStatusHandle::new(fixture.node.clone(), &node_did);
    runtime_status.initialize_startup(&agent_did).await.unwrap();
    let tool_runtime = gents_tools::surface::ToolRuntimeContext::new_with_node_did(
        fixture.node.clone(),
        gents_tools::mcp::pool::McpPool::default(),
        gents_tools::mcp::health::ServiceHealthMap::default(),
        "localhost",
        None,
        &node_did,
        Some(context.identity.clone()),
    )
    .with_plugins(context.plugins.clone());
    let installer = gents_pack::plugin::store::lock_store(home.path()).unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposal_tx, mut proposal_rx) = mpsc::channel(4);
    let (health_tx, health_rx) = mpsc::channel(1);
    let watcher = tokio::spawn(run_control_watcher_with_timing(
        ControlWatcherInputs {
            node: fixture.node.clone(),
            backend_admission: fixture.admission(),
            subscription: subscribe_control_changes(&fixture.node).unwrap(),
            node_did: node_did.clone(),
            resolve_context: context,
            tool_runtime,
            system_log: false,
            proposals_tx: proposal_tx,
            runtime_status,
            health_events_rx: health_rx,
            shutdown: shutdown_rx,
        },
        ControlWatcherTiming {
            publication_wait: Duration::from_millis(50),
            debounce: Duration::from_millis(1),
            local_debounce: Duration::from_millis(1),
            settle_retry: Duration::from_millis(5),
            settle_window: Duration::from_millis(1),
            idle_sleep: Duration::from_secs(3600),
        },
    ));
    if matches!(change, PublicationChange::RevokePlugin) {
        merge_fixture_document(
            &fixture.user_access,
            "test.publication.revoke_plugin",
            gents_store::collection::Collection::Tools,
            &node_did,
            &format!("{agent_did}:tools"),
            serde_json::json!({"integrations": {"plugins": null}}),
        )
        .await;
    }
    health_tx.send(()).await.unwrap();
    let published = tokio::time::timeout(STALL_CEILING, proposal_rx.recv())
        .await
        .expect("the held store deferred the change past its epoch")
        .unwrap();
    assert!(
        gents_pack::plugin::store::try_lock_store_shared(home.path()).is_err(),
        "the change published while the installer still held the store"
    );
    match change {
        PublicationChange::RevokePlugin => {
            assert!(published.agents.contains_key(&agent_did));
            assert!(published.tool_surfaces[&agent_did]
                .plugin_resolutions()
                .is_empty());
            let _ = shutdown_tx.send(true);
            tokio::time::timeout(STALL_CEILING, watcher)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            drop(installer);
            return;
        }
        PublicationChange::None => {}
    }
    let demoted = &published.unavailable_agents[&agent_did];
    assert!(demoted.publication_busy, "{demoted:?}");
    assert!(
        demoted.diagnostic.contains("plugin store is busy"),
        "{}",
        demoted.diagnostic
    );
    drop(installer);
    let recovered = tokio::time::timeout(STALL_CEILING, proposal_rx.recv())
        .await
        .expect("the demoted agent was stranded after the installer released")
        .unwrap();
    assert!(recovered.agents.contains_key(&agent_did));
    assert!(!recovered.unavailable_agents.contains_key(&agent_did));
    let _ = shutdown_tx.send(true);
    tokio::time::timeout(STALL_CEILING, watcher)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn a_held_store_demotes_only_after_its_epoch_and_recovers_on_release() {
    bounded_publication_case(PublicationChange::None).await;
}

#[tokio::test]
async fn a_held_store_cannot_defer_a_tools_revocation_past_its_epoch() {
    bounded_publication_case(PublicationChange::RevokePlugin).await;
}

#[tokio::test]
async fn a_watcher_whose_home_is_replaced_stops_without_writing_into_it() {
    let endpoint = crate::support::mock_endpoint::MockModelEndpoint::start("default").unwrap();
    let fixture = plugin_runtime_fixture(
        "publication-replaced-home",
        "backend-publication-replaced-home",
        endpoint.endpoint(),
    )
    .await
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let agent = configured_publication_agent(&fixture, &home).await;
    let context = agent.document_runtime_context().cloned().unwrap();
    let node_did = fixture.runtime.did().to_owned();
    let runtime_status = RuntimeStatusHandle::new(fixture.node.clone(), &node_did);
    runtime_status
        .initialize_startup(agent.default_agent_did())
        .await
        .unwrap();
    let tool_runtime = gents_tools::surface::ToolRuntimeContext::new_with_node_did(
        fixture.node.clone(),
        gents_tools::mcp::pool::McpPool::default(),
        gents_tools::mcp::health::ServiceHealthMap::default(),
        "localhost",
        None,
        &node_did,
        Some(context.identity.clone()),
    )
    .with_plugins(context.plugins.clone());
    std::fs::rename(&home, root.path().join("retired")).unwrap();
    std::fs::create_dir(&home).unwrap();
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposal_tx, mut proposal_rx) = mpsc::channel(4);
    let (health_tx, health_rx) = mpsc::channel(1);
    let watcher = tokio::spawn(run_control_watcher_with_timing(
        ControlWatcherInputs {
            node: fixture.node.clone(),
            backend_admission: fixture.admission(),
            subscription: subscribe_control_changes(&fixture.node).unwrap(),
            node_did,
            resolve_context: context,
            tool_runtime,
            system_log: false,
            proposals_tx: proposal_tx,
            runtime_status,
            health_events_rx: health_rx,
            shutdown: shutdown_rx,
        },
        ControlWatcherTiming {
            debounce: Duration::from_millis(1),
            local_debounce: Duration::from_millis(1),
            ..super::control_watcher::TEST_CONTROL_WATCHER_TIMING
        },
    ));
    health_tx.send(()).await.unwrap();
    tokio::time::timeout(STALL_CEILING, watcher)
        .await
        .expect("the watcher kept running over a replaced home")
        .unwrap()
        .unwrap();
    assert!(proposal_rx.try_recv().is_err());
    assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0);
}

/// The runtime started before its plugin store existed; setup then creates
/// the store's directory and, later, its lock file. The watcher reconciles
/// through each stage and never stops, since nothing it captured is gone.
#[tokio::test]
async fn a_store_created_after_the_watcher_started_never_stops_it() {
    let endpoint = crate::support::mock_endpoint::MockModelEndpoint::start("default").unwrap();
    let fixture = plugin_runtime_fixture(
        "publication-created-store",
        "backend-publication-created-store",
        endpoint.endpoint(),
    )
    .await
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let agent = crate::Gents::from_default_agent_documents(
        fixture.node.clone(),
        fixture.runtime.clone(),
        crate::runtime::agent::DocumentRuntimeOptions {
            backend_admission: Some(fixture.admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            plugin_home: Some(home.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let store_dir = home.join(gents_store::local::home::PLUGINS_DIR_NAME);
    assert!(!store_dir.exists(), "the runtime created its store itself");
    let context = agent.document_runtime_context().cloned().unwrap();
    let node_did = fixture.runtime.did().to_owned();
    let runtime_status = RuntimeStatusHandle::new(fixture.node.clone(), &node_did);
    runtime_status
        .initialize_startup(agent.default_agent_did())
        .await
        .unwrap();
    let tool_runtime = gents_tools::surface::ToolRuntimeContext::new_with_node_did(
        fixture.node.clone(),
        gents_tools::mcp::pool::McpPool::default(),
        gents_tools::mcp::health::ServiceHealthMap::default(),
        "localhost",
        None,
        &node_did,
        Some(context.identity.clone()),
    )
    .with_plugins(context.plugins.clone());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposal_tx, mut proposal_rx) = mpsc::channel(4);
    let (health_tx, health_rx) = mpsc::channel(1);
    let mut watcher = tokio::spawn(run_control_watcher_with_timing(
        ControlWatcherInputs {
            node: fixture.node.clone(),
            backend_admission: fixture.admission(),
            subscription: subscribe_control_changes(&fixture.node).unwrap(),
            node_did,
            resolve_context: context,
            tool_runtime,
            system_log: false,
            proposals_tx: proposal_tx,
            runtime_status,
            health_events_rx: health_rx,
            shutdown: shutdown_rx,
        },
        ControlWatcherTiming {
            debounce: Duration::from_millis(1),
            local_debounce: Duration::from_millis(1),
            ..super::control_watcher::TEST_CONTROL_WATCHER_TIMING
        },
    ));
    let stages: [&dyn Fn(); 3] = [
        &|| {},
        &|| std::fs::create_dir(&store_dir).unwrap(),
        &|| gents_pack::plugin::store::bootstrap_store(&home).unwrap(),
    ];
    for create in stages {
        create();
        health_tx.send(()).await.unwrap();
        tokio::select! {
            proposal = proposal_rx.recv() => assert!(proposal.is_some()),
            finished = &mut watcher => panic!("the watcher stopped while its store was being created: {finished:?}"),
            () = tokio::time::sleep(STALL_CEILING) => panic!("the watcher proposed nothing after a health event"),
        }
    }
    shutdown_tx.send(true).unwrap();
    tokio::time::timeout(STALL_CEILING, watcher)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

/// Every agent selects a plugin and an installer holds the store through
/// startup's wait: the node starts degraded instead of failing, and the agent
/// comes back once the installer releases, with no configuration change.
#[tokio::test]
async fn a_store_held_through_startup_starts_degraded_and_recovers_on_release() {
    let endpoint = crate::support::mock_endpoint::MockModelEndpoint::start("default").unwrap();
    let fixture = plugin_runtime_fixture(
        "publication-degraded-startup",
        "backend-publication-degraded-startup",
        endpoint.endpoint(),
    )
    .await
    .unwrap();
    let home = tempfile::tempdir().unwrap();
    let agent = configured_publication_agent(&fixture, home.path()).await;
    let agent_did = agent.default_agent_did().to_owned();
    let node_did = fixture.runtime.did().to_owned();
    let context = agent.document_runtime_context().cloned().unwrap();
    let tool_runtime = gents_tools::surface::ToolRuntimeContext::new_with_node_did(
        fixture.node.clone(),
        gents_tools::mcp::pool::McpPool::default(),
        gents_tools::mcp::health::ServiceHealthMap::default(),
        "localhost",
        None,
        &node_did,
        Some(context.identity.clone()),
    )
    .with_plugins(context.plugins.clone());
    let runtime_status = RuntimeStatusHandle::new(fixture.node.clone(), &node_did);
    runtime_status.initialize_startup(&agent_did).await.unwrap();
    let installer = gents_pack::plugin::store::lock_store(home.path()).unwrap();
    let (subscription, started) = super::super::startup::resolve_startup_until(
        &agent,
        &tool_runtime,
        &runtime_status,
        tokio::time::Instant::now() + Duration::from_millis(20),
    )
    .await
    .expect("a held store must not abort startup");
    assert!(started.agents.is_empty());
    assert!(started.unavailable_agents[&agent_did].publication_busy);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposal_tx, mut proposal_rx) = mpsc::channel(4);
    let watcher = tokio::spawn(run_control_watcher_with_timing(
        ControlWatcherInputs {
            node: fixture.node.clone(),
            backend_admission: fixture.admission(),
            subscription: subscription.unwrap(),
            node_did,
            resolve_context: context,
            tool_runtime,
            system_log: false,
            proposals_tx: proposal_tx,
            runtime_status,
            health_events_rx: mpsc::channel::<()>(1).1,
            shutdown: shutdown_rx,
        },
        ControlWatcherTiming {
            publication_wait: Duration::from_secs(3600),
            debounce: Duration::from_millis(1),
            local_debounce: Duration::from_millis(1),
            settle_retry: Duration::from_millis(5),
            settle_window: Duration::from_millis(1),
            idle_sleep: Duration::from_secs(3600),
        },
    ));
    drop(installer);
    let recovered = tokio::time::timeout(STALL_CEILING, proposal_rx.recv())
        .await
        .expect("the degraded agent was stranded after the installer released")
        .unwrap();
    assert!(recovered.agents.contains_key(&agent_did));
    assert!(!recovered.unavailable_agents.contains_key(&agent_did));
    let _ = shutdown_tx.send(true);
    tokio::time::timeout(STALL_CEILING, watcher)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

/// A recovery requested while the configuration is unchanged still reaches
/// the supervisor: a parked slot's configuration matches the active one.
#[tokio::test]
async fn a_recovery_request_sends_an_unchanged_configuration() {
    let endpoint = crate::support::mock_endpoint::MockModelEndpoint::start("default").unwrap();
    let fixture = plugin_runtime_fixture(
        "publication-recovery-wake",
        "backend-publication-recovery-wake",
        endpoint.endpoint(),
    )
    .await
    .unwrap();
    let home = tempfile::tempdir().unwrap();
    let agent = configured_publication_agent(&fixture, home.path()).await;
    let agent_did = agent.default_agent_did().to_owned();
    let node_did = fixture.runtime.did().to_owned();
    let context = agent.document_runtime_context().cloned().unwrap();
    let tool_runtime = gents_tools::surface::ToolRuntimeContext::new_with_node_did(
        fixture.node.clone(),
        gents_tools::mcp::pool::McpPool::default(),
        gents_tools::mcp::health::ServiceHealthMap::default(),
        "localhost",
        None,
        &node_did,
        Some(context.identity.clone()),
    )
    .with_plugins(context.plugins.clone());
    let runtime_status = RuntimeStatusHandle::new(fixture.node.clone(), &node_did);
    runtime_status.initialize_startup(&agent_did).await.unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (proposal_tx, mut proposal_rx) = mpsc::channel(4);
    let (health_tx, health_rx) = mpsc::channel(1);
    let watcher = tokio::spawn(run_control_watcher_with_timing(
        ControlWatcherInputs {
            node: fixture.node.clone(),
            backend_admission: fixture.admission(),
            subscription: subscribe_control_changes(&fixture.node).unwrap(),
            node_did,
            resolve_context: context,
            tool_runtime,
            system_log: false,
            proposals_tx: proposal_tx,
            runtime_status: runtime_status.clone(),
            health_events_rx: health_rx,
            shutdown: shutdown_rx,
        },
        ControlWatcherTiming {
            debounce: Duration::from_millis(1),
            local_debounce: Duration::from_millis(1),
            ..super::control_watcher::TEST_CONTROL_WATCHER_TIMING
        },
    ));
    health_tx.send(()).await.unwrap();
    let first = tokio::time::timeout(STALL_CEILING, proposal_rx.recv())
        .await
        .unwrap()
        .unwrap();
    runtime_status.request_publication_recovery();
    let again = tokio::time::timeout(STALL_CEILING, proposal_rx.recv())
        .await
        .expect("a recovery with an unchanged configuration was deduplicated")
        .unwrap();
    assert_eq!(
        first.configuration_fingerprint(),
        again.configuration_fingerprint()
    );
    assert!(again.agents.contains_key(&agent_did));
    let _ = shutdown_tx.send(true);
    tokio::time::timeout(STALL_CEILING, watcher)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
