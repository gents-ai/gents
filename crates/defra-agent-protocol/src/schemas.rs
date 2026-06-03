//! Static GraphQL schema strings for every replicated collection.
//!
//! Schema files are `include_str!`-compiled into the binary so that runtime
//! nodes and client peers register identical collection schemas without
//! pulling the files in at startup. `ALL` lists the deployment schemas in
//! registration order; `RUNTIME_ALL` lists the schemas that must be
//! registered before runtime reconciliation can begin.

// agent domain
pub use defra_agent_schemas::{
    AGENT_BEHAVIOR, AGENT_BEHAVIOR_NAME, AGENT_CONVERSATION, AGENT_CONVERSATION_NAME,
    AGENT_MESSAGE, AGENT_MESSAGE_NAME, AGENT_PRINCIPAL, AGENT_PRINCIPAL_NAME, AGENT_REQUEST,
    AGENT_REQUEST_NAME, AGENT_RESPONSE, AGENT_RESPONSE_NAME, AGENT_RUNTIME, AGENT_RUNTIME_NAME,
    AGENT_SESSION, AGENT_SESSION_NAME, AGENT_TOOL_CALL, AGENT_TOOL_CALL_NAME, AGENT_TOOL_RESULT,
    AGENT_TOOL_RESULT_NAME, ALL as AGENT_ALL, ALL_COLLECTION_NAMES as AGENT_COLLECTION_NAMES,
    BRANCHABLE_COLLECTION_NAMES as BRANCHABLE_AGENT_COLLECTION_NAMES, CODEX_THREAD_PROJECTION,
    CODEX_THREAD_PROJECTION_NAME, COMPACTION_ENTRY, COMPACTION_ENTRY_NAME, EVENT_TRIGGER,
    EVENT_TRIGGER_NAME, PEER_PAIRING_DESIRED, PEER_PAIRING_DESIRED_NAME, SCHEDULE, SCHEDULE_NAME,
    SKILL, SKILL_NAME, TASK, TASK_NAME, TOOL_SELECTION, TOOL_SELECTION_NAME,
};

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
    SKILL,
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
    SKILL_NAME,
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

pub const BRANCHABLE_COLLECTION_NAMES: &[&str] = BRANCHABLE_AGENT_COLLECTION_NAMES;

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn all_contains_every_schema() {
        assert_eq!(
            ALL.len(),
            22,
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
