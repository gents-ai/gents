//! Typed per-collection control-plane write client.
//!
//! The client is DID-parameterized: every statement carries the optional
//! `identity::Did` as the DefraDB ACP actor; authorization remains at the node.
//! Embedded paths default to the node DID and signer; HTTP paths authenticate
//! as the [`GraphqlEndpoint`]'s DID with a DefraDB bearer; anonymous HTTP
//! access is subject to DefraDB's native authorization.
//!
//! Write conventions (load-bearing — see `AGENTS.md`):
//! - every interpolated value goes through
//!   [`crate::store::graphql::escape_graphql_string`];
//! - list fields never render `[]` (typed as `JsonArray`, corrupts nillable
//!   array columns) — the shared field encoders emit `null` instead;
//! - `Option` fields omitted from an `update:` clause preserve the stored
//!   value; explicit clearing requires `field: null`.

mod agent_behavior;
#[cfg(test)]
mod agent_signer_conformance;
mod application_failure;
mod client_agent_publication;
mod client_signed_agent;
pub use client_signed_agent::ClientSigningCapability;
pub(crate) use client_signed_agent::{
    prepare_goal_delete_evidence, prepare_request_evidence, verify_goal_delete_evidence,
    verify_request_evidence,
};
#[cfg(test)]
mod client_signed_agent_conformance;
#[cfg(test)]
mod event_policy_conformance;
mod common;
mod desired_state;
#[cfg(test)]
mod dormant_file_request_conformance;
mod error;
pub(crate) mod event_source_cursor;
mod graphql;
mod inference_backend;
mod inference_profile;
mod node_deferred_operation;
mod node_document_publication;
mod node_permissions;
mod pairing_reader;
mod profile_switch;
#[cfg(test)]
mod query_tests;
mod relationships;
mod retry;
mod schema_contract;
mod schema_install;
mod schema_management;
mod self_config_permission;
mod subscription;
pub(crate) mod transport_publication;
mod txn;
pub use acp::nac::NodePermission;
pub use subscription::GraphqlSubscription;
pub use txn::TRACING_TARGET as TXN_TRACING_TARGET;
pub(crate) mod write_telemetry;

pub(crate) fn is_transaction_step_unavailable<E>(error: &E) -> bool
where
    E: TransactionFailure + ?Sized,
{
    retry::classified_transaction_conflict(error).is_some()
        || retry::is_transaction_storage_failure(error)
}

#[cfg(test)]
pub(crate) fn is_classified_transaction_conflict<E>(error: &E) -> bool
where
    E: TransactionFailure + ?Sized,
{
    retry::classified_transaction_conflict(error).is_some()
}

#[cfg(test)]
pub(crate) fn classified_conflict_for_test() -> ConfigClientError {
    retry::transaction_conflict(write_telemetry::ConflictSource::StructuredCode)
}

pub mod patch;

pub use agent_behavior::write_agent_document;
pub use client_agent_publication::{
    AgentPublicationObserver, ClientAgentJournal, PublicationStage, PublicationStatus,
};
pub use common::{mint_recreate_identity, mint_recreate_identity_timestamp};
pub use desired_state::apply_desired_state_plan;
pub use desired_state::grant_selected_self_configuration;
pub use desired_state::plan::{
    stale_expectation, DesiredStateApplyCounts, DesiredStateApplyDocument, DesiredStateApplyPlan,
    DesiredStateExpectation, DriftedDocument, StaleExpectation,
};
#[cfg(test)]
pub(crate) use desired_state::projection::canonical_struct_fields;
pub use desired_state::projection::{config_projection, desired_state_document_digest};
pub(crate) use desired_state::read::read_desired_state_document_in_txn;
pub use desired_state::read::read_record as read_desired_state_record_in_txn;
pub use desired_state::read::read_records as read_desired_state_records_in_txn;
pub(crate) use desired_state::schema_fields::collection_is_installed;
pub use desired_state::validate::{
    event_trigger_document_field_names, validate_event_trigger_document_fields,
};
pub(crate) use desired_state::validate::{validate_desired_state_plan, OutcomeSourceSchemaError};
pub(crate) use desired_state::{
    apply_desired_state_plan_with_node_binding, configuration_node_doc, ConfigurationNodeBinding,
};
pub(crate) use inference_backend::{backend_metadata_fields, write_inference_backend_in_txn};
pub use inference_backend::{
    list_inference_backends_in_txn, load_inference_backend_in_txn, serving_accounts,
    write_inference_backend_document,
};
pub use inference_profile::{
    agent_accounts, list_inference_profiles_in_txn, write_inference_profile_document,
};
pub(crate) use node_deferred_operation::recover_deferred_node_operations;
pub use node_deferred_operation::{maybe_link_deferred_operation_in_txn, DeferredOperationError};
pub(crate) use node_document_publication::{
    publish_node_child_in_txn, publish_secret_configuration_in_txn,
};
pub use pairing_reader::{PairingObservation, PairingReadReason};
pub use profile_switch::{
    switch_candidates, switch_profile_account, SwitchCandidate, SwitchPlan, SwitchReceipt,
    SWITCH_COST,
};
pub(crate) use schema_contract::collection_schema_contract_digest;
pub use schema_contract::{SchemaContractError, SchemaFieldDelta};
pub use schema_install::{
    apply_additive_schema_install, apply_schema_install, preview_additive_schema_install,
    preview_schema_install, schema_install_mismatch, SchemaInstallError, SchemaInstallMismatch,
    SchemaInstallPlan,
};
pub use tools::write_tools_document;
pub(crate) use txn::EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT;
pub use txn::{
    ConfigApplyTxn, ConfigCursor, IdempotentTransactionRetry, LocalConfigAccess,
    NativeOperationAbandoned, ReceiptWriteError, ReentrantEmbeddedWrite,
};

pub use error::{ConfigClientError, TransactionConflict};
pub use relationships::ConfigRelationshipGrant;
pub use retry::TransactionFailure;
pub use self_config_permission::{AgentSelfConfigPermission, AgentSelfConfigPermissionError};
pub use write_telemetry::WriteTelemetryError;

mod tools;

use std::collections::BTreeSet;
use std::sync::Arc;

use defra_node::Node;
use futures::future::BoxFuture;
use gents_protocol::graphql::GraphqlRequestOptions;
use serde_json::Value;

type Result<T, E = ConfigClientError> = std::result::Result<T, E>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphqlEndpoint {
    url: String,
    principal_did: Option<String>,
    delegated_authorization: Option<reqwest::header::HeaderValue>,
    client_signed_agent_creates: bool,
    client_signing_capability: Option<ClientSigningCapability>,
}

impl GraphqlEndpoint {
    pub fn anonymous(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            principal_did: None,
            delegated_authorization: None,
            client_signed_agent_creates: false,
            client_signing_capability: None,
        }
    }

    pub fn as_principal(url: impl Into<String>, principal_did: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            principal_did: Some(principal_did.into()),
            delegated_authorization: None,
            client_signed_agent_creates: false,
            client_signing_capability: None,
        }
    }

    /// Forward a caller's DefraDB bearer without minting a runtime node credential.
    pub fn with_delegated_authorization(
        url: impl Into<String>,
        authorization: &str,
    ) -> Result<Self, ConfigClientError> {
        if !(authorization.starts_with("Bearer ") && authorization.len() > 7) {
            return Err(ConfigClientError::BearerRequired);
        }
        let mut header = reqwest::header::HeaderValue::from_str(authorization)
            .map_err(ConfigClientError::AuthorizationHeader)?;
        header.set_sensitive(true);
        Ok(Self {
            url: url.into(),
            principal_did: None,
            delegated_authorization: Some(header),
            client_signed_agent_creates: false,
            client_signing_capability: None,
        })
    }

    /// Preserve an HTTP caller's verified identity and original bearer. The caller
    /// must obtain `actor` from DefraDB's authenticated request extractor; this method
    /// neither verifies credentials nor mints a replacement signature.
    pub fn with_authenticated_delegation(
        url: impl Into<String>,
        authorization: &str,
        actor: identity::Did,
    ) -> Result<Self, ConfigClientError> {
        let mut endpoint = Self::with_delegated_authorization(url, authorization)?;
        endpoint.principal_did = Some(actor.to_string());
        Ok(endpoint)
    }

    /// **Must end with `/graphql`**: begin/commit/discard derive the REST base by stripping this suffix.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Require the client principal to sign Agent, AgentContext and Tools
    /// genesis blocks and supported configuration updates locally.
    pub fn with_client_signed_agent_creates(mut self) -> Self {
        self.client_signed_agent_creates = true;
        self
    }

    /// Bind an explicitly held client key to this endpoint's authenticated actor.
    pub fn with_client_signing_capability(
        mut self,
        capability: ClientSigningCapability,
    ) -> Result<Self> {
        if self.delegated_authorization.is_some()
            || self.principal_did.as_deref() != Some(capability.did())
        {
            return Err(ConfigClientError::ClientSignerMismatch);
        }
        self.client_signing_capability = Some(capability);
        Ok(self)
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
        let url = reqwest::Url::parse(self.url.trim()).map_err(|source| {
            ConfigClientError::ParseEndpoint {
                url: self.url.clone(),
                source,
            }
        })?;
        let host = url
            .host_str()
            .ok_or_else(|| ConfigClientError::EndpointHost {
                url: self.url.clone(),
            })?;
        // The DefraDB token audience is the `Host` header, which carries the port only when it is not the scheme default.
        let audience = match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        };
        let authorization = if let Some(capability) = &self.client_signing_capability {
            crate::store::identity::defradb_bearer_authorization_with_config(
                did,
                &audience,
                &capability.config,
            )?
        } else {
            return Err(ConfigClientError::ClientAgentSignerRequired);
        };
        Ok(Some(authorization))
    }

    fn authorization_header(&self) -> Result<Option<reqwest::header::HeaderValue>> {
        let Some(authorization) = self.authorization()? else {
            return Ok(None);
        };
        let mut value = reqwest::header::HeaderValue::from_str(&authorization)
            .map_err(ConfigClientError::BearerHeader)?;
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

    pub fn local(node: Arc<Node>) -> Self {
        Self::Local(LocalConfigAccess::new(node))
    }

    /// Bind document reads and writes to this actor across retries and transactions.
    /// A denied operation retains this identity; it never retries as the node.
    pub fn local_as(node: Arc<Node>, identity: identity::Did) -> Self {
        Self::Local(LocalConfigAccess::new(node).with_identity(identity))
    }

    pub fn local_managed_agent(
        node: Arc<Node>,
        actor: identity::Did,
        identity: &dyn crate::store::identity::NodeIdentity,
    ) -> Result<Self> {
        let signer = identity
            .managed_agent_signer()
            .ok_or(ConfigClientError::ManagedAgentSignerRequired)?;
        if actor.as_str() != identity.did() {
            return Err(ConfigClientError::TransactionActorMismatch);
        }
        Self::local_with_agent_signer(node, actor, signer)
    }

    pub(crate) fn local_with_agent_signer(
        node: Arc<Node>,
        actor: identity::Did,
        signer: crate::store::identity::ManagedAgentSigner,
    ) -> Result<Self> {
        if actor.as_str() != signer.did() || node.node_identity_did() != Some(signer.node_did()) {
            return Err(ConfigClientError::TransactionActorMismatch);
        }
        let local = LocalConfigAccess::new(node)
            .with_identity(actor)
            .with_agent_signer(signer);
        Ok(Self::Local(local))
    }

    pub(crate) fn local_with_optional_agent_signer(
        node: Arc<Node>,
        actor: identity::Did,
        signer: Option<crate::store::identity::ManagedAgentSigner>,
    ) -> Result<Self> {
        match signer {
            Some(signer) => Self::local_with_agent_signer(node, actor, signer),
            None => Ok(Self::local_as(node, actor)),
        }
    }

    #[cfg(test)]
    fn has_managed_agent_signer(&self) -> bool {
        matches!(self, Self::Local(local) if local.has_agent_signer())
    }

    #[cfg(test)]
    fn check_local_mutation_signer(&self) -> Result<()> {
        match self {
            Self::Local(local) => local.require_mutation_signer(),
            Self::Graphql(_) => Err(ConfigClientError::EmbeddedAccessRequired),
        }
    }

    /// Returns the explicitly selected actor, without interpreting delegated
    /// bearer contents or substituting the runtime identity.
    pub fn actor_did(&self) -> Option<String> {
        match self {
            Self::Local(local) => local.identity().map(ToString::to_string),
            Self::Graphql(endpoint) => endpoint.principal_did.clone(),
        }
    }

    /// Preserve client signing intent when the selected transport is embedded.
    pub fn with_client_signed_configuration(mut self) -> Self {
        match &mut self {
            Self::Local(local) => local.require_client_signed_writes(),
            Self::Graphql(endpoint) => endpoint.client_signed_agent_creates = true,
        }
        self
    }

    pub(crate) fn managed_agent_signer(
        &self,
    ) -> Option<crate::store::identity::ManagedAgentSigner> {
        match self {
            Self::Local(local) => local.agent_signer(),
            Self::Graphql(_) => None,
        }
    }

    pub fn claimed_local(
        node: Arc<Node>,
        claim: crate::store::local::home::StoreLock,
    ) -> Result<Self> {
        Ok(Self::Local(LocalConfigAccess::claimed(node, claim)?))
    }

    pub fn claimed_local_as(
        node: Arc<Node>,
        claim: crate::store::local::home::StoreLock,
        identity: identity::Did,
    ) -> Result<Self> {
        Ok(Self::Local(
            LocalConfigAccess::claimed(node, claim)?.with_identity(identity),
        ))
    }

    pub fn local_node(&self) -> Option<&Arc<Node>> {
        match self {
            Self::Local(local) => Some(local.node()),
            Self::Graphql(_) => None,
        }
    }

    pub(crate) async fn run_local_owned<T, E, F, Fut>(&self, operation: F) -> Result<T, E>
    where
        T: Send + 'static,
        E: From<ConfigClientError> + Send + 'static,
        F: FnOnce(Arc<Node>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'static,
    {
        match self {
            Self::Local(local) => local.run_owned(operation).await,
            Self::Graphql(_) => Err(ConfigClientError::LocalAccessRequired.into()),
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
                    crate::store::graphql::ensure_no_errors(&response, "config GraphQL")?;
                    Ok(crate::store::graphql::into_data_envelope(response.data))
                }
            }
        })
    }

    /// Apply schema SDL; callers must perform their own structural compatibility check first.
    pub async fn add_collection(&self, sdl: &str) -> Result<()> {
        match self {
            Self::Graphql(graphql) => {
                let api_base = graphql_api_base(graphql.url())?;
                let url = format!("{api_base}/collections");
                let client = graphql::http_client().await?;
                let response = graphql
                    .authorize(client.post(&url))?
                    .header(reqwest::header::CONTENT_TYPE, "text/plain; charset=utf-8")
                    .body(sdl.to_owned())
                    .send()
                    .await
                    .map_err(|source| ConfigClientError::PostSchema {
                        url: url.clone(),
                        source,
                    })?;
                let status = response.status();
                let bytes = response.bytes().await.map_err(|source| {
                    ConfigClientError::ReadSchemaResponse {
                        url: url.clone(),
                        source,
                    }
                })?;
                if !status.is_success() {
                    return Err(ConfigClientError::SchemaStatus {
                        url,
                        status,
                        body: String::from_utf8_lossy(&bytes).into_owned(),
                    });
                }
                Ok(())
            }
            Self::Local(local) => {
                let sdl = sdl.to_owned();
                let actor = local.identity().cloned();
                local
                    .run_owned(move |node| async move {
                        match actor {
                            Some(actor) => {
                                let context = defra_core::RequestContext::default()
                                    .with_identity(Some(actor.to_string()));
                                node.add_collection_as(&context, &sdl).await.map(|_| ())
                            }
                            None => node.add_collection(&sdl).await,
                        }
                        .map_err(|error| ConfigClientError::AddSchema(Box::new(error)))
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
            Self::Local(local) if local.identity().is_some() => Ok(self
                .collection_versions()
                .await?
                .into_iter()
                .filter(|version| version["IsActive"].as_bool().unwrap_or(true))
                .filter_map(|version| version["Name"].as_str().map(str::to_owned))
                .collect()),
            Self::Local(local) => Ok(local.node().list_collections()?.into_iter().collect()),
            Self::Graphql(graphql) => {
                let api_base = graphql_api_base(graphql.url())?;
                let url = format!("{api_base}/collections?get_inactive=true");
                let client = graphql::http_client().await?;
                let versions: Value = graphql
                    .authorize(client.get(&url))?
                    .send()
                    .await
                    .map_err(|source| ConfigClientError::FetchCollectionVersions {
                        url: url.clone(),
                        source,
                    })?
                    .error_for_status()
                    .map_err(|source| ConfigClientError::FetchCollectionVersions {
                        url: url.clone(),
                        source,
                    })?
                    .json()
                    .await
                    .map_err(|source| ConfigClientError::DecodeCollectionVersions {
                        url: url.clone(),
                        source,
                    })?;
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
        crate::store::graphql::validate_collection_identifier(collection)?;
        match self {
            Self::Local(local) if local.identity().is_some() => {
                let collection = collection.to_owned();
                let actor = local.identity().cloned();
                local
                    .run_owned(move |node| async move {
                        let context = defra_core::RequestContext::default()
                            .with_identity(actor.map(|did| did.to_string()));
                        let selector = schema::CollectionSelector::with_names(vec![collection]);
                        node.get_collections_as(&context, &selector)
                            .await?
                            .into_iter()
                            .next()
                            .map(serde_json::to_value)
                            .transpose()
                            .map_err(ConfigClientError::SerializeCollectionVersion)
                    })
                    .await
            }
            Self::Local(local) => local
                .node()
                .get_collection(collection)?
                .map(serde_json::to_value)
                .transpose()
                .map_err(ConfigClientError::SerializeCollectionVersion),
            Self::Graphql(graphql) => {
                let api_base = graphql_api_base(graphql.url())?;
                let url = format!("{api_base}/collections?get_inactive=true");
                let client = graphql::http_client().await?;
                let versions: Value = graphql
                    .authorize(client.get(&url))?
                    .send()
                    .await
                    .map_err(|source| ConfigClientError::FetchCollectionVersions {
                        url: url.clone(),
                        source,
                    })?
                    .error_for_status()
                    .map_err(|source| ConfigClientError::FetchCollectionVersions {
                        url: url.clone(),
                        source,
                    })?
                    .json()
                    .await
                    .map_err(|source| ConfigClientError::DecodeCollectionVersions {
                        url: url.clone(),
                        source,
                    })?;
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

pub fn graphql_api_base(graphql: &str) -> Result<String, ConfigClientError> {
    graphql
        .trim()
        .strip_suffix("/graphql")
        .map(ToOwned::to_owned)
        .ok_or_else(|| ConfigClientError::GraphqlApiBase {
            graphql: graphql.to_owned(),
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
        .map_err(|error| ConfigClientError::HttpQuery {
            endpoint: graphql.url().to_owned(),
            source: Box::new(error),
        })
}
