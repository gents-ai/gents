mod behavior;
pub mod callback;
pub mod callback_plugin_handler;
pub mod callback_secret_fields;
mod compaction;
mod context;
mod datastore_tool_surface;
mod eval_definition;
pub mod event_trigger;
pub mod inference_backend;
pub mod inference_defaults;
mod inference_execution;
mod inference_profile;
mod inference_sampling;
mod installation;
mod installation_validation;
mod lsp_catalog;
mod lsp_config;
pub mod mailbox_write_decl;
mod projection_acp;
pub use projection_acp::parse_projection_resource_map;
mod agent_target;
mod agent_tool_config;
pub mod automation;
pub mod backend_credential;
pub(crate) mod desired_state;
pub mod event_consumer;
pub mod event_source_cursor;
mod node_document_publication;
pub mod operations;
mod pack_config;
pub mod pack_spec;
mod page;
pub mod patch;
pub mod plugin_coordinate;
pub mod plugin_job_settings;
mod plugin_source;
mod principal;
mod references;
mod schedule;
pub mod schedule_cron;
mod serde_helpers;
mod skill;
pub mod skills;
mod surface_tool;
mod task;
pub mod task_delivery;
pub mod task_goal_declaration;
mod task_validation;
pub mod template;
pub mod tool_limits;
mod tool_modes;
mod tools;

pub use tool_modes::{BashMode, FileToolMode};
mod trigger;
mod unattended_admission;
mod write_tool;

pub use principal::{
    load_default_agent_did, load_default_agent_did_in_txn, load_node, DefaultAgentSelection, Node,
};

pub use callback::{
    BuiltInCallback, Callback, CallbackBinding, CallbackHandler, CallbackInvocationOrigin,
    CallbackModule, PortCardinality, PortSpec, WorkspaceAgentSelection, WorkspaceReaderSelection,
};
pub use compaction::CompactionConfig;
pub use context::AgentContext;
pub(crate) use desired_state::plan::document_identity;
pub use desired_state::plan::{
    stale_expectation, DesiredStateApplyCounts, DesiredStateApplyDocument, DesiredStateApplyPlan,
    DesiredStateExpectation, DriftedDocument, StaleExpectation,
};
pub use desired_state::projection::canonical_struct_fields;
pub use desired_state::projection::{config_projection, desired_state_document_digest};
pub use desired_state::read::read_desired_state_document_in_txn;
pub use desired_state::read::read_record as read_desired_state_record_in_txn;
pub use desired_state::read::read_records as read_desired_state_records_in_txn;
pub use desired_state::schema_fields::collection_is_installed;
pub use desired_state::validate::{
    event_trigger_document_field_names, validate_event_trigger_document_fields,
};
pub use desired_state::{
    apply_desired_state_plan, grant_agent_catalog, grant_node_administration,
    grant_selected_self_configuration, revoke_agent_catalog,
};
pub use desired_state::{
    apply_desired_state_plan_with_node_binding, configuration_node_doc, ConfigurationNodeBinding,
};
pub use desired_state::{
    validate::validate_desired_state_plan, validate::OutcomeSourceSchemaError,
};
pub use eval_definition::{
    EvalCapture, EvalCase, EvalCheckRef, EvalContinuation, EvalDefinition, EvalFixtureDocument,
    EvalFixtureFile, EvalFixtures, EvalReducer, EvalSplit, EvalStage, EvalSubject, EvalSubjectKind,
    EvalTier, LLM_JUDGE_CHECK,
};
pub use installation::{
    ProjectionAcpBinding, ProjectionAcpObservation, RepositoryPlacement, ToolServiceRegistry,
};
pub use lsp_catalog::{builtin_catalog, CatalogServer};
pub use lsp_config::{apply_overrides, LspConfigDocument};
pub use node_document_publication::publish_node_child_in_txn;
pub use node_document_publication::publish_secret_configuration_in_txn;
pub use pack_config::PackConfig;
pub use references::references_of;
pub(crate) use references::RequiredReference;
pub use references::{ConfigReferences, MissingReference};
pub use unattended_admission::observe_unattended_agent_in_txn;

#[allow(unused_imports)]
pub(crate) use behavior::list_agent_records;
#[allow(unused_imports)]
pub use behavior::load_agent_record;
pub use behavior::{list_agents, load_agent, write_agent_document, Agent};

pub use inference_backend::write_inference_backend_document;
pub use inference_backend::{backend_metadata_fields, write_inference_backend_in_txn};
pub use inference_backend::{list_inference_backends_in_txn, load_inference_backend_in_txn};
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
    list_inference_profile_records, list_inference_profiles_in_txn, load_inference_profile,
    write_inference_profile_document, InferenceProfile,
};

pub use serde_helpers::deserialize_dual_shape;
pub(crate) use serde_helpers::string_list_schema;
pub use serde_helpers::{accepts_null, deserialize_default_on_null, noted, widen_schema};
#[allow(unused_imports)]
pub use surface_tool::validate_surface_tool_names;
#[cfg(any(test, feature = "test-fixtures"))]
pub use surface_tool::{confirm_visible_datastore_tool, revalidate_datastore_tool_in_txn};
#[allow(unused_imports)]
pub(crate) use surface_tool::{
    deserialize_optional_surface_tools, validate_query_tool_declarations,
};
pub use surface_tool::{
    merge_datastore_tool_surfaces, MergedSurfaceTools, QueryToolDecl, SurfaceToolDecl,
};
pub use surface_tool::{validate_datastore_surface, DatastoreSurfaceScope, DatastoreToolSource};
pub use tools::write_tools_document;
pub use tools::{load_agent_context_and_tools_in_txn, load_agent_tools_in_txn};
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
pub use write_tool::{reject_protected_collection_name, undeclared_field_refusal};

pub use agent_target::AgentTargetDocument;
pub use agent_tool_config::AgentToolConfig;

pub use datastore_tool_surface::{list_datastore_tool_surfaces, DatastoreToolSurfaceDocument};
pub use page::{
    is_snake_case_name, is_valid_ux_data_tool_name, list_pages, validate_ux_data, PageDocument,
    UxDataSurface, MAX_PAGE_SOURCE_BYTES, MAX_UX_DATA_SURFACE_ID_LEN, PAGE_AREAS,
};
pub use plugin_source::{
    PluginSourceCase, PluginSourceDocument, MAX_PLUGIN_SOURCE_BYTES, PLUGIN_SOURCE_NAMESPACE,
};
pub use skill::{SkillDocument, SKILL_FIELDS};

#[cfg(test)]
pub use event_trigger::count_trigger_cursor_batch_queries;
#[allow(unused_imports)]
pub use event_trigger::{
    load_trigger_next_run_at_batch, stop_trigger_for_budget, update_trigger_runtime_fields,
    TriggerRuntimeUpdate, TRIGGER_CURSOR_BATCH_SIZE,
};
pub use event_trigger::{EventGroup, EventGroupCount, EventSource};
pub use schedule::{Schedule, ScheduleCadence, ScheduleObservation};
#[allow(unused_imports)]
pub use task::{Task, TaskHook, TaskHookPhase};
pub use trigger::{
    ConcurrencyMode, Trigger, TriggerObservation, TriggerSource, BUDGET_EXHAUSTED_STATUS,
    DEFAULT_MAX_FIRES_PER_HOUR,
};
pub mod trigger_explain;

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
#[cfg(any(test, feature = "test-fixtures"))]
pub mod test_support;
