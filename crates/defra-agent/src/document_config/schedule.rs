use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use super::serde_helpers::rows_with_doc_id;

/// Description of a scheduled trigger for a task.
///
/// Mirrors the `Schedule` GraphQL schema in
/// `crates/defra-agent-protocol/schemas/agent/schedule.graphql`. Includes both
/// apply-owned fields (`schedule_id`, `task_id`, `interval_secs`, `enabled`,
/// `concurrency`, `created_at`, `updated_at`) and runtime-owned fields
/// (`next_run_at`, `last_attempt_at`, `last_status`, `last_error`,
/// `fire_count`) because `DocumentRuntimeView` is a DB-read view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Schedule {
    pub schedule_id: String,
    pub task_id: Option<String>,
    pub interval_secs: Option<i64>,
    pub enabled: bool,
    pub concurrency: Option<String>,
    pub next_run_at: Option<String>,
    pub last_attempt_at: Option<String>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub fire_count: Option<i64>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// List every `Schedule` document in the node, returning `(doc_id, schedule)`
/// pairs.
///
/// Schedules are addressed by a globally unique `schedule_id` (see
/// `schedule.graphql`), so this helper is not scoped by `agent_did`.
pub(crate) async fn list_schedule_records(
    node: &EmbeddedNode,
) -> Result<Vec<(String, Schedule)>> {
    let query = r#"{
            Schedule(order: { schedule_id: ASC }) {
                _docID
                schedule_id
                task_id
                interval_secs
                enabled
                concurrency
                next_run_at
                last_attempt_at
                last_status
                last_error
                fire_count
                created_at
                updated_at
            }
        }"#;

    let resp = node.execute(query).await;
    if resp.has_errors() {
        anyhow::bail!("list Schedule failed: {:?}", resp.errors);
    }

    Ok(rows_with_doc_id(resp.data.as_ref(), "Schedule"))
}
