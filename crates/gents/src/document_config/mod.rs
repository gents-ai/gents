mod behavior;
mod callback;
mod chain_key_binding;
pub use chain_key_binding::preserve_chain_key_binding_update_fields;
mod compaction;
mod context;
mod datastore_tool_surface;
mod eth_tool;
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
mod pack_config;
mod principal;
mod references;
mod schedule;
mod serde_helpers;
mod skill;
mod subagent_target;
mod surface_tool;
mod task;
mod task_validation;
mod tools;
mod trigger;
mod write_tool;

pub use principal::{AgentPrincipal, load_agent_principal, upsert_agent_principal};

pub use callback::{
    BuiltInCallback, Callback, CallbackBinding, CallbackHandler, CallbackInvocationOrigin,
    CallbackModule,
};
pub use compaction::CompactionConfig;
pub use context::AgentContext;
pub use graph_definition::{GraphDefinition, GraphDefinitionObservation};
pub use installation::{
    ProjectionAcpBinding, ProjectionAcpObservation, RepositoryPlacement, ToolServiceRegistry,
};
pub use pack_config::PackConfig;
pub use references::ConfigReferences;

pub use behavior::{
    AgentBehavior, list_agent_behaviors, load_agent_behavior, upsert_agent_behavior,
};
#[allow(unused_imports)]
pub(crate) use behavior::{
    list_agent_behavior_records, load_agent_behavior_record,
};

pub use inference_backend::{
    AdvertisedModel, BackendAuth, BackendModelCatalog, InferenceBackend,
    InferenceBackendObservation,
};
pub use inference_execution::{InferenceExecution, InferenceRetryPolicy};
pub use inference_sampling::InferenceSampling;

#[allow(unused_imports)]
pub(crate) use inference_profile::load_inference_profile_record;
pub use inference_profile::{
    InferenceProfile, default_inference_profile_id_for_behavior, list_inference_profile_records,
    load_inference_profile, upsert_inference_profile,
};

pub(crate) use serde_helpers::deserialize_default_on_null;
pub use serde_helpers::deserialize_dual_shape;
pub use surface_tool::{
    MergedSurfaceTools, QueryToolDecl, SurfaceToolDecl, merge_datastore_tool_surfaces,
};
#[allow(unused_imports)]
pub(crate) use surface_tool::{
    deserialize_optional_surface_tools, validate_query_tool_declarations,
    validate_surface_tool_names,
};
pub use tools::{
    BashTools, BuiltInTools, CliTool, DatastoreTools, FileTools, HostTools, IntegrationTools,
    LspTools, RemoteServiceTools, RemoteToolStyle, RemoteTools, SelfConfigTools, SubagentTools,
    Tools,
};
pub(crate) use write_tool::validate_write_tool_declarations;
pub use write_tool::{
    OutputObligationDecision, WriteToolDecl, WriteToolField, WriteToolFieldFill,
    WriteToolOutputObligation, WriteToolOutputObligationScope, is_reserved_builtin_tool_name,
};

pub use subagent_target::SubagentTargetDocument;

pub use chain_key_binding::{
    ChainKeyBindingDocument, chain_key_binding_by_id_query, create_chain_key_binding_mutation,
    delete_chain_key_binding_mutation, list_chain_key_binding_records,
    list_chain_key_bindings_query, load_chain_key_binding_by_doc_id, upsert_chain_key_binding,
    upsert_chain_key_binding_mutation,
};
pub use datastore_tool_surface::{DatastoreToolSurfaceDocument, list_datastore_tool_surfaces};
pub use eth_tool::{EthToolDocument, eth_tool_by_id_query, list_eth_tools};
pub(crate) use eth_tool::list_eth_tool_records;
pub use skill::SkillDocument;

pub use event_trigger::{EventGroup, EventGroupCount, EventSource};
#[allow(unused_imports)]
pub(crate) use event_trigger::{
    TriggerRuntimeUpdate, load_trigger_next_run_at, update_trigger_runtime_fields,
};
pub use schedule::{Schedule, ScheduleCadence, ScheduleObservation};
#[allow(unused_imports)]
pub use task::{Task, TaskHook, TaskHookPhase};
pub use trigger::{ConcurrencyMode, Trigger, TriggerObservation, TriggerSource};

use anyhow::Result;
use defra_node::EmbeddedNode;

pub fn default_behavior_id_for_agent(agent_did: &str) -> String {
    format!("{agent_did}:default")
}

/// Ensure the runtime's principal exists without inventing executable configuration.
/// Packs or explicit configuration select behaviors, contexts and inference.
pub async fn ensure_agent_principal(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<AgentPrincipal> {
    use crate::collection::Collection;
    use crate::config_client::{ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan};
    anyhow::ensure!(
        !agent_did.trim().is_empty(),
        "principal DID must not be blank"
    );
    let owner = agent_did.to_owned();
    ConfigAccess::transact_local(node, None, "ensure_agent_principal", move |txn| {
        let owner = owner.clone();
        Box::pin(async move {
            if let Some((_, value)) = crate::config_client::read_desired_state_record_in_txn(
                txn,
                Collection::AgentPrincipal,
                &owner,
                &owner,
            )
            .await?
            {
                return Ok(serde_json::from_value(value)?);
            }
            let principal = AgentPrincipal {
                agent_did: owner.clone(),
                display_name: Some(serde_helpers::default_display_name_for_did(&owner)),
                default_behavior_id: None,
                enabled: true,
                created_at: Some(chrono::Utc::now().to_rfc3339()),
                created_by: Some(owner),
                tags: Vec::new(),
            };
            let value = serde_json::to_value(&principal)?;
            let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                collection: Collection::AgentPrincipal,
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
        let principal = ensure_agent_principal(&node, "did:key:bootstrap").await?;
        assert_eq!(principal.default_behavior_id, None);
        assert_eq!(
            ensure_agent_principal(&node, "did:key:bootstrap").await?,
            principal
        );
        let response = node.execute("{ AgentPrincipal { _docID } AgentBehavior { _docID } AgentContext { _docID } InferenceProfile { _docID } InferenceBackend { _docID } }").await;
        anyhow::ensure!(
            !response.has_errors(),
            "bootstrap inspection failed: {:?}",
            response.errors
        );
        let data = response.data.as_ref().unwrap();
        assert_eq!(data["AgentPrincipal"].as_array().unwrap().len(), 1);
        for collection in [
            "AgentBehavior",
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
