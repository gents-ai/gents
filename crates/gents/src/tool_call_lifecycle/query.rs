//! Read-only queries for tool-call lifecycle reconstruction.

use std::sync::Arc;

use anyhow::{anyhow, Context as _, Result};
use defra_node::{EmbeddedNode, ExecuteRetryPolicy, QueryRequest};
use identity::Did;
use serde::Deserialize;

use crate::graphql::escape_graphql_string;

use super::{AwaitMode, CancelCause, CancelPolicy, FailureClass, ToolCallLifecycle, ToolCallState};

#[derive(Debug, Clone, Deserialize)]
struct ToolCallResultRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    tool_call_key: String,
    session_id: String,
    tool_call_id: String,
    tool_name: String,
    lifecycle_state: String,
    result_doc_id: Option<String>,
    result_composite_commit_cid: Option<String>,
    result_signer_did: Option<String>,
    omission_doc_id: Option<String>,
    omission_composite_commit_cid: Option<String>,
    omission_signer_did: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactToolCallOutput {
    pub(crate) tool_name: String,
    pub(crate) output_text: String,
    pub(crate) evidence: crate::SignedDocumentVersionRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactToolCallOmission {
    pub(crate) tool_name: String,
    pub(crate) terminal_phase: String,
    pub(crate) reason: String,
    pub(crate) detail: String,
    pub(crate) evidence: crate::SignedDocumentVersionRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExactToolCallTerminalEvidence {
    Output(ExactToolCallOutput),
    Omission(ExactToolCallOmission),
}

/// Verified immutable terminal evidence exposed to non-runtime consumers such
/// as CLI exports. The exact DefraDB document version remains attached so a
/// projection cannot silently degrade back to the mutable tool-call preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DurableToolCallTerminalEvidence {
    Output {
        tool_name: String,
        output_text: String,
        evidence: crate::SignedDocumentVersionRef,
    },
    Omission {
        tool_name: String,
        terminal_phase: String,
        reason: String,
        detail: String,
        evidence: crate::SignedDocumentVersionRef,
    },
}

#[derive(Debug, Deserialize)]
struct ExactToolResultRow {
    result_key: String,
    tool_call_key: String,
    tool_call_doc_id: String,
    tool_call_composite_commit_cid: String,
    tool_call_signer_did: String,
    session_id: String,
    output_text: String,
}

#[derive(Debug, Deserialize)]
struct ExactOmissionSummaryRow {
    omission_key: String,
    tool_call_key: String,
    tool_call_doc_id: String,
    tool_call_composite_commit_cid: String,
    tool_call_signer_did: String,
    session_id: String,
    source_phase: String,
    terminal_phase: String,
    reason: String,
    detail: String,
}

#[derive(Debug, Deserialize)]
struct ExactHistoricalToolCallRow {
    tool_call_key: String,
    session_id: String,
    tool_call_id: String,
    lifecycle_state: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SubagentReceiptAuthorityRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    tool_call_key: String,
    session_id: String,
    tool_call_id: String,
    tool_name: String,
    lifecycle_state: String,
    await_mode: Option<String>,
    child_request_id: Option<String>,
}

fn complete_exact_edge<'a>(
    doc_id: Option<&'a str>,
    cid: Option<&'a str>,
    signer: Option<&'a str>,
    label: &str,
) -> Result<Option<(&'a str, &'a str, &'a str)>> {
    match (doc_id, cid, signer) {
        (None, None, None) => Ok(None),
        (Some(doc_id), Some(cid), Some(signer))
            if !doc_id.trim().is_empty() && !cid.trim().is_empty() && !signer.trim().is_empty() =>
        {
            Ok(Some((doc_id, cid, signer)))
        }
        _ => anyhow::bail!("AgentToolCall {label} exact reference is partial or empty"),
    }
}

pub(crate) fn resolve_exact_tool_call_match<T>(
    session_id: &str,
    tool_call_id: &str,
    rows: Vec<T>,
    doc_id: impl Fn(&T) -> &str,
    tool_call_key: impl Fn(&T) -> &str,
    row_session_id: impl Fn(&T) -> &str,
    row_tool_call_id: impl Fn(&T) -> &str,
) -> Result<Option<T>> {
    let expected_key = format!("{session_id}:{tool_call_id}");
    let row = crate::session::resolve_exact_logical_match(
        "AgentToolCall",
        "tool_call_key",
        &expected_key,
        rows,
        &doc_id,
    )?;
    if let Some(row) = row.as_ref() {
        let row_doc_id = doc_id(row);
        if tool_call_key(row) != expected_key {
            anyhow::bail!(
                "AgentToolCall logical key mismatch: queried tool_call_key={expected_key} but _docID={row_doc_id} returned tool_call_key={}",
                tool_call_key(row)
            );
        }
        if row_session_id(row) != session_id || row_tool_call_id(row) != tool_call_id {
            anyhow::bail!(
                "AgentToolCall immutable identity mismatch for tool_call_key={expected_key}: _docID={row_doc_id} returned session_id={} tool_call_id={}",
                row_session_id(row),
                row_tool_call_id(row)
            );
        }
    }
    Ok(row)
}

/// Load and verify the immutable terminal evidence for a tool call, together
/// with the tool name from the exact terminal authority document.
///
/// A terminal call must bind exactly one immutable output or omission fact.
/// Keeping the two cases explicit prevents consumers from treating the legacy
/// mutable `AgentToolCall.result` projection as terminal truth.
async fn load_exact_tool_call_terminal_evidence_with_executor(
    executor: &(impl crate::GraphqlExecutor + ?Sized),
    session_id: &str,
    tool_call_id: &str,
) -> Result<ExactToolCallTerminalEvidence> {
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_tool_call_id = escape_graphql_string(tool_call_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }},
                    tool_call_id: {{ _eq: "{escaped_tool_call_id}" }}
                }}
            ) {{
                _docID
                tool_call_key
                session_id
                tool_call_id
                tool_name
                lifecycle_state
                result_doc_id
                result_composite_commit_cid
                result_signer_did
                omission_doc_id
                omission_composite_commit_cid
                omission_signer_did
            }}
        }}"#
    );

    let resp = executor.execute_graphql(&query).await?;
    if resp.has_errors() {
        anyhow::bail!(
            "loading tool call result for session_id={} tool_call_id={}: {:?}",
            session_id,
            tool_call_id,
            resp.errors
        );
    }

    let rows: Vec<ToolCallResultRow> = match resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
    {
        Some(value) => serde_json::from_value(value.clone())?,
        None => Vec::new(),
    };

    let row = resolve_exact_tool_call_match(
        session_id,
        tool_call_id,
        rows,
        |row| row.doc_id.as_str(),
        |row| row.tool_call_key.as_str(),
        |row| row.session_id.as_str(),
        |row| row.tool_call_id.as_str(),
    )?
    .ok_or_else(|| {
        anyhow::anyhow!(
            "loading tool call result: no AgentToolCall for session_id={session_id} tool_call_id={tool_call_id}"
        )
    })?;
    let call_version =
        crate::document_version::verified_current_signed_document_version_with_executor(
            executor,
            "AgentToolCall",
            &row.doc_id,
        )
        .await?;
    let exact_call = crate::document_version::verified_exact_document_snapshot_with_executor(
        executor,
        "AgentToolCall",
        &call_version.version,
        "tool_call_key session_id tool_call_id tool_name lifecycle_state result_doc_id result_composite_commit_cid result_signer_did omission_doc_id omission_composite_commit_cid omission_signer_did",
    )
    .await?;
    if exact_call.source.signer_did != call_version.signer_did {
        anyhow::bail!("AgentToolCall current version signer changed during exact reload");
    }
    let exact_call: ToolCallResultRow = exact_call.decode()?;
    if exact_call.doc_id != row.doc_id
        || exact_call.tool_call_key != format!("{session_id}:{tool_call_id}")
        || exact_call.session_id != session_id
        || exact_call.tool_call_id != tool_call_id
        || exact_call.tool_name != row.tool_name
        || exact_call.tool_name.trim().is_empty()
    {
        anyhow::bail!("exact AgentToolCall output authority changed immutable identity");
    }
    if !matches!(
        exact_call.lifecycle_state.as_str(),
        "completed" | "failed" | "timedOut" | "cancelled"
    ) {
        anyhow::bail!(
            "AgentToolCall {session_id}:{tool_call_id} is not terminal; refusing projected result"
        );
    }
    let result = complete_exact_edge(
        exact_call.result_doc_id.as_deref(),
        exact_call.result_composite_commit_cid.as_deref(),
        exact_call.result_signer_did.as_deref(),
        "result",
    )?;
    let omission = complete_exact_edge(
        exact_call.omission_doc_id.as_deref(),
        exact_call.omission_composite_commit_cid.as_deref(),
        exact_call.omission_signer_did.as_deref(),
        "omission",
    )?;
    let (Some((result_doc_id, result_cid, result_signer)), None) = (result, omission) else {
        if let (None, Some((omission_doc_id, omission_cid, omission_signer))) = (result, omission) {
            let version = crate::DocumentVersionRef::new(omission_doc_id, omission_cid);
            let snapshot = crate::document_version::verified_exact_document_snapshot_with_executor(
                executor,
                "AgentToolOutputOmission",
                &version,
                "omission_key tool_call_key tool_call_doc_id tool_call_composite_commit_cid tool_call_signer_did session_id source_phase terminal_phase reason detail",
            )
            .await?;
            if snapshot.source.signer_did != omission_signer {
                anyhow::bail!("AgentToolCall omission signer does not match exact omission fact");
            }
            let omission: ExactOmissionSummaryRow = snapshot.decode()?;
            if snapshot.source.signer_did != omission.tool_call_signer_did {
                anyhow::bail!("AgentToolOutputOmission signer is not the pinned execution owner");
            }
            if omission.tool_call_doc_id != exact_call.doc_id
                || omission.tool_call_key != exact_call.tool_call_key
                || omission.session_id != session_id
                || omission.omission_key != omission.tool_call_composite_commit_cid
                || omission.terminal_phase != exact_call.lifecycle_state
            {
                anyhow::bail!("AgentToolOutputOmission does not bind this exact AgentToolCall");
            }
            let parent_version = crate::DocumentVersionRef::new(
                &omission.tool_call_doc_id,
                &omission.tool_call_composite_commit_cid,
            );
            let parent = crate::document_version::verified_exact_document_snapshot_with_executor(
                executor,
                "AgentToolCall",
                &parent_version,
                "tool_call_key session_id tool_call_id lifecycle_state",
            )
            .await?;
            if parent.source.signer_did != omission.tool_call_signer_did {
                anyhow::bail!(
                    "AgentToolOutputOmission historical parent signer does not match pinned edge"
                );
            }
            let parent: ExactHistoricalToolCallRow = parent.decode()?;
            if parent.tool_call_key != exact_call.tool_call_key
                || parent.session_id != session_id
                || parent.tool_call_id != tool_call_id
                || parent.lifecycle_state != omission.source_phase
            {
                anyhow::bail!("AgentToolOutputOmission historical execution parent is incoherent");
            }
            let source_phase = ToolCallState::from_persisted(&omission.source_phase)
                .ok_or_else(|| anyhow!("AgentToolOutputOmission has unknown source phase"))?;
            let terminal_phase = ToolCallState::from_persisted(&omission.terminal_phase)
                .ok_or_else(|| anyhow!("AgentToolOutputOmission has unknown terminal phase"))?;
            let reason =
                super::evidence::ToolOutputOmissionReason::from_persisted(&omission.reason)
                    .ok_or_else(|| {
                        anyhow!("AgentToolOutputOmission has unknown omission reason")
                    })?;
            if !reason.allows(source_phase, terminal_phase) {
                anyhow::bail!(
                    "AgentToolOutputOmission reason does not permit its source and terminal phases"
                );
            }
            return Ok(ExactToolCallTerminalEvidence::Omission(
                ExactToolCallOmission {
                    tool_name: exact_call.tool_name,
                    terminal_phase: omission.terminal_phase,
                    reason: omission.reason,
                    detail: omission.detail,
                    evidence: snapshot.source,
                },
            ));
        }
        match (result, omission) {
            (None, None) => anyhow::bail!(
                "terminal AgentToolCall {session_id}:{tool_call_id} binds neither result nor omission"
            ),
            (Some(_), Some(_)) => anyhow::bail!(
                "terminal AgentToolCall {session_id}:{tool_call_id} binds both result and omission"
            ),
            _ => unreachable!("single exact terminal edge was handled above"),
        }
    };
    let result_version = crate::DocumentVersionRef::new(result_doc_id, result_cid);
    let result_snapshot = crate::document_version::verified_exact_document_snapshot_with_executor(
        executor,
        "AgentToolResult",
        &result_version,
        "result_key tool_call_key tool_call_doc_id tool_call_composite_commit_cid tool_call_signer_did session_id output_text",
    )
    .await?;
    if result_snapshot.source.signer_did != result_signer {
        anyhow::bail!("AgentToolCall result signer does not match exact result fact");
    }
    let result: ExactToolResultRow = result_snapshot.decode()?;
    if result_snapshot.source.signer_did != result.tool_call_signer_did {
        anyhow::bail!("AgentToolResult signer is not the pinned execution owner");
    }
    if result.tool_call_doc_id != exact_call.doc_id
        || result.tool_call_key != exact_call.tool_call_key
        || result.session_id != session_id
        || result.result_key != result.tool_call_composite_commit_cid
    {
        anyhow::bail!("AgentToolResult immutable binding does not match exact AgentToolCall");
    }
    let parent_version = crate::DocumentVersionRef::new(
        &result.tool_call_doc_id,
        &result.tool_call_composite_commit_cid,
    );
    let parent = crate::document_version::verified_exact_document_snapshot_with_executor(
        executor,
        "AgentToolCall",
        &parent_version,
        "tool_call_key session_id tool_call_id lifecycle_state",
    )
    .await?;
    if parent.source.signer_did != result.tool_call_signer_did {
        anyhow::bail!("AgentToolResult historical parent signer does not match pinned edge");
    }
    let parent: ExactHistoricalToolCallRow = parent.decode()?;
    if parent.tool_call_key != exact_call.tool_call_key
        || parent.session_id != session_id
        || parent.tool_call_id != tool_call_id
        || parent.lifecycle_state != "running"
    {
        anyhow::bail!("AgentToolResult historical execution parent is incoherent");
    }
    Ok(ExactToolCallTerminalEvidence::Output(ExactToolCallOutput {
        tool_name: exact_call.tool_name,
        output_text: result.output_text,
        evidence: result_snapshot.source,
    }))
}

/// Load exact terminal output or explicit omission through either an embedded
/// node or authenticated remote GraphQL executor.
pub async fn load_durable_tool_call_terminal_evidence(
    executor: &(impl crate::GraphqlExecutor + ?Sized),
    session_id: &str,
    tool_call_id: &str,
) -> Result<DurableToolCallTerminalEvidence> {
    Ok(
        match load_exact_tool_call_terminal_evidence_with_executor(
            executor,
            session_id,
            tool_call_id,
        )
        .await?
        {
            ExactToolCallTerminalEvidence::Output(output) => {
                DurableToolCallTerminalEvidence::Output {
                    tool_name: output.tool_name,
                    output_text: output.output_text,
                    evidence: output.evidence,
                }
            }
            ExactToolCallTerminalEvidence::Omission(omission) => {
                DurableToolCallTerminalEvidence::Omission {
                    tool_name: omission.tool_name,
                    terminal_phase: omission.terminal_phase,
                    reason: omission.reason,
                    detail: omission.detail,
                    evidence: omission.evidence,
                }
            }
        },
    )
}

pub(crate) async fn load_exact_tool_call_terminal_evidence(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> Result<ExactToolCallTerminalEvidence> {
    load_exact_tool_call_terminal_evidence_with_executor(node, session_id, tool_call_id).await
}

/// Load and verify the immutable output fact for a tool call. An exact
/// omission is a valid terminal fact, but it is not output and is returned as
/// an error to output-only consumers.
pub(crate) async fn load_exact_tool_call_output(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> Result<ExactToolCallOutput> {
    match load_exact_tool_call_terminal_evidence(node, session_id, tool_call_id).await? {
        ExactToolCallTerminalEvidence::Output(output) => Ok(output),
        ExactToolCallTerminalEvidence::Omission(omission) => anyhow::bail!(
            "AgentToolCall {session_id}:{tool_call_id} has no output ({}: {})",
            omission.reason,
            omission.detail
        ),
    }
}

/// Load the persisted result string for a tool call identified by
/// `session_id` + `tool_call_id`. Returns an error if the row is absent, is
/// nonterminal, binds an omission, or has ambiguous/incoherent exact evidence.
pub async fn load_tool_call_result(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> Result<String> {
    Ok(load_exact_tool_call_output(node, session_id, tool_call_id)
        .await?
        .output_text)
}

/// Verify that a streamed subagent receipt is a deterministic projection of
/// the exact signed running bridge document. Background receipts are emitted
/// before terminal output exists, so they cannot use `AgentToolResult`; they
/// still must not become transcript facts from unauthenticated stream bytes.
pub(crate) async fn verify_exact_subagent_receipt_authority(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
    receipt: &str,
) -> Result<String> {
    let payload: serde_json::Value =
        serde_json::from_str(receipt).context("decoding streamed subagent receipt as JSON")?;
    let receipt_child = payload
        .get("child_request_id")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("streamed subagent receipt omitted child_request_id"))?;
    let receipt_await_mode = payload
        .get("await_mode")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("streamed subagent receipt omitted await_mode"))?;
    let identity = node
        .node_identity_did()
        .ok_or_else(|| anyhow!("verifying subagent receipt requires a DefraDB query identity"))
        .and_then(|did| Did::new(did).context("parsing subagent-receipt query identity"))?;
    let expected_key = format!("{session_id}:{tool_call_id}");
    let query = format!(
        r#"{{ AgentToolCall(filter: {{ tool_call_key: {{ _eq: "{}" }} }}) {{
            _docID tool_call_key session_id tool_call_id tool_name lifecycle_state
            await_mode child_request_id
        }} }}"#,
        escape_graphql_string(&expected_key),
    );
    let response = node
        .execute_request_with_retry(
            QueryRequest::new(query).with_identity(Some(identity.clone())),
            ExecuteRetryPolicy::default(),
        )
        .await;
    if response.has_errors() {
        anyhow::bail!(
            "loading subagent receipt authority {expected_key} failed: {:?}",
            response.errors
        );
    }
    let rows: Vec<SubagentReceiptAuthorityRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    let row = resolve_exact_tool_call_match(
        session_id,
        tool_call_id,
        rows,
        |row| row.doc_id.as_str(),
        |row| row.tool_call_key.as_str(),
        |row| row.session_id.as_str(),
        |row| row.tool_call_id.as_str(),
    )?
    .ok_or_else(|| anyhow!("no AgentToolCall authority for subagent receipt {expected_key}"))?;
    let current = crate::document_version::verified_current_signed_document_version_with_identity(
        node,
        "AgentToolCall",
        &row.doc_id,
        Some(identity.clone()),
    )
    .await?;
    let exact = crate::document_version::verified_exact_document_snapshot_with_identity(
        node,
        "AgentToolCall",
        &current.version,
        "tool_call_key session_id tool_call_id tool_name lifecycle_state await_mode child_request_id",
        Some(identity),
    )
    .await?;
    if exact.source.signer_did != current.signer_did {
        anyhow::bail!("subagent receipt authority signer changed during exact reload");
    }
    let exact: SubagentReceiptAuthorityRow = exact.decode()?;
    if exact.doc_id != row.doc_id
        || exact.tool_call_key != expected_key
        || exact.session_id != session_id
        || exact.tool_call_id != tool_call_id
        || exact.tool_name != "spawn_subagent"
        || !matches!(
            exact.lifecycle_state.as_str(),
            "running" | "completed" | "failed" | "timedOut" | "cancelled"
        )
        || exact.await_mode.as_deref() != Some(receipt_await_mode)
        || exact.child_request_id.as_deref() != Some(receipt_child)
    {
        anyhow::bail!(
            "streamed subagent receipt does not match its exact signed running bridge authority"
        );
    }
    Ok(exact.tool_name)
}

#[derive(Debug, Deserialize)]
struct ToolCallRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    tool_call_key: String,
    session_id: String,
    tool_call_id: String,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    agent_did: Option<String>,
    #[serde(default)]
    requester_did: Option<String>,
    message_sequence: u32,
    tool_name: String,
    args: String,
    lifecycle_state: Option<String>,
    started_at: Option<String>,
    #[serde(default)]
    deadline_at: Option<String>,
    tool_failure_class: Option<String>,
    cancel_cause: Option<String>,
    // v3 subagent fields — nullable for v2 rows that pre-date the schema migration.
    await_mode: Option<String>,
    cancel_policy: Option<String>,
    child_request_id: Option<String>,
    spawn_target_did: Option<String>,
    unclaimed_deadline_at: Option<String>,
    workflow_group_id: Option<String>,
    workflow_role: Option<String>,
}

pub(crate) struct ExactLifecycleAdoption {
    pub(crate) doc_id: String,
    pub(crate) deadline_at: chrono::DateTime<chrono::Utc>,
    pub(crate) state: ToolCallState,
    pub(crate) started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub(crate) failure_class: Option<FailureClass>,
    pub(crate) cancel_cause: Option<CancelCause>,
    pub(crate) await_mode: AwaitMode,
    pub(crate) cancel_policy: CancelPolicy,
    pub(crate) child_request_id: Option<String>,
    pub(crate) spawn_target_did: Option<String>,
    pub(crate) unclaimed_deadline_at: Option<chrono::DateTime<chrono::Utc>>,
    pub(crate) workflow_group_id: Option<String>,
    pub(crate) workflow_role: Option<String>,
}

/// Resolve the complete logical match set, then decode lifecycle state only
/// from the exact current signed CID of the expected physical document.
/// Competing-terminal adoption must never copy a mutable projection observed
/// before signer verification.
pub(crate) async fn load_exact_lifecycle_adoption(
    node: Arc<EmbeddedNode>,
    expected_doc_id: &str,
    session_id: &str,
    tool_call_id: &str,
) -> Result<ExactLifecycleAdoption> {
    let identity = node
        .node_identity_did()
        .ok_or_else(|| anyhow!("adopting durable tool state requires a DefraDB query identity"))
        .and_then(|did| Did::new(did).context("parsing tool adoption query identity"))?;
    let expected_key = format!("{session_id}:{tool_call_id}");
    let query = format!(
        r#"{{ AgentToolCall(filter: {{ tool_call_key: {{ _eq: "{}" }} }}) {{
            _docID tool_call_key session_id tool_call_id
        }} }}"#,
        escape_graphql_string(&expected_key),
    );
    let response = node
        .execute_request_with_retry(
            QueryRequest::new(query).with_identity(Some(identity.clone())),
            ExecuteRetryPolicy::default(),
        )
        .await;
    if response.has_errors() {
        anyhow::bail!(
            "enumerating AgentToolCall adoption authority failed: {:?}",
            response.errors
        );
    }
    #[derive(Deserialize)]
    struct IdentityRow {
        #[serde(rename = "_docID")]
        doc_id: String,
        tool_call_key: String,
        session_id: String,
        tool_call_id: String,
    }
    let rows: Vec<IdentityRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    let authority = resolve_exact_tool_call_match(
        session_id,
        tool_call_id,
        rows,
        |row| row.doc_id.as_str(),
        |row| row.tool_call_key.as_str(),
        |row| row.session_id.as_str(),
        |row| row.tool_call_id.as_str(),
    )?
    .ok_or_else(|| anyhow!("AgentToolCall disappeared during terminal adoption"))?;
    if authority.doc_id != expected_doc_id {
        anyhow::bail!("terminal adoption resolved to a different physical AgentToolCall");
    }

    let current = crate::document_version::verified_current_signed_document_version_with_identity(
        &node,
        "AgentToolCall",
        expected_doc_id,
        Some(identity.clone()),
    )
    .await?;
    let snapshot = crate::document_version::verified_exact_document_snapshot_with_identity(
        &node,
        "AgentToolCall",
        &current.version,
        "tool_call_key session_id tool_call_id request_id agent_did requester_did message_sequence tool_name args lifecycle_state started_at deadline_at tool_failure_class cancel_cause await_mode cancel_policy child_request_id spawn_target_did unclaimed_deadline_at workflow_group_id workflow_role",
        Some(identity),
    )
    .await?;
    if snapshot.source.signer_did != current.signer_did {
        anyhow::bail!("AgentToolCall signer changed during exact terminal adoption");
    }
    let row: ToolCallRow = snapshot.decode()?;
    if row.doc_id != expected_doc_id
        || row.tool_call_key != expected_key
        || row.session_id != session_id
        || row.tool_call_id != tool_call_id
    {
        anyhow::bail!("exact terminal adoption authority changed immutable identity");
    }
    let state = row
        .lifecycle_state
        .as_deref()
        .and_then(ToolCallState::from_persisted)
        .ok_or_else(|| anyhow!("exact AgentToolCall adoption authority has no valid state"))?;
    let parse_time = |value: Option<String>| {
        value
            .as_deref()
            .map(chrono::DateTime::parse_from_rfc3339)
            .transpose()
            .map(|value| value.map(|value| value.with_timezone(&chrono::Utc)))
    };
    Ok(ExactLifecycleAdoption {
        doc_id: row.doc_id,
        deadline_at: parse_time(row.deadline_at)?.unwrap_or_else(chrono::Utc::now),
        state,
        started_at: parse_time(row.started_at)?,
        failure_class: row
            .tool_failure_class
            .as_deref()
            .and_then(FailureClass::from_persisted),
        cancel_cause: row
            .cancel_cause
            .as_deref()
            .and_then(CancelCause::from_persisted),
        await_mode: row
            .await_mode
            .as_deref()
            .and_then(AwaitMode::from_persisted)
            .unwrap_or(AwaitMode::Foreground),
        cancel_policy: row
            .cancel_policy
            .as_deref()
            .and_then(CancelPolicy::from_persisted)
            .unwrap_or(CancelPolicy::Cascade),
        child_request_id: row
            .child_request_id
            .filter(|value| !value.trim().is_empty()),
        spawn_target_did: row
            .spawn_target_did
            .filter(|value| !value.trim().is_empty()),
        unclaimed_deadline_at: parse_time(row.unclaimed_deadline_at)?,
        workflow_group_id: row
            .workflow_group_id
            .filter(|value| !value.trim().is_empty()),
        workflow_role: row.workflow_role.filter(|value| !value.trim().is_empty()),
    })
}

impl ToolCallLifecycle {
    /// Load an existing AgentToolCall row by session_id and tool_call_id.
    /// Returns `None` if the row does not exist.
    pub async fn load(
        node: Arc<EmbeddedNode>,
        session_id: &str,
        tool_call_id: &str,
    ) -> Result<Option<Self>> {
        let escaped_session_id = escape_graphql_string(session_id);
        let escaped_tool_call_id = escape_graphql_string(tool_call_id);
        let query = format!(
            r#"{{
                AgentToolCall(
                    filter: {{
                        session_id: {{ _eq: "{escaped_session_id}" }},
                        tool_call_id: {{ _eq: "{escaped_tool_call_id}" }}
                    }}
                ) {{
                    _docID
                    tool_call_key
                    session_id
                    tool_call_id
                    request_id
                    agent_did
                    requester_did
                    message_sequence
                    tool_name
                    args
                    lifecycle_state
                    started_at
                    deadline_at
                    tool_failure_class
                    cancel_cause
                    await_mode
                    cancel_policy
                    child_request_id
                    spawn_target_did
                    unclaimed_deadline_at
                    workflow_group_id
                    workflow_role
                }}
            }}"#
        );

        let resp = node.execute(&query).await;
        if resp.has_errors() {
            return Err(anyhow!(
                "load AgentToolCall query failed: {:?}",
                resp.errors
            ));
        }

        let rows: Vec<ToolCallRow> = resp
            .data
            .as_ref()
            .and_then(|d| d.get("AgentToolCall"))
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .context("decoding complete AgentToolCall lifecycle match set")?
            .unwrap_or_default();

        let row = match resolve_exact_tool_call_match(
            session_id,
            tool_call_id,
            rows,
            |row| row.doc_id.as_str(),
            |row| row.tool_call_key.as_str(),
            |row| row.session_id.as_str(),
            |row| row.tool_call_id.as_str(),
        )? {
            Some(r) => r,
            None => return Ok(None),
        };

        let state = row
            .lifecycle_state
            .as_deref()
            .and_then(ToolCallState::from_persisted)
            .unwrap_or(ToolCallState::Running); // legacy rows pre-migration default to Running

        let started_at = row
            .started_at
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));

        let deadline_at = row
            .deadline_at
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(chrono::Utc::now);

        let failure_class = row
            .tool_failure_class
            .as_deref()
            .and_then(FailureClass::from_persisted);

        let cancel_cause = row
            .cancel_cause
            .as_deref()
            .and_then(CancelCause::from_persisted);

        // v3 subagent fields. v2 rows (where these columns are null) fall back
        // to the same defaults that Self::new() uses, preserving backwards compat.
        let await_mode = row
            .await_mode
            .as_deref()
            .and_then(AwaitMode::from_persisted)
            .unwrap_or(AwaitMode::Foreground);

        let cancel_policy = row
            .cancel_policy
            .as_deref()
            .and_then(CancelPolicy::from_persisted)
            .unwrap_or(CancelPolicy::Cascade);

        let child_request_id = row.child_request_id.filter(|s| !s.is_empty());
        let spawn_target_did = row.spawn_target_did.filter(|s| !s.is_empty());
        let unclaimed_deadline_at = row
            .unclaimed_deadline_at
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));

        Ok(Some(Self {
            node,
            request_id: row.request_id.unwrap_or_default(),
            session_id: session_id.to_string(),
            agent_did: row.agent_did.unwrap_or_default(),
            // Current recovery paths only update the existing immutable row,
            // but preserve its route key so a future create transition cannot
            // silently rehydrate the lifecycle as unrouted.
            requester_did: row.requester_did.filter(|value| !value.trim().is_empty()),
            tool_call_id: tool_call_id.to_string(),
            message_sequence: row.message_sequence,
            tool_name: row.tool_name,
            args: row.args,
            doc_id: Some(row.doc_id),
            deadline_at,
            state,
            started_at,
            failure_class,
            cancel_cause,
            await_mode,
            cancel_policy,
            child_request_id,
            spawn_target_did,
            unclaimed_deadline_at,
            workflow_group_id: row.workflow_group_id.filter(|value| !value.is_empty()),
            workflow_role: row.workflow_role.filter(|value| !value.is_empty()),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct IdentityRow {
        doc_id: String,
        tool_call_key: String,
        session_id: String,
        tool_call_id: String,
    }

    fn identity_row(doc_id: &str) -> IdentityRow {
        IdentityRow {
            doc_id: doc_id.to_string(),
            tool_call_key: "session:call".to_string(),
            session_id: "session".to_string(),
            tool_call_id: "call".to_string(),
        }
    }

    #[test]
    fn exact_tool_call_match_rejects_logical_twins_deterministically() {
        let error = resolve_exact_tool_call_match(
            "session",
            "call",
            vec![identity_row("doc-z"), identity_row("doc-a")],
            |row| row.doc_id.as_str(),
            |row| row.tool_call_key.as_str(),
            |row| row.session_id.as_str(),
            |row| row.tool_call_id.as_str(),
        )
        .expect_err("logical twins must fail closed");

        let conflict = error
            .downcast_ref::<crate::session::LogicalDocumentResolutionError>()
            .expect("typed logical conflict");
        assert_eq!(
            conflict,
            &crate::session::LogicalDocumentResolutionError::Conflict(
                crate::session::LogicalIdConflict {
                    collection: "AgentToolCall",
                    logical_field: "tool_call_key",
                    logical_value: "session:call".to_string(),
                    document_ids: vec!["doc-a".to_string(), "doc-z".to_string()],
                }
            )
        );
    }

    #[test]
    fn exact_tool_call_match_rejects_mismatched_immutable_identity() {
        let mut row = identity_row("doc-a");
        row.session_id = "other-session".to_string();
        let error = resolve_exact_tool_call_match(
            "session",
            "call",
            vec![row],
            |row| row.doc_id.as_str(),
            |row| row.tool_call_key.as_str(),
            |row| row.session_id.as_str(),
            |row| row.tool_call_id.as_str(),
        )
        .expect_err("immutable identity mismatch must fail closed");

        assert!(error
            .to_string()
            .contains("AgentToolCall immutable identity mismatch"));
    }

    #[tokio::test]
    async fn load_preserves_immutable_requester_route() {
        let node = Arc::new(
            defra_node::EmbeddedNode::builder()
                .build()
                .await
                .expect("embedded node"),
        );
        crate::ensure_schemas(node.as_ref())
            .await
            .expect("runtime schemas");
        let mut lifecycle = ToolCallLifecycle::new(
            node.clone(),
            "request-routed".to_string(),
            "session-routed".to_string(),
            "did:test:host".to_string(),
            "tool-call-routed".to_string(),
            1,
            "test_tool".to_string(),
            "{}".to_string(),
            chrono::Utc::now() + chrono::Duration::minutes(5),
        )
        .with_requester_did(Some("did:test:coordinator".to_string()));
        lifecycle.start_running().await.expect("persist tool call");

        let loaded = ToolCallLifecycle::load(node.clone(), "session-routed", "tool-call-routed")
            .await
            .expect("load tool call")
            .expect("persisted tool call");

        assert_eq!(
            loaded.requester_did.as_deref(),
            Some("did:test:coordinator")
        );
        node.shutdown().await;
    }
}
