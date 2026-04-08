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

struct SessionState {
    session_id: Option<String>,
    agent_name: String,
    sequence: u32,
    assistant_turn_sequence: Option<u32>,
    assistant_turn_saved: bool,
    initialized: bool,
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
    pub fn new(node: Arc<EmbeddedNode>, agent_name: &str) -> Self {
        Self::with_policy(node, agent_name, FailurePolicy::default())
    }

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

    pub async fn resume(
        node: Arc<EmbeddedNode>,
        session_id: &str,
        agent_name: &str,
    ) -> anyhow::Result<Self> {
        Self::resume_with_policy(node, session_id, agent_name, FailurePolicy::default()).await
    }

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

    pub async fn session_id(&self) -> Option<String> {
        self.state.lock().await.session_id.clone()
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

fn agent_did(agent_name: &str) -> String {
    format!("did:defra-agent:{agent_name}")
}
