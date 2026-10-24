use gents_protocol::error_chain::ErrorChain;
use serde_json::Value;

use super::txn::{
    NativeOperationAbandoned, ReentrantEmbeddedWrite, EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT,
};
use super::write_telemetry::{ConflictSource, WriteTelemetryError};

#[derive(Debug, thiserror::Error)]
pub enum ConfigClientError {
    #[error("a DefraDB Bearer authorization is required")]
    BearerRequired,
    #[error("invalid authorization header")]
    AuthorizationHeader(#[source] reqwest::header::InvalidHeaderValue),
    #[error("expected GraphQL endpoint ending in /graphql, got {graphql}")]
    GraphqlApiBase { graphql: String },
    #[error("ConfigAccess::write requires a GraphQL mutation document")]
    MutationDocumentRequired,
    #[error("ConfigAccess::execute requires a GraphQL query document")]
    QueryDocumentRequired,
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
    Identity(Box<crate::identity::IdentityError>),
    #[error(transparent)]
    Node(Box<defra_node::Error>),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    AuthorizationValue(#[from] reqwest::header::ToStrError),
    #[error(transparent)]
    Response(#[from] crate::graphql::GraphqlResponseError),
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
    /// Error list returned by DefraDB over HTTP.
    #[error("graphql returned errors: {errors}")]
    HttpGraphqlErrors { errors: Value },
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
    #[error("begin_txn: {error}")]
    BeginTransaction { error: query::TransactionError },
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
    #[error("commit_txn: {error}")]
    CommitTransaction { error: query::TransactionError },
    #[error("rollback_txn: {error}")]
    RollbackTransaction { error: query::TransactionError },
    #[error("injected transaction read storage failure")]
    InjectedReadStorageFailure,
    #[error("injected failure after successful transaction mutation {count}")]
    InjectedMutationFailure { count: usize },
    #[error("injected receipt loss after committed transaction")]
    InjectedReceiptLoss,
    #[error("injected rollback failure")]
    InjectedRollbackFailure,
}

impl From<crate::identity::IdentityError> for ConfigClientError {
    fn from(error: crate::identity::IdentityError) -> Self {
        Self::Identity(Box::new(error))
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
