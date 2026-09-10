use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use super::serde_helpers::{first_row_with_doc_id, rows_with_doc_id};
use crate::graphql::escape_graphql_string;

/// Reusable task definition; firing a trigger does not create a Task document.
///
/// The trigger engine renders these templates into an AgentRequest before the
/// owned request loop executes it. Manual invocation uses the same definition.
/// All fields are desired configuration; execution state belongs to requests.
/// This structural draft is not yet reflected in GraphQL or runtime readers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub agent_did: String,
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub behavior_id: String,
    pub prompt_template: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_objective_template: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_token_budget: Option<i64>,
    /// Explicit host commands, in list order within each phase.
    /// No hooks means ordinary agent execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub hooks: Vec<TaskHook>,
    #[serde(
        default = "super::serde_helpers::default_enabled",
        deserialize_with = "super::serde_helpers::deserialize_enabled",
        skip_serializing_if = "super::serde_helpers::is_enabled"
    )]
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tags: Vec<String>,
}

/// A task-owned host command, using the behavior's HostTools.root (runtime cwd
/// when absent) and inherited host environment. No input projection, prompt
/// interpolation, callback reference, or workspace-specific argument schema.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
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
    pub timeout_secs: Option<i64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
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

/// List every `Task` document in the node, returning `(doc_id, task)` pairs.
///
/// Tasks are addressed by a globally unique `task_id` (see
/// `task.graphql`), so this helper is not scoped by `agent_did`.
pub(crate) async fn list_task_records(node: &EmbeddedNode) -> Result<Vec<(String, Task)>> {
    let query = r#"{
            Task(order: { task_id: ASC }) {
                _docID
                task_id
                name
                description
                behavior_id
                prompt_template
                goal_objective_template
                goal_token_budget
                enabled
                output_schema_ref
                created_at
                updated_at
            }
        }"#;

    let resp = node.execute(query).await;
    if resp.has_errors() {
        anyhow::bail!("list Task failed: {:?}", resp.errors);
    }

    Ok(rows_with_doc_id(resp.data.as_ref(), "Task"))
}

/// Load a single `Task` document by its DefraDB `_docID`.
///
/// Used by the control watcher's update-dispatch path to classify an updated
/// document by collection when only the `_docID` is known.
pub(crate) async fn load_task_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<(String, Task)>> {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            Task(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                limit: 1
            ) {{
                _docID
                task_id
                name
                description
                behavior_id
                prompt_template
                goal_objective_template
                goal_token_budget
                enabled
                output_schema_ref
                created_at
                updated_at
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query Task by _docID failed: {:?}", resp.errors);
    }

    Ok(first_row_with_doc_id(resp.data.as_ref(), "Task"))
}
