//! The persistence-hook seam.
//!
//! The owned loop threads every durable effect (a persisted message, a
//! tool-call/result transition, a pre-completion prompt check) through a
//! [`SessionHook`] instead of writing DefraDB itself. `gents`'s
//! `DefraSessionHook` (native) implements it; the loop only ever calls these
//! nine methods, exactly what `agent/loop_stream.rs`, its `one_shot`
//! sub-module, and `stream_processor.rs` call on the hook today.
//!
//! `run_loop_stream` and `StreamProcessor` take `H: SessionHook` as a type
//! parameter (not `Arc<dyn SessionHook>`): every call site already holds a
//! concrete `DefraSessionHook`, so a generic keeps them unchanged. The trait
//! itself is still object-safe (`#[async_trait]` boxes every future), for a
//! caller that does need a trait object.

use async_trait::async_trait;
use gents_protocol::message::{Message, ToolResult};

use crate::live_output::LiveToolOutputWriter;
use crate::tool_call_lifecycle::ToolOutcome;
use crate::{HookAction, ToolCallHookAction};

#[async_trait]
pub trait SessionHook: Send + Sync {
    /// Called once per completion turn, before the provider is dispatched:
    /// persists the prompt (and the per-request context message on turn 1)
    /// and gives the hook a chance to terminate the loop early.
    async fn on_completion_call_with_context(
        &self,
        prompt: &Message,
        history: &[Message],
        context: Option<&Message>,
    ) -> HookAction;

    /// Called when the model emits a tool call, before dispatch: persists the
    /// call and returns whether to run it, skip it (with a reason that
    /// becomes the tool result), or terminate.
    async fn on_tool_call(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> ToolCallHookAction;

    /// Called after a tool has run (or timed out, or been cancelled):
    /// persists the outcome and returns whether to continue or terminate.
    async fn on_tool_result(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
        outcome: &ToolOutcome,
    ) -> HookAction;

    /// A live-output sink for one tool call, so a long-running tool's partial
    /// output is observable before it completes.
    async fn foreground_live_output_writer(&self, internal_call_id: &str) -> LiveToolOutputWriter;

    /// The session this hook is bound to, if any.
    async fn session_id(&self) -> Option<String>;

    /// Fail-open/fail-closed policy over one persistence attempt's result:
    /// `Ok` on success, or the hook's configured decision on failure. Sync
    /// because the decision itself touches no I/O.
    fn apply_persistence_policy(
        &self,
        result: anyhow::Result<()>,
        context: &str,
    ) -> anyhow::Result<()>;

    /// Persist one durable message, returning its assigned sequence.
    async fn persist_message(&self, message: &Message) -> anyhow::Result<u32>;

    /// Persist a streamed tool result (the `one_shot` runner's path; the
    /// streaming `StreamProcessor` calls the progress-returning sibling
    /// below instead).
    async fn persist_stream_tool_result_message(
        &self,
        tool_result: &ToolResult,
        internal_call_id: &str,
    ) -> anyhow::Result<()>;

    /// `StreamProcessor`'s sibling to `persist_stream_tool_result_message`:
    /// returns whether this call actually advanced the durable turn (a
    /// duplicate streamed result is silently `Ok(false)`), so the caller
    /// knows whether to advance the request lifecycle.
    async fn persist_stream_tool_result_progress(
        &self,
        tool_result: &ToolResult,
        internal_call_id: &str,
    ) -> anyhow::Result<bool>;

    /// Persist an assistant turn still in flight (a tool call arrived mid-turn,
    /// before the turn closes), so a crash mid-turn leaves a durable partial.
    async fn persist_inflight_assistant_turn(&self, message: &Message) -> anyhow::Result<u32>;

    /// Mark the response materialized through `sequence`, so a resumed
    /// session knows how much of the streamed turn is durable.
    async fn mark_current_response_materialized(&self, sequence: u32) -> anyhow::Result<()>;

    /// Bind a streamed tool call's internal id to its provider-assigned
    /// result id, so a later streamed result can be matched back to it.
    async fn register_stream_tool_call_identity(
        &self,
        internal_call_id: &str,
        result_id: &str,
        call_id: Option<&str>,
    );
}

/// A hook that persists nothing and never terminates the loop. Used by
/// internal, sub-loop callers (the compaction summarizer's own completion
/// request) that have no session to persist into.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopSessionHook;

#[async_trait]
impl SessionHook for NoopSessionHook {
    async fn on_completion_call_with_context(
        &self,
        _prompt: &Message,
        _history: &[Message],
        _context: Option<&Message>,
    ) -> HookAction {
        HookAction::Continue
    }

    async fn on_tool_call(
        &self,
        _tool_name: &str,
        _tool_call_id: Option<String>,
        _internal_call_id: &str,
        _args: &str,
    ) -> ToolCallHookAction {
        ToolCallHookAction::Continue
    }

    async fn on_tool_result(
        &self,
        _tool_name: &str,
        _tool_call_id: Option<String>,
        _internal_call_id: &str,
        _args: &str,
        _outcome: &ToolOutcome,
    ) -> HookAction {
        HookAction::Continue
    }

    async fn foreground_live_output_writer(&self, internal_call_id: &str) -> LiveToolOutputWriter {
        crate::live_output::LiveToolOutputRegistry::default()
            .writer_for(internal_call_id.to_string())
            .await
    }

    async fn session_id(&self) -> Option<String> {
        None
    }

    fn apply_persistence_policy(
        &self,
        result: anyhow::Result<()>,
        _context: &str,
    ) -> anyhow::Result<()> {
        result
    }

    async fn persist_message(&self, _message: &Message) -> anyhow::Result<u32> {
        Ok(0)
    }

    async fn persist_stream_tool_result_message(
        &self,
        _tool_result: &ToolResult,
        _internal_call_id: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn persist_stream_tool_result_progress(
        &self,
        _tool_result: &ToolResult,
        _internal_call_id: &str,
    ) -> anyhow::Result<bool> {
        Ok(false)
    }

    async fn persist_inflight_assistant_turn(&self, _message: &Message) -> anyhow::Result<u32> {
        Ok(0)
    }

    async fn mark_current_response_materialized(&self, _sequence: u32) -> anyhow::Result<()> {
        Ok(())
    }

    async fn register_stream_tool_call_identity(
        &self,
        _internal_call_id: &str,
        _result_id: &str,
        _call_id: Option<&str>,
    ) {
    }
}
