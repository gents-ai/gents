use serde::{Deserialize, Serialize};

/// Reusable task definition; firing a trigger does not create a Task document.
///
/// The trigger engine renders these templates into an AgentRequest before the
/// owned request loop executes it. Manual invocation uses the same definition.
/// All fields are desired configuration; execution state belongs to requests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct Task {
    pub agent_did: String,
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub description: Option<String>,
    pub behavior_id: String,
    pub prompt_template: String,
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
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

/// A task-owned host command, using the behavior's HostTools.cwd (runtime cwd
/// when absent) and inherited host environment. No input projection, prompt
/// interpolation, callback reference, or workspace-specific argument schema.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct TaskHook {
    /// Unique within this task; identifies execution/recovery of this occurrence.
    pub hook_id: String,
    pub phase: TaskHookPhase,
    /// Executable followed by literal arguments. Must be nonempty. PATH lookup
    /// and relative paths use ordinary host process semantics. For shell syntax,
    /// explicitly invoke a shell, e.g. ["sh", "-c", "./prepare.sh && ./verify.sh"].
    pub command: Vec<String>,
    /// Per-command timeout in seconds. Absent/null uses 120; explicit values
    /// must be positive. Launch failure, timeout, and nonzero exit are hook errors.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub timeout_secs: Option<i64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum TaskHookPhase {
    /// Before provider execution, after durable request creation.
    /// Failure prevents the agent from starting and enters failure/cleanup phases.
    Before,
    /// After successful agent execution. Failure prevents successful completion.
    AfterSuccess,
    /// After failed agent execution or a failed before-hook; not cancellation.
    AfterFailure,
    /// Cleanup on success, failure, or cancellation once any before-hook or agent
    /// step starts, including a failing first before-hook. Every finally hook is
    /// attempted in order even if an earlier cleanup hook fails.
    /// Other hook failures must not suppress cleanup. Cleanup failures are recorded
    /// without erasing the primary failure/cancellation. Interrupted commands surface
    /// through existing request recovery; arbitrary host effects are not replayed.
    Finally,
}
