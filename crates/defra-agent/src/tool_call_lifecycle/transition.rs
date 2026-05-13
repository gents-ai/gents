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

use super::{AwaitMode, CancelPolicy, FailureClass, ToolCallLifecycle, ToolCallState};

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
        self.await_mode = current.await_mode;
        self.cancel_policy = current.cancel_policy;
        self.child_request_id = current.child_request_id;
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
        self.await_mode = current.await_mode;
        self.cancel_policy = current.cancel_policy;
        self.child_request_id = current.child_request_id;
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
        let deadline_at_str = self.deadline_at.to_rfc3339();
        let escaped_request_id = escape_graphql_string(&self.request_id);
        let escaped_session_id = escape_graphql_string(&self.session_id);
        let escaped_tool_call_id = escape_graphql_string(&self.tool_call_id);
        let escaped_tool_name = escape_graphql_string(&self.tool_name);
        let escaped_args = escape_graphql_string(&self.args);
        let tool_call_key = format!("{escaped_session_id}:{escaped_tool_call_id}");
        let message_sequence = self.message_sequence;

        // Persist subagent-specific fields when this tool has a child link.
        // These are optional schema fields so we emit them only when set.
        let subagent_fields = if let Some(ref crid) = self.child_request_id {
            let escaped_crid = escape_graphql_string(crid);
            let await_mode_str = self.await_mode.as_str();
            let cancel_policy_str = self.cancel_policy.as_str();
            format!(
                r#"child_request_id: "{escaped_crid}",
                    await_mode: "{await_mode_str}",
                    cancel_policy: "{cancel_policy_str}","#
            )
        } else {
            String::new()
        };

        let mutation = format!(
            r#"mutation {{
                create_AgentToolCall(input: {{
                    tool_call_key: "{tool_call_key}",
                    request_id: "{escaped_request_id}",
                    session_id: "{escaped_session_id}",
                    message_sequence: {message_sequence},
                    tool_name: "{escaped_tool_name}",
                    tool_call_id: "{escaped_tool_call_id}",
                    args: "{escaped_args}",
                    result: "",
                    status: "called",
                    lifecycle_state: "running",
                    started_at: "{started_at_str}",
                    deadline_at: "{deadline_at_str}",
                    {subagent_fields}
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
        let deadline_at_str = self.deadline_at.to_rfc3339();

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                    input: {{
                        result: "{escaped_result}",
                        status: "completed",
                        lifecycle_state: "completed",
                        started_at: "{started_at_str}",
                        deadline_at: "{deadline_at_str}",
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
        let deadline_at_str = self.deadline_at.to_rfc3339();

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                    input: {{
                        result: "{escaped_result}",
                        status: "completed",
                        lifecycle_state: "failed",
                        started_at: "{started_at_str}",
                        deadline_at: "{deadline_at_str}",
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
        let deadline_at_str = self.deadline_at.to_rfc3339();
        let escaped_request_id = escape_graphql_string(&self.request_id);
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
                    request_id: "{escaped_request_id}",
                    session_id: "{escaped_session_id}",
                    message_sequence: {message_sequence},
                    tool_name: "{escaped_tool_name}",
                    tool_call_id: "{escaped_tool_call_id}",
                    args: "{escaped_args}",
                    result: "{escaped_result}",
                    status: "completed",
                    lifecycle_state: "failed",
                    started_at: null,
                    deadline_at: "{deadline_at_str}",
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

    /// Running → Completed for bridge (subagent) tools.
    ///
    /// Lean parity: bridge_complete. Parent tool .running → .completed when
    /// the caller has verified the linked child request reached .completed.
    /// Persists child_result as the row's `result` field; sets state,
    /// completed_at, latency_ms following R1's complete() persistence pattern.
    ///
    /// Trust boundary: bridge_complete does NOT verify the child's terminal
    /// state internally (Lean's precondition is on the caller). R3's
    /// SubagentSource will be the natural place for that check.
    pub async fn bridge_complete(&mut self, child_result: String) -> Result<bool> {
        self.ensure_state(&[ToolCallState::Running], "bridge_complete")?;
        if self.child_request_id.is_none() {
            return Err(IllegalToolCallTransition::BridgeCompleteRequiresChildLink.into());
        }

        let doc_id = self.doc_id.as_ref().ok_or_else(|| {
            anyhow!("bridge_complete called before start_running persisted a row")
        })?;
        let now = Utc::now();
        let started_at = self
            .started_at
            .ok_or_else(|| anyhow!("bridge_complete called without started_at set"))?;
        let latency_ms = (now - started_at).num_milliseconds();

        let escaped_result = escape_graphql_string(&child_result);
        let escaped_doc_id = escape_graphql_string(doc_id);
        let now_str = now.to_rfc3339();
        // DefraDB requires DateTime fields to be re-supplied on update to
        // avoid a type-mismatch error when re-validating the document.
        let started_at_str = started_at.to_rfc3339();
        let deadline_at_str = self.deadline_at.to_rfc3339();

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        lifecycle_state: {{ _eq: "running" }}
                    }},
                    input: {{
                        result: "{escaped_result}",
                        status: "completed",
                        lifecycle_state: "completed",
                        started_at: "{started_at_str}",
                        deadline_at: "{deadline_at_str}",
                        completed_at: "{now_str}",
                        latency_ms: {latency_ms}
                    }}
                ) {{ _docID }}
            }}"#
        );

        let response = execute_mutation_with_retry(&self.node, &mutation, "bridge_complete")
            .await
            .context("bridge_complete mutation")?;
        if !response
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentToolCall"))
            .is_some_and(response_has_documents)
        {
            self.sync_after_lost_running_compare("bridge_complete")
                .await?;
            return Ok(false);
        }

        self.state = ToolCallState::Completed;
        Ok(true)
    }

    /// Running → Failed (or Cancelled for ChildTerminal::Interrupted).
    ///
    /// Lean parity: bridge_failure. Parent tool .running → .failed (or
    /// .cancelled for ChildTerminal::Interrupted). Projection per
    /// ChildTerminal::projected_state(). Persists lifecycle_state,
    /// completed_at, latency_ms; conditionally persists tool_failure_class
    /// and result when the child reached .failed.
    ///
    /// Returns BridgeFailureRequiresChildLink for native tools (no
    /// child_request_id).
    pub async fn bridge_failure(&mut self, child_terminal: super::ChildTerminal) -> Result<bool> {
        self.ensure_state(&[ToolCallState::Running], "bridge_failure")?;
        if self.child_request_id.is_none() {
            return Err(IllegalToolCallTransition::BridgeFailureRequiresChildLink.into());
        }

        let projected = child_terminal.projected_state();
        let (failure_class_for_persist, reason_for_persist) = match &child_terminal {
            super::ChildTerminal::Failed {
                reason,
                failure_class,
            } => (Some(*failure_class), Some(reason.clone())),
            _ => (None, None),
        };

        let doc_id = self
            .doc_id
            .as_ref()
            .ok_or_else(|| anyhow!("bridge_failure called before start_running persisted a row"))?;
        let now = Utc::now();
        let started_at = self
            .started_at
            .ok_or_else(|| anyhow!("bridge_failure called without started_at set"))?;
        let latency_ms = (now - started_at).num_milliseconds();

        let escaped_doc_id = escape_graphql_string(doc_id);
        let now_str = now.to_rfc3339();
        // DefraDB requires DateTime fields to be re-supplied on update.
        let started_at_str = started_at.to_rfc3339();
        let deadline_at_str = self.deadline_at.to_rfc3339();
        let lifecycle_state_str = projected.as_str();

        // Build conditional fields: tool_failure_class and result are only
        // set when the child reached .failed (mirrors R1's fail() pattern).
        let optional_fields = match (failure_class_for_persist, reason_for_persist.as_deref()) {
            (Some(fc), Some(reason)) => {
                let escaped_reason = escape_graphql_string(reason);
                let fc_str = fc.as_str();
                format!(
                    r#"result: "{escaped_reason}",
                        tool_failure_class: "{fc_str}","#
                )
            }
            _ => String::new(),
        };

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        lifecycle_state: {{ _eq: "running" }}
                    }},
                    input: {{
                        {optional_fields}
                        status: "completed",
                        lifecycle_state: "{lifecycle_state_str}",
                        started_at: "{started_at_str}",
                        deadline_at: "{deadline_at_str}",
                        completed_at: "{now_str}",
                        latency_ms: {latency_ms}
                    }}
                ) {{ _docID }}
            }}"#
        );

        let response = execute_mutation_with_retry(&self.node, &mutation, "bridge_failure")
            .await
            .context("bridge_failure mutation")?;
        if !response
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentToolCall"))
            .is_some_and(response_has_documents)
        {
            self.sync_after_lost_running_compare("bridge_failure")
                .await?;
            return Ok(false);
        }

        self.state = projected;
        self.failure_class = failure_class_for_persist;
        Ok(true)
    }

    /// Running → TimedOut. Called by the runtime deadline wrapper and startup
    /// recovery when a running tool call exceeds its effective deadline.
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
        let escaped_result = escape_graphql_string(&format!(
            "tool call deadline exceeded at {}",
            self.deadline_at.to_rfc3339()
        ));
        let now_str = now.to_rfc3339();
        // DefraDB requires DateTime fields to be re-supplied on update.
        let started_at_str = started_at.to_rfc3339();
        let deadline_at_str = self.deadline_at.to_rfc3339();
        let failure_class = FailureClass::External.as_str();

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                    input: {{
                        result: "{escaped_result}",
                        status: "completed",
                        lifecycle_state: "timedOut",
                        tool_failure_class: "{failure_class}",
                        started_at: "{started_at_str}",
                        deadline_at: "{deadline_at_str}",
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
        self.failure_class = Some(FailureClass::External);
        Ok(())
    }

    /// Pending → Cancelled. Used when a tool call is cancelled before
    /// dispatch creates a running row.
    pub async fn cancel_before_dispatch(&mut self) -> Result<()> {
        self.ensure_state(&[ToolCallState::Pending], "cancel_before_dispatch")?;

        // Pending: row may not exist yet. Create directly in Cancelled.
        let now = Utc::now();
        let started_at_str = now.to_rfc3339();
        let deadline_at_str = self.deadline_at.to_rfc3339();
        let escaped_request_id = escape_graphql_string(&self.request_id);
        let escaped_session_id = escape_graphql_string(&self.session_id);
        let escaped_tool_call_id = escape_graphql_string(&self.tool_call_id);
        let escaped_tool_name = escape_graphql_string(&self.tool_name);
        let escaped_args = escape_graphql_string(&self.args);
        let tool_call_key = format!("{escaped_session_id}:{escaped_tool_call_id}");
        let message_sequence = self.message_sequence;

        let escaped_result = escape_graphql_string("tool call cancelled before dispatch");

        let mutation = format!(
            r#"mutation {{
                create_AgentToolCall(input: {{
                    tool_call_key: "{tool_call_key}",
                    request_id: "{escaped_request_id}",
                    session_id: "{escaped_session_id}",
                    message_sequence: {message_sequence},
                    tool_name: "{escaped_tool_name}",
                    tool_call_id: "{escaped_tool_call_id}",
                    args: "{escaped_args}",
                    result: "{escaped_result}",
                    status: "completed",
                    lifecycle_state: "cancelled",
                    started_at: null,
                    deadline_at: "{deadline_at_str}",
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

    /// Running mode-flip: await_mode Foreground → Background.
    ///
    /// Lean parity: ToolCallContext.Transition.background.
    /// Requires Running state. Returns `ModeAlreadyBackground` if already in
    /// Background mode. Persists the new await_mode to the row, then updates
    /// the in-memory field on success.
    pub async fn background(&mut self) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running], "background")?;
        if self.await_mode == AwaitMode::Background {
            return Err(IllegalToolCallTransition::ModeAlreadyBackground.into());
        }

        let doc_id = self
            .doc_id
            .as_ref()
            .ok_or_else(|| anyhow!("background called before start_running persisted a row"))?;
        // DefraDB requires DateTime fields to be re-supplied on update to
        // avoid a type-mismatch error when re-validating the document.
        let started_at = self
            .started_at
            .ok_or_else(|| anyhow!("background called without started_at set"))?;
        let started_at_str = started_at.to_rfc3339();
        let deadline_at_str = self.deadline_at.to_rfc3339();

        let escaped_doc_id = escape_graphql_string(doc_id);

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        lifecycle_state: {{ _eq: "running" }},
                        await_mode: {{ _eq: "foreground" }}
                    }},
                    input: {{ await_mode: "background", started_at: "{started_at_str}", deadline_at: "{deadline_at_str}" }}
                ) {{ _docID }}
            }}"#
        );

        let response = execute_mutation_with_retry(&self.node, &mutation, "background")
            .await
            .context("background mutation")?;
        if !response
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentToolCall"))
            .is_some_and(response_has_documents)
        {
            self.sync_after_lost_mode_compare("background", AwaitMode::Background)
                .await?;
            return Ok(());
        }

        self.await_mode = AwaitMode::Background;
        Ok(())
    }

    /// Running mode-flip: await_mode Background → Foreground.
    ///
    /// Lean parity: ToolCallContext.Transition.foreground.
    /// Requires Running state. Returns `ModeAlreadyForeground` if already in
    /// Foreground mode. Persists the new await_mode to the row, then updates
    /// the in-memory field on success.
    pub async fn foreground(&mut self) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running], "foreground")?;
        if self.await_mode == AwaitMode::Foreground {
            return Err(IllegalToolCallTransition::ModeAlreadyForeground.into());
        }

        let doc_id = self
            .doc_id
            .as_ref()
            .ok_or_else(|| anyhow!("foreground called before start_running persisted a row"))?;
        // DefraDB requires DateTime fields to be re-supplied on update to
        // avoid a type-mismatch error when re-validating the document.
        let started_at = self
            .started_at
            .ok_or_else(|| anyhow!("foreground called without started_at set"))?;
        let started_at_str = started_at.to_rfc3339();
        let deadline_at_str = self.deadline_at.to_rfc3339();

        let escaped_doc_id = escape_graphql_string(doc_id);

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        lifecycle_state: {{ _eq: "running" }},
                        await_mode: {{ _eq: "background" }}
                    }},
                    input: {{ await_mode: "foreground", started_at: "{started_at_str}", deadline_at: "{deadline_at_str}" }}
                ) {{ _docID }}
            }}"#
        );

        let response = execute_mutation_with_retry(&self.node, &mutation, "foreground")
            .await
            .context("foreground mutation")?;
        if !response
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentToolCall"))
            .is_some_and(response_has_documents)
        {
            self.sync_after_lost_mode_compare("foreground", AwaitMode::Foreground)
                .await?;
            return Ok(());
        }

        self.await_mode = AwaitMode::Foreground;
        Ok(())
    }

    /// Pending|Running policy-flip: cancel_policy Cascade → Detach.
    ///
    /// Lean parity: ToolCallContext.Transition.detach. Allowed in both Pending
    /// and Running states (h_live : pre.state = .pending ∨ pre.state = .running).
    /// Returns `PolicyAlreadyDetach` if already in Detach policy. One-way — no
    /// inverse method (matches Lean's structural irreversibility).
    pub async fn detach(&mut self) -> Result<()> {
        self.ensure_state(&[ToolCallState::Pending, ToolCallState::Running], "detach")?;
        if self.cancel_policy == CancelPolicy::Detach {
            return Err(IllegalToolCallTransition::PolicyAlreadyDetach.into());
        }

        let doc_id = self
            .doc_id
            .as_ref()
            .ok_or_else(|| anyhow!("doc_id must be set before policy-flip"))?;
        // DefraDB requires DateTime fields to be re-supplied on update to
        // avoid a type-mismatch error when re-validating the document.
        // started_at is only set once the row is in Running state; for Pending
        // state the row has not been created yet so this field will be absent.
        let started_at_fragment = if let Some(started_at) = self.started_at {
            format!(", started_at: \"{}\"", started_at.to_rfc3339())
        } else {
            String::new()
        };
        let deadline_at_str = self.deadline_at.to_rfc3339();

        let escaped_doc_id = escape_graphql_string(doc_id);

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                    input: {{ cancel_policy: "detach", deadline_at: "{deadline_at_str}"{started_at_fragment} }}
                ) {{ _docID }}
            }}"#
        );

        execute_mutation_with_retry(&self.node, &mutation, "detach")
            .await
            .context("detach mutation")?;

        self.cancel_policy = CancelPolicy::Detach;
        Ok(())
    }

    /// Lean parity: bridge_cancel_cascade. Pure — returns the action that should
    /// be taken on the child AgentRequest. Caller (typically R3's daemon
    /// interrupt dispatcher) performs the actual write to set
    /// interrupt_requested_at on the child. Returns None for native tools,
    /// detached subagents, or non-cancelled bridge tools.
    pub async fn bridge_cancel_cascade(&self) -> Result<Option<super::CascadeIntent>> {
        if self.state != ToolCallState::Cancelled {
            return Err(IllegalToolCallTransition::CascadeRequiresCancelled.into());
        }
        if self.cancel_policy != CancelPolicy::Cascade {
            return Ok(None); // detached: no cascade
        }
        let Some(child_request_id) = self.child_request_id.clone() else {
            return Ok(None); // native: no bridge edge
        };
        Ok(Some(super::CascadeIntent {
            child_request_id,
            at: chrono::Utc::now(),
        }))
    }

    /// Running → Cancelled. Called by request interruption handling and
    /// startup recovery for interrupted parent requests.
    ///
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
        let escaped_result = escape_graphql_string("tool call cancelled");
        let now_str = now.to_rfc3339();
        // DefraDB requires DateTime fields to be re-supplied on update.
        let started_at_str = started_at.to_rfc3339();
        let deadline_at_str = self.deadline_at.to_rfc3339();

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        lifecycle_state: {{ _eq: "running" }}
                    }},
                    input: {{
                        result: "{escaped_result}",
                        status: "completed",
                        lifecycle_state: "cancelled",
                        started_at: "{started_at_str}",
                        deadline_at: "{deadline_at_str}",
                        completed_at: "{now_str}",
                        latency_ms: {latency_ms}
                    }}
                ) {{ _docID }}
            }}"#
        );

        let response = execute_mutation_with_retry(&self.node, &mutation, "cancel_during_run")
            .await
            .context("cancel_during_run mutation")?;
        if !response
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentToolCall"))
            .is_some_and(response_has_documents)
        {
            self.sync_after_lost_running_compare("cancel_during_run")
                .await?;
            return Ok(());
        }

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

    fn test_deadline() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now() + chrono::Duration::minutes(5)
    }

    /// Return a subagent-typed lifecycle already in Running state.
    /// Uses the pub(crate) setters to skip `start_running` (which would
    /// require schema setup). The guard under test fires before the DB call,
    /// so no mutation ever reaches the node.
    async fn subagent_lc_in_running() -> ToolCallLifecycle {
        let node = test_node().await;
        let mut lc = ToolCallLifecycle::new_subagent(
            node,
            "request-1".to_string(),
            "session-1".to_string(),
            "tcid-1".to_string(),
            0,
            "spawn_agent".to_string(),
            "{}".to_string(),
            test_deadline(),
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

    #[tokio::test]
    async fn background_rejects_already_background() {
        let node = test_node().await;
        let mut lc = ToolCallLifecycle::new_subagent(
            node,
            "req-bg-2".to_string(),
            "sess-bg-2".to_string(),
            "tc-bg-2".to_string(),
            1,
            "spawn_subagent".to_string(),
            "{}".to_string(),
            test_deadline(),
            AwaitMode::Background, // start already in Background
            CancelPolicy::Cascade,
            "child-req-bg-2".to_string(),
        );
        lc.set_state(ToolCallState::Running);
        lc.set_doc_id(Some("fake-doc-id-bg-2".to_string()));
        lc.set_started_at(Some(chrono::Utc::now()));

        let err = lc.background().await.unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<IllegalToolCallTransition>(),
                Some(IllegalToolCallTransition::ModeAlreadyBackground)
            ),
            "expected ModeAlreadyBackground, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn background_rejects_pending_state() {
        let node = test_node().await;
        let mut lc = ToolCallLifecycle::new(
            node,
            "req-bg-3".to_string(),
            "sess-bg-3".to_string(),
            "tc-bg-3".to_string(),
            1,
            "spawn_subagent".to_string(),
            "{}".to_string(),
            test_deadline(),
        );
        // state is Pending (default); do not advance it

        let err = lc.background().await.unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<IllegalToolCallTransition>(),
                Some(IllegalToolCallTransition::BadState { .. })
            ),
            "expected BadState, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn foreground_rejects_already_foreground() {
        let node = test_node().await;
        let mut lc = ToolCallLifecycle::new_subagent(
            node,
            "req-fg-1".to_string(),
            "sess-fg-1".to_string(),
            "tc-fg-1".to_string(),
            1,
            "spawn_subagent".to_string(),
            "{}".to_string(),
            test_deadline(),
            AwaitMode::Foreground, // start already in Foreground
            CancelPolicy::Cascade,
            "child-req-fg-1".to_string(),
        );
        lc.set_state(ToolCallState::Running);
        lc.set_doc_id(Some("fake-doc-id-fg-1".to_string()));
        lc.set_started_at(Some(chrono::Utc::now()));

        let err = lc.foreground().await.unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<IllegalToolCallTransition>(),
                Some(IllegalToolCallTransition::ModeAlreadyForeground)
            ),
            "expected ModeAlreadyForeground, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn foreground_rejects_pending_state() {
        let node = test_node().await;
        let mut lc = ToolCallLifecycle::new(
            node,
            "req-fg-2".to_string(),
            "sess-fg-2".to_string(),
            "tc-fg-2".to_string(),
            1,
            "spawn_subagent".to_string(),
            "{}".to_string(),
            test_deadline(),
        );
        // state is Pending (default); do not advance it

        let err = lc.foreground().await.unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<IllegalToolCallTransition>(),
                Some(IllegalToolCallTransition::BadState { .. })
            ),
            "expected BadState, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn detach_rejects_already_detach() {
        let node = test_node().await;
        let mut lc = ToolCallLifecycle::new_subagent(
            node,
            "req-detach-1".to_string(),
            "sess-detach-1".to_string(),
            "tc-detach-1".to_string(),
            1,
            "spawn_subagent".to_string(),
            "{}".to_string(),
            test_deadline(),
            AwaitMode::Foreground,
            CancelPolicy::Detach, // already in Detach policy
            "child-req-detach-1".to_string(),
        );
        lc.set_state(ToolCallState::Running);
        lc.set_doc_id(Some("fake-doc-id-detach-1".to_string()));
        lc.set_started_at(Some(chrono::Utc::now()));

        let err = lc.detach().await.unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<IllegalToolCallTransition>(),
                Some(IllegalToolCallTransition::PolicyAlreadyDetach)
            ),
            "expected PolicyAlreadyDetach, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn detach_rejects_terminal_state() {
        let node = test_node().await;
        let mut lc = ToolCallLifecycle::new_subagent(
            node,
            "req-detach-2".to_string(),
            "sess-detach-2".to_string(),
            "tc-detach-2".to_string(),
            1,
            "spawn_subagent".to_string(),
            "{}".to_string(),
            test_deadline(),
            AwaitMode::Foreground,
            CancelPolicy::Cascade,
            "child-req-detach-2".to_string(),
        );
        lc.set_state(ToolCallState::Cancelled); // terminal state
        lc.set_doc_id(Some("fake-doc-id-detach-2".to_string()));
        lc.set_started_at(Some(chrono::Utc::now()));

        let err = lc.detach().await.unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<IllegalToolCallTransition>(),
                Some(IllegalToolCallTransition::BadState { .. })
            ),
            "expected BadState, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn bridge_failure_rejects_native_tool() {
        // Native tool: constructed with new() — no child_request_id.
        let node = test_node().await;
        let mut lc = ToolCallLifecycle::new(
            node,
            "req-bf-1".to_string(),
            "sess-bf-1".to_string(),
            "tc-bf-1".to_string(),
            0,
            "native_tool".to_string(),
            "{}".to_string(),
            test_deadline(),
        );
        lc.set_state(ToolCallState::Running);
        lc.set_doc_id(Some("fake-doc-id-bf-1".to_string()));
        lc.set_started_at(Some(chrono::Utc::now()));

        let err = lc
            .bridge_failure(super::super::ChildTerminal::Dead)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<IllegalToolCallTransition>(),
                Some(IllegalToolCallTransition::BridgeFailureRequiresChildLink)
            ),
            "expected BridgeFailureRequiresChildLink, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn bridge_failure_rejects_pending_state() {
        // Subagent tool, but never advanced to Running.
        let node = test_node().await;
        let lc_base = ToolCallLifecycle::new_subagent(
            node,
            "req-bf-2".to_string(),
            "sess-bf-2".to_string(),
            "tc-bf-2".to_string(),
            0,
            "spawn_agent".to_string(),
            "{}".to_string(),
            test_deadline(),
            AwaitMode::Foreground,
            CancelPolicy::Cascade,
            "child-1".to_string(),
        );
        // Leave state as Pending (default); do not call start_running.
        let mut lc = lc_base;

        let err = lc
            .bridge_failure(super::super::ChildTerminal::Dead)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<IllegalToolCallTransition>(),
                Some(IllegalToolCallTransition::BadState { .. })
            ),
            "expected BadState, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn bridge_failure_projected_state_interrupted_is_cancelled() {
        use super::super::ChildTerminal;
        assert_eq!(
            ChildTerminal::Interrupted.projected_state(),
            ToolCallState::Cancelled
        );
    }

    #[tokio::test]
    async fn bridge_failure_projected_state_failed_is_failed() {
        use super::super::{ChildTerminal, FailureClass};
        assert_eq!(
            ChildTerminal::Failed {
                reason: "error".to_string(),
                failure_class: FailureClass::External,
            }
            .projected_state(),
            ToolCallState::Failed
        );
        assert_eq!(ChildTerminal::Dead.projected_state(), ToolCallState::Failed);
        assert_eq!(
            ChildTerminal::Superseded.projected_state(),
            ToolCallState::Failed
        );
    }

    #[tokio::test]
    async fn bridge_complete_rejects_native_tool() {
        // Native tool: constructed with new() — no child_request_id.
        let node = test_node().await;
        let mut lc = ToolCallLifecycle::new(
            node,
            "req-bc-1".to_string(),
            "sess-bc-1".to_string(),
            "tc-bc-1".to_string(),
            0,
            "native_tool".to_string(),
            "{}".to_string(),
            test_deadline(),
        );
        lc.set_state(ToolCallState::Running);
        lc.set_doc_id(Some("fake-doc-id-bc-1".to_string()));
        lc.set_started_at(Some(chrono::Utc::now()));

        let err = lc.bridge_complete("x".to_string()).await.unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<IllegalToolCallTransition>(),
                Some(IllegalToolCallTransition::BridgeCompleteRequiresChildLink)
            ),
            "expected BridgeCompleteRequiresChildLink, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn bridge_complete_rejects_pending_state() {
        // Subagent tool, but never advanced to Running.
        let node = test_node().await;
        let lc_base = ToolCallLifecycle::new_subagent(
            node,
            "req-bc-2".to_string(),
            "sess-bc-2".to_string(),
            "tc-bc-2".to_string(),
            0,
            "spawn_agent".to_string(),
            "{}".to_string(),
            test_deadline(),
            AwaitMode::Foreground,
            CancelPolicy::Cascade,
            "child-1".to_string(),
        );
        // Leave state as Pending (default); do not call start_running.
        let mut lc = lc_base;

        let err = lc.bridge_complete("x".to_string()).await.unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<IllegalToolCallTransition>(),
                Some(IllegalToolCallTransition::BadState { .. })
            ),
            "expected BadState, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn bridge_cancel_cascade_returns_intent_for_cascade_subagent() {
        let node = test_node().await;
        let mut lc = ToolCallLifecycle::new_subagent(
            node,
            "req-cas-1".to_string(),
            "sess-cas-1".to_string(),
            "tc-cas-1".to_string(),
            0,
            "spawn_agent".to_string(),
            "{}".to_string(),
            test_deadline(),
            AwaitMode::Foreground,
            CancelPolicy::Cascade,
            "child-cas-1".to_string(),
        );
        lc.set_state(ToolCallState::Cancelled);

        let intent = lc.bridge_cancel_cascade().await.unwrap();
        let intent = intent.expect("should return Some(CascadeIntent)");
        assert_eq!(intent.child_request_id, "child-cas-1");
    }

    #[tokio::test]
    async fn bridge_cancel_cascade_returns_none_for_detached() {
        let node = test_node().await;
        let mut lc = ToolCallLifecycle::new_subagent(
            node,
            "req-cas-2".to_string(),
            "sess-cas-2".to_string(),
            "tc-cas-2".to_string(),
            0,
            "spawn_agent".to_string(),
            "{}".to_string(),
            test_deadline(),
            AwaitMode::Foreground,
            CancelPolicy::Detach,
            "child-cas-2".to_string(),
        );
        lc.set_state(ToolCallState::Cancelled);

        let intent = lc.bridge_cancel_cascade().await.unwrap();
        assert!(intent.is_none(), "Detach policy returns None");
    }

    #[tokio::test]
    async fn bridge_cancel_cascade_returns_none_for_native() {
        let node = test_node().await;
        let mut lc = ToolCallLifecycle::new(
            node,
            "req-cas-3".to_string(),
            "sess-cas-3".to_string(),
            "tc-cas-3".to_string(),
            0,
            "native_tool".to_string(),
            "{}".to_string(),
            test_deadline(),
        );
        lc.set_state(ToolCallState::Cancelled);

        let intent = lc.bridge_cancel_cascade().await.unwrap();
        assert!(
            intent.is_none(),
            "Native tool (no child_request_id) returns None"
        );
    }

    #[tokio::test]
    async fn bridge_cancel_cascade_rejects_non_cancelled_state() {
        let node = test_node().await;
        let mut lc = ToolCallLifecycle::new_subagent(
            node,
            "req-cas-4".to_string(),
            "sess-cas-4".to_string(),
            "tc-cas-4".to_string(),
            0,
            "spawn_agent".to_string(),
            "{}".to_string(),
            test_deadline(),
            AwaitMode::Foreground,
            CancelPolicy::Cascade,
            "child-cas-4".to_string(),
        );
        lc.set_state(ToolCallState::Running);

        let err = lc.bridge_cancel_cascade().await.unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<IllegalToolCallTransition>(),
                Some(IllegalToolCallTransition::CascadeRequiresCancelled)
            ),
            "expected CascadeRequiresCancelled, got: {err:?}"
        );
    }
}
