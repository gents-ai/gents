use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::Serialize;
use tokio::sync::mpsc;

use crate::documents::automation::{
    ResolvedAutomation, ResolvedEventTrigger, ResolvedSchedule, ResolvedTask,
};
pub use crate::documents::ConcurrencyMode;
pub use crate::documents::ScheduleCadence;
use crate::inference::admission::BackendAdmissionConfig;
use crate::inference::resolved::ResolvedAgent;
use crate::store::identity::RuntimeNode;
use crate::tools::surface::ToolSurface;
use gents_protocol::node_readiness::AgentReadinessUnavailableReason;
use gents_protocol::node_readiness::{
    effective_agent_readiness_admission, EffectiveAgentReadinessAdmission,
};

pub type DispatcherMap = RapidHashMap<String, mpsc::Sender<AgentRequest>>;

use gents_protocol::row::AgentRequest;
#[derive(Clone)]
pub(crate) struct ResolvedWatcherIdentity(pub(crate) Arc<dyn crate::store::identity::NodeIdentity>);

impl std::fmt::Debug for ResolvedWatcherIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ResolvedWatcherIdentity")
            .field(&self.0.did())
            .finish()
    }
}

impl PartialEq for ResolvedWatcherIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.0.did() == other.0.did()
    }
}
impl Eq for ResolvedWatcherIdentity {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnavailableAgent {
    pub public_reason: AgentReadinessUnavailableReason,
    pub diagnostic: String,
    pub(crate) watcher_identity: Option<ResolvedWatcherIdentity>,
}

impl UnavailableAgent {
    pub fn new(
        public_reason: AgentReadinessUnavailableReason,
        diagnostic: impl Into<String>,
    ) -> Self {
        Self {
            public_reason,
            diagnostic: diagnostic.into(),
            watcher_identity: None,
        }
    }

    pub(crate) fn with_watcher_identity(
        mut self,
        identity: Arc<dyn crate::store::identity::NodeIdentity>,
    ) -> Self {
        self.watcher_identity = Some(ResolvedWatcherIdentity(identity));
        self
    }

    pub fn public_message(&self) -> &'static str {
        self.public_reason.public_message()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EffectiveAgentAdmission<'a> {
    Ready,
    Unavailable {
        public_reason: AgentReadinessUnavailableReason,
        diagnostic: &'a str,
    },
    Unassigned,
}

pub(crate) fn effective_agent_admission<'a>(
    dispatcher_present: bool,
    unavailable: Option<&'a UnavailableAgent>,
    startup_diagnostic: Option<&'a str>,
) -> EffectiveAgentAdmission<'a> {
    match effective_agent_readiness_admission(
        dispatcher_present,
        unavailable.map(|unavailable| unavailable.public_reason),
        startup_diagnostic.is_some(),
    ) {
        EffectiveAgentReadinessAdmission::Ready => EffectiveAgentAdmission::Ready,
        EffectiveAgentReadinessAdmission::Unavailable(public_reason) => {
            EffectiveAgentAdmission::Unavailable {
                public_reason,
                diagnostic: startup_diagnostic
                    .or_else(|| unavailable.map(|unavailable| unavailable.diagnostic.as_str()))
                    .expect("unavailable admission has a diagnostic source"),
            }
        }
        EffectiveAgentReadinessAdmission::Unassigned => EffectiveAgentAdmission::Unassigned,
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedRuntimeSnapshot {
    pub(crate) node: Option<Arc<RuntimeNode>>,
    pub(crate) local_did: String,
    pub(crate) default_agent_did: String,
    pub(crate) agents: RapidHashMap<String, Arc<ResolvedAgent>>,
    pub(crate) tool_surfaces: RapidHashMap<String, Arc<ToolSurface>>,
    pub(crate) backend_admission_configs: RapidHashMap<String, BackendAdmissionConfig>,
    pub(crate) unavailable_agents: RapidHashMap<String, UnavailableAgent>,
    pub(crate) active_schedules: RapidHashMap<String, ResolvedSchedule>,
    pub(crate) unavailable_schedules: RapidHashSet<String>,
    pub(crate) active_event_triggers: RapidHashMap<String, ResolvedEventTrigger>,
    pub(crate) unavailable_event_triggers: RapidHashSet<String>,
    pub(crate) active_tasks: RapidHashMap<String, ResolvedTask>,
}

impl ResolvedRuntimeSnapshot {
    pub(crate) fn validate_node_readiness_source(&self) -> Result<()> {
        let canonical = |value: &str| !value.is_empty() && value == value.trim();
        if !canonical(&self.default_agent_did) {
            anyhow::bail!(
                "default agent {:?} is not a canonical agent identifier",
                self.default_agent_did
            );
        }
        if self
            .agents
            .keys()
            .chain(self.unavailable_agents.keys())
            .any(|agent_did| !canonical(agent_did))
        {
            anyhow::bail!("runtime agent identifiers must be non-empty and trimmed");
        }
        let runnable_agent_dids = self.agents.keys().collect::<BTreeSet<_>>();
        let tool_surface_agent_dids = self.tool_surfaces.keys().collect::<BTreeSet<_>>();
        if runnable_agent_dids != tool_surface_agent_dids {
            let missing = runnable_agent_dids
                .difference(&tool_surface_agent_dids)
                .map(|agent_did| agent_did.as_str())
                .collect::<Vec<_>>();
            let extra = tool_surface_agent_dids
                .difference(&runnable_agent_dids)
                .map(|agent_did| agent_did.as_str())
                .collect::<Vec<_>>();
            anyhow::bail!(
                "runnable agent/tool-surface keysets differ (missing={missing:?}, extra={extra:?})"
            );
        }
        if !self.agents.contains_key(&self.default_agent_did)
            && !self
                .unavailable_agents
                .contains_key(&self.default_agent_did)
        {
            anyhow::bail!(
                "default agent {:?} is not assigned to the resolved runtime",
                self.default_agent_did
            );
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn from_parts(
        default_agent_did: String,
        agents: Vec<Arc<ResolvedAgent>>,
        tool_surfaces: RapidHashMap<String, Arc<ToolSurface>>,
        unavailable_agents: RapidHashMap<String, UnavailableAgent>,
    ) -> Self {
        Self::from_parts_with_admission_configs(
            default_agent_did,
            agents,
            tool_surfaces,
            RapidHashMap::new(),
            unavailable_agents,
        )
    }

    pub(crate) fn from_parts_with_admission_configs(
        default_agent_did: String,
        agents: Vec<Arc<ResolvedAgent>>,
        tool_surfaces: RapidHashMap<String, Arc<ToolSurface>>,
        backend_admission_configs: RapidHashMap<String, BackendAdmissionConfig>,
        unavailable_agents: RapidHashMap<String, UnavailableAgent>,
    ) -> Self {
        Self {
            node: None,
            local_did: String::new(),
            default_agent_did,
            agents: agents
                .into_iter()
                .map(|agent| (agent.agent_did.clone(), agent))
                .collect(),
            tool_surfaces,
            backend_admission_configs,
            unavailable_agents,
            active_schedules: RapidHashMap::new(),
            unavailable_schedules: RapidHashSet::new(),
            active_event_triggers: RapidHashMap::new(),
            unavailable_event_triggers: RapidHashSet::new(),
            active_tasks: RapidHashMap::new(),
        }
    }

    pub(crate) fn with_node(mut self, node: Arc<RuntimeNode>) -> Self {
        self.node = Some(node);
        self
    }

    pub(crate) fn with_local_did(mut self, local_did: String) -> Self {
        self.local_did = local_did;
        self
    }

    pub(crate) fn with_automation(mut self, automation: ResolvedAutomation) -> Self {
        self.active_tasks = automation.tasks;
        self.active_schedules = automation.schedules;
        self.unavailable_schedules = automation.unavailable_schedules;
        self.active_event_triggers = automation.event_triggers;
        self.unavailable_event_triggers = automation.unavailable_event_triggers;
        self
    }

    /// Applies the unavailable-agent cascades that resolution runs for agents it
    /// rejects: drop local agent targets of the agent, and mark its tasks,
    /// schedules and event triggers unavailable.
    pub(crate) fn demote_agent(&mut self, agent_did: &str, reason: UnavailableAgent) {
        let identity = self
            .agents
            .remove(agent_did)
            .map(|agent| agent.agent_identity.clone());
        self.tool_surfaces.remove(agent_did);
        let reason = match identity {
            Some(identity) => reason.with_watcher_identity(identity),
            None => reason,
        };
        self.unavailable_agents
            .insert(agent_did.to_string(), reason);

        let active_agent_dids = self.agents.keys().cloned().collect::<RapidHashSet<_>>();
        for surface in self.tool_surfaces.values_mut() {
            Arc::make_mut(surface).retain_agent_targets(&self.local_did, &active_agent_dids);
        }
        self.active_tasks
            .retain(|_, task| task.agent_did != agent_did);
        self.active_schedules.retain(|trigger_id, schedule| {
            let keep = schedule.task.agent_did != agent_did;
            if !keep {
                self.unavailable_schedules.insert(trigger_id.clone());
            }
            keep
        });
        self.active_event_triggers.retain(|trigger_id, event| {
            let keep = event.task.agent_did != agent_did;
            if !keep {
                self.unavailable_event_triggers.insert(trigger_id.clone());
            }
            keep
        });
    }

    #[cfg(test)]
    pub(crate) fn activate(
        self,
        generation: u64,
        dispatchers: DispatcherMap,
    ) -> ActiveRuntimeSnapshot {
        let agent_executor_capacities = dispatchers
            .keys()
            .map(|agent_did| (agent_did.clone(), 1))
            .collect();
        let agent_executor_queue_capacities = dispatchers
            .iter()
            .map(|(agent_did, dispatcher)| (agent_did.clone(), dispatcher.max_capacity()))
            .collect();
        self.activate_with_executor_metadata(
            generation,
            dispatchers,
            agent_executor_capacities,
            agent_executor_queue_capacities,
        )
    }

    pub(crate) fn activate_with_executor_metadata(
        self,
        generation: u64,
        dispatchers: DispatcherMap,
        agent_executor_capacities: RapidHashMap<String, usize>,
        agent_executor_queue_capacities: RapidHashMap<String, usize>,
    ) -> ActiveRuntimeSnapshot {
        debug_assert!(
            self.node.is_some(),
            "ResolvedRuntimeSnapshot::activate called without node set — \
             every production construction path must call .with_node(...) \
             before activation",
        );
        ActiveRuntimeSnapshot {
            generation,
            node: self.node,
            local_did: self.local_did,
            default_agent_did: self.default_agent_did,
            agents: self.agents,
            tool_surfaces: self.tool_surfaces,
            backend_admission_configs: self.backend_admission_configs,
            unavailable_agents: self.unavailable_agents,
            active_schedules: self.active_schedules,
            unavailable_schedules: self.unavailable_schedules,
            active_event_triggers: self.active_event_triggers,
            unavailable_event_triggers: self.unavailable_event_triggers,
            active_tasks: self.active_tasks,
            dispatchers,
            agent_executor_capacities,
            agent_executor_queue_capacities,
        }
    }

    pub(crate) fn configuration_fingerprint(&self) -> String {
        configuration_fingerprint(
            &self.default_agent_did,
            &self.local_did,
            &self.agents,
            &self.tool_surfaces,
            &self.backend_admission_configs,
            &self.unavailable_agents,
            &self.active_schedules,
            &self.unavailable_schedules,
            &self.active_event_triggers,
            &self.unavailable_event_triggers,
            &self.active_tasks,
        )
    }
}

#[derive(Clone, Debug)]
pub struct ActiveRuntimeSnapshot {
    pub generation: u64,
    pub node: Option<Arc<RuntimeNode>>,
    pub local_did: String,
    pub default_agent_did: String,
    pub agents: RapidHashMap<String, Arc<ResolvedAgent>>,
    pub tool_surfaces: RapidHashMap<String, Arc<ToolSurface>>,
    pub backend_admission_configs: RapidHashMap<String, BackendAdmissionConfig>,
    pub unavailable_agents: RapidHashMap<String, UnavailableAgent>,
    pub active_schedules: RapidHashMap<String, ResolvedSchedule>,
    pub unavailable_schedules: RapidHashSet<String>,
    pub active_event_triggers: RapidHashMap<String, ResolvedEventTrigger>,
    pub unavailable_event_triggers: RapidHashSet<String>,
    pub active_tasks: RapidHashMap<String, ResolvedTask>,
    pub dispatchers: DispatcherMap,
    pub agent_executor_capacities: RapidHashMap<String, usize>,
    pub agent_executor_queue_capacities: RapidHashMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AgentExecutorStatus {
    pub(crate) worker_capacity: usize,
    pub(crate) queue_depth: usize,
    pub(crate) queue_capacity: usize,
}

impl ActiveRuntimeSnapshot {
    pub(crate) fn watcher_identities(
        &self,
    ) -> Result<Vec<Arc<dyn crate::store::identity::NodeIdentity>>> {
        let mut identities = self
            .agents
            .iter()
            .map(|(did, agent)| {
                anyhow::ensure!(
                    did == &agent.agent_did && did == agent.agent_identity.did(),
                    "runtime snapshot has an inconsistent agent identity"
                );
                Ok(agent.agent_identity.clone())
            })
            .collect::<Result<Vec<_>>>()?;
        for (did, unavailable) in &self.unavailable_agents {
            if let Some(identity) = &unavailable.watcher_identity {
                anyhow::ensure!(
                    did == identity.0.did(),
                    "runtime snapshot has an inconsistent unavailable agent identity"
                );
                identities.push(identity.0.clone());
            }
        }
        Ok(identities)
    }

    pub(crate) fn agent(&self, agent_did: &str) -> Option<&Arc<ResolvedAgent>> {
        self.agents.get(agent_did)
    }

    pub(crate) fn active_schedules(&self) -> &RapidHashMap<String, ResolvedSchedule> {
        &self.active_schedules
    }

    pub(crate) fn active_event_triggers(&self) -> &RapidHashMap<String, ResolvedEventTrigger> {
        &self.active_event_triggers
    }

    pub(crate) fn active_tasks(&self) -> &RapidHashMap<String, ResolvedTask> {
        &self.active_tasks
    }

    pub(crate) fn tool_surface(&self, agent_did: &str) -> Option<&Arc<ToolSurface>> {
        self.tool_surfaces.get(agent_did)
    }

    pub(crate) fn unavailable_public_message(&self, agent_did: &str) -> Option<&'static str> {
        self.unavailable_agents
            .get(agent_did)
            .map(UnavailableAgent::public_message)
    }

    pub(crate) fn agent_executor_statuses(&self) -> BTreeMap<String, AgentExecutorStatus> {
        let mut agent_dids = self
            .agents
            .keys()
            .chain(self.dispatchers.keys())
            .chain(self.agent_executor_capacities.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        agent_dids.extend(self.agent_executor_queue_capacities.keys().cloned());

        agent_dids
            .into_iter()
            .map(|agent_did| {
                let dispatcher = self.dispatchers.get(&agent_did);
                let queue_capacity = self
                    .agent_executor_queue_capacities
                    .get(&agent_did)
                    .copied()
                    .or_else(|| dispatcher.map(mpsc::Sender::max_capacity))
                    .unwrap_or_default();
                let queue_depth = dispatcher
                    .map(|dispatcher| queue_capacity.saturating_sub(dispatcher.capacity()))
                    .unwrap_or_default();
                let worker_capacity = self
                    .agent_executor_capacities
                    .get(&agent_did)
                    .copied()
                    .unwrap_or_else(|| if dispatcher.is_some() { 1 } else { 0 });
                (
                    agent_did,
                    AgentExecutorStatus {
                        worker_capacity,
                        queue_depth,
                        queue_capacity,
                    },
                )
            })
            .collect()
    }

    pub(crate) fn configuration_fingerprint(&self) -> String {
        configuration_fingerprint(
            &self.default_agent_did,
            &self.local_did,
            &self.agents,
            &self.tool_surfaces,
            &self.backend_admission_configs,
            &self.unavailable_agents,
            &self.active_schedules,
            &self.unavailable_schedules,
            &self.active_event_triggers,
            &self.unavailable_event_triggers,
            &self.active_tasks,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn configuration_fingerprint(
    default_agent_did: &str,
    local_did: &str,
    agents: &RapidHashMap<String, Arc<ResolvedAgent>>,
    tool_surfaces: &RapidHashMap<String, Arc<ToolSurface>>,
    backend_admission_configs: &RapidHashMap<String, BackendAdmissionConfig>,
    unavailable_agents: &RapidHashMap<String, UnavailableAgent>,
    active_schedules: &RapidHashMap<String, ResolvedSchedule>,
    unavailable_schedules: &RapidHashSet<String>,
    active_event_triggers: &RapidHashMap<String, ResolvedEventTrigger>,
    unavailable_event_triggers: &RapidHashSet<String>,
    active_tasks: &RapidHashMap<String, ResolvedTask>,
) -> String {
    let mut fingerprint = String::new();
    fingerprint.push_str("local_did:");
    fingerprint.push_str(local_did);
    fingerprint.push('\n');
    fingerprint.push_str("default:");
    fingerprint.push_str(default_agent_did);
    fingerprint.push('\n');

    let mut agent_dids = agents.keys().cloned().collect::<Vec<_>>();
    agent_dids.sort();
    for agent_did in agent_dids {
        let agent = agents
            .get(&agent_did)
            .expect("agent id came from agents map");
        fingerprint.push_str("agent:");
        fingerprint.push_str(&agent_did);
        fingerprint.push('=');
        fingerprint.push_str(&format!("{agent:?}"));
        fingerprint.push('\n');
    }

    let mut tool_ids = tool_surfaces.keys().cloned().collect::<Vec<_>>();
    tool_ids.sort();
    for agent_did in tool_ids {
        let tool_surface = tool_surfaces
            .get(&agent_did)
            .expect("agent id came from tool surface map");
        fingerprint.push_str("tools:");
        fingerprint.push_str(&agent_did);
        fingerprint.push('=');
        fingerprint.push_str(&format!("{tool_surface:?}"));
        fingerprint.push('\n');
    }

    let mut backend_ids = backend_admission_configs
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    backend_ids.sort();
    for backend_id in backend_ids {
        let config = backend_admission_configs
            .get(&backend_id)
            .expect("backend id came from backend admission config map");
        fingerprint.push_str("backend_admission:");
        fingerprint.push_str(&backend_id);
        fingerprint.push('=');
        fingerprint.push_str(&format!("{config:?}"));
        fingerprint.push('\n');
    }

    let mut unavailable_ids = unavailable_agents.keys().cloned().collect::<Vec<_>>();
    unavailable_ids.sort();
    for agent_did in unavailable_ids {
        let reason = unavailable_agents
            .get(&agent_did)
            .expect("agent id came from unavailable agent map");
        fingerprint.push_str("unavailable:");
        fingerprint.push_str(&agent_did);
        fingerprint.push('=');
        fingerprint.push_str(&reason.diagnostic);
        fingerprint.push(':');
        fingerprint.push_str(&format!("{:?}", reason.public_reason));
        fingerprint.push(':');
        if let Some(identity) = &reason.watcher_identity {
            fingerprint.push_str(identity.0.did());
        }
        fingerprint.push('\n');
    }

    let mut schedule_ids = active_schedules.keys().cloned().collect::<Vec<_>>();
    schedule_ids.sort();
    for schedule_id in schedule_ids {
        let schedule = active_schedules
            .get(&schedule_id)
            .expect("schedule id came from active schedules map");
        fingerprint.push_str("schedule:");
        fingerprint.push_str(&schedule_id);
        fingerprint.push('=');
        fingerprint.push_str(&format!("{schedule:?}"));
        fingerprint.push('\n');
    }

    let mut unavailable_schedule_ids = unavailable_schedules.iter().cloned().collect::<Vec<_>>();
    unavailable_schedule_ids.sort();
    for schedule_id in unavailable_schedule_ids {
        fingerprint.push_str("unavailable_schedule:");
        fingerprint.push_str(&schedule_id);
        fingerprint.push('\n');
    }

    let mut event_trigger_ids = active_event_triggers.keys().cloned().collect::<Vec<_>>();
    event_trigger_ids.sort();
    for trigger_id in event_trigger_ids {
        let trigger = active_event_triggers
            .get(&trigger_id)
            .expect("event trigger id came from active event triggers map");
        fingerprint.push_str("event_trigger:");
        fingerprint.push_str(&trigger_id);
        fingerprint.push('=');
        fingerprint.push_str(&format!("{trigger:?}"));
        fingerprint.push('\n');
    }

    let mut unavailable_event_trigger_ids = unavailable_event_triggers
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    unavailable_event_trigger_ids.sort();
    for trigger_id in unavailable_event_trigger_ids {
        fingerprint.push_str("unavailable_event_trigger:");
        fingerprint.push_str(&trigger_id);
        fingerprint.push('\n');
    }

    let mut task_ids = active_tasks.keys().cloned().collect::<Vec<_>>();
    task_ids.sort();
    for task_id in task_ids {
        let task = active_tasks
            .get(&task_id)
            .expect("task id came from active tasks map");
        fingerprint.push_str("task:");
        fingerprint.push_str(&task_id);
        fingerprint.push('=');
        fingerprint.push_str(&format!("{task:?}"));
        fingerprint.push('\n');
    }

    fingerprint
}

#[cfg(test)]
mod tests;
