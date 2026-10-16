//! The gent's owned completion loop, carved out of `gents` (G-1) so it
//! compiles for `wasm32-wasip1`: provider-input assembly, retry/retract
//! decisions, streamed-turn accumulation, tool dispatch, and message
//! threading — no socket, no TLS, no filesystem, no DefraDB, no thread spawn.
//! Durable effects are reached through three seam traits:
//! [`session_hook::SessionHook`], [`stream_writer::StreamWriter`], and
//! [`request_lifecycle::RequestLifecycleControl`]. `gents`'s native
//! `DefraSessionHook`, `DefraStreamWriter`, and `RequestLifecycle` implement
//! them and re-export every symbol here at its original `gents` path.

pub mod account_usage;
pub mod backend_provider;
pub mod claude_messages_body;
pub mod compaction;
pub mod completion_retry;
pub mod error;
pub mod execution_origin;
pub mod execution_policy;
pub mod live_output;
pub mod loop_stream;
pub mod openai_wire;
pub mod output_obligation;
pub mod prompt;
pub mod provider_activity;
pub mod provider_audit;
pub mod provider_input;
pub mod provider_limit;
pub mod provider_patches;
pub mod provider_stream;
pub mod provider_usage;
pub mod rendered_request;
pub mod request_lifecycle;
pub mod responses_normalize;
pub mod rig_compat;
pub mod session_hook;
pub mod stream_processor;
pub mod stream_writer;
pub mod tool;
pub mod tool_call_lifecycle;
pub mod tool_effects;
pub mod tool_policy;
pub mod truncation;

pub use session_hook::SessionHook;

/// The persisted message family, re-exported from `gents-protocol`.
pub use gents_protocol::message;

/// Whether the model must call a tool before answering (rig `ToolChoice`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ToolChoice {
    #[default]
    Auto,
    None,
    Required,
    Specific { function_names: Vec<String> },
}

/// Whether a hook callback continues the loop or terminates it early (rig
/// `HookAction`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookAction {
    Continue,
    Terminate { reason: String },
}

impl HookAction {
    pub fn cont() -> Self {
        Self::Continue
    }

    pub fn terminate(reason: impl Into<String>) -> Self {
        Self::Terminate {
            reason: reason.into(),
        }
    }
}

/// Whether the pre-execution tool-call hook runs, skips, or terminates (rig
/// `ToolCallHookAction`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCallHookAction {
    Continue,
    Skip { reason: String },
    Terminate { reason: String },
}

impl ToolCallHookAction {
    pub fn cont() -> Self {
        Self::Continue
    }

    pub fn skip(reason: impl Into<String>) -> Self {
        Self::Skip {
            reason: reason.into(),
        }
    }

    pub fn terminate(reason: impl Into<String>) -> Self {
        Self::Terminate {
            reason: reason.into(),
        }
    }
}

/// Shared in-crate test fixtures (mirrors `gents`'s own `test_support`).
#[cfg(test)]
pub(crate) mod test_support {
    /// Byte-identical to `gents`'s own `CORRUPT_TOOL_ARGS_589` fixture so the
    /// moved conformance tests stay meaningful.
    pub(crate) const CORRUPT_TOOL_ARGS_589: &str = "{\"raw_schema\": false, \
         \"service_id\": \"observability-mcp\", \"tool房\n</think\": \"\n<tool_call>\n\
         <function=describe_tool>\", \"raw_schema\": false, \
         \"service_id\": \"observability-mcp\", \"tool_name\": \"list_hosts\"}";
}

#[cfg(test)]
mod end_to_end_test;
