use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use gents::defra_node::EmbeddedNode;
use gents_codex_protocol as codex;
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch, Mutex};
use tokio::task::JoinHandle;

mod agent_projection;
mod background;
mod bound_behavior;
mod caused_threads;
mod child_stream;
mod command_projection;
mod compaction_projection;
mod compat;
mod continuation_stream;
mod handlers;
mod history_projection;
mod host_runtime;
mod progress;
mod projection_state;
mod protocol;
mod store;
mod thread_projection;
mod thread_routes;
mod trace;
mod turn;
mod turn_projection;

const JSONRPC_INVALID_REQUEST: i64 = -32600;
const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;
const JSONRPC_INVALID_PARAMS: i64 = -32602;
const JSONRPC_INTERNAL_ERROR: i64 = -32603;

/// Display text is returned to Codex clients and the shim's stderr; keep it stable.
#[derive(Debug, thiserror::Error)]
enum CodexShimError {
    #[error("live request query omitted rows")]
    LiveRequestRowsOmitted,
    #[error("missing or ambiguous physical live request")]
    AmbiguousLiveRequest,
    #[error("live projection crossed exact request scope")]
    LiveRequestScopeCrossed,
    #[error("live request row missing")]
    LiveRequestRowMissing,
    #[error("decoding canonical Codex request")]
    DecodeCanonicalRequest(#[source] serde_json::Error),
    #[error("canonical Codex output is denied")]
    OutputDenied,
    #[error("canonical Codex output is conflicted")]
    OutputConflicted,
    #[error("canonical Codex output is invalid")]
    OutputInvalid,
    #[error("timed out waiting for terminal canonical output {request_id} after {timeout_secs}s of inactivity\n{hint}")]
    TerminalOutputTimeout {
        request_id: String,
        timeout_secs: u64,
        hint: String,
    },
    #[error("settled Codex request has no valid terminal output: {output}")]
    SettledWithoutTerminalOutput { output: String },
    #[error("timed out waiting for queued Codex steering request {request_id} after {timeout_secs}s of inactivity\n{hint}")]
    SteeringQueueTimeout {
        request_id: String,
        timeout_secs: u64,
        hint: String,
    },
    #[error("timed out waiting for canonical output for request {request_id} after {timeout_secs}s of inactivity\n{hint}")]
    OutputTimeout {
        request_id: String,
        timeout_secs: u64,
        hint: String,
    },
    #[error("committed request missing agent")]
    CommittedRequestMissingAgent,
    #[error("consumed input query omitted rows")]
    ConsumedInputRowsOmitted,
    #[error("consumed input lacks physical identity")]
    ConsumedInputMissingPhysicalIdentity,
    #[error("consumed input lacks authored owner")]
    ConsumedInputMissingOwner,
    #[error("consumed message query omitted rows")]
    ConsumedMessageRowsOmitted,
    #[error("consumed input has conflicting authored headers")]
    ConsumedInputConflictingHeaders,
    #[error("consumed input authored header is not yet visible")]
    ConsumedInputHeaderMissing,
    #[error("consumed input header changed during reconstruction")]
    ConsumedInputHeaderChanged,
    #[error("steering request query omitted rows")]
    SteeringRequestRowsOmitted,
    #[error("steering physical request missing or ambiguous")]
    SteeringRequestAmbiguous,
    #[error("steering request omitted content")]
    SteeringRequestContentMissing,
    #[error("decoding persisted steering input")]
    DecodeSteeringInput(#[source] serde_json::Error),
    #[error("steering request has no physical identity")]
    SteeringRequestMissingPhysicalIdentity,
    #[error("active request query omitted rows")]
    ActiveRequestRowsOmitted,
    #[error("ambiguous scoped active request label")]
    AmbiguousScopedActiveRequestLabel,
    #[error("decoding canonical AgentRequest row")]
    DecodeAgentRequestRow(#[source] serde_json::Error),
    #[error("AgentRequest row missing physical identity")]
    AgentRequestMissingPhysicalIdentity,
    #[error("AgentRequest row missing created_at")]
    AgentRequestMissingCreatedAt,
    #[error("interrupt request has no physical identity")]
    InterruptRequestMissingPhysicalIdentity,
    #[error("active request has no physical identity")]
    ActiveRequestMissingPhysicalIdentity,
    #[error("cycle in Codex steering queue ancestry at request {request_id}")]
    SteeringQueueCycle { request_id: String },
    #[error("bound Agent {agent_id:?} missing for {node_did:?}")]
    BoundAgentMissing { agent_id: String, node_did: String },
    #[error("decoding bound Agent {agent_id:?}")]
    DecodeBoundAgent {
        agent_id: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("bound inference profile {profile_id:?} missing for {node_did:?}")]
    BoundInferenceProfileMissing {
        profile_id: String,
        node_did: String,
    },
    #[error("bound inference profile is invalid")]
    BoundInferenceProfileInvalid(#[source] Box<gents::document_config::DocumentConfigError>),
    #[error("model selection must be nonempty")]
    ModelSelectionEmpty,
    #[error("no backend advertises model {model:?} in this node's credential scope")]
    ModelNotAdvertised { model: String },
    #[error("ambiguous advertised model {model:?}; specify backend::model")]
    ModelAmbiguous { model: String },
    #[error("bound agent does not exist for this node")]
    BoundAgentNotFoundForNode,
    #[error("bound agent is disabled")]
    BoundAgentDisabled,
    #[error("bound inference profile does not exist for this node")]
    BoundInferenceProfileNotFoundForNode,
    #[error("selected backend does not exist for this node")]
    SelectedBackendNotFound,
    #[error("selected backend is disabled")]
    SelectedBackendDisabled,
    #[error("selected backend has no discovery observation")]
    SelectedBackendNoObservation,
    #[error("selected backend has no catalog in this credential scope")]
    SelectedBackendNoCatalog,
    #[error("selected backend does not advertise requested model")]
    SelectedModelNotAdvertised,
    #[error("ambiguous advertised model")]
    SelectedModelAmbiguous,
    #[error("selected model does not advertise configured reasoning effort {effort}")]
    ReasoningEffortUnsupported { effort: String },
    #[error("no skill named {name:?}")]
    SkillNotFound { name: String },
    #[error("skills/config/write requires a path or name selector")]
    SkillSelectorRequired,
    #[error("validating forked thread notification")]
    ForkedThreadNotification(#[source] serde_json::Error),
}

mod server;
mod state;

#[allow(unused_imports)]
pub(crate) use server::{
    bind_codex_shim, resolve_codex_shim_agent_id, BoundCodexShim, CodexShimBindArgs,
    CodexShimBindError,
};
#[cfg(test)]
use server::{request_is_authorized, validate_bind_security};
#[allow(unused_imports)]
pub(crate) use state::{CodexSidecar, DEFAULT_MEMORY_MODE};
use state::{ConnectionState, Outbound, ShimState, TurnStreamControl};

#[cfg(test)]
mod tests;
