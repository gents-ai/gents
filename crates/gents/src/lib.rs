#[cfg(any(test, feature = "test-fixtures"))]
extern crate self as gents;

#[cfg(any(test, feature = "test-fixtures"))]
#[doc(hidden)]
#[path = "../tests/support/mod.rs"]
pub mod support;

mod claude_request_context;
pub mod eval;
pub mod operations;
pub use gents_loop::provider_input::ProviderInputProfile;
pub mod provider_budget {
    pub use crate::turn::provider_input::budget::{effective_input_budget, threshold_budget};
}
pub mod runtime;

#[cfg(any(test, feature = "test-fixtures"))]
#[doc(hidden)]
#[allow(dead_code)]
pub mod test_support;
pub use gents_protocol::request_admission::SIGNED_REQUEST_FIELDS;
pub mod turn;

pub use defra_node;
pub use gents_loop::openai_wire::OpenAiWireApi;
pub use gents_loop::truncation::{TruncationLimits, TruncationMode};
pub use gents_protocol::client_protocol;
pub use gents_protocol::request_lifecycle::{
    next_request_hop, request_hop_within_bound, EnqueuedAgentRequest, RequestHopCapObservation,
    RequestHopCause,
};
pub use gents_protocol::row::AgentRequest;
pub use runtime::agent::{
    AgentBuilder, DocumentRuntimeOptions, Gents, GentsBuilder, ProcessLifecycleObserver,
    ProcessLifecycleState,
};
pub use runtime::oneshot::{
    run_openai_oneshot_admitted, run_openai_oneshot_with_tools_admitted, OneshotRunResult,
};
pub use turn::prompt::{LayeredPromptBuilder, PromptBuilder};

#[doc(hidden)]
pub mod __test_internals {
    pub async fn park_backend(
        lifecycle: &mut gents_session::request::lifecycle::RequestLifecycle,
    ) -> anyhow::Result<bool> {
        lifecycle.park_for_backend().await
    }

    pub async fn wake_backend(
        lifecycle: &mut gents_session::request::lifecycle::RequestLifecycle,
    ) -> anyhow::Result<bool> {
        Ok(lifecycle
            .wake_for_backend(Some(chrono::Utc::now()))
            .await?
            .resumed())
    }

    pub fn backend_generation(
        lifecycle: &gents_session::request::lifecycle::RequestLifecycle,
    ) -> anyhow::Result<&str> {
        lifecycle.execution_generation()
    }

    pub fn backend_deadline(
        lifecycle: &gents_session::request::lifecycle::RequestLifecycle,
    ) -> Option<chrono::DateTime<chrono::Utc>> {
        lifecycle.claimed_deadline_at()
    }

    pub async fn settle_parked_backend(
        access: &gents_store::config_client::ConfigAccess,
        request_doc_id: &str,
        generation: &str,
        interrupted: bool,
    ) -> anyhow::Result<gents_session::request::lifecycle::TerminalizeResult> {
        gents_session::request::lifecycle::terminalize_parked_request(
            access,
            request_doc_id,
            generation,
            interrupted,
        )
        .await
    }
    pub use crate::runtime::agent::principal_assembly::AgentBuildError;
    pub use gents_protocol::request_lifecycle::EnqueuedAgentRequest;
}

#[cfg(test)]
mod automation_tests;
#[cfg(test)]
mod toolcalls_tests;

#[cfg(test)]
mod documents_config_tests;

#[cfg(test)]
mod documents_selection_tests;

#[cfg(test)]
mod pack_export_tests;


#[cfg(test)]
mod tools_tests;

#[cfg(test)]
mod session_tests;
