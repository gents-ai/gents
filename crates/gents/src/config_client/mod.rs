//! Typed per-collection control-plane write client.
//!
//! The client is DID-parameterized: every statement carries the optional
//! `identity::Did` as the DefraDB ACP actor; authorization remains at the node.
//! Embedded paths default to the node DID and signer; HTTP paths authenticate
//! as the [`GraphqlEndpoint`]'s DID with a DefraDB bearer; anonymous HTTP
//! access is read-only because a served home admits writes only from its own DID.
//!
//! Write conventions (load-bearing — see `AGENTS.md`):
//! - every interpolated value goes through
//!   [`crate::graphql::escape_graphql_string`];
//! - list fields never render `[]` (typed as `JsonArray`, corrupts nillable
//!   array columns) — the shared field encoders emit `null` instead;
//! - `Option` fields omitted from an `update:` clause preserve the stored
//!   value; explicit clearing requires `field: null`.

mod agent_behavior;
mod common;
mod desired_state;
pub(crate) mod event_source_cursor;
mod graphql;
mod inference_backend;
mod inference_profile;
mod profile_switch;
#[cfg(test)]
mod query_tests;
mod retry;
mod schema_contract;
mod schema_install;
mod schema_management;
mod txn;
pub(crate) mod write_telemetry;

pub(crate) fn is_transaction_step_unavailable(error: &anyhow::Error) -> bool {
    retry::classified_transaction_conflict(error).is_some()
        || retry::is_transaction_storage_failure(error)
}

#[cfg(test)]
pub(crate) fn is_classified_transaction_conflict(error: &anyhow::Error) -> bool {
    retry::classified_transaction_conflict(error).is_some()
}

#[cfg(test)]
pub(crate) fn classified_conflict_for_test() -> anyhow::Error {
    retry::transaction_conflict(write_telemetry::ConflictSource::StructuredCode)
}

pub mod patch;

pub use agent_behavior::write_agent_document;
pub use common::{mint_recreate_identity, mint_recreate_identity_timestamp};
#[cfg(test)]
pub(crate) use desired_state::canonical_struct_fields;
pub use desired_state::read_record as read_desired_state_record_in_txn;
pub use desired_state::read_records as read_desired_state_records_in_txn;
pub use desired_state::{
    apply_desired_state_plan, config_projection, desired_state_document_digest,
    event_trigger_document_field_names, stale_expectation, validate_event_trigger_document_fields,
    DesiredStateApplyCounts, DesiredStateApplyDocument, DesiredStateApplyPlan,
    DesiredStateExpectation, DriftedDocument, StaleExpectation,
};
pub(crate) use desired_state::{
    collection_is_installed, read_desired_state_document_in_txn, validate_desired_state_plan,
    OutcomeSourceSchemaError,
};
pub(crate) use inference_backend::write_inference_backend_in_txn;
pub use inference_backend::{
    list_inference_backends_in_txn, load_inference_backend_in_txn, serving_accounts,
    write_inference_backend_document,
};
pub use inference_profile::{
    agent_accounts, list_inference_profiles_in_txn, write_inference_profile_document,
};
pub use profile_switch::{
    switch_candidates, switch_profile_account, SwitchCandidate, SwitchPlan, SwitchReceipt,
    SWITCH_COST,
};
pub(crate) use schema_contract::collection_schema_contract_digest;
pub use schema_contract::SchemaFieldDelta;
pub(crate) use schema_install::SchemaInstallMismatch;
pub use schema_install::{
    apply_additive_schema_install, apply_schema_install, preview_additive_schema_install,
    preview_schema_install, SchemaInstallPlan,
};
pub use tools::write_tools_document;
#[cfg(test)]
pub(crate) use txn::ReentrantEmbeddedWrite;
pub(crate) use txn::TransactionOutcome;
pub(crate) use txn::EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT;
pub use txn::{ConfigApplyTxn, IdempotentTransactionRetry, LocalConfigAccess};

mod tools;

use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use futures::future::BoxFuture;
use gents_protocol::graphql::GraphqlRequestOptions;
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphqlEndpoint {
    url: String,
    principal_did: Option<String>,
    delegated_authorization: Option<reqwest::header::HeaderValue>,
}

impl GraphqlEndpoint {
    pub fn anonymous(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            principal_did: None,
            delegated_authorization: None,
        }
    }

    pub fn as_principal(url: impl Into<String>, principal_did: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            principal_did: Some(principal_did.into()),
            delegated_authorization: None,
        }
    }

    /// Forward a caller's DefraDB bearer without minting a runtime node credential.
    pub fn with_delegated_authorization(
        url: impl Into<String>,
        authorization: &str,
    ) -> Result<Self> {
        anyhow::ensure!(
            authorization.starts_with("Bearer ") && authorization.len() > 7,
            "a DefraDB Bearer authorization is required"
        );
        let mut header = reqwest::header::HeaderValue::from_str(authorization)
            .context("invalid authorization header")?;
        header.set_sensitive(true);
        Ok(Self {
            url: url.into(),
            principal_did: None,
            delegated_authorization: Some(header),
        })
    }

    /// **Must end with `/graphql`**: begin/commit/discard derive the REST base by stripping this suffix.
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn is_anonymous(&self) -> bool {
        self.principal_did.is_none() && self.delegated_authorization.is_none()
    }

    pub fn authorization(&self) -> Result<Option<String>> {
        if let Some(value) = &self.delegated_authorization {
            return Ok(Some(value.to_str()?.to_owned()));
        }
        let Some(did) = self.principal_did.as_deref() else {
            return Ok(None);
        };
        let url = reqwest::Url::parse(self.url.trim())
            .with_context(|| format!("parsing GraphQL endpoint {}", self.url))?;
        let host = url
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("GraphQL endpoint {} has no host", self.url))?;
        // The DefraDB token audience is the `Host` header, which carries the port only when it is not the scheme default.
        let audience = match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        };
        crate::identity::defradb_bearer_authorization(did, &audience).map(Some)
    }

    fn authorization_header(&self) -> Result<Option<reqwest::header::HeaderValue>> {
        let Some(authorization) = self.authorization()? else {
            return Ok(None);
        };
        let mut value = reqwest::header::HeaderValue::from_str(&authorization)
            .context("encoding DefraDB bearer header")?;
        value.set_sensitive(true);
        Ok(Some(value))
    }

    pub fn authorize(&self, request: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder> {
        Ok(match self.authorization_header()? {
            Some(value) => request.header(reqwest::header::AUTHORIZATION, value),
            None => request,
        })
    }
}

/// Serialized as its URL: the acting DID is an access decision, not output.
impl serde::Serialize for GraphqlEndpoint {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.url)
    }
}

impl std::fmt::Display for GraphqlEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.url)
    }
}

#[derive(Clone)]
pub enum ConfigAccess {
    Graphql(GraphqlEndpoint),
    Local(LocalConfigAccess),
}

/// Reads use the existing owner; adapters add no retry or authorization decision.
#[async_trait::async_trait]
pub trait ConfigRead: Send + Sync {
    async fn execute_read(&self, document: &str) -> Result<Value>;
}

#[async_trait::async_trait]
impl ConfigRead for ConfigAccess {
    async fn execute_read(&self, document: &str) -> Result<Value> {
        self.execute(document).await
    }
}

#[async_trait::async_trait]
impl ConfigRead for ConfigApplyTxn<'_> {
    async fn execute_read(&self, document: &str) -> Result<Value> {
        graphql::ensure_query_document(document)?;
        self.execute(document).await
    }
}

impl ConfigAccess {
    pub fn graphql(url: impl Into<String>) -> Self {
        Self::Graphql(GraphqlEndpoint::anonymous(url))
    }

    pub fn graphql_as(url: impl Into<String>, principal_did: impl Into<String>) -> Self {
        Self::Graphql(GraphqlEndpoint::as_principal(url, principal_did))
    }

    pub fn local(node: Arc<EmbeddedNode>) -> Self {
        Self::Local(LocalConfigAccess::new(node))
    }

    pub fn claimed_local(node: Arc<EmbeddedNode>, claim: crate::home::StoreLock) -> Result<Self> {
        Ok(Self::Local(LocalConfigAccess::claimed(node, claim)?))
    }

    pub fn local_node(&self) -> Option<&Arc<EmbeddedNode>> {
        match self {
            Self::Local(local) => Some(local.node()),
            Self::Graphql(_) => None,
        }
    }

    pub(crate) async fn run_local_owned<T, F, Fut>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<EmbeddedNode>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T>> + Send + 'static,
    {
        match self {
            Self::Local(local) => local.run_owned(operation).await,
            Self::Graphql(_) => anyhow::bail!("owned native operation requires local access"),
        }
    }

    pub fn mode(&self) -> &'static str {
        match self {
            Self::Graphql(_) => "graphql",
            Self::Local(_) => "local",
        }
    }

    pub fn execute<'a>(&'a self, query: &'a str) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            match self {
                Self::Graphql(graphql) => post_graphql(graphql, query).await,
                Self::Local(local) => {
                    let response =
                        Self::read_local_response_owned(local, query, "config GraphQL").await?;
                    crate::graphql::ensure_no_errors(&response, "config GraphQL")?;
                    Ok(crate::graphql::into_data_envelope(response.data))
                }
            }
        })
    }

    /// Apply schema SDL; callers must perform their own structural compatibility check first.
    pub async fn add_schema(&self, sdl: &str) -> Result<()> {
        match self {
            Self::Graphql(graphql) => {
                let api_base = graphql_api_base(graphql.url())?;
                let url = format!("{api_base}/schema");
                let client = graphql::http_client().await?;
                let response = graphql
                    .authorize(client.post(&url))?
                    .header(reqwest::header::CONTENT_TYPE, "text/plain; charset=utf-8")
                    .body(sdl.to_owned())
                    .send()
                    .await
                    .with_context(|| format!("posting schema SDL to {url}"))?;
                let status = response.status();
                let bytes = response
                    .bytes()
                    .await
                    .with_context(|| format!("reading schema SDL response from {url}"))?;
                if !status.is_success() {
                    anyhow::bail!(
                        "schema SDL request to {url} failed with {status}: {}",
                        String::from_utf8_lossy(&bytes)
                    );
                }
                Ok(())
            }
            Self::Local(local) => {
                let sdl = sdl.to_owned();
                local
                    .run_owned(move |node| async move {
                        node.add_schema(&sdl).await.context("adding schema SDL")
                    })
                    .await
            }
        }
    }

    pub async fn collection_fields(&self, collection: &str) -> Result<Option<BTreeSet<String>>> {
        Ok(self.collection_version(collection).await?.map(|version| {
            version
                .get("Fields")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|field| field.get("Name").and_then(Value::as_str))
                .map(ToOwned::to_owned)
                .collect()
        }))
    }

    pub async fn collection_names(&self) -> Result<BTreeSet<String>> {
        match self {
            Self::Local(local) => Ok(local.node().list_collections()?.into_iter().collect()),
            Self::Graphql(graphql) => {
                let api_base = graphql_api_base(graphql.url())?;
                let url = format!("{api_base}/collections/versions");
                let client = graphql::http_client().await?;
                let versions: Value = graphql
                    .authorize(client.get(&url))?
                    .send()
                    .await
                    .with_context(|| format!("fetching collection versions from {url}"))?
                    .error_for_status()
                    .with_context(|| format!("fetching collection versions from {url}"))?
                    .json()
                    .await
                    .with_context(|| format!("decoding collection versions from {url}"))?;
                Ok(versions
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|version| {
                        version
                            .get("IsActive")
                            .and_then(Value::as_bool)
                            .unwrap_or(true)
                    })
                    .filter_map(|version| version.get("Name").and_then(Value::as_str))
                    .map(ToOwned::to_owned)
                    .collect())
            }
        }
    }

    pub async fn collection_version(&self, collection: &str) -> Result<Option<Value>> {
        crate::graphql::validate_collection_identifier(collection)?;
        match self {
            Self::Local(local) => local
                .node()
                .get_collection(collection)?
                .map(serde_json::to_value)
                .transpose()
                .context("serializing active collection version"),
            Self::Graphql(graphql) => {
                let api_base = graphql_api_base(graphql.url())?;
                let url = format!("{api_base}/collections/versions");
                let client = graphql::http_client().await?;
                let versions: Value = graphql
                    .authorize(client.get(&url))?
                    .send()
                    .await
                    .with_context(|| format!("fetching collection versions from {url}"))?
                    .error_for_status()
                    .with_context(|| format!("fetching collection versions from {url}"))?
                    .json()
                    .await
                    .with_context(|| format!("decoding collection versions from {url}"))?;
                Ok(versions.as_array().and_then(|versions| {
                    versions
                        .iter()
                        .find(|version| {
                            version.get("Name").and_then(Value::as_str) == Some(collection)
                                && version
                                    .get("IsActive")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(true)
                        })
                        .cloned()
                }))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExistingDocumentRef {
    pub doc_id: String,
    pub deleted: bool,
}

pub fn graphql_api_base(graphql: &str) -> Result<String> {
    graphql
        .trim()
        .strip_suffix("/graphql")
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            anyhow::anyhow!("expected GraphQL endpoint ending in /graphql, got {graphql}")
        })
}

fn is_probably_local_graphql_endpoint(graphql: &str) -> bool {
    let graphql = graphql.trim();
    graphql.contains("127.0.0.1") || graphql.contains("localhost")
}

/// Operator guidance appended to HTTP GraphQL errors; CLI-flavored and stripped by non-CLI consumers.
pub fn graphql_diagnostic_hint(graphql: &str) -> String {
    if is_probably_local_graphql_endpoint(graphql) {
        "Next:\n  1. If this home is not initialized, run `gents init`\n  2. Start the runtime with `gents server`\n  3. Inspect it with `gents status`".to_string()
    } else {
        format!(
            "Next:\n  1. Verify the GraphQL endpoint {graphql}\n  2. Retry with `--graphql {graphql}` or point the command at the correct runtime"
        )
    }
}

pub async fn post_graphql(graphql: &GraphqlEndpoint, query: &str) -> Result<Value> {
    query_graphql_with_options(
        graphql,
        query,
        GraphqlRequestOptions {
            timeout: std::time::Duration::from_secs(30),
            max_attempts: 5,
            retry_backoff: std::time::Duration::from_millis(100),
        },
    )
    .await
}

/// Execute one HTTP GraphQL read; mutations are rejected before network I/O.
pub async fn query_graphql_with_options(
    graphql: &GraphqlEndpoint,
    query: &str,
    options: GraphqlRequestOptions,
) -> Result<Value> {
    graphql::query_with_options(graphql, query, options)
        .await
        .map_err(|error| {
            let diagnostic = format!("{error}\n{}", graphql_diagnostic_hint(graphql.url()));
            error.context(diagnostic)
        })
}
