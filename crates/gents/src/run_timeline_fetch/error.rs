use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum RunTimelineError {
    #[error("request_id {request_id} is ambiguous across AgentRequest documents {existing_doc_id:?} and {doc_id:?}")]
    AmbiguousRequest {
        request_id: String,
        existing_doc_id: Option<String>,
        doc_id: Option<String>,
    },
    #[error("AgentRequest _docID {doc_id} is bound to both {existing} and {request_id}")]
    AgentRequestDocBound {
        doc_id: String,
        existing: String,
        request_id: String,
    },
    #[error("AgentMessage row disagrees with its canonical request reference")]
    MessageRequestMismatch,
    #[error("AgentMessage {message} names an unresolved physical request {doc_id}")]
    UnresolvedMessageRequest { message: String, doc_id: String },
    #[error("{collection} {label} has incomplete request lineage: request_id={request_id:?} request_doc_id={request_doc_id:?}")]
    IncompleteRequestLineage {
        collection: String,
        label: String,
        request_id: Option<String>,
        request_doc_id: Option<String>,
    },
    #[error("{collection} {label} request_doc_id {request_doc_id} belongs to {expected}, not {request_id}")]
    RequestDocBelongsElsewhere {
        collection: String,
        label: String,
        request_doc_id: String,
        expected: String,
        request_id: String,
    },
    #[error("{collection} {label} points to AgentRequest {request_doc_id}, which is outside this timeline")]
    RequestDocOutsideTimeline {
        collection: String,
        label: String,
        request_doc_id: String,
    },
    #[error("child AgentRequest {request_id} has incomplete parent tool lineage")]
    IncompleteParentToolLineage { request_id: String },
    #[error("child AgentRequest {request_id} points to missing AgentToolCall {tool_doc_id}")]
    MissingBridgeTool {
        request_id: String,
        tool_doc_id: String,
    },
    #[error("child AgentRequest {request_id} has a mismatched physical AgentToolCall bridge {tool_doc_id}")]
    MismatchedBridgeTool {
        request_id: String,
        tool_doc_id: String,
    },
    #[error("{collection} {label} has no {field}")]
    MissingLineage {
        collection: String,
        label: String,
        field: String,
    },
    #[error("timeline {collection} query failed: {errors}")]
    RequiredRowsFailed { collection: String, errors: Value },
    #[error("timeline query omitted required {collection} row array")]
    RequiredRowsOmitted { collection: String },
    #[error("timeline {collection} query returned a non-document row")]
    NonDocumentRows { collection: String },
    #[error("ambiguous owner/requester-scoped timeline session")]
    AmbiguousSessionScope,
    #[error("timeline session is outside authoritative request scope")]
    SessionOutsideScope,
    #[error(transparent)]
    Session(#[from] crate::session::SessionError),
    #[error(transparent)]
    Access(#[from] crate::config_client::ConfigClientError),
    #[error(transparent)]
    OAuthCredential(#[from] crate::oauth_credential::OAuthCredentialError),
    #[error(transparent)]
    SessionOrigin(#[from] crate::session_origin::SessionOriginError),
    #[error(transparent)]
    GoalHistory(#[from] crate::goal::GoalHistoryError),
    #[error(transparent)]
    InferenceBackend(#[from] crate::config_client::InferenceBackendError),
    #[error("reading required timeline collection {collection}")]
    RequiredRowsRead {
        collection: String,
        #[source]
        source: crate::config_client::ConfigClientError,
    },
    #[error("decoding {collection} rows")]
    RowsDecode {
        collection: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("request {request_id} not found")]
    RequestNotFound { request_id: String },
    #[error("request_id {request_id} is ambiguous across {count} AgentRequest documents")]
    RequestAmbiguous { request_id: String, count: usize },
    #[error("timeline AgentSession query failed: {errors}")]
    SessionsQueryFailed { errors: Value },
    #[error("timeline AgentSession query omitted data")]
    SessionsDataOmitted,
    #[error("timeline query omitted AgentSession scope s{index}")]
    SessionScopeOmitted { index: usize },
    #[error(transparent)]
    CanonicalRows(#[from] crate::session::canonical_rows::CanonicalRowsError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Query(#[from] crate::tool_call_lifecycle::query::ToolCallQueryError),
    #[error("timeline AgentMessage query failed: {errors}")]
    MessagesQueryFailed { errors: Value },
    #[error("canonical AgentMessage {doc_id} crossed the requested session scope")]
    HeaderCrossedScope { doc_id: String },
    #[error("resolving canonical AgentMessage {doc_id} for timeline")]
    ResolveMessage {
        doc_id: String,
        #[source]
        source: crate::session::SessionOutputError,
    },
    #[error("canonical AgentMessage {doc_id} changed during timeline reconstruction")]
    HeaderChanged { doc_id: String },
    #[error("canonical AgentMessage {doc_id} was not resolved")]
    HeaderUnresolved { doc_id: String },
    #[error("AgentMessage row omitted _docID")]
    MessageMissingDocId,
    #[error("spawned timeline tool lacks physical identity")]
    SpawnedToolMissingDocId,
    #[error("spawned timeline tool lacks request binding")]
    SpawnedToolMissingRequest,
    #[error("spawned timeline tool output lacks one exact closure")]
    SpawnedOutputClosure,
    #[error("accepted tool lacks its accepted message sequence")]
    AcceptedToolMissingSequence,
    #[error("accepted tool sequence has no canonical messages")]
    AcceptedSequenceMessagesMissing,
    #[error("accepted tool call crossed the requested session scope")]
    AcceptedCrossedScope,
    #[error("activity request has no session owner")]
    ActivityMissingOwner,
    #[error("decoding live InferenceCall activity")]
    LiveInferenceDecode(#[source] serde_json::Error),
    #[error("decoding live AgentToolCall activity")]
    LiveToolDecode(#[source] serde_json::Error),
    #[error("timeline root request has no session owner")]
    RootMissingSessionOwner,
    #[error("timeline root AgentRequest has no _docID")]
    RootMissingDocId,
    #[error("child AgentRequest {child} physical parent is {physical_parent}, but logical parent is {logical_parent:?}")]
    ChildParentMismatch {
        child: String,
        physical_parent: String,
        logical_parent: Option<String>,
    },
    #[error("timeline session request has no owner")]
    SessionRequestMissingOwner,
    #[error("timeline root request has no goal owner")]
    RootMissingGoalOwner,
    #[error(transparent)]
    SpawnedOutputReconstruction(gents_protocol::output::ReconstructionError),
    #[error("spawned timeline source is not tool output")]
    SpawnedNotToolOutput,
    #[error("timeline AgentToolCall omits authoritative lifecycle_state")]
    ToolLifecycleMissing,
    #[error("timeline tool lacks physical identity")]
    ToolMissingDocId,
    #[error("timeline tool lacks physical request binding")]
    ToolMissingRequest,
    #[error("accepted tool header reconstructed as a non-assistant message")]
    HeaderNotAssistant,
    #[error("accepted physical tool does not match its native block position")]
    NativeBlockMismatch,
    #[error("accepted tool native identity disagrees with its header")]
    NativeIdentityMismatch,
    #[error("spawned tool parent is not an accepted spawn_process invocation")]
    SpawnParentNotSpawn,
    #[error("spawned tool name differs from parent input")]
    SpawnedToolNameMismatch,
    #[error("timeline tool row disagrees with accepted native identity")]
    ToolRowIdentityMismatch,
    #[error("timeline tool lacks one exact accepted physical binding")]
    ToolBindingNotExact,
    #[error("timeline tool has conflicting canonical deliveries")]
    ToolDeliveriesConflict,
    #[error("timeline InferenceCall returned a row outside its request batch")]
    InferenceBatchOutside,
    #[error("timeline InferenceCall omitted request_doc_id")]
    InferenceMissingRequest,
    #[error("timeline InferenceCall returned an unrequested request document")]
    InferenceUnrequested,
    #[error("timeline ProviderContextReduction returned a row outside its request batch")]
    ReductionBatchOutside,
    #[error("timeline ProviderContextReduction returned an unrequested request document")]
    ReductionUnrequested,
}

impl crate::config_client::TransactionFailure for RunTimelineError {
    fn config_client_error(&self) -> Option<&crate::config_client::ConfigClientError> {
        match self {
            Self::Access(error) => Some(error),
            Self::RequiredRowsRead { source, .. } => Some(source),
            Self::Session(error) => error.config_client_error(),
            Self::ResolveMessage { source, .. } => source.config_client_error(),
            Self::OAuthCredential(error) => error.config_client_error(),
            Self::SessionOrigin(error) => error.config_client_error(),
            Self::GoalHistory(error) => error.config_client_error(),
            Self::InferenceBackend(error) => error.config_client_error(),
            Self::Query(error) => error.config_client_error(),
            _ => None,
        }
    }
}

impl RunTimelineError {
    pub(crate) fn reconstruction(&self) -> Option<&gents_protocol::output::ReconstructionError> {
        match self {
            Self::SpawnedOutputReconstruction(error) => Some(error),
            Self::ResolveMessage { source, .. } => source.reconstruction(),
            Self::Query(error) => error.reconstruction(),
            _ => None,
        }
    }
}

pub(super) type Result<T, E = RunTimelineError> = std::result::Result<T, E>;
