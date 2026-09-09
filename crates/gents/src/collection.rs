//! Canonical operator-controlled document collections.
//!
//! Names and owner-scoped logical keys follow `ConfigDocuments`, shared by Lean's
//! apply and self-configuration models. Enumeration is deterministic, not a
//! dependency ordering: references can form cycles. Atomic publication of a
//! complete owner-closed manifest provides reference safety.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Collection {
    AgentPrincipal,
    AgentBehavior,
    AgentContext,
    Compaction,
    Skill,
    DatastoreToolSurface,
    ChainKeyBinding,
    EthTool,
    Tools,
    SubagentTarget,
    InferenceBackend,
    InferenceProfile,
    InferenceSampling,
    InferenceExecution,
    InferenceRetryPolicy,
    ToolServiceRegistry,
    ProjectionAcpBinding,
    Task,
    Schedule,
    EventSource,
    Trigger,
    Callback,
    CallbackBinding,
    CallbackModule,
    RepositoryPlacement,
    GraphDefinition,
}

/// Deterministic document traversal for desired-state writes. This enumeration
/// provides no reference-safety guarantee; publication must validate the complete
/// manifest and atomically commit it, including legal reference cycles.
pub const DESIRED_STATE_APPLY_ORDER: [Collection; 26] = Collection::ALL;

impl Collection {
    pub const ALL: [Collection; 26] = [
        Self::AgentPrincipal,
        Self::AgentBehavior,
        Self::AgentContext,
        Self::Compaction,
        Self::Skill,
        Self::DatastoreToolSurface,
        Self::ChainKeyBinding,
        Self::EthTool,
        Self::Tools,
        Self::SubagentTarget,
        Self::InferenceBackend,
        Self::InferenceProfile,
        Self::InferenceSampling,
        Self::InferenceExecution,
        Self::InferenceRetryPolicy,
        Self::ToolServiceRegistry,
        Self::ProjectionAcpBinding,
        Self::Task,
        Self::Schedule,
        Self::EventSource,
        Self::Trigger,
        Self::Callback,
        Self::CallbackBinding,
        Self::CallbackModule,
        Self::RepositoryPlacement,
        Self::GraphDefinition,
    ];

    pub fn file_name(self) -> Option<&'static str> {
        match self {
            Self::AgentPrincipal => Some("agent_principal.json"),
            _ => None,
        }
    }

    pub fn dir_name(self) -> Option<&'static str> {
        match self {
            Self::AgentPrincipal => None,
            Self::AgentBehavior => Some("agent_behaviors"),
            Self::AgentContext => Some("contexts"),
            Self::Compaction => Some("compactions"),
            Self::Skill => Some("skills"),
            Self::DatastoreToolSurface => Some("datastore_tool_surfaces"),
            Self::ChainKeyBinding => Some("chain_key_bindings"),
            Self::EthTool => Some("eth_tools"),
            Self::Tools => Some("tools"),
            Self::SubagentTarget => Some("subagent_targets"),
            Self::InferenceBackend => Some("inference_backends"),
            Self::InferenceProfile => Some("inference_profiles"),
            Self::InferenceSampling => Some("inference_sampling"),
            Self::InferenceExecution => Some("inference_execution"),
            Self::InferenceRetryPolicy => Some("inference_retry_policies"),
            Self::ToolServiceRegistry => Some("tool_service_registries"),
            Self::ProjectionAcpBinding => Some("projection_acp_bindings"),
            Self::Task => Some("tasks"),
            Self::Schedule => Some("schedules"),
            Self::EventSource => Some("event_sources"),
            Self::Trigger => Some("triggers"),
            Self::Callback => Some("callbacks"),
            Self::CallbackBinding => Some("callback_bindings"),
            Self::CallbackModule => Some("callback_modules"),
            Self::RepositoryPlacement => Some("repository_placements"),
            Self::GraphDefinition => Some("graphs"),
        }
    }

    pub fn graphql_type(self) -> &'static str {
        match self {
            Self::AgentPrincipal => "AgentPrincipal",
            Self::AgentBehavior => "AgentBehavior",
            Self::AgentContext => "AgentContext",
            Self::Compaction => "CompactionConfig",
            Self::Skill => "Skill",
            Self::DatastoreToolSurface => "DatastoreToolSurface",
            Self::ChainKeyBinding => "ChainKeyBinding",
            Self::EthTool => "EthTool",
            Self::Tools => "Tools",
            Self::SubagentTarget => "SubagentTarget",
            Self::InferenceBackend => "InferenceBackend",
            Self::InferenceProfile => "InferenceProfile",
            Self::InferenceSampling => "InferenceSampling",
            Self::InferenceExecution => "InferenceExecution",
            Self::InferenceRetryPolicy => "InferenceRetryPolicy",
            Self::ToolServiceRegistry => "ToolServiceRegistry",
            Self::ProjectionAcpBinding => "ProjectionAcpBinding",
            Self::Task => "Task",
            Self::Schedule => "Schedule",
            Self::EventSource => "EventSource",
            Self::Trigger => "Trigger",
            Self::Callback => "Callback",
            Self::CallbackBinding => "CallbackBinding",
            Self::CallbackModule => "CallbackModule",
            Self::RepositoryPlacement => "RepositoryPlacement",
            Self::GraphDefinition => "GraphDefinition",
        }
    }

    /// Logical document key within agent_did, never global uniqueness.
    pub fn unique_field(self) -> &'static str {
        match self {
            Self::AgentPrincipal => "agent_did",
            Self::AgentBehavior => "behavior_id",
            Self::AgentContext => "context_id",
            Self::Compaction => "compaction_id",
            Self::Skill => "skill_id",
            Self::DatastoreToolSurface => "surface_id",
            Self::ChainKeyBinding => "binding_id",
            Self::EthTool => "tool_id",
            Self::Tools => "tools_id",
            Self::SubagentTarget => "target_id",
            Self::InferenceBackend => "backend_id",
            Self::InferenceProfile => "profile_id",
            Self::InferenceSampling => "sampling_id",
            Self::InferenceExecution => "execution_id",
            Self::InferenceRetryPolicy => "retry_policy_id",
            Self::ToolServiceRegistry => "service_id",
            Self::ProjectionAcpBinding => "binding_id",
            Self::Task => "task_id",
            Self::Schedule => "schedule_id",
            Self::EventSource => "event_source_id",
            Self::Trigger => "trigger_id",
            Self::Callback => "callback_id",
            Self::CallbackBinding => "binding_id",
            Self::CallbackModule => "module_id",
            Self::RepositoryPlacement => "repository_id",
            Self::GraphDefinition => "graph_id",
        }
    }
}

impl fmt::Display for Collection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.dir_name().unwrap_or("agent_principal"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn vocabulary_matches_canonical_lean_document_catalog() {
        // Read the authoritative formal vocabulary, rather than maintaining
        // another hand-written enum/rank fixture in this test.
        let lean = include_str!("../proofs/Proofs/ConfigDocuments.lean");
        let expected: Vec<_> = lean
            .lines()
            .filter_map(|line| {
                let (_, row) = line.split_once("=> ⟨")?;
                let fields: Vec<_> = row.split('"').collect();
                Some((fields[1], fields[3]))
            })
            .collect();
        let actual: Vec<_> = Collection::ALL
            .iter()
            .map(|collection| (collection.graphql_type(), collection.unique_field()))
            .collect();
        assert!(!expected.is_empty(), "formal document catalog not found");
        assert_eq!(actual, expected);
    }

    #[test]
    fn authored_names_match_pack_config_document_roots() {
        let pack = include_str!("document_config/pack_config.rs");
        let roots: BTreeSet<_> = pack
            .lines()
            .filter_map(|line| {
                let line = line.trim().strip_prefix("pub ")?;
                let (name, _) = line.split_once(':')?;
                Some(name)
            })
            .filter(|name| !matches!(*name, "graph_intents" | "graph_capabilities"))
            .collect();
        // Graph intents and capabilities are composition inputs, not document
        // collections; every other canonical pack root must be represented.
        let names: BTreeSet<_> = Collection::ALL.iter().map(|c| c.to_string()).collect();
        assert_eq!(names.len(), Collection::ALL.len());
        assert_eq!(
            names.iter().map(String::as_str).collect::<BTreeSet<_>>(),
            roots
        );
        for c in Collection::ALL {
            assert!(c.file_name().is_some() ^ c.dir_name().is_some());
        }
    }

    #[test]
    fn traversal_covers_each_document_collection_once() {
        let visited: BTreeSet<_> = DESIRED_STATE_APPLY_ORDER.into_iter().collect();
        assert_eq!(visited.len(), DESIRED_STATE_APPLY_ORDER.len());
        assert_eq!(visited, Collection::ALL.into_iter().collect());
        let names: BTreeSet<_> = Collection::ALL.iter().map(|c| c.graphql_type()).collect();
        assert_eq!(names.len(), Collection::ALL.len());
    }
}
