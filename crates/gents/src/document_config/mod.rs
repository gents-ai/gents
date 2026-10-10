mod behavior;
mod callback;
mod chain_key_binding;
pub use chain_key_binding::preserve_chain_key_binding_update_fields;
mod compaction;
mod context;
mod datastore_tool_surface;
mod eth_tool;
mod eval_definition;
mod event_trigger;
mod graph_definition;
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
mod serde_helpers;
mod skill;
mod surface_tool;
mod task;
mod task_validation;
mod tools;
mod trigger;
mod write_tool;

#[cfg(test)]
pub(crate) use principal::upsert_node;
pub use principal::{load_node, Node, DEFAULT_MAX_REQUEST_HOP};

pub use callback::{
    BuiltInCallback, Callback, CallbackBinding, CallbackHandler, CallbackInvocationOrigin,
    CallbackModule,
};
pub use compaction::CompactionConfig;
pub use context::AgentContext;
pub use eval_definition::{
    EvalCapture, EvalCase, EvalCheckRef, EvalContinuation, EvalDefinition, EvalFixtureDocument,
    EvalFixtureFile, EvalFixtures, EvalReducer, EvalSplit, EvalStage, EvalSubject, EvalSubjectKind,
    EvalTier, LLM_JUDGE_CHECK,
};
pub use graph_definition::{GraphDefinition, GraphDefinitionObservation};
pub use installation::{
    ProjectionAcpBinding, ProjectionAcpObservation, RepositoryPlacement, ToolServiceRegistry,
};
pub use pack_config::PackConfig;
pub use references::{ConfigReferences, MissingReference};

#[allow(unused_imports)]
pub(crate) use behavior::{list_agent_records, load_agent_record};
pub use behavior::{list_agents, load_agent, upsert_agent, Agent};

pub use inference_backend::{
    AdvertisedModel, BackendAuth, BackendModelCatalog, InferenceBackend,
    InferenceBackendObservation,
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
pub(crate) use tools::load_agent_tools_in_txn;
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

pub use chain_key_binding::{
    chain_key_binding_by_id_query, create_chain_key_binding_mutation,
    delete_chain_key_binding_mutation, list_chain_key_binding_records,
    list_chain_key_bindings_query, load_chain_key_binding_by_doc_id, upsert_chain_key_binding,
    upsert_chain_key_binding_mutation, ChainKeyBindingDocument,
};
pub use datastore_tool_surface::{list_datastore_tool_surfaces, DatastoreToolSurfaceDocument};
pub use eth_tool::{eth_tool_by_id_query, list_eth_tools, EthToolDocument};
pub use skill::SkillDocument;

#[allow(unused_imports)]
pub(crate) use event_trigger::{
    load_trigger_next_run_at, update_trigger_runtime_fields, TriggerRuntimeUpdate,
};
pub use event_trigger::{EventGroup, EventGroupCount, EventSource};
pub use schedule::{Schedule, ScheduleCadence, ScheduleObservation};
#[allow(unused_imports)]
pub use task::{Task, TaskHook, TaskHookPhase};
pub use trigger::{ConcurrencyMode, Trigger, TriggerObservation, TriggerSource};

use anyhow::Result;
use defra_node::EmbeddedNode;

pub fn default_agent_id_for_node(node_did: &str) -> String {
    format!("{node_did}:default")
}

/// Ensure the runtime's node exists without inventing executable configuration.
/// Packs or explicit configuration select agents, contexts and inference.
pub async fn ensure_node(node: &EmbeddedNode, node_did: &str) -> Result<Node> {
    use crate::collection::Collection;
    use crate::config_client::{ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan};
    anyhow::ensure!(!node_did.trim().is_empty(), "node DID must not be blank");
    let owner = node_did.to_owned();
    let existing: Option<Node> =
        ConfigAccess::transact_local_readonly(node, None, "ensure_node.read", |txn| {
            let owner = &owner;
            Box::pin(async move {
                crate::config_client::read_desired_state_record_in_txn(
                    txn,
                    Collection::Node,
                    owner,
                    owner,
                )
                .await?
                .map(|(_, value)| serde_json::from_value(value).map_err(Into::into))
                .transpose()
            })
        })
        .await?;
    if let Some(principal) = existing {
        return Ok(principal);
    }
    ConfigAccess::transact_local(node, None, "ensure_node", move |txn| {
        let owner = owner.clone();
        Box::pin(async move {
            if let Some((_, value)) = crate::config_client::read_desired_state_record_in_txn(
                txn,
                Collection::Node,
                &owner,
                &owner,
            )
            .await?
            {
                return Ok(serde_json::from_value(value)?);
            }
            let principal = Node {
                node_did: owner.clone(),
                display_name: Some(serde_helpers::default_display_name_for_did(&owner)),
                default_agent_id: None,
                enabled: true,
                created_at: Some(chrono::Utc::now().to_rfc3339()),
                created_by: Some(owner),
                max_request_hop: None,
                tags: Vec::new(),
            };
            let value = serde_json::to_value(&principal)?;
            let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                collection: Collection::Node,
                add: value.clone(),
                update: value,
            }])?;
            crate::config_client::apply_desired_state_plan(txn, &plan).await?;
            Ok(principal)
        })
    })
    .await
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod config_model_tests;

#[cfg(test)]
mod principal_bootstrap_tests {
    use super::*;

    #[tokio::test]
    async fn bootstrap_is_idempotent_and_does_not_invent_executable_configuration() -> Result<()> {
        let node = EmbeddedNode::builder().build().await?;
        crate::ensure_runtime_schemas(&node).await?;
        let principal = ensure_node(&node, "did:key:bootstrap").await?;
        assert_eq!(principal.default_agent_id, None);
        assert_eq!(ensure_node(&node, "did:key:bootstrap").await?, principal);
        let response = node.execute("{ Node { _docID } Agent { _docID } AgentContext { _docID } InferenceProfile { _docID } InferenceBackend { _docID } }").await;
        anyhow::ensure!(
            !response.has_errors(),
            "bootstrap inspection failed: {:?}",
            response.errors
        );
        let data = response.data.as_ref().unwrap();
        assert_eq!(data["Node"].as_array().unwrap().len(), 1);
        for collection in [
            "Agent",
            "AgentContext",
            "InferenceProfile",
            "InferenceBackend",
        ] {
            assert!(
                data[collection].as_array().unwrap().is_empty(),
                "unexpected {collection}"
            );
        }
        Ok(())
    }
}
