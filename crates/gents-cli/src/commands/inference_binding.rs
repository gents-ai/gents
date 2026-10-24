//! Shared node-scoped inference selection for protocol adapters.

use anyhow::Result;
use gents::defra_node::EmbeddedNode;
use gents::load_node;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub(crate) enum InferenceBindingError {
    #[error("agent {agent_id:?} missing for {node_did:?}")]
    AgentMissing { agent_id: String, node_did: String },
    #[error("bound agent is disabled")]
    AgentDisabled,
    #[error("bound inference profile missing")]
    ProfileMissing,
    #[error("bound inference backend missing")]
    BackendMissing,
    #[error("bound inference backend is disabled")]
    BackendDisabled,
    #[error("bound backend disappeared")]
    BackendDisappeared,
    #[error("selected model is absent from backend catalog")]
    ModelAbsent,
    #[error("ambiguous selected backend model")]
    AmbiguousModel,
    #[error("context window must be positive")]
    NonPositiveContextWindow,
    #[error("node {node_did:?} is not configured")]
    NodeNotConfigured { node_did: String },
    #[error("node {node_did:?} has no default agent binding")]
    NoDefaultAgent { node_did: String },
    #[error(transparent)]
    LoadNode(Box<gents::document_config::DocumentConfigError>),
}

pub(crate) async fn load_bound_profile(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_id: &str,
) -> Result<gents::document_config::InferenceProfile> {
    use gents::config_client::{read_desired_state_record_in_txn as read, ConfigAccess};
    use gents::Collection;
    ConfigAccess::transact_local_write(node, None, "codex.bound_profile", |txn| {
        Box::pin(async move {
            let agent: gents::document_config::Agent = serde_json::from_value(
                read(txn, Collection::Agent, node_did, agent_id)
                    .await?
                    .map(|(_, value)| value)
                    .ok_or_else(|| InferenceBindingError::AgentMissing {
                        agent_id: agent_id.to_owned(),
                        node_did: node_did.to_owned(),
                    })?,
            )?;
            if !agent.enabled {
                return Err(InferenceBindingError::AgentDisabled.into());
            }
            let profile: gents::document_config::InferenceProfile = serde_json::from_value(
                read(
                    txn,
                    Collection::InferenceProfile,
                    node_did,
                    &agent.inference_profile_id,
                )
                .await?
                .map(|(_, value)| value)
                .ok_or(InferenceBindingError::ProfileMissing)?,
            )?;
            profile.validate()?;
            let backend: gents::document_config::InferenceBackend = serde_json::from_value(
                read(
                    txn,
                    Collection::InferenceBackend,
                    node_did,
                    &profile.backend_id,
                )
                .await?
                .map(|(_, value)| value)
                .ok_or(InferenceBindingError::BackendMissing)?,
            )?;
            backend.validate()?;
            if !backend.enabled {
                return Err(InferenceBindingError::BackendDisabled.into());
            }
            Ok(profile)
        })
    })
    .await
}

pub(crate) async fn load_bound_context_window(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_id: &str,
) -> Result<i64> {
    let profile = load_bound_profile(node, node_did, agent_id).await?;
    let backend = gents::backend_registry::lookup_backend(node, node_did, &profile.backend_id)
        .await?
        .ok_or(InferenceBindingError::BackendDisappeared)?;
    let observation =
        gents::backend_registry::lookup_backend_observation(node, node_did, &profile.backend_id)
            .await?;
    let credential_scope = matches!(
        backend.auth,
        gents::document_config::BackendAuth::NodeOAuth { .. }
    )
    .then_some(node_did);
    let catalog = observation
        .as_ref()
        .map(|observation| observation.catalog_for(credential_scope))
        .transpose()?
        .flatten();
    let advertised = if let Some(catalog) = catalog {
        let mut models = catalog
            .models
            .iter()
            .filter(|model| model.model_name == profile.model_name);
        let model = models.next().ok_or(InferenceBindingError::ModelAbsent)?;
        if models.next().is_some() {
            return Err(InferenceBindingError::AmbiguousModel.into());
        }
        model.context_window
    } else {
        None
    };
    let value = profile
        .context_window
        .or(advertised)
        .unwrap_or(gents::DEFAULT_CONTEXT_WINDOW as i64);
    if value <= 0 {
        return Err(InferenceBindingError::NonPositiveContextWindow.into());
    }
    Ok(value)
}

pub(crate) async fn resolve_bound_agent_id(
    node: &EmbeddedNode,
    override_agent_id: Option<&str>,
    node_did: &str,
) -> Result<String, InferenceBindingError> {
    if let Some(value) = explicit_agent_override(override_agent_id) {
        return Ok(value);
    }
    let principal = load_node(node, node_did)
        .await
        .map_err(|error| InferenceBindingError::LoadNode(Box::new(error)))?
        .ok_or_else(|| InferenceBindingError::NodeNotConfigured {
            node_did: node_did.to_owned(),
        })?;
    principal
        .default_agent_id
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .ok_or_else(|| InferenceBindingError::NoDefaultAgent {
            node_did: node_did.to_owned(),
        })
}

pub(crate) fn explicit_agent_override(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}
