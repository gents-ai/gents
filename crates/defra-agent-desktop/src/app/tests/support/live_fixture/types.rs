use super::*;

fn request_is_terminal(row: &defra_agent_protocol::row::AgentRequestRow) -> bool {
    matches!(
        row.lifecycle_state.as_deref(),
        Some("completed" | "error" | "failed" | "dead" | "superseded")
    )
}

fn response_is_terminal(row: &defra_agent_protocol::row::AgentResponseRow) -> bool {
    matches!(
        row.status.as_deref(),
        Some("complete" | "completed" | "error" | "failed" | "failure")
    )
}

fn request_has_terminal_response(snapshot: &crate::client::ClientStore, request_id: &str) -> bool {
    snapshot
        .latest_response_for_request(request_id)
        .is_some_and(response_is_terminal)
}

fn wait_for_client_quiescence(
    runtime: &Runtime,
    core: &ClientCore,
    label: &str,
    agent_did_scope: Option<&str>,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        runtime.block_on(core.refresh_store())?;
        let snapshot = core.store().snapshot();
        let active_requests = snapshot
            .requests
            .iter()
            .filter(|row| {
                agent_did_scope.is_none_or(|agent_did| row.agent_did.as_deref() == Some(agent_did))
            })
            .filter(|row| {
                !request_is_terminal(row)
                    && !request_has_terminal_response(&snapshot, &row.request_id)
            })
            .count();
        let active_responses = snapshot
            .responses
            .iter()
            .filter(|row| {
                agent_did_scope.is_none_or(|agent_did| row.agent_did.as_deref() == Some(agent_did))
            })
            .filter(|row| !response_is_terminal(row))
            .count();

        if active_requests == 0 && active_responses == 0 {
            return Ok(());
        }

        if Instant::now() >= deadline {
            let active_request_details = snapshot
                .requests
                .iter()
                .filter(|row| {
                    agent_did_scope.is_none_or(|agent_did| row.agent_did.as_deref() == Some(agent_did))
                })
                .filter(|row| {
                    !request_is_terminal(row) && !request_has_terminal_response(&snapshot, &row.request_id)
                })
                .map(|row| {
                    format!(
                        "request_id={} session_id={} lifecycle_state={} status={} agent_did={} failure_reason={} has_terminal_response={}",
                        row.request_id,
                        row.session_id.as_deref().unwrap_or_default(),
                        row.lifecycle_state.as_deref().unwrap_or_default(),
                        row.status.as_deref().unwrap_or_default(),
                        row.agent_did.as_deref().unwrap_or_default(),
                        row.failure_reason.as_deref().unwrap_or_default(),
                        request_has_terminal_response(&snapshot, &row.request_id),
                    )
                })
                .take(5)
                .collect::<Vec<_>>()
                .join(" | ");
            let active_response_details = snapshot
                .responses
                .iter()
                .filter(|row| {
                    agent_did_scope.is_none_or(|agent_did| row.agent_did.as_deref() == Some(agent_did))
                })
                .filter(|row| !response_is_terminal(row))
                .map(|row| {
                    format!(
                        "response_key={} request_id={} session_id={} status={} agent_did={} error_message={}",
                        row.response_key,
                        row.request_id.as_deref().unwrap_or_default(),
                        row.session_id.as_deref().unwrap_or_default(),
                        row.status.as_deref().unwrap_or_default(),
                        row.agent_did.as_deref().unwrap_or_default(),
                        row.error_message.as_deref().unwrap_or_default(),
                    )
                })
                .take(5)
                .collect::<Vec<_>>()
                .join(" | ");
            anyhow::bail!(
                "timed out waiting for live fixture quiescence for {label}; active_requests={active_requests} active_responses={active_responses}; active_request_details=[{active_request_details}] active_response_details=[{active_response_details}]"
            );
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LiveAgentDocs {
    pub(crate) behavior_id: String,
    pub(crate) backend_id: String,
    pub(crate) tool_selection_id: String,
    pub(crate) inference_profile_id: String,
    // The legacy `ScheduledTask` collection split into `Task` + `Schedule`
    // in the event-driven-tasks rework. Live fixtures now seed both and
    // expose both ids so tests can address each surface independently.
    pub(crate) task_id: String,
    pub(crate) schedule_id: String,
}

pub(crate) struct LiveDesktopFixture {
    pub(crate) runtime: Arc<Runtime>,
    pub(crate) _tempdir: tempfile::TempDir,
    pub(crate) driver: AuditDriver,
    pub(crate) running_agent: Option<RunningAgent>,
    pub(crate) remote_core: Option<Arc<ClientCore>>,
    pub(crate) docs: LiveAgentDocs,
    #[allow(dead_code)]
    pub(crate) runtime_apis: Vec<BootstrapRuntimeApi>,
}

impl LiveDesktopFixture {
    pub(crate) fn shutdown(mut self) -> Result<()> {
        tracing::info!("live desktop fixture shutdown: begin");
        for runtime_api in self.runtime_apis.drain(..) {
            runtime_api.shutdown();
        }
        if let Some(remote_core) = self.remote_core.as_ref() {
            tracing::info!("live desktop fixture shutdown: waiting for remote quiescence");
            wait_for_client_quiescence(
                self.runtime.as_ref(),
                remote_core.as_ref(),
                "live remote fixture",
                None,
                Duration::from_secs(5),
            )?;
        }
        if let Some(desktop_core) = self.driver.app.client.as_ref() {
            tracing::info!("live desktop fixture shutdown: waiting for desktop quiescence");
            wait_for_client_quiescence(
                self.runtime.as_ref(),
                desktop_core.as_ref(),
                "live desktop fixture",
                None,
                Duration::from_secs(5),
            )?;
        }
        if let Some(running_agent) = self.running_agent.take() {
            tracing::info!("live desktop fixture shutdown: stopping running agent");
            self.runtime.block_on(running_agent.shutdown())?;
        }
        if let Some(remote_core) = self.remote_core.take() {
            tracing::info!("live desktop fixture shutdown: shutting down remote core");
            self.runtime.block_on(remote_core.shutdown())?;
        }
        tracing::info!("live desktop fixture shutdown: shutting down desktop client");
        self.driver.app.shutdown_client();
        tracing::info!("live desktop fixture shutdown: complete");
        Ok(())
    }
}

pub(crate) struct LiveRemoteDeployment {
    pub(crate) label: String,
    pub(crate) peer_id: String,
    pub(crate) agent_did: String,
    pub(crate) core: Arc<ClientCore>,
    pub(crate) running_agent: RunningAgent,
    pub(crate) docs: LiveAgentDocs,
}

pub(crate) struct MultiAgentLiveDesktopFixture {
    pub(crate) runtime: Arc<Runtime>,
    pub(crate) _tempdir: tempfile::TempDir,
    pub(crate) driver: AuditDriver,
    pub(crate) desktop_api: BootstrapRuntimeApi,
    pub(crate) deployments: Vec<LiveRemoteDeployment>,
    pub(crate) backend: AgentBackendConfig,
    pub(crate) runtime_apis: Vec<BootstrapRuntimeApi>,
}

impl MultiAgentLiveDesktopFixture {
    pub(crate) fn shutdown(mut self) -> Result<()> {
        let started = Instant::now();
        let phase_started = Instant::now();
        self.desktop_api.shutdown();
        tracing::info!(
            elapsed_ms = phase_started.elapsed().as_millis() as u64,
            "multi-agent fixture shutdown: desktop_api stopped"
        );
        let phase_started = Instant::now();
        for runtime_api in self.runtime_apis.drain(..) {
            runtime_api.shutdown();
        }
        tracing::info!(
            elapsed_ms = phase_started.elapsed().as_millis() as u64,
            "multi-agent fixture shutdown: runtime_apis stopped"
        );
        for deployment in &self.deployments {
            let phase_started = Instant::now();
            if let Err(error) = wait_for_client_quiescence(
                self.runtime.as_ref(),
                deployment.core.as_ref(),
                &format!("live remote fixture {}", deployment.label),
                Some(&deployment.agent_did),
                Duration::from_secs(5),
            ) {
                tracing::warn!(
                    deployment = %deployment.label,
                    error = %error,
                    elapsed_ms = phase_started.elapsed().as_millis() as u64,
                    "multi-agent fixture shutdown: remote quiescence timed out; continuing teardown"
                );
            } else {
                tracing::info!(
                    deployment = %deployment.label,
                    elapsed_ms = phase_started.elapsed().as_millis() as u64,
                    "multi-agent fixture shutdown: remote quiescence complete"
                );
            }
        }
        if let Some(desktop_core) = self.driver.app.client.as_ref() {
            let phase_started = Instant::now();
            if let Err(error) = wait_for_client_quiescence(
                self.runtime.as_ref(),
                desktop_core.as_ref(),
                "live desktop fixture",
                None,
                Duration::from_secs(5),
            ) {
                tracing::warn!(
                    error = %error,
                    elapsed_ms = phase_started.elapsed().as_millis() as u64,
                    "multi-agent fixture shutdown: desktop quiescence timed out; continuing teardown"
                );
            } else {
                tracing::info!(
                    elapsed_ms = phase_started.elapsed().as_millis() as u64,
                    "multi-agent fixture shutdown: desktop quiescence complete"
                );
            }
        }
        for deployment in self.deployments.drain(..) {
            let phase_started = Instant::now();
            self.runtime.block_on(deployment.running_agent.shutdown())?;
            tracing::info!(
                deployment = %deployment.label,
                elapsed_ms = phase_started.elapsed().as_millis() as u64,
                "multi-agent fixture shutdown: running agent stopped"
            );
            let phase_started = Instant::now();
            self.runtime.block_on(deployment.core.shutdown())?;
            tracing::info!(
                deployment = %deployment.label,
                elapsed_ms = phase_started.elapsed().as_millis() as u64,
                "multi-agent fixture shutdown: remote core stopped"
            );
        }
        let phase_started = Instant::now();
        self.driver.app.shutdown_client();
        tracing::info!(
            elapsed_ms = phase_started.elapsed().as_millis() as u64,
            "multi-agent fixture shutdown: desktop client stopped"
        );
        tracing::info!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            "multi-agent fixture shutdown: complete"
        );
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct LiveDeploymentCase<'a> {
    pub(crate) label: String,
    pub(crate) peer_id: String,
    pub(crate) agent_did: String,
    pub(crate) docs: LiveAgentDocs,
    pub(crate) remote_core: &'a ClientCore,
}

pub(crate) struct LiveSubmissionCase {
    pub(crate) prompt: String,
    pub(crate) request_id: String,
    pub(crate) effective_request_id: String,
    pub(crate) response: String,
    pub(crate) session_id: String,
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
