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

fn wait_for_client_quiescence(
    runtime: &Runtime,
    core: &ClientCore,
    label: &str,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        runtime.block_on(core.refresh_store())?;
        let snapshot = core.store().snapshot();
        let active_requests = snapshot
            .requests
            .iter()
            .filter(|row| !request_is_terminal(row))
            .count();
        let active_responses = snapshot
            .responses
            .iter()
            .filter(|row| !response_is_terminal(row))
            .count();

        if active_requests == 0 && active_responses == 0 {
            return Ok(());
        }

        if Instant::now() >= deadline {
            let active_request_details = snapshot
                .requests
                .iter()
                .filter(|row| !request_is_terminal(row))
                .map(|row| {
                    format!(
                        "request_id={} session_id={} lifecycle_state={} status={} agent_did={} failure_reason={}",
                        row.request_id,
                        row.session_id.as_deref().unwrap_or_default(),
                        row.lifecycle_state.as_deref().unwrap_or_default(),
                        row.status.as_deref().unwrap_or_default(),
                        row.agent_did.as_deref().unwrap_or_default(),
                        row.failure_reason.as_deref().unwrap_or_default(),
                    )
                })
                .take(5)
                .collect::<Vec<_>>()
                .join(" | ");
            let active_response_details = snapshot
                .responses
                .iter()
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
    pub(crate) scheduled_task_id: String,
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
        if let Some(remote_core) = self.remote_core.as_ref() {
            tracing::info!("live desktop fixture shutdown: waiting for remote quiescence");
            wait_for_client_quiescence(
                self.runtime.as_ref(),
                remote_core.as_ref(),
                "live remote fixture",
                Duration::from_secs(5),
            )?;
        }
        if let Some(desktop_core) = self.driver.app.client.as_ref() {
            tracing::info!("live desktop fixture shutdown: waiting for desktop quiescence");
            wait_for_client_quiescence(
                self.runtime.as_ref(),
                desktop_core.as_ref(),
                "live desktop fixture",
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
        for deployment in &self.deployments {
            tracing::info!(deployment = %deployment.label, "multi-agent fixture shutdown: waiting for remote quiescence");
            wait_for_client_quiescence(
                self.runtime.as_ref(),
                deployment.core.as_ref(),
                &format!("live remote fixture {}", deployment.label),
                Duration::from_secs(5),
            )?;
        }
        if let Some(desktop_core) = self.driver.app.client.as_ref() {
            tracing::info!("multi-agent fixture shutdown: waiting for desktop quiescence");
            wait_for_client_quiescence(
                self.runtime.as_ref(),
                desktop_core.as_ref(),
                "live desktop fixture",
                Duration::from_secs(5),
            )?;
        }
        for deployment in self.deployments.drain(..) {
            tracing::info!(deployment = %deployment.label, "multi-agent fixture shutdown: stopping running agent");
            self.runtime.block_on(deployment.running_agent.shutdown())?;
            tracing::info!(deployment = %deployment.label, "multi-agent fixture shutdown: shutting down remote core");
            self.runtime.block_on(deployment.core.shutdown())?;
        }
        tracing::info!("multi-agent fixture shutdown: shutting down desktop client");
        self.driver.app.shutdown_client();
        tracing::info!("multi-agent fixture shutdown: complete");
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
