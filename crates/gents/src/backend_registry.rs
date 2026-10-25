use crate::backend_provider::BackendProviderKind;
use crate::config_client::ConfigClientError;
pub use crate::document_config::InferenceBackend;
use crate::document_config::{
    AdvertisedModel, BackendAuth, BackendModelCatalog, InferenceBackendObservation,
};
use crate::graphql::escape_graphql_string;
use crate::openai_wire::OpenAiWireApi;
use std::sync::Arc;

use defra_node::EmbeddedNode;

type Result<T, E = BackendRegistryError> = std::result::Result<T, E>;

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
}

#[derive(Debug, thiserror::Error)]
pub enum BackendRegistryError {
    #[error("InferenceBackend must be an object")]
    NotAnObject,
    #[error("decoding InferenceBackend configuration")]
    Decode(#[source] serde_json::Error),
    #[error("{violations}")]
    Invalid { violations: String },
    #[error("API key must not be blank")]
    ApiKeyBlank,
    #[error("API key environment variable must not be blank")]
    EnvironmentVariableBlank,
    #[error("API key environment variable {variable:?} is unavailable")]
    EnvironmentVariableUnavailable {
        variable: String,
        #[source]
        source: std::env::VarError,
    },
    #[error("API key environment variable {variable:?} is blank")]
    EnvironmentVariableEmpty { variable: String },
    #[error("node OAuth requires the invoking node's OAuthCredential")]
    NodeOAuthCredentialRequired,
    #[error("ambiguous backend catalog authentication scope")]
    AmbiguousCatalogScope,
    #[error("ambiguous catalog authentication scope")]
    AmbiguousCatalogEntryScope,
    #[error("backend owner DID must not be blank")]
    OwnerBlank,
    #[error("backend ID must not be blank")]
    BackendIdBlank,
    #[error(transparent)]
    Access(#[from] ConfigClientError),
    #[error(transparent)]
    Graphql(#[from] gents_protocol::graphql::GraphqlError),
    #[error(transparent)]
    BackendProvider(#[from] crate::backend_provider::BackendProviderError),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Timestamp(#[from] chrono::ParseError),
    #[error("ambiguous backend reference {backend_id:?} for {node_did:?}")]
    AmbiguousReference {
        backend_id: String,
        node_did: String,
    },
    #[error("InferenceBackend query did not return rows")]
    QueryRowsMissing,
    #[error("InferenceBackend row missing _docID")]
    RowDocIdMissing,
    #[error("backend owner must be nonempty")]
    OwnerRequired,
    #[error("ambiguous backend reference")]
    AmbiguousScope,
    #[error("backend observation target does not exist")]
    ObservationTargetMissing,
    #[error("backend observation query did not return rows")]
    ObservationRowsMissing,
    #[error("ambiguous backend observation reference")]
    AmbiguousObservation,
    #[error("decoding backend observation")]
    DecodeObservation(#[source] serde_json::Error),
    #[error("invalid catalog observation timestamp")]
    CatalogObservedAt(#[source] chrono::ParseError),
    #[error("catalog credential scope does not match backend authentication")]
    CatalogScopeMismatch,
    #[error("catalog model name must be nonempty and canonical")]
    CatalogModelName,
    #[error("duplicate catalog model name")]
    DuplicateCatalogModel,
    #[error("advertised token limits must be positive when known")]
    TokenLimitsNotPositive,
    #[error("backend catalog query returned no rows")]
    CatalogRowsMissing,
    #[error("backend catalog target is absent or ambiguous")]
    CatalogTargetAmbiguous,
    #[error("backend connection changed during discovery")]
    ConnectionChanged,
    #[error("decoding backend catalogs")]
    DecodeCatalogs(#[source] serde_json::Error),
    #[error("backend catalog target has no physical identity")]
    CatalogDocIdMissing,
    #[error("backend connection query returned no rows")]
    ConnectionRowsMissing,
    #[error("discovered backend observation is missing")]
    DiscoveredObservationMissing,
}

impl crate::config_client::TransactionFailure for BackendRegistryError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Access(error) => Some(error),
            _ => None,
        }
    }
}

pub(crate) const BACKEND_CONFIG_FIELDS: &str = "node_did backend_id name provider_kind openai_wire_api endpoint auth connect_timeout_secs discovery_timeout_secs max_concurrent max_queue_depth enabled tags";

impl InferenceBackend {
    pub fn from_value(value: &serde_json::Value) -> Result<Self, BackendRegistryError> {
        let mut config = value.clone();
        let object = config
            .as_object_mut()
            .ok_or(BackendRegistryError::NotAnObject)?;
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
        let backend: Self = serde_json::from_value(config).map_err(BackendRegistryError::Decode)?;
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
        if matches!(self.auth, BackendAuth::NodeOAuth { .. })
            != self.provider_kind.is_node_scoped_oauth()
        {
            violations
                .push("backend auth selection is incompatible with the provider adapter".into());
        }
        let anthropic_endpoint = crate::claude_subscription::ANTHROPIC_API_ENDPOINT;
        if self.provider_kind == BackendProviderKind::AnthropicApiKey
            && self.endpoint.trim_end_matches('/') != anthropic_endpoint
        {
            violations.push(format!(
                "AnthropicApiKey endpoint must be {anthropic_endpoint}; a different endpoint is not supported"
            ));
        }
        violations
    }

    pub fn validate(&self) -> Result<(), BackendRegistryError> {
        let violations = self.validation_violations();
        if !violations.is_empty() {
            return Err(BackendRegistryError::Invalid {
                violations: violations.join("; "),
            });
        }
        Ok(())
    }

    pub fn catalog_scope(&self) -> Option<&str> {
        matches!(self.auth, BackendAuth::NodeOAuth { .. }).then_some(self.node_did.as_str())
    }
}

impl BackendAuth {
    pub fn resolve_api_key(&self) -> Result<Option<String>, BackendRegistryError> {
        match self {
            Self::Unauthenticated => Ok(None),
            Self::ApiKey { key } => {
                if key.trim().is_empty() {
                    return Err(BackendRegistryError::ApiKeyBlank);
                }
                Ok(Some(key.clone()))
            }
            Self::Environment { variable } => {
                if variable.trim().is_empty() {
                    return Err(BackendRegistryError::EnvironmentVariableBlank);
                }
                let key = std::env::var(variable).map_err(|source| {
                    BackendRegistryError::EnvironmentVariableUnavailable {
                        variable: variable.clone(),
                        source,
                    }
                })?;
                if key.trim().is_empty() {
                    return Err(BackendRegistryError::EnvironmentVariableEmpty {
                        variable: variable.clone(),
                    });
                }
                Ok(Some(key))
            }
            Self::NodeOAuth { .. } => Err(BackendRegistryError::NodeOAuthCredentialRequired),
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

    pub fn catalog_for(
        &self,
        principal: Option<&str>,
    ) -> Result<Option<&BackendModelCatalog>, BackendRegistryError> {
        let mut catalogs = self
            .catalogs
            .iter()
            .filter(|catalog| catalog.node_did.as_deref() == principal);
        let catalog = catalogs.next();
        if catalogs.next().is_some() {
            return Err(BackendRegistryError::AmbiguousCatalogScope);
        }
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

fn scope_filter(node_did: &str, backend_id: &str) -> Result<String, BackendRegistryError> {
    if node_did.trim().is_empty() {
        return Err(BackendRegistryError::OwnerBlank);
    }
    if backend_id.trim().is_empty() {
        return Err(BackendRegistryError::BackendIdBlank);
    }
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
    if records.len() > 1 {
        return Err(BackendRegistryError::AmbiguousReference {
            backend_id: backend_id.to_owned(),
            node_did: node_did.to_owned(),
        });
    }
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
    let query =
        format!("query {{ InferenceBackend({arguments}) {{ _docID {BACKEND_CONFIG_FIELDS} }} }}");
    let response =
        crate::graphql::graphql_with_transaction_retry(node, &query, "query_backend_records")
            .await?;
    response
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceBackend"))
        .and_then(serde_json::Value::as_array)
        .ok_or(BackendRegistryError::QueryRowsMissing)?
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
        .map(|row| -> Result<(String, InferenceBackend)> {
            let doc_id = row
                .get("_docID")
                .and_then(serde_json::Value::as_str)
                .ok_or(BackendRegistryError::RowDocIdMissing)?;
            Ok((doc_id.to_owned(), InferenceBackend::from_value(row)?))
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

pub async fn list_enabled_backends_for_agent(
    node: &EmbeddedNode,
    node_did: &str,
) -> Result<Vec<InferenceBackend>> {
    if node_did.trim().is_empty() {
        return Err(BackendRegistryError::OwnerRequired);
    }
    let filter = format!(
        r#"filter: {{ node_did: {{ _eq: "{}" }}, enabled: {{ _eq: true }} }}"#,
        escape_graphql_string(node_did)
    );
    let records = query_backend_records(node, &filter, true).await?;
    let mut ids = std::collections::BTreeSet::new();
    records
        .into_iter()
        .map(|(_, backend)| -> Result<InferenceBackend> {
            if !ids.insert(backend.backend_id.clone()) {
                return Err(BackendRegistryError::AmbiguousScope);
            }
            Ok(backend)
        })
        .collect()
}

pub async fn lookup_backend_observation(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    backend_id: &str,
) -> Result<Option<InferenceBackendObservation>> {
    crate::config_client::ConfigAccess::transact_local_readonly(
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
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    backend_id: &str,
) -> Result<Option<InferenceBackendObservation>> {
    let filter = scope_filter(node_did, backend_id)?;
    let response = txn.execute(&format!(
        "query {{ InferenceBackend(filter: {filter}) {{ backend_id catalogs probe_status last_probe }} }}"
    )).await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get("InferenceBackend"))
        .and_then(serde_json::Value::as_array)
        .ok_or(BackendRegistryError::ObservationRowsMissing)?;
    if rows.len() > 1 {
        return Err(BackendRegistryError::AmbiguousObservation);
    }
    rows.first()
        .map(|row| {
            serde_json::from_value(row.clone()).map_err(BackendRegistryError::DecodeObservation)
        })
        .transpose()
}

pub async fn set_backend_probe_status(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    backend_id: &str,
    probe_status: &str,
) -> Result<()> {
    write_probe_observation(
        node,
        node_did,
        backend_id,
        serde_json::json!({"probe_status": probe_status}),
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
        node,
        node_did,
        backend_id,
        serde_json::json!({"probe_status": probe_status, "last_probe": last_probe.to_rfc3339()}),
    )
    .await
}

async fn write_probe_observation(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    backend_id: &str,
    input: serde_json::Value,
) -> Result<()> {
    // Resolve a unique scoped document first: a duplicate logical key must
    // never cause a multi-document update.
    let (doc_id, _) = lookup_backend_record(node, node_did, backend_id)
        .await?
        .ok_or(BackendRegistryError::ObservationTargetMissing)?;
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

// The connection is re-checked in the transaction so an in-flight probe
// cannot publish a catalog for a replaced provider/authentication selection.
pub(crate) async fn record_model_catalog(
    node: &std::sync::Arc<EmbeddedNode>,
    backend: &InferenceBackend,
    catalog: BackendModelCatalog,
) -> Result<()> {
    crate::config_client::ConfigAccess::transact_local_write(
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

pub async fn record_model_catalog_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    backend: &InferenceBackend,
    catalog: BackendModelCatalog,
) -> Result<()> {
    if catalog.node_did.as_deref() != backend.catalog_scope() {
        return Err(BackendRegistryError::CatalogScopeMismatch);
    }
    let observed_at = chrono::DateTime::parse_from_rfc3339(&catalog.observed_at)
        .map_err(BackendRegistryError::CatalogObservedAt)?;
    let mut names = rapidhash::RapidHashSet::default();
    for model in &catalog.models {
        if model.model_name.trim().is_empty() || model.model_name.trim() != model.model_name {
            return Err(BackendRegistryError::CatalogModelName);
        }
        if !names.insert(&model.model_name) {
            return Err(BackendRegistryError::DuplicateCatalogModel);
        }
        if !(model.context_window.is_none_or(|value| value > 0)
            && model.max_output_tokens.is_none_or(|value| value > 0))
        {
            return Err(BackendRegistryError::TokenLimitsNotPositive);
        }
    }
    let filter = scope_filter(&backend.node_did, &backend.backend_id)?;
    let response = txn.execute(&format!(
                    "{{ InferenceBackend(filter: {filter}, limit: 2) {{ _docID {BACKEND_CONFIG_FIELDS} catalogs }} }}"
                )).await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get("InferenceBackend"))
        .and_then(serde_json::Value::as_array)
        .ok_or(BackendRegistryError::CatalogRowsMissing)?;
    if rows.len() != 1 {
        return Err(BackendRegistryError::CatalogTargetAmbiguous);
    }
    let row = &rows[0];
    let current = InferenceBackend::from_value(row)?;
    if !(current.endpoint == backend.endpoint
        && current.provider_kind == backend.provider_kind
        && current.auth == backend.auth)
    {
        return Err(BackendRegistryError::ConnectionChanged);
    }
    let mut catalogs = serde_json::from_value::<InferenceBackendObservation>(row.clone())
        .map_err(BackendRegistryError::DecodeCatalogs)?
        .catalogs;
    let matching: Vec<_> = catalogs
        .iter()
        .enumerate()
        .filter(|(_, old)| old.node_did == catalog.node_did)
        .collect();
    if matching.len() > 1 {
        return Err(BackendRegistryError::AmbiguousCatalogEntryScope);
    }
    if let Some((index, old)) = matching.first() {
        if chrono::DateTime::parse_from_rfc3339(&old.observed_at)? > observed_at
            || old.models == catalog.models
        {
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
    let catalogs = serde_json::json!({ "entries": catalogs });
    let doc_id = row
        .get("_docID")
        .and_then(serde_json::Value::as_str)
        .ok_or(BackendRegistryError::CatalogDocIdMissing)?;
    txn.execute_with_variables(
        &format!(
            "mutation($catalogs: JSON) {{ update_InferenceBackend(docID: \"{}\", input: {{ catalogs: $catalogs }}) {{ _docID }} }}",
            escape_graphql_string(doc_id)
        ),
        &serde_json::json!({ "catalogs": catalogs }),
    )
    .await?;
    Ok(())
}

// The credential that authorized discovery has no representation in
// `BackendModelCatalog`.
pub async fn record_discovered_catalog_on(
    access: &crate::config_client::ConfigAccess,
    backend: &InferenceBackend,
    models: Vec<AdvertisedModel>,
) -> Result<BackendModelCatalog> {
    let catalog = BackendModelCatalog {
        node_did: backend.catalog_scope().map(str::to_owned),
        observed_at: chrono::Utc::now().to_rfc3339(),
        models,
    };
    access
        .transact_write("backend_registry.record_operator_catalog", |txn| {
            let catalog = catalog.clone();
            Box::pin(async move { record_model_catalog_in_txn(txn, backend, catalog).await })
        })
        .await?;
    Ok(catalog)
}

pub async fn record_connection_catalog_on(
    access: &crate::config_client::ConfigAccess,
    owner: &str,
    provider_kind: BackendProviderKind,
    endpoint: &str,
    auth: &BackendAuth,
    models: Vec<AdvertisedModel>,
) -> Result<usize> {
    if owner.trim().is_empty() {
        return Err(BackendRegistryError::OwnerRequired);
    }
    let query = format!(
        r#"{{ InferenceBackend(filter: {{ node_did: {{ _eq: "{}" }}, provider_kind: {{ _eq: "{}" }} }}) {{ {BACKEND_CONFIG_FIELDS} }} }}"#,
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
                    .and_then(|data| data.get("InferenceBackend"))
                    .and_then(serde_json::Value::as_array)
                    .ok_or(BackendRegistryError::ConnectionRowsMissing)?;
                let mut recorded = 0;
                for row in rows {
                    let backend = InferenceBackend::from_value(row)?;
                    if backend.endpoint != endpoint || &backend.auth != auth {
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
        if matches!(backend.auth, BackendAuth::NodeOAuth { .. }) {
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
            node_did: None,
            observed_at: now.to_rfc3339(),
            models,
        },
    )
    .await?;
    set_backend_probe_status_with_last_probe(
        node,
        &backend.node_did,
        &backend.backend_id,
        HEALTHY_PROBE_STATUS,
        chrono::Utc::now(),
    )
    .await?;
    lookup_backend_observation(node, &backend.node_did, &backend.backend_id)
        .await?
        .ok_or(BackendRegistryError::DiscoveredObservationMissing)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod read_ownership_tests;
