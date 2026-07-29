//! Runtime-side schema registration helpers.
//!
//! Schema strings remain the canonical exports of `gents_protocol::schemas`
//! (docs, desktop collection resolution, SelfConfig). **Registration** always
//! goes through [`crate::migration::ensure_all_runtime_migrations`] /
//! `gents_migration::ensure_migrations` so every database shares one version
//! lineage. There is no public "register schemas only" path.

use anyhow::Result;
use defra_node::EmbeddedNode;

#[cfg(feature = "agent-memory")]
pub use gents_protocol::schemas::AGENT_MEMORY as AGENT_MEMORY_SCHEMA;
#[cfg(not(feature = "agent-memory"))]
pub use gents_protocol::schemas::AGENT_MEMORY as AGENT_MEMORY_SCHEMA;
pub use gents_protocol::schemas::{
    AGENT_BEHAVIOR as AGENT_BEHAVIOR_SCHEMA, AGENT_CONVERSATION as AGENT_CONVERSATION_SCHEMA,
    AGENT_MESSAGE as AGENT_MESSAGE_SCHEMA, AGENT_PRINCIPAL as AGENT_PRINCIPAL_SCHEMA,
    AGENT_REQUEST as AGENT_REQUEST_SCHEMA, AGENT_RESPONSE as AGENT_RESPONSE_SCHEMA,
    AGENT_RUNTIME as AGENT_RUNTIME_SCHEMA, AGENT_SESSION as AGENT_SESSION_SCHEMA,
    AGENT_TOOL_CALL as AGENT_TOOL_CALL_SCHEMA, AGENT_TOOL_RESULT as AGENT_TOOL_RESULT_SCHEMA,
    COMPACTION_ENTRY as COMPACTION_ENTRY_SCHEMA,
    DATA_PLANE_PAIRING_DESIRED as DATA_PLANE_PAIRING_DESIRED_SCHEMA, GOAL as GOAL_SCHEMA,
    INFERENCE_BACKEND as INFERENCE_BACKEND_SCHEMA, INFERENCE_CALL as INFERENCE_CALL_SCHEMA,
    INFERENCE_PROFILE as INFERENCE_PROFILE_SCHEMA, OAUTH_CREDENTIAL as OAUTH_CREDENTIAL_SCHEMA,
    PEER_PAIRING_APPLIED as PEER_PAIRING_APPLIED_SCHEMA,
    PEER_PAIRING_DESIRED as PEER_PAIRING_DESIRED_SCHEMA,
    PROJECTION_ACP_BINDING as PROJECTION_ACP_BINDING_SCHEMA, RUNTIME_ALL,
    SCHEDULE as SCHEDULE_SCHEMA, TASK as TASK_SCHEMA, TOOL_SELECTION as TOOL_SELECTION_SCHEMA,
    TOOL_SERVICE_HEALTH_STATE as TOOL_SERVICE_HEALTH_STATE_SCHEMA,
    TOOL_SERVICE_REGISTRY as TOOL_SERVICE_REGISTRY_SCHEMA,
};

/// Replaced by the full baseline. Kept as a name alias so docs/tests that
/// still mention the six-collection init subset compile; calling
/// [`ensure_config_bootstrap_schemas`] registers the **full** baseline.
#[deprecated(note = "partial bootstrap forks lineage; use ensure_migrations / ensure_runtime_schemas")]
pub const CONFIG_BOOTSTRAP: &[&str] = &[
    AGENT_PRINCIPAL_SCHEMA,
    AGENT_BEHAVIOR_SCHEMA,
    TOOL_SELECTION_SCHEMA,
    OAUTH_CREDENTIAL_SCHEMA,
    INFERENCE_BACKEND_SCHEMA,
    INFERENCE_PROFILE_SCHEMA,
];

/// Register baseline + verify lineage (full engine). Feature-invariant.
pub async fn ensure_runtime_schemas(node: &EmbeddedNode) -> Result<()> {
    gents_migration::ensure_migrations(node)
        .await
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!(e))
}

/// Alias of [`ensure_runtime_schemas`] — full baseline, not a subset.
pub async fn ensure_config_bootstrap_schemas(node: &EmbeddedNode) -> Result<()> {
    ensure_runtime_schemas(node).await
}

/// Alias of [`ensure_runtime_schemas`]. Historical name for test helpers.
pub async fn ensure_schemas(node: &EmbeddedNode) -> Result<()> {
    ensure_runtime_schemas(node).await
}
