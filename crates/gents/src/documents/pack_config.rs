use super::*;
use serde::{Deserialize, Serialize};

/// Canonical materialized document bundle for packs; the
/// loader fills missing root node_did from PackInstallOptions and explicit
/// owners must match. Validate ownership, references, unknown authoring keys,
/// and resource bounds before persistence.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct PackConfig {
    pub node: Node,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<Agent>>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub agents: Vec<Agent>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<AgentContext>>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub contexts: Vec<AgentContext>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<CompactionConfig>>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub compactions: Vec<CompactionConfig>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<Tools>>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub tools: Vec<Tools>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<AgentTargetDocument>>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub agent_targets: Vec<AgentTargetDocument>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<SkillDocument>>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub skills: Vec<SkillDocument>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<DatastoreToolSurfaceDocument>>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub datastore_tool_surfaces: Vec<DatastoreToolSurfaceDocument>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<InferenceBackend>>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub inference_backends: Vec<InferenceBackend>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<InferenceProfile>>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub inference_profiles: Vec<InferenceProfile>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<InferenceSampling>>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub inference_sampling: Vec<InferenceSampling>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<InferenceExecution>>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub inference_execution: Vec<InferenceExecution>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<InferenceRetryPolicy>>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub inference_retry_policies: Vec<InferenceRetryPolicy>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<ToolServiceRegistry>>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub tool_service_registries: Vec<ToolServiceRegistry>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<ProjectionAcpBinding>>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub projection_acp_bindings: Vec<ProjectionAcpBinding>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<Task>>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub tasks: Vec<Task>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<Trigger>>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub triggers: Vec<Trigger>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<Schedule>>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub schedules: Vec<Schedule>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<EventSource>>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub event_sources: Vec<EventSource>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<Callback>>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub callbacks: Vec<Callback>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<CallbackBinding>>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub callback_bindings: Vec<CallbackBinding>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<CallbackModule>>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub callback_modules: Vec<CallbackModule>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<RepositoryPlacement>>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub repository_placements: Vec<RepositoryPlacement>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<EvalDefinition>>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub eval_definitions: Vec<EvalDefinition>,
}
