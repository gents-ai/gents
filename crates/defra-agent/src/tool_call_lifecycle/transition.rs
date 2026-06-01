//! Transition methods on ToolCallLifecycle.
//!
//! Mirrors `crates/defra-agent/src/lifecycle/transition.rs`. Each transition
//! method calls `ensure_state` at the top to assert the precondition state,
//! then performs the GraphQL mutation atomically, then updates in-memory
//! state on confirmed success.
//!
//! `ensure_state` is verified via Bucket 3 integration tests (Task 25), which
//! exercise it through every transition method's precondition path. There is
//! no standalone unit test — fabricating a stub `Arc<EmbeddedNode>` would
//! require unsafe memory tricks and the integration coverage is sufficient.

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use defra_node::QueryResponse;

use crate::graphql::{escape_graphql_string, response_has_documents};
use crate::session::execute_mutation_with_retry;
use crate::toolset::CommandPolicyDenial;

use super::{
    AwaitMode, CancelCause, CancelPolicy, CascadeDispatch, CascadeIntent, ChildTerminal,
    FailureClass, ToolCallLifecycle, ToolCallState,
};

/// Error returned when a transition method is called from an illegal
/// pre-state, or when a subagent-specific guard is violated.
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
    #[error("cancel_policy flip rejected: tool already Detach")]
    PolicyAlreadyDetach,
    #[error("bridge_complete called on tool without child_request_id")]
    BridgeCompleteRequiresChildLink,
    #[error("bridge_failure called on tool without child_request_id")]
    BridgeFailureRequiresChildLink,
    #[error("bridge_cancel_cascade called on tool not in .cancelled state")]
    CascadeRequiresCancelled,
    #[error("create_subagent_request rejected: depth exceeds maxSubagentDepth")]
    SubagentDepthExceeded,
    #[error("AgentRequest parent linkage incoherent: must set both or neither parent fields")]
    ParentLinkageIncoherent,
    #[error("native complete() called on subagent-typed tool (child_request_id is set)")]
    NativeCompleteOnSubagentTool,
    #[error("native fail() called on subagent-typed tool (child_request_id is set)")]
    NativeFailOnSubagentTool,
}

impl ToolCallLifecycle {
    async fn sync_after_lost_running_compare(&mut self, method: &'static str) -> Result<()> {
        let current =
            ToolCallLifecycle::load(self.node.clone(), &self.session_id, &self.tool_call_id)
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
        self.cancel_policy = current.cancel_policy;
        self.child_request_id = current.child_request_id;
        self.unclaimed_deadline_at = current.unclaimed_deadline_at;
        Ok(())
    }

    async fn sync_after_lost_mode_compare(
        &mut self,
        method: &'static str,
        target_mode: AwaitMode,
    ) -> Result<()> {
        let current =
            ToolCallLifecycle::load(self.node.clone(), &self.session_id, &self.tool_call_id)
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
        self.cancel_policy = current.cancel_policy;
        self.child_request_id = current.child_request_id;
        self.unclaimed_deadline_at = current.unclaimed_deadline_at;
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

    fn clear_unclaimed_deadline_fragment(&self) -> &'static str {
        if self.unclaimed_deadline_at.is_some() {
            ", unclaimed_deadline_at: null"
        } else {
            ""
        }
    }

    fn resupply_unclaimed_deadline_fragment(&self) -> String {
        self.unclaimed_deadline_at
            .map(|deadline| {
                let escaped_deadline = escape_graphql_string(
                    &deadline.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                );
                format!(r#", unclaimed_deadline_at: "{escaped_deadline}""#)
            })
            .unwrap_or_default()
    }
}

fn command_denial_fields_fragment(denial: Option<&CommandPolicyDenial>) -> String {
    let Some(denial) = denial else {
        return String::new();
    };
    format!(
        r#"denial_reason: {denial_reason},
                        denied_argv: {denied_argv},
                        denied_command: {denied_command},
                        denied_argument: {denied_argument},
                        denied_subcommand: {denied_subcommand},
                        denied_prefix: {denied_prefix},
                        policy_mode: {policy_mode},
                        policy_network: {policy_network},"#,
        denial_reason = optional_string_literal(Some(denial.to_contract())),
        denied_argv = optional_string_array_literal(denial.reason.denied_argv()),
        denied_command = optional_string_literal(denial.reason.denied_command()),
        denied_argument = optional_string_literal(denial.reason.denied_argument()),
        denied_subcommand = optional_string_literal(denial.reason.denied_subcommand()),
        denied_prefix = optional_string_array_literal(denial.reason.matched_prefix()),
        policy_mode = optional_string_literal(Some(denial.policy_mode.as_str())),
        policy_network = optional_string_literal(Some(denial.policy_network.as_str())),
    )
}

fn optional_string_literal(value: Option<&str>) -> String {
    value
        .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
        .unwrap_or_else(|| "null".to_string())
}

fn optional_string_array_literal(values: Option<&[String]>) -> String {
    values
        .map(|values| {
            let values = values
                .iter()
                .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{values}]")
        })
        .unwrap_or_else(|| "null".to_string())
}

mod bridge;
mod mode_policy;
mod native;

#[cfg(test)]
mod tests;

/// Helper to extract `_docID` from a `create_*` mutation response.
/// Patterned off `crates/defra-agent/src/lifecycle/materialize.rs`.
///
/// DefraDB versions may return the key as either `"create_AgentToolCall"` or
/// `"add_AgentToolCall"` (the latter is observed at runtime). Both the scalar
/// and array forms are handled:
///   `{ "add_AgentToolCall": [{ "_docID": "..." }] }`
///   `{ "create_AgentToolCall": { "_docID": "..." } }`
fn extract_doc_id_from_create_response(resp: &QueryResponse) -> Option<String> {
    let data = resp.data.as_ref()?;
    // Try both "create_" and "add_" prefixes — DefraDB may return either.
    let value = data
        .get("create_AgentToolCall")
        .or_else(|| data.get("add_AgentToolCall"))?;
    value
        .get("_docID")
        .and_then(|doc_id| doc_id.as_str())
        .or_else(|| {
            value
                .as_array()
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("_docID"))
                .and_then(|doc_id| doc_id.as_str())
        })
        .map(|s| s.to_string())
}
