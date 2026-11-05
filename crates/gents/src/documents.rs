mod behavior;
mod callback;
mod compaction;
mod context;
mod datastore_tool_surface;
mod eval_definition;
mod event_trigger;
mod inference_backend;
mod inference_execution;
mod inference_profile;
mod inference_sampling;
mod installation;
mod installation_validation;
mod projection_acp;
pub use projection_acp::parse_projection_resource_map;
mod agent_target;
mod pack_config;
mod principal;
mod references;
mod schedule;
pub mod schedule_cron;
mod serde_helpers;
mod skill;
pub mod skills;
mod surface_tool;
mod task;
mod task_validation;
pub mod template;
mod tools;
mod trigger;
mod write_tool;

#[cfg(test)]
pub(crate) use principal::upsert_node;
pub use principal::{load_node, Node};

pub use callback::{
    BuiltInCallback, Callback, CallbackBinding, CallbackHandler, CallbackInvocationOrigin,
    CallbackModule, PortCardinality, PortSpec, WorkspaceAgentSelection,
};
pub use compaction::CompactionConfig;
pub use context::AgentContext;
pub use eval_definition::{
    EvalCapture, EvalCase, EvalCheckRef, EvalContinuation, EvalDefinition, EvalFixtureDocument,
    EvalFixtureFile, EvalFixtures, EvalReducer, EvalSplit, EvalStage, EvalSubject, EvalSubjectKind,
    EvalTier, LLM_JUDGE_CHECK,
};
pub use installation::{
    ProjectionAcpBinding, ProjectionAcpObservation, RepositoryPlacement, ToolServiceRegistry,
};
pub use pack_config::PackConfig;
pub use references::{ConfigReferences, MissingReference};

#[allow(unused_imports)]
pub(crate) use behavior::{list_agent_records, load_agent_record};
pub use behavior::{list_agents, load_agent, upsert_agent, Agent};

pub use inference_backend::{
    AdvertisedModel, BackendAuth, BackendAuthSelection, BackendAuthSelectionFields,
    BackendModelCatalog, ClaudePlatform, InferenceBackend, InferenceBackendMetadata,
    InferenceBackendObservation, InvalidBackendAuthSelection,
};
pub use inference_execution::{
    InferenceExecution, InferenceRetryPolicy, MAX_DEADLINE_DURATION_SECS,
};
pub use inference_sampling::InferenceSampling;

#[allow(unused_imports)]
pub(crate) use inference_profile::load_inference_profile_record;
pub use inference_profile::{
    default_inference_profile_id_for_agent, default_inference_profile_id_for_node,
    list_inference_profile_records, load_inference_profile, upsert_inference_profile,
    InferenceProfile,
};

pub(crate) use serde_helpers::deserialize_default_on_null;
pub use serde_helpers::deserialize_dual_shape;
#[allow(unused_imports)]
pub(crate) use surface_tool::{
    deserialize_optional_surface_tools, validate_query_tool_declarations,
    validate_surface_tool_names,
};
pub use surface_tool::{
    merge_datastore_tool_surfaces, MergedSurfaceTools, QueryToolDecl, SurfaceToolDecl,
};
#[cfg(test)]
pub(crate) use surface_tool::{confirm_visible_datastore_tool, revalidate_datastore_tool_in_txn};
pub(crate) use surface_tool::{
    validate_datastore_surface, DatastoreSurfaceScope, DatastoreToolSource,
};
pub use tools::{load_agent_context_and_tools_in_txn, load_agent_tools_in_txn};
pub use tools::{unwire_plugin, wire_plugin, WireReport};
pub use tools::{
    AgentTools, BashTools, BuiltInTools, CliTool, DatastoreTools, FileTools, HostTools,
    IntegrationTools, LspTools, PluginToolRef, RemoteServiceTools, RemoteToolStyle, RemoteTools,
    SelfConfigTools, Tools,
};
pub use write_tool::{
    is_reserved_builtin_tool_name, runtime_filled_refusal, OutputObligationDecision, WriteToolDecl,
    WriteToolField, WriteToolFieldFill, WriteToolOutputObligation, WriteToolOutputObligationScope,
    PROTECTED_DATASTORE_COLLECTIONS,
};
pub(crate) use write_tool::{reject_protected_collection_name, undeclared_field_refusal};

pub use agent_target::AgentTargetDocument;

pub use datastore_tool_surface::{list_datastore_tool_surfaces, DatastoreToolSurfaceDocument};
pub use skill::SkillDocument;

#[cfg(test)]
pub(crate) use event_trigger::count_trigger_cursor_batch_queries;
#[allow(unused_imports)]
pub(crate) use event_trigger::{
    load_trigger_next_run_at_batch, update_trigger_runtime_fields, TriggerRuntimeUpdate,
    TRIGGER_CURSOR_BATCH_SIZE,
};
pub use event_trigger::{EventGroup, EventGroupCount, EventSource};
pub use schedule::{Schedule, ScheduleCadence, ScheduleObservation};
#[allow(unused_imports)]
pub use task::{Task, TaskHook, TaskHookPhase};
pub use trigger::{ConcurrencyMode, Trigger, TriggerObservation, TriggerSource};

use anyhow::Result;
use defra_node::Node as EmbeddedNode;

/// Runtime startup requires a Node created by an authenticated client.
pub async fn ensure_node(node: &std::sync::Arc<EmbeddedNode>, node_did: &str) -> Result<Node> {
    use anyhow::Context;
    anyhow::ensure!(!node_did.trim().is_empty(), "node DID must not be blank");
    load_node(node, node_did)
        .await?
        .context("Node must be created by an authenticated client")
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod claude_platform_tests;
#[cfg(test)]
mod config_model_tests;
