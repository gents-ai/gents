#[derive(Debug, Clone)]
pub(crate) struct LiveAgentDocs {
    behavior_id: String,
    backend_id: String,
    tool_selection_id: String,
    inference_profile_id: String,
    scheduled_task_id: String,
}

pub(crate) struct LiveDesktopFixture {
    runtime: Arc<Runtime>,
    _tempdir: tempfile::TempDir,
    driver: AuditDriver,
    running_agent: Option<RunningAgent>,
    remote_core: Option<Arc<ClientCore>>,
    docs: LiveAgentDocs,
    backend: AgentBackendConfig,
}

impl LiveDesktopFixture {
    fn shutdown(mut self) -> Result<()> {
        self.driver.app.shutdown_client();
        if let Some(running_agent) = self.running_agent.take() {
            self.runtime.block_on(running_agent.shutdown())?;
        }
        if let Some(remote_core) = self.remote_core.take() {
            self.runtime.block_on(remote_core.shutdown())?;
        }
        Ok(())
    }
}

pub(crate) struct LiveRemoteDeployment {
    label: String,
    peer_id: String,
    agent_did: String,
    core: Arc<ClientCore>,
    running_agent: RunningAgent,
    docs: LiveAgentDocs,
}

pub(crate) struct MultiAgentLiveDesktopFixture {
    runtime: Arc<Runtime>,
    _tempdir: tempfile::TempDir,
    driver: AuditDriver,
    deployments: Vec<LiveRemoteDeployment>,
    backend: AgentBackendConfig,
}

impl MultiAgentLiveDesktopFixture {
    fn shutdown(mut self) -> Result<()> {
        self.driver.app.shutdown_client();
        for deployment in self.deployments.drain(..) {
            self.runtime.block_on(deployment.running_agent.shutdown())?;
            self.runtime.block_on(deployment.core.shutdown())?;
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct LiveDeploymentCase<'a> {
    label: String,
    peer_id: String,
    agent_did: String,
    docs: LiveAgentDocs,
    remote_core: &'a ClientCore,
}

pub(crate) struct LiveSubmissionCase {
    prompt: String,
    request_id: String,
    response: String,
    session_id: String,
}

pub(crate) fn live_deployment_case(deployment: &LiveRemoteDeployment) -> LiveDeploymentCase<'_> {
    LiveDeploymentCase {
        label: deployment.label.clone(),
        peer_id: deployment.peer_id.clone(),
        agent_did: deployment.agent_did.clone(),
        docs: deployment.docs.clone(),
        remote_core: deployment.core.as_ref(),
    }
}
pub(crate) fn wait_for_live_deployment_docs_in_store(
    desktop_core: &ClientCore,
    deployment_label: &str,
    agent_did: &str,
    docs: &LiveAgentDocs,
) -> Result<()> {
    wait_for_value(
        &format!("observed live deployment docs for {deployment_label}"),
        Duration::from_secs(120),
        || {
            let snapshot = desktop_core.store().snapshot();
            let has_principal = snapshot
                .agent_principals
                .iter()
                .any(|row| row.agent_did == agent_did);
            let has_behavior = snapshot
                .behaviors
                .iter()
                .any(|row| row.behavior_id == docs.behavior_id);
            let has_backend = snapshot
                .inference_backends
                .iter()
                .any(|row| row.backend_id == docs.backend_id);
            let has_tools = snapshot
                .tool_selections
                .iter()
                .any(|row| row.selection_id == docs.tool_selection_id);
            let has_profile = snapshot
                .inference_profiles
                .iter()
                .any(|row| row.profile_id == docs.inference_profile_id);
            (has_principal && has_behavior && has_backend && has_tools && has_profile).then_some(())
        },
    )
}

pub(crate) fn wait_for_bootstrap_chat_ready(
    driver: &mut AuditDriver,
    peer_id: &str,
    agent_did: &str,
) -> Result<()> {
    wait_for_value("desktop bootstrap status", Duration::from_secs(20), || {
        let texts = driver.render();
        texts
            .iter()
            .any(|text| text.contains("replication: subscriptions armed"))
            .then_some(())
    })?;
    let deployment_target = audit::targets::chat_deployment(peer_id);
    driver.wait_for_target(
        "bootstrapped chat deployment row",
        Duration::from_secs(20),
        &deployment_target,
    )?;
    driver.click_target(&deployment_target);
    wait_for_value(
        "bootstrapped chat selection",
        Duration::from_secs(10),
        || {
            (driver.app.state.chat.shell.selected_peer_id.as_deref() == Some(peer_id)
                && driver.app.state.chat.shell.selected_agent_did.as_deref() == Some(agent_did))
            .then_some(())
        },
    )
}

pub(crate) fn seed_bootstrap_peer_directory(
    paths: &DesktopPaths,
    records: &[crate::client::PeerRecord],
) -> Result<()> {
    std::fs::create_dir_all(paths.root())?;
    let payload = serde_json::json!({
        "peers": records,
    });
    std::fs::write(
        paths.peer_directory_path(),
        serde_json::to_vec_pretty(&payload)?,
    )?;
    Ok(())
}

pub(crate) fn wait_for_bootstrap_chat_rows(
    driver: &mut AuditDriver,
    deployments: &[LiveRemoteDeployment],
) -> Result<()> {
    wait_for_value(
        "desktop multi-agent bootstrap status",
        Duration::from_secs(20),
        || {
            let texts = driver.render();
            texts
                .iter()
                .any(|text| text.contains("replication: subscriptions armed"))
                .then_some(())
        },
    )?;
    for deployment in deployments {
        let deployment_target = audit::targets::chat_deployment(&deployment.peer_id);
        driver.wait_for_target(
            &format!("bootstrapped chat deployment row for {}", deployment.label),
            Duration::from_secs(20),
            &deployment_target,
        )?;
    }
    Ok(())
}

pub(crate) fn build_live_desktop_fixture(
    label: &str,
    log_store: Arc<DesktopLogStore>,
) -> Result<LiveDesktopFixture> {
    init_test_tracing();

    let backend = AgentBackendConfig::live_from_env()?;
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let remote_core = Arc::new(runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("remote")),
        bootstrap_live_core_options(),
    ))?);

    let unique_label = format!("{label}-{}", uuid::Uuid::new_v4().simple());
    let running_agent = runtime.block_on(spawn_backed_agent(
        remote_core.node_arc(),
        tempdir
            .path()
            .join("agent")
            .join(format!("{unique_label}.key")),
        &unique_label,
        &backend,
    ))?;
    let docs = runtime.block_on(seed_live_operator_documents(
        remote_core.as_ref(),
        &running_agent.did,
        &unique_label,
        &backend,
    ))?;
    let remote_addr = runtime.block_on(wait_for_connectable_iroh_addr(
        remote_core.as_ref(),
        &unique_label,
    ))?;
    let runtime_api =
        BootstrapRuntimeApi::start(&runtime, Arc::clone(&remote_core), remote_addr.clone())?;

    let desktop_paths = DesktopPaths::from_root(tempdir.path().join("desktop"));
    let agent_home = tempdir.path().join("agent-home");
    seed_agent_home_runtime_state_for_bootstrap(
        &agent_home,
        &unique_label,
        &running_agent.did,
        runtime_api.graphql_url(),
        remote_core.local_peer_id(),
        &remote_addr,
    )?;
    let init_summary = runtime.block_on(crate::local_runtime::init_standard_local_runtime(
        crate::local_runtime::DesktopInitOptions {
            agent_home: agent_home.clone(),
            desktop_paths: desktop_paths.clone(),
            label: unique_label.clone(),
        },
    ))?;
    let peer_id = init_summary.peer_record_id.clone();

    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        desktop_paths,
        bootstrap_live_core_options(),
    ))?;
    if !core.bootstrap_errors().is_empty() {
        anyhow::bail!(
            "unexpected bootstrap errors in live desktop fixture: {:?}",
            core.bootstrap_errors()
        );
    }
    runtime.block_on(wait_for_connected_peer(
        &core,
        remote_core.local_peer_id(),
        "single-agent live desktop bootstrap",
    ))?;
    runtime.block_on(wait_for_connected_peer(
        remote_core.as_ref(),
        core.local_peer_id(),
        "single-agent live runtime bootstrap",
    ))?;
    drop(runtime_api);
    wait_for_live_deployment_docs_in_store(&core, &unique_label, &running_agent.did, &docs)?;

    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let mut app = DesktopApp::from_parts(
        &cc,
        Arc::clone(&runtime),
        Some(Arc::new(core)),
        Vec::new(),
        log_store,
    );
    app.state.activity = Activity::Chat;
    let mut driver = AuditDriver::new(app, ctx);
    wait_for_bootstrap_chat_ready(&mut driver, &peer_id, &running_agent.did)?;

    Ok(LiveDesktopFixture {
        runtime,
        _tempdir: tempdir,
        driver,
        running_agent: Some(running_agent),
        remote_core: Some(remote_core),
        docs,
        backend,
    })
}

pub(crate) fn build_multi_agent_desktop_fixture_with_backend(
    label: &str,
    backend: &AgentBackendConfig,
    log_store: Arc<DesktopLogStore>,
) -> Result<MultiAgentLiveDesktopFixture> {
    init_test_tracing();

    let backend = backend.clone();
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let desktop_paths = DesktopPaths::from_root(tempdir.path().join("desktop"));
    let mut deployments = Vec::new();
    let mut runtime_apis = Vec::new();
    let mut peer_records = Vec::new();

    for suffix in ["alpha", "bravo"] {
        let remote_core = Arc::new(runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path().join(format!("remote-{suffix}"))),
            bootstrap_live_core_options(),
        ))?);

        let unique_label = format!("{label}-{suffix}-{}", uuid::Uuid::new_v4().simple());
        let running_agent = runtime.block_on(spawn_backed_agent(
            remote_core.node_arc(),
            tempdir
                .path()
                .join(format!("agent-{suffix}"))
                .join(format!("{unique_label}.key")),
            &unique_label,
            &backend,
        ))?;
        let docs = runtime.block_on(seed_live_operator_documents(
            remote_core.as_ref(),
            &running_agent.did,
            &unique_label,
            &backend,
        ))?;

        let deployment_label = format!("{} Server", title_case_ascii(suffix));
        let remote_addr = runtime.block_on(wait_for_connectable_iroh_addr(
            remote_core.as_ref(),
            &deployment_label,
        ))?;
        let runtime_api =
            BootstrapRuntimeApi::start(&runtime, Arc::clone(&remote_core), remote_addr.clone())?;
        let mut peer_record =
            crate::client::PeerRecord::new(&deployment_label, &remote_addr, &running_agent.did);
        peer_record.graphql = Some(runtime_api.graphql_url().to_string());
        let peer_id = peer_record.peer_id.clone();
        runtime_apis.push(runtime_api);
        peer_records.push(peer_record);
        deployments.push(LiveRemoteDeployment {
            label: deployment_label,
            peer_id,
            agent_did: running_agent.did.clone(),
            core: remote_core,
            running_agent,
            docs,
        });
    }

    seed_bootstrap_peer_directory(&desktop_paths, &peer_records)?;
    let desktop_core = runtime.block_on(ClientCore::start_with_paths_and_options(
        desktop_paths,
        bootstrap_live_core_options(),
    ))?;
    if !desktop_core.bootstrap_errors().is_empty() {
        anyhow::bail!(
            "unexpected bootstrap errors in multi-agent live desktop fixture: {:?}",
            desktop_core.bootstrap_errors()
        );
    }
    for deployment in &deployments {
        runtime.block_on(wait_for_connected_peer(
            &desktop_core,
            deployment.core.local_peer_id(),
            &format!("desktop -> {}", deployment.label),
        ))?;
        runtime.block_on(wait_for_connected_peer(
            deployment.core.as_ref(),
            desktop_core.local_peer_id(),
            &format!("{} -> desktop", deployment.label),
        ))?;
    }
    drop(runtime_apis);
    for deployment in &deployments {
        wait_for_live_deployment_docs_in_store(
            &desktop_core,
            &deployment.label,
            &deployment.agent_did,
            &deployment.docs,
        )?;
    }

    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let mut app = DesktopApp::from_parts(
        &cc,
        Arc::clone(&runtime),
        Some(Arc::new(desktop_core)),
        Vec::new(),
        log_store,
    );
    app.state.activity = Activity::Chat;
    let mut driver = AuditDriver::new(app, ctx);
    wait_for_bootstrap_chat_rows(&mut driver, &deployments)?;

    Ok(MultiAgentLiveDesktopFixture {
        runtime,
        _tempdir: tempdir,
        driver,
        deployments,
        backend,
    })
}

pub(crate) fn build_multi_agent_live_desktop_fixture(
    label: &str,
    log_store: Arc<DesktopLogStore>,
) -> Result<MultiAgentLiveDesktopFixture> {
    let backend = AgentBackendConfig::live_from_env()?;
    build_multi_agent_desktop_fixture_with_backend(label, &backend, log_store)
}
