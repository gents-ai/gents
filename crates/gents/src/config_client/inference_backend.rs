use crate::graphql::escape_graphql_string;
use crate::{BackendProviderKind, OpenAiWireApi};
use anyhow::{Context, Result};

use super::{mint_recreate_identity_timestamp, ConfigAccess};
use gents_protocol::graphql::{graphql_bool_literal, nullable_string_field, string_list_field};

pub async fn load_inference_backend_in_txn(
    txn: &super::ConfigApplyTxn<'_>,
    backend_id: &str,
) -> Result<Option<crate::InferenceBackend>> {
    let backend_id = escape_graphql_string(backend_id);
    let response = txn
        .execute(&format!(
            r#"{{ InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{backend_id}" }} }}, limit: 1
            ) {{ backend_id name provider_kind openai_wire_api endpoint api_key api_key_env_var
                 max_concurrent max_queue_depth enabled models probe_status }} }}"#
        ))
        .await?;
    gents_protocol::graphql::graphql_rows_from_response(&response, "InferenceBackend")
        .into_iter()
        .next()
        .map(|row| crate::InferenceBackend::from_value(&row))
        .transpose()
        .context("decoding existing InferenceBackend")
}

#[derive(Debug, Clone)]
pub struct InferenceBackendUpsertDocument {
    pub backend_id: String,
    pub name: String,
    pub provider_kind: BackendProviderKind,
    pub openai_wire_api: Option<OpenAiWireApi>,
    pub endpoint: String,
    pub api_key: Option<String>,
    pub api_key_env_var: Option<String>,
    pub max_concurrent: i64,
    pub max_queue_depth: i64,
    pub enabled: bool,
    pub models_on_add: Vec<String>,
    pub models_on_update: Option<Vec<String>>,
    pub probe_status: String,
}

pub async fn write_inference_backend_document(
    access: &ConfigAccess,
    backend: &InferenceBackendUpsertDocument,
) -> Result<String> {
    crate::InferenceBackend {
        backend_id: backend.backend_id.clone(),
        name: backend.name.clone(),
        provider_kind: backend.provider_kind,
        openai_wire_api: backend.openai_wire_api,
        endpoint: backend.endpoint.clone(),
        api_key: backend.api_key.clone(),
        api_key_env_var: backend.api_key_env_var.clone(),
        max_concurrent: backend.max_concurrent,
        max_queue_depth: backend.max_queue_depth,
        enabled: backend.enabled,
        models: backend
            .models_on_update
            .clone()
            .unwrap_or_else(|| backend.models_on_add.clone()),
        probe_status: backend.probe_status.clone(),
    }
    .validate(None)?;
    let recreate_identity = escape_graphql_string(&mint_recreate_identity_timestamp());
    let models_add = string_list_field("models", &backend.models_on_add)
        .ok_or_else(|| anyhow::anyhow!("backend models field could not be rendered"))?;
    let models_update = backend
        .models_on_update
        .as_ref()
        .and_then(|models| string_list_field("models", models));
    let update_fields = vec![
        Some(format!(
            r#"name: "{}""#,
            escape_graphql_string(&backend.name)
        )),
        Some(format!(
            r#"provider_kind: "{}""#,
            escape_graphql_string(backend.provider_kind.as_str())
        )),
        Some(nullable_string_field(
            "openai_wire_api",
            backend.openai_wire_api.map(OpenAiWireApi::as_str),
        )),
        Some(format!(
            r#"endpoint: "{}""#,
            escape_graphql_string(&backend.endpoint)
        )),
        Some(nullable_string_field("api_key", backend.api_key.as_deref())),
        Some(nullable_string_field(
            "api_key_env_var",
            backend.api_key_env_var.as_deref(),
        )),
        Some(format!("max_concurrent: {}", backend.max_concurrent)),
        Some(format!("max_queue_depth: {}", backend.max_queue_depth)),
        Some(format!(
            "enabled: {}",
            graphql_bool_literal(backend.enabled)
        )),
        models_update,
        Some(format!(
            r#"probe_status: "{}""#,
            escape_graphql_string(&backend.probe_status)
        )),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");
    let mutation = format!(
        r#"mutation {{
            upsert_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{backend_id}" }} }},
                add: {{
                    backend_id: "{backend_id}",
                    name: "{name}",
                    provider_kind: "{provider_kind}",
                    {openai_wire_api},
                    endpoint: "{endpoint}",
                    {api_key},
                    {api_key_env_var},
                    max_concurrent: {max_concurrent},
                    max_queue_depth: {max_queue_depth},
                    enabled: {enabled},
                    {models_add},
                    probe_status: "{probe_status}",
                    updated_at: "{recreate_identity}"
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#,
        backend_id = escape_graphql_string(&backend.backend_id),
        name = escape_graphql_string(&backend.name),
        provider_kind = escape_graphql_string(backend.provider_kind.as_str()),
        openai_wire_api = nullable_string_field(
            "openai_wire_api",
            backend.openai_wire_api.map(OpenAiWireApi::as_str)
        ),
        endpoint = escape_graphql_string(&backend.endpoint),
        api_key = nullable_string_field("api_key", backend.api_key.as_deref()),
        api_key_env_var =
            nullable_string_field("api_key_env_var", backend.api_key_env_var.as_deref()),
        max_concurrent = backend.max_concurrent,
        max_queue_depth = backend.max_queue_depth,
        enabled = graphql_bool_literal(backend.enabled),
        models_add = models_add,
        probe_status = escape_graphql_string(&backend.probe_status),
        recreate_identity = recreate_identity,
        update_fields = update_fields,
    );
    let response = access
        .execute_mutation(&mutation, "upsert InferenceBackend")
        .await?;
    gents_protocol::graphql::extract_mutation_doc_id(&response, "InferenceBackend")
}
