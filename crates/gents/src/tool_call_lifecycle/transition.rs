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
use chrono::Utc;
use defra_node::{QueryRequest, QueryResponse};
use serde::Deserialize;

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
    #[error("detach rejected: tool has no child_request_id (only a bridged subagent may detach)")]
    DetachRequiresChildLink,
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

#[derive(Debug, Deserialize)]
struct ExactToolEvidenceRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    tool_call_doc_id: String,
    tool_call_composite_commit_cid: String,
    tool_call_signer_did: String,
    agent_did: String,
}

#[derive(Debug, Deserialize)]
struct ExactToolCallStateRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    agent_did: String,
    lifecycle_state: String,
}

pub(super) enum ExactEvidenceTransitionOutcome {
    Applied(QueryResponse),
    /// Another actor already moved the physical execution out of the source
    /// phase. The caller should reload and adopt that durable state.
    Lost,
    /// The execution remained in the source phase but its exact head changed
    /// after the evidence was published. The evidence must be regenerated
    /// against the new accepted version; it cannot authorize this transition.
    Stale,
}

pub(super) struct ExactToolEvidence<'a> {
    pub(super) collection: &'static str,
    pub(super) exact: &'a crate::SignedDocumentVersionRef,
    pub(super) require_execution_owner: bool,
}

async fn exact_tool_call_state_at_cid_in_txn(
    node: &defra_node::EmbeddedNode,
    transaction: &defra_node::TransactionHandle,
    doc_id: &str,
    cid: &str,
) -> Result<ExactToolCallStateRow> {
    let query = format!(
        r#"{{ AgentToolCall(cid: ["{}"]) {{ _docID agent_did lifecycle_state }} }}"#,
        escape_graphql_string(cid),
    );
    let response = node
        .execute_request_in_txn(QueryRequest::new(query), transaction)
        .await;
    if response.has_errors() {
        anyhow::bail!(
            "loading exact AgentToolCall {doc_id} version {cid} in transaction failed: {:?}",
            response.errors
        );
    }
    let rows: Vec<ExactToolCallStateRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    match rows.as_slice() {
        [row] if row.doc_id == doc_id => Ok(ExactToolCallStateRow {
            doc_id: row.doc_id.clone(),
            agent_did: row.agent_did.clone(),
            lifecycle_state: row.lifecycle_state.clone(),
        }),
        rows => anyhow::bail!(
            "exact AgentToolCall {doc_id} version {cid} reconstructed {} rows or another document",
            rows.len()
        ),
    }
}

/// Advance one mutable execution phase only after reading the exact current
/// source version, its cryptographically verified signer, and its stamped
/// owner in the same transaction as the compare-and-set. This is the
/// evidence-free counterpart to [`execute_transition_with_exact_evidence`]
/// for admission transitions such as pending -> running/held: the accepted
/// signed source document is itself the authorization fact.
pub(super) async fn execute_transition_from_signed_source(
    node: &defra_node::EmbeddedNode,
    tool_call_doc_id: &str,
    expected_agent_did: &str,
    expected_source_state: ToolCallState,
    mutation: &str,
    mutation_field: &'static str,
    operation: &'static str,
) -> Result<ExactEvidenceTransitionOutcome> {
    crate::session::retry_operation(operation, || async {
        let transaction = node
            .runner()
            .begin_txn(false)
            .await
            .map_err(|error| anyhow!("begin {operation} transaction: {error}"))?;
        let attempt = async {
            let current =
                crate::document_version::verified_current_signed_document_version_in_txn(
                    node,
                    &transaction,
                    "AgentToolCall",
                    tool_call_doc_id,
                )
                .await?;
            let row = exact_tool_call_state_at_cid_in_txn(
                node,
                &transaction,
                tool_call_doc_id,
                &current.version.composite_commit_cid,
            )
            .await?;
            if row.lifecycle_state != expected_source_state.as_str() {
                return Ok(ExactEvidenceTransitionOutcome::Lost);
            }
            if row.agent_did != expected_agent_did {
                anyhow::bail!(
                    "AgentToolCall {tool_call_doc_id} stamped agent mismatch before {operation}: stamped={}, expected={expected_agent_did}, signer={}",
                    row.agent_did,
                    current.signer_did
                );
            }
            let response = node
                .execute_request_in_txn(QueryRequest::new(mutation), &transaction)
                .await;
            if response.has_errors() {
                anyhow::bail!("{operation} mutation failed: {:?}", response.errors);
            }
            if !response
                .data
                .as_ref()
                .and_then(|data| data.get(mutation_field))
                .is_some_and(response_has_documents)
            {
                return Ok(ExactEvidenceTransitionOutcome::Lost);
            }
            Ok(ExactEvidenceTransitionOutcome::Applied(response))
        }
        .await;
        match attempt {
            Ok(ExactEvidenceTransitionOutcome::Applied(response)) => {
                if let Err(error) = node.runner().commit_txn(&transaction).await {
                    let _ = node.runner().rollback_txn(&transaction).await;
                    anyhow::bail!("commit {operation} transaction: {error}");
                }
                Ok(ExactEvidenceTransitionOutcome::Applied(response))
            }
            Ok(outcome) => {
                node.runner()
                    .rollback_txn(&transaction)
                    .await
                    .map_err(|error| anyhow!("roll back {operation} transaction: {error}"))?;
                Ok(outcome)
            }
            Err(error) => {
                if let Err(rollback_error) = node.runner().rollback_txn(&transaction).await {
                    tracing::warn!(%rollback_error, operation, "rolling back signed-source transition failed");
                }
                Err(error)
            }
        }
    })
    .await
}

/// Verify an immutable evidence fact and its exact execution parent, then
/// perform the state transition in the same DefraDB transaction.
///
/// This closes the state-only CAS gap: an output/approval/omission for an old
/// `running` or `awaitingApproval` head cannot authorize a newer head that
/// happens to carry the same textual lifecycle state.
#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_transition_with_exact_evidence(
    node: &defra_node::EmbeddedNode,
    tool_call_doc_id: &str,
    expected_source_state: ToolCallState,
    evidence: &[ExactToolEvidence<'_>],
    mutation: &str,
    mutation_field: &'static str,
    operation: &'static str,
) -> Result<ExactEvidenceTransitionOutcome> {
    if evidence.is_empty() {
        anyhow::bail!("{operation} requires at least one exact evidence fact");
    }
    crate::session::retry_operation(operation, || async {
        let transaction = node
            .runner()
            .begin_txn(false)
            .await
            .map_err(|error| anyhow!("begin {operation} transaction: {error}"))?;
        let attempt = async {
            let current_call =
                crate::document_version::verified_current_signed_document_version_in_txn(
                    node,
                    &transaction,
                    "AgentToolCall",
                    tool_call_doc_id,
                )
                .await?;
            let current_call_row = exact_tool_call_state_at_cid_in_txn(
                node,
                &transaction,
                tool_call_doc_id,
                &current_call.version.composite_commit_cid,
            )
            .await?;
            if current_call_row.agent_did.trim().is_empty() {
                anyhow::bail!(
                    "AgentToolCall {tool_call_doc_id} has no stamped principal before {operation}"
                );
            }
            let mut stale_parent = false;
            for item in evidence {
                let verified_evidence =
                    crate::document_version::verified_current_signed_document_version_in_txn(
                        node,
                        &transaction,
                        item.collection,
                        &item.exact.version.doc_id,
                    )
                    .await?;
                if &verified_evidence != item.exact {
                    anyhow::bail!(
                        "{} evidence changed before {operation}: expected {:?}, observed {:?}",
                        item.collection,
                        item.exact,
                        verified_evidence
                    );
                }
                let evidence_query = format!(
                    r#"{{ {}(cid: ["{}"]) {{
                        _docID
                        tool_call_doc_id
                        tool_call_composite_commit_cid
                        tool_call_signer_did
                        agent_did
                    }} }}"#,
                    item.collection,
                    escape_graphql_string(&item.exact.version.composite_commit_cid),
                );
                let evidence_response = node
                    .execute_request_in_txn(QueryRequest::new(evidence_query), &transaction)
                    .await;
                if evidence_response.has_errors() {
                    anyhow::bail!(
                        "loading exact {} evidence for {operation} failed: {:?}",
                        item.collection,
                        evidence_response.errors
                    );
                }
                let evidence_rows: Vec<ExactToolEvidenceRow> = evidence_response
                    .data
                    .as_ref()
                    .and_then(|data| data.get(item.collection))
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()?
                    .unwrap_or_default();
                let evidence_row = match evidence_rows.as_slice() {
                    [row] if row.doc_id == item.exact.version.doc_id => row,
                    rows => anyhow::bail!(
                        "exact {} evidence reconstructed {} rows or another document",
                        item.collection,
                        rows.len()
                    ),
                };
                if evidence_row.tool_call_doc_id != tool_call_doc_id {
                    anyhow::bail!(
                        "{} evidence belongs to AgentToolCall {}, not {tool_call_doc_id}",
                        item.collection,
                        evidence_row.tool_call_doc_id
                    );
                }
                if evidence_row.agent_did != current_call_row.agent_did {
                    anyhow::bail!(
                        "{} evidence principal {} does not match AgentToolCall principal {} before {operation}",
                        item.collection,
                        evidence_row.agent_did,
                        current_call_row.agent_did
                    );
                }
                let evidence_parent = crate::SignedDocumentVersionRef::new(
                    crate::DocumentVersionRef::new(
                        &evidence_row.tool_call_doc_id,
                        &evidence_row.tool_call_composite_commit_cid,
                    ),
                    &evidence_row.tool_call_signer_did,
                );
                if item.require_execution_owner
                    && item.exact.signer_did != evidence_parent.signer_did
                {
                    anyhow::bail!(
                        "{} signer {} is not execution owner {}",
                        item.collection,
                        item.exact.signer_did,
                        evidence_parent.signer_did
                    );
                }
                stale_parent |= current_call != evidence_parent;
            }
            if stale_parent {
                return Ok(if current_call_row.lifecycle_state == expected_source_state.as_str() {
                    ExactEvidenceTransitionOutcome::Stale
                } else {
                    ExactEvidenceTransitionOutcome::Lost
                });
            }

            let response = node
                .execute_request_in_txn(QueryRequest::new(mutation), &transaction)
                .await;
            if response.has_errors() {
                anyhow::bail!("{operation} mutation failed: {:?}", response.errors);
            }
            if !response
                .data
                .as_ref()
                .and_then(|data| data.get(mutation_field))
                .is_some_and(response_has_documents)
            {
                return Ok(ExactEvidenceTransitionOutcome::Lost);
            }
            Ok(ExactEvidenceTransitionOutcome::Applied(response))
        }
        .await;

        match attempt {
            Ok(ExactEvidenceTransitionOutcome::Applied(response)) => {
                if let Err(error) = node.runner().commit_txn(&transaction).await {
                    let _ = node.runner().rollback_txn(&transaction).await;
                    anyhow::bail!("commit {operation} transaction: {error}");
                }
                Ok(ExactEvidenceTransitionOutcome::Applied(response))
            }
            Ok(outcome) => {
                node.runner()
                    .rollback_txn(&transaction)
                    .await
                    .map_err(|error| anyhow!("roll back {operation} transaction: {error}"))?;
                Ok(outcome)
            }
            Err(error) => {
                if let Err(rollback_error) = node.runner().rollback_txn(&transaction).await {
                    tracing::warn!(%rollback_error, operation, "rolling back exact-evidence transition failed");
                }
                Err(error)
            }
        }
    })
    .await
}

impl ToolCallLifecycle {
    async fn require_current_signed_execution(
        &self,
        doc_id: &str,
    ) -> Result<crate::SignedDocumentVersionRef> {
        crate::document_version::verified_current_signed_document_version(
            &self.node,
            "AgentToolCall",
            doc_id,
        )
        .await
    }

    /// Reload and adopt the durable execution when a competing writer moved
    /// it out of `expected_source` before this writer could even publish its
    /// terminal evidence. Evidence publication precedes the terminal CAS, so
    /// first-terminal-wins must cover this earlier race window as well.
    async fn adopt_if_source_moved(
        &mut self,
        expected_source: ToolCallState,
        method: &'static str,
    ) -> Result<bool> {
        let expected_doc_id = self.doc_id.as_deref().ok_or_else(|| {
            anyhow!("{method} evidence race cannot adopt an unpersisted AgentToolCall")
        })?;
        let current = super::query::load_exact_lifecycle_adoption(
            self.node.clone(),
            expected_doc_id,
            &self.session_id,
            &self.tool_call_id,
        )
        .await?;
        if current.state == expected_source {
            return Ok(false);
        }
        super::query::load_exact_tool_call_terminal_evidence(
            &self.node,
            &self.session_id,
            &self.tool_call_id,
        )
        .await
        .with_context(|| {
            format!(
                "{method} cannot adopt AgentToolCall {expected_doc_id} after it moved from {} without coherent terminal evidence",
                expected_source.as_str()
            )
        })?;
        self.doc_id = Some(current.doc_id);
        self.deadline_at = current.deadline_at;
        self.state = current.state;
        self.started_at = current.started_at;
        self.failure_class = current.failure_class;
        self.cancel_cause = current.cancel_cause;
        self.await_mode = current.await_mode;
        self.cancel_policy = current.cancel_policy;
        self.child_request_id = current.child_request_id;
        self.spawn_target_did = current.spawn_target_did;
        self.unclaimed_deadline_at = current.unclaimed_deadline_at;
        self.workflow_group_id = current.workflow_group_id;
        self.workflow_role = current.workflow_role;
        Ok(true)
    }

    /// Publication errors are suppressed only for a fully verified competing
    /// terminal graph. Otherwise the writer's original failure remains the
    /// primary error, augmented with the failed adoption check when present.
    async fn adopt_after_terminal_evidence_publication_error(
        &mut self,
        expected_source: ToolCallState,
        method: &'static str,
        publication_error: anyhow::Error,
    ) -> Result<()> {
        match self.adopt_if_source_moved(expected_source, method).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(publication_error),
            Err(adoption_error) => Err(publication_error.context(format!(
                "{method} cannot adopt a competing terminal execution: {adoption_error:#}"
            ))),
        }
    }

    async fn retain_terminal_omission_fact(
        &self,
        source: ToolCallState,
        terminal: ToolCallState,
        reason: super::evidence::ToolOutputOmissionReason,
        detail: &str,
    ) -> Result<crate::SignedDocumentVersionRef> {
        let doc_id = self
            .doc_id
            .as_deref()
            .ok_or_else(|| anyhow!("terminal omission requires a persisted AgentToolCall"))?;
        super::evidence::retain_tool_output_omission(
            &self.node, doc_id, source, terminal, reason, detail,
        )
        .await
    }

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

        let current_doc_id = current
            .doc_id
            .as_deref()
            .ok_or_else(|| anyhow!("{method} current AgentToolCall has no _docID"))?;
        self.require_current_signed_execution(current_doc_id)
            .await?;

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

        let current_doc_id = current
            .doc_id
            .as_deref()
            .ok_or_else(|| anyhow!("{method} current AgentToolCall has no _docID"))?;
        self.require_current_signed_execution(current_doc_id)
            .await?;

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

pub(super) fn exact_result_fields_fragment(result: &crate::SignedDocumentVersionRef) -> String {
    format!(
        r#"result_doc_id: "{}",
                        result_composite_commit_cid: "{}",
                        result_signer_did: "{}","#,
        escape_graphql_string(&result.version.doc_id),
        escape_graphql_string(&result.version.composite_commit_cid),
        escape_graphql_string(&result.signer_did),
    )
}

pub(super) fn exact_omission_fields_fragment(omission: &crate::SignedDocumentVersionRef) -> String {
    format!(
        r#"omission_doc_id: "{}",
                        omission_composite_commit_cid: "{}",
                        omission_signer_did: "{}","#,
        escape_graphql_string(&omission.version.doc_id),
        escape_graphql_string(&omission.version.composite_commit_cid),
        escape_graphql_string(&omission.signer_did),
    )
}

fn exact_approval_fields_fragment(approval: &crate::SignedDocumentVersionRef) -> String {
    format!(
        r#"approval_doc_id: "{}",
                        approval_composite_commit_cid: "{}",
                        approval_signer_did: "{}","#,
        escape_graphql_string(&approval.version.doc_id),
        escape_graphql_string(&approval.version.composite_commit_cid),
        escape_graphql_string(&approval.signer_did),
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
/// Patterned off `crates/gents/src/lifecycle/materialize.rs`.
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
