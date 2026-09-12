//! Gents-native LLM type vocabulary.
//!
//! The tool vocabulary (`tool`), the rig conversion boundary (`rig_compat`),
//! and the loop's small mode enums (`ToolChoice`, `HookAction`,
//! `ToolCallHookAction`) moved to `gents-loop` (G-1): the owned loop needs
//! them with no DefraDB dependency. Re-exported here so `crate::llm` keeps
//! every symbol this crate's callers already use.

/// Native message family — lives in `gents-protocol` (the persisted
/// format is protocol vocabulary shared by every peer); re-exported here so
/// crate paths read `crate::llm::message::Message`.
pub use gents_protocol::message;
pub(crate) mod backend_client;
pub mod provider_stream;
pub mod responses_normalize;
pub mod rig_compat;
pub mod tool;

pub use gents_loop::{HookAction, ToolCallHookAction, ToolChoice};

// Deterministic client-side JSON arg-repair pins for the post_status tool
// shape; moved out of tests/e2e_live so that binary holds only live-gated
// tests (the file needs no backend and exercises the public ToolDyn seam).
#[cfg(test)]
mod post_status_json_repair_tests;
