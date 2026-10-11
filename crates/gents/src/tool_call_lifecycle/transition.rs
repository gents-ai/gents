//! Transition methods on ToolCallLifecycle.
//!
//! Mirrors `crates/gents/src/lifecycle/transition.rs`. Each transition
//! method calls `ensure_state` at the top to assert the precondition state,
//! then performs the GraphQL mutation atomically, then updates in-memory
//! state on confirmed success.
//!
//! `ensure_state` is verified via Bucket 3 integration tests (Task 25), which
//! exercise it through every transition method's precondition path. There is
//! no standalone unit test — fabricating a stub `Arc<EmbeddedNode>` would
//! require unsafe memory tricks and the integration coverage is sufficient.

use anyhow::{anyhow, Context, Result};
use defra_node::{EmbeddedNode, QueryResponse};

use crate::graphql::{escape_graphql_string, response_has_documents};
use crate::toolset::CommandPolicyDenial;

use super::{AwaitMode, CancelCause, FailureClass, ToolCallLifecycle, ToolCallState};

async fn execute_mutation_with_retry(
    node: &EmbeddedNode,
    mutation: &str,
    operation: &'static str,
) -> Result<QueryResponse> {
    crate::config_client::ConfigAccess::write_local_response(node, operation, mutation).await
}

/// Error returned when a transition method is called from an illegal
/// pre-state.
/// Programmer error, not a user-visible failure.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum IllegalToolCallTransition {
    #[error(
        "illegal tool call transition: cannot {method} from state {from:?} (allowed: {allowed:?})"
    )]
    BadState {
        method: &'static str,
        from: ToolCallState,
        allowed: Vec<ToolCallState>,
    },
    #[error("await_mode flip rejected: tool already Background")]
    ModeAlreadyBackground,
    #[error("await_mode flip rejected: tool already Foreground")]
    ModeAlreadyForeground,
    #[error("AgentRequest parent linkage incoherent: must set both or neither parent fields")]
    ParentLinkageIncoherent,
    #[error("foreground rejected: an agent_new/agent_message row is background-only")]
    SessionMessageIsBackgroundOnly,
}

impl ToolCallLifecycle {
    async fn sync_after_lost_running_compare(&mut self, method: &'static str) -> Result<()> {
        let current =
            ToolCallLifecycle::load_by_doc_id(self.node.clone(), self.doc_id.as_deref().context("lost compare lifecycle missing physical identity")?, &self.node_did, &self.session_id, self.requester_did.as_deref())
                .await?
                .ok_or_else(|| {
                    anyhow!(
                        "{method} compare failed and AgentToolCall row disappeared for session_id={} tool_call_id={}",
                        self.session_id,
                        self.tool_call_id
                    )
                })?;

        if current.state == ToolCallState::Running {
            anyhow::bail!(
                "{method} compare failed but AgentToolCall row is still running for session_id={} tool_call_id={}",
                self.session_id,
                self.tool_call_id
            );
        }

        self.doc_id = current.doc_id;
        self.deadline_at = current.deadline_at;
        self.state = current.state;
        self.started_at = current.started_at;
        self.failure_class = current.failure_class;
        self.cancel_cause = current.cancel_cause;
        self.await_mode = current.await_mode;
        self.spawned_by_tool_call_doc_id = current.spawned_by_tool_call_doc_id;
        self.plugin_effect = current.plugin_effect;
        Ok(())
    }

    async fn sync_after_lost_mode_compare(
        &mut self,
        method: &'static str,
        target_mode: AwaitMode,
    ) -> Result<()> {
        let current =
            ToolCallLifecycle::load_by_doc_id(self.node.clone(), self.doc_id.as_deref().context("lost compare lifecycle missing physical identity")?, &self.node_did, &self.session_id, self.requester_did.as_deref())
                .await?
                .ok_or_else(|| {
                    anyhow!(
                        "{method} compare failed and AgentToolCall row disappeared for session_id={} tool_call_id={}",
                        self.session_id,
                        self.tool_call_id
                    )
                })?;

        if current.state == ToolCallState::Running && current.await_mode != target_mode {
            anyhow::bail!(
                "{method} compare failed but AgentToolCall row is still running in {:?} for session_id={} tool_call_id={}",
                current.await_mode,
                self.session_id,
                self.tool_call_id
            );
        }

        self.doc_id = current.doc_id;
        self.deadline_at = current.deadline_at;
        self.state = current.state;
        self.started_at = current.started_at;
        self.failure_class = current.failure_class;
        self.cancel_cause = current.cancel_cause;
        self.await_mode = current.await_mode;
        self.spawned_by_tool_call_doc_id = current.spawned_by_tool_call_doc_id;
        self.plugin_effect = current.plugin_effect;
        Ok(())
    }

    /// Assert that the current state is in `allowed`. Returns
    /// `IllegalToolCallTransition` otherwise.
    pub(crate) fn ensure_state(
        &self,
        allowed: &[ToolCallState],
        method: &'static str,
    ) -> Result<()> {
        if allowed.contains(&self.state) {
            Ok(())
        } else {
            Err(anyhow!(IllegalToolCallTransition::BadState {
                method,
                from: self.state,
                allowed: allowed.to_vec(),
            }))
        }
    }
}

mod cancel;
mod session_message;
pub use session_message::CausedRequestTerminal;
mod mode_policy;
mod native;

#[cfg(test)]
mod tests;
