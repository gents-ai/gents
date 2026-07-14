use std::collections::HashSet;
use std::sync::Arc;

use crate::llm::tool::ToolDyn;
use anyhow::{Context, Result};
use rig::client::CompletionClient;
use tokio::sync::{mpsc, watch, Mutex};

use crate::admission::AdmissionRegistry;
use crate::agent::daemon::BehaviorDaemon;
use crate::backend_provider::BackendProviderKind;
use crate::completion_factory::build_admitted_model;
use crate::hook::{BackgroundExecutionRegistry, BackgroundToolRegistry};
use crate::prompt::LayeredPromptBuilder;
use crate::tool_surface::{self, ToolRuntimeContext, ToolSurface};
use crate::watcher::AgentRequest;

#[derive(Clone)]
pub(super) struct RuntimeContext {
    pub(super) node: Arc<defra_node::EmbeddedNode>,
    pub(super) tool_runtime: ToolRuntimeContext,
    pub(super) admission_registry: AdmissionRegistry,
    pub(super) hook_failure_policy: crate::hook::FailurePolicy,
    pub(super) rendered_request_capture_factory:
        Option<crate::rendered_request::RenderedRequestCaptureFactory>,
    pub(super) background_execution_registry: BackgroundExecutionRegistry,
    pub(super) startup_barrier: Arc<StartupBarrier>,
    /// Startup readiness knobs (#559): the per-attempt build timeout below and
    /// the failure budget consumed by the slot loop's demotion policy.
    pub(super) startup_readiness: crate::startup_readiness::StartupReadinessOptions,
    /// Demotion ledger, cleared by the daemon on a successful start so a
    /// demotion racing a late success self-heals.
    pub(super) startup_demotions: Arc<crate::startup_readiness::StartupDemotions>,
}

pub(super) struct BehaviorResolution {
    pub(super) behavior_id: String,
    pub(super) rejection_reason: Option<String>,
}

pub struct StartupBarrier {
    pending_behaviors: Mutex<HashSet<String>>,
    /// The pending count as a watch channel rather than a `Notify`: the old
    /// check-then-`notified()` loop could miss a final `notify_waiters` fired
    /// between the check and the registration, hanging `wait_ready` on an
    /// empty set. A watch receiver observes the latest value by construction,
    /// so the release can never be lost.
    pending_count_tx: watch::Sender<usize>,
}

impl StartupBarrier {
    pub(super) fn new(behaviors: &[Arc<crate::config::AgentBehavior>]) -> Self {
        let pending: HashSet<String> = behaviors
            .iter()
            .map(|behavior| behavior.behavior_id.clone())
            .collect();
        let (pending_count_tx, _) = watch::channel(pending.len());
        Self {
            pending_behaviors: Mutex::new(pending),
            pending_count_tx,
        }
    }

    async fn release(&self, behavior_id: &str) {
        let mut pending = self.pending_behaviors.lock().await;
        if pending.remove(behavior_id) {
            // send_replace, not send: a release that lands before any waiter
            // subscribes must still be stored, or the count freezes at its
            // seeded value and wait_ready never observes zero.
            let _ = self.pending_count_tx.send_replace(pending.len());
        }
    }

    #[cfg(test)]
    pub(in crate::agent) fn ready_for_test() -> Self {
        Self::new(&[])
    }

    pub async fn mark_behavior_ready(&self, behavior_id: &str) {
        self.release(behavior_id).await;
    }

    /// Release a behavior from the barrier WITHOUT claiming it healthy (#559).
    ///
    /// The demotion accounting (reason, counts, router rejection) lives in
    /// `StartupDemotions`; the barrier's only job is to stop waiting so one
    /// persistently un-buildable behavior cannot wedge `Ready` and the trigger
    /// engine forever. The distinction from readiness is carried by the
    /// ledger, exactly as the Lean model separates `ready` from `demoted`
    /// while `released` covers both.
    pub async fn mark_behavior_demoted(&self, behavior_id: &str) {
        self.release(behavior_id).await;
    }

    /// Release a behavior whose slot reconcile retired before it ever started
    /// (config change or removal mid-startup). Without this, retirement
    /// orphaned the pending entry — the second #559 hang path. Mirrors
    /// `RuntimeReconcile.StartupReadiness.retire`.
    pub async fn mark_behavior_superseded(&self, behavior_id: &str) {
        self.release(behavior_id).await;
    }

    /// Whether the barrier is still waiting on this behavior — i.e. it has
    /// neither started once, been demoted, nor been superseded. The slot
    /// failure policy uses this to tell a build failure (pending) from a
    /// post-start crash (released), which keeps today's restart behavior.
    pub async fn is_pending(&self, behavior_id: &str) -> bool {
        self.pending_behaviors.lock().await.contains(behavior_id)
    }

    /// The behaviors the barrier is still waiting on, for watchdog logging.
    pub async fn pending_behaviors(&self) -> Vec<String> {
        let mut pending: Vec<String> = self
            .pending_behaviors
            .lock()
            .await
            .iter()
            .cloned()
            .collect();
        pending.sort();
        pending
    }

    pub(super) async fn wait_ready(&self) {
        let mut rx = self.pending_count_tx.subscribe();
        // `wait_for` inspects the current value first, so a barrier that is
        // already (or was seeded) empty returns immediately.
        let _ = rx.wait_for(|count| *count == 0).await;
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
        // Gate skills on the effective `skills` capability, not just the presence
        // of configured skills: an operator ceiling that denies skills must drop
        // `load_skill` even when the behavior declares skills (the capability is
        // governed, mirroring meta/defra_query).
        if tool_surface.includes_skills() && !behavior.skills.is_empty() {
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
                if behavior.openai_wire_api == crate::OpenAiWireApi::ChatCompletions {
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
                        crate::inference_http::SessionTaggingHttpClient<
                            crate::inference_http::ResponsesNormalizingHttpClient,
                        >,
                    > = crate::inference_http::build_openai_responses_client(
                        &api_key,
                        &behavior.backend_endpoint,
                        crate::inference_http::SessionTaggingHttpClient::new(
                            crate::inference_http::ResponsesNormalizingHttpClient::default(),
                        ),
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
                // The only async client build (DB + network): a hang here would
                // produce no outcome for the slot's build budget, so bound it —
                // the timeout converts a hang into a failed attempt and the
                // demotion machinery covers it (#559).
                let client = tokio::time::timeout(
                    self.startup_readiness.build_timeout,
                    crate::chatgpt_codex::build_responses_client(
                        self.node.clone(),
                        behavior.agent_did(),
                        &behavior.backend_endpoint,
                    ),
                )
                .await
                .map_err(|_| {
                    anyhow::anyhow!(
                        "timed out after {:?} building the ChatGPT Codex completion client",
                        self.startup_readiness.build_timeout
                    )
                })
                .and_then(|result| result)
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
            self.hook_failure_policy,
            self.rendered_request_capture_factory.clone(),
            background_tool_registry,
            self.background_execution_registry.clone(),
            self.startup_barrier.clone(),
            self.startup_demotions.clone(),
        );
        daemon.run(request_rx, shutdown).await
    }
}

#[cfg(test)]
mod startup_barrier_tests {
    use super::*;

    fn behavior(behavior_id: &str) -> Arc<crate::config::AgentBehavior> {
        Arc::new(
            crate::agent::PendingAgentBehavior::new(behavior_id).build_with_identity_for_test(
                crate::KeyIdentity::load_or_create(
                    std::env::temp_dir().join(format!(
                        "barrier-{behavior_id}-{}.key",
                        uuid::Uuid::new_v4()
                    )),
                    None,
                )
                .unwrap(),
            ),
        )
    }

    #[tokio::test]
    async fn empty_seed_is_immediately_ready() {
        let barrier = StartupBarrier::new(&[]);
        tokio::time::timeout(std::time::Duration::from_secs(1), barrier.wait_ready())
            .await
            .expect("an empty barrier must not wait");
    }

    /// The lost-wakeup regression (#559): the final release may land at any
    /// point relative to the waiter. With the watch channel the waiter always
    /// observes the current count, so release-before-wait, release-after-wait,
    /// and anything between all complete.
    #[tokio::test]
    async fn release_before_and_after_wait_both_complete() {
        let barrier = Arc::new(StartupBarrier::new(&[behavior("a"), behavior("b")]));

        // Release one before any waiter exists.
        barrier.mark_behavior_ready("a").await;

        let waiter = {
            let barrier = barrier.clone();
            tokio::spawn(async move { barrier.wait_ready().await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        barrier.mark_behavior_ready("b").await;

        tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("waiter must observe the final release")
            .unwrap();
    }

    /// Every release class frees the barrier: readiness, demotion (budget
    /// spent), and supersession (retired mid-startup). Only readiness claims
    /// health; the others carry their verdict in the demotion ledger.
    #[tokio::test]
    async fn demotion_and_supersession_release_without_readiness() {
        let barrier = Arc::new(StartupBarrier::new(&[
            behavior("healthy"),
            behavior("unbuildable"),
            behavior("retired"),
        ]));

        barrier.mark_behavior_ready("healthy").await;
        barrier.mark_behavior_demoted("unbuildable").await;
        assert!(barrier.is_pending("retired").await);
        barrier.mark_behavior_superseded("retired").await;
        assert!(!barrier.is_pending("retired").await);

        tokio::time::timeout(std::time::Duration::from_secs(1), barrier.wait_ready())
            .await
            .expect("all release classes together must free the barrier");
        assert!(barrier.pending_behaviors().await.is_empty());
    }
}
