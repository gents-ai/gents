use anyhow::Result;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::BackendProviderKind;

use crate::config_writes::ConfigAccess;
use crate::{graphql_bool_literal, nullable_string_field, string_list_field};

#[derive(Debug, Clone)]
pub(crate) struct InferenceBackendUpsertDocument {
    pub(crate) backend_id: String,
    pub(crate) name: String,
    pub(crate) provider_kind: BackendProviderKind,
    pub(crate) endpoint: String,
    pub(crate) api_key: Option<String>,
    pub(crate) api_key_env_var: Option<String>,
    pub(crate) max_concurrent: i64,
    pub(crate) max_queue_depth: i64,
    pub(crate) enabled: bool,
    pub(crate) models_on_add: Vec<String>,
    pub(crate) models_on_update: Option<Vec<String>>,
    pub(crate) probe_status: String,
}

pub(crate) async fn write_inference_backend_document(
    access: &ConfigAccess,
    backend: &InferenceBackendUpsertDocument,
) -> Result<String> {
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
                    endpoint: "{endpoint}",
                    {api_key},
                    {api_key_env_var},
                    max_concurrent: {max_concurrent},
                    max_queue_depth: {max_queue_depth},
                    enabled: {enabled},
                    {models_add},
                    probe_status: "{probe_status}"
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#,
        backend_id = escape_graphql_string(&backend.backend_id),
        name = escape_graphql_string(&backend.name),
        provider_kind = escape_graphql_string(backend.provider_kind.as_str()),
        endpoint = escape_graphql_string(&backend.endpoint),
        api_key = nullable_string_field("api_key", backend.api_key.as_deref()),
        api_key_env_var =
            nullable_string_field("api_key_env_var", backend.api_key_env_var.as_deref()),
        max_concurrent = backend.max_concurrent,
        max_queue_depth = backend.max_queue_depth,
        enabled = graphql_bool_literal(backend.enabled),
        models_add = models_add,
        probe_status = escape_graphql_string(&backend.probe_status),
        update_fields = update_fields,
    );
    let response = access.execute(&mutation).await?;
    crate::extract_mutation_doc_id(&response, "InferenceBackend")
}
