use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use super::serde_helpers::rows_with_doc_id;

/// Apply-owned description of a task.
///
/// Mirrors the `Task` GraphQL schema in
/// `crates/defra-agent-protocol/schemas/agent/task.graphql`. All fields are
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
