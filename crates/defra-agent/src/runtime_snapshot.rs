use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::{mpsc, watch};

use crate::admission::BackendAdmissionConfig;
use crate::config::BehaviorConfig;
use crate::tool_surface::ToolSurface;
use crate::watcher::AgentRequest;

pub(crate) type DispatcherMap = HashMap<String, mpsc::Sender<AgentRequest>>;

/// Resolved view of a `Task` document ready for the trigger engine to fire.
///
/// Captures only the fields the engine needs to build an `AgentRequest` at
/// fire time — behavior, prompt template, and optional output-schema
/// reference. Other `Task` fields (descriptions, timestamps, runtime-owned
/// status) stay in `DocumentRuntimeView`.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedTask {
    pub(crate) task_id: String,
    pub(crate) behavior_id: String,
    pub(crate) prompt_template: String,
    pub(crate) output_schema_ref: Option<String>,
}

/// Resolved view of a `Schedule` document paired with its resolved `Task`.
///
/// The task is embedded so the engine can "join once, fire many" — it does
/// not need to look the task up again at each fire time. Only schedules with
/// a resolvable task and an enabled, runnable behavior end up here; the rest
/// go in `unavailable_schedules`.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedSchedule {
    pub(crate) schedule_id: String,
    pub(crate) task_id: String,
    pub(crate) task: ResolvedTask,
    pub(crate) interval_secs: i64,
    pub(crate) enabled: bool,
    pub(crate) concurrency: ConcurrencyMode,
}

/// How a schedule handles overlapping runs when the previous fire has not
/// completed by the next interval.
///
/// * `Parallel` — launch every tick regardless of in-flight runs.
/// * `Serial` — skip a tick if a prior run is still in flight.
/// * `LatestOnly` — supersede the in-flight run with the newer tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ConcurrencyMode {
    Parallel,
    Serial,
    LatestOnly,
}

impl ConcurrencyMode {
    /// Parse the schedule `concurrency` string. Strict exact match on
    /// `"parallel"`, `"serial"`, or `"latest_only"` — no case folding,
    /// aliases, or whitespace trimming. Unknown inputs return `None` so the
    /// caller can mark the schedule unavailable.
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "parallel" => Some(Self::Parallel),
            "serial" => Some(Self::Serial),
            "latest_only" => Some(Self::LatestOnly),
            _ => None,
        }
    }
}

/// Resolved view of an `EventTrigger` document paired with its resolved
/// `Task`. Mirrors `ResolvedSchedule`: the task is embedded so the trigger
/// engine can "join once, fire many" without re-looking-up the task at each
/// fire time. Only triggers with a resolvable task and an enabled, runnable
/// behavior end up here; the rest go in `unavailable_event_triggers`.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedEventTrigger {
    pub(crate) trigger_id: String,
    pub(crate) task_id: String,
    pub(crate) task: ResolvedTask,
    pub(crate) source_collection: String,
    /// Currently always `"created"`; future PRs may add `"updated"` /
    /// `"deleted"` support.
    pub(crate) event_kind: String,
    pub(crate) filter: Option<String>,
    pub(crate) enabled: bool,
    pub(crate) concurrency: ConcurrencyMode,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedRuntimeSnapshot {
    pub(crate) default_behavior_id: String,
    pub(crate) behaviors: HashMap<String, Arc<BehaviorConfig>>,
    pub(crate) tool_surfaces: HashMap<String, Arc<ToolSurface>>,
    pub(crate) backend_admission_configs: HashMap<String, BackendAdmissionConfig>,
    pub(crate) unavailable_behaviors: HashMap<String, String>,
    pub(crate) active_schedules: HashMap<String, ResolvedSchedule>,
    pub(crate) unavailable_schedules: HashSet<String>,
    pub(crate) active_event_triggers: HashMap<String, ResolvedEventTrigger>,
    pub(crate) unavailable_event_triggers: HashSet<String>,
    pub(crate) active_tasks: HashMap<String, ResolvedTask>,
}

impl ResolvedRuntimeSnapshot {
    #[allow(dead_code)]
    pub(crate) fn from_parts(
        default_behavior_id: String,
        behaviors: Vec<Arc<BehaviorConfig>>,
        tool_surfaces: HashMap<String, Arc<ToolSurface>>,
        unavailable_behaviors: HashMap<String, String>,
    ) -> Self {
        Self::from_parts_with_admission_configs(
            default_behavior_id,
            behaviors,
            tool_surfaces,
            HashMap::new(),
            unavailable_behaviors,
        )
    }

    pub(crate) fn from_parts_with_admission_configs(
        default_behavior_id: String,
        behaviors: Vec<Arc<BehaviorConfig>>,
        tool_surfaces: HashMap<String, Arc<ToolSurface>>,
        backend_admission_configs: HashMap<String, BackendAdmissionConfig>,
        unavailable_behaviors: HashMap<String, String>,
    ) -> Self {
        Self {
            default_behavior_id,
            behaviors: behaviors
                .into_iter()
                .map(|behavior| (behavior.name.clone(), behavior))
                .collect(),
            tool_surfaces,
            backend_admission_configs,
            unavailable_behaviors,
            active_schedules: HashMap::new(),
            unavailable_schedules: HashSet::new(),
            active_event_triggers: HashMap::new(),
            unavailable_event_triggers: HashSet::new(),
            active_tasks: HashMap::new(),
        }
    }

    /// Attach resolved schedules plus any schedule ids that failed resolution.
    ///
    /// Task 18 builds the schedule maps during `resolve_document_snapshot_*`
    /// and layers them onto the snapshot via this builder so the existing
    /// `from_parts_*` callers (tests, startup fallback) stay untouched.
    pub(crate) fn with_schedules(
        mut self,
        active_schedules: HashMap<String, ResolvedSchedule>,
        unavailable_schedules: HashSet<String>,
    ) -> Self {
        self.active_schedules = active_schedules;
        self.unavailable_schedules = unavailable_schedules;
        self
    }

    /// Attach resolved event triggers plus any trigger ids that failed
    /// resolution. Mirrors `with_schedules`: active triggers are eligible to
    /// fire, unavailable ids let callers report/diff misconfigured triggers
    /// without keeping them runnable.
    pub(crate) fn with_event_triggers(
        mut self,
        active_event_triggers: HashMap<String, ResolvedEventTrigger>,
        unavailable_event_triggers: HashSet<String>,
    ) -> Self {
        self.active_event_triggers = active_event_triggers;
        self.unavailable_event_triggers = unavailable_event_triggers;
        self
    }

    /// Attach resolved tasks to the snapshot. Mirrors `with_schedules` /
    /// `with_event_triggers`: active tasks are the join-target for
    /// `ManualTriggerHandle::run_task_now`, which needs to map a caller-
    /// supplied `task_id` to a `ResolvedTask` at fire time.
    pub(crate) fn with_tasks(mut self, tasks: HashMap<String, ResolvedTask>) -> Self {
        self.active_tasks = tasks;
        self
    }

    pub(crate) fn activate(
        self,
        generation: u64,
        dispatchers: DispatcherMap,
    ) -> ActiveRuntimeSnapshot {
        ActiveRuntimeSnapshot {
            generation,
            default_behavior_id: self.default_behavior_id,
            behaviors: self.behaviors,
            tool_surfaces: self.tool_surfaces,
            backend_admission_configs: self.backend_admission_configs,
            unavailable_behaviors: self.unavailable_behaviors,
            active_schedules: self.active_schedules,
            unavailable_schedules: self.unavailable_schedules,
            active_event_triggers: self.active_event_triggers,
            unavailable_event_triggers: self.unavailable_event_triggers,
            active_tasks: self.active_tasks,
            dispatchers,
        }
    }

    pub(crate) fn configuration_fingerprint(&self) -> String {
        configuration_fingerprint(
            &self.default_behavior_id,
            &self.behaviors,
            &self.tool_surfaces,
            &self.backend_admission_configs,
            &self.unavailable_behaviors,
            &self.active_schedules,
            &self.unavailable_schedules,
            &self.active_event_triggers,
            &self.unavailable_event_triggers,
            &self.active_tasks,
        )
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ActiveRuntimeSnapshot {
    pub(crate) generation: u64,
    pub(crate) default_behavior_id: String,
    pub(crate) behaviors: HashMap<String, Arc<BehaviorConfig>>,
    pub(crate) tool_surfaces: HashMap<String, Arc<ToolSurface>>,
    pub(crate) backend_admission_configs: HashMap<String, BackendAdmissionConfig>,
    pub(crate) unavailable_behaviors: HashMap<String, String>,
    pub(crate) active_schedules: HashMap<String, ResolvedSchedule>,
    pub(crate) unavailable_schedules: HashSet<String>,
    pub(crate) active_event_triggers: HashMap<String, ResolvedEventTrigger>,
    pub(crate) unavailable_event_triggers: HashSet<String>,
    pub(crate) active_tasks: HashMap<String, ResolvedTask>,
    pub(crate) dispatchers: DispatcherMap,
}

impl ActiveRuntimeSnapshot {
    pub(crate) fn behavior(&self, behavior_id: &str) -> Option<&Arc<BehaviorConfig>> {
        self.behaviors.get(behavior_id)
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

    pub(crate) fn tool_surface(&self, behavior_id: &str) -> Option<&Arc<ToolSurface>> {
        self.tool_surfaces.get(behavior_id)
    }

    pub(crate) fn unavailable_reason(&self, behavior_id: &str) -> Option<&str> {
        self.unavailable_behaviors
            .get(behavior_id)
            .map(String::as_str)
    }

    pub(crate) fn configuration_fingerprint(&self) -> String {
        configuration_fingerprint(
            &self.default_behavior_id,
            &self.behaviors,
            &self.tool_surfaces,
            &self.backend_admission_configs,
            &self.unavailable_behaviors,
            &self.active_schedules,
            &self.unavailable_schedules,
            &self.active_event_triggers,
            &self.unavailable_event_triggers,
            &self.active_tasks,
        )
    }
}

pub(crate) fn refresh_active_snapshot(
    active_snapshot: &mut Arc<ActiveRuntimeSnapshot>,
    active_snapshot_rx: &mut watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
) -> bool {
    match active_snapshot_rx.has_changed() {
        Ok(true) => {
            *active_snapshot = active_snapshot_rx.borrow_and_update().clone();
            true
        }
        Ok(false) | Err(_) => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn configuration_fingerprint(
    default_behavior_id: &str,
    behaviors: &HashMap<String, Arc<BehaviorConfig>>,
    tool_surfaces: &HashMap<String, Arc<ToolSurface>>,
    backend_admission_configs: &HashMap<String, BackendAdmissionConfig>,
    unavailable_behaviors: &HashMap<String, String>,
    active_schedules: &HashMap<String, ResolvedSchedule>,
    unavailable_schedules: &HashSet<String>,
    active_event_triggers: &HashMap<String, ResolvedEventTrigger>,
    unavailable_event_triggers: &HashSet<String>,
    active_tasks: &HashMap<String, ResolvedTask>,
) -> String {
    let mut fingerprint = String::new();
    fingerprint.push_str("default:");
    fingerprint.push_str(default_behavior_id);
    fingerprint.push('\n');

    let mut behavior_ids = behaviors.keys().cloned().collect::<Vec<_>>();
    behavior_ids.sort();
    for behavior_id in behavior_ids {
        let behavior = behaviors
            .get(&behavior_id)
            .expect("behavior id came from behaviors map");
        fingerprint.push_str("behavior:");
        fingerprint.push_str(&behavior_id);
        fingerprint.push('=');
        fingerprint.push_str(&format!("{behavior:?}"));
        fingerprint.push('\n');
    }

    let mut tool_ids = tool_surfaces.keys().cloned().collect::<Vec<_>>();
    tool_ids.sort();
    for behavior_id in tool_ids {
        let tool_surface = tool_surfaces
            .get(&behavior_id)
            .expect("behavior id came from tool surface map");
        fingerprint.push_str("tools:");
        fingerprint.push_str(&behavior_id);
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

    let mut unavailable_ids = unavailable_behaviors.keys().cloned().collect::<Vec<_>>();
    unavailable_ids.sort();
    for behavior_id in unavailable_ids {
        let reason = unavailable_behaviors
            .get(&behavior_id)
            .expect("behavior id came from unavailable behavior map");
        fingerprint.push_str("unavailable:");
        fingerprint.push_str(&behavior_id);
        fingerprint.push('=');
        fingerprint.push_str(reason);
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

    let mut unavailable_event_trigger_ids =
        unavailable_event_triggers.iter().cloned().collect::<Vec<_>>();
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
