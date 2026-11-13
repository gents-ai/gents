mod agent_target_access;
mod apply;
mod declared_readers;
mod event_grants;
pub(crate) mod plan;
pub(crate) mod projection;
pub(crate) mod read;
pub(super) mod schema_fields;
pub(super) mod selection;
mod self_config_inference;
pub(crate) mod semantics;
pub(super) mod task_selection;
pub(super) mod validate;
mod workspace_selection;

use anyhow::{Context, Result};
use serde_json::Value;

use super::inference_backend::AdvertisedProfilePolicy;
use super::inference_backend::{
    prepare_backend_metadata_in_txn, remove_backend_metadata_in_txn, write_backend_metadata_in_txn,
};
use super::InferenceBackend;
#[cfg(test)]
use apply::confirm_removal;
pub(crate) use apply::find_node_doc_id;
pub use apply::{configuration_node_doc, ConfigurationNodeBinding};
pub use apply::{
    grant_agent_catalog, grant_node_administration, grant_selected_self_configuration,
    revoke_agent_catalog,
};
use gents_store::collection::Collection;
use gents_store::config_client::ConfigApplyTxn;
#[cfg(test)]
use gents_store::graphql::escape_graphql_string;
#[cfg(test)]
use plan::document_identity;
use plan::{DesiredStateApplyCounts, DesiredStateApplyPlan};
use semantics::{DesiredStateSemantics, PendingEventGrants};

/// The configuration-document meaning of a desired-state apply: the
/// InferenceBackend metadata and credential split, event grants, and event
/// consumer cursors. The store apply calls it at its fixed extension points.
pub(crate) struct DocumentSemantics;

#[async_trait::async_trait]
impl DesiredStateSemantics for DocumentSemantics {
    async fn capture_event_grants(
        &self,
        txn: &ConfigApplyTxn<'_>,
        plan: &DesiredStateApplyPlan,
    ) -> Result<Box<dyn PendingEventGrants>> {
        Ok(Box::new(event_grants::capture(txn, plan).await?))
    }

    async fn write_inference_backend(
        &self,
        txn: &ConfigApplyTxn<'_>,
        update: &Value,
        existing: Option<&Value>,
    ) -> Result<()> {
        let backend: InferenceBackend = serde_json::from_value(update.clone())?;
        backend.validate()?;
        let (metadata, credential) =
            prepare_backend_metadata_in_txn(txn, backend, existing).await?;
        let projected = serde_json::to_value(metadata.clone().into_backend()?)?;
        let normalized =
            projection::config_projection(Collection::InferenceBackend, Some(&projected))?
                .1
                .context("backend metadata projection missing")?;
        if credential.is_some() || existing.is_none_or(|value| value != &normalized) {
            gents_loop::openai_wire::OpenAiWireApi::warn_if_ignored(
                metadata.provider_kind,
                metadata.openai_wire_api,
                &metadata.backend_id,
            );
            write_backend_metadata_in_txn(txn, &metadata, credential.as_ref())
                .await
                .context("applying InferenceBackend metadata")?;
        }
        Ok(())
    }

    async fn remove_inference_backend(
        &self,
        txn: &ConfigApplyTxn<'_>,
        metadata_doc: &str,
        node_did: &str,
        backend_id: &str,
    ) -> Result<()> {
        remove_backend_metadata_in_txn(txn, metadata_doc, node_did, backend_id).await
    }

    async fn seed_event_consumers(
        &self,
        txn: &ConfigApplyTxn<'_>,
        plan: &DesiredStateApplyPlan,
        binding: Option<&ConfigurationNodeBinding>,
    ) -> Result<()> {
        super::event_consumer::seed_planned_consumers(txn, plan, binding).await
    }
}

#[async_trait::async_trait]
impl PendingEventGrants for event_grants::Capture {
    async fn grant_changes(self: Box<Self>, txn: &ConfigApplyTxn<'_>) -> Result<()> {
        event_grants::grant_changes(txn, *self).await
    }
}

/// Documents commit in the caller's transaction. Managed Agent creates stage
/// their creator-managed relationships in that transaction; the client HTTP
/// publication route retains its separately observed post-commit grants.
/// Reference checks run after staged writes so cyclic configurations do not
/// depend on write order.
pub async fn apply_desired_state_plan(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
    policy: &dyn AdvertisedProfilePolicy,
) -> Result<DesiredStateApplyCounts> {
    apply_desired_state_plan_with_node_binding(txn, plan, None, policy).await
}

pub async fn apply_desired_state_plan_with_node_binding(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
    binding: Option<&ConfigurationNodeBinding>,
    policy: &dyn AdvertisedProfilePolicy,
) -> Result<DesiredStateApplyCounts> {
    apply::apply_desired_state_plan_with_semantics(txn, plan, binding, &DocumentSemantics, policy)
        .await
}

#[cfg(test)]
mod tests;

pub(crate) fn callback_agent_selection() -> gents_store::config_client::CallbackAgentSelection {
    gents_store::config_client::CallbackAgentSelection(|owner, callback| {
        workspace_selection::selected_agents(owner, callback).ok()
    })
}
