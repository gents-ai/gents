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

use crate::graphql::escape_graphql_string;
use crate::session::execute_mutation_with_retry;

use super::{ToolCallLifecycle, ToolCallState};

/// Error returned when a transition method is called from an illegal
/// pre-state, or when a subagent-specific guard is violated.
/// Programmer error, not a user-visible failure.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum IllegalToolCallTransition {
    #[error("illegal tool call transition: cannot {method} from state {from:?} (allowed: {allowed:?})")]
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

    /// Pending → Running. Creates the DefraDB row if missing; idempotent if
    /// already in Running. Sets `started_at` to `now`.
    pub async fn start_running(&mut self) -> Result<()> {
        if self.state == ToolCallState::Running {
            // Idempotent re-entry (retry path).
            return Ok(());
        }
        self.ensure_state(&[ToolCallState::Pending], "start_running")?;

        let now = Utc::now();
        let started_at_str = now.to_rfc3339();
        let escaped_session_id = escape_graphql_string(&self.session_id);
        let escaped_tool_call_id = escape_graphql_string(&self.tool_call_id);
        let escaped_tool_name = escape_graphql_string(&self.tool_name);
        let escaped_args = escape_graphql_string(&self.args);
        let tool_call_key = format!("{escaped_session_id}:{escaped_tool_call_id}");
        let message_sequence = self.message_sequence;

        let mutation = format!(
            r#"mutation {{
                create_AgentToolCall(input: {{
                    tool_call_key: "{tool_call_key}",
                    session_id: "{escaped_session_id}",
                    message_sequence: {message_sequence},
                    tool_name: "{escaped_tool_name}",
                    tool_call_id: "{escaped_tool_call_id}",
                    args: "{escaped_args}",
                    result: "",
                    status: "called",
                    lifecycle_state: "running",
                    started_at: "{started_at_str}",
                    selected_service_id: null,
                    selected_tool_name: null,
                    tool_failure_class: null,
                    latency_ms: null
                }}) {{ _docID }}
            }}"#
        );

        let resp = execute_mutation_with_retry(&self.node, &mutation, "start_running")
            .await
            .context("start_running mutation")?;

        let doc_id = extract_doc_id_from_create_response(&resp)
            .ok_or_else(|| anyhow!("create_AgentToolCall returned no _docID"))?;

        self.doc_id = Some(doc_id);
        self.state = ToolCallState::Running;
        self.started_at = Some(now);
        Ok(())
    }

    /// Running → Completed. Writes the tool result; sets completed_at,
    /// latency_ms.
    pub async fn complete(&mut self, result: &str) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running], "complete")?;
        if self.child_request_id.is_some() {
            return Err(IllegalToolCallTransition::NativeCompleteOnSubagentTool.into());
        }

        let doc_id = self
            .doc_id
            .as_ref()
            .ok_or_else(|| anyhow!("complete called before start_running persisted a row"))?;
        let now = Utc::now();
        let started_at = self
            .started_at
            .ok_or_else(|| anyhow!("complete called without started_at set"))?;
        let latency_ms = (now - started_at).num_milliseconds();

        let escaped_result = escape_graphql_string(result);
        let escaped_doc_id = escape_graphql_string(doc_id);
        let now_str = now.to_rfc3339();
        // DefraDB requires DateTime fields to be re-supplied on update to
        // avoid a type-mismatch error when re-validating the document.
        let started_at_str = started_at.to_rfc3339();

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                    input: {{
                        result: "{escaped_result}",
                        status: "completed",
                        lifecycle_state: "completed",
                        started_at: "{started_at_str}",
                        completed_at: "{now_str}",
                        latency_ms: {latency_ms}
                    }}
                ) {{ _docID }}
            }}"#
        );

        execute_mutation_with_retry(&self.node, &mutation, "complete")
            .await
            .context("complete mutation")?;

        self.state = ToolCallState::Completed;
        Ok(())
    }

    /// Running → Failed. For tool errors during execution. Sets failure_class.
    pub async fn fail(&mut self, result: &str, failure: super::FailureClass) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running], "fail")?;
        if self.child_request_id.is_some() {
            return Err(IllegalToolCallTransition::NativeFailOnSubagentTool.into());
        }

        let doc_id = self
            .doc_id
            .as_ref()
            .ok_or_else(|| anyhow!("fail called before start_running persisted a row"))?;
        let now = Utc::now();
        let started_at = self
            .started_at
            .ok_or_else(|| anyhow!("fail called without started_at set"))?;
        let latency_ms = (now - started_at).num_milliseconds();

        let escaped_result = escape_graphql_string(result);
        let escaped_doc_id = escape_graphql_string(doc_id);
        let now_str = now.to_rfc3339();
        let failure_class_str = failure.as_str();
        // DefraDB requires DateTime fields to be re-supplied on update.
        let started_at_str = started_at.to_rfc3339();

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                    input: {{
                        result: "{escaped_result}",
                        status: "completed",
                        lifecycle_state: "failed",
                        started_at: "{started_at_str}",
                        completed_at: "{now_str}",
                        tool_failure_class: "{failure_class_str}",
                        latency_ms: {latency_ms}
                    }}
                ) {{ _docID }}
            }}"#
        );

        execute_mutation_with_retry(&self.node, &mutation, "fail")
            .await
            .context("fail mutation")?;

        self.state = ToolCallState::Failed;
        self.failure_class = Some(failure);
        Ok(())
    }

    /// Pending → Failed. Used when the dispatcher cannot start the call
    /// (MCP service unreachable, argument parse failure pre-spawn).
    pub async fn spawn_failed(&mut self, failure: super::FailureClass, reason: &str) -> Result<()> {
        self.ensure_state(&[ToolCallState::Pending], "spawn_failed")?;

        // Pending means the row hasn't been created yet. We create it
        // directly in Failed state.
        let now = Utc::now();
        let started_at_str = now.to_rfc3339();
        let escaped_session_id = escape_graphql_string(&self.session_id);
        let escaped_tool_call_id = escape_graphql_string(&self.tool_call_id);
        let escaped_tool_name = escape_graphql_string(&self.tool_name);
        let escaped_args = escape_graphql_string(&self.args);
        let escaped_result = escape_graphql_string(reason);
        let tool_call_key = format!("{escaped_session_id}:{escaped_tool_call_id}");
        let message_sequence = self.message_sequence;
        let failure_class_str = failure.as_str();

        let mutation = format!(
            r#"mutation {{
                create_AgentToolCall(input: {{
                    tool_call_key: "{tool_call_key}",
                    session_id: "{escaped_session_id}",
                    message_sequence: {message_sequence},
                    tool_name: "{escaped_tool_name}",
                    tool_call_id: "{escaped_tool_call_id}",
                    args: "{escaped_args}",
                    result: "{escaped_result}",
                    status: "completed",
                    lifecycle_state: "failed",
                    started_at: null,
                    completed_at: "{started_at_str}",
                    tool_failure_class: "{failure_class_str}",
                    latency_ms: 0
                }}) {{ _docID }}
            }}"#
        );

        let resp = execute_mutation_with_retry(&self.node, &mutation, "spawn_failed")
            .await
            .context("spawn_failed mutation")?;

        let doc_id = extract_doc_id_from_create_response(&resp)
            .ok_or_else(|| anyhow!("create_AgentToolCall returned no _docID"))?;

        self.doc_id = Some(doc_id);
        self.state = ToolCallState::Failed;
        self.failure_class = Some(failure);
        Ok(())
    }

    /// Running → TimedOut. R1 does not call this from runtime code; R3 wires
    /// it up to fire on deadline expiry. Defined here so the API surface
    /// matches the Lean spec.
    pub async fn timeout(&mut self) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running], "timeout")?;

        let doc_id = self
            .doc_id
            .as_ref()
            .ok_or_else(|| anyhow!("timeout called before start_running persisted a row"))?;
        let now = Utc::now();
        let started_at = self
            .started_at
            .ok_or_else(|| anyhow!("timeout called without started_at set"))?;
        let latency_ms = (now - started_at).num_milliseconds();

        let escaped_doc_id = escape_graphql_string(doc_id);
        let now_str = now.to_rfc3339();
        // DefraDB requires DateTime fields to be re-supplied on update.
        let started_at_str = started_at.to_rfc3339();

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                    input: {{
                        status: "completed",
                        lifecycle_state: "timedOut",
                        started_at: "{started_at_str}",
                        completed_at: "{now_str}",
                        latency_ms: {latency_ms}
                    }}
                ) {{ _docID }}
            }}"#
        );

        execute_mutation_with_retry(&self.node, &mutation, "timeout")
            .await
            .context("timeout mutation")?;

        self.state = ToolCallState::TimedOut;
        Ok(())
    }

    /// Pending → Cancelled. R1 does not call from runtime code; R4 wires up.
    pub async fn cancel_before_dispatch(&mut self) -> Result<()> {
        self.ensure_state(&[ToolCallState::Pending], "cancel_before_dispatch")?;

        // Pending: row may not exist yet. Create directly in Cancelled.
        let now = Utc::now();
        let started_at_str = now.to_rfc3339();
        let escaped_session_id = escape_graphql_string(&self.session_id);
        let escaped_tool_call_id = escape_graphql_string(&self.tool_call_id);
        let escaped_tool_name = escape_graphql_string(&self.tool_name);
        let escaped_args = escape_graphql_string(&self.args);
        let tool_call_key = format!("{escaped_session_id}:{escaped_tool_call_id}");
        let message_sequence = self.message_sequence;

        let mutation = format!(
            r#"mutation {{
                create_AgentToolCall(input: {{
                    tool_call_key: "{tool_call_key}",
                    session_id: "{escaped_session_id}",
                    message_sequence: {message_sequence},
                    tool_name: "{escaped_tool_name}",
                    tool_call_id: "{escaped_tool_call_id}",
                    args: "{escaped_args}",
                    result: "",
                    status: "completed",
                    lifecycle_state: "cancelled",
                    started_at: null,
                    completed_at: "{started_at_str}",
                    latency_ms: 0
                }}) {{ _docID }}
            }}"#
        );

        let resp = execute_mutation_with_retry(&self.node, &mutation, "cancel_before_dispatch")
            .await
            .context("cancel_before_dispatch mutation")?;
        let doc_id = extract_doc_id_from_create_response(&resp)
            .ok_or_else(|| anyhow!("create_AgentToolCall returned no _docID"))?;

        self.doc_id = Some(doc_id);
        self.state = ToolCallState::Cancelled;
        Ok(())
    }

    /// Running → Cancelled. R1 does not call from runtime code; R4 wires up.
    pub async fn cancel_during_run(&mut self) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running], "cancel_during_run")?;

        let doc_id = self.doc_id.as_ref().ok_or_else(|| {
            anyhow!("cancel_during_run called before start_running persisted a row")
        })?;
        let now = Utc::now();
        let started_at = self
            .started_at
            .ok_or_else(|| anyhow!("cancel_during_run called without started_at set"))?;
        let latency_ms = (now - started_at).num_milliseconds();

        let escaped_doc_id = escape_graphql_string(doc_id);
        let now_str = now.to_rfc3339();
        // DefraDB requires DateTime fields to be re-supplied on update.
        let started_at_str = started_at.to_rfc3339();

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                    input: {{
                        status: "completed",
                        lifecycle_state: "cancelled",
                        started_at: "{started_at_str}",
                        completed_at: "{now_str}",
                        latency_ms: {latency_ms}
                    }}
                ) {{ _docID }}
            }}"#
        );

        execute_mutation_with_retry(&self.node, &mutation, "cancel_during_run")
            .await
            .context("cancel_during_run mutation")?;

        self.state = ToolCallState::Cancelled;
        Ok(())
    }
}

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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::{AwaitMode, CancelPolicy, ToolCallLifecycle, ToolCallState};
    use super::IllegalToolCallTransition;

    /// Build a minimal in-memory node. Schema setup is not required for these
    /// tests because the h_native guards fire before any DB mutation.
    async fn test_node() -> Arc<defra_node::EmbeddedNode> {
        Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap())
    }

    /// Return a subagent-typed lifecycle already in Running state.
    /// Uses the pub(crate) setters to skip `start_running` (which would
    /// require schema setup). The guard under test fires before the DB call,
    /// so no mutation ever reaches the node.
    async fn subagent_lc_in_running() -> ToolCallLifecycle {
        let node = test_node().await;
        let mut lc = ToolCallLifecycle::new_subagent(
            node,
            "session-1".to_string(),
            "tcid-1".to_string(),
            0,
            "spawn_agent".to_string(),
            "{}".to_string(),
            AwaitMode::Foreground,
            CancelPolicy::Cascade,
            "child-req-1".to_string(),
        );
        lc.set_state(ToolCallState::Running);
        lc.set_doc_id(Some("fake-doc-id".to_string()));
        lc.set_started_at(Some(chrono::Utc::now()));
        lc
    }

    #[tokio::test]
    async fn complete_rejects_subagent_typed_tool() {
        let mut lc = subagent_lc_in_running().await;
        let err = lc.complete("result").await.unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<IllegalToolCallTransition>(),
                Some(IllegalToolCallTransition::NativeCompleteOnSubagentTool)
            ),
            "expected NativeCompleteOnSubagentTool, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn fail_rejects_subagent_typed_tool() {
        use super::super::FailureClass;
        let mut lc = subagent_lc_in_running().await;
        let err = lc
            .fail("error output", FailureClass::External)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<IllegalToolCallTransition>(),
                Some(IllegalToolCallTransition::NativeFailOnSubagentTool)
            ),
            "expected NativeFailOnSubagentTool, got: {err:?}"
        );
    }
}
