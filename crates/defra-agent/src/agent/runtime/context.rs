use std::collections::HashSet;
use std::sync::Arc;

use crate::llm::tool::ToolDyn;
use anyhow::{Context, Result};
use rig::client::CompletionClient;
use tokio::sync::{mpsc, watch, Mutex, Notify};

use crate::admission::AdmissionRegistry;
use crate::agent::daemon::BehaviorDaemon;
use crate::backend_provider::BackendProviderKind;
use crate::completion_factory::build_admitted_model;
use crate::hook::{BackgroundExecutionRegistry, BackgroundToolRegistry};
use crate::prompt::LayeredPromptBuilder;
use crate::retry::RetryPolicy;
use crate::tool_surface::{self, ToolRuntimeContext, ToolSurface};
use crate::watcher::AgentRequest;

#[derive(Clone)]
pub(super) struct RuntimeContext {
    pub(super) node: Arc<defra_node::EmbeddedNode>,
    pub(super) tool_runtime: ToolRuntimeContext,
    pub(super) admission_registry: AdmissionRegistry,
    pub(super) retry_policy: RetryPolicy,
    pub(super) hook_failure_policy: crate::hook::FailurePolicy,
    pub(super) rendered_request_capture_factory:
        Option<crate::rendered_request::RenderedRequestCaptureFactory>,
    pub(super) background_execution_registry: BackgroundExecutionRegistry,
    pub(super) startup_barrier: Arc<StartupBarrier>,
}

pub(super) struct BehaviorResolution {
    pub(super) behavior_id: String,
    pub(super) rejection_reason: Option<String>,
}

pub struct StartupBarrier {
    pending_behaviors: Mutex<HashSet<String>>,
    notify: Notify,
}

impl StartupBarrier {
    pub(super) fn new(behaviors: &[Arc<crate::config::AgentBehavior>]) -> Self {
        Self {
            pending_behaviors: Mutex::new(
                behaviors
                    .iter()
                    .map(|behavior| behavior.behavior_id.clone())
                    .collect::<HashSet<_>>(),
            ),
            notify: Notify::new(),
        }
    }

    pub async fn mark_behavior_ready(&self, behavior_id: &str) {
        let mut pending = self.pending_behaviors.lock().await;
        let removed = pending.remove(behavior_id);
        let is_empty = pending.is_empty();
        drop(pending);

        if removed && is_empty {
            self.notify.notify_waiters();
        }
    }

    pub(super) async fn wait_ready(&self) {
        loop {
            if self.pending_behaviors.lock().await.is_empty() {
                return;
            }
            self.notify.notified().await;
        }
    }
}

impl RuntimeContext {
    pub(super) async fn run_behavior(
        &self,
        behavior: Arc<crate::config::AgentBehavior>,
        tool_surface: Arc<ToolSurface>,
        request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
        shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        let tool_names = tool_surface.tool_names();
        let api_key = behavior.completion_client_api_key()?;
        let allowed_targets =
            tool_surface::resolve_subagent_target_descriptions(tool_surface.as_ref());
        let prompt_builder =
            LayeredPromptBuilder::new(behavior.as_ref(), tool_surface.as_ref(), &allowed_targets);
        let preamble = prompt_builder.preamble().to_string();
        // Build the inference tool surface (host/MCP/meta/subagent/etc plus, when
        // the behavior has skills, the progressive-disclosure `load_skill` tool —
        // scoped to this behavior's effective skill set + tool ceiling so it never
        // reveals a foreign skill or widens the surface).
        //
        // The owned completion loop (#400) applies its own deadline/cancellation
        // envelope, so these are unwrapped (not `RuntimeManagedTool`).
        let mut loop_tools = tool_surface.build_tools(&self.tool_runtime)?;
        if !behavior.skills.is_empty() {
            let ceiling = crate::skills::skill_tool_ceiling(
                tool_surface.tool_names(),
                tool_surface.allowed_mcp_service_ids(),
                tool_surface.includes_meta_tools(),
            );
            loop_tools.push(Box::new(crate::skills::LoadSkillTool::new(
                behavior.skills.clone(),
                ceiling,
            )));
        }
        let loop_tools = std::sync::Arc::new(loop_tools);
        let background_tool_registry = BackgroundToolRegistry::from_tools(
            tool_surface
                .build_tools(&self.tool_runtime)?
                .into_iter()
                .map(crate::tool_call_lifecycle::runtime::wrap_tool)
                .collect(),
            &tool_surface.background_tools().allowlist,
        );
        tracing::info!(
            behavior_id = %behavior.behavior_id,
            did = %behavior.agent_did(),
            model = %behavior.model_name,
            tools = ?tool_names,
            "building behavior runtime"
        );

        match behavior.backend_provider_kind {
            BackendProviderKind::OpenAiCompatible => {
                let build_context = format!(
                    "building OpenAI-compatible completion client for behavior {} against {}",
                    behavior.behavior_id, behavior.backend_endpoint
                );
                // The owned loop is identical for Responses and Chat Completions;
                // this branch only chooses the OpenAI-compatible wire API.
                if crate::inference_http::force_openai_chat_completions() {
                    let client: rig::providers::openai::CompletionsClient<
                        crate::inference_http::SessionTaggingHttpClient,
                    > = crate::inference_http::build_openai_chat_completions_client(
                        &api_key,
                        &behavior.backend_endpoint,
                        crate::inference_http::SessionTaggingHttpClient::default(),
                    )
                    .with_context(|| build_context.clone())?;
                    self.run_behavior_with_client(
                        behavior,
                        request_rx,
                        shutdown,
                        prompt_builder,
                        preamble,
                        loop_tools.clone(),
                        background_tool_registry,
                        client,
                    )
                    .await
                } else {
                    let client: rig::providers::openai::Client<
                        crate::inference_http::SessionTaggingHttpClient,
                    > = crate::inference_http::build_openai_responses_client(
                        &api_key,
                        &behavior.backend_endpoint,
                        crate::inference_http::SessionTaggingHttpClient::default(),
                        Default::default(),
                    )
                    .with_context(|| build_context.clone())?;
                    self.run_behavior_with_client(
                        behavior,
                        request_rx,
                        shutdown,
                        prompt_builder,
                        preamble,
                        loop_tools.clone(),
                        background_tool_registry,
                        client,
                    )
                    .await
                }
            }
            BackendProviderKind::OpenRouter => {
                let build_context = format!(
                    "building OpenRouter completion client for behavior {} against {}",
                    behavior.behavior_id, behavior.backend_endpoint
                );
                let client: rig::providers::openrouter::Client =
                    rig::providers::openrouter::Client::builder()
                        .api_key(&api_key)
                        .base_url(&behavior.backend_endpoint)
                        .build()
                        .with_context(|| build_context.clone())?;
                self.run_behavior_with_client(
                    behavior,
                    request_rx,
                    shutdown,
                    prompt_builder,
                    preamble,
                    loop_tools.clone(),
                    background_tool_registry,
                    client,
                )
                .await
            }
            BackendProviderKind::ChatGptCodex => {
                let client =
                    crate::chatgpt_codex::build_responses_client(&behavior.backend_endpoint)
                        .await
                        .with_context(|| {
                            format!(
                            "building ChatGPT Codex completion client for behavior {} against {}",
                            behavior.behavior_id, behavior.backend_endpoint
                        )
                        })?;
                self.run_behavior_with_client(
                    behavior,
                    request_rx,
                    shutdown,
                    prompt_builder,
                    preamble,
                    loop_tools.clone(),
                    background_tool_registry,
                    client,
                )
                .await
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_behavior_with_client<C>(
        &self,
        behavior: Arc<crate::config::AgentBehavior>,
        request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
        shutdown: watch::Receiver<bool>,
        prompt_builder: LayeredPromptBuilder,
        preamble: String,
        loop_tools: Arc<Vec<Box<dyn ToolDyn>>>,
        background_tool_registry: BackgroundToolRegistry,
        client: C,
    ) -> Result<()>
    where
        C: CompletionClient,
        C::CompletionModel: 'static,
    {
        let model = Arc::new(build_admitted_model(
            client,
            self.admission_registry.clone(),
            behavior.as_ref(),
        ));
        let mut daemon = BehaviorDaemon::new(
            self.node.clone(),
            behavior,
            model,
            preamble,
            loop_tools,
            prompt_builder,
            self.retry_policy.clone(),
            self.hook_failure_policy,
            self.rendered_request_capture_factory.clone(),
            background_tool_registry,
            self.background_execution_registry.clone(),
            self.startup_barrier.clone(),
        );
        daemon.run(request_rx, shutdown).await
    }
}
