use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::Serialize;
use tokio::sync::mpsc;

use crate::admission::BackendAdmissionConfig;
use crate::config::ResolvedAgent;
pub use crate::document_config::ConcurrencyMode;
pub use crate::document_config::ScheduleCadence;
use crate::document_config::TaskHook;
use crate::identity::RuntimeNode;
use crate::schedule_cron::{next_cron_run_after, CronMissedRunPolicy};
use crate::tool_surface::ToolSurface;
use crate::watcher::AgentRequest;
use gents_protocol::node_readiness::AgentReadinessUnavailableReason;
use gents_protocol::node_readiness::{
    effective_agent_readiness_admission, EffectiveAgentReadinessAdmission,
};

pub type DispatcherMap = HashMap<String, mpsc::Sender<AgentRequest>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnavailableAgent {
    pub public_reason: AgentReadinessUnavailableReason,
    pub diagnostic: String,
}

impl UnavailableAgent {
    pub fn new(
        public_reason: AgentReadinessUnavailableReason,
        diagnostic: impl Into<String>,
    ) -> Self {
        Self {
            public_reason,
            diagnostic: diagnostic.into(),
        }
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

/// Single agent-admission decision shared by routing and readiness
/// publication. Explicit unavailability and startup demotion always veto an
/// installed dispatcher.
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

#[derive(Debug, Clone)]
pub struct ResolvedTask {
    pub emit_outcome: bool,
    pub task_id: String,
    pub name: Option<String>,
    pub agent_id: String,
    pub prompt_template: String,
    pub goal_objective_template: Option<String>,
    pub goal_token_budget: Option<i64>,
    #[allow(dead_code)]
    pub output_schema_ref: Option<String>,
    /// Hooks are explicitly configured host commands carried by Task; no
    /// second persisted model and no TaskRun lifecycle state here.
    pub hooks: Vec<TaskHook>,
}

impl ResolvedTask {
    pub(crate) fn display_label(&self) -> &str {
        self.name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(&self.task_id)
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedSchedule {
    pub session_id_template: Option<String>,
    /// Physical id of the Trigger document whose Schedule source points at
    /// this schedule; empty when no enabled trigger references it.
    pub trigger_doc_id: String,
    pub schedule_id: String,
    #[allow(dead_code)]
    pub task_id: String,
    pub task: ResolvedTask,
    pub cadence: ScheduleCadence,
    #[allow(dead_code)]
    pub enabled: bool,
    pub concurrency: ConcurrencyMode,
}

pub const MAX_EVENT_TRIGGER_GROUP_DOCS: usize = 256;

/// Runtime delivery mode derived from the presence of `EventSource.group`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventTriggerFireMode {
    PerDocument,
    PerGroup,
}

#[derive(Debug, Clone)]
pub struct ResolvedEventTrigger {
    pub session_id_template: Option<String>,
    /// Physical id of the Trigger document carrying this event source.
    pub trigger_doc_id: String,
    pub trigger_id: String,
    #[allow(dead_code)]
    pub task_id: String,
    pub task: ResolvedTask,
    pub source_collection: String,
    pub event_kind: String,
    pub filter: Option<String>,
    #[allow(dead_code)]
    pub enabled: bool,
    pub concurrency: ConcurrencyMode,
    pub fire_mode: EventTriggerFireMode,
    pub correlation_field: Option<String>,
    pub expected_count: Option<usize>,
    pub expected_count_field: Option<String>,
    pub group_timeout_secs: Option<u64>,
    pub group_min_count: usize,
    pub workspace_authority: Option<String>,
}

/// Resolved automation projection installed on the runtime snapshot in one
/// step via [`ResolvedRuntimeSnapshot::with_automation`].
#[derive(Debug, Clone, Default)]
pub(crate) struct ResolvedAutomation {
    pub(crate) tasks: HashMap<String, ResolvedTask>,
    pub(crate) schedules: HashMap<String, ResolvedSchedule>,
    pub(crate) unavailable_schedules: HashSet<String>,
    pub(crate) event_triggers: HashMap<String, ResolvedEventTrigger>,
    pub(crate) unavailable_event_triggers: HashSet<String>,
}

/// Seeds the first `next_run_at` cursor for a canonical cadence: interval
/// schedules are immediately due; cron schedules align to the next match.
pub(crate) fn seed_schedule_next_run_at(
    cadence: &ScheduleCadence,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>> {
    match cadence {
        ScheduleCadence::Interval { .. } => Ok(now),
        ScheduleCadence::Cron {
            expression,
            timezone,
            ..
        } => next_cron_run_after(expression, timezone, now),
    }
}

/// Advances a parsed `next_run_at` cursor after a fire attempt: interval
/// schedules add their interval; cron schedules (latest_only) jump to the
/// next match after `now`.
pub(crate) fn advance_schedule_next_run_at(
    cadence: &ScheduleCadence,
    parsed_next_run_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>> {
    match cadence {
        ScheduleCadence::Interval { interval_secs } => {
            Ok(parsed_next_run_at + ChronoDuration::seconds(*interval_secs))
        }
        ScheduleCadence::Cron {
            expression,
            timezone,
            missed_run_policy,
        } => match missed_run_policy {
            None | Some(CronMissedRunPolicy::LatestOnly) => {
                next_cron_run_after(expression, timezone, now)
            }
        },
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedRuntimeSnapshot {
    pub(crate) node: Option<Arc<RuntimeNode>>,
    pub(crate) local_did: String,
    pub(crate) default_agent_id: String,
    pub(crate) agents: HashMap<String, Arc<ResolvedAgent>>,
    pub(crate) tool_surfaces: HashMap<String, Arc<ToolSurface>>,
    pub(crate) backend_admission_configs: HashMap<String, BackendAdmissionConfig>,
    pub(crate) unavailable_agents: HashMap<String, UnavailableAgent>,
    pub(crate) active_schedules: HashMap<String, ResolvedSchedule>,
    pub(crate) unavailable_schedules: HashSet<String>,
    pub(crate) active_event_triggers: HashMap<String, ResolvedEventTrigger>,
    pub(crate) unavailable_event_triggers: HashSet<String>,
    pub(crate) active_tasks: HashMap<String, ResolvedTask>,
}

impl ResolvedRuntimeSnapshot {
    /// Validates the runtime-authored identity set before any executor slot is
    /// started or an active generation is installed.
    pub(crate) fn validate_node_readiness_source(&self) -> Result<()> {
        let canonical = |value: &str| !value.is_empty() && value == value.trim();
        if !canonical(&self.default_agent_id) {
            anyhow::bail!(
                "default agent {:?} is not a canonical agent identifier",
                self.default_agent_id
            );
        }
        if self
            .agents
            .keys()
            .chain(self.unavailable_agents.keys())
            .any(|agent_id| !canonical(agent_id))
        {
            anyhow::bail!("runtime agent identifiers must be non-empty and trimmed");
        }
        let runnable_agent_ids = self.agents.keys().collect::<BTreeSet<_>>();
        let tool_surface_agent_ids = self.tool_surfaces.keys().collect::<BTreeSet<_>>();
        if runnable_agent_ids != tool_surface_agent_ids {
            let missing = runnable_agent_ids
                .difference(&tool_surface_agent_ids)
                .map(|agent_id| agent_id.as_str())
                .collect::<Vec<_>>();
            let extra = tool_surface_agent_ids
                .difference(&runnable_agent_ids)
                .map(|agent_id| agent_id.as_str())
                .collect::<Vec<_>>();
            anyhow::bail!(
                "runnable agent/tool-surface keysets differ (missing={missing:?}, extra={extra:?})"
            );
        }
        if !self.agents.contains_key(&self.default_agent_id)
            && !self.unavailable_agents.contains_key(&self.default_agent_id)
        {
            anyhow::bail!(
                "default agent {:?} is not assigned to the resolved runtime",
                self.default_agent_id
            );
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn from_parts(
        default_agent_id: String,
        agents: Vec<Arc<ResolvedAgent>>,
        tool_surfaces: HashMap<String, Arc<ToolSurface>>,
        unavailable_agents: HashMap<String, UnavailableAgent>,
    ) -> Self {
        Self::from_parts_with_admission_configs(
            default_agent_id,
            agents,
            tool_surfaces,
            HashMap::new(),
            unavailable_agents,
        )
    }

    pub(crate) fn from_parts_with_admission_configs(
        default_agent_id: String,
        agents: Vec<Arc<ResolvedAgent>>,
        tool_surfaces: HashMap<String, Arc<ToolSurface>>,
        backend_admission_configs: HashMap<String, BackendAdmissionConfig>,
        unavailable_agents: HashMap<String, UnavailableAgent>,
    ) -> Self {
        Self {
            node: None,
            local_did: String::new(),
            default_agent_id,
            agents: agents
                .into_iter()
                .map(|agent| (agent.agent_id.clone(), agent))
                .collect(),
            tool_surfaces,
            backend_admission_configs,
            unavailable_agents,
            active_schedules: HashMap::new(),
            unavailable_schedules: HashSet::new(),
            active_event_triggers: HashMap::new(),
            unavailable_event_triggers: HashSet::new(),
            active_tasks: HashMap::new(),
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

    /// Single canonical automation setter: installs resolved tasks, schedules
    /// and event triggers with their unavailability sets.
    pub(crate) fn with_automation(mut self, automation: ResolvedAutomation) -> Self {
        self.active_tasks = automation.tasks;
        self.active_schedules = automation.schedules;
        self.unavailable_schedules = automation.unavailable_schedules;
        self.active_event_triggers = automation.event_triggers;
        self.unavailable_event_triggers = automation.unavailable_event_triggers;
        self
    }

    #[cfg(test)]
    pub(crate) fn activate(
        self,
        generation: u64,
        dispatchers: DispatcherMap,
    ) -> ActiveRuntimeSnapshot {
        let agent_executor_capacities = dispatchers
            .keys()
            .map(|agent_id| (agent_id.clone(), 1))
            .collect();
        let agent_executor_queue_capacities = dispatchers
            .iter()
            .map(|(agent_id, dispatcher)| (agent_id.clone(), dispatcher.max_capacity()))
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
        agent_executor_capacities: HashMap<String, usize>,
        agent_executor_queue_capacities: HashMap<String, usize>,
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
            default_agent_id: self.default_agent_id,
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
            &self.default_agent_id,
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
    pub default_agent_id: String,
    pub agents: HashMap<String, Arc<ResolvedAgent>>,
    pub tool_surfaces: HashMap<String, Arc<ToolSurface>>,
    pub backend_admission_configs: HashMap<String, BackendAdmissionConfig>,
    pub unavailable_agents: HashMap<String, UnavailableAgent>,
    pub active_schedules: HashMap<String, ResolvedSchedule>,
    pub unavailable_schedules: HashSet<String>,
    pub active_event_triggers: HashMap<String, ResolvedEventTrigger>,
    pub unavailable_event_triggers: HashSet<String>,
    pub active_tasks: HashMap<String, ResolvedTask>,
    pub dispatchers: DispatcherMap,
    pub agent_executor_capacities: HashMap<String, usize>,
    pub agent_executor_queue_capacities: HashMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AgentExecutorStatus {
    pub(crate) worker_capacity: usize,
    pub(crate) queue_depth: usize,
    pub(crate) queue_capacity: usize,
}

impl ActiveRuntimeSnapshot {
    pub(crate) fn agent(&self, agent_id: &str) -> Option<&Arc<ResolvedAgent>> {
        self.agents.get(agent_id)
    }

    pub(crate) fn active_schedules(&self) -> &HashMap<String, ResolvedSchedule> {
        &self.active_schedules
    }

    pub(crate) fn active_event_triggers(&self) -> &HashMap<String, ResolvedEventTrigger> {
        &self.active_event_triggers
    }

    pub(crate) fn active_tasks(&self) -> &HashMap<String, ResolvedTask> {
        &self.active_tasks
    }

    pub(crate) fn tool_surface(&self, agent_id: &str) -> Option<&Arc<ToolSurface>> {
        self.tool_surfaces.get(agent_id)
    }

    pub(crate) fn unavailable_public_message(&self, agent_id: &str) -> Option<&'static str> {
        self.unavailable_agents
            .get(agent_id)
            .map(UnavailableAgent::public_message)
    }

    pub(crate) fn agent_executor_statuses(&self) -> BTreeMap<String, AgentExecutorStatus> {
        let mut agent_ids = self
            .agents
            .keys()
            .chain(self.dispatchers.keys())
            .chain(self.agent_executor_capacities.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        agent_ids.extend(self.agent_executor_queue_capacities.keys().cloned());

        agent_ids
            .into_iter()
            .map(|agent_id| {
                let dispatcher = self.dispatchers.get(&agent_id);
                let queue_capacity = self
                    .agent_executor_queue_capacities
                    .get(&agent_id)
                    .copied()
                    .or_else(|| dispatcher.map(mpsc::Sender::max_capacity))
                    .unwrap_or_default();
                let queue_depth = dispatcher
                    .map(|dispatcher| queue_capacity.saturating_sub(dispatcher.capacity()))
                    .unwrap_or_default();
                let worker_capacity = self
                    .agent_executor_capacities
                    .get(&agent_id)
                    .copied()
                    .unwrap_or_else(|| if dispatcher.is_some() { 1 } else { 0 });
                (
                    agent_id,
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
            &self.default_agent_id,
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
    default_agent_id: &str,
    local_did: &str,
    agents: &HashMap<String, Arc<ResolvedAgent>>,
    tool_surfaces: &HashMap<String, Arc<ToolSurface>>,
    backend_admission_configs: &HashMap<String, BackendAdmissionConfig>,
    unavailable_agents: &HashMap<String, UnavailableAgent>,
    active_schedules: &HashMap<String, ResolvedSchedule>,
    unavailable_schedules: &HashSet<String>,
    active_event_triggers: &HashMap<String, ResolvedEventTrigger>,
    unavailable_event_triggers: &HashSet<String>,
    active_tasks: &HashMap<String, ResolvedTask>,
) -> String {
    let mut fingerprint = String::new();
    fingerprint.push_str("local_did:");
    fingerprint.push_str(local_did);
    fingerprint.push('\n');
    fingerprint.push_str("default:");
    fingerprint.push_str(default_agent_id);
    fingerprint.push('\n');

    let mut agent_ids = agents.keys().cloned().collect::<Vec<_>>();
    agent_ids.sort();
    for agent_id in agent_ids {
        let agent = agents
            .get(&agent_id)
            .expect("agent id came from agents map");
        fingerprint.push_str("agent:");
        fingerprint.push_str(&agent_id);
        fingerprint.push('=');
        fingerprint.push_str(&format!("{agent:?}"));
        fingerprint.push('\n');
    }

    let mut tool_ids = tool_surfaces.keys().cloned().collect::<Vec<_>>();
    tool_ids.sort();
    for agent_id in tool_ids {
        let tool_surface = tool_surfaces
            .get(&agent_id)
            .expect("agent id came from tool surface map");
        fingerprint.push_str("tools:");
        fingerprint.push_str(&agent_id);
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
    for agent_id in unavailable_ids {
        let reason = unavailable_agents
            .get(&agent_id)
            .expect("agent id came from unavailable agent map");
        fingerprint.push_str("unavailable:");
        fingerprint.push_str(&agent_id);
        fingerprint.push('=');
        fingerprint.push_str(&reason.diagnostic);
        fingerprint.push(':');
        fingerprint.push_str(&format!("{:?}", reason.public_reason));
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
