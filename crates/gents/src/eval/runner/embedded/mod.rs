pub mod executor;
pub mod home;
mod inspect;
mod live;
mod live_context;
pub mod observe;

pub use executor::outcome::provider_reason_from_failure;
pub use executor::EmbeddedExecutor;
pub use home::{boot_runtime, wait_for_runtime_ready, EmbeddedHome, RunningRuntime};
pub use inspect::{inspect_retained_requests, RetainedRequest};
pub use observe::{
    classify_request_outcome, evidence_query, inference_sample_query, request_evidence_from_query_data,
    request_evidence_from_sources, InferenceCallEvidence, MessageEvidence, NoHook, ObservationHook,
    RequestEvidence, ResponseEvidence, TerminalObservation, ToolCallEvidence,
};
