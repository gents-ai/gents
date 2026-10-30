use serde::{Deserialize, Serialize};

/// Reusable task definition; the trigger engine renders templates into an
/// AgentRequest before the owned loop executes it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct Task {
    pub node_did: String,
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub description: Option<String>,
    pub agent_id: String,
    pub prompt_template: String,
    /// Emit one durable FireOutcome at the terminal boundary.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "super::serde_helpers::is_disabled"
    )]
    pub emit_outcome: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub goal_objective_template: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub goal_token_budget: Option<i64>,
    /// Explicit host commands, in list order within each phase.
    /// No hooks means ordinary agent execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<TaskHook>>", optional = nullable))]
    pub hooks: Vec<TaskHook>,
    #[serde(
        default = "super::serde_helpers::default_enabled",
        deserialize_with = "super::serde_helpers::deserialize_enabled",
        skip_serializing_if = "super::serde_helpers::is_enabled"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional = nullable))]
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub output_schema_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub updated_at: Option<String>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

/// A task-owned host command, using the behavior's HostTools.root and inherited
/// host environment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct TaskHook {
    /// Unique within this task; identifies execution/recovery of this occurrence.
    pub hook_id: String,
    pub phase: TaskHookPhase,
    /// Executable followed by literal arguments; PATH lookup and relative
    /// paths use ordinary host process semantics.
    pub command: Vec<String>,
    /// Per-command timeout in seconds; absent uses 120, explicit 1..=86400.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub timeout_secs: Option<i64>,
    /// Nonzero exit codes that stop a before hook without starting the agent.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<i64>>", optional = nullable))]
    pub skip_exit_codes: Vec<i64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum TaskHookPhase {
    /// Before provider execution; failure prevents the agent from starting.
    Before,
    /// After successful agent execution; failure prevents successful completion.
    AfterSuccess,
    /// After failed agent execution or a failed before-hook; not cancellation.
    AfterFailure,
    /// Cleanup once any before-hook or agent step starts; every finally hook is
    /// attempted in order, other hook failures must not suppress cleanup, and
    /// cleanup failures are recorded without erasing the primary outcome.
    Finally,
}
