pub use gents_protocol::message;
pub(crate) mod backend_client;
pub mod provider_stream;
pub mod responses_normalize;
pub mod rig_compat;
pub mod tool;

pub use gents_loop::{HookAction, ToolCallHookAction, ToolChoice};

// Deterministic client-side JSON arg-repair pins for the post_status tool shape.
#[cfg(test)]
mod post_status_json_repair_tests;
