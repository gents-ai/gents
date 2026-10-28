use async_trait::async_trait;
use gents_protocol::message::Message;

use crate::live_output::LiveToolOutputWriter;
use crate::tool_call_lifecycle::ToolOutcome;
use crate::{HookAction, ToolCallHookAction};

pub type ForegroundCall = Box<
    dyn FnOnce(tokio_util::sync::CancellationToken) -> crate::tool::BoxFuture<'static, ToolOutcome>
        + Send,
>;

pub enum ForegroundResult {
    Settled(ToolOutcome),
    /// The same execution continues under the durable background owner.
    Backgrounded(String),
}

#[async_trait]
pub trait CanonicalSessionHook<Accepted>: SessionHook
where
    Accepted: Send,
{
    async fn adopt_accepted_tool_calls(
        &self,
        calls: Vec<(String, Accepted)>,
    ) -> Result<(), Self::Error>;
}

#[async_trait]
pub trait SessionHook: Send + Sync {
    type Error: Into<Box<dyn std::error::Error + Send + Sync>> + Send + Sync + 'static;

    async fn run_foreground_tool(
        &self,
        _name: &str,
        _internal_id: &str,
        call: ForegroundCall,
    ) -> ForegroundResult {
        let token = crate::tool_call_lifecycle::runtime::current_tool_runtime_context()
            .map(|scope| scope.cancellation_token)
            .unwrap_or_default();
        ForegroundResult::Settled(call(token).await)
    }

    /// Bind canonical arguments and the physical parent before exposing the
    /// derived child's internal identity to dispatch hooks.
    async fn admit_tool_effect(
        &self,
        _parent_internal_id: &str,
        _ordinal: u32,
        _tool_name: &str,
        _arguments: &str,
    ) -> Result<String, String> {
        Err("tool effects require a durable session owner".to_owned())
    }

    /// Settle the exact durable child if cancellation interrupts admission.
    async fn finish_tool_effect(
        &self,
        _internal_id: &str,
        _interruption: Option<&ToolOutcome>,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Only writer-acknowledged candidates may enter the provider projection.
    async fn pending_steering_inputs(
        &self,
    ) -> Result<Vec<crate::loop_stream::FoldedPrompt>, Self::Error> {
        Ok(Vec::new())
    }

    async fn finish_steering_snapshot(
        &self,
    ) -> Result<Option<crate::loop_stream::SteeringSnapshot>, Self::Error> {
        Ok(None)
    }

    /// Persist the prompt before dispatching the provider turn.
    async fn on_completion_call_with_context(
        &self,
        prompt: &Message,
        history: &[Message],
        context: Option<&Message>,
    ) -> HookAction;

    /// Persist and decide a tool call before dispatch.
    async fn on_tool_call(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> ToolCallHookAction;

    /// Settle rejected calls from Pending with their pre-dispatch failure.
    async fn on_tool_admission_rejected(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
        outcome: &ToolOutcome,
    ) -> HookAction;

    /// Persist a tool's outcome before continuing the loop.
    async fn on_tool_result(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
        outcome: &ToolOutcome,
    ) -> HookAction;

    async fn foreground_live_output_writer(&self, internal_call_id: &str) -> LiveToolOutputWriter;

    async fn session_id(&self) -> Option<String>;

    async fn register_stream_tool_call_identity(
        &self,
        internal_call_id: &str,
        result_id: &str,
        call_id: Option<&str>,
    );
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NoopSessionHook;

#[async_trait]
impl SessionHook for NoopSessionHook {
    type Error = std::convert::Infallible;

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

    async fn on_tool_admission_rejected(
        &self,
        _tool_name: &str,
        _tool_call_id: Option<String>,
        _internal_call_id: &str,
        _args: &str,
        _outcome: &ToolOutcome,
    ) -> HookAction {
        HookAction::Continue
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

    async fn register_stream_tool_call_identity(
        &self,
        _internal_call_id: &str,
        _result_id: &str,
        _call_id: Option<&str>,
    ) {
    }
}
