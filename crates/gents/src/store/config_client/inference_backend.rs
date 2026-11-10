use super::ConfigApplyTxn;
use crate::{Collection, InferenceBackend};
use anyhow::{Context, Result};

pub async fn load_inference_backend_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    backend_id: &str,
) -> Result<Option<InferenceBackend>> {
    super::desired_state::read::read_record(txn, Collection::InferenceBackend, node_did, backend_id)
        .await?
        .map(|(_, value)| serde_json::from_value(value).context("decoding scoped InferenceBackend"))
        .transpose()
}

pub async fn list_inference_backends_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
) -> Result<Vec<InferenceBackend>> {
    let fields = super::desired_state::projection::canonical_struct_fields::<
        crate::documents::InferenceBackendMetadata,
    >()?
    .join(" ");
    let response = txn
        .execute(&format!(
            r#"{{ InferenceBackendMetadata(filter: {{ node_did: {{ _eq: "{}" }} }}) {{ {fields} }} }}"#,
            crate::store::graphql::escape_graphql_string(node_did)
        ))
        .await?;
    gents_protocol::graphql::graphql_rows_from_response(&response, "InferenceBackendMetadata")
        .into_iter()
        .map(|row| {
            let metadata: crate::documents::InferenceBackendMetadata = serde_json::from_value(row)?;
            Ok(metadata.into_backend()?)
        })
        .collect()
}
