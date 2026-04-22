use serde::{Deserialize, Serialize};

/// Description of an event-driven trigger for a task.
///
/// Mirrors the `EventTrigger` GraphQL schema in
/// `crates/defra-agent-protocol/schemas/agent/event_trigger.graphql`. Includes
/// both apply-owned fields (`trigger_id`, `task_id`, `source_collection`,
/// `event_kind`, `filter`, `enabled`, `concurrency`, `created_at`,
/// `updated_at`) and runtime-owned fields (`last_attempt_at`,
/// `last_fired_source_doc_id`, `last_status`, `last_error`, `fire_count`)
/// because `DocumentRuntimeView` is a DB-read view.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct EventTrigger {
    pub(crate) trigger_id: String,
    #[serde(default)]
    pub(crate) task_id: Option<String>,
    #[serde(default)]
    pub(crate) source_collection: Option<String>,
    #[serde(default)]
    pub(crate) event_kind: Option<String>,
    #[serde(default)]
    pub(crate) filter: Option<String>,
    #[serde(default)]
    pub(crate) enabled: Option<bool>,
    #[serde(default)]
    pub(crate) concurrency: Option<String>,
    #[serde(default)]
    pub(crate) created_at: Option<String>,
    #[serde(default)]
    pub(crate) updated_at: Option<String>,
    // runtime-owned:
    #[serde(default)]
    pub(crate) last_attempt_at: Option<String>,
    #[serde(default)]
    pub(crate) last_fired_source_doc_id: Option<String>,
    #[serde(default)]
    pub(crate) last_status: Option<String>,
    #[serde(default)]
    pub(crate) last_error: Option<String>,
    #[serde(default)]
    pub(crate) fire_count: Option<i64>,
}
