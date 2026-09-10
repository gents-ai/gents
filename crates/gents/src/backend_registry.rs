//! Backend configuration and observation lookups. Logical references are scoped
//! by their owning principal; authentication remains an explicit selection.

use crate::backend_provider::BackendProviderKind;
pub use crate::document_config::InferenceBackend;
use crate::document_config::{BackendAuth, BackendModelCatalog, InferenceBackendObservation};
use crate::graphql::escape_graphql_string;
use crate::openai_wire::OpenAiWireApi;
use anyhow::{Context, Result};
use defra_node::EmbeddedNode;

pub const DEFAULT_MAX_CONCURRENT: i64 = 1;
pub const DEFAULT_MAX_QUEUE_DEPTH: i64 = 100;
pub const HEALTHY_PROBE_STATUS: &str = "healthy";
pub const UNKNOWN_PROBE_STATUS: &str = "unknown";

/// Effective provider input assembled from one backend connection. Credentials
/// retain their explicit selection until resolved by the invoking principal.
#[derive(Debug, Clone)]
pub struct BackendFields {
    pub backend_id: Option<String>,
    pub backend_provider_kind: BackendProviderKind,
    pub openai_wire_api: OpenAiWireApi,
    pub backend_endpoint: String,
    pub backend_auth: BackendAuth,
}

pub(crate) const BACKEND_CONFIG_FIELDS: &str = "agent_did backend_id name provider_kind openai_wire_api endpoint auth connect_timeout_secs discovery_timeout_secs max_concurrent max_queue_depth enabled tags";

impl InferenceBackend {
    pub fn from_value(value: &serde_json::Value) -> Result<Self> {
        let mut config = value.clone();
        let object = config
            .as_object_mut()
            .context("InferenceBackend must be an object")?;
        // DefraDB identity and runtime observations are envelopes, never desired config.
        for field in [
            "_docID",
            "_deleted",
            "updated_at",
            "catalogs",
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
                &self.backend_id,
            ),
            backend_endpoint: self.endpoint.clone(),
            backend_auth: self.auth.clone(),
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
            ("agent_did", self.agent_did.as_str()),
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
        if matches!(self.auth, BackendAuth::PrincipalOAuth)
            != self.provider_kind.is_agent_scoped_oauth()
        {
            violations
                .push("backend auth selection is incompatible with the provider adapter".into());
        }
        violations
    }

    pub fn validate(&self) -> Result<()> {
        let violations = self.validation_violations();
        anyhow::ensure!(violations.is_empty(), "{}", violations.join("; "));
        Ok(())
    }
}

impl BackendAuth {
    /// Resolve only shared credentials. OAuth callers must use the existing
    /// agent-scoped OAuthCredential owner, not fall back to unauthenticated HTTP.
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
            Self::PrincipalOAuth => {
                anyhow::bail!("principal OAuth requires the invoking principal's OAuthCredential")
            }
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

    /// Exact authentication scope: shared credentials do not imply OAuth
    /// entitlements, and one principal never inherits another's advertised list.
    pub fn catalog_for(&self, principal: Option<&str>) -> Result<Option<&BackendModelCatalog>> {
        let mut catalogs = self
            .catalogs
            .iter()
            .filter(|catalog| catalog.agent_did.as_deref() == principal);
        let catalog = catalogs.next();
        anyhow::ensure!(
            catalogs.next().is_none(),
            "ambiguous backend catalog authentication scope"
        );
        Ok(catalog)
    }
}

/// Pure function backing [`InferenceBackendObservation::display_state`]. Lives outside
/// the impl so the Tauri bridge can call it on raw `(enabled, probe_status)`
/// pairs from the Lean witness fixtures without constructing a full
/// `InferenceBackend`.
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

fn scope_filter(agent_did: &str, backend_id: &str) -> Result<String> {
    anyhow::ensure!(
        !agent_did.trim().is_empty(),
        "backend owner DID must not be blank"
    );
    anyhow::ensure!(
        !backend_id.trim().is_empty(),
        "backend ID must not be blank"
    );
    Ok(format!(
        r#"{{ agent_did: {{ _eq: "{}" }}, backend_id: {{ _eq: "{}" }} }}"#,
        escape_graphql_string(agent_did),
        escape_graphql_string(backend_id)
    ))
}

pub async fn lookup_backend(
    node: &EmbeddedNode,
    agent_did: &str,
    backend_id: &str,
) -> Result<Option<InferenceBackend>> {
    Ok(lookup_backend_record(node, agent_did, backend_id)
        .await?
        .map(|(_, backend)| backend))
}

pub(crate) async fn lookup_backend_record(
    node: &EmbeddedNode,
    agent_did: &str,
    backend_id: &str,
) -> Result<Option<(String, InferenceBackend)>> {
    let filter = scope_filter(agent_did, backend_id)?;
    let mut records = query_backend_records(node, &format!("filter: {filter}")).await?;
    anyhow::ensure!(
        records.len() <= 1,
        "ambiguous backend reference {backend_id:?} for {agent_did:?}"
    );
    Ok(records.pop())
}

async fn query_backend_records(
    node: &EmbeddedNode,
    arguments: &str,
) -> Result<Vec<(String, InferenceBackend)>> {
    let query =
        format!("query {{ InferenceBackend({arguments}) {{ _docID {BACKEND_CONFIG_FIELDS} }} }}");
    let response = node.execute(&query).await;
    anyhow::ensure!(
        !response.has_errors(),
        "query InferenceBackend failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceBackend"))
        .and_then(serde_json::Value::as_array)
        .context("InferenceBackend query did not return rows")?
        .iter()
        .map(|row| {
            let doc_id = row
                .get("_docID")
                .and_then(serde_json::Value::as_str)
                .context("InferenceBackend row missing _docID")?;
            Ok((doc_id.to_owned(), InferenceBackend::from_value(row)?))
        })
        .collect()
}

/// Administrative enumeration only. Callers resolving a logical reference must
/// use lookup_backend_record with the owning DID.
pub(crate) async fn list_backend_records(
    node: &EmbeddedNode,
) -> Result<Vec<(String, InferenceBackend)>> {
    query_backend_records(node, "order: { backend_id: ASC }").await
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
        query_backend_records(node, "filter: { enabled: { _eq: true } }")
            .await?
            .into_iter()
            .map(|(_, backend)| backend)
            .collect(),
    )
}

/// Enumerate enabled configuration only within the selected principal.
pub async fn list_enabled_backends_for_agent(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<Vec<InferenceBackend>> {
    anyhow::ensure!(
        !agent_did.trim().is_empty(),
        "backend owner must be nonempty"
    );
    let filter = format!(
        r#"filter: {{ agent_did: {{ _eq: "{}" }}, enabled: {{ _eq: true }} }}"#,
        escape_graphql_string(agent_did)
    );
    let records = query_backend_records(node, &filter).await?;
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

pub async fn lookup_backend_observation(
    node: &EmbeddedNode,
    agent_did: &str,
    backend_id: &str,
) -> Result<Option<InferenceBackendObservation>> {
    crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "backend_registry.observation",
        |txn| {
            Box::pin(
                async move { lookup_backend_observation_in_txn(txn, agent_did, backend_id).await },
            )
        },
    )
    .await
}

/// Read the existing observation inside a caller's configuration transaction.
pub async fn lookup_backend_observation_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    agent_did: &str,
    backend_id: &str,
) -> Result<Option<InferenceBackendObservation>> {
    let filter = scope_filter(agent_did, backend_id)?;
    let response = txn.execute(&format!(
        "query {{ InferenceBackend(filter: {filter}) {{ backend_id catalogs probe_status last_probe }} }}"
    )).await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get("InferenceBackend"))
        .and_then(serde_json::Value::as_array)
        .context("backend observation query did not return rows")?;
    anyhow::ensure!(rows.len() <= 1, "ambiguous backend observation reference");
    rows.first()
        .map(|row| serde_json::from_value(row.clone()).context("decoding backend observation"))
        .transpose()
}

pub async fn set_backend_probe_status(
    node: &EmbeddedNode,
    agent_did: &str,
    backend_id: &str,
    probe_status: &str,
) -> Result<()> {
    write_probe_observation(
        node,
        agent_did,
        backend_id,
        serde_json::json!({"probe_status": probe_status}),
    )
    .await
}

pub async fn set_backend_probe_status_with_last_probe(
    node: &EmbeddedNode,
    agent_did: &str,
    backend_id: &str,
    probe_status: &str,
    last_probe: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    write_probe_observation(
        node,
        agent_did,
        backend_id,
        serde_json::json!({"probe_status": probe_status, "last_probe": last_probe.to_rfc3339()}),
    )
    .await
}

async fn write_probe_observation(
    node: &EmbeddedNode,
    agent_did: &str,
    backend_id: &str,
    input: serde_json::Value,
) -> Result<()> {
    // Resolve a unique scoped document before writing; a duplicate logical key
    // must never cause a multi-document update.
    let (doc_id, _) = lookup_backend_record(node, agent_did, backend_id)
        .await?
        .context("backend observation target does not exist")?;
    let input = gents_protocol::graphql::graphql_input_literal(&input)?;
    let mutation = format!(
        r#"mutation {{ update_InferenceBackend(docID: "{}", input: {input}) {{ _docID }} }}"#,
        escape_graphql_string(&doc_id)
    );
    crate::config_client::ConfigAccess::write_local_response(
        node,
        "backend_registry.update_probe",
        &mutation,
    )
    .await?;
    Ok(())
}

/// Record a successful discovery without overwriting another credential scope.
/// The connection is checked again in the transaction so an in-flight probe
/// cannot publish a catalog for a replaced provider/authentication selection.
pub(crate) async fn record_model_catalog(
    node: &EmbeddedNode,
    backend: &InferenceBackend,
    catalog: BackendModelCatalog,
) -> Result<()> {
    crate::config_client::ConfigAccess::transact_local(
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

/// Publish through the same observation owner for local and remote transactions.
/// The caller supplies the backend configuration actually used for discovery.
pub async fn record_model_catalog_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    backend: &InferenceBackend,
    catalog: BackendModelCatalog,
) -> Result<()> {
    let expected_scope =
        matches!(backend.auth, BackendAuth::PrincipalOAuth).then_some(backend.agent_did.as_str());
    anyhow::ensure!(
        catalog.agent_did.as_deref() == expected_scope,
        "catalog credential scope does not match backend authentication"
    );
    let observed_at = chrono::DateTime::parse_from_rfc3339(&catalog.observed_at)
        .context("invalid catalog observation timestamp")?;
    let mut names = std::collections::HashSet::new();
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
    let filter = scope_filter(&backend.agent_did, &backend.backend_id)?;
    let response = txn.execute(&format!(
                    "{{ InferenceBackend(filter: {filter}, limit: 2) {{ _docID {BACKEND_CONFIG_FIELDS} catalogs }} }}"
                )).await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get("InferenceBackend"))
        .and_then(serde_json::Value::as_array)
        .context("backend catalog query returned no rows")?;
    anyhow::ensure!(
        rows.len() == 1,
        "backend catalog target is absent or ambiguous"
    );
    let row = &rows[0];
    let current = InferenceBackend::from_value(row)?;
    anyhow::ensure!(
        current.endpoint == backend.endpoint
            && current.provider_kind == backend.provider_kind
            && current.auth == backend.auth,
        "backend connection changed during discovery"
    );
    let mut catalogs = serde_json::from_value::<InferenceBackendObservation>(row.clone())
        .context("decoding backend catalogs")?
        .catalogs;
    let matching: Vec<_> = catalogs
        .iter()
        .enumerate()
        .filter(|(_, old)| old.agent_did == catalog.agent_did)
        .collect();
    anyhow::ensure!(
        matching.len() <= 1,
        "ambiguous catalog authentication scope"
    );
    if let Some((index, old)) = matching.first() {
        if chrono::DateTime::parse_from_rfc3339(&old.observed_at)? > observed_at {
            return Ok(());
        }
        let index = *index;
        catalogs[index] = catalog;
    } else {
        catalogs.push(catalog);
    }
    // DefraDB represents a top-level JSON array as JsonArray, which is not a
    // Scalar(Json). Keep the array inside a JSON object so the declared scalar
    // kind and the stored value agree.
    let catalogs = gents_protocol::graphql::graphql_input_literal(
        &serde_json::json!({ "entries": catalogs }),
    )?;
    let doc_id = row
        .get("_docID")
        .and_then(serde_json::Value::as_str)
        .context("backend catalog target has no physical identity")?;
    txn.execute(&format!(
        "mutation {{ update_InferenceBackend(docID: \"{}\", input: {{ catalogs: {catalogs} }}) {{ _docID }} }}",
        escape_graphql_string(doc_id)
    ))
    .await?;
    Ok(())
}

pub async fn probe_and_promote_enabled_backends(node: &EmbeddedNode) {
    let backends = match list_enabled_backends(node).await {
        Ok(backends) => backends,
        Err(error) => {
            tracing::warn!(%error, "startup backend probe: could not list backends");
            return;
        }
    };
    for backend in backends {
        // Agent-scoped credential refresh and discovery remain with the existing
        // invoking-agent owner. A fleet scan cannot choose an OAuth principal.
        if matches!(backend.auth, BackendAuth::PrincipalOAuth) {
            continue;
        }
        if let Err(error) = probe_shared_backend(node, &backend).await {
            tracing::warn!(agent_did = %backend.agent_did, backend_id = %backend.backend_id, %error, "startup backend probe failed; preserving previous observations");
        }
    }
}

async fn probe_shared_backend(node: &EmbeddedNode, backend: &InferenceBackend) -> Result<()> {
    backend.validate()?;
    let discovery_timeout =
        std::time::Duration::from_secs(backend.discovery_timeout_secs.unwrap_or(10) as u64);
    let connect_timeout =
        std::time::Duration::from_secs(backend.connect_timeout_secs.unwrap_or(10) as u64)
            .min(discovery_timeout);
    let client = reqwest::Client::builder()
        .connect_timeout(connect_timeout)
        .timeout(discovery_timeout)
        .build()?;
    let api_key = backend.auth.resolve_api_key()?;
    let models = crate::backend_provider::discover_models(
        &client,
        backend.provider_kind,
        &backend.endpoint,
        api_key.as_deref(),
        None,
    )
    .await?;
    let now = chrono::Utc::now();
    record_model_catalog(
        node,
        backend,
        BackendModelCatalog {
            agent_did: None,
            observed_at: now.to_rfc3339(),
            models,
        },
    )
    .await?;
    set_backend_probe_status_with_last_probe(
        node,
        &backend.agent_did,
        &backend.backend_id,
        HEALTHY_PROBE_STATUS,
        chrono::Utc::now(),
    )
    .await
}

#[cfg(test)]
mod tests;
