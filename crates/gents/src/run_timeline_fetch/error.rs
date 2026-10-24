use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub(super) enum RunTimelineError {
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
}

pub(super) type Result<T, E = RunTimelineError> = std::result::Result<T, E>;
