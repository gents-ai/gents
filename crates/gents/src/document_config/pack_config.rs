use super::*;
use serde::{Deserialize, Serialize};

/// Canonical authored document bundle for both ordinary and graph packs.
/// The loader resolves sidecars/environment values before decoding these same
/// types. The shared loader fills missing root agent_did from PackInstallOptions;
/// explicit owners must match. Foreign delegation/provenance DIDs are never rewritten.
/// Validate ownership, references, unknown authoring keys, and resource
/// bounds before persistence. Sparse patches use a separate representation.
/// Graph topology/revisions add composition, never another model-selection path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct PackConfig {
    pub agent_principal: AgentPrincipal,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<AgentBehavior>>", optional = nullable))]
    pub agent_behaviors: Vec<AgentBehavior>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<AgentContext>>", optional = nullable))]
    pub contexts: Vec<AgentContext>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<CompactionConfig>>", optional = nullable))]
    pub compactions: Vec<CompactionConfig>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<Tools>>", optional = nullable))]
    pub tools: Vec<Tools>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<SubagentTargetDocument>>", optional = nullable))]
    pub subagent_targets: Vec<SubagentTargetDocument>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<SkillDocument>>", optional = nullable))]
    pub skills: Vec<SkillDocument>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<DatastoreToolSurfaceDocument>>", optional = nullable))]
    pub datastore_tool_surfaces: Vec<DatastoreToolSurfaceDocument>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<ChainKeyBindingDocument>>", optional = nullable))]
    pub chain_key_bindings: Vec<ChainKeyBindingDocument>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<EthToolDocument>>", optional = nullable))]
    pub eth_tools: Vec<EthToolDocument>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<InferenceBackend>>", optional = nullable))]
    pub inference_backends: Vec<InferenceBackend>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<InferenceProfile>>", optional = nullable))]
    pub inference_profiles: Vec<InferenceProfile>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<InferenceSampling>>", optional = nullable))]
    pub inference_sampling: Vec<InferenceSampling>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<InferenceExecution>>", optional = nullable))]
    pub inference_execution: Vec<InferenceExecution>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<InferenceRetryPolicy>>", optional = nullable))]
    pub inference_retry_policies: Vec<InferenceRetryPolicy>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<ToolServiceRegistry>>", optional = nullable))]
    pub tool_service_registries: Vec<ToolServiceRegistry>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<ProjectionAcpBinding>>", optional = nullable))]
    pub projection_acp_bindings: Vec<ProjectionAcpBinding>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<Task>>", optional = nullable))]
    pub tasks: Vec<Task>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<Trigger>>", optional = nullable))]
    pub triggers: Vec<Trigger>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<Schedule>>", optional = nullable))]
    pub schedules: Vec<Schedule>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<EventSource>>", optional = nullable))]
    pub event_sources: Vec<EventSource>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<Callback>>", optional = nullable))]
    pub callbacks: Vec<Callback>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<CallbackBinding>>", optional = nullable))]
    pub callback_bindings: Vec<CallbackBinding>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<CallbackModule>>", optional = nullable))]
    pub callback_modules: Vec<CallbackModule>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<RepositoryPlacement>>", optional = nullable))]
    pub repository_placements: Vec<RepositoryPlacement>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<GraphDefinition>>", optional = nullable))]
    pub graphs: Vec<GraphDefinition>,
    /// Canonical authored topology, compiled after ordinary config resolution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<crate::graph_pipeline::GraphIntent>>", optional = nullable))]
    pub graph_intents: Vec<crate::graph_pipeline::GraphIntent>,
    /// Canonical interfaces referencing installed tasks; no graph asset/model rewrite.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<crate::graph_pipeline::StageCapability>>", optional = nullable))]
    pub graph_capabilities: Vec<crate::graph_pipeline::StageCapability>,
}
