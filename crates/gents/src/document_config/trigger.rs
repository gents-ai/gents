use serde::{Deserialize, Serialize};

/// Desired configuration for firing a reusable task.
///
/// A source supplies fire variables; the shared trigger engine renders the task
/// and enqueues an AgentRequest. Execution and terminal state belong to that
/// request, not to a new task-run lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct Trigger {
    pub agent_did: String,
    pub trigger_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub description: Option<String>,
    pub task_id: String,
    pub source: TriggerSource,
    #[serde(
        default = "super::serde_helpers::default_enabled",
        deserialize_with = "super::serde_helpers::deserialize_enabled",
        skip_serializing_if = "super::serde_helpers::is_enabled"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional = nullable))]
    pub enabled: bool,
    /// Reuse parallel / serial / latest_only semantics. Absent/null is Parallel
    /// for both schedule and event sources, matching the graph-edge default.
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
    pub tags: Vec<String>,
}

/// Exactly one configured source per trigger. Source documents can be reused;
/// each trigger retains its own delivery identity and runtime observations.
/// Manual task invocation remains supported by the existing manual source and
/// does not require a persisted Trigger document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    LatestOnly,
}
