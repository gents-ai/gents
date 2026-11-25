use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Result;
use futures::future::BoxFuture;
use serde::Serialize;
use tokio::sync::{mpsc, watch};

use gents_documents::automation::{
    ResolvedAutomation, ResolvedEventTrigger, ResolvedSchedule, ResolvedTask,
};
use gents_inference::admission::BackendAdmissionConfig;
use gents_inference::resolved::ResolvedAgent;
use gents_protocol::node_readiness::AgentReadinessUnavailableReason;
use gents_protocol::node_readiness::{
    effective_agent_readiness_admission, EffectiveAgentReadinessAdmission,
};
use gents_protocol::row::AgentRequest;
use gents_store::agent_identity_watch::{
    AgentIdentityWatch, AgentIdentityWatchClosed, WatchedAndActiveAgents,
};
use gents_store::identity::RuntimeNode;
use gents_tools::surface::{AgentToolSurfaceConfig, AgentWithTools, ToolSurface};

pub type DispatcherMap = RapidHashMap<String, AgentDispatcher>;

/// An Agent's executor queues: one per worker level, where a request waits
/// only for the workers of its own level (Lean `WorkerLevels.lane`), and one
/// for auxiliary requests (session-title audits, access reviews), which take
/// no worker and so never wait for a request to end.
#[derive(Clone, Debug)]
pub struct AgentDispatcher {
    levels: Arc<[mpsc::Sender<AgentRequest>]>,
    auxiliary: mpsc::Sender<AgentRequest>,
}

impl AgentDispatcher {
    pub fn new(
        levels: Vec<mpsc::Sender<AgentRequest>>,
        auxiliary: mpsc::Sender<AgentRequest>,
    ) -> Self {
        assert!(!levels.is_empty(), "an Agent has at least one worker level");
        Self {
            levels: levels.into(),
            auxiliary,
        }
    }

    /// One queue for every request, as a fixture's single executor reads it.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn single(queue: mpsc::Sender<AgentRequest>) -> Self {
        Self::new(vec![queue.clone()], queue)
    }

    fn level(request_hop: u32, levels: usize) -> usize {
        usize::try_from(request_hop)
            .unwrap_or(usize::MAX)
            .min(levels.saturating_sub(1))
    }

    pub fn for_request(&self, request: &AgentRequest) -> &mpsc::Sender<AgentRequest> {
        if request.purpose.is_auxiliary() {
            return &self.auxiliary;
        }
        &self.levels[Self::level(request.request_hop, self.levels.len())]
    }

    fn queues(&self) -> impl Iterator<Item = &mpsc::Sender<AgentRequest>> {
        let auxiliary = (!self
            .levels
            .iter()
            .any(|level| level.same_channel(&self.auxiliary)))
        .then_some(&self.auxiliary);
        self.levels.iter().chain(auxiliary)
    }

    pub fn max_capacity(&self) -> usize {
        self.queues().map(mpsc::Sender::max_capacity).sum()
    }

    pub fn queued(&self) -> usize {
        self.queues()
            .map(|queue| queue.max_capacity().saturating_sub(queue.capacity()))
            .sum()
    }

    pub fn same_channel(&self, other: &Self) -> bool {
        self.auxiliary.same_channel(&other.auxiliary)
            && self.levels.len() == other.levels.len()
            && self
                .levels
                .iter()
                .zip(other.levels.iter())
                .all(|(mine, theirs)| mine.same_channel(theirs))
    }
}

#[derive(Clone)]
pub struct ResolvedWatcherIdentity(pub Arc<dyn gents_store::identity::NodeIdentity>);

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
    /// A cause whose rejection says more than its reason's fixed wording: the
    /// text a request is rejected with, carrying the facts its fix needs.
    pub public_text: Option<String>,
    pub awaiting_first_probe: bool,
    pub watcher_identity: Option<ResolvedWatcherIdentity>,
    /// The agent's plugin tools could not be built because an installer held
    /// the plugin store: a transient cause the runtime recovers from itself.
    pub publication_busy: bool,
}

impl UnavailableAgent {
    pub fn new(
        public_reason: AgentReadinessUnavailableReason,
        diagnostic: impl Into<String>,
    ) -> Self {
        Self {
            public_reason,
            diagnostic: diagnostic.into(),
            public_text: None,
            awaiting_first_probe: false,
            watcher_identity: None,
            publication_busy: false,
        }
    }

    pub fn with_publication_busy(mut self, publication_busy: bool) -> Self {
        self.publication_busy = publication_busy;
        self
    }

    /// The rejection text a cause states itself, beyond its reason's wording.
    pub fn with_public_message(mut self, text: impl Into<String>) -> Self {
        self.public_text = Some(text.into());
        self
    }

    pub fn with_awaiting_first_probe(mut self, awaiting_first_probe: bool) -> Self {
        self.awaiting_first_probe = awaiting_first_probe;
        self
    }

    pub fn with_watcher_identity(
        mut self,
        identity: Arc<dyn gents_store::identity::NodeIdentity>,
    ) -> Self {
        self.watcher_identity = Some(ResolvedWatcherIdentity(identity));
        self
    }

    pub fn public_message(&self) -> &'static str {
        self.public_reason.public_message()
    }

    /// The text a request rejected for this Agent carries: the cause's own when
    /// it has one, its reason's wording otherwise.
    pub fn rejection_text(&self) -> &str {
        self.public_text
            .as_deref()
            .unwrap_or_else(|| self.public_reason.public_message())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveAgentAdmission<'a> {
    Ready,
    AwaitFirstProbe,
    Unavailable {
        public_reason: AgentReadinessUnavailableReason,
        diagnostic: &'a str,
        /// The text a rejected request carries: the cause's own when it says
        /// more than its reason's wording, that wording otherwise.
        rejection_text: &'a str,
    },
    Unassigned,
}

pub fn effective_agent_admission<'a>(
    dispatcher_present: bool,
    unavailable: Option<&'a UnavailableAgent>,
    startup_diagnostic: Option<&'a str>,
) -> EffectiveAgentAdmission<'a> {
    if startup_diagnostic.is_none() && unavailable.is_some_and(|agent| agent.awaiting_first_probe) {
        return EffectiveAgentAdmission::AwaitFirstProbe;
    }
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
                rejection_text: unavailable.map_or_else(
                    || public_reason.public_message(),
                    UnavailableAgent::rejection_text,
                ),
            }
        }
        EffectiveAgentReadinessAdmission::Unassigned => EffectiveAgentAdmission::Unassigned,
    }
}

#[derive(Clone, Debug)]
pub struct ResolvedRuntimeSnapshot {
    pub node: Option<Arc<RuntimeNode>>,
    pub(crate) local_did: String,
    pub default_agent_did: String,
    pub agents: RapidHashMap<String, Arc<ResolvedAgent>>,
    pub tool_configs: RapidHashMap<String, Arc<AgentToolSurfaceConfig>>,
    pub tool_surfaces: RapidHashMap<String, Arc<ToolSurface>>,
    pub backend_admission_configs: RapidHashMap<String, BackendAdmissionConfig>,
    pub unavailable_agents: RapidHashMap<String, UnavailableAgent>,
    pub active_schedules: RapidHashMap<String, ResolvedSchedule>,
    pub unavailable_schedules: RapidHashSet<String>,
    pub active_event_triggers: RapidHashMap<String, ResolvedEventTrigger>,
    pub unavailable_event_triggers: RapidHashSet<String>,
    pub active_tasks: RapidHashMap<String, ResolvedTask>,
}

impl ResolvedRuntimeSnapshot {
    pub fn validate_node_readiness_source(&self) -> Result<()> {
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
        if self.tool_configs.len() != self.agents.len()
            || !self
                .agents
                .keys()
                .all(|agent_did| self.tool_configs.contains_key(agent_did))
        {
            anyhow::bail!("runnable agent/tool-configuration keysets differ");
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

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn from_parts(
        default_agent_did: String,
        agents: Vec<AgentWithTools>,
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

    pub fn from_parts_with_admission_configs(
        default_agent_did: String,
        agents: Vec<AgentWithTools>,
        tool_surfaces: RapidHashMap<String, Arc<ToolSurface>>,
        backend_admission_configs: RapidHashMap<String, BackendAdmissionConfig>,
        unavailable_agents: RapidHashMap<String, UnavailableAgent>,
    ) -> Self {
        let mut resolved_agents = RapidHashMap::with_capacity(agents.len());
        let mut tool_configs = RapidHashMap::with_capacity(agents.len());
        for AgentWithTools { agent, tools } in agents {
            tool_configs.insert(agent.agent_did.clone(), tools);
            resolved_agents.insert(agent.agent_did.clone(), agent);
        }
        Self {
            node: None,
            local_did: String::new(),
            default_agent_did,
            agents: resolved_agents,
            tool_configs,
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

    /// A runnable Agent paired with the tool surface configuration it was resolved with.
    pub fn agent_with_tools(&self, agent_did: &str) -> Option<AgentWithTools> {
        Some(AgentWithTools {
            agent: self.agents.get(agent_did)?.clone(),
            tools: self.tool_configs.get(agent_did)?.clone(),
        })
    }

    /// Every runnable Agent paired with the tool surface configuration it was resolved with.
    pub fn agents_with_tools(&self) -> Result<Vec<AgentWithTools>> {
        self.agents
            .keys()
            .map(|agent_did| {
                self.agent_with_tools(agent_did).ok_or_else(|| {
                    anyhow::anyhow!("runnable agent {agent_did} has no tool surface configuration")
                })
            })
            .collect()
    }

    pub fn with_node(mut self, node: Arc<RuntimeNode>) -> Self {
        self.node = Some(node);
        self
    }

    pub fn with_local_did(mut self, local_did: String) -> Self {
        self.local_did = local_did;
        self
    }

    pub fn with_automation(mut self, automation: ResolvedAutomation) -> Self {
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
    pub fn demote_agent(&mut self, agent_did: &str, reason: UnavailableAgent) {
        let identity = self
            .agents
            .remove(agent_did)
            .map(|agent| agent.agent_identity.clone());
        self.tool_configs.remove(agent_did);
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

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn activate(
        self,
        generation: u64,
        dispatchers: RapidHashMap<String, mpsc::Sender<AgentRequest>>,
    ) -> ActiveRuntimeSnapshot {
        let dispatchers: DispatcherMap = dispatchers
            .into_iter()
            .map(|(agent_did, sender)| (agent_did, AgentDispatcher::single(sender)))
            .collect();
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

    pub fn activate_with_executor_metadata(
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
            tool_configs: self.tool_configs,
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

    pub fn configuration_fingerprint(&self) -> String {
        SnapshotConfiguration {
            default_agent_did: &self.default_agent_did,
            local_did: &self.local_did,
            agents: &self.agents,
            tool_configs: &self.tool_configs,
            tool_surfaces: &self.tool_surfaces,
            backend_admission_configs: &self.backend_admission_configs,
            unavailable_agents: &self.unavailable_agents,
            active_schedules: &self.active_schedules,
            unavailable_schedules: &self.unavailable_schedules,
            active_event_triggers: &self.active_event_triggers,
            unavailable_event_triggers: &self.unavailable_event_triggers,
            active_tasks: &self.active_tasks,
        }
        .fingerprint()
    }
}

#[derive(Clone, Debug)]
pub struct ActiveRuntimeSnapshot {
    pub generation: u64,
    pub node: Option<Arc<RuntimeNode>>,
    pub local_did: String,
    pub default_agent_did: String,
    pub agents: RapidHashMap<String, Arc<ResolvedAgent>>,
    pub tool_configs: RapidHashMap<String, Arc<AgentToolSurfaceConfig>>,
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

/// The background owners' view of the active Agent signers: each consumer
/// holds its own receiver, so its `changed` marks only its own observation.
pub struct ActiveSnapshotAgentIdentities(watch::Receiver<Arc<ActiveRuntimeSnapshot>>);

impl ActiveSnapshotAgentIdentities {
    pub fn boxed(
        snapshots: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    ) -> Box<dyn AgentIdentityWatch> {
        Box::new(Self(snapshots))
    }
}

impl AgentIdentityWatch for ActiveSnapshotAgentIdentities {
    fn watcher_identities(&self) -> Result<Vec<Arc<dyn gents_store::identity::NodeIdentity>>> {
        self.0.borrow().watcher_identities()
    }

    fn watched_and_active_agents(&self) -> Result<WatchedAndActiveAgents> {
        let snapshot = self.0.borrow();
        Ok((
            snapshot.watcher_identities()?,
            snapshot.agents.keys().cloned().collect(),
        ))
    }

    fn changed(&mut self) -> BoxFuture<'_, std::result::Result<(), AgentIdentityWatchClosed>> {
        Box::pin(async move { self.0.changed().await.map_err(|_| AgentIdentityWatchClosed) })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentExecutorStatus {
    pub worker_capacity: usize,
    pub queue_depth: usize,
    pub(crate) queue_capacity: usize,
}

impl ActiveRuntimeSnapshot {
    pub fn watcher_identities(&self) -> Result<Vec<Arc<dyn gents_store::identity::NodeIdentity>>> {
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

    pub fn agent(&self, agent_did: &str) -> Option<&Arc<ResolvedAgent>> {
        self.agents.get(agent_did)
    }

    pub(crate) fn active_schedules(&self) -> &RapidHashMap<String, ResolvedSchedule> {
        &self.active_schedules
    }

    pub(crate) fn active_event_triggers(&self) -> &RapidHashMap<String, ResolvedEventTrigger> {
        &self.active_event_triggers
    }

    pub fn tool_config(&self, agent_did: &str) -> Option<&Arc<AgentToolSurfaceConfig>> {
        self.tool_configs.get(agent_did)
    }

    pub(crate) fn tool_surface(&self, agent_did: &str) -> Option<&Arc<ToolSurface>> {
        self.tool_surfaces.get(agent_did)
    }

    pub(crate) fn unavailable_public_message(&self, agent_did: &str) -> Option<&'static str> {
        self.unavailable_agents
            .get(agent_did)
            .map(UnavailableAgent::public_message)
    }

    pub fn agent_executor_statuses(&self) -> BTreeMap<String, AgentExecutorStatus> {
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
                    .or_else(|| dispatcher.map(AgentDispatcher::max_capacity))
                    .unwrap_or_default();
                let queue_depth = dispatcher.map(AgentDispatcher::queued).unwrap_or_default();
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

    pub fn configuration_fingerprint(&self) -> String {
        SnapshotConfiguration {
            default_agent_did: &self.default_agent_did,
            local_did: &self.local_did,
            agents: &self.agents,
            tool_configs: &self.tool_configs,
            tool_surfaces: &self.tool_surfaces,
            backend_admission_configs: &self.backend_admission_configs,
            unavailable_agents: &self.unavailable_agents,
            active_schedules: &self.active_schedules,
            unavailable_schedules: &self.unavailable_schedules,
            active_event_triggers: &self.active_event_triggers,
            unavailable_event_triggers: &self.unavailable_event_triggers,
            active_tasks: &self.active_tasks,
        }
        .fingerprint()
    }
}

/// The configuration both snapshot stages carry; equal configurations
/// fingerprint equally.
struct SnapshotConfiguration<'a> {
    default_agent_did: &'a str,
    local_did: &'a str,
    agents: &'a RapidHashMap<String, Arc<ResolvedAgent>>,
    tool_configs: &'a RapidHashMap<String, Arc<AgentToolSurfaceConfig>>,
    tool_surfaces: &'a RapidHashMap<String, Arc<ToolSurface>>,
    backend_admission_configs: &'a RapidHashMap<String, BackendAdmissionConfig>,
    unavailable_agents: &'a RapidHashMap<String, UnavailableAgent>,
    active_schedules: &'a RapidHashMap<String, ResolvedSchedule>,
    unavailable_schedules: &'a RapidHashSet<String>,
    active_event_triggers: &'a RapidHashMap<String, ResolvedEventTrigger>,
    unavailable_event_triggers: &'a RapidHashSet<String>,
    active_tasks: &'a RapidHashMap<String, ResolvedTask>,
}

impl SnapshotConfiguration<'_> {
    fn fingerprint(self) -> String {
        let SnapshotConfiguration {
            default_agent_did,
            local_did,
            agents,
            tool_configs,
            tool_surfaces,
            backend_admission_configs,
            unavailable_agents,
            active_schedules,
            unavailable_schedules,
            active_event_triggers,
            unavailable_event_triggers,
            active_tasks,
        } = self;
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
            if let Some(tool_config) = tool_configs.get(&agent_did) {
                fingerprint.push_str("tool_config:");
                fingerprint.push_str(&agent_did);
                fingerprint.push('=');
                fingerprint.push_str(&format!("{tool_config:?}"));
                fingerprint.push('\n');
            }
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
            fingerprint.push_str(&format!(
                "{:?}:{}",
                reason.public_reason, reason.awaiting_first_probe
            ));
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

        let mut unavailable_schedule_ids =
            unavailable_schedules.iter().cloned().collect::<Vec<_>>();
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
}

#[cfg(test)]
mod tests;
