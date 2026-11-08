mod event_grants;

use anyhow::{Context, Result};
use serde_json::Value;

use super::inference_backend::{
    prepare_backend_metadata_in_txn, remove_backend_metadata_in_txn, write_backend_metadata_in_txn,
};
use super::InferenceBackend;
use crate::store::config_client::{
    ConfigApplyTxn, ConfigurationNodeBinding, DesiredStateApplyCounts, DesiredStateApplyPlan,
    DesiredStateSemantics, PendingEventGrants,
};
use crate::Collection;

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
        let normalized = crate::store::config_client::config_projection(
            Collection::InferenceBackend,
            Some(&projected),
        )?
        .1
        .context("backend metadata projection missing")?;
        if credential.is_some() || existing.is_none_or(|value| value != &normalized) {
            crate::OpenAiWireApi::warn_if_ignored(
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
) -> Result<DesiredStateApplyCounts> {
    apply_desired_state_plan_with_node_binding(txn, plan, None).await
}

pub(crate) async fn apply_desired_state_plan_with_node_binding(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
    binding: Option<&ConfigurationNodeBinding>,
) -> Result<DesiredStateApplyCounts> {
    crate::store::config_client::apply_desired_state_plan_with_semantics(
        txn,
        plan,
        binding,
        &DocumentSemantics,
    )
    .await
}

pub(crate) async fn validate_desired_state_plan(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
) -> Result<()> {
    crate::store::config_client::validate_desired_state_plan(txn, plan).await
}
