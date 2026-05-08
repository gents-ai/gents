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
/// pre-state. Programmer error, not a user-visible failure.
#[derive(Debug, thiserror::Error)]
#[error("illegal tool call transition: cannot {method} from state {from:?} (allowed: {allowed:?})")]
pub struct IllegalToolCallTransition {
    pub method: &'static str,
    pub from: ToolCallState,
    pub allowed: Vec<ToolCallState>,
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
            Err(anyhow!(IllegalToolCallTransition {
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
}

/// Helper to extract `_docID` from a `create_*` mutation response.
/// Patterned off `crates/defra-agent/src/lifecycle/materialize.rs`.
///
/// DefraDB returns either:
///   `{ "create_AgentToolCall": { "_docID": "..." } }`
/// or the array form:
///   `{ "create_AgentToolCall": [{ "_docID": "..." }] }`
fn extract_doc_id_from_create_response(resp: &QueryResponse) -> Option<String> {
    resp.data
        .as_ref()
        .and_then(|d| d.get("create_AgentToolCall"))
        .and_then(|value| {
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
        })
}
