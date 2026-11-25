use std::sync::Arc;

use anyhow::Result;
use gents_automation::triggers::runtime_snapshot::ActiveRuntimeSnapshot;
use gents_inference::resolved::ResolvedAgent;
use gents_loop::tool::ToolDyn;
use gents_toolcalls::call::hook::BackgroundToolRegistry;
use gents_tools::surface::{self, ToolRuntimeContext, ToolSurface};
use tokio::sync::watch;

use crate::turn::prompt::LayeredPromptBuilder;

/// What a slot builds from an agent's resolved tool surface: its tools, the
/// preamble that names them, and what the hook binds beside them. A slot
/// builds it once; a request rebinding to a later generation builds the same
/// from that generation's surface.
pub(in crate::runtime::agent) struct SlotTools {
    pub(in crate::runtime::agent) prompt_builder: LayeredPromptBuilder,
    pub(in crate::runtime::agent) preamble: String,
    pub(in crate::runtime::agent) loop_tools: Arc<Vec<Box<dyn ToolDyn>>>,
    pub(in crate::runtime::agent) background_tool_registry: BackgroundToolRegistry,
    pub(in crate::runtime::agent) remote_tools: Option<gents_documents::RemoteTools>,
    pub(in crate::runtime::agent) job_tools:
        rapidhash::RapidHashMap<String, gents_toolcalls::background::job::JobBinding>,
    pub(in crate::runtime::agent) output_obligations:
        Vec<(String, gents_documents::WriteToolOutputObligation)>,
    pub(in crate::runtime::agent) root_execution_guard: Option<surface::RootExecutionGuard>,
    /// The agent targets `agent_new` offers.
    pub(in crate::runtime::agent) agent_targets: Vec<String>,
}

impl SlotTools {
    pub(in crate::runtime::agent) async fn build(
        runtime: &ToolRuntimeContext,
        agent_config: &ResolvedAgent,
        tool_surface: &ToolSurface,
    ) -> Result<Self> {
        let allowed_targets = surface::resolve_agent_target_descriptions(tool_surface);
        let prompt_builder =
            LayeredPromptBuilder::new(agent_config, tool_surface, &allowed_targets);
        let preamble = prompt_builder.preamble().to_string();
        let (mut loop_tools, background_tools) =
            build_slot_tools(&runtime.for_agent(agent_config), tool_surface).await?;
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
        Ok(Self {
            prompt_builder,
            preamble,
            loop_tools: Arc::new(loop_tools),
            background_tool_registry: BackgroundToolRegistry::from_config(
                background_tools,
                tool_surface.background_tools(),
            ),
            remote_tools: tool_surface.remote_tools().cloned(),
            job_tools: gents_toolcalls::background::job::JobBinding::bind_all(
                tool_surface.plugin_resolutions(),
            ),
            output_obligations: tool_surface.output_obligations(),
            root_execution_guard: tool_surface.root_execution_guard().cloned(),
            agent_targets: allowed_targets.into_iter().map(|(name, _)| name).collect(),
        })
    }
}

/// A slot's foreground and background tools, built on one side of an
/// installer's publication: a surface with plugin tools holds the store's
/// shared lease while it builds, waiting for an installer until the
/// publication deadline. A plugin-free surface never waits for the store.
async fn build_slot_tools(
    runtime: &ToolRuntimeContext,
    surface: &ToolSurface,
) -> Result<(Vec<Box<dyn ToolDyn>>, Vec<Box<dyn ToolDyn>>)> {
    let deadline = tokio::time::Instant::now() + surface::PUBLICATION_WAIT;
    let _publication = if surface.has_plugin_tools() {
        surface::publication_guard_until(runtime.plugin_store_pin(), deadline).await?
    } else {
        None
    };
    Ok((
        surface.build_tools_until(runtime, deadline).await?,
        surface.build_tools_until(runtime, deadline).await?,
    ))
}

/// The runtime's active generations, bound once the generation supervisor
/// publishes its first; slots built before then read it later.
#[derive(Clone, Default)]
pub(in crate::runtime::agent) struct ActiveSnapshots(
    Arc<std::sync::OnceLock<watch::Receiver<Arc<ActiveRuntimeSnapshot>>>>,
);

impl ActiveSnapshots {
    pub(in crate::runtime::agent) fn bind(
        &self,
        receiver: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    ) {
        let _ = self.0.set(receiver);
    }

    pub(in crate::runtime::agent) fn receiver(
        &self,
    ) -> Option<watch::Receiver<Arc<ActiveRuntimeSnapshot>>> {
        self.0.get().cloned()
    }
}
