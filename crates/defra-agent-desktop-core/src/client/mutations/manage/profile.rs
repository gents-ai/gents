use anyhow::Result;
use defra_agent_protocol::row::InferenceProfileRow;
use defra_node::EmbeddedNode;

use super::super::graphql::{
    escape_graphql_string, execute_mutation, graphql_optional_bool_field,
    graphql_optional_float_field, graphql_optional_int_field, graphql_optional_int_list_field,
    graphql_string_field, join_fields, normalize_required,
};

pub async fn upsert_inference_profile(
    node: &EmbeddedNode,
    row: &InferenceProfileRow,
) -> Result<()> {
    let profile_id = normalize_required("profile_id", &row.profile_id)?;
    if row
        .stream_liveness_timeout_secs
        .is_some_and(|value| value <= 0)
    {
        anyhow::bail!("stream_liveness_timeout_secs must be positive");
    }

    let add_fields = [
        Some(format!(
            r#"profile_id: "{}""#,
            escape_graphql_string(profile_id)
        )),
        Some(graphql_string_field(
            "display_name",
            row.display_name.as_deref(),
        )),
        Some(graphql_optional_int_field(
            "context_window",
            row.context_window,
        )),
        Some(graphql_optional_int_field(
            "max_output_tokens",
            row.max_output_tokens,
        )),
        Some(graphql_optional_int_field("max_turns", row.max_turns)),
        Some(graphql_optional_float_field("temperature", row.temperature)),
        Some(graphql_optional_int_field(
            "stream_batch_ms",
            row.stream_batch_ms,
        )),
        Some(graphql_optional_int_field(
            "stream_liveness_timeout_secs",
            row.stream_liveness_timeout_secs,
        )),
        Some(graphql_optional_int_field(
            "deadline_duration_secs",
            row.deadline_duration_secs,
        )),
        Some(graphql_optional_int_field(
            "retry_max_transport",
            row.retry_max_transport,
        )),
        Some(graphql_optional_int_list_field(
            "retry_backoff_ms",
            row.retry_backoff_ms.as_deref(),
        )),
        Some(graphql_optional_int_field(
            "retry_max_resample",
            row.retry_max_resample,
        )),
        Some(graphql_optional_bool_field(
            "retry_allow_repair",
            row.retry_allow_repair,
        )),
        Some(graphql_optional_int_field(
            "retry_interactive_max",
            row.retry_interactive_max,
        )),
    ];
    let update_fields = [
        Some(graphql_string_field(
            "display_name",
            row.display_name.as_deref(),
        )),
        Some(graphql_optional_int_field(
            "context_window",
            row.context_window,
        )),
        Some(graphql_optional_int_field(
            "max_output_tokens",
            row.max_output_tokens,
        )),
        Some(graphql_optional_int_field("max_turns", row.max_turns)),
        Some(graphql_optional_float_field("temperature", row.temperature)),
        Some(graphql_optional_int_field(
            "stream_batch_ms",
            row.stream_batch_ms,
        )),
        Some(graphql_optional_int_field(
            "stream_liveness_timeout_secs",
            row.stream_liveness_timeout_secs,
        )),
        Some(graphql_optional_int_field(
            "deadline_duration_secs",
            row.deadline_duration_secs,
        )),
        Some(graphql_optional_int_field(
            "retry_max_transport",
            row.retry_max_transport,
        )),
        Some(graphql_optional_int_list_field(
            "retry_backoff_ms",
            row.retry_backoff_ms.as_deref(),
        )),
        Some(graphql_optional_int_field(
            "retry_max_resample",
            row.retry_max_resample,
        )),
        Some(graphql_optional_bool_field(
            "retry_allow_repair",
            row.retry_allow_repair,
        )),
        Some(graphql_optional_int_field(
            "retry_interactive_max",
            row.retry_interactive_max,
        )),
    ];

    let mutation = format!(
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
        }}"#,
        profile_id = escape_graphql_string(profile_id),
        add_fields = join_fields(&add_fields),
        update_fields = join_fields(&update_fields),
    );
    execute_mutation(node, &mutation, "upsert_inference_profile").await
}
