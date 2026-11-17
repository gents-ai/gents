use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use gents_inference::llm::rig_compat::ProviderClient;
use gents_loop::tool::ToolDyn;
use tokio::sync::{mpsc, watch, Mutex};

use crate::runtime::agent::daemon::{AgentDaemon, AgentDaemonInit};
use crate::turn::completion_factory::build_admitted_model;
use crate::turn::prompt::LayeredPromptBuilder;
use gents_inference::admission::AdmissionRegistry;
use gents_protocol::row::AgentRequest;
use gents_toolcalls::call::hook::{BackgroundExecutionRegistry, BackgroundToolRegistry};
use gents_tools::surface::{self, ToolRuntimeContext, ToolSurface};

#[derive(Clone)]
pub(super) struct RuntimeContext {
    pub(super) document_context: Option<crate::runtime::agent::DocumentResolveContext>,
    pub(super) node: Arc<defra_node::Node>,
    pub(super) tool_runtime: ToolRuntimeContext,
    pub(super) admission_registry: AdmissionRegistry,
    pub(super) backend_health: gents_inference::backends::health::BackendHealthMap,
    pub(super) hook_failure_policy: gents_toolcalls::call::hook::FailurePolicy,
    pub(super) rendered_request_capture_factory:
        Option<gents_inference::rendered_request::RenderedRequestCaptureFactory>,
    pub(super) background_execution_registry: BackgroundExecutionRegistry,
    pub(super) inference_observations:
        gents_inference::inference_observation::InferenceObservationRegistry,
    pub(super) startup_barrier: Arc<StartupBarrier>,
    pub(super) startup_readiness: crate::runtime::startup_readiness::StartupReadinessOptions,
    pub(super) runtime_status: crate::runtime::status::RuntimeStatusHandle,
    pub(super) operator_tool_root: Option<PathBuf>,
    /// The runtime's single job driver, created once in `run_agent_owned`.
    pub(super) job_driver: Arc<gents_toolcalls::background::job::driver::JobDriver>,
    pub(super) enrollment_authority: gents_p2p::p2p_reconcile::EnrollmentAuthorityHandle,
}

pub(super) struct AgentResolution {
    pub(super) agent_did: String,
    pub(super) rejection_reason: Option<String>,
}

pub struct StartupBarrier {
    pending_standings: Mutex<BTreeSet<(String, u64)>>,
    pending_count_tx: watch::Sender<usize>,
}

impl StartupBarrier {
    pub(super) fn new(agents: &[Arc<gents_inference::resolved::ResolvedAgent>]) -> Self {
        let pending: BTreeSet<(String, u64)> = agents
            .iter()
            .map(|agent_config| (agent_config.agent_did.clone(), 1))
            .collect();
        let (pending_count_tx, _) = watch::channel(pending.len());
        Self {
            pending_standings: Mutex::new(pending),
            pending_count_tx,
        }
    }

    async fn release(&self, agent_did: &str, generation: u64) -> bool {
        let mut pending = self.pending_standings.lock().await;
        if pending.remove(&(agent_did.to_string(), generation)) {
            let _ = self.pending_count_tx.send_replace(pending.len());
            true
        } else {
            false
        }
    }

    pub async fn register_agent(&self, agent_did: &str, generation: u64) {
        let mut pending = self.pending_standings.lock().await;
        if pending.insert((agent_did.to_string(), generation)) {
            self.pending_count_tx.send_replace(pending.len());
        }
    }

    #[cfg(test)]
    pub(in crate::runtime::agent) fn ready_for_test() -> Self {
        Self::new(&[])
    }

    pub async fn mark_agent_ready(&self, agent_did: &str, generation: u64) -> bool {
        self.release(agent_did, generation).await
    }

    pub async fn mark_agent_demoted(&self, agent_did: &str, generation: u64) -> bool {
        self.release(agent_did, generation).await
    }

    pub async fn mark_agent_superseded(&self, agent_did: &str, generation: u64) -> bool {
        self.release(agent_did, generation).await
    }

    pub async fn is_pending(&self, agent_did: &str, generation: u64) -> bool {
        self.pending_standings
            .lock()
            .await
            .contains(&(agent_did.to_string(), generation))
    }

    pub async fn pending_agents(&self) -> Vec<String> {
        self.pending_standings
            .lock()
            .await
            .iter()
            .map(|(agent_did, generation)| format!("{agent_did}@{generation}"))
            .collect()
    }

    pub(super) async fn wait_ready(&self) {
        let mut rx = self.pending_count_tx.subscribe();
        let _ = rx.wait_for(|count| *count == 0).await;
    }
}

impl RuntimeContext {
    pub(super) async fn run_agent(
        &self,
        agent: gents_tools::surface::AgentWithTools,
        tool_surface: Arc<ToolSurface>,
        request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
        capacity: Arc<crate::runtime::agent::worker_capacity::WorkerCapacity>,
        slot_generation: u64,
        shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        let mut cancel = shutdown.clone();
        if *cancel.borrow() {
            return Ok(());
        }
        let prepared = tokio::select! {
            biased;
            _ = cancel.changed() => return Ok(()),
            result = build_slot_surface_fenced(
                &self.tool_runtime,
                self.document_context.as_ref(),
                agent,
                tool_surface,
            ) => result?,
        };
        self.run_agent_inner(prepared, request_rx, capacity, slot_generation, shutdown)
            .await
    }

    async fn run_agent_inner(
        &self,
        prepared: PreparedSlotSurface,
        request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
        capacity: Arc<crate::runtime::agent::worker_capacity::WorkerCapacity>,
        slot_generation: u64,
        shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        let PreparedSlotSurface {
            agent,
            surface: tool_surface,
            tools: mut loop_tools,
            background,
        } = prepared;
        let gents_tools::surface::AgentWithTools {
            agent: agent_config,
            tools: tool_config,
        } = agent;
        let tool_names = tool_surface.tool_names();
        let api_key = if agent_config.backend_auth.uses_node_oauth()
            || matches!(
                &agent_config.backend_auth,
                gents_documents::BackendAuth::Credential { .. }
            ) {
            "no-key".to_owned()
        } else {
            agent_config.completion_client_api_key()?
        };
        let allowed_targets = surface::resolve_agent_target_descriptions(tool_surface.as_ref());
        let prompt_builder = LayeredPromptBuilder::new(
            agent_config.as_ref(),
            tool_surface.as_ref(),
            &allowed_targets,
            &loop_tools,
        );
        let preamble = prompt_builder.preamble().to_string();
        if tool_surface.includes_skills() && !agent_config.skills.is_empty() {
            let ceiling = gents_documents::skills::skill_tool_ceiling(
                tool_surface.tool_names(),
                tool_surface.allowed_mcp_service_ids(),
                tool_surface.includes_meta_tools(),
            );
            loop_tools.push(Box::new(
                gents_tools::native::skills::LoadSkillTool::for_agent(
                    agent_config.skills.clone(),
                    ceiling,
                    &agent_config.configuration,
                ),
            ));
        }
        let loop_tools = std::sync::Arc::new(loop_tools);
        let background_tool_registry =
            BackgroundToolRegistry::from_config(background, tool_surface.background_tools());
        tracing::info!(
            agent_did = %agent_config.agent_did,
            did = %agent_config.node_did(),
            model = %agent_config.model_name,
            tools = ?tool_names,
            "building agent runtime"
        );

        let client = gents_inference::llm::backend_client::build_backend_client(
            self.tool_runtime.usage_cache.clone(),
            self.node.clone(),
            agent_config.as_ref(),
            &api_key,
            self.startup_readiness.build_timeout,
        )
        .await?;
        let provider_family = Some(client.provider_family().to_owned());
        let replay_issuer = client.replay_issuer()?;
        let account_hops = client.account_hops();

        gents_inference::llm::backend_client::with_backend_client!(client, |client| {
            Box::pin(self.run_agent_with_client(
                agent_config,
                tool_config,
                provider_family,
                replay_issuer,
                account_hops,
                request_rx,
                capacity,
                slot_generation,
                shutdown,
                prompt_builder,
                preamble,
                loop_tools.clone(),
                background_tool_registry,
                tool_surface.remote_tools().cloned(),
                gents_toolcalls::background::job::JobBinding::bind_all(
                    tool_surface.plugin_resolutions(),
                ),
                tool_surface.output_obligations(),
                tool_surface.root_execution_guard().cloned(),
                client,
            ))
            .await
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_agent_with_client<C>(
        &self,
        agent_config: Arc<gents_inference::resolved::ResolvedAgent>,
        tool_config: Arc<gents_tools::surface::AgentToolSurfaceConfig>,
        provider_family: Option<String>,
        replay_issuer: Option<gents_loop::claude_messages_body::ReplayIssuer>,
        account_hops: u32,
        request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
        capacity: Arc<crate::runtime::agent::worker_capacity::WorkerCapacity>,
        slot_generation: u64,
        shutdown: watch::Receiver<bool>,
        prompt_builder: LayeredPromptBuilder,
        preamble: String,
        loop_tools: Arc<Vec<Box<dyn ToolDyn>>>,
        background_tool_registry: BackgroundToolRegistry,
        remote_tools: Option<gents_documents::RemoteTools>,
        job_tools: rapidhash::RapidHashMap<String, gents_toolcalls::background::job::JobBinding>,
        output_obligations: Vec<(String, gents_documents::WriteToolOutputObligation)>,
        root_execution_guard: Option<gents_tools::surface::RootExecutionGuard>,
        client: C,
    ) -> Result<()>
    where
        C: ProviderClient,
    {
        let model = Arc::new(build_admitted_model(
            client,
            self.admission_registry.clone(),
            agent_config.as_ref(),
        ));
        let request_admission =
            gents_session::request::admission::AgentRequestAdmissionVerifier::new(
                self.node.clone(),
                agent_config.node_identity().clone(),
                self.enrollment_authority.clone(),
            );
        let summary_compactor = crate::turn::completion_factory::build_compaction_engine(
            self.tool_runtime.usage_cache.clone(),
            self.node.clone(),
            &agent_config,
            self.admission_registry.clone(),
            self.startup_readiness.build_timeout,
        )
        .await?;
        let capture_factory = match &self.rendered_request_capture_factory {
            Some(factory) => factory.clone(),
            None => {
                gents_inference::rendered_request::defra_rendered_request_capture_factory_for_agent(
                    self.node.clone(),
                    agent_config.agent_identity.as_ref(),
                )?
            }
        };
        let mut daemon = AgentDaemon::new(AgentDaemonInit {
            node: self.node.clone(),
            agent_config,
            tool_config,
            provider_family,
            model,
            preamble,
            loop_tools,
            prompt_builder,
            hook_failure_policy: self.hook_failure_policy,
            rendered_request_capture_factory: Some(capture_factory),
            background_tool_registry,
            background_execution_registry: self.background_execution_registry.clone(),
            startup_barrier: self.startup_barrier.clone(),
            runtime_status: self.runtime_status.clone(),
            slot_generation,
            request_admission,
        })?
        .with_inference_observations(self.inference_observations.clone())
        .with_backend_health(self.backend_health.clone())
        .with_replay_issuer(replay_issuer)
        .with_account_hops(account_hops)
        .with_approval_home(
            self.tool_runtime
                .plugin_home()
                .map(std::path::Path::to_path_buf),
        )
        .with_remote_tools(remote_tools)
        .with_job_tools(job_tools)
        .with_job_driver(self.job_driver.clone())
        .with_output_obligations(output_obligations)
        .with_tool_surface_runtime_policy(root_execution_guard, self.operator_tool_root.clone());
        if let Some((compactor, provider_family)) = summary_compactor {
            daemon = daemon.with_compactor(compactor, provider_family);
        }
        daemon.run(request_rx, capacity, shutdown).await
    }
}

/// Slot construction can race an install after its proposal was resolved.
/// Refresh once, under the fence, on the typed mismatch. Never reinterpret a
/// mismatch as permission to run the installed digest with the old pin.
pub(in crate::runtime::agent::runtime) struct PreparedSlotSurface {
    pub(in crate::runtime::agent::runtime) agent: surface::AgentWithTools,
    pub(in crate::runtime::agent::runtime) surface: Arc<ToolSurface>,
    pub(in crate::runtime::agent::runtime) tools: Vec<Box<dyn ToolDyn>>,
    pub(in crate::runtime::agent::runtime) background: Vec<Box<dyn ToolDyn>>,
}

pub(in crate::runtime::agent::runtime) async fn build_slot_surface_fenced(
    runtime: &ToolRuntimeContext,
    resolve: Option<&crate::runtime::agent::DocumentResolveContext>,
    mut agent: surface::AgentWithTools,
    mut tool_surface: Arc<ToolSurface>,
) -> Result<PreparedSlotSurface> {
    let _guard = if needs_publication_fence(tool_surface.has_plugin_tools()) {
        surface::pinned_publication_guard_until(
            runtime.plugin_store_pin(),
            tokio::time::Instant::now() + std::time::Duration::from_secs(60),
        )
        .await?
    } else {
        None
    };
    let mut refreshed = false;
    loop {
        let tools = tool_surface
            .build_tools(&runtime.for_agent(&agent.agent))
            .await;
        match tools {
            Ok(tools) => {
                let background = tool_surface
                    .build_tools(&runtime.for_agent(&agent.agent))
                    .await?;
                return Ok(PreparedSlotSurface {
                    agent,
                    surface: tool_surface,
                    tools,
                    background,
                });
            }
            Err(error)
                if !refreshed
                    && gents_toolcalls::plugin::executor::PluginResolutionError::error_chain_is_pin_mismatch(&error) =>
            {
                let Some(resolve) = resolve else {
                    return Err(error);
                };
                refreshed = true;
                let view = super::super::document_view::load_document_runtime_view_as(
                    runtime.node(),
                    agent.agent.node_did(),
                    resolve.identity.did(),
                )
                .await?;
                let mut snapshot =
                    super::super::document_view::resolve_document_runtime_snapshot_from_view(
                        runtime.node(),
                        resolve,
                        &view,
                    )
                    .await?;
                let did = &agent.agent.agent_did;
                let current = snapshot.agents.remove(did).ok_or_else(|| {
                    anyhow::anyhow!(
                    "agent {did} was revoked or became unavailable while refreshing a stale pin"
                )
                })?;
                let tools = snapshot
                    .tool_configs
                    .remove(did)
                    .ok_or_else(|| anyhow::anyhow!("current Tools missing for {did}"))?;
                tool_surface = snapshot
                    .tool_surfaces
                    .remove(did)
                    .ok_or_else(|| anyhow::anyhow!("current surface missing for {did}"))?;
                agent = surface::AgentWithTools {
                    agent: current,
                    tools,
                };
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod startup_barrier_tests {
    use super::*;
    use gents_lean_vocab::families::runtime_startup_barrier_obligations as family;

    fn agent_config(agent_did: &str) -> Arc<gents_inference::resolved::ResolvedAgent> {
        let mut agent = crate::runtime::agent::PendingAgent::new(agent_did)
            .build_with_identity_for_test(
                gents_store::identity::KeyIdentity::load_or_create(
                    std::env::temp_dir()
                        .join(format!("barrier-{agent_did}-{}.key", uuid::Uuid::new_v4())),
                    None,
                )
                .unwrap(),
            )
            .agent;
        Arc::make_mut(&mut agent).agent_did = agent_did.to_owned();
        agent
    }

    /// Readiness as seen by a waiter that was already waiting when the last
    /// operation ran; a completed waiter is replaced by a fresh one.
    fn waiter_ready<'a>(
        barrier: &'a StartupBarrier,
        waiter: &mut Option<futures::future::LocalBoxFuture<'a, ()>>,
    ) -> bool {
        let pending = waiter.get_or_insert_with(|| Box::pin(barrier.wait_ready()));
        let ready = futures::FutureExt::now_or_never(pending.as_mut()).is_some();
        if ready {
            *waiter = None;
        }
        ready
    }

    async fn observation<'a>(
        barrier: &'a StartupBarrier,
        waiter: &mut Option<futures::future::LocalBoxFuture<'a, ()>>,
        released: Option<bool>,
    ) -> family::Observation {
        family::Observation {
            pending: barrier
                .pending_standings
                .lock()
                .await
                .iter()
                .map(|(agent, generation)| family::Slot {
                    agent: agent.clone(),
                    generation: *generation,
                })
                .collect(),
            ready: waiter_ready(barrier, waiter),
            released,
        }
    }

    #[tokio::test]
    async fn generated_startup_obligations_release_only_the_selected_generation() {
        for case in family::cases::<String>() {
            let agents = case
                .input
                .seed
                .iter()
                .map(|agent| agent_config(agent))
                .collect::<Vec<_>>();
            let barrier = StartupBarrier::new(&agents);
            let mut waiter = None;
            let mut observed = vec![observation(&barrier, &mut waiter, None).await];
            for action in &case.input.actions {
                let slot = &action.slot;
                let released = match action.operation.as_str() {
                    "register" => {
                        barrier.register_agent(&slot.agent, slot.generation).await;
                        None
                    }
                    "ready" => Some(barrier.mark_agent_ready(&slot.agent, slot.generation).await),
                    "demote" => Some(
                        barrier
                            .mark_agent_demoted(&slot.agent, slot.generation)
                            .await,
                    ),
                    "supersede" => Some(
                        barrier
                            .mark_agent_superseded(&slot.agent, slot.generation)
                            .await,
                    ),
                    other => panic!("unknown modeled startup operation {other}"),
                };
                observed.push(observation(&barrier, &mut waiter, released).await);
            }
            case.check(observed);
        }
    }
}

pub(in crate::runtime::agent::runtime) fn needs_publication_fence(has_plugins: bool) -> bool {
    has_plugins
}

#[cfg(test)]
#[test]
fn plugin_free_surface_fencing_replays_the_lean_owner() {
    gents_lean_vocab::families::runtime_reconcile_surface_publication_needs_publication_fence::replay(|i| needs_publication_fence(i.has_plugins));
}
