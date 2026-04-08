use anyhow::Result;
use defra_node::EmbeddedNode;

pub const INFERENCE_BACKEND_SCHEMA: &str =
    include_str!("../schemas/inference/inference_backend.graphql");
pub const AGENT_CONVERSATION_SCHEMA: &str =
    include_str!("../schemas/agent/agent_conversation.graphql");
pub const AGENT_REQUEST_SCHEMA: &str = include_str!("../schemas/agent/agent_request.graphql");
pub const AGENT_RESPONSE_SCHEMA: &str = include_str!("../schemas/agent/agent_response.graphql");
pub const AGENT_TOOL_RESULT_SCHEMA: &str =
    include_str!("../schemas/agent/agent_tool_result.graphql");
pub const AGENT_SESSION_SCHEMA: &str = include_str!("../schemas/agent/agent_session.graphql");
pub const AGENT_MESSAGE_SCHEMA: &str = include_str!("../schemas/agent/agent_message.graphql");
pub const AGENT_TOOL_CALL_SCHEMA: &str = include_str!("../schemas/agent/agent_tool_call.graphql");
pub const COMPACTION_ENTRY_SCHEMA: &str = include_str!("../schemas/agent/compaction_entry.graphql");
pub const TOOL_SERVICE_REGISTRY_SCHEMA: &str =
    include_str!("../schemas/services/tool_service_registry.graphql");

pub const RUNTIME_ALL: &[&str] = &[INFERENCE_BACKEND_SCHEMA];

pub const ALL: &[&str] = &[
    AGENT_CONVERSATION_SCHEMA,
    AGENT_REQUEST_SCHEMA,
    AGENT_RESPONSE_SCHEMA,
    AGENT_TOOL_RESULT_SCHEMA,
    AGENT_SESSION_SCHEMA,
    AGENT_MESSAGE_SCHEMA,
    AGENT_TOOL_CALL_SCHEMA,
    COMPACTION_ENTRY_SCHEMA,
    TOOL_SERVICE_REGISTRY_SCHEMA,
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

pub async fn ensure_schemas(node: &EmbeddedNode) -> Result<()> {
    ensure_schema_set(node, ALL).await
}
