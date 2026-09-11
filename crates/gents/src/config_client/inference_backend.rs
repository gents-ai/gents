use super::{ConfigAccess, ConfigApplyTxn, DesiredStateApplyDocument, DesiredStateApplyPlan};
use crate::{Collection, InferenceBackend};
use anyhow::{Context, Result};

pub async fn load_inference_backend_in_txn(
    txn: &ConfigApplyTxn<'_>,
    agent_did: &str,
    backend_id: &str,
) -> Result<Option<InferenceBackend>> {
    super::desired_state::read_record(txn, Collection::InferenceBackend, agent_did, backend_id)
        .await?
        .map(|(_, value)| serde_json::from_value(value).context("decoding scoped InferenceBackend"))
        .transpose()
}

/// Full canonical replacement through the shared retained-candidate owner.
/// Runtime catalogs and health never enter the authored configuration.
pub async fn write_inference_backend_document(
    access: &ConfigAccess,
    backend: &InferenceBackend,
) -> Result<String> {
    backend.validate()?;
    access
        .transact("config.inference_backend.upsert", |txn| {
            Box::pin(async move {
                let value = serde_json::to_value(backend)?;
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection: Collection::InferenceBackend,
                    add: value.clone(),
                    update: value,
                }])?;
                super::apply_desired_state_plan(txn, &plan).await?;
                super::desired_state::read_record(
                    txn,
                    Collection::InferenceBackend,
                    &backend.agent_did,
                    &backend.backend_id,
                )
                .await?
                .map(|(id, _)| id)
                .context("replaced InferenceBackend missing")
            })
        })
        .await
}
