use serde::{Deserialize, Serialize};

/// Lean: `Triggers.FireBudget.defaultMaxPerHour`.
pub const DEFAULT_MAX_FIRES_PER_HOUR: u32 = 1000;

/// Lean: `Triggers.FireBudget.exhaustedStatus`.
pub const BUDGET_EXHAUSTED_STATUS: &str = "budget_exhausted";

/// Desired configuration for firing a reusable task.
///
/// A source supplies fire variables; the shared trigger engine renders the task
/// and enqueues an AgentRequest. Execution and terminal state belong to that
/// request, not to a new task-run lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct Trigger {
    pub node_did: String,
    pub trigger_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub description: Option<String>,
    pub task_id: String,
    pub source: TriggerSource,
    /// Render an existing session ID from the fire scope. Missing/null creates
    /// a session per fire; a configured template must render a nonempty owned
    /// session unless `create_session` admits a missing one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub session_id_template: Option<String>,
    /// Create the session `session_id_template` renders on its first fire.
    /// Lean: `Triggers.SessionKey`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub create_session: Option<bool>,
    /// Fires admitted per rolling hour before the Trigger disables itself and
    /// records why. Null uses the default. Lean: `Triggers.FireBudget`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_fires_per_hour: Option<u32>,
    #[serde(
        default = "super::serde_helpers::default_enabled",
        deserialize_with = "super::serde_helpers::deserialize_enabled",
        skip_serializing_if = "super::serde_helpers::is_enabled"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional = nullable))]
    #[schemars(transform = crate::accepts_null)]
    pub enabled: bool,
    /// Absent defaults to Parallel.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub concurrency: Option<ConcurrencyMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub updated_at: Option<String>,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    #[schemars(transform = crate::accepts_null)]
    pub tags: Vec<String>,
}

impl Trigger {
    /// Lean: `Triggers.FireBudget.effective`.
    pub fn effective_max_fires_per_hour(&self) -> u32 {
        self.max_fires_per_hour
            .unwrap_or(DEFAULT_MAX_FIRES_PER_HOUR)
    }

    /// Lean: `Triggers.SessionKey.configValid` and `Triggers.FireBudget.configValid`;
    /// `command_task` is whether the Trigger's Task runs a host command.
    pub fn validate_options(&self, command_task: bool) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.create_session != Some(true) || self.session_id_template.is_some(),
            "create_session requires session_id_template: render the session key to get or create"
        );
        anyhow::ensure!(
            self.max_fires_per_hour != Some(0),
            "max_fires_per_hour must be positive; omit it to use the default"
        );
        anyhow::ensure!(
            self.max_fires_per_hour.is_none()
                || matches!(self.source, TriggerSource::Event { .. })
                || self.session_id_template.is_some()
                || command_task,
            "max_fires_per_hour needs an event source, a scheduled session_id_template or a command Task; an untargeted schedule of an Agent's Task is limited by its cadence"
        );
        Ok(())
    }
}

/// Exactly one configured source per trigger. Source documents can be reused;
/// each trigger retains its own delivery identity and runtime observations.
/// Manual task invocation does not require a persisted Trigger document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum TriggerSource {
    Schedule { schedule_id: String },
    Event { event_source_id: String },
}

/// Runtime-owned observations, excluded from desired configuration exports.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TriggerObservation {
    pub trigger_id: String,
    pub last_attempt_at: Option<String>,
    pub last_fired_source_doc_id: Option<String>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub fire_count: Option<i64>,
}

/// One run of a command Task, as the Node records it on the Trigger that
/// fired it (`Trigger.last_run`): written when the command starts and again
/// when it ends.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandRun {
    pub run_id: String,
    pub state: CommandRunState,
    /// The process exit status; absent until it exits, or when a signal ended it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    /// The end of the captured stdout and stderr, at most 4 KiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tail: Option<String>,
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandRunState {
    Running,
    /// Exited with status 0.
    Succeeded,
    /// Exited nonzero, by a signal, or could not be launched.
    Failed,
    TimedOut,
    /// Stopped by a newer `latest_only` fire or by shutdown.
    Interrupted,
}

/// Shared concurrency vocabulary for all task-trigger entry points.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum ConcurrencyMode {
    #[default]
    Parallel,
    Serial,
    /// Persist every fire and admit one request at a time in source order.
    QueuedSerial,
    LatestOnly,
}

impl ConcurrencyMode {
    pub const ALL: [Self; 4] = [
        Self::Parallel,
        Self::Serial,
        Self::QueuedSerial,
        Self::LatestOnly,
    ];
}
