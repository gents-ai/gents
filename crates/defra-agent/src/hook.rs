use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use defra_node::EmbeddedNode;
use rig::agent::{HookAction, PromptHook, ToolCallHookAction};
use rig::completion::message::{Message, Text, ToolResult, ToolResultContent, UserContent};
use rig::completion::{CompletionModel, CompletionResponse};
use rig::one_or_many::OneOrMany;
use tokio::sync::Mutex;

use crate::session;
use crate::truncation::{
    truncate_text, DefraSpillTruncator, TruncationLimits, TruncationMode, Truncator,
};

/// How the hook behaves when persistence fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FailurePolicy {
    /// Log the error and continue inference. Persistence is
    /// best-effort — the agent loop is not interrupted.
    FailOpen,
    /// Terminate the agent loop on persistence failure (default). Use when
    /// data loss is unacceptable — the agent must not proceed with gaps
    /// in conversation history.
    #[default]
    FailClosed,
}

/// Counters exposed for health checks / observability.
#[derive(Debug)]
pub struct HookStats {
    pub persistence_failures: u64,
    pub persistence_successes: u64,
}

struct HookCounters {
    failures: AtomicU64,
    successes: AtomicU64,
}

/// Internal state tracked across hook calls within a single agent loop.
struct SessionState {
    session_id: Option<String>,
    agent_name: String,
    sequence: u32,
    assistant_turn_sequence: Option<u32>,
    assistant_turn_saved: bool,
    initialized: bool,
}

/// Rig `PromptHook` that persists agent interactions to DefraDB.
///
/// Attach to any agent via `.with_hook(hook)` — every prompt, response,
/// tool call, and tool result gets written to the embedded DefraDB as
/// it happens. With the default `FailClosed` policy, persistence failures
/// terminate the agent loop to prevent gaps in conversation history.
///
/// # Usage
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use defra_node::EmbeddedNode;
/// use defra_agent::DefraSessionHook;
///
/// # async fn example(node: Arc<EmbeddedNode>) -> anyhow::Result<()> {
/// let hook = DefraSessionHook::new(node, "my-agent");
/// // agent.prompt("hello").with_hook(hook).await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct DefraSessionHook {
    node: Arc<EmbeddedNode>,
    agent_did: String,
    truncation_limits: TruncationLimits,
    failure_policy: FailurePolicy,
    counters: Arc<HookCounters>,
    state: Arc<Mutex<SessionState>>,
}

enum PolicyDecision {
    Continue,
    Terminate(String),
}

impl DefraSessionHook {
    /// Create a hook for a new session. The session is created lazily on
    /// the first `on_completion_call`.
    pub fn new(node: Arc<EmbeddedNode>, agent_name: &str) -> Self {
        Self::with_policy(node, agent_name, FailurePolicy::default())
    }

    /// Create a hook with an explicit failure policy.
    pub fn with_policy(
        node: Arc<EmbeddedNode>,
        agent_name: &str,
        failure_policy: FailurePolicy,
    ) -> Self {
        Self::with_identity(node, agent_name, &agent_did(agent_name), failure_policy)
    }

    pub fn with_identity(
        node: Arc<EmbeddedNode>,
        agent_name: &str,
        agent_did: &str,
        failure_policy: FailurePolicy,
    ) -> Self {
        Self {
            node,
            agent_did: agent_did.to_string(),
            truncation_limits: TruncationLimits::default(),
            failure_policy,
            counters: Arc::new(HookCounters {
                failures: AtomicU64::new(0),
                successes: AtomicU64::new(0),
            }),
            state: Arc::new(Mutex::new(SessionState {
                session_id: None,
                agent_name: agent_name.to_string(),
                sequence: 0,
                assistant_turn_sequence: None,
                assistant_turn_saved: false,
                initialized: false,
            })),
        }
    }

    /// Resume tracking for an existing session (crash recovery path).
    /// Queries the max sequence number so new messages continue the count.
    pub async fn resume(
        node: Arc<EmbeddedNode>,
        session_id: &str,
        agent_name: &str,
    ) -> anyhow::Result<Self> {
        Self::resume_with_policy(node, session_id, agent_name, FailurePolicy::default()).await
    }

    /// Resume with an explicit failure policy.
    pub async fn resume_with_policy(
        node: Arc<EmbeddedNode>,
        session_id: &str,
        agent_name: &str,
        failure_policy: FailurePolicy,
    ) -> anyhow::Result<Self> {
        Self::resume_with_identity_policy(
            node,
            session_id,
            agent_name,
            &agent_did(agent_name),
            failure_policy,
        )
        .await
    }

    pub async fn resume_with_identity_policy(
        node: Arc<EmbeddedNode>,
        session_id: &str,
        agent_name: &str,
        agent_did: &str,
        failure_policy: FailurePolicy,
    ) -> anyhow::Result<Self> {
        session::ensure_session(&node, session_id, agent_name).await?;
        let max_seq = session::max_sequence(&node, session_id).await?;

        Ok(Self {
            node,
            agent_did: agent_did.to_string(),
            truncation_limits: TruncationLimits::default(),
            failure_policy,
            counters: Arc::new(HookCounters {
                failures: AtomicU64::new(0),
                successes: AtomicU64::new(0),
            }),
            state: Arc::new(Mutex::new(SessionState {
                session_id: Some(session_id.to_string()),
                agent_name: agent_name.to_string(),
                sequence: max_seq,
                assistant_turn_sequence: None,
                assistant_turn_saved: false,
                initialized: true,
            })),
        })
    }

    /// Snapshot of persistence success/failure counts.
    pub fn stats(&self) -> HookStats {
        HookStats {
            persistence_failures: self.counters.failures.load(Ordering::Relaxed),
            persistence_successes: self.counters.successes.load(Ordering::Relaxed),
        }
    }

    fn record_success(&self) {
        self.counters.successes.fetch_add(1, Ordering::Relaxed);
    }

    fn decide_persistence_outcome(&self, context: &str, error: &anyhow::Error) -> PolicyDecision {
        self.counters.failures.fetch_add(1, Ordering::Relaxed);
        match self.failure_policy {
            FailurePolicy::FailOpen => {
                tracing::warn!(error = %error, context = %context, "persistence failed (fail-open)");
                PolicyDecision::Continue
            }
            FailurePolicy::FailClosed => {
                tracing::error!(error = %error, context = %context, "persistence failed (fail-closed) — terminating");
                PolicyDecision::Terminate(format!("persistence failed: {error}"))
            }
        }
    }

    fn on_persistence_error(&self, context: &str, error: &anyhow::Error) -> HookAction {
        match self.decide_persistence_outcome(context, error) {
            PolicyDecision::Continue => HookAction::Continue,
            PolicyDecision::Terminate(reason) => HookAction::Terminate { reason },
        }
    }

    fn on_tool_persistence_error(
        &self,
        context: &str,
        error: &anyhow::Error,
    ) -> ToolCallHookAction {
        match self.decide_persistence_outcome(context, error) {
            PolicyDecision::Continue => ToolCallHookAction::Continue,
            PolicyDecision::Terminate(reason) => ToolCallHookAction::Terminate { reason },
        }
    }

    pub async fn resume_or_create_with_policy(
        node: Arc<EmbeddedNode>,
        session_id: &str,
        agent_name: &str,
        failure_policy: FailurePolicy,
    ) -> anyhow::Result<Self> {
        Self::resume_with_policy(node, session_id, agent_name, failure_policy).await
    }

    pub async fn resume_or_create_with_identity_policy(
        node: Arc<EmbeddedNode>,
        session_id: &str,
        agent_name: &str,
        agent_did: &str,
        failure_policy: FailurePolicy,
    ) -> anyhow::Result<Self> {
        Self::resume_with_identity_policy(node, session_id, agent_name, agent_did, failure_policy)
            .await
    }

    /// Get the session ID (if the session has been created).
    pub async fn session_id(&self) -> Option<String> {
        self.state.lock().await.session_id.clone()
    }

    /// Close the session. Call this when the agent conversation ends.
    pub async fn close(&self) -> anyhow::Result<()> {
        let session_id = self.state.lock().await.session_id.clone();
        if let Some(id) = session_id {
            session::close_session(&self.node, &id).await?;
        }
        Ok(())
    }

    /// Apply the hook's failure policy to a streaming-path persistence result.
    /// FailClosed returns the error; FailOpen logs and swallows it.
    pub fn apply_persistence_policy(
        &self,
        result: anyhow::Result<()>,
        context: &str,
    ) -> anyhow::Result<()> {
        match result {
            Ok(()) => {
                self.record_success();
                Ok(())
            }
            Err(e) => match self.decide_persistence_outcome(context, &e) {
                PolicyDecision::Continue => Ok(()),
                PolicyDecision::Terminate(_) => Err(e),
            },
        }
    }

    pub async fn persist_message(&self, message: &Message) -> anyhow::Result<u32> {
        let (session_id, sequence, role) = {
            let mut state = self.state.lock().await;
            let session_id = state
                .session_id
                .clone()
                .ok_or_else(|| anyhow::anyhow!("session hook missing session id"))?;

            match message {
                Message::User { .. } => {
                    state.sequence += 1;
                    let sequence = state.sequence;
                    state.assistant_turn_sequence = None;
                    state.assistant_turn_saved = false;
                    (session_id, sequence, "user")
                }
                Message::Assistant { .. } => {
                    let sequence = match state.assistant_turn_sequence {
                        Some(sequence) if !state.assistant_turn_saved => sequence,
                        Some(sequence) => {
                            anyhow::bail!(
                                "assistant turn for sequence {} already persisted",
                                sequence
                            );
                        }
                        None => {
                            state.sequence += 1;
                            let sequence = state.sequence;
                            state.assistant_turn_sequence = Some(sequence);
                            sequence
                        }
                    };
                    state.assistant_turn_saved = true;
                    (session_id, sequence, "assistant")
                }
            }
        };

        let content = serde_json::to_string(message)?;
        session::save_message(&self.node, &session_id, sequence, role, &content).await?;
        Ok(sequence)
    }

    pub async fn persist_stream_tool_result_message(
        &self,
        tool_result: &ToolResult,
    ) -> anyhow::Result<()> {
        let session_id = self
            .state
            .lock()
            .await
            .session_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session hook missing session id"))?;

        // Look up by tool_result.id — Rig's internal call ID — which matches
        // what on_tool_call stores as the storage key.
        let tool_call_id = &tool_result.id;
        let stored_result = session::load_tool_call_result(&self.node, &session_id, tool_call_id)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(
                    error = %e,
                    tool_call_id = %tool_call_id,
                    "failed to load stored tool result, falling back to stream payload"
                );
                // Truncate the raw stream payload to avoid oversized mutations —
                // the stored result would have been truncated by on_tool_result,
                // so the fallback must not exceed what we'd normally persist.
                let raw = render_tool_result_text(tool_result);
                let (text, _, _) =
                    truncate_text(&raw, TruncationMode::Head, &self.truncation_limits);
                text
            });

        let persisted_result = ToolResult {
            id: tool_result.id.clone(),
            call_id: tool_result.call_id.clone(),
            content: OneOrMany::one(ToolResultContent::Text(Text {
                text: stored_result,
            })),
        };

        let message = Message::User {
            content: OneOrMany::one(UserContent::ToolResult(persisted_result)),
        };
        self.persist_message(&message).await?;
        Ok(())
    }

    async fn ensure_assistant_turn_sequence(&self) -> anyhow::Result<(String, u32)> {
        let mut state = self.state.lock().await;
        let session_id = state
            .session_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session hook missing session id"))?;

        let sequence = match state.assistant_turn_sequence {
            Some(sequence) => sequence,
            None => {
                state.sequence += 1;
                let sequence = state.sequence;
                state.assistant_turn_sequence = Some(sequence);
                state.assistant_turn_saved = false;
                sequence
            }
        };

        Ok((session_id, sequence))
    }
}

impl<M: CompletionModel> PromptHook<M> for DefraSessionHook {
    async fn on_completion_call(&self, prompt: &Message, _history: &[Message]) -> HookAction {
        let result: anyhow::Result<()> = async {
            let mut state = self.state.lock().await;

            if !state.initialized {
                let session_id = session::create_session(&self.node, &state.agent_name).await?;
                state.session_id = Some(session_id);
                state.initialized = true;
            }

            state.assistant_turn_sequence = None;
            state.assistant_turn_saved = false;
            drop(state);

            self.persist_message(prompt).await?;
            Ok(())
        }
        .await;

        match result {
            Ok(()) => {
                self.record_success();
                HookAction::Continue
            }
            Err(e) => self.on_persistence_error("persist user prompt", &e),
        }
    }

    async fn on_completion_response(
        &self,
        _prompt: &Message,
        response: &CompletionResponse<M::Response>,
    ) -> HookAction {
        let result: anyhow::Result<()> = async {
            let message = Message::Assistant {
                id: response.message_id.clone(),
                content: response.choice.clone(),
            };
            self.persist_message(&message).await?;
            Ok(())
        }
        .await;

        match result {
            Ok(()) => {
                self.record_success();
                HookAction::Continue
            }
            Err(e) => self.on_persistence_error("persist assistant response", &e),
        }
    }

    async fn on_tool_call(
        &self,
        tool_name: &str,
        _tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> ToolCallHookAction {
        let result: anyhow::Result<()> = async {
            let (session_id, seq) = self.ensure_assistant_turn_sequence().await?;

            // Storage key uses Rig's internal call ID — this is the same ID that
            // appears as ToolResult.id in the streaming path, ensuring consistent
            // lookup in persist_stream_tool_result_message.
            let storage_id = internal_call_id.to_string();
            session::save_tool_call(
                &self.node,
                &session_id,
                seq,
                tool_name,
                &storage_id,
                args,
                "called",
            )
            .await?;

            Ok(())
        }
        .await;

        match result {
            Ok(()) => {
                self.record_success();
                ToolCallHookAction::Continue
            }
            Err(e) => self.on_tool_persistence_error("persist tool call", &e),
        }
    }

    async fn on_tool_result(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
        result: &str,
    ) -> HookAction {
        let persist_result: anyhow::Result<()> = async {
            let (session_id, should_persist_message) = {
                let state = self.state.lock().await;
                let session_id = state
                    .session_id
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("session hook missing session id"))?;
                (session_id, state.assistant_turn_saved)
            };

            let truncator =
                DefraSpillTruncator::new(self.node.clone(), &self.agent_did, &session_id);
            let truncated = truncator
                .truncate(
                    tool_name,
                    args,
                    result,
                    truncation_mode_for(tool_name),
                    &self.truncation_limits,
                    None,
                )
                .await?;

            let storage_id = internal_call_id.to_string();
            session::complete_tool_call(
                &self.node,
                &session_id,
                &storage_id,
                &truncated.text,
                "completed",
            )
            .await?;

            if should_persist_message {
                // The persisted message must use IDs that match what the
                // model emitted in the assistant turn's ToolCall:
                //   ToolCall { id: internal_call_id, call_id: provider_call_id }
                // so that history replay pairs them correctly.
                let tool_result_message = Message::User {
                    content: OneOrMany::one(UserContent::ToolResult(ToolResult {
                        id: internal_call_id.to_string(),
                        call_id: tool_call_id,
                        content: OneOrMany::one(ToolResultContent::Text(Text {
                            text: truncated.text.clone(),
                        })),
                    })),
                };
                self.persist_message(&tool_result_message).await?;
            }

            Ok(())
        }
        .await;

        match persist_result {
            Ok(()) => {
                self.record_success();
                HookAction::Continue
            }
            Err(e) => self.on_persistence_error("persist tool result", &e),
        }
    }
}

fn agent_did(agent_name: &str) -> String {
    format!("did:defra-agent:{agent_name}")
}

fn truncation_mode_for(tool_name: &str) -> TruncationMode {
    match tool_name {
        "bash" | "shell" | "command" => TruncationMode::Tail,
        _ => TruncationMode::Head,
    }
}

fn render_tool_result_text(tool_result: &ToolResult) -> String {
    tool_result
        .content
        .iter()
        .filter_map(|content| match content {
            ToolResultContent::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::ensure_schemas;
    use rig::completion::message::{AssistantContent, Reasoning, Text, ToolCall, ToolFunction};
    use rig::completion::{CompletionError, CompletionRequest, CompletionResponse};
    use rig::streaming::StreamingCompletionResponse;
    use serde_json::json;

    #[derive(Clone, Default)]
    struct TestModel;

    #[allow(refining_impl_trait)]
    impl CompletionModel for TestModel {
        type Response = ();
        type StreamingResponse = ();
        type Client = ();

        fn make(_: &Self::Client, _: impl Into<String>) -> Self {
            Self
        }

        async fn completion(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            Err(CompletionError::ProviderError(
                "completion is unused in hook tests".to_string(),
            ))
        }

        async fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
            Err(CompletionError::ProviderError(
                "streaming is unused in hook tests".to_string(),
            ))
        }
    }

    fn user_text_message(text: &str) -> Message {
        Message::User {
            content: OneOrMany::one(UserContent::Text(Text {
                text: text.to_string(),
            })),
        }
    }

    #[tokio::test]
    async fn streaming_turn_persists_full_assistant_history_in_sequence() {
        let data_path =
            std::env::temp_dir().join(format!("agent-daemon-hook-{}", uuid::Uuid::new_v4()));
        let node = Arc::new(
            defra_node::EmbeddedNode::builder()
                .data_path(&data_path)
                .build()
                .await
                .unwrap(),
        );
        ensure_schemas(&node).await.unwrap();

        let hook = DefraSessionHook::new(node.clone(), "general");
        let user_prompt = user_text_message("Inspect /tmp/main.rs");
        assert!(matches!(
            PromptHook::<TestModel>::on_completion_call(&hook, &user_prompt, &[]).await,
            HookAction::Continue
        ));

        let tool_args = r#"{"file_path":"/tmp/main.rs"}"#;
        assert!(matches!(
            PromptHook::<TestModel>::on_tool_call(
                &hook,
                "read",
                Some("call-1".to_string()),
                "internal-1",
                tool_args,
            )
            .await,
            ToolCallHookAction::Continue
        ));

        assert!(matches!(
            PromptHook::<TestModel>::on_tool_result(
                &hook,
                "read",
                Some("call-1".to_string()),
                "internal-1",
                tool_args,
                "fn main() {}\n",
            )
            .await,
            HookAction::Continue
        ));

        let streamed_assistant_turn = Message::Assistant {
            id: None,
            content: OneOrMany::many(vec![
                AssistantContent::Reasoning(
                    Reasoning::new("Need to inspect the file first").with_id("rs_1".to_string()),
                ),
                AssistantContent::ToolCall(ToolCall {
                    id: "internal-1".to_string(),
                    call_id: Some("call-1".to_string()),
                    function: ToolFunction {
                        name: "read".to_string(),
                        arguments: json!({ "file_path": "/tmp/main.rs" }),
                    },
                    signature: None,
                    additional_params: None,
                }),
                AssistantContent::Text(Text {
                    text: "I'm reading the file now.".to_string(),
                }),
            ])
            .unwrap(),
        };
        hook.persist_message(&streamed_assistant_turn)
            .await
            .unwrap();

        hook.persist_stream_tool_result_message(&ToolResult {
            id: "internal-1".to_string(),
            call_id: Some("call-1".to_string()),
            content: OneOrMany::one(ToolResultContent::Text(Text {
                text: "ephemeral stream payload".to_string(),
            })),
        })
        .await
        .unwrap();

        hook.persist_message(&Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::Text(Text {
                text: "The file looks healthy.".to_string(),
            })),
        })
        .await
        .unwrap();

        let session_id = hook.session_id().await.expect("session id");
        let history = session::load_history(&node, &session_id).await.unwrap();
        assert_eq!(history.len(), 4);

        assert!(matches!(
            &history[0],
            Message::User { content }
                if matches!(content.first_ref(), UserContent::Text(Text { text }) if text == "Inspect /tmp/main.rs")
        ));
        assert!(matches!(
            &history[1],
            Message::Assistant { content, .. }
                if content.len() == 3
                    && matches!(content.first_ref(), AssistantContent::Reasoning(reasoning) if reasoning.id.as_deref() == Some("rs_1"))
                    && matches!(content.iter().nth(1), Some(AssistantContent::ToolCall(tool_call)) if tool_call.call_id.as_deref() == Some("call-1"))
                    && matches!(content.iter().nth(2), Some(AssistantContent::Text(Text { text })) if text == "I'm reading the file now.")
        ));
        assert!(matches!(
            &history[2],
            Message::User { content }
                if matches!(content.first_ref(), UserContent::ToolResult(tool_result)
                    if tool_result.call_id.as_deref() == Some("call-1")
                        && matches!(tool_result.content.first_ref(), ToolResultContent::Text(Text { text }) if text == "fn main() {}\n"))
        ));
        assert!(matches!(
            &history[3],
            Message::Assistant { content, .. }
                if matches!(content.first_ref(), AssistantContent::Text(Text { text }) if text == "The file looks healthy.")
        ));

        let resp = node
            .execute(&format!(
                r#"{{
                    AgentToolCall(
                        filter: {{
                            session_id: {{ _eq: "{session_id}" }},
                            tool_call_id: {{ _eq: "internal-1" }}
                        }},
                        limit: 1
                    ) {{
                        message_sequence
                        result
                        status
                    }}
                }}"#
            ))
            .await;
        assert!(
            !resp.has_errors(),
            "query tool call failed: {:?}",
            resp.errors
        );

        let row = resp
            .data
            .as_ref()
            .and_then(|data| data.get("AgentToolCall"))
            .and_then(|value| value.as_array())
            .and_then(|rows| rows.first())
            .cloned()
            .expect("tool call row");

        assert_eq!(
            row.get("message_sequence").and_then(|value| value.as_u64()),
            Some(2)
        );
        assert_eq!(
            row.get("result").and_then(|value| value.as_str()),
            Some("fn main() {}\n")
        );
        assert_eq!(
            row.get("status").and_then(|value| value.as_str()),
            Some("completed")
        );

        let _ = std::fs::remove_dir_all(&data_path);
    }
}
