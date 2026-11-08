mod catalog_freshness;
pub mod observation_binding;
pub use catalog_freshness::decode_backend_observation;

pub use crate::documents::InferenceBackend;
use crate::documents::{
    AdvertisedModel, BackendAuth, BackendModelCatalog, ClaudePlatform, InferenceBackendObservation,
};
use crate::inference::backends::provider::BackendProviderKind;
use crate::openai_wire::OpenAiWireApi;
use crate::store::graphql::escape_graphql_string;
use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;

pub const DEFAULT_MAX_CONCURRENT: i64 = 1;
pub const DEFAULT_MAX_QUEUE_DEPTH: i64 = 100;
pub const HEALTHY_PROBE_STATUS: &str = "healthy";
pub const UNKNOWN_PROBE_STATUS: &str = "unknown";

#[derive(Debug, Clone)]
pub struct BackendFields {
    pub backend_id: Option<String>,
    pub backend_provider_kind: BackendProviderKind,
    pub openai_wire_api: OpenAiWireApi,
    pub backend_endpoint: String,
    pub backend_auth: BackendAuth,
    pub claude_platform: Option<ClaudePlatform>,
}

pub(crate) const BACKEND_CONFIG_FIELDS: &str = "node_did backend_id name provider_kind openai_wire_api endpoint auth_kind credential_id credential_version environment_variable oauth_account_ref oauth_allow_overage claude_platform connect_timeout_secs discovery_timeout_secs max_concurrent max_queue_depth enabled tags";

impl InferenceBackend {
    fn metadata_from_value(value: &serde_json::Value) -> Result<Self> {
        let mut config = value.clone();
        let object = config
            .as_object_mut()
            .context("backend metadata must be an object")?;
        for field in [
            "_docID",
            "_deleted",
            "updated_at",
            "catalogs",
            "catalog_freshness",
            "probe_status",
            "last_probe",
        ] {
            object.remove(field);
        }
        let metadata: crate::documents::InferenceBackendMetadata = serde_json::from_value(config)?;
        let backend = metadata.into_backend()?;
        backend.validate()?;
        Ok(backend)
    }

    pub fn from_value(value: &serde_json::Value) -> Result<Self> {
        let mut config = value.clone();
        let object = config
            .as_object_mut()
            .context("InferenceBackend must be an object")?;
        for field in [
            "_docID",
            "_deleted",
            "updated_at",
            "catalogs",
            "catalog_freshness",
            "probe_status",
            "last_probe",
        ] {
            object.remove(field);
        }
        let backend: Self =
            serde_json::from_value(config).context("decoding InferenceBackend configuration")?;
        backend.validate()?;
        Ok(backend)
    }

    pub fn backend_fields(&self) -> BackendFields {
        BackendFields {
            backend_id: Some(self.backend_id.clone()),
            backend_provider_kind: self.provider_kind,
            openai_wire_api: OpenAiWireApi::effective_for_provider(
                self.provider_kind,
                self.openai_wire_api,
            ),
            backend_endpoint: self.endpoint.clone(),
            backend_auth: self.auth.clone(),
            claude_platform: self.claude_platform.clone(),
        }
    }

    pub fn effective_max_concurrent(&self) -> i64 {
        self.max_concurrent.unwrap_or(DEFAULT_MAX_CONCURRENT)
    }

    pub fn effective_max_queue_depth(&self) -> i64 {
        self.max_queue_depth.unwrap_or(DEFAULT_MAX_QUEUE_DEPTH)
    }

    pub fn validation_violations(&self) -> Vec<String> {
        let mut violations = Vec::new();
        for (name, value) in [
            ("node_did", self.node_did.as_str()),
            ("backend_id", self.backend_id.as_str()),
            ("endpoint", self.endpoint.as_str()),
        ] {
            if value.trim().is_empty() {
                violations.push(format!("{name} must not be empty"));
            }
        }
        if self.effective_max_concurrent() <= 0 {
            violations.push("max_concurrent must be positive".into());
        }
        if self.effective_max_queue_depth() < 0 {
            violations.push("max_queue_depth must not be negative".into());
        }
        for (name, value) in [
            ("connect_timeout_secs", self.connect_timeout_secs),
            ("discovery_timeout_secs", self.discovery_timeout_secs),
        ] {
            if value.is_some_and(|value| value <= 0) {
                violations.push(format!("{name} must be positive"));
            }
        }
        match &self.auth {
            BackendAuth::ApiKey { key } if key.trim().is_empty() => {
                violations.push("API key must not be blank".into())
            }
            BackendAuth::Environment { variable } if variable.trim().is_empty() => {
                violations.push("API key environment variable must not be blank".into())
            }
            _ => {}
        }
        let node_oauth = self.auth.uses_node_oauth();
        if (node_oauth && !self.provider_kind.accepts_node_oauth())
            || (!node_oauth && self.provider_kind.is_node_scoped_oauth())
            || !self.auth.suits_provider(self.provider_kind)
        {
            violations
                .push("backend auth selection is incompatible with the provider adapter".into());
        }
        if let Err(error) =
            ClaudePlatform::validate(self.provider_kind, self.claude_platform.as_ref())
        {
            violations.push(error.to_string());
        }
        if self.provider_kind.is_claude_platform()
            && !(self.endpoint.starts_with("https://") || self.endpoint.starts_with("http://"))
        {
            violations.push("endpoint must be an http or https URL".into());
        }
        // The rotation policy reads Claude's unified rate-limit headers; no
        // other provider adapter reports the limit state it chooses by.
        if matches!(self.auth, BackendAuth::NodeOAuthPool { .. })
            && self.provider_kind != BackendProviderKind::ClaudeCliSubscription
        {
            violations.push(
                "account pools rotate Claude subscription accounts; select one account for this provider"
                    .into(),
            );
        }
        let anthropic_endpoint =
            crate::inference::clients::claude_subscription::ANTHROPIC_API_ENDPOINT;
        if self.provider_kind == BackendProviderKind::AnthropicApiKey
            && self.endpoint.trim_end_matches('/') != anthropic_endpoint
        {
            violations.push(format!(
                "AnthropicApiKey endpoint must be {anthropic_endpoint}; a different endpoint is not supported"
            ));
        }
        violations
    }

    pub fn validate(&self) -> Result<()> {
        let violations = self.validation_violations();
        anyhow::ensure!(violations.is_empty(), "{}", violations.join("; "));
        Ok(())
    }

    pub fn catalog_scope(&self) -> Option<&str> {
        self.auth
            .uses_node_oauth()
            .then_some(self.node_did.as_str())
    }
}

impl BackendAuth {
    /// The key a health probe may send: cloud credentials sign requests rather than
    /// travel as a key, so a probe has none to resolve for them.
    pub fn probe_key(&self) -> Result<Option<String>> {
        match self {
            Self::AwsCredentials { .. } | Self::GoogleCredentials { .. } => Ok(None),
            other => other.resolve_api_key(),
        }
    }

    pub fn resolve_api_key(&self) -> Result<Option<String>> {
        match self {
            Self::Unauthenticated => Ok(None),
            Self::ApiKey { key } => {
                anyhow::ensure!(!key.trim().is_empty(), "API key must not be blank");
                Ok(Some(key.clone()))
            }
            Self::Environment { variable } => {
                anyhow::ensure!(
                    !variable.trim().is_empty(),
                    "API key environment variable must not be blank"
                );
                let key = std::env::var(variable).with_context(|| {
                    format!("API key environment variable {variable:?} is unavailable")
                })?;
                anyhow::ensure!(
                    !key.trim().is_empty(),
                    "API key environment variable {variable:?} is blank"
                );
                Ok(Some(key))
            }
            Self::NodeOAuth { .. } | Self::NodeOAuthPool { .. } => {
                anyhow::bail!("node OAuth requires the invoking node's OAuthCredential")
            }
            Self::Credential { .. } => {
                anyhow::bail!("backend credential requires runtime resolution")
            }
            // AWS credentials sign each request and Google credentials mint
            // tokens; neither is a key to send.
            Self::AwsCredentials { .. } | Self::GoogleCredentials { .. } => Ok(None),
        }
    }
}

impl InferenceBackendObservation {
    pub fn display_state(&self, enabled: bool) -> &'static str {
        derive_display_state(
            enabled,
            self.probe_status.as_deref().unwrap_or(UNKNOWN_PROBE_STATUS),
        )
    }

    pub fn catalog_for(&self, principal: Option<&str>) -> Result<Option<&BackendModelCatalog>> {
        let mut catalogs = self
            .catalogs
            .iter()
            .filter(|catalog| catalog.node_did.as_deref() == principal);
        let catalog = catalogs.next();
        anyhow::ensure!(
            catalogs.next().is_none(),
            "ambiguous backend catalog authentication scope"
        );
        Ok(catalog)
    }
}

pub fn derive_display_state(enabled: bool, probe_status: &str) -> &'static str {
    if !enabled {
        return "disabled";
    }
    match probe_status {
        "healthy" => "available",
        "unhealthy" => "unhealthy",
        "stale" => "stale",
        "rate_limited" => "rate-limited",
        "circuit_open" => "circuit-open",
        "unknown" => "unknown",
        _ => "unknown",
    }
}

fn scope_filter(node_did: &str, backend_id: &str) -> Result<String> {
    anyhow::ensure!(
        !node_did.trim().is_empty(),
        "backend owner DID must not be blank"
    );
    anyhow::ensure!(
        !backend_id.trim().is_empty(),
        "backend ID must not be blank"
    );
    Ok(format!(
        r#"{{ node_did: {{ _eq: "{}" }}, backend_id: {{ _eq: "{}" }} }}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(backend_id)
    ))
}

pub async fn lookup_backend(
    node: &EmbeddedNode,
    node_did: &str,
    backend_id: &str,
) -> Result<Option<InferenceBackend>> {
    Ok(lookup_backend_record(node, node_did, backend_id)
        .await?
        .map(|(_, backend)| backend))
}

pub(crate) async fn lookup_backend_record(
    node: &EmbeddedNode,
    node_did: &str,
    backend_id: &str,
) -> Result<Option<(String, InferenceBackend)>> {
    let filter = scope_filter(node_did, backend_id)?;
    let mut records = query_backend_records(node, &format!("filter: {filter}"), false).await?;
    anyhow::ensure!(
        records.len() <= 1,
        "ambiguous backend reference {backend_id:?} for {node_did:?}"
    );
    Ok(records.pop())
}

// A stored row whose provider_kind this build does not know (a newer peer
// wrote it). Lists skip it; single-document decodes stay strict.
pub(crate) fn unknown_provider_kind(row: &serde_json::Value) -> Option<&str> {
    let kind = row.get("provider_kind")?.as_str()?;
    (!BackendProviderKind::ALL
        .iter()
        .any(|known| known.as_str() == kind))
    .then_some(kind)
}

async fn query_backend_records(
    node: &EmbeddedNode,
    arguments: &str,
    skip_unknown_kind: bool,
) -> Result<Vec<(String, InferenceBackend)>> {
    let query = format!(
        "query {{ InferenceBackendMetadata({arguments}) {{ _docID {BACKEND_CONFIG_FIELDS} }} }}"
    );
    let response = crate::store::graphql::graphql_with_transaction_retry(
        node,
        &query,
        "query_backend_records",
    )
    .await?;
    decode_backend_records(response.data.as_ref(), skip_unknown_kind)
}

fn decode_backend_records(
    data: Option<&serde_json::Value>,
    skip_unknown_kind: bool,
) -> Result<Vec<(String, InferenceBackend)>> {
    data.and_then(|data| data.get("InferenceBackendMetadata"))
        .and_then(serde_json::Value::as_array)
        .context("InferenceBackend query did not return rows")?
        .iter()
        .filter(|row| {
            let Some(provider_kind) = unknown_provider_kind(row).filter(|_| skip_unknown_kind)
            else {
                return true;
            };
            tracing::warn!(
                backend_id = row.get("backend_id").and_then(serde_json::Value::as_str),
                provider_kind,
                "skipping backend of a provider kind this build does not know"
            );
            false
        })
        .map(|row| {
            let doc_id = row
                .get("_docID")
                .and_then(serde_json::Value::as_str)
                .context("InferenceBackend row missing _docID")?;
            Ok((
                doc_id.to_owned(),
                InferenceBackend::metadata_from_value(row)?,
            ))
        })
        .collect()
}

pub(crate) async fn list_backend_records(
    node: &EmbeddedNode,
) -> Result<Vec<(String, InferenceBackend)>> {
    query_backend_records(node, "order: { backend_id: ASC }", true).await
}

pub async fn list_all_backends(node: &EmbeddedNode) -> Result<Vec<InferenceBackend>> {
    Ok(list_backend_records(node)
        .await?
        .into_iter()
        .map(|(_, backend)| backend)
        .collect())
}

pub async fn list_enabled_backends(node: &EmbeddedNode) -> Result<Vec<InferenceBackend>> {
    Ok(
        query_backend_records(node, "filter: { enabled: { _eq: true } }", true)
            .await?
            .into_iter()
            .map(|(_, backend)| backend)
            .collect(),
    )
}

fn enabled_backend_filter(node_did: &str) -> String {
    format!(
        r#"filter: {{ node_did: {{ _eq: "{}" }}, enabled: {{ _eq: true }} }}"#,
        escape_graphql_string(node_did)
    )
}

fn unique_backends(records: Vec<(String, InferenceBackend)>) -> Result<Vec<InferenceBackend>> {
    let mut ids = std::collections::BTreeSet::new();
    records
        .into_iter()
        .map(|(_, backend)| {
            anyhow::ensure!(
                ids.insert(backend.backend_id.clone()),
                "ambiguous backend reference"
            );
            Ok(backend)
        })
        .collect()
}

pub async fn list_enabled_backends_for_agent(
    node: &EmbeddedNode,
    node_did: &str,
) -> Result<Vec<InferenceBackend>> {
    anyhow::ensure!(
        !node_did.trim().is_empty(),
        "backend owner must be nonempty"
    );
    unique_backends(query_backend_records(node, &enabled_backend_filter(node_did), true).await?)
}

/// Lists the held Node's visible backend configuration for its operational prober.
pub(crate) async fn list_enabled_backends_for_runtime(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
) -> Result<Vec<InferenceBackend>> {
    let access = crate::inference::auth::credential::runtime_credential_access(node, node_did)?;
    let filter = enabled_backend_filter(node_did);
    let response = access
        .execute(&format!(
            "query {{ InferenceBackendMetadata({filter}) {{ _docID {BACKEND_CONFIG_FIELDS} }} }}"
        ))
        .await?;
    unique_backends(decode_backend_records(response.get("data"), true)?)
}

pub(crate) async fn lookup_backend_observation_for_runtime(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    backend_id: &str,
) -> Result<Option<InferenceBackendObservation>> {
    let access = crate::inference::auth::credential::runtime_credential_access(node, node_did)?;
    access
        .transact_readonly("backend_registry.runtime_observation", |txn| {
            Box::pin(
                async move { lookup_backend_observation_in_txn(txn, node_did, backend_id).await },
            )
        })
        .await
}

pub async fn lookup_backend_observation(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    backend_id: &str,
) -> Result<Option<InferenceBackendObservation>> {
    crate::store::config_client::ConfigAccess::transact_local_readonly(
        node,
        None,
        "backend_registry.observation",
        |txn| {
            Box::pin(
                async move { lookup_backend_observation_in_txn(txn, node_did, backend_id).await },
            )
        },
    )
    .await
}

pub async fn lookup_backend_observation_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    backend_id: &str,
) -> Result<Option<InferenceBackendObservation>> {
    let filter = scope_filter(node_did, backend_id)?;
    let response = txn.execute(&format!(
        "query {{ InferenceBackendMetadata(filter: {filter}, limit: 2) {{ _docID }} InferenceBackendObservation(filter: {filter}) {{ node_did metadata_doc_id backend_id catalogs catalog_freshness probe_status last_probe }} }}"
    )).await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get("InferenceBackendObservation"))
        .and_then(serde_json::Value::as_array)
        .context("backend observation query did not return rows")?;
    let metadata =
        gents_protocol::graphql::graphql_rows_from_response(&response, "InferenceBackendMetadata");
    let [metadata] = metadata.as_slice() else {
        anyhow::ensure!(
            rows.is_empty(),
            "backend observation does not match current metadata"
        );
        return Ok(None);
    };
    let metadata_doc = metadata["_docID"]
        .as_str()
        .context("backend metadata has no physical ID")?;
    observation_binding::bind_rows(node_did, backend_id, metadata_doc, rows)?
        .map(|row| decode_backend_observation(row.clone()).context("decoding backend observation"))
        .transpose()
}

pub async fn set_backend_probe_status(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    backend_id: &str,
    probe_status: &str,
) -> Result<()> {
    set_backend_probe_status_as(
        &crate::ConfigAccess::local(node.clone()),
        node_did,
        backend_id,
        probe_status,
    )
    .await
}

/// Store a probe observation using the explicit actor that owns the operation.
pub async fn set_backend_probe_status_as(
    access: &crate::ConfigAccess,
    node_did: &str,
    backend_id: &str,
    probe_status: &str,
) -> Result<()> {
    write_probe_observation(
        access,
        node_did,
        backend_id,
        serde_json::json!({"probe_status": probe_status}),
        None,
    )
    .await
}

pub async fn set_backend_probe_status_with_last_probe(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    backend_id: &str,
    probe_status: &str,
    last_probe: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    write_probe_observation(
        &crate::ConfigAccess::local(node.clone()),
        node_did,
        backend_id,
        serde_json::json!({"probe_status": probe_status, "last_probe": last_probe.to_rfc3339()}),
        None,
    )
    .await
}

pub(crate) async fn set_backend_probe_status_for_connection(
    node: &Arc<EmbeddedNode>,
    backend: &InferenceBackend,
    expected_doc: &str,
    probe_status: &str,
    last_probe: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    write_probe_observation(
        &crate::inference::auth::credential::runtime_credential_access(node, &backend.node_did)?,
        &backend.node_did,
        &backend.backend_id,
        serde_json::json!({"probe_status": probe_status, "last_probe": last_probe.to_rfc3339()}),
        Some((backend, expected_doc)),
    )
    .await
}

async fn write_probe_observation(
    access: &crate::ConfigAccess,
    node_did: &str,
    backend_id: &str,
    input: serde_json::Value,
    expected: Option<(&InferenceBackend, &str)>,
) -> Result<()> {
    access
        .transact_write("backend_registry.update_probe", |txn| {
            let input = input.clone();
            Box::pin(async move {
                let (metadata_doc, value) = crate::store::config_client::read_desired_state_record_in_txn(
                    txn, crate::Collection::InferenceBackend, node_did, backend_id,
                )
                .await?
                .context("backend observation target does not exist")?;
                let current = InferenceBackend::from_value(&value)?;
                if let Some((expected, expected_doc)) = expected {
                    anyhow::ensure!(metadata_doc == expected_doc, "backend was replaced during probe");
                    anyhow::ensure!(current.enabled &&
                        crate::inference::admission::backend_connection_fingerprint(&current.backend_fields()) ==
                        crate::inference::admission::backend_connection_fingerprint(&expected.backend_fields()),
                        "backend connection changed during probe");
                }
                let filter = scope_filter(node_did, backend_id)?;
                let response = txn.execute(&format!("{{ InferenceBackendObservation(filter: {filter}, limit: 2) {{ _docID metadata_doc_id }} }}")).await?;
                let rows = gents_protocol::graphql::graphql_rows_from_response(&response, "InferenceBackendObservation");
                anyhow::ensure!(rows.len() == 1, "backend observation is missing or ambiguous");
                anyhow::ensure!(rows[0]["metadata_doc_id"] == metadata_doc, "backend observation does not match current metadata");
                let doc_id = rows[0]["_docID"].as_str().context("backend observation has no physical identity")?;
                let mutation = format!(
                    r#"mutation($input: InferenceBackendObservationMutationInputArg!) {{ update_InferenceBackendObservation(docID: "{}", input: $input) {{ _docID }} }}"#,
                    escape_graphql_string(doc_id)
                );
                txn.execute_client_signed_update(&mutation, &serde_json::json!({"input": input}), gents_protocol::schemas::INFERENCE_BACKEND_OBSERVATION).await?;
                Ok(())
            })
        })
        .await
}

// The connection is re-checked in the transaction so an in-flight probe
// cannot publish a catalog for a replaced provider/authentication selection.
#[cfg(test)]
pub(crate) async fn record_model_catalog(
    node: &std::sync::Arc<EmbeddedNode>,
    backend: &InferenceBackend,
    catalog: BackendModelCatalog,
) -> Result<()> {
    crate::store::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "backend_registry.record_catalog",
        |txn| {
            let catalog = catalog.clone();
            Box::pin(async move { record_model_catalog_in_txn(txn, backend, catalog).await })
        },
    )
    .await
}

pub(crate) async fn capture_backend_document(
    node: &Arc<EmbeddedNode>,
    backend: &InferenceBackend,
) -> Result<String> {
    crate::inference::auth::credential::runtime_credential_access(node, &backend.node_did)?
        .transact_readonly("backend_registry.capture_probe", |txn| {
            Box::pin(async move {
                let (doc, value) = crate::store::config_client::read_desired_state_record_in_txn(
                    txn,
                    crate::Collection::InferenceBackend,
                    &backend.node_did,
                    &backend.backend_id,
                )
                .await?
                .context("backend is unavailable")?;
                let current = InferenceBackend::from_value(&value)?;
                anyhow::ensure!(
                    current.enabled
                        && crate::inference::admission::backend_connection_fingerprint(
                            &current.backend_fields()
                        ) == crate::inference::admission::backend_connection_fingerprint(
                            &backend.backend_fields()
                        ),
                    "backend connection changed"
                );
                Ok(doc)
            })
        })
        .await
}

pub(crate) async fn record_model_catalog_for_document(
    node: &Arc<EmbeddedNode>,
    backend: &InferenceBackend,
    doc: &str,
    catalog: BackendModelCatalog,
) -> Result<()> {
    crate::inference::auth::credential::runtime_credential_access(node, &backend.node_did)?
        .transact_write("backend_registry.record_catalog_document", |txn| {
            let catalog = catalog.clone();
            Box::pin(async move {
                record_model_catalog_for_document_in_txn(txn, backend, Some(doc), catalog).await
            })
        })
        .await
}

pub async fn record_model_catalog_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    backend: &InferenceBackend,
    catalog: BackendModelCatalog,
) -> Result<()> {
    record_model_catalog_for_document_in_txn(txn, backend, None, catalog).await
}

async fn record_model_catalog_for_document_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    backend: &InferenceBackend,
    expected_doc: Option<&str>,
    catalog: BackendModelCatalog,
) -> Result<()> {
    anyhow::ensure!(
        catalog.node_did.as_deref() == backend.catalog_scope(),
        "catalog credential scope does not match backend authentication"
    );
    let observed_at = chrono::DateTime::parse_from_rfc3339(&catalog.observed_at)
        .context("invalid catalog observation timestamp")?;
    let mut names = rapidhash::RapidHashSet::default();
    for model in &catalog.models {
        anyhow::ensure!(
            !model.model_name.trim().is_empty() && model.model_name.trim() == model.model_name,
            "catalog model name must be nonempty and canonical"
        );
        anyhow::ensure!(
            names.insert(&model.model_name),
            "duplicate catalog model name"
        );
        anyhow::ensure!(
            model.context_window.is_none_or(|value| value > 0)
                && model.max_output_tokens.is_none_or(|value| value > 0),
            "advertised token limits must be positive when known"
        );
    }
    let filter = scope_filter(&backend.node_did, &backend.backend_id)?;
    let response = txn.execute(&format!(
                    "{{ InferenceBackendMetadata(filter: {filter}, limit: 2) {{ _docID {BACKEND_CONFIG_FIELDS} }} InferenceBackendObservation(filter: {filter}, limit: 2) {{ _docID node_did metadata_doc_id backend_id catalogs catalog_freshness }} }}"
                )).await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get("InferenceBackendMetadata"))
        .and_then(serde_json::Value::as_array)
        .context("backend catalog query returned no rows")?;
    anyhow::ensure!(
        rows.len() == 1,
        "backend catalog target is absent or ambiguous"
    );
    let row = &rows[0];
    if let Some(doc) = expected_doc {
        anyhow::ensure!(
            row["_docID"] == doc,
            "backend was replaced during discovery"
        );
    }
    let current = InferenceBackend::metadata_from_value(row)?;
    anyhow::ensure!(
        current.endpoint == backend.endpoint
            && current.provider_kind == backend.provider_kind
            && current.auth == backend.auth,
        "backend connection changed during discovery"
    );
    let observations = gents_protocol::graphql::graphql_rows_from_response(
        &response,
        "InferenceBackendObservation",
    );
    let metadata_doc = row["_docID"]
        .as_str()
        .context("backend metadata has no physical ID")?;
    let observation = observation_binding::bind_rows(
        &backend.node_did,
        &backend.backend_id,
        metadata_doc,
        &observations,
    )?
    .context("backend observation is missing")?;
    let mut catalogs = serde_json::from_value::<InferenceBackendObservation>(observation.clone())
        .context("decoding backend catalogs")?
        .catalogs;
    let matching: Vec<_> = catalogs
        .iter()
        .enumerate()
        .filter(|(_, old)| old.node_did == catalog.node_did)
        .collect();
    anyhow::ensure!(
        matching.len() <= 1,
        "ambiguous catalog authentication scope"
    );
    let mut freshness = catalog_freshness::Freshness::load(observation, &catalogs)?;
    let mut content_changed = true;
    let scope_index;
    let stamp = catalog.observed_at.clone();
    if let Some((index, old)) = matching.first() {
        let latest = freshness.observed_at(old)?;
        if observed_at < latest {
            return Ok(());
        }
        scope_index = *index;
        if old.models == catalog.models {
            if observed_at == latest && freshness.contains_scope(&catalog.node_did) {
                return Ok(());
            }
            content_changed = false;
        } else {
            catalogs[scope_index] = catalog;
        }
    } else {
        scope_index = catalogs.len();
        catalogs.push(catalog);
    }
    freshness.record(&catalogs[scope_index], stamp)?;
    let mut input = serde_json::Map::new();
    input.insert("catalog_freshness".into(), serde_json::to_value(freshness)?);
    if content_changed {
        // A JSON scalar keeps its model array inside an object, rather than
        // publishing a top-level JsonArray to DefraDB's Scalar(Json) field.
        input.insert(
            "catalogs".into(),
            serde_json::json!({ "entries": catalogs }),
        );
    }
    let doc_id = observation
        .get("_docID")
        .and_then(serde_json::Value::as_str)
        .context("backend catalog target has no physical identity")?;
    txn.execute_client_signed_update(
        &format!("mutation($input: InferenceBackendObservationMutationInputArg!) {{ update_InferenceBackendObservation(docID: \"{}\", input: $input) {{ _docID }} }}", escape_graphql_string(doc_id)),
        &serde_json::json!({"input":input}),
        gents_protocol::schemas::INFERENCE_BACKEND_OBSERVATION,
    ).await?;
    Ok(())
}

// The credential that authorized discovery has no representation in
// `BackendModelCatalog`.
pub async fn record_discovered_catalog_on(
    access: &crate::store::config_client::ConfigAccess,
    backend: &InferenceBackend,
    models: Vec<AdvertisedModel>,
) -> Result<BackendModelCatalog> {
    let catalog = BackendModelCatalog {
        node_did: backend.catalog_scope().map(str::to_owned),
        observed_at: chrono::Utc::now().to_rfc3339(),
        models,
    };
    // The catalog write re-reads the backend's metadata and observation rows
    // in its transaction and has no other effect, so a confirmed conflict with
    // the runtime's own probe of the same backend is rebuilt from fresh reads
    // and signed again rather than surfaced.
    access
        .transact_signed_conflict_rebuild("backend_registry.record_operator_catalog", |txn| {
            let catalog = catalog.clone();
            Box::pin(async move { record_model_catalog_in_txn(txn, backend, catalog).await })
        })
        .await?;
    Ok(catalog)
}

pub async fn record_connection_catalog_on(
    access: &crate::store::config_client::ConfigAccess,
    owner: &str,
    provider_kind: BackendProviderKind,
    endpoint: &str,
    auth: &BackendAuth,
    models: Vec<AdvertisedModel>,
) -> Result<usize> {
    anyhow::ensure!(!owner.trim().is_empty(), "backend owner must be nonempty");
    let query = format!(
        r#"{{ InferenceBackendMetadata(filter: {{ node_did: {{ _eq: "{}" }}, provider_kind: {{ _eq: "{}" }} }}) {{ {BACKEND_CONFIG_FIELDS} }} }}"#,
        escape_graphql_string(owner),
        escape_graphql_string(provider_kind.as_str()),
    );
    let observed_at = chrono::Utc::now().to_rfc3339();
    access
        .transact_write("backend_registry.record_connection_catalog", |txn| {
            let (query, models, observed_at) = (&query, &models, &observed_at);
            Box::pin(async move {
                let response = txn.execute(query).await?;
                let rows = response
                    .get("data")
                    .and_then(|data| data.get("InferenceBackendMetadata"))
                    .and_then(serde_json::Value::as_array)
                    .context("backend connection query returned no rows")?;
                let mut recorded = 0;
                for row in rows {
                    let backend = InferenceBackend::metadata_from_value(row)?;
                    if backend.endpoint != endpoint {
                        continue;
                    }
                    let auth_matches = match (&backend.auth, auth) {
                        (BackendAuth::Credential { id, version }, raw)
                            if raw.is_stored_secret() =>
                        {
                            let credential =
                                crate::inference::auth::credential::backend_credential_in_txn(
                                    txn, id,
                                )
                                .await?;
                            credential.matches_binding(
                                owner,
                                provider_kind.as_str(),
                                endpoint,
                                version,
                            ) && &credential.secret.into_backend_auth() == raw
                        }
                        (stored, selected) => stored == selected,
                    };
                    if !auth_matches {
                        continue;
                    }
                    let catalog = BackendModelCatalog {
                        node_did: backend.catalog_scope().map(str::to_owned),
                        observed_at: observed_at.clone(),
                        models: models.clone(),
                    };
                    record_model_catalog_in_txn(txn, &backend, catalog).await?;
                    recorded += 1;
                }
                Ok(recorded)
            })
        })
        .await
}

// Other authentication scopes' advertisements must never be exposed.
pub fn scoped_observation_view(
    observation: Option<&InferenceBackendObservation>,
    owner: &str,
    provider_kind: BackendProviderKind,
) -> Result<serde_json::Value> {
    let scope = provider_kind.is_node_scoped_oauth().then_some(owner);
    let catalog = observation
        .map(|observation| observation.catalog_for(scope))
        .transpose()?
        .flatten();
    Ok(serde_json::json!({
        "catalog": catalog.map(|catalog| serde_json::json!({
            "observed_at": catalog.observed_at,
            "models": catalog.models,
        })),
        "probe_status": observation.and_then(|observation| observation.probe_status.as_deref()),
        "last_probe": observation.and_then(|observation| observation.last_probe.as_deref()),
    }))
}

pub async fn probe_and_promote_enabled_backends(node: &Arc<EmbeddedNode>) {
    let backends = match list_enabled_backends(node).await {
        Ok(backends) => backends,
        Err(error) => {
            tracing::warn!(%error, "startup backend probe: could not list backends");
            return;
        }
    };
    for backend in backends {
        // Agent-scoped credential refresh stays with the invoking-agent owner;
        // a fleet scan cannot choose an OAuth principal.
        if backend.auth.uses_node_oauth() {
            continue;
        }
        if let Err(error) = discover_shared_backend(node, &backend).await {
            tracing::warn!(node_did = %backend.node_did, backend_id = %backend.backend_id, %error, "startup backend probe failed; preserving previous observations");
        }
    }
}

// The caller must resolve and authorize the backend document first; this
// function never accepts a free-standing URL.
pub(crate) async fn discover_shared_backend(
    node: &Arc<EmbeddedNode>,
    backend: &InferenceBackend,
) -> Result<InferenceBackendObservation> {
    let access = crate::ConfigAccess::local(node.clone());
    let doc = capture_backend_document(node, backend).await?;
    let models = discover_runtime_backend_models(&access, backend).await?;
    let now = chrono::Utc::now();
    record_model_catalog_for_document(
        node,
        backend,
        &doc,
        BackendModelCatalog {
            node_did: backend.catalog_scope().map(str::to_owned),
            observed_at: now.to_rfc3339(),
            models,
        },
    )
    .await?;
    set_backend_probe_status_for_connection(
        node,
        backend,
        &doc,
        HEALTHY_PROBE_STATUS,
        chrono::Utc::now(),
    )
    .await?;
    lookup_backend_observation(node, &backend.node_did, &backend.backend_id)
        .await?
        .context("discovered backend observation is missing")
}

/// Observe the exact backend as the calling actor before runtime secret access.
pub async fn discover_runtime_backend_models(
    access: &crate::ConfigAccess,
    backend: &InferenceBackend,
) -> Result<Vec<AdvertisedModel>> {
    let current = access
        .transact_readonly("backend_registry.discovery_authorize", |txn| {
            Box::pin(async move {
                crate::store::config_client::load_inference_backend_in_txn(
                    txn,
                    &backend.node_did,
                    &backend.backend_id,
                )
                .await?
                .context("selected backend is unavailable")
            })
        })
        .await?;
    anyhow::ensure!(
        current.provider_kind == backend.provider_kind
            && current.endpoint == backend.endpoint
            && current.auth == backend.auth
            && current.enabled,
        "selected backend changed or is unavailable"
    );
    let node = access
        .local_node()
        .context("runtime credential resolver unavailable")?;
    backend.validate()?;
    let discovery_timeout =
        std::time::Duration::from_secs(backend.discovery_timeout_secs.unwrap_or(10) as u64);
    let connect_timeout =
        std::time::Duration::from_secs(backend.connect_timeout_secs.unwrap_or(10) as u64)
            .min(discovery_timeout);
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(connect_timeout)
        .timeout(discovery_timeout)
        .build()?;
    let oauth_credential = if backend.auth.uses_node_oauth() {
        let context = crate::inference::backends::health::OAuthProbeContext {
            node: node.clone(),
            node_did: &backend.node_did,
        };
        Some(
            crate::inference::backends::health::oauth::oauth_credential_for_probe(
                &context, backend,
            )
            .await
            .map_err(|_| anyhow::anyhow!("subscription credential is unavailable"))?,
        )
    } else {
        None
    };
    let auth = crate::inference::auth::credential::resolve_runtime_backend_auth(
        node,
        &backend.node_did,
        backend.provider_kind,
        &backend.endpoint,
        &backend.auth,
    )
    .await?;
    let api_key = if oauth_credential.is_some() {
        None
    } else {
        auth.probe_key()?
    };
    let models = crate::inference::backends::provider::discover_models(
        &client,
        backend.provider_kind,
        &backend.endpoint,
        api_key.as_deref(),
        oauth_credential.as_ref(),
    )
    .await
    .map_err(|error| {
        if matches!(&backend.auth, BackendAuth::Credential { .. }) || oauth_credential.is_some() {
            anyhow::anyhow!("backend discovery failed")
        } else {
            error
        }
    })?;
    Ok(models)
}

#[derive(Debug, thiserror::Error)]
pub enum ActorBackendDiscoveryError {
    #[error("selected backend is unavailable")]
    Unavailable,
    #[error("backend discovery failed")]
    Discovery(#[source] anyhow::Error),
    #[error("backend discovery observation could not be committed")]
    Commit(#[source] anyhow::Error),
}

/// Perform an actor-authorized discovery and commit its observation as the
/// runtime only while the actor retains native update authority over that backend.
pub async fn discover_backend_for_actor(
    node: &Arc<EmbeddedNode>,
    actor: &identity::Did,
    node_did: &str,
    backend_id: &str,
) -> std::result::Result<Vec<AdvertisedModel>, ActorBackendDiscoveryError> {
    let actor_access = crate::ConfigAccess::local_as(node.clone(), actor.clone());
    let (doc_id, backend) = actor_access
        .transact_readonly("backend_registry.actor_discovery", |txn| {
            Box::pin(async move {
                let (doc, value) = crate::store::config_client::read_desired_state_record_in_txn(
                    txn,
                    crate::Collection::InferenceBackend,
                    node_did,
                    backend_id,
                )
                .await?
                .context("selected backend is unavailable")?;
                Ok::<_, anyhow::Error>((doc, serde_json::from_value::<InferenceBackend>(value)?))
            })
        })
        .await
        .map_err(|_| ActorBackendDiscoveryError::Unavailable)?;
    crate::store::config_client::ConfigAccess::transact_local_readonly(
        node,
        None,
        "backend_registry.actor_discovery_authorize",
        |txn| {
            let doc_id = &doc_id;
            Box::pin(async move {
                let observed = txn
                    .observe_document_access(
                        actor,
                        "InferenceBackendMetadata",
                        std::slice::from_ref(doc_id),
                        defra_node::DocumentAccessMode::Named {
                            permission: "update".into(),
                            node_permission: acp::nac::NodePermission::DocumentUpdate,
                        },
                    )
                    .await?;
                anyhow::ensure!(
                    matches!(
                        observed.as_slice(),
                        [defra_node::DocumentAccessDecision::Allowed]
                    ),
                    "selected backend is unavailable"
                );
                Ok::<_, anyhow::Error>(())
            })
        },
    )
    .await
    .map_err(|_| ActorBackendDiscoveryError::Unavailable)?;
    let models = discover_runtime_backend_models(&actor_access, &backend)
        .await
        .map_err(ActorBackendDiscoveryError::Discovery)?;
    crate::store::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "backend_registry.actor_discovery_commit",
        |txn| {
            let (doc_id, backend, models, actor) = (&doc_id, &backend, &models, actor);
            Box::pin(async move {
                let observed = txn
                    .observe_document_access(
                        actor,
                        "InferenceBackendMetadata",
                        std::slice::from_ref(doc_id),
                        defra_node::DocumentAccessMode::Named {
                            permission: "update".into(),
                            node_permission: acp::nac::NodePermission::DocumentUpdate,
                        },
                    )
                    .await?;
                anyhow::ensure!(
                    matches!(
                        observed.as_slice(),
                        [defra_node::DocumentAccessDecision::Allowed]
                    ),
                    "selected backend is unavailable"
                );
                let (current_doc, _) =
                    crate::store::config_client::read_desired_state_record_in_txn(
                        txn,
                        crate::Collection::InferenceBackend,
                        &backend.node_did,
                        &backend.backend_id,
                    )
                    .await?
                    .context("selected backend is unavailable")?;
                anyhow::ensure!(current_doc == *doc_id, "selected backend changed");
                record_model_catalog_in_txn(
                    txn,
                    backend,
                    BackendModelCatalog {
                        node_did: backend.catalog_scope().map(str::to_owned),
                        observed_at: chrono::Utc::now().to_rfc3339(),
                        models: models.clone(),
                    },
                )
                .await
            })
        },
    )
    .await
    .map_err(ActorBackendDiscoveryError::Commit)?;
    Ok(models)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod claude_platform_tests;

#[cfg(test)]
mod read_ownership_tests;
