//! Embedded trial homes and runtime boot.

pub mod executor;
pub mod home;
mod inspect;
mod live;
mod live_context;
pub mod observe;

pub use executor::{provider_reason_from_failure, EmbeddedExecutor};
pub use home::{boot_runtime, wait_for_runtime_ready, EmbeddedHome, RunningRuntime};
pub use inspect::{inspect_retained_requests, RetainedRequest};
pub use observe::{
    await_terminal, await_terminal_with, classify_request_outcome, collect_request_evidence,
    evidence_query, inference_sample_query, request_evidence_from_query_data,
    request_evidence_from_sources, InferenceCallEvidence, MessageEvidence, NoHook, ObservationHook,
    RequestEvidence, ResponseEvidence, TerminalObservation, ToolCallEvidence,
};
