use gents_protocol::error_chain::ErrorChain;

use super::txn::{
    NativeOperationAbandoned, ReentrantEmbeddedWrite, EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT,
};
use super::write_telemetry::{ConflictSource, WriteTelemetryError};

#[derive(Debug, thiserror::Error)]
pub enum ConfigClientError {
    #[error(transparent)]
    PackApproval(#[from] super::PackApprovalError),
    #[error("verified pack effect failed")]
    PackEvidence(#[source] Box<super::PackEvidenceError>),
    #[error("client Agent publication journal could not be recorded")]
    ClientAgentJournal(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("client Agent publication is interrupted or unconfirmed; inspect the retained publication journal before repair")]
    ClientAgentPublicationUnconfirmed(#[source] Box<ConfigClientError>),
    #[error("HTTP authentication and client signing require a locally held actor key")]
    ClientAgentSignerRequired,
    #[error("held client signing key does not match the authenticated actor or bearer")]
    ClientSignerMismatch,
    #[error("client-signed Agent creation is unavailable for this transaction")]
    ClientAgentTransportRequired,
    #[error("client-signed writes require a running HTTP endpoint; start the runtime and retry")]
    EmbeddedClientSigningUnavailable,
    #[error("client-signed updates are unavailable for this collection or transaction")]
    ClientConfigurationUpdateUnsupported,
    #[error("client-authorized HTTP mutations require the client-signed transaction owner")]
    ClientUnsignedMutationUnsupported,
    #[error("client-signed update requires one explicit document and matching collection")]
    ClientConfigurationUpdateInvalid,
    #[error("configuration removal was not confirmed")]
    DesiredStateRemovalUnconfirmed,
    #[error("client-signed delete requires one exact document and a trusted configuration schema")]
    ClientConfigurationDeleteInvalid,
    #[error("client-signed deletion did not confirm the requested document")]
    ClientConfigurationDeleteNotApplied,
    #[error("immutable configuration field {field} cannot be changed")]
    ClientConfigurationImmutableField { field: String },
    #[error("configuration mutation did not update {collection} {node_did}/{id}; the document may be unavailable or the actor may lack update permission")]
    ConfigurationMutationNotApplied {
        collection: &'static str,
        node_did: String,
        id: String,
    },
    #[error("resolving the trusted canonical schema lineage")]
    ClientCanonicalSchema(#[source] gents_migration::Error),
    #[error("building the client-signed configuration update graph")]
    ClientConfigurationGraph(#[source] db::block::builder::ClientUpdateError),
    #[error("decoding the client-signed mutation history response")]
    ClientSignedSnapshotDecode(#[source] serde_json::Error),
    #[error("building the client-signed configuration delete graph")]
    ClientConfigurationDeleteGraph(#[source] db::block::builder::ClientDeleteError),
    #[error("verifying the client-signed configuration mutation graph")]
    ClientVerifiedMutationGraph(#[source] db::block::builder::VerifiedMutationError),
    #[error("building the client-signed Agent graph")]
    ClientAgentGraph(#[source] db::block::builder::ClientCreateError),
    #[error("joining client-signed Agent graph preparation")]
    ClientAgentGraphJoin(#[source] tokio::task::JoinError),
    #[error("client-signed Agent conversation failed")]
    ClientAgentSigning(#[source] defra_http::client_signed_create::ClientSignedCreateError),
    #[error("client-signed Agent connection failed")]
    ClientAgentConnection(#[source] Box<tokio_tungstenite::tungstenite::Error>),
    #[error("client-signed Agent conversation timed out")]
    ClientAgentTimeout(#[source] tokio::time::error::Elapsed),
    #[error("a DefraDB Bearer authorization is required")]
    BearerRequired,
    #[error("GraphQL subscriptions require an authenticated remote endpoint")]
    RemoteSubscriptionRequired,
    #[error("GraphQL subscription response is not an event stream: {0:?}")]
    SubscriptionContentType(Option<String>),
    #[error("GraphQL subscription handshake timed out")]
    SubscriptionHandshake(#[source] tokio::time::error::Elapsed),
    #[error("reading GraphQL subscription events")]
    SubscriptionStream(#[source] eventsource_stream::EventStreamError<reqwest::Error>),
    #[error("unexpected GraphQL subscription event: {0}")]
    SubscriptionEvent(String),
    #[error("invalid authorization header")]
    AuthorizationHeader(#[source] reqwest::header::InvalidHeaderValue),
    #[error("expected GraphQL endpoint ending in /graphql, got {graphql}")]
    GraphqlApiBase { graphql: String },
    #[error("ConfigAccess::write requires a GraphQL mutation document")]
    MutationDocumentRequired,
    #[error("ConfigAccess::execute requires a GraphQL query document")]
    QueryDocumentRequired,
    #[error(transparent)]
    LensConfig(#[from] lens::Error),
    #[error(transparent)]
    TransactionConflict(TransactionConflict),
    /// Displays the whole wrapped chain and also exposes it as the source.
    #[error("{}", ErrorChain(.0.as_ref()))]
    StorageFailure(#[source] Box<ConfigClientError>),
    #[error(transparent)]
    WriteTelemetry(#[from] WriteTelemetryError),
    #[error(transparent)]
    Reentrant(#[from] ReentrantEmbeddedWrite),
    #[error(transparent)]
    Protocol(#[from] gents_protocol::graphql::GraphqlError),
    #[error(transparent)]
    Identity(Box<crate::store::identity::IdentityError>),
    #[error(transparent)]
    Node(Box<defra_node::Error>),
    #[error(transparent)]
    Query(Box<query::error::QueryError>),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    AuthorizationValue(#[from] reqwest::header::ToStrError),
    #[error(transparent)]
    Response(#[from] crate::store::graphql::GraphqlResponseError),
    #[error("parsing GraphQL endpoint {url}")]
    ParseEndpoint {
        url: String,
        #[source]
        source: <reqwest::Url as std::str::FromStr>::Err,
    },
    #[error("GraphQL endpoint {url} has no host")]
    EndpointHost { url: String },
    #[error("encoding DefraDB bearer header")]
    BearerHeader(#[source] reqwest::header::InvalidHeaderValue),
    #[error("owned native operation requires local access")]
    LocalAccessRequired,
    #[error("plugin data actor does not match the transaction actor")]
    TransactionActorMismatch,
    #[error("validated managed Agent signing capability is required")]
    ManagedAgentSignerRequired,
    #[error("a document relationship requires an authenticated actor")]
    RelationshipActorRequired,
    #[error("invalid transaction owner observation response")]
    InvalidOwnerObservation,
    #[error("invalid document relationship actor")]
    RelationshipTargetIdentity(#[source] identity::Error),
    #[error("transaction committed, but document relationships remain incomplete for creator {creator}: {pending:?}")]
    CommittedRelationshipsIncomplete {
        creator: String,
        pending: Vec<super::ConfigRelationshipGrant>,
        #[source]
        source: Box<ConfigClientError>,
    },
    #[error("transaction outcome is unconfirmed; document relationships may be incomplete for creator {creator}: {pending:?}")]
    UnconfirmedRelationships {
        creator: String,
        pending: Vec<super::ConfigRelationshipGrant>,
        #[source]
        source: Box<ConfigClientError>,
    },
    #[error("node operation requires an authenticated runtime identity")]
    NodePermissionActorRequired,
    #[error("invalid node operation identity")]
    NodePermissionIdentity(#[source] identity::Error),
    #[error("node permission grant failed ({status}): {body}")]
    NodePermissionStatus {
        status: reqwest::StatusCode,
        body: String,
    },
    #[error("{operation} returned HTTP {status}")]
    HttpOperationStatus {
        operation: &'static str,
        status: reqwest::StatusCode,
    },
    #[error("transactional relationship request failed ({status}): {body}")]
    RelationshipStatus {
        status: reqwest::StatusCode,
        body: String,
    },
    #[error(
        "this backend does not support transaction-bound relationship or permission operations"
    )]
    TransactionAuthorizationUnsupported,
    #[error("invalid resource relationship target")]
    InvalidResourceRelationshipTarget,
    #[error("document relationship request failed ({status}): {body}")]
    DocumentRelationshipStatus {
        status: reqwest::StatusCode,
        body: String,
    },
    #[error("transactional permission observation returned no decision")]
    PermissionObservationMissing,
    #[error("runtime identity cannot act for agent document work")]
    RuntimeDocumentActor,
    #[error("{source}\n{}", super::graphql_diagnostic_hint(endpoint))]
    HttpQuery {
        endpoint: String,
        #[source]
        source: Box<ConfigClientError>,
    },
    #[error("posting schema SDL to {url}")]
    PostSchema {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("reading schema SDL response from {url}")]
    ReadSchemaResponse {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("schema SDL request to {url} failed with {status}: {body}")]
    SchemaStatus {
        url: String,
        status: reqwest::StatusCode,
        body: String,
    },
    #[error("adding schema SDL")]
    AddSchema(#[source] Box<defra_node::Error>),
    #[error("fetching collection versions from {url}")]
    FetchCollectionVersions {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("decoding collection versions from {url}")]
    DecodeCollectionVersions {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(
        "the pinned DefraDB HTTP API does not expose collection materialization; use the schema tool on the runtime node"
    )]
    MaterializeUnsupported,
    #[error("schema HTTP request requires an HTTP endpoint")]
    SchemaEndpointRequired,
    #[error("schema operation at {url}")]
    SchemaOperation {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("schema operation failed ({status}): {body}")]
    SchemaOperationStatus {
        status: reqwest::StatusCode,
        body: String,
    },
    #[error("serializing active collection version")]
    SerializeCollectionVersion(#[source] serde_json::Error),
    #[error("building DefraDB configuration HTTP client")]
    HttpClientBuild(#[source] reqwest::Error),
    #[error("joining DefraDB configuration HTTP client initialization")]
    HttpClientJoin(#[source] tokio::task::JoinError),
    #[error("posting GraphQL mutation to {endpoint}")]
    PostMutation {
        endpoint: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("reading GraphQL mutation response from {endpoint}")]
    ReadMutationResponse {
        endpoint: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("decoding GraphQL mutation response from {endpoint}")]
    DecodeMutationResponse {
        endpoint: String,
        #[source]
        source: reqwest::Error,
    },
    /// Error list returned by DefraDB; its source exposes storage exhaustion.
    #[error(transparent)]
    Database(#[from] gents_protocol::database_error::GraphqlFailure),
    /// Error list returned by the embedded DefraDB node.
    #[error("graphql returned errors: {errors:?}")]
    EmbeddedGraphqlErrors {
        errors: Vec<query::QueryResponseError>,
    },
    #[error("posting tx begin to {endpoint}")]
    PostTxBegin {
        endpoint: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("reading tx begin body")]
    ReadTxBegin(#[source] reqwest::Error),
    #[error("tx begin returned HTTP {status} from {endpoint}: {body}")]
    TxBeginStatus {
        status: reqwest::StatusCode,
        endpoint: String,
        body: String,
    },
    #[error("tx begin returned HTTP {status} from {endpoint}")]
    TxBeginStorageFull {
        status: reqwest::StatusCode,
        endpoint: String,
        #[source]
        source: gents_protocol::database_error::StorageFull,
    },
    #[error("decoding tx begin body")]
    DecodeTxBegin(#[source] serde_json::Error),
    #[error("tx begin missing id")]
    TxBeginMissingId,
    #[error("posting transactional GraphQL to {endpoint}")]
    PostTxGraphql {
        endpoint: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("reading transactional GraphQL from {endpoint}")]
    ReadTxGraphql {
        endpoint: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("decoding transactional GraphQL from {endpoint}")]
    DecodeTxGraphql {
        endpoint: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("posting tx commit to {endpoint}")]
    PostTxCommit {
        endpoint: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("reading tx commit body")]
    ReadTxCommit(#[source] reqwest::Error),
    #[error("tx commit returned HTTP {status} from {endpoint}: {body}")]
    TxCommitStatus {
        status: reqwest::StatusCode,
        endpoint: String,
        body: String,
    },
    #[error("tx commit returned HTTP {status} from {endpoint}: {body}")]
    TxCommitStorageFull {
        status: reqwest::StatusCode,
        endpoint: String,
        body: String,
        #[source]
        source: gents_protocol::database_error::StorageFull,
    },
    #[error("posting tx discard to {endpoint}")]
    PostTxDiscard {
        endpoint: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("reading tx discard body")]
    ReadTxDiscard(#[source] reqwest::Error),
    #[error("tx discard returned HTTP {status} from {endpoint}: {body}")]
    TxDiscardStatus {
        status: reqwest::StatusCode,
        endpoint: String,
        body: String,
    },
    #[error("owned embedded operation ended without a result")]
    OwnedOperationEnded(#[source] tokio::sync::oneshot::error::RecvError),
    #[error("embedded node already has a live store claim")]
    StoreClaimHeld,
    #[error("embedded write gate is closed: {error}")]
    WriteGateClosed { error: tokio::sync::AcquireError },
    #[error(
        "embedded transaction write admission for {operation} timed out after {timeout:?}; {active_owner_count} active owners, longest-held owner {owner_operation} held for {owner_held_ms}ms"
    )]
    WriteAdmissionTimeout {
        operation: &'static str,
        timeout: std::time::Duration,
        active_owner_count: usize,
        owner_operation: &'static str,
        owner_held_ms: u64,
    },
    #[error("embedded transaction is poisoned by an abandoned native operation")]
    PoisonedByAbandonedOperation(#[source] NativeOperationAbandoned),
    #[error(
        "embedded execute caller timed out after {:?}; native operation remains owned",
        EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT
    )]
    ExecuteCallerTimeout(#[source] NativeOperationAbandoned),
    #[error("native signed execution task ended without a response")]
    ExecutionTaskEnded(#[source] NativeOperationAbandoned),
    #[error("commit rejected after an abandoned native transaction operation")]
    CommitAfterAbandonedOperation(#[source] NativeOperationAbandoned),
    #[error("rollback deferred until abandoned native transaction operations return")]
    RollbackDeferred(#[source] NativeOperationAbandoned),
    #[error(
        "embedded auto-commit caller timed out after {:?}; native operation and write gate remain owned",
        EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT
    )]
    AutoCommitCallerTimeout(#[source] NativeOperationAbandoned),
    #[error("native auto-commit task ended without a response")]
    AutoCommitTaskEnded(#[source] NativeOperationAbandoned),
    #[error("embedded transaction {phase} timed out after {timeout:?}")]
    PhaseTimeout {
        phase: &'static str,
        timeout: std::time::Duration,
    },
    #[error("begin_txn")]
    BeginTransaction {
        #[source]
        source: query::TransactionError,
    },
    #[error("native transaction owner ended before registration")]
    OwnerEndedBeforeRegistration,
    #[error("native transaction registration")]
    Registration(#[source] tokio::sync::oneshot::error::RecvError),
    #[error("begin HTTP transaction task")]
    BeginTask(#[source] tokio::task::JoinError),
    #[error("GraphQL variables must be an object")]
    VariablesNotObject,
    #[error("native transaction responses require embedded access")]
    EmbeddedAccessRequired,
    #[error("authenticated transaction owner observations require embedded access")]
    OwnerObservationRequiresEmbedded,
    #[error("commit_txn")]
    CommitTransaction {
        #[source]
        source: query::TransactionError,
    },
    #[error("rollback_txn")]
    RollbackTransaction {
        #[source]
        source: query::TransactionError,
    },
    #[error("injected transaction read storage failure")]
    InjectedReadStorageFailure,
    #[error("injected failure after successful transaction mutation {count}")]
    InjectedMutationFailure { count: usize },
    #[error("injected receipt loss after committed transaction")]
    InjectedReceiptLoss,
    #[error("injected rollback failure")]
    InjectedRollbackFailure,
}

impl From<crate::store::identity::IdentityError> for ConfigClientError {
    fn from(error: crate::store::identity::IdentityError) -> Self {
        Self::Identity(Box::new(error))
    }
}

impl ConfigClientError {
    /// DefraDB's explicit structured refusal or an HTTP authentication/ACP
    /// rejection. A missing or hidden document is not evidence of either.
    pub fn is_permission_denied(&self) -> bool {
        let denied = |status: reqwest::StatusCode| {
            matches!(
                status,
                reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
            )
        };
        match self {
            Self::Query(error) => matches!(
                error.as_ref(),
                query::error::QueryError::PermissionDenied(_)
            ),
            Self::EmbeddedGraphqlErrors { errors } => errors.iter().any(|error| {
                error.code() == Some(gents_protocol::database_error::PERMISSION_DENIED_CODE)
            }),
            Self::NodePermissionStatus { status, .. }
            | Self::HttpOperationStatus { status, .. }
            | Self::RelationshipStatus { status, .. }
            | Self::SchemaStatus { status, .. }
            | Self::SchemaOperationStatus { status, .. }
            | Self::TxBeginStatus { status, .. }
            | Self::TxCommitStatus { status, .. }
            | Self::TxDiscardStatus { status, .. } => denied(*status),
            Self::Response(crate::store::graphql::GraphqlResponseError::Access(error))
            | Self::StorageFailure(error)
            | Self::HttpQuery { source: error, .. } => error.is_permission_denied(),
            _ => {
                self.database_failure().is_some_and(
                    gents_protocol::database_error::GraphqlFailure::is_permission_denied,
                ) || self
                    .transport_error()
                    .and_then(reqwest::Error::status)
                    .is_some_and(denied)
            }
        }
    }

    /// The HTTP transport source held by this typed access error, if any.
    pub fn transport_error(&self) -> Option<&reqwest::Error> {
        match self {
            Self::Http(source)
            | Self::HttpClientBuild(source)
            | Self::ReadTxBegin(source)
            | Self::ReadTxCommit(source)
            | Self::ReadTxDiscard(source)
            | Self::PostSchema { source, .. }
            | Self::ReadSchemaResponse { source, .. }
            | Self::FetchCollectionVersions { source, .. }
            | Self::DecodeCollectionVersions { source, .. }
            | Self::SchemaOperation { source, .. }
            | Self::PostMutation { source, .. }
            | Self::ReadMutationResponse { source, .. }
            | Self::DecodeMutationResponse { source, .. }
            | Self::PostTxBegin { source, .. }
            | Self::PostTxGraphql { source, .. }
            | Self::ReadTxGraphql { source, .. }
            | Self::DecodeTxGraphql { source, .. }
            | Self::PostTxCommit { source, .. }
            | Self::PostTxDiscard { source, .. } => Some(source),
            Self::Protocol(error) => error.transport_error(),
            Self::StorageFailure(source) | Self::HttpQuery { source, .. } => {
                source.transport_error()
            }
            Self::Response(crate::store::graphql::GraphqlResponseError::Access(source)) => {
                source.transport_error()
            }
            _ => None,
        }
    }
    /// The access error this one carries: a GraphQL response owner forwarding
    /// an access failure adds no meaning of its own, so classification looks
    /// through it.
    fn access(&self) -> &ConfigClientError {
        match self {
            Self::Response(crate::store::graphql::GraphqlResponseError::Access(error)) => {
                error.access()
            }
            _ => self,
        }
    }

    /// The conflict origin when the transaction owner may replay this failure.
    pub(crate) fn transaction_conflict(&self) -> Option<ConflictSource> {
        match self.access() {
            Self::TransactionConflict(conflict) => Some(conflict.origin),
            _ => None,
        }
    }

    /// A storage failure the transaction owner classified while running an
    /// attempt, as opposed to an error the callback returned.
    pub(crate) fn is_storage_failure(&self) -> bool {
        matches!(self.access(), Self::StorageFailure(_))
    }

    /// A caller stopped waiting while the native operation stays owned by its
    /// worker, so the outcome of that operation is not yet known.
    pub(crate) fn native_operation_abandoned(&self) -> bool {
        match self {
            Self::PoisonedByAbandonedOperation(_)
            | Self::ExecuteCallerTimeout(_)
            | Self::ExecutionTaskEnded(_)
            | Self::CommitAfterAbandonedOperation(_)
            | Self::RollbackDeferred(_)
            | Self::AutoCommitCallerTimeout(_)
            | Self::AutoCommitTaskEnded(_) => true,
            Self::Response(crate::store::graphql::GraphqlResponseError::Access(error))
            | Self::StorageFailure(error)
            | Self::HttpQuery { source: error, .. } => error.native_operation_abandoned(),
            _ => false,
        }
    }

    /// An identity key refused for its permissions, through the transparent
    /// identity variant and the access wrappers.
    pub fn insecure_key(&self) -> Option<&crate::store::identity::InsecureKeyPermissions> {
        match self {
            Self::Identity(error) => error.insecure_key(),
            Self::Response(crate::store::graphql::GraphqlResponseError::Access(error))
            | Self::StorageFailure(error)
            | Self::HttpQuery { source: error, .. } => error.insecure_key(),
            _ => None,
        }
    }

    /// The GraphQL errors DefraDB answered over HTTP, whichever owner carried
    /// them; each wraps them transparently, so `source()` cannot reach them.
    fn database_failure(&self) -> Option<&gents_protocol::database_error::GraphqlFailure> {
        match self {
            Self::Database(failure)
            | Self::Response(crate::store::graphql::GraphqlResponseError::Database(failure)) => {
                Some(failure)
            }
            Self::Protocol(error) => error.database_failure(),
            _ => None,
        }
    }

    /// DefraDB answered the operation with a store conflict its GraphQL
    /// errors code as retryable.
    pub(crate) fn is_retryable_database_conflict(&self) -> bool {
        match self {
            Self::EmbeddedGraphqlErrors { errors } => {
                !self.is_permission_denied()
                    && errors
                        .iter()
                        .any(|error| error.code() == Some(query::executor::TXN_CONFLICT_ERROR_CODE))
            }
            Self::Response(crate::store::graphql::GraphqlResponseError::Access(error))
            | Self::StorageFailure(error)
            | Self::HttpQuery { source: error, .. } => error.is_retryable_database_conflict(),
            _ => self
                .database_failure()
                .is_some_and(gents_protocol::database_error::GraphqlFailure::is_retryable_conflict),
        }
    }

    /// The database processed the operation and answered with GraphQL errors,
    /// as opposed to a transport, storage or local failure.
    pub fn is_graphql_rejection(&self) -> bool {
        match self {
            Self::EmbeddedGraphqlErrors { .. } => true,
            Self::Response(crate::store::graphql::GraphqlResponseError::Access(error))
            | Self::StorageFailure(error)
            | Self::HttpQuery { source: error, .. } => error.is_graphql_rejection(),
            _ => self.database_failure().is_some(),
        }
    }
}

impl From<query::error::QueryError> for ConfigClientError {
    fn from(error: query::error::QueryError) -> Self {
        Self::Query(Box::new(error))
    }
}

impl From<defra_node::Error> for ConfigClientError {
    fn from(error: defra_node::Error) -> Self {
        Self::Node(Box::new(error))
    }
}

/// A conflict the transaction owner may replay; its origin is telemetry only.
#[derive(Debug, thiserror::Error)]
#[error("DefraDB transaction conflict")]
pub struct TransactionConflict {
    pub(super) origin: ConflictSource,
}
