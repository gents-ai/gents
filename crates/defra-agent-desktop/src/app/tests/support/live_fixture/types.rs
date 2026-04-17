use super::*;

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
}

impl LiveDesktopFixture {
    pub(crate) fn shutdown(mut self) -> Result<()> {
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
    pub(crate) deployments: Vec<LiveRemoteDeployment>,
    pub(crate) backend: AgentBackendConfig,
}

impl MultiAgentLiveDesktopFixture {
    pub(crate) fn shutdown(mut self) -> Result<()> {
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
