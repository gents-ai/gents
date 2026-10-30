use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;

use super::{auxiliary_tasks, AgentDaemon, AgentDaemonInit};
use crate::inference::compaction::{ProviderReductionEngine, ReductionEngine, ReductionOptions};
use crate::inference::llm::rig_compat::ProviderModel;

impl<M> AgentDaemon<M>
where
    M: ProviderModel,
{
    pub(in crate::agent) fn new(init: AgentDaemonInit<M>) -> Result<Self> {
        let AgentDaemonInit {
            node,
            agent_config,
            provider_family,
            model,
            preamble,
            loop_tools,
            prompt_builder,
            hook_failure_policy,
            rendered_request_capture_factory,
            background_tool_registry,
            background_execution_registry,
            startup_barrier,
            runtime_status,
            slot_generation,
            request_admission,
        } = init;
        let mut compaction_config = crate::completion_factory::loop_config(
            agent_config.as_ref(),
            preamble.clone(),
            0,
            crate::inference::rendered_request::CaptureScopeKind::Compaction,
        );
        compaction_config.max_turns = 0;
        let compactor = Arc::new(ProviderReductionEngine::new(
            model.clone(),
            compaction_config,
        ));
        let compaction_options =
            crate::inference::compaction::reduction_options_for_agent(&agent_config)?;

        Ok(Self {
            node,
            agent_config,
            provider_family: provider_family.clone(),
            replay_issuer: None,
            compaction_provider_family: provider_family,
            model,
            preamble,
            loop_tools,
            prompt_builder,
            compactor,
            compaction_options,
            hook_failure_policy,
            rendered_request_capture_factory,
            background_tool_registry,
            background_execution_registry,
            inference_observations: Default::default(),
            remote_tools: None,
            job_tools: Default::default(),
            job_driver: None,
            output_obligations: Arc::new(Vec::new()),
            startup_barrier,
            runtime_status,
            slot_generation,
            operator_tool_root: None,
            approval_home: None,
            request_admission,
            root_execution_guard: None,
            auxiliary_tasks: auxiliary_tasks::AuxiliaryTasks::default(),
        })
    }

    pub(in crate::agent) fn with_inference_observations(
        mut self,
        registry: crate::inference_observation::InferenceObservationRegistry,
    ) -> Self {
        self.inference_observations = registry;
        self
    }

    pub(in crate::agent) fn with_compactor(
        mut self,
        compactor: Arc<dyn ReductionEngine>,
        provider_family: String,
    ) -> Self {
        self.compactor = compactor;
        self.compaction_provider_family = Some(provider_family);
        self
    }

    pub(in crate::agent) fn with_replay_issuer(
        mut self,
        issuer: Option<gents_loop::claude_messages_body::ReplayIssuer>,
    ) -> Self {
        self.replay_issuer = issuer;
        self
    }

    pub(in crate::agent) fn with_operator_tool_root(mut self, root: Option<PathBuf>) -> Self {
        crate::workspace::install_process_operator_tool_root(root.clone());
        self.operator_tool_root = root;
        self
    }

    pub(in crate::agent) fn with_approval_home(mut self, home: Option<PathBuf>) -> Self {
        self.approval_home = home;
        self
    }

    pub(in crate::agent) fn with_root_execution_guard(
        mut self,
        guard: Option<crate::tool_surface::RootExecutionGuard>,
    ) -> Self {
        self.root_execution_guard = guard;
        self
    }

    pub(in crate::agent) fn with_tool_surface_runtime_policy(
        self,
        guard: Option<crate::tool_surface::RootExecutionGuard>,
        operator_root: Option<PathBuf>,
    ) -> Self {
        self.with_root_execution_guard(guard)
            .with_operator_tool_root(operator_root)
    }

    pub(in crate::agent) fn compaction_options_for_request(
        &self,
        deadline: Option<chrono::DateTime<chrono::Utc>>,
        aggregate_token_budget: Option<crate::agent::loop_stream::AggregateTokenBudget>,
        sampling_seed: Option<i64>,
        context_calibration: crate::provider_input::budget::ContextCalibration,
    ) -> ReductionOptions {
        ReductionOptions {
            deadline,
            aggregate_token_budget,
            sampling_seed,
            context_calibration: Some(context_calibration),
            ..self.compaction_options.clone()
        }
    }

    pub(in crate::agent) fn with_remote_tools(
        mut self,
        tools: Option<crate::document_config::RemoteTools>,
    ) -> Self {
        self.remote_tools = tools;
        self
    }

    pub(in crate::agent) fn with_job_tools(
        mut self,
        job_tools: rapidhash::RapidHashMap<String, crate::plugin_job::JobBinding>,
    ) -> Self {
        self.job_tools = job_tools;
        self
    }

    pub(in crate::agent) fn with_job_driver(
        mut self,
        driver: Arc<crate::plugin_job::driver::JobDriver>,
    ) -> Self {
        self.job_driver = Some(driver);
        self
    }

    pub(in crate::agent) fn with_output_obligations(
        mut self,
        obligations: Vec<(String, crate::document_config::WriteToolOutputObligation)>,
    ) -> Self {
        self.output_obligations = Arc::new(obligations);
        self
    }
}
