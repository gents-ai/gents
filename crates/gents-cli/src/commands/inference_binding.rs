//! Shared node-scoped inference selection for protocol adapters.

use anyhow::{anyhow, Result};
use gents::defra_node::EmbeddedNode;
use gents::load_node;

pub(crate) async fn load_bound_profile(
    node: &EmbeddedNode,
    node_did: &str,
    agent_id: &str,
) -> Result<gents::document_config::InferenceProfile> {
    use gents::config_client::{read_desired_state_record_in_txn as read, ConfigAccess};
    use gents::Collection;
    ConfigAccess::transact_local(node, None, "codex.bound_profile", |txn| {
        Box::pin(async move {
            let agent: gents::document_config::Agent = serde_json::from_value(
                read(txn, Collection::Agent, node_did, agent_id)
                    .await?
                    .map(|(_, value)| value)
                    .ok_or_else(|| anyhow!("agent {agent_id:?} missing for {node_did:?}"))?,
            )?;
            anyhow::ensure!(agent.enabled, "bound agent is disabled");
            let profile: gents::document_config::InferenceProfile = serde_json::from_value(
                read(
                    txn,
                    Collection::InferenceProfile,
                    node_did,
                    &agent.inference_profile_id,
                )
                .await?
                .map(|(_, value)| value)
                .ok_or_else(|| anyhow!("bound inference profile missing"))?,
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
                .ok_or_else(|| anyhow!("bound inference backend missing"))?,
            )?;
            backend.validate()?;
            anyhow::ensure!(backend.enabled, "bound inference backend is disabled");
            Ok(profile)
        })
    })
    .await
}

pub(crate) async fn load_bound_context_window(
    node: &EmbeddedNode,
    node_did: &str,
    agent_id: &str,
) -> Result<i64> {
    let profile = load_bound_profile(node, node_did, agent_id).await?;
    let backend = gents::backend_registry::lookup_backend(node, node_did, &profile.backend_id)
        .await?
        .ok_or_else(|| anyhow!("bound backend disappeared"))?;
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
        let model = models
            .next()
            .ok_or_else(|| anyhow!("selected model is absent from backend catalog"))?;
        anyhow::ensure!(models.next().is_none(), "ambiguous selected backend model");
        model.context_window
    } else {
        None
    };
    let value = profile
        .context_window
        .or(advertised)
        .unwrap_or(gents::DEFAULT_CONTEXT_WINDOW as i64);
    anyhow::ensure!(value > 0, "context window must be positive");
    Ok(value)
}

/// Resolve the agent the Codex shim binds to.
///
/// An explicit override always wins. Otherwise the exact principal document
/// and its configured default agent are required.
pub(crate) async fn resolve_bound_agent_id(
    node: &EmbeddedNode,
    override_agent_id: Option<&str>,
    node_did: &str,
) -> Result<String> {
    if let Some(value) = explicit_agent_override(override_agent_id) {
        return Ok(value);
    }
    let principal = load_node(node, node_did)
        .await?
        .ok_or_else(|| anyhow!("node {node_did:?} is not configured"))?;
    principal
        .default_agent_id
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow!("node {node_did:?} has no default agent binding"))
}

/// Normalize an optional adapter agent selector before resolving documents.
pub(crate) fn explicit_agent_override(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}
