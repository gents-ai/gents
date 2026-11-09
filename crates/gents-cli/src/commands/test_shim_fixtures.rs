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
pub(crate) use jobs::seed_job_row;
