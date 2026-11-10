use super::{InferenceBackend, InferenceBackendMetadata};
use crate::documents::desired_state::projection::canonical_struct_fields;
use crate::documents::desired_state::read::read_record;
use crate::store::config_client::ConfigApplyTxn;
use crate::Collection;
use anyhow::{Context, Result};

pub async fn load_inference_backend_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    backend_id: &str,
) -> Result<Option<InferenceBackend>> {
    read_record(txn, Collection::InferenceBackend, node_did, backend_id)
        .await?
        .map(|(_, value)| serde_json::from_value(value).context("decoding scoped InferenceBackend"))
        .transpose()
}

pub async fn list_inference_backends_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
) -> Result<Vec<InferenceBackend>> {
    let fields = canonical_struct_fields::<InferenceBackendMetadata>()?.join(" ");
    let response = txn
        .execute(&format!(
            r#"{{ InferenceBackendMetadata(filter: {{ node_did: {{ _eq: "{}" }} }}) {{ {fields} }} }}"#,
            crate::store::graphql::escape_graphql_string(node_did)
        ))
        .await?;
    gents_protocol::graphql::graphql_rows_from_response(&response, "InferenceBackendMetadata")
        .into_iter()
        .map(|row| {
            let metadata: InferenceBackendMetadata = serde_json::from_value(row)?;
            Ok(metadata.into_backend()?)
        })
        .collect()
}
