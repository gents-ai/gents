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

/// Every backend of `agent_did` in this transaction's snapshot.
pub(crate) async fn list_inference_backends_in_txn(
    txn: &ConfigApplyTxn<'_>,
    agent_did: &str,
) -> Result<Vec<InferenceBackend>> {
    let fields = super::config_projection(Collection::InferenceBackend, None)?
        .0
        .join(" ");
    let response = txn
        .execute(&format!(
            r#"{{ InferenceBackend(filter: {{ agent_did: {{ _eq: "{}" }} }}) {{ {fields} }} }}"#,
            crate::graphql::escape_graphql_string(agent_did)
        ))
        .await?;
    gents_protocol::graphql::graphql_rows_from_response(&response, "InferenceBackend")
        .into_iter()
        .map(|row| serde_json::from_value(row).context("decoding scoped InferenceBackend"))
        .collect()
}

/// Replace one backend inside a caller's transaction; the whole principal's
/// configuration is validated before the caller commits.
pub(crate) async fn write_inference_backend_in_txn(
    txn: &ConfigApplyTxn<'_>,
    backend: &InferenceBackend,
) -> Result<()> {
    backend.validate()?;
    let value = serde_json::to_value(backend)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::InferenceBackend,
        add: value.clone(),
        update: value,
    }])?;
    super::apply_desired_state_plan(txn, &plan).await?;
    Ok(())
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
                write_inference_backend_in_txn(txn, backend).await?;
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
