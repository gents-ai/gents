//! Exact, typed approval evidence shared by approval readers and writers.

use anyhow::Result;
use serde::Deserialize;

pub(crate) const EXACT_APPROVAL_SELECTION: &str =
    "approval_id approval_key tool_call_id tool_call_key tool_call_doc_id \
     tool_call_composite_commit_cid tool_call_signer_did request_id session_id agent_did \
     requester_did decision approver_did reason";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalDecisionKind {
    Approved,
    Denied,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct ExactApprovalRow {
    #[serde(rename = "_docID")]
    pub(crate) doc_id: String,
    pub(crate) approval_id: String,
    pub(crate) approval_key: String,
    pub(crate) tool_call_id: String,
    pub(crate) tool_call_key: String,
    pub(crate) tool_call_doc_id: String,
    pub(crate) tool_call_composite_commit_cid: String,
    pub(crate) tool_call_signer_did: String,
    pub(crate) request_id: Option<String>,
    pub(crate) session_id: String,
    pub(crate) agent_did: String,
    pub(crate) requester_did: Option<String>,
    pub(crate) decision: String,
    pub(crate) approver_did: String,
    pub(crate) reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedApprovalDecision {
    pub(crate) source: crate::SignedDocumentVersionRef,
    pub(crate) row: ExactApprovalRow,
    pub(crate) decision: ApprovalDecisionKind,
}

impl VerifiedApprovalDecision {
    pub(crate) fn reason(&self) -> Option<&str> {
        self.row
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
    }

    pub(crate) fn denial_message(&self, tool_name: &str) -> Result<String> {
        if self.decision != ApprovalDecisionKind::Denied {
            anyhow::bail!("approved AgentToolApproval cannot authorize a denial transition");
        }
        Ok(match self.reason() {
            Some(reason) => format!("tool call {tool_name} denied by operator: {reason}"),
            None => format!("tool call {tool_name} denied by operator"),
        })
    }

    pub(crate) fn require_binding(
        &self,
        tool_call_doc_id: &str,
        tool_call_id: &str,
        session_id: &str,
        agent_did: &str,
    ) -> Result<()> {
        if self.row.approval_key != tool_call_doc_id
            || self.row.tool_call_doc_id != tool_call_doc_id
            || self.row.tool_call_id != tool_call_id
            || self.row.session_id != session_id
            || self.row.agent_did != agent_did
        {
            anyhow::bail!("exact AgentToolApproval does not bind the expected held tool call");
        }
        Ok(())
    }

    pub(crate) fn require_identity_binding(
        &self,
        tool_call_doc_id: &str,
        tool_call_id: &str,
        agent_did: &str,
    ) -> Result<()> {
        if self.row.approval_key != tool_call_doc_id
            || self.row.tool_call_doc_id != tool_call_doc_id
            || self.row.tool_call_id != tool_call_id
            || self.row.agent_did != agent_did
        {
            anyhow::bail!("exact AgentToolApproval does not bind the expected held tool call");
        }
        Ok(())
    }

    pub(crate) fn require_decision(&self, expected: ApprovalDecisionKind) -> Result<()> {
        if self.decision != expected {
            anyhow::bail!(
                "exact AgentToolApproval decision {:?} cannot authorize {:?} transition",
                self.decision,
                expected
            );
        }
        Ok(())
    }
}

pub(crate) fn decode_verified_approval_snapshot(
    snapshot: crate::document_version::VerifiedExactDocumentSnapshot,
) -> Result<VerifiedApprovalDecision> {
    let row: ExactApprovalRow = snapshot.decode()?;
    if row.doc_id != snapshot.source.version.doc_id {
        anyhow::bail!("exact AgentToolApproval snapshot returned another physical document");
    }
    if row.approval_key != row.tool_call_doc_id {
        anyhow::bail!("AgentToolApproval logical key does not equal its physical tool-call parent");
    }
    if row.approver_did.trim().is_empty() || row.approver_did != snapshot.source.signer_did {
        anyhow::bail!("approval approver_did does not match exact verified commit signer");
    }
    let decision = match row.decision.as_str() {
        "approved" => ApprovalDecisionKind::Approved,
        "denied" => ApprovalDecisionKind::Denied,
        decision => anyhow::bail!("AgentToolApproval has unrecognized decision {decision:?}"),
    };
    Ok(VerifiedApprovalDecision {
        source: snapshot.source,
        row,
        decision,
    })
}

pub(crate) async fn load_verified_exact_approval(
    node: &defra_node::EmbeddedNode,
    approval: &crate::SignedDocumentVersionRef,
) -> Result<VerifiedApprovalDecision> {
    let snapshot = crate::document_version::verified_exact_document_snapshot_with_identity(
        node,
        "AgentToolApproval",
        &approval.version,
        EXACT_APPROVAL_SELECTION,
        None,
    )
    .await?;
    if snapshot.source != *approval {
        anyhow::bail!("AgentToolApproval exact signed snapshot does not match the bound edge");
    }
    decode_verified_approval_snapshot(snapshot)
}
