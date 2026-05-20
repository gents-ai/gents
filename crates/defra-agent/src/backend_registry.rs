//! Backend registry — DefraDB lookups for inference backend documents.
//!
//! The runtime uses this to resolve a behavior's backend and check health.

use anyhow::Result;
use defra_node::EmbeddedNode;

use crate::backend_provider::BackendProviderKind;
use crate::graphql::escape_graphql_string;

pub const DEFAULT_MAX_QUEUE_DEPTH: i64 = 100;
pub const HEALTHY_PROBE_STATUS: &str = "healthy";
pub const UNKNOWN_PROBE_STATUS: &str = "unknown";

#[derive(Debug, Clone)]
pub struct InferenceBackend {
    pub backend_id: String,
    pub name: String,
    pub provider_kind: BackendProviderKind,
    /// OpenAI-compatible API base URL, including the `/v1` path segment.
    pub endpoint: String,
    pub api_key: Option<String>,
    pub api_key_env_var: Option<String>,
    pub max_concurrent: i64,
    pub max_queue_depth: i64,
    pub enabled: bool,
    pub models: Vec<String>,
    pub probe_status: String,
}

impl InferenceBackend {
    pub fn from_value(v: &serde_json::Value) -> Result<Self> {
        Ok(Self {
            backend_id: v
                .get("backend_id")
                .and_then(|value| value.as_str())
                .ok_or_else(|| anyhow::anyhow!("backend_id is required"))?
                .to_string(),
            name: v
                .get("name")
                .and_then(|value| value.as_str())
                .ok_or_else(|| anyhow::anyhow!("backend name is required"))?
                .to_string(),
            provider_kind: BackendProviderKind::parse_optional(
                v.get("provider_kind").and_then(|value| value.as_str()),
            )?,
            endpoint: v
                .get("endpoint")
                .and_then(|value| value.as_str())
                .ok_or_else(|| anyhow::anyhow!("backend endpoint is required"))?
                .to_string(),
            api_key: v
                .get("api_key")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(ToOwned::to_owned),
            api_key_env_var: v
                .get("api_key_env_var")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(ToOwned::to_owned),
            max_concurrent: v
                .get("max_concurrent")
                .and_then(|value| value.as_i64())
                .ok_or_else(|| anyhow::anyhow!("max_concurrent is required"))?,
            max_queue_depth: v
                .get("max_queue_depth")
                .and_then(|value| value.as_i64())
                .unwrap_or(DEFAULT_MAX_QUEUE_DEPTH),
            enabled: v
                .get("enabled")
                .and_then(|value| value.as_bool())
                .ok_or_else(|| anyhow::anyhow!("enabled is required"))?,
            models: v
                .get("models")
                .and_then(|value| value.as_array())
                .map(|rows| {
                    rows.iter()
                        .filter_map(|row| row.as_str())
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            probe_status: v
                .get("probe_status")
                .and_then(|v| v.as_str())
                .unwrap_or(UNKNOWN_PROBE_STATUS)
                .to_string(),
        })
    }

    /// Whether this backend is available for scheduling.
    pub fn is_available(&self) -> bool {
        self.enabled && self.probe_status == HEALTHY_PROBE_STATUS
    }
}

pub async fn lookup_backend(
    node: &EmbeddedNode,
    backend_id: &str,
) -> Result<Option<InferenceBackend>> {
    Ok(lookup_backend_record(node, backend_id)
        .await?
        .map(|(_, backend)| backend))
}

pub(crate) async fn lookup_backend_record(
    node: &EmbeddedNode,
    backend_id: &str,
) -> Result<Option<(String, InferenceBackend)>> {
    let escaped_id = escape_graphql_string(backend_id);
    let query = format!(
        r#"query {{ InferenceBackend(filter: {{backend_id: {{_eq: "{}"}}}}) {{ _docID backend_id name provider_kind endpoint api_key api_key_env_var max_concurrent max_queue_depth enabled models probe_status }} }}"#,
        escaped_id
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query InferenceBackend failed: {:?}", resp.errors);
    }

    let backend = resp
        .data
        .as_ref()
        .and_then(|d| d.get("InferenceBackend"))
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .map(|row| {
            Ok::<_, anyhow::Error>((
                row.get("_docID")
                    .and_then(|value| value.as_str())
                    .ok_or_else(|| anyhow::anyhow!("InferenceBackend row is missing _docID"))?
                    .to_string(),
                InferenceBackend::from_value(row)?,
            ))
        })
        .transpose()?;

    Ok(backend)
}

pub(crate) async fn lookup_backend_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<(String, InferenceBackend)>> {
    let escaped_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"query {{ InferenceBackend(filter: {{_docID: {{_eq: "{}"}}}}, limit: 1) {{ _docID backend_id name provider_kind endpoint api_key api_key_env_var max_concurrent max_queue_depth enabled models probe_status }} }}"#,
        escaped_id
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query InferenceBackend by _docID failed: {:?}", resp.errors);
    }

    let backend = resp
        .data
        .as_ref()
        .and_then(|d| d.get("InferenceBackend"))
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .map(|row| {
            Ok::<_, anyhow::Error>((
                row.get("_docID")
                    .and_then(|value| value.as_str())
                    .ok_or_else(|| anyhow::anyhow!("InferenceBackend row is missing _docID"))?
                    .to_string(),
                InferenceBackend::from_value(row)?,
            ))
        })
        .transpose()?;

    Ok(backend)
}

pub(crate) async fn list_backend_records(
    node: &EmbeddedNode,
) -> Result<Vec<(String, InferenceBackend)>> {
    let query = r#"query {
        InferenceBackend(order: { backend_id: ASC }) {
            _docID
            backend_id
            name
            provider_kind
            endpoint
            api_key
            api_key_env_var
            max_concurrent
            max_queue_depth
            enabled
            models
            probe_status
        }
    }"#;

    let resp = node.execute(query).await;
    if resp.has_errors() {
        anyhow::bail!("list InferenceBackend failed: {:?}", resp.errors);
    }

    let backends = resp
        .data
        .as_ref()
        .and_then(|d| d.get("InferenceBackend"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|row| {
                    Ok::<_, anyhow::Error>((
                        row.get("_docID")
                            .and_then(|value| value.as_str())
                            .ok_or_else(|| {
                                anyhow::anyhow!("InferenceBackend row is missing _docID")
                            })?
                            .to_string(),
                        InferenceBackend::from_value(row)?,
                    ))
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();

    Ok(backends)
}

pub async fn list_enabled_backends(node: &EmbeddedNode) -> Result<Vec<InferenceBackend>> {
    let query = r#"query { InferenceBackend(filter: {enabled: {_eq: true}}) { backend_id name provider_kind endpoint api_key api_key_env_var max_concurrent max_queue_depth enabled probe_status models last_probe } }"#;

    let resp = node.execute(query).await;
    if resp.has_errors() {
        anyhow::bail!("query InferenceBackend failed: {:?}", resp.errors);
    }

    let backends = resp
        .data
        .as_ref()
        .and_then(|d| d.get("InferenceBackend"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(InferenceBackend::from_value)
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();

    Ok(backends)
}

#[cfg(test)]
mod tests;
