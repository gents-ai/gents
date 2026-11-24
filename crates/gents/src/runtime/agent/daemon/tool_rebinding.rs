use std::sync::Arc;
use std::time::Duration;

use gents_automation::triggers::runtime_snapshot::ActiveRuntimeSnapshot;
use gents_inference::resolved::ResolvedConfiguration;
use gents_loop::loop_stream::{ReboundTools, ToolRebinding};
use gents_loop::tool::{BoxFuture, ToolDyn, ToolRequestContext};
use gents_toolcalls::call::hook::{DefraSessionHook, HookToolBindings};
use gents_tools::surface::ToolRuntimeContext;
use gents_tools::tools_rebinding::{OwnToolsRebinding, ToolsRebinding};

use crate::runtime::agent::runtime::slot_tools::{ActiveSnapshots, SlotTools};

/// How long a config call that committed its own Tools waits for a
/// generation holding the change: the watcher starts reconciling within its
/// debounce ceiling, and a second ceiling covers resolving and activating
/// it. A change still unpublished then applies from the next request.
const REBINDING_WAIT: Duration =
    crate::runtime::agent::runtime::control_watcher::CONTROL_RECONCILE_DEBOUNCE.saturating_mul(2);

/// Lean `RuntimeReconcile.ToolRebinding.Publication`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Publication {
    Older,
    Unavailable,
    Runs,
}

/// Lean `RuntimeReconcile.ToolRebinding.Answer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Answer {
    Rebind,
    Wait,
    Keep,
}

/// Lean `RuntimeReconcile.ToolRebinding.answer`.
fn answer(
    publication: Publication,
    visible: bool,
    versions_current: bool,
    expired: bool,
) -> Answer {
    match publication {
        Publication::Unavailable => Answer::Keep,
        Publication::Runs if visible && versions_current => Answer::Rebind,
        Publication::Runs | Publication::Older if expired => Answer::Keep,
        Publication::Runs | Publication::Older => Answer::Wait,
    }
}

/// What a slot shares with the requests it runs so they can rebind.
#[derive(Clone)]
pub(in crate::runtime::agent) struct RebindingSource {
    pub(in crate::runtime::agent) tool_runtime: ToolRuntimeContext,
    pub(in crate::runtime::agent) active_snapshots: ActiveSnapshots,
}

/// The generation a request's current toolset was built from.
struct Bound {
    generation: u64,
    configuration: ResolvedConfiguration,
    tool_names: Vec<String>,
}

struct Staged {
    rebound: ReboundTools,
    bindings: HookToolBindings,
}

/// One running request's toolset: its config call stages a rebuilt one after
/// committing the request's own Tools, and the loop takes it at the next turn
/// boundary.
pub(super) struct RequestToolRebinding {
    source: RebindingSource,
    agent_did: String,
    request: ToolRequestContext,
    bound: tokio::sync::Mutex<Bound>,
    staged: std::sync::Mutex<Option<Staged>>,
}

impl RequestToolRebinding {
    pub(super) fn new(
        source: RebindingSource,
        agent: &gents_inference::resolved::ResolvedAgent,
        generation: u64,
        tools: &[Box<dyn ToolDyn>],
        request: ToolRequestContext,
    ) -> Self {
        Self {
            source,
            agent_did: agent.agent_did.clone(),
            request,
            bound: tokio::sync::Mutex::new(Bound {
                generation,
                configuration: agent.configuration.clone(),
                tool_names: advertised(tools),
            }),
            staged: std::sync::Mutex::new(None),
        }
    }

    /// The loop's side: the staged toolset, whose hook bindings replace the
    /// request's as the loop takes it.
    pub(super) fn loop_rebinding(self: &Arc<Self>, hook: DefraSessionHook) -> ToolRebinding {
        let rebinding = self.clone();
        Arc::new(move || {
            let staged = rebinding
                .staged
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            let hook = hook.clone();
            Box::pin(async move {
                let staged = staged?;
                hook.rebind_tools(staged.bindings);
                Some(staged.rebound)
            })
        })
    }

    async fn rebind(&self) -> ToolsRebinding {
        let mut bound = self.bound.lock().await;
        if !tools_moved(&bound.configuration).await {
            return ToolsRebinding::NotOwn;
        }
        let Some(mut snapshots) = self.source.active_snapshots.receiver() else {
            return ToolsRebinding::Unpublished;
        };
        let deadline = tokio::time::Instant::now() + REBINDING_WAIT;
        loop {
            let snapshot = snapshots.borrow_and_update().clone();
            let observed = observe(&snapshot, bound.generation, &self.agent_did).await;
            let expired = tokio::time::Instant::now() >= deadline;
            match answer(
                observed.publication,
                observed.visible,
                observed.versions_current,
                expired,
            ) {
                Answer::Rebind => return self.stage(&snapshot, &mut bound).await,
                Answer::Keep => {
                    return observed
                        .unavailable
                        .map_or(ToolsRebinding::Unpublished, |reason| {
                            ToolsRebinding::Unrunnable { reason }
                        })
                }
                Answer::Wait => {
                    tokio::select! {
                        changed = snapshots.changed() => if changed.is_err() {
                            return ToolsRebinding::Unpublished;
                        },
                        () = tokio::time::sleep_until(deadline) => {}
                    }
                }
            }
        }
    }

    /// Builds `snapshot`'s toolset for the agent as its slot does and stages
    /// it with the hook bindings and the configuration check it came with.
    async fn stage(&self, snapshot: &ActiveRuntimeSnapshot, bound: &mut Bound) -> ToolsRebinding {
        let (Some(agent), Some(tool_config), Some(surface)) = (
            snapshot.agents.get(&self.agent_did),
            snapshot.tool_configs.get(&self.agent_did),
            snapshot.tool_surfaces.get(&self.agent_did),
        ) else {
            return ToolsRebinding::Unpublished;
        };
        let tools = match SlotTools::build(&self.source.tool_runtime, agent, surface).await {
            Ok(tools) => tools,
            Err(error) => {
                return ToolsRebinding::Unrunnable {
                    reason: gents_protocol::failure_text::BoundedErrorChain(error.as_ref())
                        .to_string(),
                }
            }
        };
        let names = advertised(&tools.loop_tools);
        let added = difference(&names, &bound.tool_names);
        let removed = difference(&bound.tool_names, &names);
        *self
            .staged
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Staged {
            rebound: ReboundTools {
                tools: gents_loop::tool::bind_request_tools(tools.loop_tools, self.request.clone()),
                preamble: Some(tools.preamble),
                configuration_guard:
                    crate::turn::provider_input::configuration::configuration_guard(
                        &agent.configuration,
                    ),
            },
            bindings: HookToolBindings {
                background_tool_registry: tools.background_tool_registry,
                remote_tools: tools.remote_tools,
                job_tools: tools.job_tools,
                ..Default::default()
            }
            .with_goal_tool_authority(
                tool_config.goal_tools_requested(),
                tool_config.goal_creation_requested(),
            ),
        });
        *bound = Bound {
            generation: snapshot.generation,
            configuration: agent.configuration.clone(),
            tool_names: names,
        };
        ToolsRebinding::Rebound { added, removed }
    }
}

impl OwnToolsRebinding for RequestToolRebinding {
    fn after_tools_commit(&self) -> BoxFuture<'_, ToolsRebinding> {
        Box::pin(self.rebind())
    }
}

/// Whether the Tools document a generation resolved has moved since.
async fn tools_moved(configuration: &ResolvedConfiguration) -> bool {
    match configuration {
        ResolvedConfiguration::Static => false,
        ResolvedConfiguration::Documents(selection) => !matches!(
            selection
                .current_in(gents_store::collection::Collection::Tools)
                .await,
            Ok(true)
        ),
    }
}

struct Observed {
    publication: Publication,
    visible: bool,
    versions_current: bool,
    unavailable: Option<String>,
}

/// What the active generation holds for the agent, read as the agent, as a
/// fresh request's configuration admission reads it.
async fn observe(snapshot: &ActiveRuntimeSnapshot, built: u64, agent_did: &str) -> Observed {
    let observed = |publication, visible, versions_current, unavailable| Observed {
        publication,
        visible,
        versions_current,
        unavailable,
    };
    if snapshot.generation <= built {
        return observed(Publication::Older, false, false, None);
    }
    let Some(agent) = snapshot.agents.get(agent_did) else {
        let reason = snapshot.unavailable_agents.get(agent_did).map_or_else(
            || "the runtime does not run the agent".to_owned(),
            |unavailable| unavailable.rejection_text().to_owned(),
        );
        return observed(Publication::Unavailable, false, false, Some(reason));
    };
    match &agent.configuration {
        ResolvedConfiguration::Static => observed(Publication::Runs, true, true, None),
        ResolvedConfiguration::Documents(selection) => match selection.check_current().await {
            Ok(current) => observed(Publication::Runs, true, current, None),
            Err(_) => observed(Publication::Runs, false, false, None),
        },
    }
}

fn advertised(tools: &[Box<dyn ToolDyn>]) -> Vec<String> {
    tools
        .iter()
        .filter(|tool| !tool.hidden())
        .map(|tool| tool.name())
        .collect()
}

fn difference(of: &[String], without: &[String]) -> Vec<String> {
    of.iter()
        .filter(|name| !without.contains(name))
        .cloned()
        .collect()
}

#[cfg(test)]
#[test]
fn the_rebinding_answer_replays_the_lean_owner() {
    gents_lean_vocab::families::runtime_reconcile_tool_rebinding_answer::replay::<
        Answer,
        Publication,
    >(|i| answer(i.publication, i.visible, i.versions_current, i.expired));
}
