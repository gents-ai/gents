//! Runtime-side schema registration helpers.
//!
//! Schema strings are the canonical exports of `defra_agent_protocol::schemas`.
//! This module preserves the legacy `*_SCHEMA` names via re-exported aliases
//! and wires the canonical arrays to an `EmbeddedNode` via `ensure_schemas`
//! and `ensure_runtime_schemas`.

use anyhow::Result;
#[cfg(feature = "agent-memory")]
pub use defra_agent_protocol::schemas::AGENT_MEMORY as AGENT_MEMORY_SCHEMA;
#[cfg(not(feature = "agent-memory"))]
use defra_agent_protocol::schemas::AGENT_MEMORY;
pub use defra_agent_protocol::schemas::{
    AGENT_BEHAVIOR as AGENT_BEHAVIOR_SCHEMA, AGENT_CONVERSATION as AGENT_CONVERSATION_SCHEMA,
    AGENT_MESSAGE as AGENT_MESSAGE_SCHEMA, AGENT_PRINCIPAL as AGENT_PRINCIPAL_SCHEMA,
    AGENT_REQUEST as AGENT_REQUEST_SCHEMA, AGENT_RESPONSE as AGENT_RESPONSE_SCHEMA,
    AGENT_RUNTIME as AGENT_RUNTIME_SCHEMA, AGENT_SESSION as AGENT_SESSION_SCHEMA,
    AGENT_TOOL_CALL as AGENT_TOOL_CALL_SCHEMA, AGENT_TOOL_RESULT as AGENT_TOOL_RESULT_SCHEMA,
    COMPACTION_ENTRY as COMPACTION_ENTRY_SCHEMA, INFERENCE_BACKEND as INFERENCE_BACKEND_SCHEMA,
    INFERENCE_CALL as INFERENCE_CALL_SCHEMA, INFERENCE_PROFILE as INFERENCE_PROFILE_SCHEMA,
    PEER_PAIRING_APPLIED as PEER_PAIRING_APPLIED_SCHEMA,
    PEER_PAIRING_DESIRED as PEER_PAIRING_DESIRED_SCHEMA,
    PROJECTION_ACP_BINDING as PROJECTION_ACP_BINDING_SCHEMA, RUNTIME_ALL,
    SCHEDULE as SCHEDULE_SCHEMA, TASK as TASK_SCHEMA, TOOL_SELECTION as TOOL_SELECTION_SCHEMA,
    TOOL_SERVICE_HEALTH_STATE as TOOL_SERVICE_HEALTH_STATE_SCHEMA,
    TOOL_SERVICE_REGISTRY as TOOL_SERVICE_REGISTRY_SCHEMA,
};
use defra_node::EmbeddedNode;

pub const CONFIG_BOOTSTRAP: &[&str] = &[
    AGENT_PRINCIPAL_SCHEMA,
    AGENT_BEHAVIOR_SCHEMA,
    TOOL_SELECTION_SCHEMA,
    INFERENCE_BACKEND_SCHEMA,
    INFERENCE_PROFILE_SCHEMA,
];
async fn ensure_schema_set(node: &EmbeddedNode, schemas: &[&str]) -> Result<()> {
    for sdl in schemas {
        match node.add_schema(sdl).await {
            Ok(()) => {}
            Err(error) => {
                if error.to_string().contains("already exists") {
                    tracing::debug!(
                        schema = %sdl.lines().next().unwrap_or(""),
                        "schema already exists"
                    );
                } else {
                    return Err(error);
                }
            }
        }
    }

    Ok(())
}

pub async fn ensure_runtime_schemas(node: &EmbeddedNode) -> Result<()> {
    ensure_schema_set(node, RUNTIME_ALL).await?;
    ensure_schemas(node).await
}

pub async fn ensure_config_bootstrap_schemas(node: &EmbeddedNode) -> Result<()> {
    ensure_schema_set(node, CONFIG_BOOTSTRAP).await
}

#[cfg(feature = "agent-memory")]
pub async fn ensure_schemas(node: &EmbeddedNode) -> Result<()> {
    ensure_schema_set(node, defra_agent_protocol::schemas::ALL).await
}

#[cfg(not(feature = "agent-memory"))]
pub async fn ensure_schemas(node: &EmbeddedNode) -> Result<()> {
    let schemas = defra_agent_protocol::schemas::ALL
        .iter()
        .copied()
        .filter(|schema| *schema != AGENT_MEMORY)
        .collect::<Vec<_>>();
    ensure_schema_set(node, &schemas).await
}
