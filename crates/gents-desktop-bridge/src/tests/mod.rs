mod cause_derivation;
mod file_change_review;
mod operations_interrupt;
mod operations_job_control;
mod process_control;
mod session_goal_observation;
mod session_live_tail;
mod session_provenance;
#[path = "../../../gents/tests/support/streaming_backend.rs"]
#[expect(
    dead_code,
    reason = "The shared backend also supports stream gates and counters used by other test binaries."
)]
pub mod streaming_backend;
pub mod support;
