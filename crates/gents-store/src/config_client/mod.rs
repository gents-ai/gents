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
//!   [`crate::graphql::escape_graphql_string`];
//! - list fields never render `[]` (typed as `JsonArray`, corrupts nillable
//!   array columns) — the shared field encoders emit `null` instead;
//! - `Option` fields omitted from an `update:` clause preserve the stored
//!   value; explicit clearing requires `field: null`.

#[cfg(any(test, feature = "test-fixtures"))]
pub mod access_classification_fixture;
#[cfg(test)]
mod agent_signer_conformance;
mod application_failure;
mod application_pairing;
pub mod assignment_receiver;
pub mod assignments;
pub mod authored_document;
mod client_agent_publication;
pub mod client_authentication;
mod client_pack_evidence;
pub mod client_request_input;
pub mod client_signed_agent;
mod device_group;
pub(crate) use device_group::verified_publisher_node;
pub(crate) mod transport_replica;
pub use device_group::DeviceGroupReadError;
pub mod runtime_access;
pub mod runtime_endpoints;
pub use client_pack_evidence::{
    prepare_pack_evidence, prepare_pack_evidence_from_history, prepare_pack_evidence_in_txn,
    prepare_pack_relationship, prepare_pack_schema_approval, verify_pack_schema_approval,
    PackApprovalScope, PackEvidence, PackEvidenceError, PackMutation, PackRelationship,
    PackRelationshipEvidence, PackRelationshipTarget, PackSchemaEvidence,
};
mod pack_difference;
pub use client_request_input::{client_request_input, ClientRequestInputError};
pub use client_signed_agent::prepare_request_evidence;
pub use client_signed_agent::ClientSigningCapability;
pub use client_signed_agent::{
    configuration_replacement_patch, prepare_goal_delete_evidence, verify_goal_delete_evidence,
    verify_request_evidence,
};
pub use pack_difference::{
    PackMutationDifference, PackMutationKind, ValueShape, MAX_REPORTED_DIFFERENCES,
};
pub mod agent_session;
#[cfg(test)]
mod client_signed_agent_conformance;
mod common;
pub mod decision_read;
mod delegation;
#[cfg(test)]
mod delegation_conformance;
mod destructive_decision;
pub mod document_decide;
#[cfg(test)]
mod dormant_file_request_conformance;
mod error;
#[cfg(test)]
mod event_policy_conformance;
mod graphql;
mod held_audience;
mod home_provider;
pub mod home_status;
pub mod node_deferred_operation;
mod node_document_publication;
mod node_permissions;
pub mod node_readiness;
pub mod owed;
#[cfg(test)]
mod owed_conformance;
mod owed_delivery;
mod owner_staged_route;
mod pack_effect;
mod pairing_reader;
#[cfg(test)]
mod query_tests;
mod relationships;
mod retry;
mod schema_contract;
mod schema_install;
pub mod schema_introspection;
mod schema_management;
mod self_config_permission;
pub mod session_children;
mod stored_create;
mod subscription;
pub mod transport_publication;
pub(crate) mod transport_sentinel;
mod txn;
pub mod user_write;
#[cfg(test)]
mod user_write_conformance;
pub use acp::nac::NodePermission;
pub use pack_effect::{PackApprovalError, PackEffectApprover, PackReplayTransport};
pub use subscription::{GraphqlSubscription, SubscriptionEnd, SUBSCRIPTION_RENEWAL_PERIOD};
pub use txn::TRACING_TARGET as TXN_TRACING_TARGET;
pub use txn::{VerifiedPackTransactionError, VerifiedPackTxn};
pub mod write_telemetry;

pub fn is_transaction_step_unavailable<E>(error: &E) -> bool
where
    E: TransactionFailure + ?Sized,
{
    retry::classified_transaction_conflict(error).is_some()
        || retry::is_transaction_storage_failure(error)
}

/// Whether `error` is a typed DefraDB transaction conflict, after every
/// attempt the transaction owner allowed.
pub fn is_classified_transaction_conflict<E>(error: &E) -> bool
where
    E: TransactionFailure + ?Sized,
{
    retry::classified_transaction_conflict(error).is_some()
}

/// For a typed DefraDB transaction conflict that outlasted the transaction
/// owner's attempts, whether a client may repeat the failed operation (Lean
/// `TransactionReplay.clientMayRepeat`); `None` when `error` is no conflict.
pub fn classified_conflict_repeatable<E>(error: &E) -> Option<bool>
where
    E: TransactionFailure + ?Sized,
{
    error.config_client_error()?.conflict_repeatable()
}

#[cfg(any(test, feature = "test-fixtures"))]
#[doc(hidden)]
pub fn classified_conflict_for_test() -> ConfigClientError {
    retry::transaction_conflict(write_telemetry::ConflictSource::StructuredCode)
}

#[cfg(any(test, feature = "test-fixtures"))]
#[doc(hidden)]
pub fn unconfirmed_commit_conflict_for_test() -> ConfigClientError {
    let mut error = classified_conflict_for_test();
    error.mark_unconfirmed_commit();
    error
}

pub use authored_document::{
    create_authored_document, AuthoredDocumentCreateError, DocumentSchemaSource,
};
pub use client_agent_publication::{
    recovery_disposition, AgentPublicationObserver, ClientAgentJournal, PublicationStage,
    PublicationStatus, RecoveryDisposition,
};
pub use common::{mint_recreate_identity, mint_recreate_identity_timestamp};
pub use node_deferred_operation::recover_deferred_node_operations;
pub use node_deferred_operation::{
    maybe_link_deferred_operation_in_txn, require_node_update_in_txn, DeferredOperationError,
};
pub use node_document_publication::publish_node_managers_in_txn;
pub use owner_staged_route::{
    consume_signed_command_nonce, OwnerStagedRoute, SignedCommandNonceError,
    NONCE_RETENTION_SECONDS,
};
pub use pairing_reader::{PairingObservation, PairingReadReason};
pub use schema_contract::collection_schema_contract_digest;
pub use schema_contract::{SchemaContractError, SchemaFieldDelta};
pub use schema_install::{
    apply_client_schema_install, apply_schema_install, preview_client_schema_install,
    preview_schema_install, schema_install_decision, schema_install_mismatch,
    schema_install_refusal, schema_request_admitted, SchemaInstallError, SchemaInstallMismatch,
    SchemaInstallPlan,
};
#[cfg(any(test, feature = "test-fixtures"))]
pub use txn::NativeOperationObservation;
pub(crate) use txn::ReadAccess;
pub use txn::EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT;
#[cfg(any(test, feature = "test-fixtures"))]
pub use txn::{count_statements, StatementCounts};
pub use txn::{
    CallbackAgentSelection, ConfigApplyTxn, ConfigCursor, IdempotentTransactionRetry,
    LocalConfigAccess, NativeOperationAbandoned, ReceiptWriteError, ReentrantEmbeddedWrite,
    TransactionFinalityError,
};
#[cfg(any(test, feature = "test-fixtures"))]
pub use txn::{RelationshipChange, RelationshipChangeKind, RelationshipMatch};

#[cfg(any(test, feature = "test-fixtures"))]
#[doc(hidden)]
pub use delegation::mint_with_lifetime_for_test;
pub use delegation::{
    DelegationError, RequestBoundAudience, VERA_DELEGATION_LIFETIME, VERA_REQUEST_BOUND_LIFETIME,
};
pub use destructive_decision::{DestructiveAuthority, DestructiveRefusal, InventoryRead};
pub use error::{ConfigClientError, RerunNote, RerunRequest, TransactionConflict};
pub use held_audience::ResolveAudience;
pub use relationships::ConfigRelationshipGrant;
pub(crate) use retry::access_owner;
pub use retry::TransactionFailure;
pub use self_config_permission::{AgentSelfConfigPermission, AgentSelfConfigPermissionError};
pub use stored_create::StoredCreateCommands;
pub use write_telemetry::WriteTelemetryError;

use std::collections::BTreeSet;
use std::sync::Arc;

use defra_node::Node;
use futures::future::BoxFuture;
use gents_protocol::graphql::GraphqlRequestOptions;
use serde_json::Value;

type Result<T, E = ConfigClientError> = std::result::Result<T, E>;

pub(crate) enum WriteRoute<'a> {
    Http(&'a GraphqlEndpoint),
    Local(&'a LocalConfigAccess),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphqlEndpoint {
    url: String,
    principal_did: Option<String>,
    client_authentication: Arc<runtime_endpoints::EndpointAuthentication>,
    delegated_authorization: Option<reqwest::header::HeaderValue>,
    client_signed_agent_creates: bool,
    client_signing_capability: Option<ClientSigningCapability>,
    delegation_audience: held_audience::HeldAudienceCell,
    pack_replay: Option<pack_effect::PackReplayHandle>,
    extensions: crate::node_extension::Extensions,
    /// What this endpoint's answers said of gzip request bodies, shared by
    /// the endpoint's clones (Lean `RequestEncoding`).
    request_encoding: RequestEncodingCell,
}

/// What an endpoint learned from its answers. It is not part of which
/// endpoint this is, so two endpoints compare equal whatever they learned.
#[derive(Clone, Debug, Default)]
struct RequestEncodingCell(Arc<gents_protocol::graphql::RequestEncoding>);

impl PartialEq for RequestEncodingCell {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for RequestEncodingCell {}

impl GraphqlEndpoint {
    pub fn anonymous(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            principal_did: None,
            client_authentication: Arc::new(runtime_endpoints::EndpointAuthentication::Fixed(
                gents_protocol::client_authentication::ClientAuthentication::Local,
            )),
            delegated_authorization: None,
            client_signed_agent_creates: false,
            client_signing_capability: None,
            delegation_audience: held_audience::HeldAudienceCell::default(),
            pack_replay: None,
            extensions: crate::node_extension::Extensions::default(),
            request_encoding: RequestEncodingCell::default(),
        }
    }

    pub fn principal_did(&self) -> Option<&str> {
        self.principal_did.as_deref()
    }

    pub fn as_principal(url: impl Into<String>, principal_did: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            principal_did: Some(principal_did.into()),
            client_authentication: Arc::new(runtime_endpoints::EndpointAuthentication::Fixed(
                gents_protocol::client_authentication::ClientAuthentication::Local,
            )),
            delegated_authorization: None,
            client_signed_agent_creates: false,
            client_signing_capability: None,
            delegation_audience: held_audience::HeldAudienceCell::default(),
            pack_replay: None,
            extensions: crate::node_extension::Extensions::default(),
            request_encoding: RequestEncodingCell::default(),
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
            client_authentication: Arc::new(runtime_endpoints::EndpointAuthentication::Fixed(
                gents_protocol::client_authentication::ClientAuthentication::Local,
            )),
            delegated_authorization: Some(header),
            client_signed_agent_creates: false,
            client_signing_capability: None,
            delegation_audience: held_audience::HeldAudienceCell::default(),
            pack_replay: None,
            extensions: crate::node_extension::Extensions::default(),
            request_encoding: RequestEncodingCell::default(),
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

    /// Return only the explicitly held key bound to this client actor.
    pub fn held_client_signing_capability(&self) -> Result<&ClientSigningCapability> {
        let capability = self
            .client_signing_capability
            .as_ref()
            .ok_or(ConfigClientError::ClientAgentSignerRequired)?;
        if self.delegated_authorization.is_some()
            || self.principal_did.as_deref() != Some(capability.did())
        {
            return Err(ConfigClientError::ClientSignerMismatch);
        }
        Ok(capability)
    }

    /// Hold the delegation audience the endpoint's node reported in its
    /// status, so that a held key mints a Vera delegation for it; a node that
    /// reports none keeps the identity token
    /// (`Identity.UserDelegation.resolve`). An endpoint is resolved when it
    /// is built and again after its node refuses its credential.
    pub fn with_reported_delegation_audience(
        mut self,
        reported: gents_protocol::delegation_audience::ReportedDelegationAudience,
    ) -> Result<Self> {
        use gents_protocol::delegation_audience::ReportedDelegationAudience as Reported;
        self.delegation_audience = held_audience::HeldAudienceCell::new(match reported {
            Reported::Absent => None,
            Reported::Named(audience) => Some(audience),
            Reported::Unavailable => return Err(DelegationError::AudienceUnavailable.into()),
        });
        Ok(self)
    }

    /// Bind the existing client actor to one runtime-hosted pack interpreter.
    pub fn with_pack_replay(mut self, replay: Arc<dyn PackReplayTransport>) -> Result<Self> {
        self.held_client_signing_capability()?;
        self.pack_replay = Some(pack_effect::PackReplayHandle(replay));
        self.client_signed_agent_creates = true;
        Ok(self)
    }

    pub fn with_extension<T>(
        mut self,
        value: Arc<T>,
    ) -> std::result::Result<Self, crate::node_extension::ExtensionError>
    where
        T: std::any::Any + Send + Sync,
    {
        self.extensions.attach(value)?;
        Ok(self)
    }

    pub fn with_extensions(
        mut self,
        extensions: &crate::node_extension::Extensions,
    ) -> std::result::Result<Self, crate::node_extension::ExtensionError> {
        self.extensions.attach_all(extensions)?;
        Ok(self)
    }

    pub fn extension<T>(&self) -> Option<Arc<T>>
    where
        T: std::any::Any + Send + Sync,
    {
        self.extensions.get::<T>()
    }

    pub(crate) fn replays_pack(&self) -> bool {
        self.pack_replay.is_some()
    }

    pub fn is_anonymous(&self) -> bool {
        self.principal_did.is_none() && self.delegated_authorization.is_none()
    }

    /// The endpoint authority a request to it carries in `Host`: DefraDB's
    /// token audience, with the port only when it is not the scheme default.
    pub(crate) fn audience(&self) -> Result<String> {
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
        Ok(match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        })
    }

    /// Whether the runtime behind this endpoint decides document access
    /// through an external provider, as its status reported.
    pub fn reports_external_provider(&self) -> bool {
        self.delegation_audience.get().is_some()
    }

    /// Whether `authorization` mints a Vera delegation, which lapses within
    /// minutes, rather than forwarding a bearer or minting an identity token.
    pub fn mints_delegations(&self) -> bool {
        self.delegated_authorization.is_none()
            && self.client_signing_capability.is_some()
            && self.delegation_audience.get().is_some()
    }

    pub fn authorization(&self) -> Result<Option<String>> {
        if let Some(value) = &self.delegated_authorization {
            return Ok(Some(value.to_str()?.to_owned()));
        }
        let Some(did) = self.principal_did.as_deref() else {
            return Ok(None);
        };
        let Some(capability) = &self.client_signing_capability else {
            return Err(ConfigClientError::ClientAgentSignerRequired);
        };
        let record = self.delegation_audience.record();
        let authorization = match &record.audience {
            Some(node) => format!(
                "Bearer {}",
                delegation::mint(
                    capability,
                    node,
                    record.clock.as_ref(),
                    self.delegation_audience.device_now()
                )?
            ),
            None => crate::identity::defradb_bearer_authorization_with_config(
                did,
                &self.audience()?,
                &capability.config,
            )?,
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

    /// Authorize a request inside the transaction `id`. Its transaction
    /// recovers a refusal of its credential as a unit
    /// (`Identity.HeldAudience.resendUnit`), never the request alone.
    pub(crate) fn authorize_in_transaction(
        &self,
        request: reqwest::RequestBuilder,
        id: &str,
    ) -> Result<reqwest::RequestBuilder> {
        Ok(self.authorize(request)?.header("x-defradb-tx", id))
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
    Local(Box<LocalConfigAccess>),
}

/// Reads use the existing owner; adapters add no retry or authorization decision.
#[async_trait::async_trait]
pub trait ConfigRead: Send + Sync {
    async fn execute_read(&self, document: &str) -> Result<Value>;

    fn reader_did(&self) -> Option<String> {
        None
    }

    async fn execute_read_with_variables(
        &self,
        _document: &str,
        _variables: &Value,
    ) -> Result<Value> {
        Err(ConfigClientError::ReadVariablesUnsupported)
    }
}

#[async_trait::async_trait]
impl ConfigRead for ConfigAccess {
    async fn execute_read(&self, document: &str) -> Result<Value> {
        self.execute(document).await
    }

    fn reader_did(&self) -> Option<String> {
        self.actor_did()
    }

    async fn execute_read_with_variables(
        &self,
        document: &str,
        variables: &Value,
    ) -> Result<Value> {
        self.execute_with_variables(document, variables).await
    }
}

#[async_trait::async_trait]
impl ConfigRead for ConfigApplyTxn<'_> {
    async fn execute_read(&self, document: &str) -> Result<Value> {
        graphql::ensure_query_document(document)?;
        self.execute(document).await
    }

    fn reader_did(&self) -> Option<String> {
        self.actor_did().map(str::to_owned)
    }

    async fn execute_read_with_variables(
        &self,
        document: &str,
        variables: &Value,
    ) -> Result<Value> {
        graphql::ensure_query_document(document)?;
        self.execute_with_variables(document, variables).await
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
        Self::Local(Box::new(LocalConfigAccess::new(node)))
    }

    /// Bind document reads and writes to this actor across retries and transactions.
    /// A denied operation retains this identity; it never retries as the node.
    pub fn local_as(node: Arc<Node>, identity: identity::Did) -> Self {
        Self::Local(Box::new(
            LocalConfigAccess::new(node).with_identity(identity),
        ))
    }

    /// Carry the Vera delegation the direct request this access serves
    /// presented, for the writes it makes in this process as its User
    /// (`Identity.UserDelegation.admitPresented`); a non-local access is
    /// unchanged.
    pub fn with_presented_delegation(
        self,
        presented: Option<user_write::PresentedDelegation>,
    ) -> Self {
        match (self, presented) {
            (Self::Local(local), Some(presented)) => {
                Self::Local(Box::new(local.with_presented_delegation(presented)))
            }
            (access, _) => access,
        }
    }

    /// The runtime host's own access as its node: it holds the runtime key,
    /// so it carries the key's proof (`Identity.OwnerStaged.KeyCarrier.hostAccess`).
    pub fn local_runtime(
        node: Arc<Node>,
        runtime: Arc<dyn crate::identity::NodeIdentity>,
    ) -> Result<Self> {
        let actor = identity::Did::new(runtime.did())
            .map_err(ConfigClientError::RelationshipTargetIdentity)?;
        let access = Self::local_as(node, actor);
        Ok(access.with_runtime_agent_proof(crate::identity::RuntimeAgentProof::new(runtime)))
    }

    pub fn local_managed_agent(
        node: Arc<Node>,
        actor: identity::Did,
        identity: &dyn crate::identity::NodeIdentity,
    ) -> Result<Self> {
        let signer = identity
            .managed_agent_signer()
            .ok_or(ConfigClientError::ManagedAgentSignerRequired)?;
        if actor.as_str() != identity.did() {
            return Err(ConfigClientError::TransactionActorMismatch);
        }
        Self::local_with_agent_signer(node, actor, signer)
    }

    pub fn local_with_agent_signer(
        node: Arc<Node>,
        actor: identity::Did,
        signer: crate::identity::ManagedAgentSigner,
    ) -> Result<Self> {
        if actor.as_str() != signer.did() || node.node_identity_did() != Some(signer.node_did()) {
            return Err(ConfigClientError::TransactionActorMismatch);
        }
        let local = LocalConfigAccess::new(node)
            .with_identity(actor)
            .with_agent_signer(signer);
        Ok(Self::Local(Box::new(local)))
    }

    /// Hand this local access the runtime key's derivation proof, so the
    /// owner-staged decision may stage as an Agent the runtime controls
    /// (`Identity.OwnerStaged.derivedAgents`). A proof for another node's
    /// key is not attached.
    pub fn with_runtime_agent_proof(
        self,
        proof: Option<crate::identity::RuntimeAgentProof>,
    ) -> Self {
        let Some(proof) = proof else {
            return self;
        };
        match self {
            Self::Local(mut local) => {
                let proof_did = proof.runtime_did().to_owned();
                if !local.attach_agent_proof(proof) {
                    tracing::warn!(
                        proof_did,
                        node_did = local.node().node_identity_did(),
                        "runtime agent proof does not match the node; not attached"
                    );
                }
                Self::Local(local)
            }
            other => other,
        }
    }

    pub fn local_with_optional_agent_signer(
        node: Arc<Node>,
        actor: identity::Did,
        signer: Option<crate::identity::ManagedAgentSigner>,
    ) -> Result<Self> {
        match signer {
            Some(signer) => Self::local_with_agent_signer(node, actor, signer),
            None => Ok(Self::local_as(node, actor)),
        }
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    #[doc(hidden)]
    pub fn has_managed_agent_signer(&self) -> bool {
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
    /// Embedded access whose client-signed writes travel through `endpoint`,
    /// the loopback HTTP endpoint of this same node held by the same actor.
    /// Reads stay on the embedded node as that actor.
    pub fn with_client_signed_writes_through(mut self, endpoint: GraphqlEndpoint) -> Result<Self> {
        let Self::Local(local) = &mut self else {
            return Err(ConfigClientError::EmbeddedAccessRequired);
        };
        let capability = endpoint.held_client_signing_capability()?;
        if local.identity().map(identity::Did::as_str) != Some(capability.did()) {
            return Err(ConfigClientError::ClientSignerMismatch);
        }
        local.route_client_signed_writes(endpoint);
        Ok(self)
    }

    /// The transport a mutation takes from this access.
    pub(crate) fn write_route(&self) -> WriteRoute<'_> {
        match self {
            Self::Graphql(endpoint) => WriteRoute::Http(endpoint),
            Self::Local(local) => local
                .client_endpoint()
                .map_or(WriteRoute::Local(local), WriteRoute::Http),
        }
    }

    /// The DID whose signing key this access's client holds: its endpoint's,
    /// or the endpoint an embedded access routes its client-signed writes
    /// through.
    pub fn client_held_signer_did(&self) -> Option<&str> {
        match self.write_route() {
            WriteRoute::Http(endpoint) => endpoint
                .held_client_signing_capability()
                .ok()
                .map(|capability| capability.did()),
            WriteRoute::Local(_) => None,
        }
    }

    pub fn client_signer(&self) -> Result<ClientSigningCapability> {
        match self.write_route() {
            WriteRoute::Http(endpoint) => endpoint.held_client_signing_capability().cloned(),
            WriteRoute::Local(_) => Err(ConfigClientError::ClientAgentSignerRequired),
        }
    }

    pub fn with_client_signed_configuration(mut self) -> Self {
        match &mut self {
            Self::Local(local) => local.require_client_signed_writes(),
            Self::Graphql(endpoint) => endpoint.client_signed_agent_creates = true,
        }
        self
    }

    pub fn managed_agent_signer(&self) -> Option<crate::identity::ManagedAgentSigner> {
        match self {
            Self::Local(local) => local.agent_signer(),
            Self::Graphql(_) => None,
        }
    }

    /// See [`LocalConfigAccess::begin_store_release`].
    pub fn begin_store_release(&self) -> Option<crate::local::home::StoreRelease> {
        match self {
            Self::Local(local) => local.begin_store_release(),
            Self::Graphql(_) => None,
        }
    }

    /// See [`LocalConfigAccess::release_store`].
    pub fn release_store(&self) {
        if let Self::Local(local) = self {
            local.release_store();
        }
    }

    pub fn claimed_local(node: Arc<Node>, claim: crate::local::home::StoreLock) -> Result<Self> {
        Ok(Self::Local(Box::new(LocalConfigAccess::claimed(
            node, claim,
        )?)))
    }

    pub fn claimed_local_as(
        node: Arc<Node>,
        claim: crate::local::home::StoreLock,
        identity: identity::Did,
    ) -> Result<Self> {
        Ok(Self::Local(Box::new(
            LocalConfigAccess::claimed(node, claim)?.with_identity(identity),
        )))
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
                    let response = Self::read_local_response_owned(
                        local,
                        query,
                        None,
                        "config GraphQL",
                        txn::ReadAccess::Partial,
                    )
                    .await?;
                    crate::graphql::ensure_no_errors(&response, "config GraphQL")?;
                    Ok(crate::graphql::into_data_envelope(response.data))
                }
            }
        })
    }

    /// [`Self::execute`] with GraphQL variables for the document.
    pub fn execute_with_variables<'a>(
        &'a self,
        query: &'a str,
        variables: &'a Value,
    ) -> BoxFuture<'a, Result<Value>> {
        self.read_with_variables(query, variables, txn::ReadAccess::Partial)
    }

    fn read_with_variables<'a>(
        &'a self,
        query: &'a str,
        variables: &'a Value,
        access: txn::ReadAccess,
    ) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            match self {
                Self::Graphql(graphql) => {
                    post_graphql_with_variables(graphql, query, variables).await
                }
                Self::Local(local) => {
                    let response = Self::read_local_response_owned(
                        local,
                        query,
                        Some(variables),
                        "config GraphQL",
                        access,
                    )
                    .await?;
                    crate::graphql::ensure_no_errors(&response, "config GraphQL")?;
                    Ok(crate::graphql::into_data_envelope(response.data))
                }
            }
        })
    }

    /// A read a decision is taken on: the absence of a row it does not
    /// return is acted upon (a receipt to withdraw, an owed removal, a
    /// client access request). Embedded, it is a strict read, and an access
    /// check without an answer is [`ConfigClientError::DecisionReadUnanswered`]
    /// or [`ConfigClientError::DecisionReadFailed`]
    /// (`DecisionRead.decides_only_on_a_complete_read`). External premise:
    /// DefraDB's HTTP API has no strict read at this pin, so over HTTP it is
    /// [`Self::read`].
    pub fn read_for_decision<'a>(
        &'a self,
        operation: &'a gents_protocol::document_query::Rendered,
    ) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            decision_read::decided(
                self.read_with_variables(
                    &operation.document,
                    &operation.variables,
                    txn::ReadAccess::Strict,
                )
                .await,
            )
        })
    }

    /// Execute a rendered read and return its envelope.
    pub fn read<'a>(
        &'a self,
        operation: &'a gents_protocol::document_query::Rendered,
    ) -> BoxFuture<'a, Result<Value>> {
        self.execute_with_variables(&operation.document, &operation.variables)
    }

    /// Apply schema SDL; callers must perform their own structural compatibility check first.
    pub async fn add_collection(&self, sdl: &str) -> Result<()> {
        match self {
            Self::Graphql(graphql) => {
                let api_base = graphql_api_base(graphql.url())?;
                let url = format!("{api_base}/collections");
                let client = graphql::http_client().await?;
                let response = graphql
                    .send_resolving(|graphql| {
                        Ok(graphql
                            .authorize(client.post(&url))?
                            .header(reqwest::header::CONTENT_TYPE, "text/plain; charset=utf-8")
                            .body(sdl.to_owned()))
                    })
                    .await?
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
                let context = local.request_context();
                local
                    .run_owned(move |node| async move {
                        match context.identity() {
                            Some(_) => node.add_collection_as(&context, &sdl).await.map(|_| ()),
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
                    .send_resolving(|graphql| graphql.authorize(client.get(&url)))
                    .await?
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
        crate::graphql::validate_collection_identifier(collection)?;
        match self {
            Self::Local(local) if local.identity().is_some() => {
                let collection = collection.to_owned();
                let context = local.request_context();
                local
                    .run_owned(move |node| async move {
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
                    .send_resolving(|graphql| graphql.authorize(client.get(&url)))
                    .await?
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
/// What a person reads of a failed query: an update of Gents when the edge
/// of a Vera home asked for a delegation this client cannot present
/// (`Identity.HeldAudience.told`), otherwise the failure and its next steps.
fn graphql_query_failure(graphql: &str, source: &ConfigClientError) -> String {
    if source.edge_requires_delegation() {
        crate::local::document_acp::DocumentAcpError::DelegationRequired.to_string()
    } else {
        format!(
            "{source}\n{}",
            graphql_diagnostic_hint(graphql, source.refused_by_runtime())
        )
    }
}

pub fn graphql_diagnostic_hint(graphql: &str, refused: bool) -> String {
    if refused {
        format!(
            "Next:\n  1. The runtime at {graphql} does not admit this identity; run the command from the home that runtime serves\n  2. Or have that home's owner admit this identity\n  3. If the runtime was updated more recently than this device, update Gents on this device"
        )
    } else if is_probably_local_graphql_endpoint(graphql) {
        "Next:\n  1. If this home is not initialized, run `gents init`\n  2. Start the runtime with `gents runtime server`\n  3. Inspect it with `gents status`".to_string()
    } else {
        format!(
            "Next:\n  1. Verify the GraphQL endpoint {graphql}\n  2. Retry with `--graphql {graphql}` or point the command at the correct runtime"
        )
    }
}

/// [`post_graphql`] with GraphQL variables for the document.
pub async fn post_graphql_with_variables(
    graphql: &GraphqlEndpoint,
    query: &str,
    variables: &Value,
) -> Result<Value> {
    graphql::query_with_options(
        graphql,
        query,
        Some(variables),
        GraphqlRequestOptions::default(),
    )
    .await
    .map_err(|error| ConfigClientError::HttpQuery {
        endpoint: graphql.url().to_owned(),
        source: Box::new(error),
    })
}

pub async fn post_graphql(graphql: &GraphqlEndpoint, query: &str) -> Result<Value> {
    query_graphql_with_options(graphql, query, GraphqlRequestOptions::default()).await
}

/// Execute one HTTP GraphQL read; mutations are rejected before network I/O.
pub async fn query_graphql_with_options(
    graphql: &GraphqlEndpoint,
    query: &str,
    options: GraphqlRequestOptions,
) -> Result<Value> {
    graphql::query_with_options(graphql, query, None, options)
        .await
        .map_err(|error| ConfigClientError::HttpQuery {
            endpoint: graphql.url().to_owned(),
            source: Box::new(error),
        })
}
