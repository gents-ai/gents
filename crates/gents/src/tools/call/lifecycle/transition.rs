use anyhow::{anyhow, Context, Result};

use crate::store::graphql::{escape_graphql_string, response_has_documents};
use crate::tools::native::CommandPolicyDenial;

use super::{AwaitMode, CancelCause, FailureClass, ToolCallLifecycle, ToolCallState, ToolRowKind};

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
    #[error("foreground rejected: a plugin job row is background-only")]
    PluginJobIsBackgroundOnly,
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
mod plugin_job;
pub(crate) use plugin_job::PluginJobTerminal;

#[cfg(test)]
mod tests;
