use anyhow::{Context as _, Result};
use serde::Deserialize;

use crate::graphql::escape_graphql_string;
use crate::session::execute_mutation_with_retry;

use super::ToolCallState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolOutputOmissionReason {
    PreDispatchFailure,
    ApprovalDenied,
    TimedOut,
    Cancelled,
    RecoveryFailure,
    ExecutionLost,
    ChildDead,
    ChildSuperseded,
}

impl ToolOutputOmissionReason {
    pub(crate) fn from_persisted(value: &str) -> Option<Self> {
        match value {
            "preDispatchFailure" => Some(Self::PreDispatchFailure),
            "approvalDenied" => Some(Self::ApprovalDenied),
            "timedOut" => Some(Self::TimedOut),
            "cancelled" => Some(Self::Cancelled),
            "recoveryFailure" => Some(Self::RecoveryFailure),
            "executionLost" => Some(Self::ExecutionLost),
            "childDead" => Some(Self::ChildDead),
            "childSuperseded" => Some(Self::ChildSuperseded),
            _ => None,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::PreDispatchFailure => "preDispatchFailure",
            Self::ApprovalDenied => "approvalDenied",
            Self::TimedOut => "timedOut",
            Self::Cancelled => "cancelled",
            Self::RecoveryFailure => "recoveryFailure",
            Self::ExecutionLost => "executionLost",
            Self::ChildDead => "childDead",
            Self::ChildSuperseded => "childSuperseded",
        }
    }

    pub(crate) const fn allows(self, source: ToolCallState, terminal: ToolCallState) -> bool {
        match self {
            Self::PreDispatchFailure => {
                matches!(source, ToolCallState::Pending)
                    && matches!(terminal, ToolCallState::Failed)
            }
            Self::ApprovalDenied => {
                matches!(source, ToolCallState::AwaitingApproval)
                    && matches!(terminal, ToolCallState::Failed)
            }
            Self::TimedOut => {
                matches!(
                    source,
                    ToolCallState::Running | ToolCallState::AwaitingApproval
                ) && matches!(terminal, ToolCallState::TimedOut)
            }
            Self::Cancelled => {
                matches!(
                    source,
                    ToolCallState::Pending
                        | ToolCallState::AwaitingApproval
                        | ToolCallState::Running
                ) && matches!(terminal, ToolCallState::Cancelled)
            }
            Self::RecoveryFailure
            | Self::ExecutionLost
            | Self::ChildDead
            | Self::ChildSuperseded => {
                matches!(source, ToolCallState::Running)
                    && matches!(terminal, ToolCallState::Failed)
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct ToolCallOmissionParentRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    tool_call_key: String,
    agent_did: String,
    requester_did: Option<String>,
    session_id: String,
    tool_name: String,
    args: String,
    lifecycle_state: String,
}

#[derive(Debug, Deserialize)]
struct CurrentToolCallPhaseRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    session_id: String,
    tool_call_id: String,
    lifecycle_state: String,
}

/// Return whether another writer has already moved the exact signed execution
/// away from the source phase this terminal writer expected and closed it with
/// one coherent terminal-evidence graph.
///
/// Terminal evidence is necessarily published before it can be bound in the
/// AgentToolCall compare-and-set. A competing writer may therefore win before
/// this writer can even publish its fact. That is a normal first-terminal-wins
/// outcome, not a recovery failure, but only after the winner's current source
/// document has itself been cryptographically verified.
async fn exact_signed_terminal_source_moved(
    node: &defra_node::EmbeddedNode,
    tool_call_doc_id: &str,
    expected_source: ToolCallState,
) -> Result<bool> {
    let current = crate::document_version::verified_current_signed_document_version(
        node,
        "AgentToolCall",
        tool_call_doc_id,
    )
    .await?;
    let snapshot = crate::document_version::verified_exact_document_snapshot_with_identity(
        node,
        "AgentToolCall",
        &current.version,
        "session_id tool_call_id lifecycle_state",
        None,
    )
    .await?;
    if snapshot.source.signer_did != current.signer_did {
        anyhow::bail!(
            "AgentToolCall {tool_call_doc_id} signer changed while adopting a competing terminal writer"
        );
    }
    let row: CurrentToolCallPhaseRow = snapshot.decode()?;
    if row.doc_id != tool_call_doc_id {
        anyhow::bail!(
            "AgentToolCall {tool_call_doc_id} exact source reload returned {}",
            row.doc_id
        );
    }
    let state = ToolCallState::from_persisted(&row.lifecycle_state).ok_or_else(|| {
        anyhow::anyhow!(
            "AgentToolCall {tool_call_doc_id} has unknown lifecycle state {}",
            row.lifecycle_state
        )
    })?;
    if state == expected_source {
        return Ok(false);
    }
    super::query::load_exact_tool_call_terminal_evidence(node, &row.session_id, &row.tool_call_id)
        .await
        .with_context(|| {
            format!(
                "AgentToolCall {tool_call_doc_id} moved from {} without coherent terminal evidence",
                expected_source.as_str()
            )
        })?;
    Ok(true)
}

/// Preserve the publication failure unless a competing terminal writer can be
/// adopted through the same exact-evidence verifier used by durable readers.
async fn require_verified_terminal_after_publication_error(
    node: &defra_node::EmbeddedNode,
    tool_call_doc_id: &str,
    expected_source: ToolCallState,
    operation: &'static str,
    publication_error: anyhow::Error,
) -> Result<()> {
    match exact_signed_terminal_source_moved(node, tool_call_doc_id, expected_source).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(publication_error),
        Err(adoption_error) => Err(publication_error.context(format!(
            "{operation} cannot adopt a competing terminal execution: {adoption_error:#}"
        ))),
    }
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(Clone))]
struct ExistingOmissionFact {
    #[serde(rename = "_docID")]
    doc_id: String,
    omission_key: String,
    tool_call_key: String,
    tool_call_doc_id: String,
    tool_call_composite_commit_cid: String,
    tool_call_signer_did: String,
    agent_did: String,
    requester_did: Option<String>,
    session_id: String,
    source_phase: String,
    terminal_phase: String,
    reason: String,
    detail: String,
}

fn created_doc_id(data: &serde_json::Value) -> Option<&str> {
    let value = data
        .get("create_AgentToolOutputOmission")
        .or_else(|| data.get("add_AgentToolOutputOmission"))?;
    value
        .get("_docID")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            value
                .as_array()
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("_docID"))
                .and_then(serde_json::Value::as_str)
        })
}

async fn exact_current_parent(
    node: &defra_node::EmbeddedNode,
    doc_id: &str,
    expected_source: ToolCallState,
) -> Result<(ToolCallOmissionParentRow, crate::SignedDocumentVersionRef)> {
    let parent = crate::document_version::verified_current_signed_document_version(
        node,
        "AgentToolCall",
        doc_id,
    )
    .await?;
    let response = node
        .execute(&format!(
            r#"{{ AgentToolCall(cid: ["{}"]) {{
                _docID tool_call_key agent_did requester_did session_id tool_name args lifecycle_state
            }} }}"#,
            escape_graphql_string(&parent.version.composite_commit_cid),
        ))
        .await;
    if response.has_errors() {
        anyhow::bail!(
            "loading exact AgentToolCall omission parent failed: {:?}",
            response.errors
        );
    }
    let rows: Vec<ToolCallOmissionParentRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    let row = match rows.as_slice() {
        [row] if row.doc_id == doc_id => ToolCallOmissionParentRow {
            doc_id: row.doc_id.clone(),
            tool_call_key: row.tool_call_key.clone(),
            agent_did: row.agent_did.clone(),
            requester_did: row.requester_did.clone(),
            session_id: row.session_id.clone(),
            tool_name: row.tool_name.clone(),
            args: row.args.clone(),
            lifecycle_state: row.lifecycle_state.clone(),
        },
        rows => anyhow::bail!(
            "exact AgentToolCall omission parent reconstructed {} rows or another document",
            rows.len()
        ),
    };
    if row.lifecycle_state != expected_source.as_str() {
        anyhow::bail!(
            "AgentToolCall {doc_id} is {}, not the expected omission source {}",
            row.lifecycle_state,
            expected_source.as_str()
        );
    }
    Ok((row, parent))
}

#[derive(Debug, Deserialize)]
struct ExistingOutputFact {
    #[serde(rename = "_docID")]
    doc_id: String,
    result_key: String,
    tool_call_key: String,
    tool_call_doc_id: String,
    tool_call_composite_commit_cid: String,
    tool_call_signer_did: String,
    agent_did: String,
    requester_did: Option<String>,
    session_id: String,
    tool_name: String,
    tool_input: String,
    output_text: String,
    model_output_truncated: bool,
    truncation_metadata: String,
}

/// Publish one immutable full-output fact against the exact current running
/// execution. This borrowed-node entry point is used by startup/periodic
/// recovery, which intentionally does not own an `Arc<EmbeddedNode>`.
pub(crate) async fn retain_running_output(
    node: &defra_node::EmbeddedNode,
    tool_call_doc_id: &str,
    output: &str,
) -> Result<crate::SignedDocumentVersionRef> {
    let (parent_row, parent) =
        exact_current_parent(node, tool_call_doc_id, ToolCallState::Running).await?;
    let result_key = parent.version.composite_commit_cid.clone();
    let mode = crate::truncation::tool_result_truncation_mode(&parent_row.tool_name);
    let limits = crate::truncation::TruncationLimits::default();
    let (_, _, model_output_truncated) = crate::truncation::truncate_text(output, mode, &limits);
    let metadata = crate::truncation::model_projection_metadata(output, mode, &limits);
    let lookup = format!(
        r#"{{ AgentToolResult(filter: {{ result_key: {{ _eq: "{}" }} }}) {{
            _docID result_key tool_call_key tool_call_doc_id tool_call_composite_commit_cid
            tool_call_signer_did agent_did requester_did session_id tool_name tool_input output_text
            model_output_truncated truncation_metadata
        }} }}"#,
        escape_graphql_string(&result_key),
    );
    let load = || async {
        let response = node.execute(&lookup).await;
        if response.has_errors() {
            anyhow::bail!(
                "enumerating recovery AgentToolResult twins failed: {:?}",
                response.errors
            );
        }
        response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentToolResult"))
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map(|rows| rows.unwrap_or_default())
            .context("decoding recovery AgentToolResult rows")
    };
    let matches = |row: &ExistingOutputFact| {
        row.result_key == result_key
            && row.tool_call_key == parent_row.tool_call_key
            && row.tool_call_doc_id == parent.version.doc_id
            && row.tool_call_composite_commit_cid == parent.version.composite_commit_cid
            && row.tool_call_signer_did == parent.signer_did
            && row.agent_did == parent_row.agent_did
            && row.requester_did == parent_row.requester_did
            && row.session_id == parent_row.session_id
            && row.tool_name == parent_row.tool_name
            && row.tool_input == parent_row.args
            && row.output_text == output
            && row.model_output_truncated == model_output_truncated
            && row.truncation_metadata == metadata
    };
    let matching = |rows: Vec<ExistingOutputFact>| -> Option<ExistingOutputFact> {
        let mut rows = rows.into_iter().filter(&matches).collect::<Vec<_>>();
        rows.sort_by(|left, right| left.doc_id.cmp(&right.doc_id));
        rows.into_iter().next()
    };
    if let Some(row) = matching(load().await?) {
        let exact = crate::document_version::verified_current_signed_document_version(
            node,
            "AgentToolResult",
            &row.doc_id,
        )
        .await?;
        if exact.signer_did != parent.signer_did {
            anyhow::bail!("AgentToolResult signer does not match execution owner");
        }
        return Ok(exact);
    }
    let requester_field =
        crate::session::requester_did_create_field(parent_row.requester_did.as_deref());
    let mutation = format!(
        r#"mutation {{ create_AgentToolResult(input: {{
            result_key: "{}"
            tool_call_key: "{}"
            tool_call_doc_id: "{}"
            tool_call_composite_commit_cid: "{}"
            tool_call_signer_did: "{}"
            agent_did: "{}"
            {requester_field}
            session_id: "{}"
            tool_name: "{}"
            tool_input: "{}"
            output_text: "{}"
            model_output_truncated: {model_output_truncated}
            truncation_metadata: "{}"
            conversation_doc_id: ""
            created_at: "{}"
        }}) {{ _docID }} }}"#,
        escape_graphql_string(&result_key),
        escape_graphql_string(&parent_row.tool_call_key),
        escape_graphql_string(&parent.version.doc_id),
        escape_graphql_string(&parent.version.composite_commit_cid),
        escape_graphql_string(&parent.signer_did),
        escape_graphql_string(&parent_row.agent_did),
        escape_graphql_string(&parent_row.session_id),
        escape_graphql_string(&parent_row.tool_name),
        escape_graphql_string(&parent_row.args),
        escape_graphql_string(output),
        escape_graphql_string(&metadata),
        chrono::Utc::now().to_rfc3339(),
    );
    let response =
        match execute_mutation_with_retry(node, &mutation, "retain recovery tool output").await {
            Ok(response) => response,
            Err(create_error) => {
                if let Some(row) = matching(load().await?) {
                    let exact = crate::document_version::verified_current_signed_document_version(
                        node,
                        "AgentToolResult",
                        &row.doc_id,
                    )
                    .await?;
                    if exact.signer_did != parent.signer_did {
                        anyhow::bail!("AgentToolResult signer does not match execution owner");
                    }
                    return Ok(exact);
                }
                return Err(create_error);
            }
        };
    let doc_id = response
        .data
        .as_ref()
        .and_then(|data| {
            let value = data
                .get("create_AgentToolResult")
                .or_else(|| data.get("add_AgentToolResult"))?;
            value
                .get("_docID")
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    value
                        .as_array()
                        .and_then(|rows| rows.first())
                        .and_then(|row| row.get("_docID"))
                        .and_then(serde_json::Value::as_str)
                })
        })
        .ok_or_else(|| anyhow::anyhow!("recovery AgentToolResult create returned no _docID"))?;
    let exact = crate::document_version::verified_current_signed_document_version(
        node,
        "AgentToolResult",
        doc_id,
    )
    .await?;
    if exact.signer_did != parent.signer_did {
        anyhow::bail!("AgentToolResult signer does not match execution owner");
    }
    let rows = load().await?;
    let row = rows
        .iter()
        .find(|row| row.doc_id == exact.version.doc_id)
        .ok_or_else(|| anyhow::anyhow!("created AgentToolResult disappeared"))?;
    if !matches(row) {
        anyhow::bail!("created AgentToolResult payload does not match its execution proposal");
    }
    Ok(exact)
}

async fn retained_output_projection(
    node: &defra_node::EmbeddedNode,
    exact: &crate::SignedDocumentVersionRef,
) -> Result<String> {
    #[derive(Deserialize)]
    struct Row {
        output_text: String,
        model_output_truncated: bool,
        truncation_metadata: String,
    }

    let snapshot = crate::document_version::verified_exact_document_snapshot_with_identity(
        node,
        "AgentToolResult",
        &exact.version,
        "output_text model_output_truncated truncation_metadata",
        None,
    )
    .await?;
    if snapshot.source != *exact {
        anyhow::bail!("AgentToolResult changed while deriving its recovery projection");
    }
    let row: Row = snapshot.decode()?;
    crate::truncation::canonical_model_projection(
        &row.output_text,
        &exact.version.doc_id,
        row.model_output_truncated,
        &row.truncation_metadata,
    )
}

fn fact_matches(
    row: &ExistingOmissionFact,
    parent_row: &ToolCallOmissionParentRow,
    parent: &crate::SignedDocumentVersionRef,
    expected_source: ToolCallState,
    terminal: ToolCallState,
    reason: ToolOutputOmissionReason,
    detail: &str,
) -> bool {
    row.omission_key == parent.version.composite_commit_cid
        && row.tool_call_key == parent_row.tool_call_key
        && row.tool_call_doc_id == parent.version.doc_id
        && row.tool_call_composite_commit_cid == parent.version.composite_commit_cid
        && row.tool_call_signer_did == parent.signer_did
        && row.agent_did == parent_row.agent_did
        && row.requester_did == parent_row.requester_did
        && row.session_id == parent_row.session_id
        && row.source_phase == expected_source.as_str()
        && row.terminal_phase == terminal.as_str()
        && row.reason == reason.as_str()
        && row.detail == detail
}

#[allow(clippy::too_many_arguments)]
fn validate_exact_omission_payload(
    row: &ExistingOmissionFact,
    exact: &crate::SignedDocumentVersionRef,
    parent_row: &ToolCallOmissionParentRow,
    parent: &crate::SignedDocumentVersionRef,
    expected_source: ToolCallState,
    terminal: ToolCallState,
    reason: ToolOutputOmissionReason,
    detail: &str,
) -> Result<()> {
    if row.doc_id != exact.version.doc_id
        || exact.signer_did != parent.signer_did
        || !fact_matches(
            row,
            parent_row,
            parent,
            expected_source,
            terminal,
            reason,
            detail,
        )
    {
        anyhow::bail!(
            "exact AgentToolOutputOmission does not license {} -> {} with reason {}",
            expected_source.as_str(),
            terminal.as_str(),
            reason.as_str()
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn verify_existing(
    node: &defra_node::EmbeddedNode,
    doc_id: &str,
    parent_row: &ToolCallOmissionParentRow,
    parent: &crate::SignedDocumentVersionRef,
    expected_source: ToolCallState,
    terminal: ToolCallState,
    reason: ToolOutputOmissionReason,
    detail: &str,
) -> Result<crate::SignedDocumentVersionRef> {
    let exact = crate::document_version::verified_current_signed_document_version(
        node,
        "AgentToolOutputOmission",
        doc_id,
    )
    .await?;
    let snapshot = crate::document_version::verified_exact_document_snapshot_with_identity(
        node,
        "AgentToolOutputOmission",
        &exact.version,
        "omission_key tool_call_key tool_call_doc_id tool_call_composite_commit_cid \
         tool_call_signer_did agent_did requester_did session_id source_phase terminal_phase \
         reason detail",
        None,
    )
    .await?;
    if snapshot.source != exact {
        anyhow::bail!("AgentToolOutputOmission changed while verifying its exact payload");
    }
    let row: ExistingOmissionFact = snapshot.decode()?;
    validate_exact_omission_payload(
        &row,
        &exact,
        parent_row,
        parent,
        expected_source,
        terminal,
        reason,
        detail,
    )?;
    // `execute_transition_with_exact_evidence` re-reads this current exact ref
    // inside the transition transaction. Since the payload above was decoded
    // from its content-addressed CID, equality there revalidates these typed
    // semantics without trusting another mutable head projection.
    Ok(exact)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn retain_tool_output_omission(
    node: &defra_node::EmbeddedNode,
    tool_call_doc_id: &str,
    expected_source: ToolCallState,
    terminal: ToolCallState,
    reason: ToolOutputOmissionReason,
    detail: &str,
) -> Result<crate::SignedDocumentVersionRef> {
    if !reason.allows(expected_source, terminal) {
        anyhow::bail!(
            "omission reason {} cannot license {} -> {}",
            reason.as_str(),
            expected_source.as_str(),
            terminal.as_str()
        );
    }
    let (parent_row, parent) =
        exact_current_parent(node, tool_call_doc_id, expected_source).await?;
    let omission_key = parent.version.composite_commit_cid.clone();
    let lookup = format!(
        r#"{{ AgentToolOutputOmission(filter: {{ omission_key: {{ _eq: "{}" }} }}) {{
            _docID omission_key tool_call_key tool_call_doc_id
            tool_call_composite_commit_cid tool_call_signer_did agent_did
            requester_did session_id source_phase terminal_phase reason detail
        }} }}"#,
        escape_graphql_string(&omission_key),
    );
    let load = || async {
        let response = node.execute(&lookup).await;
        if response.has_errors() {
            anyhow::bail!(
                "enumerating AgentToolOutputOmission twins failed: {:?}",
                response.errors
            );
        }
        response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentToolOutputOmission"))
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map(|rows| rows.unwrap_or_default())
            .context("decoding AgentToolOutputOmission rows")
    };
    let matching = |rows: Vec<ExistingOmissionFact>| -> Option<ExistingOmissionFact> {
        let mut rows = rows
            .into_iter()
            .filter(|row| {
                fact_matches(
                    row,
                    &parent_row,
                    &parent,
                    expected_source,
                    terminal,
                    reason,
                    detail,
                )
            })
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| left.doc_id.cmp(&right.doc_id));
        rows.into_iter().next()
    };
    if let Some(existing) = matching(load().await?) {
        return verify_existing(
            node,
            &existing.doc_id,
            &parent_row,
            &parent,
            expected_source,
            terminal,
            reason,
            detail,
        )
        .await;
    }

    let requester_field =
        crate::session::requester_did_create_field(parent_row.requester_did.as_deref());
    let mutation = format!(
        r#"mutation {{ create_AgentToolOutputOmission(input: {{
            omission_key: "{}"
            tool_call_key: "{}"
            tool_call_doc_id: "{}"
            tool_call_composite_commit_cid: "{}"
            tool_call_signer_did: "{}"
            agent_did: "{}"
            {requester_field}
            session_id: "{}"
            source_phase: "{}"
            terminal_phase: "{}"
            reason: "{}"
            detail: "{}"
            created_at: "{}"
        }}) {{ _docID }} }}"#,
        escape_graphql_string(&omission_key),
        escape_graphql_string(&parent_row.tool_call_key),
        escape_graphql_string(&parent.version.doc_id),
        escape_graphql_string(&parent.version.composite_commit_cid),
        escape_graphql_string(&parent.signer_did),
        escape_graphql_string(&parent_row.agent_did),
        escape_graphql_string(&parent_row.session_id),
        expected_source.as_str(),
        terminal.as_str(),
        reason.as_str(),
        escape_graphql_string(detail),
        chrono::Utc::now().to_rfc3339(),
    );
    let response = match execute_mutation_with_retry(
        node,
        &mutation,
        "create AgentToolOutputOmission",
    )
    .await
    {
        Ok(response) => response,
        Err(create_error) => {
            if let Some(existing) = matching(load().await?) {
                return verify_existing(
                    node,
                    &existing.doc_id,
                    &parent_row,
                    &parent,
                    expected_source,
                    terminal,
                    reason,
                    detail,
                )
                .await;
            }
            return Err(create_error);
        }
    };
    let doc_id = response
        .data
        .as_ref()
        .and_then(created_doc_id)
        .ok_or_else(|| anyhow::anyhow!("AgentToolOutputOmission create returned no _docID"))?;
    verify_existing(
        node,
        doc_id,
        &parent_row,
        &parent,
        expected_source,
        terminal,
        reason,
        detail,
    )
    .await
}

/// Shared adapter for recovery and other paths that do not hold an in-memory
/// [`super::ToolCallLifecycle`]. The caller supplies only terminal-specific
/// fields; exact omission creation, parent-version validation, transactional
/// binding, and bounded stale-head retry stay centralized here.
pub(crate) async fn terminalize_with_omission<F>(
    node: &defra_node::EmbeddedNode,
    tool_call_doc_id: &str,
    source: ToolCallState,
    terminal: ToolCallState,
    reason: ToolOutputOmissionReason,
    detail: &str,
    operation: &'static str,
    build_mutation: F,
) -> Result<bool>
where
    F: Fn(&crate::SignedDocumentVersionRef) -> String,
{
    let mut omission =
        match retain_tool_output_omission(node, tool_call_doc_id, source, terminal, reason, detail)
            .await
        {
            Ok(omission) => omission,
            Err(error) => {
                require_verified_terminal_after_publication_error(
                    node,
                    tool_call_doc_id,
                    source,
                    operation,
                    error,
                )
                .await?;
                tracing::debug!(
                    tool_call_doc_id,
                    operation,
                    expected_source = source.as_str(),
                    "adopting competing writer before omission publication"
                );
                return Ok(false);
            }
        };
    for stale_retry in 0..=crate::retry::DEFRA_DB_CONFLICT_MAX_RETRIES {
        let mutation = build_mutation(&omission);
        match super::transition::execute_transition_with_exact_evidence(
            node,
            tool_call_doc_id,
            source,
            &[super::transition::ExactToolEvidence {
                collection: "AgentToolOutputOmission",
                exact: &omission,
                require_execution_owner: true,
            }],
            &mutation,
            "update_AgentToolCall",
            operation,
        )
        .await?
        {
            super::transition::ExactEvidenceTransitionOutcome::Applied(_) => return Ok(true),
            super::transition::ExactEvidenceTransitionOutcome::Lost => return Ok(false),
            super::transition::ExactEvidenceTransitionOutcome::Stale
                if stale_retry < crate::retry::DEFRA_DB_CONFLICT_MAX_RETRIES =>
            {
                omission = match retain_tool_output_omission(
                    node,
                    tool_call_doc_id,
                    source,
                    terminal,
                    reason,
                    detail,
                )
                .await
                {
                    Ok(omission) => omission,
                    Err(error) => {
                        require_verified_terminal_after_publication_error(
                            node,
                            tool_call_doc_id,
                            source,
                            operation,
                            error,
                        )
                        .await?;
                        tracing::debug!(
                            tool_call_doc_id,
                            operation,
                            expected_source = source.as_str(),
                            "adopting competing writer before stale omission republish"
                        );
                        return Ok(false);
                    }
                };
            }
            super::transition::ExactEvidenceTransitionOutcome::Stale => anyhow::bail!(
                "AgentToolCall {tool_call_doc_id} kept changing while binding {} omission",
                reason.as_str()
            ),
        }
    }
    unreachable!("bounded exact-omission loop returns on every outcome")
}

pub(crate) async fn terminalize_with_output<F>(
    node: &defra_node::EmbeddedNode,
    tool_call_doc_id: &str,
    output: &str,
    operation: &'static str,
    build_mutation: F,
) -> Result<bool>
where
    F: Fn(&crate::SignedDocumentVersionRef, &str) -> String,
{
    let mut exact = match retain_running_output(node, tool_call_doc_id, output).await {
        Ok(exact) => exact,
        Err(error) => {
            require_verified_terminal_after_publication_error(
                node,
                tool_call_doc_id,
                ToolCallState::Running,
                operation,
                error,
            )
            .await?;
            tracing::debug!(
                tool_call_doc_id,
                operation,
                "adopting competing writer before output publication"
            );
            return Ok(false);
        }
    };
    let mut projection = retained_output_projection(node, &exact).await?;
    for stale_retry in 0..=crate::retry::DEFRA_DB_CONFLICT_MAX_RETRIES {
        let mutation = build_mutation(&exact, &projection);
        match super::transition::execute_transition_with_exact_evidence(
            node,
            tool_call_doc_id,
            ToolCallState::Running,
            &[super::transition::ExactToolEvidence {
                collection: "AgentToolResult",
                exact: &exact,
                require_execution_owner: true,
            }],
            &mutation,
            "update_AgentToolCall",
            operation,
        )
        .await?
        {
            super::transition::ExactEvidenceTransitionOutcome::Applied(_) => return Ok(true),
            super::transition::ExactEvidenceTransitionOutcome::Lost => return Ok(false),
            super::transition::ExactEvidenceTransitionOutcome::Stale
                if stale_retry < crate::retry::DEFRA_DB_CONFLICT_MAX_RETRIES =>
            {
                exact = match retain_running_output(node, tool_call_doc_id, output).await {
                    Ok(exact) => exact,
                    Err(error) => {
                        require_verified_terminal_after_publication_error(
                            node,
                            tool_call_doc_id,
                            ToolCallState::Running,
                            operation,
                            error,
                        )
                        .await?;
                        tracing::debug!(
                            tool_call_doc_id,
                            operation,
                            "adopting competing writer before stale output republish"
                        );
                        return Ok(false);
                    }
                };
                projection = retained_output_projection(node, &exact).await?;
            }
            super::transition::ExactEvidenceTransitionOutcome::Stale => anyhow::bail!(
                "AgentToolCall {tool_call_doc_id} kept changing while binding exact output"
            ),
        }
    }
    unreachable!("bounded exact-output loop returns on every outcome")
}

pub(crate) fn result_fields_fragment(result: &crate::SignedDocumentVersionRef) -> String {
    super::transition::exact_result_fields_fragment(result)
}

pub(crate) fn omission_fields_fragment(omission: &crate::SignedDocumentVersionRef) -> String {
    super::transition::exact_omission_fields_fragment(omission)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn omission_validation_fixture() -> (
        ExistingOmissionFact,
        ToolCallOmissionParentRow,
        crate::SignedDocumentVersionRef,
        crate::SignedDocumentVersionRef,
    ) {
        let parent = crate::SignedDocumentVersionRef::new(
            crate::DocumentVersionRef::new("tool-call-doc", "tool-call-cid"),
            "did:key:execution-owner",
        );
        let parent_row = ToolCallOmissionParentRow {
            doc_id: parent.version.doc_id.clone(),
            tool_call_key: "session-1:call-1".to_string(),
            agent_did: "did:key:agent".to_string(),
            requester_did: Some("did:key:requester".to_string()),
            session_id: "session-1".to_string(),
            tool_name: "test_tool".to_string(),
            args: "{}".to_string(),
            lifecycle_state: ToolCallState::Running.as_str().to_string(),
        };
        let exact = crate::SignedDocumentVersionRef::new(
            crate::DocumentVersionRef::new("omission-doc", "omission-cid"),
            parent.signer_did.clone(),
        );
        let row = ExistingOmissionFact {
            doc_id: exact.version.doc_id.clone(),
            omission_key: parent.version.composite_commit_cid.clone(),
            tool_call_key: parent_row.tool_call_key.clone(),
            tool_call_doc_id: parent.version.doc_id.clone(),
            tool_call_composite_commit_cid: parent.version.composite_commit_cid.clone(),
            tool_call_signer_did: parent.signer_did.clone(),
            agent_did: parent_row.agent_did.clone(),
            requester_did: parent_row.requester_did.clone(),
            session_id: parent_row.session_id.clone(),
            source_phase: ToolCallState::Running.as_str().to_string(),
            terminal_phase: ToolCallState::Failed.as_str().to_string(),
            reason: ToolOutputOmissionReason::ExecutionLost.as_str().to_string(),
            detail: "execution lease expired".to_string(),
        };
        (row, parent_row, parent, exact)
    }

    fn validate_fixture_row(
        row: &ExistingOmissionFact,
        parent_row: &ToolCallOmissionParentRow,
        parent: &crate::SignedDocumentVersionRef,
        exact: &crate::SignedDocumentVersionRef,
    ) -> Result<()> {
        validate_exact_omission_payload(
            row,
            exact,
            parent_row,
            parent,
            ToolCallState::Running,
            ToolCallState::Failed,
            ToolOutputOmissionReason::ExecutionLost,
            "execution lease expired",
        )
    }

    #[test]
    fn omission_reason_phase_pairs_are_closed() {
        assert!(ToolOutputOmissionReason::PreDispatchFailure
            .allows(ToolCallState::Pending, ToolCallState::Failed));
        assert!(ToolOutputOmissionReason::ApprovalDenied
            .allows(ToolCallState::AwaitingApproval, ToolCallState::Failed));
        assert!(ToolOutputOmissionReason::TimedOut
            .allows(ToolCallState::AwaitingApproval, ToolCallState::TimedOut));
        assert!(ToolOutputOmissionReason::TimedOut
            .allows(ToolCallState::Running, ToolCallState::TimedOut));
        assert!(!ToolOutputOmissionReason::ExecutionLost
            .allows(ToolCallState::Pending, ToolCallState::Failed));
        assert!(!ToolOutputOmissionReason::Cancelled
            .allows(ToolCallState::Running, ToolCallState::Completed));
    }

    #[test]
    fn exact_omission_validation_rejects_rebound_terminal_semantics() {
        let (row, parent_row, parent, exact) = omission_validation_fixture();
        validate_fixture_row(&row, &parent_row, &parent, &exact).unwrap();

        for rebound in [
            ExistingOmissionFact {
                source_phase: ToolCallState::Pending.as_str().to_string(),
                ..row.clone()
            },
            ExistingOmissionFact {
                terminal_phase: ToolCallState::Cancelled.as_str().to_string(),
                ..row.clone()
            },
            ExistingOmissionFact {
                reason: ToolOutputOmissionReason::Cancelled.as_str().to_string(),
                ..row.clone()
            },
            ExistingOmissionFact {
                detail: "different terminal cause".to_string(),
                ..row.clone()
            },
        ] {
            let error = validate_fixture_row(&rebound, &parent_row, &parent, &exact)
                .expect_err("a rebound exact CID must not retain stale terminal semantics");
            assert!(error.to_string().contains("does not license"), "{error:#}");
        }
    }

    #[test]
    fn exact_omission_validation_rejects_rebound_execution_identity() {
        let (row, parent_row, parent, exact) = omission_validation_fixture();
        for rebound in [
            ExistingOmissionFact {
                doc_id: "other-omission-doc".to_string(),
                ..row.clone()
            },
            ExistingOmissionFact {
                tool_call_doc_id: "other-tool-call-doc".to_string(),
                ..row.clone()
            },
            ExistingOmissionFact {
                tool_call_composite_commit_cid: "other-tool-call-cid".to_string(),
                ..row.clone()
            },
            ExistingOmissionFact {
                tool_call_signer_did: "did:key:other-owner".to_string(),
                ..row.clone()
            },
            ExistingOmissionFact {
                agent_did: "did:key:other-agent".to_string(),
                ..row.clone()
            },
        ] {
            validate_fixture_row(&rebound, &parent_row, &parent, &exact)
                .expect_err("a rebound exact CID must not change execution identity");
        }

        let rebound_exact =
            crate::SignedDocumentVersionRef::new(exact.version.clone(), "did:key:other-owner");
        validate_fixture_row(&row, &parent_row, &parent, &rebound_exact)
            .expect_err("the omission signer must remain the execution owner");
    }
}
