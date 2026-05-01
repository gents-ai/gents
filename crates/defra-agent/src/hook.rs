use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use defra_node::EmbeddedNode;
use rig::agent::{HookAction, ToolCallHookAction};
use tokio::sync::Mutex;

use crate::session;
use crate::truncation::TruncationLimits;

mod persistence;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FailurePolicy {
    FailOpen,
    #[default]
    FailClosed,
}

#[derive(Debug)]
pub struct HookStats {
    pub persistence_failures: u64,
    pub persistence_successes: u64,
}

struct HookCounters {
    failures: AtomicU64,
    successes: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TranscriptTurnState {
    Idle,
    AssistantBuilding { sequence: u32 },
    AssistantPersisted { sequence: u32 },
}

struct SessionState {
    session_id: Option<String>,
    current_request_id: Option<String>,
    agent_name: String,
    sequence: u32,
    transcript_turn: TranscriptTurnState,
    persisted_tool_result_ids: HashSet<String>,
    initialized: bool,
}

impl SessionState {
    fn reset_after_user_message(&mut self) {
        self.transcript_turn = TranscriptTurnState::Idle;
    }

    fn begin_or_continue_assistant_turn(&mut self) -> u32 {
        match self.transcript_turn {
            TranscriptTurnState::AssistantBuilding { sequence } => sequence,
            TranscriptTurnState::Idle | TranscriptTurnState::AssistantPersisted { .. } => {
                self.sequence += 1;
                let sequence = self.sequence;
                self.transcript_turn = TranscriptTurnState::AssistantBuilding { sequence };
                sequence
            }
        }
    }

    fn persist_assistant_turn(&mut self) -> anyhow::Result<u32> {
        let sequence = match self.transcript_turn {
            TranscriptTurnState::AssistantBuilding { sequence } => sequence,
            TranscriptTurnState::Idle => {
                self.sequence += 1;
                self.sequence
            }
            TranscriptTurnState::AssistantPersisted { sequence } => {
                anyhow::bail!("assistant turn for sequence {sequence} already persisted");
            }
        };
        self.transcript_turn = TranscriptTurnState::AssistantPersisted { sequence };
        Ok(sequence)
    }

    fn mark_tool_result_seen_for_persisted_turn(&mut self, internal_call_id: &str) -> bool {
        matches!(
            self.transcript_turn,
            TranscriptTurnState::AssistantPersisted { .. }
        ) && self
            .persisted_tool_result_ids
            .insert(internal_call_id.to_string())
    }

    fn mark_stream_tool_result_seen(&mut self, internal_call_id: &str) -> anyhow::Result<bool> {
        if !matches!(
            self.transcript_turn,
            TranscriptTurnState::AssistantPersisted { .. }
        ) {
            anyhow::bail!(
                "cannot persist streamed tool result before its assistant turn is persisted"
            );
        }
        Ok(self
            .persisted_tool_result_ids
            .insert(internal_call_id.to_string()))
    }
}

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
                current_request_id: None,
                agent_name: agent_name.to_string(),
                sequence: 0,
                transcript_turn: TranscriptTurnState::Idle,
                persisted_tool_result_ids: HashSet::new(),
                initialized: false,
            })),
        }
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
                current_request_id: None,
                agent_name: agent_name.to_string(),
                sequence: max_seq,
                transcript_turn: TranscriptTurnState::Idle,
                persisted_tool_result_ids: HashSet::new(),
                initialized: true,
            })),
        })
    }

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

    pub async fn session_id(&self) -> Option<String> {
        self.state.lock().await.session_id.clone()
    }

    pub async fn set_active_request_id(&self, request_id: Option<String>) {
        self.state.lock().await.current_request_id = request_id;
    }

    pub async fn mark_current_response_materialized(&self, sequence: u32) -> anyhow::Result<()> {
        let request_id = self.state.lock().await.current_request_id.clone();
        let Some(request_id) = request_id.as_deref() else {
            return Ok(());
        };
        session::mark_response_materialized(&self.node, request_id, sequence).await
    }

    pub async fn close(&self) -> anyhow::Result<()> {
        let session_id = self.state.lock().await.session_id.clone();
        if let Some(id) = session_id {
            session::close_session(&self.node, &id).await?;
        }
        Ok(())
    }

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
}
