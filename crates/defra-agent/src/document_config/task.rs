use serde::{Deserialize, Serialize};

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
