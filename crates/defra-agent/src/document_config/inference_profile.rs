use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use super::graphql_fields;
use super::serde_helpers;
use crate::config::{
    DEFAULT_CONTEXT_WINDOW, DEFAULT_DEADLINE_DURATION_SECS, DEFAULT_MAX_OUTPUT_TOKENS,
    DEFAULT_MAX_TURNS, DEFAULT_STREAM_BATCH_MS,
};
use crate::graphql::escape_graphql_string;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InferenceProfile {
    pub profile_id: String,
    pub display_name: Option<String>,
    pub context_window: Option<i64>,
    pub max_output_tokens: Option<i64>,
    pub max_turns: Option<i64>,
    pub temperature: Option<f64>,
    pub stream_batch_ms: Option<i64>,
    pub deadline_duration_secs: Option<i64>,
}

const DEFAULT_INFERENCE_PROFILE_LABEL: &str = "Default";

pub fn default_inference_profile_id_for_behavior(behavior_id: &str) -> String {
    format!("{behavior_id}-profile")
}

pub(super) fn default_inference_profile_for_behavior(behavior_id: &str) -> InferenceProfile {
    InferenceProfile {
        profile_id: default_inference_profile_id_for_behavior(behavior_id),
        display_name: Some(DEFAULT_INFERENCE_PROFILE_LABEL.to_string()),
        context_window: Some(DEFAULT_CONTEXT_WINDOW as i64),
        max_output_tokens: Some(DEFAULT_MAX_OUTPUT_TOKENS as i64),
        max_turns: Some(DEFAULT_MAX_TURNS as i64),
        temperature: Some(0.0),
        stream_batch_ms: Some(DEFAULT_STREAM_BATCH_MS as i64),
        deadline_duration_secs: Some(DEFAULT_DEADLINE_DURATION_SECS as i64),
    }
}

pub(super) async fn create_default_inference_profile(
    node: &EmbeddedNode,
    behavior_id: &str,
) -> Result<InferenceProfile> {
    let profile = default_inference_profile_for_behavior(behavior_id);
    upsert_inference_profile(node, &profile).await?;
    Ok(profile)
}

pub async fn load_inference_profile(
    node: &EmbeddedNode,
    profile_id: &str,
) -> Result<Option<InferenceProfile>> {
    Ok(load_inference_profile_record(node, profile_id)
        .await?
        .map(|(_, profile)| profile))
}

pub(crate) async fn load_inference_profile_record(
    node: &EmbeddedNode,
    profile_id: &str,
) -> Result<Option<(String, InferenceProfile)>> {
    let escaped_profile_id = escape_graphql_string(profile_id);
    let query = format!(
        r#"{{
            InferenceProfile(
                filter: {{ profile_id: {{ _eq: "{escaped_profile_id}" }} }},
                limit: 1
            ) {{
                _docID
                profile_id
                display_name
                context_window
                max_output_tokens
                max_turns
                temperature
                stream_batch_ms
                deadline_duration_secs
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query InferenceProfile failed: {:?}", resp.errors);
    }

    Ok(serde_helpers::first_row_with_doc_id(
        resp.data.as_ref(),
        "InferenceProfile",
    ))
}

pub(crate) async fn load_inference_profile_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<(String, InferenceProfile)>> {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            InferenceProfile(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                limit: 1
            ) {{
                _docID
                profile_id
                display_name
                context_window
                max_output_tokens
                max_turns
                temperature
                stream_batch_ms
                deadline_duration_secs
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query InferenceProfile by _docID failed: {:?}", resp.errors);
    }

    Ok(serde_helpers::first_row_with_doc_id(
        resp.data.as_ref(),
        "InferenceProfile",
    ))
}

pub async fn list_inference_profile_records(
    node: &EmbeddedNode,
) -> Result<Vec<(String, InferenceProfile)>> {
    let query = r#"{
            InferenceProfile(order: { profile_id: ASC }) {
                _docID
                profile_id
                display_name
                context_window
                max_output_tokens
                max_turns
                temperature
                stream_batch_ms
                deadline_duration_secs
            }
        }"#;

    let resp = node.execute(query).await;
    if resp.has_errors() {
        anyhow::bail!("list InferenceProfile failed: {:?}", resp.errors);
    }

    Ok(serde_helpers::rows_with_doc_id(
        resp.data.as_ref(),
        "InferenceProfile",
    ))
}

pub async fn upsert_inference_profile(
    node: &EmbeddedNode,
    profile: &InferenceProfile,
) -> Result<()> {
    let escaped_profile_id = escape_graphql_string(&profile.profile_id);

    let add_fields = vec![
        Some(format!(r#"profile_id: "{escaped_profile_id}""#)),
        graphql_fields::graphql_string_field("display_name", profile.display_name.as_deref()),
        graphql_fields::graphql_optional_int_field("context_window", profile.context_window),
        graphql_fields::graphql_optional_int_field("max_output_tokens", profile.max_output_tokens),
        graphql_fields::graphql_optional_int_field("max_turns", profile.max_turns),
        graphql_fields::graphql_optional_float_field("temperature", profile.temperature),
        graphql_fields::graphql_optional_int_field("stream_batch_ms", profile.stream_batch_ms),
        graphql_fields::graphql_optional_int_field(
            "deadline_duration_secs",
            profile.deadline_duration_secs,
        ),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");

    let update_fields = vec![
        graphql_fields::graphql_string_field("display_name", profile.display_name.as_deref()),
        graphql_fields::graphql_optional_int_field("context_window", profile.context_window),
        graphql_fields::graphql_optional_int_field("max_output_tokens", profile.max_output_tokens),
        graphql_fields::graphql_optional_int_field("max_turns", profile.max_turns),
        graphql_fields::graphql_optional_float_field("temperature", profile.temperature),
        graphql_fields::graphql_optional_int_field("stream_batch_ms", profile.stream_batch_ms),
        graphql_fields::graphql_optional_int_field(
            "deadline_duration_secs",
            profile.deadline_duration_secs,
        ),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");

    let mutation = format!(
        r#"mutation {{
            upsert_InferenceProfile(
                filter: {{ profile_id: {{ _eq: "{escaped_profile_id}" }} }},
                add: {{
                    {add_fields}
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#
    );

    let resp = node.execute(&mutation).await;
    if resp.has_errors() {
        anyhow::bail!("upsert InferenceProfile failed: {:?}", resp.errors);
    }
    Ok(())
}
