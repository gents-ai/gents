//! Embedded trial homes and runtime boot.

pub mod home;
pub mod observe;

pub use home::{boot_runtime, wait_for_runtime_ready, EmbeddedHome, RunningRuntime};
pub use observe::{
    await_terminal, await_terminal_with, classify_request_outcome, collect_request_evidence,
    evidence_query, inference_sample_query, request_evidence_from_query_data,
    InferenceCallEvidence, MessageEvidence, NoHook, ObservationHook, RequestEvidence,
    ResponseEvidence, TerminalObservation, ToolCallEvidence,
};
