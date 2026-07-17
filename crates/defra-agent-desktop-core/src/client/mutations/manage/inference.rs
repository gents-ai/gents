use anyhow::{bail, Result};
use defra_agent_protocol::row::InferenceBackendRow;
use defra_node::EmbeddedNode;
use serde_json::Value;

use super::super::graphql::{
    escape_graphql_string, execute_mutation, execute_remote_delete_mutation,
    graphql_optional_bool_field, graphql_optional_int_field, graphql_string_field,
    graphql_string_list_field, join_fields, normalize_required,
};

pub async fn upsert_inference_backend(
    node: &EmbeddedNode,
    row: &InferenceBackendRow,
) -> Result<()> {
    let backend_id = normalize_required("backend_id", &row.backend_id)?;

    let add_fields = [
        Some(format!(
            r#"backend_id: "{}""#,
            escape_graphql_string(backend_id)
        )),
        Some(graphql_string_field("name", row.name.as_deref())),
        Some(graphql_string_field(
            "provider_kind",
            row.provider_kind.as_deref(),
        )),
        Some(graphql_string_field(
            "openai_wire_api",
            row.openai_wire_api.as_deref(),
        )),
        Some(graphql_string_field("endpoint", row.endpoint.as_deref())),
        Some(graphql_string_field("api_key", row.api_key.as_deref())),
        Some(graphql_string_field(
            "api_key_env_var",
            row.api_key_env_var.as_deref(),
        )),
        Some(graphql_optional_int_field(
            "max_concurrent",
            row.max_concurrent,
        )),
        Some(graphql_optional_int_field(
            "max_queue_depth",
            row.max_queue_depth,
        )),
        Some(graphql_optional_bool_field("enabled", row.enabled)),
        Some(graphql_string_list_field("models", &row.models)),
        Some(graphql_string_field(
            "last_probe",
            row.last_probe.as_deref(),
        )),
        Some(graphql_string_field(
            "probe_status",
            row.probe_status.as_deref(),
        )),
    ];
    let update_fields = [
        Some(graphql_string_field("name", row.name.as_deref())),
        Some(graphql_string_field(
            "provider_kind",
            row.provider_kind.as_deref(),
        )),
        Some(graphql_string_field(
            "openai_wire_api",
            row.openai_wire_api.as_deref(),
        )),
        Some(graphql_string_field("endpoint", row.endpoint.as_deref())),
        Some(graphql_string_field("api_key", row.api_key.as_deref())),
        Some(graphql_string_field(
            "api_key_env_var",
            row.api_key_env_var.as_deref(),
        )),
        Some(graphql_optional_int_field(
            "max_concurrent",
            row.max_concurrent,
        )),
        Some(graphql_optional_int_field(
            "max_queue_depth",
            row.max_queue_depth,
        )),
        Some(graphql_optional_bool_field("enabled", row.enabled)),
        Some(graphql_string_list_field("models", &row.models)),
        Some(graphql_string_field(
            "last_probe",
            row.last_probe.as_deref(),
        )),
        Some(graphql_string_field(
            "probe_status",
            row.probe_status.as_deref(),
        )),
    ];

    let mutation = format!(
        r#"mutation {{
            upsert_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{backend_id}" }} }},
                add: {{
                    {add_fields}
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#,
        backend_id = escape_graphql_string(backend_id),
        add_fields = join_fields(&add_fields),
        update_fields = join_fields(&update_fields),
    );
    execute_mutation(node, &mutation, "upsert_inference_backend").await
}

pub async fn delete_inference_backend(node: &EmbeddedNode, backend_id: &str) -> Result<usize> {
    let mutation = build_delete_inference_backend_mutation(backend_id)?;
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        bail!(
            "delete_inference_backend failed: {}",
            response
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("delete_InferenceBackend"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0))
}

pub async fn delete_inference_backend_from_graphql(
    graphql: &str,
    backend_id: &str,
) -> Result<usize> {
    let graphql = normalize_required("graphql", graphql)?;
    let mutation = build_delete_inference_backend_mutation(backend_id)?;
    execute_remote_delete_mutation(
        graphql,
        &mutation,
        "delete_inference_backend",
        "delete_InferenceBackend",
    )
    .await
}

fn build_delete_inference_backend_mutation(backend_id: &str) -> Result<String> {
    let backend_id = normalize_required("backend_id", backend_id)?;
    let backend_id = escape_graphql_string(backend_id);
    Ok(format!(
        r#"mutation {{
            delete_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{backend_id}" }} }}
            ) {{ _docID }}
        }}"#
    ))
}
