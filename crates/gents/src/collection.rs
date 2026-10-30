use std::fmt;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum Collection {
    Node,
    Agent,
    AgentContext,
    Compaction,
    Skill,
    DatastoreToolSurface,
    Tools,
    AgentTarget,
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
    EvalDefinition,
}

/// This enumeration provides no reference-safety guarantee; publication must
/// validate the complete manifest and atomically commit it, including legal
/// reference cycles.
pub const DESIRED_STATE_APPLY_ORDER: [Collection; 24] = Collection::ALL;

impl Collection {
    pub const ALL: [Collection; 24] = [
        Self::Node,
        Self::Agent,
        Self::AgentContext,
        Self::Compaction,
        Self::Skill,
        Self::DatastoreToolSurface,
        Self::Tools,
        Self::AgentTarget,
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
        Self::EvalDefinition,
    ];

    pub fn file_name(self) -> Option<&'static str> {
        match self {
            Self::Node => Some("node.json"),
            _ => None,
        }
    }

    pub fn dir_name(self) -> Option<&'static str> {
        match self {
            Self::Node => None,
            Self::Agent => Some("agents"),
            Self::AgentContext => Some("contexts"),
            Self::Compaction => Some("compactions"),
            Self::Skill => Some("skills"),
            Self::DatastoreToolSurface => Some("datastore_tool_surfaces"),
            Self::Tools => Some("tools"),
            Self::AgentTarget => Some("agent_targets"),
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
            Self::EvalDefinition => Some("eval_definitions"),
        }
    }

    pub fn graphql_type(self) -> &'static str {
        match self {
            Self::Node => "Node",
            Self::Agent => "Agent",
            Self::AgentContext => "AgentContext",
            Self::Compaction => "CompactionConfig",
            Self::Skill => "Skill",
            Self::DatastoreToolSurface => "DatastoreToolSurface",
            Self::Tools => "Tools",
            Self::AgentTarget => "AgentTarget",
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
            Self::EvalDefinition => "EvalDefinition",
        }
    }

    /// Logical document key within node_did, never global uniqueness.
    pub fn unique_field(self) -> &'static str {
        match self {
            Self::Node => "node_did",
            Self::Agent => "agent_id",
            Self::AgentContext => "context_id",
            Self::Compaction => "compaction_id",
            Self::Skill => "skill_id",
            Self::DatastoreToolSurface => "surface_id",
            Self::Tools => "tools_id",
            Self::AgentTarget => "target_id",
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
            Self::EvalDefinition => "definition_id",
        }
    }
}

impl fmt::Display for Collection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.dir_name().unwrap_or("node"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn vocabulary_matches_canonical_lean_document_catalog() {
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
            .collect();
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
    fn every_apply_collection_declares_the_recreate_identity_stamp() {
        for collection in Collection::ALL {
            let name = collection.graphql_type();
            let baseline = crate::migration::DEFAULT_BASELINE
                .iter()
                .find(|entry| entry.name == name)
                .unwrap_or_else(|| panic!("{name} has no baseline schema"));
            assert!(
                baseline
                    .sdl
                    .lines()
                    .any(|line| line.trim_start().starts_with("updated_at:")),
                "{name} must declare the updated_at that mint_recreate_identity stamps"
            );
        }
    }
}
