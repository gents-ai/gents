//! The gent's owned completion loop, carved out of `gents` (G-1) so it
//! compiles for `wasm32-wasip1`: the guest half of the H18 split (see
//! `docs/gents-cloud-v1.md` §15.0). Everything here is provider-input
//! assembly, retry/retract decisions, streamed-turn accumulation, tool
//! dispatch, and message threading - no socket, no TLS, no filesystem, no
//! DefraDB, no thread spawn. Durable effects (persisted messages, tool-call
//! transitions, streamed tokens, the request lifecycle) are reached through
//! three seam traits: [`session_hook::SessionHook`],
//! [`stream_writer::StreamWriter`], and
//! [`request_lifecycle::RequestLifecycleControl`]. `gents`'s native
//! `DefraSessionHook`, `DefraStreamWriter`, and `RequestLifecycle` implement
//! them and re-export every symbol here at its original `gents` path, so this
//! crate's extraction changes no caller.
//!
//! ## What stays in `gents`
//!
//! Anything that needs a socket, a process, a filesystem, or DefraDB itself:
//! the embedded node, the mesh, the write path, the trigger engine, the
//! health checker, the MCP pool, background tools, native (shell/file/LSP)
//! tools, the admission controller, and the workspace/artifact overlay on the
//! tool-runtime scope. `gents::tool_call_lifecycle::runtime` extends
//! [`tool_call_lifecycle::runtime`]'s task-local scope with that overlay.

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
pub mod provider_input;
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
pub mod tool_policy;
pub mod truncation;

pub use session_hook::SessionHook;

/// The persisted message family. Lives in `gents-protocol` (shared with every
/// peer); re-exported here so crate paths read `gents_loop::message::Message`
/// the way `gents` reads `crate::llm::message::Message`.
pub use gents_protocol::message;

/// Whether/how the model must call a tool before answering. Mirrors rig's
/// `ToolChoice`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ToolChoice {
    /// The model decides whether to call a tool.
    #[default]
    Auto,
    /// The model must not call a tool.
    None,
    /// The model must call some tool.
    Required,
    /// The model must call one of the named tools.
    Specific { function_names: Vec<String> },
}

/// Outcome of a hook callback for a completion/tool-result event: continue the
/// loop, or terminate it early with a reason. Mirrors rig's `HookAction`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookAction {
    /// Continue loop execution as normal.
    Continue,
    /// Terminate the loop early.
    Terminate { reason: String },
}

impl HookAction {
    /// Continue the loop.
    pub fn cont() -> Self {
        Self::Continue
    }

    /// Terminate the loop early with `reason`.
    pub fn terminate(reason: impl Into<String>) -> Self {
        Self::Terminate {
            reason: reason.into(),
        }
    }
}

/// Outcome of the pre-execution tool-call hook: run the tool, skip it (returning
/// the reason as the tool result), or terminate the loop. Mirrors rig's
/// `ToolCallHookAction`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCallHookAction {
    /// Run the tool as normal.
    Continue,
    /// Skip execution; `reason` becomes the tool result.
    Skip { reason: String },
    /// Terminate the loop early.
    Terminate { reason: String },
}

impl ToolCallHookAction {
    /// Run the tool as normal.
    pub fn cont() -> Self {
        Self::Continue
    }

    /// Skip execution; `reason` becomes the tool result.
    pub fn skip(reason: impl Into<String>) -> Self {
        Self::Skip {
            reason: reason.into(),
        }
    }

    /// Terminate the loop early with `reason`.
    pub fn terminate(reason: impl Into<String>) -> Self {
        Self::Terminate {
            reason: reason.into(),
        }
    }
}

/// Shared in-crate test fixtures (mirrors `gents`'s own `test_support`).
#[cfg(test)]
pub(crate) mod test_support {
    /// The #589 production poison: a model tool-call `arguments` string
    /// contaminated by out-of-channel tokens. Byte-identical to `gents`'s own
    /// fixture so the moved conformance tests stay meaningful.
    pub(crate) const CORRUPT_TOOL_ARGS_589: &str = "{\"raw_schema\": false, \
         \"service_id\": \"observability-mcp\", \"tool房\n</think\": \"\n<tool_call>\n\
         <function=describe_tool>\", \"raw_schema\": false, \
         \"service_id\": \"observability-mcp\", \"tool_name\": \"list_hosts\"}";
}

#[cfg(test)]
mod end_to_end_test;
