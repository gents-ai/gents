use super::bootstrap::{
    wait_for_bootstrap_chat_ready, wait_for_bootstrap_chat_rows,
    wait_for_live_deployment_docs_in_store,
};
use super::*;

struct StartedLiveRemoteDeployment {
    deployment: LiveRemoteDeployment,
    runtime_api: BootstrapRuntimeApi,
    peer_record: crate::client::PeerRecord,
    remote_addr: String,
}

fn start_chat_driver(
    runtime: Arc<Runtime>,
    core: Arc<ClientCore>,
    log_store: Arc<DesktopLogStore>,
) -> AuditDriver {
    let mut driver = build_driver_with_client(runtime, core, log_store);
    driver.app.state.activity = Activity::Chat;
    driver.render();
    driver
}

fn start_live_remote_deployment(
    runtime: &Arc<Runtime>,
    tempdir: &tempfile::TempDir,
    remote_dir: &str,
    key_dir: &str,
    agent_label: &str,
    deployment_label: &str,
    backend: &AgentBackendConfig,
) -> Result<StartedLiveRemoteDeployment> {
    let remote_core = Arc::new(runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join(remote_dir)),
        bootstrap_live_core_options(),
    ))?);
    let running_agent = runtime.block_on(spawn_backed_agent(
        remote_core.node_arc(),
        tempdir
            .path()
            .join(key_dir)
            .join(format!("{agent_label}.key")),
        agent_label,
        backend,
    ))?;
    let docs = runtime.block_on(seed_live_manage_documents(
        remote_core.as_ref(),
        &running_agent.did,
        agent_label,
        backend,
    ))?;

    let remote_addr = runtime.block_on(wait_for_connectable_iroh_addr(
        remote_core.as_ref(),
        deployment_label,
    ))?;
    let runtime_api = BootstrapRuntimeApi::start(
        runtime,
        Arc::clone(&remote_core),
        deployment_label,
        remote_addr.clone(),
    )?;
    let mut peer_record =
        crate::client::PeerRecord::new(deployment_label, &remote_addr, &running_agent.did);
    peer_record.graphql = Some(runtime_api.graphql_url().to_string());

    Ok(StartedLiveRemoteDeployment {
        deployment: LiveRemoteDeployment {
            label: deployment_label.to_string(),
            peer_id: peer_record.peer_id.clone(),
            agent_did: running_agent.did.clone(),
            core: remote_core,
            running_agent,
            docs,
        },
        runtime_api,
        peer_record,
        remote_addr,
    })
}

pub(crate) fn build_live_desktop_fixture(
    label: &str,
    log_store: Arc<DesktopLogStore>,
) -> Result<LiveDesktopFixture> {
    init_test_tracing();

    let backend = AgentBackendConfig::live_from_env()?;
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;

    let unique_label = format!("{label}-{}", uuid::Uuid::new_v4().simple());
    let started = start_live_remote_deployment(
        &runtime,
        &tempdir,
        "remote",
        "agent",
        &unique_label,
        &unique_label,
        &backend,
    )?;

    let desktop_paths = DesktopPaths::from_root(tempdir.path().join("desktop"));
    let agent_home = tempdir.path().join("agent-home");
    seed_agent_home_runtime_state_for_bootstrap(
        &agent_home,
        &unique_label,
        &started.deployment.agent_did,
        started.runtime_api.graphql_url(),
        started.deployment.core.local_peer_id(),
        &started.remote_addr,
    )?;
    let init_summary = runtime.block_on(crate::local_runtime::init_standard_local_runtime(
        crate::local_runtime::DesktopInitOptions {
            agent_home: agent_home.clone(),
            desktop_paths: desktop_paths.clone(),
            label: unique_label.clone(),
        },
    ))?;
    let peer_id = init_summary.peer_record_id.clone();

    let desktop_core = Arc::new(runtime.block_on(ClientCore::start_with_paths_and_options(
        desktop_paths,
        bootstrap_live_core_options(),
    ))?);
    if !desktop_core.bootstrap_errors().is_empty() {
        anyhow::bail!(
            "unexpected bootstrap errors in live desktop fixture: {:?}",
            desktop_core.bootstrap_errors()
        );
    }
    let desktop_addr = runtime.block_on(wait_for_connectable_iroh_addr(
        desktop_core.as_ref(),
        "single-agent live desktop",
    ))?;
    let _desktop_api =
        BootstrapRuntimeApi::start(&runtime, Arc::clone(&desktop_core), "Desktop", desktop_addr)?;
    runtime.block_on(wait_for_connected_peer(
        desktop_core.as_ref(),
        started.deployment.core.local_peer_id(),
        "single-agent live desktop bootstrap",
    ))?;
    runtime.block_on(wait_for_connected_peer(
        started.deployment.core.as_ref(),
        desktop_core.local_peer_id(),
        "single-agent live runtime bootstrap",
    ))?;
    wait_for_live_deployment_docs_in_store(
        desktop_core.as_ref(),
        &unique_label,
        &started.deployment.agent_did,
        &started.deployment.docs,
    )?;

    let mut driver = start_chat_driver(Arc::clone(&runtime), Arc::clone(&desktop_core), log_store);
    wait_for_bootstrap_chat_ready(&mut driver, &peer_id, &started.deployment.agent_did)?;

    Ok(LiveDesktopFixture {
        runtime,
        _tempdir: tempdir,
        driver,
        running_agent: Some(started.deployment.running_agent),
        remote_core: Some(started.deployment.core),
        docs: started.deployment.docs,
        runtime_apis: vec![started.runtime_api],
    })
}

pub(crate) fn build_multi_agent_desktop_fixture_with_backend(
    label: &str,
    backend: &AgentBackendConfig,
    log_store: Arc<DesktopLogStore>,
) -> Result<MultiAgentLiveDesktopFixture> {
    build_named_multi_agent_desktop_fixture_with_backend(
        label,
        &["alpha", "bravo"],
        backend,
        log_store,
    )
}

pub(crate) fn build_named_multi_agent_desktop_fixture_with_backend(
    label: &str,
    deployment_suffixes: &[&str],
    backend: &AgentBackendConfig,
    log_store: Arc<DesktopLogStore>,
) -> Result<MultiAgentLiveDesktopFixture> {
    init_test_tracing();

    if deployment_suffixes.is_empty() {
        anyhow::bail!("expected at least one live deployment suffix");
    }

    let backend = backend.clone();
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let desktop_paths = DesktopPaths::from_root(tempdir.path().join("desktop"));
    let mut deployments = Vec::new();
    let mut runtime_apis = Vec::new();
    let mut peer_records = Vec::new();

    for suffix in deployment_suffixes {
        let unique_label = format!("{label}-{suffix}-{}", uuid::Uuid::new_v4().simple());
        let deployment_label = format!("{} Server", title_case_ascii(suffix));
        let started = start_live_remote_deployment(
            &runtime,
            &tempdir,
            &format!("remote-{suffix}"),
            &format!("agent-{suffix}"),
            &unique_label,
            &deployment_label,
            &backend,
        )?;
        runtime_apis.push(started.runtime_api);
        peer_records.push(started.peer_record);
        deployments.push(started.deployment);
    }

    write_peer_directory_records(&desktop_paths, &peer_records)?;
    let desktop_core = Arc::new(runtime.block_on(ClientCore::start_with_paths_and_options(
        desktop_paths,
        bootstrap_live_core_options(),
    ))?);
    if !desktop_core.bootstrap_errors().is_empty() {
        anyhow::bail!(
            "unexpected bootstrap errors in multi-agent live desktop fixture: {:?}",
            desktop_core.bootstrap_errors()
        );
    }
    let desktop_addr = runtime.block_on(wait_for_connectable_iroh_addr(
        desktop_core.as_ref(),
        "multi-agent live desktop",
    ))?;
    let desktop_api =
        BootstrapRuntimeApi::start(&runtime, Arc::clone(&desktop_core), "Desktop", desktop_addr)?;
    for deployment in &deployments {
        runtime.block_on(wait_for_connected_peer(
            desktop_core.as_ref(),
            deployment.core.local_peer_id(),
            &format!("desktop -> {}", deployment.label),
        ))?;
        runtime.block_on(wait_for_connected_peer(
            deployment.core.as_ref(),
            desktop_core.local_peer_id(),
            &format!("{} -> desktop", deployment.label),
        ))?;
    }
    for deployment in &deployments {
        wait_for_live_deployment_docs_in_store(
            &desktop_core,
            &deployment.label,
            &deployment.agent_did,
            &deployment.docs,
        )?;
    }

    let mut driver = start_chat_driver(Arc::clone(&runtime), Arc::clone(&desktop_core), log_store);
    wait_for_bootstrap_chat_rows(&mut driver, &deployments)?;

    Ok(MultiAgentLiveDesktopFixture {
        runtime,
        _tempdir: tempdir,
        driver,
        desktop_api,
        deployments,
        backend,
        runtime_apis,
    })
}

pub(crate) fn build_multi_agent_live_desktop_fixture(
    label: &str,
    log_store: Arc<DesktopLogStore>,
) -> Result<MultiAgentLiveDesktopFixture> {
    let backend = AgentBackendConfig::live_from_env()?;
    build_multi_agent_desktop_fixture_with_backend(label, &backend, log_store)
}
