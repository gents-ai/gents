//! Static GraphQL schema strings for every replicated collection.
//!
//! Schema files are `include_str!`-compiled into the binary so that runtime
//! nodes and client peers register identical collection schemas without
//! pulling the files in at startup. `ALL` lists the deployment schemas in
//! registration order; `RUNTIME_ALL` lists the schemas that must be
//! registered before runtime reconciliation can begin.

// agent domain
pub const AGENT_PRINCIPAL_NAME: &str = "AgentPrincipal";
pub const AGENT_PRINCIPAL: &str = include_str!("../schemas/agent/agent_principal.graphql");
pub const AGENT_BEHAVIOR_NAME: &str = "AgentBehavior";
pub const AGENT_BEHAVIOR: &str = include_str!("../schemas/agent/agent_behavior.graphql");
pub const AGENT_RUNTIME_NAME: &str = "AgentRuntime";
pub const AGENT_RUNTIME: &str = include_str!("../schemas/agent/agent_runtime.graphql");
pub const AGENT_CONVERSATION_NAME: &str = "AgentConversation";
pub const AGENT_CONVERSATION: &str = include_str!("../schemas/agent/agent_conversation.graphql");
pub const AGENT_REQUEST_NAME: &str = "AgentRequest";
pub const AGENT_REQUEST: &str = include_str!("../schemas/agent/agent_request.graphql");
pub const AGENT_RESPONSE_NAME: &str = "AgentResponse";
pub const AGENT_RESPONSE: &str = include_str!("../schemas/agent/agent_response.graphql");
pub const AGENT_MESSAGE_NAME: &str = "AgentMessage";
pub const AGENT_MESSAGE: &str = include_str!("../schemas/agent/agent_message.graphql");
pub const AGENT_SESSION_NAME: &str = "AgentSession";
pub const AGENT_SESSION: &str = include_str!("../schemas/agent/agent_session.graphql");
pub const AGENT_TOOL_CALL_NAME: &str = "AgentToolCall";
pub const AGENT_TOOL_CALL: &str = include_str!("../schemas/agent/agent_tool_call.graphql");
pub const AGENT_TOOL_RESULT_NAME: &str = "AgentToolResult";
pub const AGENT_TOOL_RESULT: &str = include_str!("../schemas/agent/agent_tool_result.graphql");
pub const COMPACTION_ENTRY_NAME: &str = "CompactionEntry";
pub const COMPACTION_ENTRY: &str = include_str!("../schemas/agent/compaction_entry.graphql");
pub const CODEX_THREAD_PROJECTION_NAME: &str = "CodexThreadProjection";
pub const CODEX_THREAD_PROJECTION: &str =
    include_str!("../schemas/agent/codex_thread_projection.graphql");
pub const TOOL_SELECTION_NAME: &str = "ToolSelection";
pub const TOOL_SELECTION: &str = include_str!("../schemas/agent/tool_selection.graphql");
pub const TASK_NAME: &str = "Task";
pub const TASK: &str = include_str!("../schemas/agent/task.graphql");
pub const SCHEDULE_NAME: &str = "Schedule";
pub const SCHEDULE: &str = include_str!("../schemas/agent/schedule.graphql");
pub const EVENT_TRIGGER_NAME: &str = "EventTrigger";
pub const EVENT_TRIGGER: &str = include_str!("../schemas/agent/event_trigger.graphql");
pub const PEER_PAIRING_DESIRED_NAME: &str = "PeerPairingDesired";
pub const PEER_PAIRING_DESIRED: &str =
    include_str!("../schemas/agent/peer_pairing_desired.graphql");

// inference domain
pub const INFERENCE_BACKEND_NAME: &str = "InferenceBackend";
pub const INFERENCE_BACKEND: &str = include_str!("../schemas/inference/inference_backend.graphql");
pub const INFERENCE_CALL_NAME: &str = "InferenceCall";
pub const INFERENCE_CALL: &str = include_str!("../schemas/inference/inference_call.graphql");
pub const INFERENCE_PROFILE_NAME: &str = "InferenceProfile";
pub const INFERENCE_PROFILE: &str = include_str!("../schemas/inference/inference_profile.graphql");

// services domain
pub const TOOL_SERVICE_REGISTRY_NAME: &str = "ToolServiceRegistry";
pub const TOOL_SERVICE_REGISTRY: &str =
    include_str!("../schemas/services/tool_service_registry.graphql");
pub const TOOL_SERVICE_HEALTH_STATE_NAME: &str = "ToolServiceHealthState";
pub const TOOL_SERVICE_HEALTH_STATE: &str =
    include_str!("../schemas/services/tool_service_health_state.graphql");

/// Schemas that must be registered before the runtime can start reconciling.
/// Mirrors the legacy `defra_agent::schema::RUNTIME_ALL`.
pub const RUNTIME_ALL: &[&str] = &[INFERENCE_BACKEND];
pub const RUNTIME_COLLECTION_NAMES: &[&str] = &[INFERENCE_BACKEND_NAME];

/// Every schema required by a full agent deployment. Registration order
/// matches the legacy `defra_agent::schema::ALL`.
pub const ALL: &[&str] = &[
    AGENT_PRINCIPAL,
    AGENT_BEHAVIOR,
    AGENT_RUNTIME,
    TOOL_SELECTION,
    INFERENCE_PROFILE,
    INFERENCE_CALL,
    AGENT_CONVERSATION,
    AGENT_REQUEST,
    AGENT_RESPONSE,
    AGENT_TOOL_RESULT,
    AGENT_SESSION,
    AGENT_MESSAGE,
    AGENT_TOOL_CALL,
    COMPACTION_ENTRY,
    CODEX_THREAD_PROJECTION,
    TASK,
    SCHEDULE,
    EVENT_TRIGGER,
    TOOL_SERVICE_REGISTRY,
    TOOL_SERVICE_HEALTH_STATE,
    PEER_PAIRING_DESIRED,
];
pub const ALL_COLLECTION_NAMES: &[&str] = &[
    AGENT_PRINCIPAL_NAME,
    AGENT_BEHAVIOR_NAME,
    AGENT_RUNTIME_NAME,
    TOOL_SELECTION_NAME,
    INFERENCE_PROFILE_NAME,
    INFERENCE_CALL_NAME,
    AGENT_CONVERSATION_NAME,
    AGENT_REQUEST_NAME,
    AGENT_RESPONSE_NAME,
    AGENT_TOOL_RESULT_NAME,
    AGENT_SESSION_NAME,
    AGENT_MESSAGE_NAME,
    AGENT_TOOL_CALL_NAME,
    COMPACTION_ENTRY_NAME,
    CODEX_THREAD_PROJECTION_NAME,
    TASK_NAME,
    SCHEDULE_NAME,
    EVENT_TRIGGER_NAME,
    TOOL_SERVICE_REGISTRY_NAME,
    TOOL_SERVICE_HEALTH_STATE_NAME,
    PEER_PAIRING_DESIRED_NAME,
];

pub const BRANCHABLE_COLLECTION_NAMES: &[&str] = &[
    AGENT_RUNTIME_NAME,
    AGENT_CONVERSATION_NAME,
    AGENT_REQUEST_NAME,
    AGENT_RESPONSE_NAME,
    AGENT_TOOL_RESULT_NAME,
    AGENT_SESSION_NAME,
    AGENT_MESSAGE_NAME,
    AGENT_TOOL_CALL_NAME,
    COMPACTION_ENTRY_NAME,
    CODEX_THREAD_PROJECTION_NAME,
    TASK_NAME,
    SCHEDULE_NAME,
    EVENT_TRIGGER_NAME,
];

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn all_contains_every_schema() {
        assert_eq!(
            ALL.len(),
            21,
            "ALL should enumerate every non-runtime schema"
        );
    }

    #[test]
    fn every_schema_starts_with_type_declaration() {
        for sdl in ALL.iter().chain(RUNTIME_ALL.iter()) {
            let first_sdl_line = sdl
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty() && !line.starts_with('#'))
                .unwrap_or("");
            assert!(
                first_sdl_line.starts_with("type "),
                "schema must begin with `type`: {}",
                sdl.lines().next().unwrap_or("")
            );
        }
    }

    #[test]
    fn collection_names_align_with_sdl_arrays() {
        assert_eq!(ALL.len(), ALL_COLLECTION_NAMES.len());
        assert_eq!(RUNTIME_ALL.len(), RUNTIME_COLLECTION_NAMES.len());
    }

    #[test]
    fn collection_names_are_unique() {
        let mut seen = HashSet::new();

        for name in ALL_COLLECTION_NAMES
            .iter()
            .chain(RUNTIME_COLLECTION_NAMES.iter())
        {
            assert!(seen.insert(*name), "duplicate collection name: {name}");
        }
    }
}
