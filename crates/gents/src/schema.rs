use defra_node::EmbeddedNode;

#[cfg(feature = "agent-memory")]
pub use gents_protocol::schemas::NODE_MEMORY as NODE_MEMORY_SCHEMA;
#[cfg(not(feature = "agent-memory"))]
pub use gents_protocol::schemas::NODE_MEMORY as NODE_MEMORY_SCHEMA;
pub use gents_protocol::schemas::{
    AGENT as AGENT_SCHEMA, AGENT_MESSAGE as AGENT_MESSAGE_SCHEMA,
    AGENT_OUTPUT_SEGMENT as AGENT_OUTPUT_SEGMENT_SCHEMA, AGENT_REQUEST as AGENT_REQUEST_SCHEMA,
    AGENT_SESSION as AGENT_SESSION_SCHEMA, AGENT_TOOL_CALL as AGENT_TOOL_CALL_SCHEMA,
    COMPACTION_ENTRY as COMPACTION_ENTRY_SCHEMA,
    DATA_PLANE_PAIRING_DESIRED as DATA_PLANE_PAIRING_DESIRED_SCHEMA, GOAL as GOAL_SCHEMA,
    GRAPH_DEFINITION as GRAPH_DEFINITION_SCHEMA, INFERENCE_BACKEND as INFERENCE_BACKEND_SCHEMA,
    INFERENCE_CALL as INFERENCE_CALL_SCHEMA, INFERENCE_PROFILE as INFERENCE_PROFILE_SCHEMA,
    MAILBOX_ITEM as MAILBOX_ITEM_SCHEMA, NODE as NODE_SCHEMA, NODE_RUNTIME as NODE_RUNTIME_SCHEMA,
    OAUTH_CREDENTIAL as OAUTH_CREDENTIAL_SCHEMA,
    PEER_PAIRING_APPLIED as PEER_PAIRING_APPLIED_SCHEMA,
    PEER_PAIRING_DESIRED as PEER_PAIRING_DESIRED_SCHEMA,
    PROJECTION_ACP_BINDING as PROJECTION_ACP_BINDING_SCHEMA, RUNTIME_ALL,
    SCHEDULE as SCHEDULE_SCHEMA, TASK as TASK_SCHEMA, TOOLS as TOOLS_SCHEMA,
    TOOL_SERVICE_HEALTH_STATE as TOOL_SERVICE_HEALTH_STATE_SCHEMA,
    TOOL_SERVICE_REGISTRY as TOOL_SERVICE_REGISTRY_SCHEMA,
};

#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    #[error(transparent)]
    Migration(#[from] crate::migration::RuntimeMigrationError),
}

pub async fn ensure_runtime_schemas(node: &EmbeddedNode) -> Result<(), SchemaError> {
    if let Some(access) = crate::config_client::ConfigAccess::claimed_for_node(node) {
        return access
            .run_local_owned(|node| async move {
                crate::migration::ensure_migrations_borrowed(node.as_ref()).await
            })
            .await
            .map_err(SchemaError::from);
    }
    Ok(crate::migration::ensure_migrations_borrowed(node).await?)
}
