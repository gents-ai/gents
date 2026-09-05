use anyhow::{Context, Result};
use gents_protocol::graphql::{
    extract_mutation_doc_id, graphql_rows_from_response, optional_bool_field, optional_f64_field,
    optional_i64_field, optional_i64_list_field, optional_string_field,
};

use crate::document_config::InferenceProfile;
use crate::graphql::escape_graphql_string;

use super::{mint_recreate_identity_timestamp, ConfigAccess, ConfigApplyTxn};

/// Upsert a possibly sparse profile after validating the effective stored
/// document in the same transaction as the mutation.
pub async fn write_inference_profile_document(
    access: &ConfigAccess,
    profile: &InferenceProfile,
) -> Result<String> {
    let txn = access.begin_apply_txn().await?;
    let result = write_inference_profile_in_txn(&txn, profile).await;
    match result {
        Ok(doc_id) => {
            txn.commit().await?;
            Ok(doc_id)
        }
        Err(error) => {
            let _ = txn.discard().await;
            Err(error)
        }
    }
}

async fn write_inference_profile_in_txn(
    txn: &ConfigApplyTxn<'_>,
    patch: &InferenceProfile,
) -> Result<String> {
    effective_inference_profile(txn, patch).await?.validate()?;
    let mutation = sparse_inference_profile_mutation(patch);
    let response = txn.execute(&mutation).await?;
    extract_mutation_doc_id(&response, "InferenceProfile")
}

pub(crate) async fn effective_inference_profile(
    txn: &ConfigApplyTxn<'_>,
    patch: &InferenceProfile,
) -> Result<InferenceProfile> {
    let profile_id = escape_graphql_string(&patch.profile_id);
    let query = format!(
        r#"{{
            InferenceProfile(
                filter: {{ profile_id: {{ _eq: "{profile_id}" }} }},
                limit: 1
            ) {{
                profile_id display_name context_window max_output_tokens max_turns temperature
                top_p top_k seed min_p frequency_penalty presence_penalty repetition_penalty
                reasoning_effort stream_batch_ms stream_liveness_timeout_secs
                deadline_duration_secs retry_max_transport retry_backoff_ms retry_max_resample
                retry_allow_repair retry_interactive_max
            }}
        }}"#
    );
    let response = txn.execute(&query).await?;
    let existing = graphql_rows_from_response(&response, "InferenceProfile")
        .into_iter()
        .next()
        .map(serde_json::from_value)
        .transpose()
        .context("decoding existing InferenceProfile")?;
    super::common::merge_sparse_document(existing, patch, &[], &[])
}

fn sparse_inference_profile_mutation(profile: &InferenceProfile) -> String {
    let profile_id = escape_graphql_string(&profile.profile_id);
    let fields = vec![
        optional_string_field("display_name", profile.display_name.as_deref()),
        optional_i64_field("context_window", profile.context_window),
        optional_i64_field("max_output_tokens", profile.max_output_tokens),
        optional_i64_field("max_turns", profile.max_turns),
        optional_f64_field("temperature", profile.temperature),
        optional_f64_field("top_p", profile.top_p),
        optional_i64_field("top_k", profile.top_k),
        optional_i64_field("seed", profile.seed),
        optional_f64_field("min_p", profile.min_p),
        optional_f64_field("frequency_penalty", profile.frequency_penalty),
        optional_f64_field("presence_penalty", profile.presence_penalty),
        optional_f64_field("repetition_penalty", profile.repetition_penalty),
        optional_string_field("reasoning_effort", profile.reasoning_effort.as_deref()),
        optional_i64_field("stream_batch_ms", profile.stream_batch_ms),
        optional_i64_field(
            "stream_liveness_timeout_secs",
            profile.stream_liveness_timeout_secs,
        ),
        optional_i64_field("deadline_duration_secs", profile.deadline_duration_secs),
        optional_i64_field("retry_max_transport", profile.retry_max_transport),
        optional_i64_list_field("retry_backoff_ms", profile.retry_backoff_ms.as_deref()),
        optional_i64_field("retry_max_resample", profile.retry_max_resample),
        optional_bool_field("retry_allow_repair", profile.retry_allow_repair),
        optional_i64_field("retry_interactive_max", profile.retry_interactive_max),
    ];
    let update_fields = fields
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join(",\n                    ");
    let mut add_fields = vec![format!(r#"profile_id: "{profile_id}""#)];
    add_fields.extend(fields.into_iter().flatten());
    add_fields.push(format!(
        r#"updated_at: "{}""#,
        escape_graphql_string(&mint_recreate_identity_timestamp())
    ));
    let add_fields = add_fields.join(",\n                    ");
    format!(
        r#"mutation {{
            upsert_InferenceProfile(
                filter: {{ profile_id: {{ _eq: "{profile_id}" }} }},
                add: {{
                    {add_fields}
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ensure_runtime_schemas, load_inference_profile};
    use defra_node::{EmbeddedNode, StorageBackend};
    use std::sync::Arc;

    #[tokio::test]
    async fn sparse_deadline_change_validates_preserved_liveness_timeout() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(dir.path().join("data"))
                .with_storage_backend(StorageBackend::Regolith)
                .build()
                .await?,
        );
        ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::Local(node.clone());

        let initial = InferenceProfile {
            profile_id: "profile".to_string(),
            stream_liveness_timeout_secs: Some(4_000),
            deadline_duration_secs: Some(5_000),
            ..Default::default()
        };
        write_inference_profile_document(&access, &initial).await?;

        let patch = InferenceProfile {
            profile_id: initial.profile_id.clone(),
            deadline_duration_secs: Some(3_600),
            ..Default::default()
        };
        let error = write_inference_profile_document(&access, &patch)
            .await
            .expect_err("preserved liveness timeout must be checked against the new deadline")
            .to_string();
        assert!(
            error.contains("must be less than deadline_duration_secs"),
            "{error}"
        );

        let stored = load_inference_profile(&node, &initial.profile_id)
            .await?
            .expect("stored profile");
        assert_eq!(stored.deadline_duration_secs, Some(5_000));
        Ok(())
    }
}
