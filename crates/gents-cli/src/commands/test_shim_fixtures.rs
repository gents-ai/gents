mod agent_config;
mod jobs;
mod tool_admission;
mod tool_completion;
mod transcript;

#[path = "../../../gents/tests/support/streaming_backend.rs"]
#[expect(
    dead_code,
    reason = "The shared backend also supports stream gates and counters used by other test binaries."
)]
pub(crate) mod streaming_backend;

#[cfg(unix)]
pub(crate) use agent_config::configure_runtime_agent;
#[cfg(unix)]
pub(super) use agent_config::seed_test_agent_configuration;
pub(crate) use jobs::seed_job_row;
pub(crate) use tool_admission::seed_canonical_tool_call;
#[cfg(unix)]
pub(super) use tool_completion::{
    complete_canonical_spawned_process_output, complete_canonical_tool_call,
    seed_canonical_live_tool_output,
};
pub(super) use transcript::next_request_sequence;
#[cfg(unix)]
pub(super) use transcript::seed_canonical_assistant_message;
