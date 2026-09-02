use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use super::serde_helpers::{first_row_with_doc_id, rows_with_doc_id};
use crate::graphql::escape_graphql_string;

/// Apply-owned description of a task.
///
/// Mirrors the `Task` GraphQL schema in
/// `crates/gents-schemas/schemas/agent/task.graphql`. All fields are
/// apply-owned: the runtime does not mutate any `Task` document field at
/// runtime. Optional fields use `Option<...>` and `DateTime` fields are carried
/// as RFC3339 `String`s to match the rest of `document_config`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Task {
    pub task_id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub behavior_id: Option<String>,
    pub prompt_template: Option<String>,
    pub goal_objective_template: Option<String>,
    pub goal_token_budget: Option<i64>,
    pub enabled: bool,
    pub output_schema_ref: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
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
