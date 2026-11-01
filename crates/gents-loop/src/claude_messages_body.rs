//! The Claude Messages request a Gents completion becomes: the planner that
//! types the Claude Code request, the history codec, the image limits and the
//! reasoning replay selection.

mod history_params;
mod images;
mod messages;
mod plan;
mod replay;

use gents_claude::metadata::MetadataError;
use gents_claude::wire::WireError;
use serde_json::Value;

use rig::completion::CompletionRequest;

pub(crate) use history_params::stamp_history_params;
pub use history_params::{PREVIOUS_REQUEST_ID_PARAM, RAW_TOOL_ARGUMENTS_BY_CALL_PARAM};
pub use messages::{assistant_blocks, encode_assistant_content, AssistantCodecError};
pub use plan::{
    plan_claude_request, plan_claude_request_for, plan_with_ids, ClaudeAccountContext,
    ClaudeAgentIdentity, ClaudeRequestContext, ClaudeRequestIds, ClaudeRequestPlan, ClaudeTarget,
    CLAUDE_REQUEST_PARAM,
};
pub use replay::*;

#[doc(hidden)]
pub const ADVERTISED_REASONING_EFFORTS_PARAM: &str = "_gents_advertised_reasoning_efforts";

/// Request parameter (never sent) moving the message cache breakpoint to the
/// message before the final one, as Claude Code does for a call that must not
/// write the cache. Requests that share everything but their final
/// instruction (a guided summary attempt and its strict JSON fallback) then
/// share a cache entry at that boundary.
pub const CACHE_PREFIX_BREAKPOINT_PARAM: &str = "_gents_cache_prefix_breakpoint";

/// Request parameter (never sent) naming the session a request belongs to, so
/// account rotation keeps one session on one account.
pub const SESSION_KEY_PARAM: &str = "_gents_session_key";

pub const CLAUDE_CODE_IDENTITY: &str = gents_claude::system::CLI_IDENTITY;

/// Why a Messages request body could not be built.
#[derive(Debug, thiserror::Error)]
pub enum MessagesBodyError {
    #[error(transparent)]
    Codec(#[from] AssistantCodecError),
    /// The request asks for structured output the body cannot carry. Sending
    /// it unguided would silently drop the contract.
    #[error("structured output requested but its schema cannot be sent: {0}")]
    UnsendableOutputSchema(String),
    /// The request context parameter is not a Claude request context.
    #[error("the Claude request context is invalid: {0}")]
    Context(String),
    /// The account identity cannot form the request metadata.
    #[error("the Claude account identity is invalid: {0}")]
    Metadata(#[from] MetadataError),
    /// The request body cannot be serialized.
    #[error(transparent)]
    Wire(#[from] WireError),
    /// A tool schema or call input cannot be serialized.
    #[error("a tool definition cannot be encoded as JSON: {0}")]
    Tool(#[from] serde_json::Error),
}

/// The request body as JSON, for token estimation and the replay frontier.
///
/// The account is the fixed projection: metadata, cache lifetime and betas are
/// outside the replayed prefix, so every account projects the same prefix.
pub fn build_messages_body(
    model: &str,
    request: &CompletionRequest,
) -> Result<Value, MessagesBodyError> {
    let plan = plan_claude_request(model, request, &ClaudeAccountContext::projection())?;
    Ok(serde_json::to_value(&plan.body)?)
}

#[cfg(test)]
#[path = "claude_messages_output_tests.rs"]
mod output_tests;
