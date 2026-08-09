//! Tool-call approval client: list held calls and write verdict documents.
//!
//! Shared by the CLI (`tools holds` / `tools approve`) and the desktop
//! bridge. An operator approves by writing an `AgentToolApproval` document —
//! same shape as every other control-plane action; the runtime's verdict
//! watcher (hook/persistence/approval.rs) notices and drives the Lean-fenced
//! approve/deny edge. A physical call admits one immutable decision fact;
//! conflicting replays and replicated physical twins fail closed.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::graphql::escape_graphql_string;
use crate::session::{GraphqlExecutor as _, HttpGraphqlExecutor};

use super::{ConfigAccess, ConfigApplyTxn};

/// A tool call persisted in `awaitingApproval`, as surfaced to operators.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeldToolCall {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    pub tool_call_id: String,
    pub request_id: Option<String>,
    pub session_id: Option<String>,
    pub agent_did: Option<String>,
    pub tool_name: Option<String>,
    pub args: Option<String>,
    pub deadline_at: Option<String>,
    #[serde(default, skip_serializing)]
    status: Option<String>,
}

/// List every tool call currently held for approval, optionally scoped to one
/// agent DID.
pub async fn list_held_tool_calls(
    access: &ConfigAccess,
    agent_did: Option<&str>,
) -> Result<Vec<HeldToolCall>> {
    let agent_filter = agent_did
        .map(|did| {
            let escaped = escape_graphql_string(did);
            format!(r#", agent_did: {{ _eq: "{escaped}" }}"#)
        })
        .unwrap_or_default();
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ lifecycle_state: {{ _eq: "awaitingApproval" }}{agent_filter} }},
                order: {{ deadline_at: ASC }}
            ) {{
                _docID
                tool_call_id
                request_id
                session_id
                agent_did
                tool_name
                args
                deadline_at
                status
            }}
        }}"#
    );
    let response = access.execute(&query).await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get("AgentToolCall"))
        .cloned()
        .unwrap_or(serde_json::Value::Array(Vec::new()));
    let rows: Vec<HeldToolCall> =
        serde_json::from_value(rows).context("decode held AgentToolCall rows")?;
    Ok(rows
        .into_iter()
        .filter(|row| row.status.as_deref() != Some("forkStaging"))
        .collect())
}

/// Verdict to record for a held tool call.
#[derive(Debug, Clone)]
pub struct ToolApprovalVerdict {
    pub tool_call_doc_id: String,
    pub tool_call_id: String,
    pub agent_did: String,
    pub request_id: Option<String>,
    /// true = approved, false = denied.
    pub approve: bool,
    pub approver_did: String,
    pub reason: Option<String>,
}

#[derive(Deserialize)]
struct ApprovalCallRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    tool_call_id: String,
    tool_call_key: String,
    request_id: String,
    session_id: String,
    agent_did: String,
    requester_did: Option<String>,
    #[serde(default)]
    status: String,
    lifecycle_state: String,
}

#[derive(Deserialize)]
struct ApprovalCommitParent {
    cid: String,
    #[serde(rename = "fieldName")]
    field_name: Option<String>,
}

#[derive(Deserialize)]
struct ApprovalCommitRow {
    cid: String,
    #[serde(default)]
    heads: Vec<ApprovalCommitParent>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExistingApprovalRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    approval_id: String,
    tool_call_id: String,
    tool_call_key: String,
    tool_call_doc_id: String,
    tool_call_composite_commit_cid: String,
    tool_call_signer_did: String,
    request_id: Option<String>,
    session_id: String,
    agent_did: String,
    requester_did: Option<String>,
    decision: String,
    approver_did: String,
    reason: Option<String>,
}

async fn exact_held_call(
    access: &ConfigAccess,
    txn: &ConfigApplyTxn<'_>,
    verdict: &ToolApprovalVerdict,
) -> Result<(ApprovalCallRow, String, String)> {
    let query = format!(
        r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID tool_call_id tool_call_key request_id session_id agent_did requester_did status lifecycle_state }} }}"#,
        escape_graphql_string(&verdict.tool_call_doc_id),
    );
    let rows: Vec<ApprovalCallRow> = serde_json::from_value(
        txn.execute(&query)
            .await?
            .get("data")
            .and_then(|data| data.get("AgentToolCall"))
            .cloned()
            .unwrap_or_default(),
    )?;
    let [row] = rows.as_slice() else {
        anyhow::bail!("held tool _docID resolved to {} physical rows", rows.len());
    };
    if row.doc_id != verdict.tool_call_doc_id
        || row.tool_call_id != verdict.tool_call_id
        || row.agent_did != verdict.agent_did
    {
        anyhow::bail!("held tool _docID does not match the requested logical identity");
    }
    let query = format!(
        r#"{{ _commits(docID: ["{}"], filter: {{ fieldName: {{ _eq: "_C" }} }}) {{ cid heads {{ cid fieldName }} }} }}"#,
        escape_graphql_string(&row.doc_id)
    );
    let commits: Vec<ApprovalCommitRow> = serde_json::from_value(
        txn.execute(&query)
            .await?
            .get("data")
            .and_then(|data| data.get("_commits"))
            .cloned()
            .unwrap_or_default(),
    )?;
    let nested = commits
        .iter()
        .flat_map(|commit| commit.heads.iter())
        .filter(|head| head.field_name.as_deref() == Some("_C"))
        .map(|head| head.cid.as_str())
        .collect::<HashSet<_>>();
    let current = commits
        .iter()
        .filter(|commit| !nested.contains(commit.cid.as_str()))
        .collect::<Vec<_>>();
    let [current] = current.as_slice() else {
        anyhow::bail!(
            "held tool call has {} current composite heads",
            current.len()
        );
    };
    let signer = match access {
        ConfigAccess::Local(node) => node.verified_block_signer_did(&current.cid).await?,
        ConfigAccess::Graphql(graphql) => {
            HttpGraphqlExecutor::new(graphql.clone())
                .verified_signer_did(&current.cid)
                .await?
        }
    };
    let exact_query = format!(
        r#"{{ AgentToolCall(cid: ["{}"]) {{ _docID tool_call_id tool_call_key request_id session_id agent_did requester_did status lifecycle_state }} }}"#,
        escape_graphql_string(&current.cid),
    );
    let exact_rows: Vec<ApprovalCallRow> = serde_json::from_value(
        txn.execute(&exact_query)
            .await?
            .get("data")
            .and_then(|data| data.get("AgentToolCall"))
            .cloned()
            .unwrap_or_default(),
    )?;
    let [exact] = exact_rows.as_slice() else {
        anyhow::bail!(
            "held tool current CID reconstructed {} physical rows",
            exact_rows.len()
        );
    };
    if exact.doc_id != row.doc_id
        || exact.tool_call_id != row.tool_call_id
        || exact.tool_call_key != row.tool_call_key
        || exact.request_id != row.request_id
        || exact.session_id != row.session_id
        || exact.agent_did != row.agent_did
        || exact.requester_did != row.requester_did
        || exact.status != row.status
        || exact.lifecycle_state != row.lifecycle_state
    {
        anyhow::bail!("held tool current CID does not reconstruct the observed call");
    }
    Ok((
        exact_rows.into_iter().next().unwrap(),
        current.cid.clone(),
        signer,
    ))
}

async fn verify_approval_parent(access: &ConfigAccess, row: &ExistingApprovalRow) -> Result<()> {
    let cid = escape_graphql_string(&row.tool_call_composite_commit_cid);
    let response = access
        .execute(&format!(
            r#"{{ AgentToolCall(cid: ["{cid}"]) {{ _docID tool_call_id tool_call_key request_id session_id agent_did requester_did lifecycle_state }} }}"#
        ))
        .await?;
    let parents: Vec<ApprovalCallRow> = serde_json::from_value(
        response
            .get("data")
            .and_then(|data| data.get("AgentToolCall"))
            .cloned()
            .unwrap_or_default(),
    )?;
    match parents.as_slice() {
        [parent]
            if parent.doc_id == row.tool_call_doc_id
                && parent.tool_call_id == row.tool_call_id
                && parent.tool_call_key == row.tool_call_key
                && row.request_id.as_deref() == Some(parent.request_id.as_str())
                && parent.session_id == row.session_id
                && parent.agent_did == row.agent_did
                && parent.requester_did == row.requester_did
                && parent.lifecycle_state == "awaitingApproval" => {}
        rows => anyhow::bail!(
            "approval parent reconstructed {} rows or not the exact held physical call",
            rows.len()
        ),
    }
    let signer = match access {
        ConfigAccess::Local(node) => {
            node.verified_block_signer_did(&row.tool_call_composite_commit_cid)
                .await?
        }
        ConfigAccess::Graphql(graphql) => {
            HttpGraphqlExecutor::new(graphql.clone())
                .verified_signer_did(&row.tool_call_composite_commit_cid)
                .await?
        }
    };
    if signer != row.tool_call_signer_did {
        anyhow::bail!(
            "approval parent signer {signer} does not match pinned {}",
            row.tool_call_signer_did
        );
    }
    Ok(())
}

async fn verify_approval_fact(access: &ConfigAccess, row: &ExistingApprovalRow) -> Result<()> {
    let snapshot = match access {
        ConfigAccess::Local(node) => {
            let current = crate::document_version::verified_current_signed_document_version(
                node,
                "AgentToolApproval",
                &row.doc_id,
            )
            .await?;
            crate::document_version::verified_exact_document_snapshot_with_identity(
                node,
                "AgentToolApproval",
                &current.version,
                crate::tool_call_lifecycle::approval_evidence::EXACT_APPROVAL_SELECTION,
                None,
            )
            .await?
        }
        ConfigAccess::Graphql(graphql) => {
            let executor = HttpGraphqlExecutor::new(graphql.clone());
            let current =
                crate::document_version::verified_current_signed_document_version_with_executor(
                    &executor,
                    "AgentToolApproval",
                    &row.doc_id,
                )
                .await?;
            crate::document_version::verified_exact_document_snapshot_with_executor(
                &executor,
                "AgentToolApproval",
                &current.version,
                crate::tool_call_lifecycle::approval_evidence::EXACT_APPROVAL_SELECTION,
            )
            .await?
        }
    };
    let exact =
        crate::tool_call_lifecycle::approval_evidence::decode_verified_approval_snapshot(snapshot)?;
    if exact.row.doc_id != row.doc_id
        || exact.row.approval_id != row.approval_id
        || exact.row.tool_call_id != row.tool_call_id
        || exact.row.tool_call_key != row.tool_call_key
        || exact.row.tool_call_doc_id != row.tool_call_doc_id
        || exact.row.tool_call_composite_commit_cid != row.tool_call_composite_commit_cid
        || exact.row.tool_call_signer_did != row.tool_call_signer_did
        || exact.row.request_id != row.request_id
        || exact.row.session_id != row.session_id
        || exact.row.agent_did != row.agent_did
        || exact.row.requester_did != row.requester_did
        || exact.row.decision != row.decision
        || exact.row.approver_did != row.approver_did
        || exact.row.reason != row.reason
    {
        anyhow::bail!("approval replay row does not match its exact signed current snapshot");
    }
    Ok(())
}

async fn verify_existing_approval(access: &ConfigAccess, row: &ExistingApprovalRow) -> Result<()> {
    verify_approval_parent(access, row).await?;
    verify_approval_fact(access, row).await
}

async fn approval_rows(
    access: &ConfigAccess,
    approval_key: &str,
) -> Result<Vec<ExistingApprovalRow>> {
    let query = format!(
        r#"{{ AgentToolApproval(filter: {{ approval_key: {{ _eq: "{}" }} }}) {{ _docID approval_id tool_call_id tool_call_key tool_call_doc_id tool_call_composite_commit_cid tool_call_signer_did request_id session_id agent_did requester_did decision approver_did reason }} }}"#,
        escape_graphql_string(approval_key)
    );
    Ok(serde_json::from_value(
        access
            .execute(&query)
            .await?
            .get("data")
            .and_then(|data| data.get("AgentToolApproval"))
            .cloned()
            .unwrap_or_default(),
    )?)
}

async fn approval_rows_in_txn(
    txn: &ConfigApplyTxn<'_>,
    approval_key: &str,
) -> Result<Vec<ExistingApprovalRow>> {
    let query = format!(
        r#"{{ AgentToolApproval(filter: {{ approval_key: {{ _eq: "{}" }} }}) {{ _docID approval_id tool_call_id tool_call_key tool_call_doc_id tool_call_composite_commit_cid tool_call_signer_did request_id session_id agent_did requester_did decision approver_did reason }} }}"#,
        escape_graphql_string(approval_key)
    );
    Ok(serde_json::from_value(
        txn.execute(&query)
            .await?
            .get("data")
            .and_then(|data| data.get("AgentToolApproval"))
            .cloned()
            .unwrap_or_default(),
    )?)
}

fn approval_decision_matches(
    row: &ExistingApprovalRow,
    call: &ApprovalCallRow,
    verdict: &ToolApprovalVerdict,
    approval_id: &str,
    decision: &str,
) -> bool {
    row.approval_id == approval_id
        && row.tool_call_id == verdict.tool_call_id
        && row.tool_call_key == call.tool_call_key
        && row.tool_call_doc_id == call.doc_id
        && row.request_id.as_deref() == Some(call.request_id.as_str())
        && row.session_id == call.session_id
        && row.agent_did == verdict.agent_did
        && row.requester_did == call.requester_did
        && row.decision == decision
        && row.approver_did == verdict.approver_did
        && row.reason.as_deref().unwrap_or_default()
            == verdict.reason.as_deref().unwrap_or_default()
}

fn observe_approval_rows(
    rows: &[ExistingApprovalRow],
    call: &ApprovalCallRow,
    verdict: &ToolApprovalVerdict,
    approval_id: &str,
    decision: &str,
) -> Result<bool> {
    match rows {
        [] => Ok(false),
        [row] if approval_decision_matches(row, call, verdict, approval_id, decision) => Ok(true),
        [_] => anyhow::bail!("approval replay conflicts with immutable decision fact"),
        rows => anyhow::bail!("approval logical key has {} physical twins", rows.len()),
    }
}

async fn recover_exact_approval(
    access: &ConfigAccess,
    approval_key: &str,
    call: &ApprovalCallRow,
    verdict: &ToolApprovalVerdict,
    approval_id: &str,
    decision: &str,
) -> Result<bool> {
    let rows = approval_rows(access, approval_key).await?;
    if !observe_approval_rows(&rows, call, verdict, approval_id, decision)? {
        return Ok(false);
    }
    verify_existing_approval(access, &rows[0]).await?;
    Ok(true)
}

struct PreparedApproval {
    call: ApprovalCallRow,
    approval_id: String,
    approval_key: String,
    decision: &'static str,
    created: bool,
}

async fn prepare_tool_approval_in_txn(
    access: &ConfigAccess,
    txn: &ConfigApplyTxn<'_>,
    verdict: &ToolApprovalVerdict,
) -> Result<PreparedApproval> {
    let (call, call_cid, call_signer) = exact_held_call(access, txn, verdict).await?;
    if call.status == "forkStaging" {
        anyhow::bail!("fork-staging tool calls cannot accept operator approval decisions");
    }
    if verdict.request_id.as_deref() != Some(call.request_id.as_str()) {
        anyhow::bail!("approval request_id does not match the exact held call");
    }
    let approval_id = format!("approval-{}", call.doc_id);
    let approval_key = call.doc_id.clone();
    let escaped_approval_id = escape_graphql_string(&approval_id);
    let escaped_tool_call_id = escape_graphql_string(&verdict.tool_call_id);
    let escaped_agent_did = escape_graphql_string(&verdict.agent_did);
    let escaped_request_id = escape_graphql_string(&call.request_id);
    let escaped_session_id = escape_graphql_string(&call.session_id);
    let requester_did_field =
        crate::session::requester_did_create_field(call.requester_did.as_deref());
    let escaped_approver_did = escape_graphql_string(&verdict.approver_did);
    let decision = if verdict.approve {
        "approved"
    } else {
        "denied"
    };
    let reason_field = verdict
        .reason
        .as_deref()
        .map(|reason| {
            let escaped = escape_graphql_string(reason);
            format!(r#"reason: "{escaped}","#)
        })
        .unwrap_or_default();
    let created_at = chrono::Utc::now().to_rfc3339();

    let existing = approval_rows_in_txn(txn, &approval_key).await?;
    if observe_approval_rows(&existing, &call, verdict, &approval_id, decision)? {
        return Ok(PreparedApproval {
            call,
            approval_id,
            approval_key,
            decision,
            created: false,
        });
    }
    if call.lifecycle_state != "awaitingApproval" {
        anyhow::bail!("new approval fact requires an awaitingApproval call");
    }
    let mutation = format!(
        r#"mutation {{
            create_AgentToolApproval(input: {{
                approval_id: "{escaped_approval_id}",
                approval_key: "{}",
                tool_call_id: "{escaped_tool_call_id}",
                tool_call_key: "{}",
                tool_call_doc_id: "{}",
                tool_call_composite_commit_cid: "{}",
                tool_call_signer_did: "{}",
                request_id: "{escaped_request_id}",
                session_id: "{escaped_session_id}",
                agent_did: "{escaped_agent_did}",
                {requester_did_field}
                decision: "{decision}",
                approver_did: "{escaped_approver_did}",
                {reason_field}
                created_at: "{created_at}"
            }}) {{ _docID }}
        }}"#,
        escape_graphql_string(&approval_key),
        escape_graphql_string(&call.tool_call_key),
        escape_graphql_string(&call.doc_id),
        escape_graphql_string(&call_cid),
        escape_graphql_string(&call_signer),
    );
    txn.execute_once(&mutation)
        .await
        .context("create AgentToolApproval")?;
    let created = approval_rows_in_txn(txn, &approval_key).await?;
    if !observe_approval_rows(&created, &call, verdict, &approval_id, decision)? {
        anyhow::bail!("approval mutation returned without one exact decision fact");
    }
    Ok(PreparedApproval {
        call,
        approval_id,
        approval_key,
        decision,
        created: true,
    })
}

/// Write the `AgentToolApproval` decision document. Returns the approval_id.
pub async fn write_tool_approval(
    access: &ConfigAccess,
    verdict: &ToolApprovalVerdict,
) -> Result<String> {
    // Establish the exact mutation signer before loading or writing approval
    // evidence. In particular, a remote request identity that differs from
    // the node signer must fail before an immutable fact can be created.
    let mutation_signer = access
        .known_mutation_signer_did()
        .await
        .context("resolving exact approval mutation signer")?;
    if verdict.approver_did != mutation_signer {
        anyhow::bail!(
            "approval approver_did {} does not match mutation signer {mutation_signer}",
            verdict.approver_did
        );
    }

    let txn = access
        .begin_apply_txn()
        .await
        .context("begin exact approval transaction")?;
    let prepared = match prepare_tool_approval_in_txn(access, &txn, verdict).await {
        Ok(prepared) => prepared,
        Err(error) => {
            if let Err(discard_error) = txn.discard().await {
                tracing::warn!(%discard_error, "failed to discard rejected approval transaction");
            }
            return Err(error);
        }
    };

    if !prepared.created {
        if let Err(discard_error) = txn.discard().await {
            tracing::warn!(%discard_error, "failed to discard approval replay transaction");
        }
        if recover_exact_approval(
            access,
            &prepared.approval_key,
            &prepared.call,
            verdict,
            &prepared.approval_id,
            prepared.decision,
        )
        .await?
        {
            return Ok(prepared.approval_id);
        }
        anyhow::bail!("approval replay disappeared after transaction snapshot");
    }

    // `commit` consumes the DefraDB transaction even when finalization returns
    // an explicit conflict. Do not attempt a follow-up discard: for a lost HTTP
    // response the outcome is ambiguous, and exact immutable-key recovery is
    // the only safe way to distinguish a committed decision from no decision.
    let commit_error = txn.commit().await.err();
    match recover_exact_approval(
        access,
        &prepared.approval_key,
        &prepared.call,
        verdict,
        &prepared.approval_id,
        prepared.decision,
    )
    .await
    {
        Ok(true) => Ok(prepared.approval_id),
        Ok(false) => match commit_error {
            Some(error) => Err(error).context("commit exact approval transaction"),
            None => anyhow::bail!("committed approval transaction has no exact decision fact"),
        },
        Err(recovery_error) => match commit_error {
            Some(error) => Err(anyhow::anyhow!(
                "commit exact approval transaction failed: {error:#}; exact recovery failed: {recovery_error:#}"
            )),
            None => Err(recovery_error).context("verify committed exact approval fact"),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::identity::{AgentIdentity as _, KeyIdentity};

    use super::*;

    async fn remote_approval_node_with_bridge_binding(
        name: &str,
        bind_bridge: bool,
    ) -> (
        Arc<defra_node::EmbeddedNode>,
        Arc<KeyIdentity>,
        String,
        tempfile::TempDir,
    ) {
        let reserved = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = reserved.local_addr().unwrap().port();
        drop(reserved);
        let key_dir = tempfile::Builder::new().prefix(name).tempdir().unwrap();
        let identity =
            Arc::new(KeyIdentity::load_or_create(key_dir.path().join("node.key"), None).unwrap());
        let signed_block_http = crate::signed_block_http::SignedBlockHttpBridge::new();
        let node = Arc::new(
            defra_node::EmbeddedNode::builder()
                .with_node_identity_did(identity.did())
                .with_http(
                    defra_node::HttpConfig::new(port).with_extra_routes(signed_block_http.router()),
                )
                .build()
                .await
                .unwrap(),
        );
        if bind_bridge {
            signed_block_http.bind(&node).unwrap();
        }
        crate::ensure_schemas(&node).await.unwrap();
        let endpoint = format!("http://127.0.0.1:{port}/api/v0/graphql");
        let client = reqwest::Client::new();
        for _ in 0..50 {
            if client
                .get(format!("http://127.0.0.1:{port}/api/v0/node/identity"))
                .send()
                .await
                .is_ok()
            {
                return (node, identity, endpoint, key_dir);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("remote approval DefraDB HTTP endpoint did not become ready");
    }

    async fn remote_approval_node(
        name: &str,
    ) -> (
        Arc<defra_node::EmbeddedNode>,
        Arc<KeyIdentity>,
        String,
        tempfile::TempDir,
    ) {
        remote_approval_node_with_bridge_binding(name, true).await
    }

    async fn create_held_call(
        node: &defra_node::EmbeddedNode,
        suffix: &str,
        tool_call_id: &str,
    ) -> String {
        let deadline = (chrono::Utc::now() + chrono::Duration::seconds(60)).to_rfc3339();
        let response = node
            .execute(&format!(
                r#"mutation {{
                    create_AgentToolCall(input: {{
                        tool_call_key: "session-{suffix}:{tool_call_id}",
                        request_id: "req-{suffix}",
                        session_id: "session-{suffix}",
                        agent_did: "did:test:general",
                        message_sequence: 1,
                        tool_name: "guarded",
                        tool_call_id: "{tool_call_id}",
                        args: "{{}}",
                        result: "",
                        status: "called",
                        lifecycle_state: "awaitingApproval",
                        started_at: null,
                        deadline_at: "{deadline}"
                    }}) {{ _docID }}
                }}"#
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let query = node
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ tool_call_key: {{ _eq: "session-{suffix}:{tool_call_id}" }} }}) {{ _docID }} }}"#
            ))
            .await;
        assert!(!query.has_errors(), "{:?}", query.errors);
        query.data.as_ref().unwrap()["AgentToolCall"]
            .as_array()
            .and_then(|rows| rows.first())
            .and_then(|row| row["_docID"].as_str())
            .expect("created held call doc id")
            .to_string()
    }

    #[tokio::test]
    async fn approval_transaction_conflicts_if_held_call_advances_before_commit() {
        let data_path =
            std::env::temp_dir().join(format!("approval-ssi-conflict-{}", uuid::Uuid::new_v4()));
        let signing_identity = crate::test_support::signed_test_identity("approval-ssi-node");
        let node = Arc::new(
            defra_node::EmbeddedNode::builder()
                .with_node_identity_did(signing_identity.did())
                .data_path(&data_path)
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_schemas(&node).await.unwrap();
        let call_doc_id = create_held_call(&node, "ssi-conflict", "call-ssi-conflict").await;
        let access = ConfigAccess::Local(node.clone());
        let verdict = ToolApprovalVerdict {
            tool_call_doc_id: call_doc_id.clone(),
            tool_call_id: "call-ssi-conflict".to_string(),
            agent_did: "did:test:general".to_string(),
            request_id: Some("req-ssi-conflict".to_string()),
            approve: true,
            approver_did: signing_identity.did().to_string(),
            reason: None,
        };

        let txn = access.begin_apply_txn().await.unwrap();
        let prepared = prepare_tool_approval_in_txn(&access, &txn, &verdict)
            .await
            .unwrap();
        assert!(prepared.created);

        let transition = node
            .execute(&format!(
                r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: "running" }}) {{ _docID }} }}"#,
                escape_graphql_string(&call_doc_id)
            ))
            .await;
        assert!(!transition.has_errors(), "{:?}", transition.errors);

        let error = txn
            .commit()
            .await
            .expect_err("SSI must reject approval based on a superseded held snapshot");
        assert!(
            error.to_string().to_ascii_lowercase().contains("conflict"),
            "unexpected commit error: {error:#}"
        );
        assert!(
            approval_rows(&access, &call_doc_id)
                .await
                .unwrap()
                .is_empty(),
            "conflicted transaction must not publish its approval fact"
        );

        node.shutdown().await;
        let _ = std::fs::remove_dir_all(&data_path);
    }

    #[tokio::test]
    async fn approval_replay_rejects_fields_rebound_to_a_different_exact_snapshot() {
        let identity = crate::test_support::signed_test_identity("approval-replay-exact-snapshot");
        let node = Arc::new(
            defra_node::EmbeddedNode::builder()
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_schemas(&node).await.unwrap();
        let call_doc_id =
            create_held_call(&node, "replay-exact-snapshot", "call-replay-exact").await;
        let access = ConfigAccess::Local(node.clone());
        write_tool_approval(
            &access,
            &ToolApprovalVerdict {
                tool_call_doc_id: call_doc_id.clone(),
                tool_call_id: "call-replay-exact".to_string(),
                agent_did: "did:test:general".to_string(),
                request_id: Some("req-replay-exact-snapshot".to_string()),
                approve: false,
                approver_did: identity.did().to_string(),
                reason: Some("denied exactly".to_string()),
            },
        )
        .await
        .unwrap();

        let rows = approval_rows(&access, &call_doc_id).await.unwrap();
        let [exact] = rows.as_slice() else {
            panic!("expected one approval fact, got {}", rows.len());
        };
        verify_approval_fact(&access, exact)
            .await
            .expect("exact replay row");

        let mut rebound = exact.clone();
        rebound.decision = "approved".to_string();
        rebound.reason = None;
        let error = verify_approval_fact(&access, &rebound)
            .await
            .expect_err("same-signer fields from another observation must not be rebound");
        assert!(
            format!("{error:#}").contains("does not match its exact signed current snapshot"),
            "unexpected exact-replay error: {error:#}"
        );

        node.shutdown().await;
    }

    #[tokio::test]
    async fn write_and_list_round_trip_against_local_node() {
        let data_path =
            std::env::temp_dir().join(format!("agent-approval-client-{}", uuid::Uuid::new_v4()));
        let signing_identity =
            crate::test_support::signed_test_identity("agent-approval-client-node");
        let node = defra_node::EmbeddedNode::builder()
            .with_node_identity_did(signing_identity.did())
            .data_path(&data_path)
            .build()
            .await
            .unwrap();
        crate::ensure_schemas(&node).await.unwrap();
        let access = ConfigAccess::Local(std::sync::Arc::new(node));

        // Persist a held row shaped like the runtime's hold_for_approval.
        let deadline = (chrono::Utc::now() + chrono::Duration::seconds(60)).to_rfc3339();
        let seeded = access
            .execute(&format!(
                r#"mutation {{
                    held: create_AgentToolCall(input: {{
                        tool_call_key: "session-client:call-client",
                        request_id: "req-client",
                        session_id: "session-client",
                        agent_did: "did:test:general",
                        message_sequence: 1,
                        tool_name: "guarded",
                        tool_call_id: "call-client",
                        args: "{{}}",
                        result: "",
                        status: "called",
                        lifecycle_state: "awaitingApproval",
                        started_at: null,
                        deadline_at: "{deadline}"
                    }}) {{ _docID }}
                    staging: create_AgentToolCall(input: {{
                        tool_call_key: "session-client:call-client-staging",
                        request_id: "req-client",
                        session_id: "session-client",
                        agent_did: "did:test:general",
                        message_sequence: 2,
                        tool_name: "guarded-staging",
                        tool_call_id: "call-client-staging",
                        args: "{{}}",
                        result: "",
                        status: "forkStaging",
                        lifecycle_state: "awaitingApproval",
                        started_at: null,
                        deadline_at: "{deadline}",
                        fork_source_doc_id: "source-staging-doc",
                        fork_source_composite_commit_cid: "source-staging-cid",
                        fork_source_signer_did: "did:test:source"
                    }}) {{ _docID }}
                }}"#
            ))
            .await
            .unwrap();

        let staging_doc_id = seeded
            .pointer("/data/staging/_docID")
            .or_else(|| seeded.pointer("/data/staging/0/_docID"))
            .and_then(serde_json::Value::as_str)
            .expect("staging call doc id")
            .to_string();

        let held = list_held_tool_calls(&access, Some("did:test:general"))
            .await
            .unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].tool_call_id, "call-client");
        assert_eq!(held[0].tool_name.as_deref(), Some("guarded"));
        let staging_error = write_tool_approval(
            &access,
            &ToolApprovalVerdict {
                tool_call_doc_id: staging_doc_id,
                tool_call_id: "call-client-staging".to_string(),
                agent_did: "did:test:general".to_string(),
                request_id: Some("req-client".to_string()),
                approve: true,
                approver_did: signing_identity.did().to_string(),
                reason: None,
            },
        )
        .await
        .expect_err("fork staging must not accept an operator decision");
        assert!(
            staging_error.to_string().contains("fork-staging"),
            "{staging_error:#}"
        );
        let call_ref = crate::document_version::verified_current_signed_document_version(
            match &access {
                ConfigAccess::Local(node) => node,
                ConfigAccess::Graphql(_) => unreachable!("test uses an embedded node"),
            },
            "AgentToolCall",
            &held[0].doc_id,
        )
        .await
        .unwrap();

        let approval_id = write_tool_approval(
            &access,
            &ToolApprovalVerdict {
                tool_call_doc_id: held[0].doc_id.clone(),
                tool_call_id: "call-client".to_string(),
                agent_did: "did:test:general".to_string(),
                request_id: Some("req-client".to_string()),
                approve: false,
                approver_did: call_ref.signer_did.clone(),
                reason: Some("blocked in test".to_string()),
            },
        )
        .await
        .unwrap();
        assert_eq!(approval_id, format!("approval-{}", held[0].doc_id));

        // The call is mutable: after the decision is attached its current CID
        // advances. An identical approval replay must still validate the
        // stored historical parent snapshot and converge on the same fact.
        access
            .execute(&format!(
                r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: "running" }}) {{ _docID }} }}"#,
                escape_graphql_string(&held[0].doc_id)
            ))
            .await
            .unwrap();
        let replayed = write_tool_approval(
            &access,
            &ToolApprovalVerdict {
                tool_call_doc_id: held[0].doc_id.clone(),
                tool_call_id: "call-client".to_string(),
                agent_did: "did:test:general".to_string(),
                request_id: Some("req-client".to_string()),
                approve: false,
                approver_did: call_ref.signer_did,
                reason: Some("blocked in test".to_string()),
            },
        )
        .await
        .unwrap();
        assert_eq!(replayed, approval_id);

        let decision = access
            .execute(
                r#"{ AgentToolApproval(filter: { tool_call_id: { _eq: "call-client" } }) { decision reason approver_did } }"#,
            )
            .await
            .unwrap();
        let rows = decision
            .get("data")
            .and_then(|data| data.get("AgentToolApproval"))
            .and_then(|rows| rows.as_array())
            .cloned()
            .unwrap_or_default();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get("decision").and_then(|value| value.as_str()),
            Some("denied")
        );

        let _ = std::fs::remove_dir_all(&data_path);
    }

    #[tokio::test]
    async fn remote_write_records_the_proven_endpoint_signer_as_approver() {
        let (node, node_identity, endpoint, _key_dir) =
            remote_approval_node("approval-remote-exact").await;
        let node_did = node_identity.did().to_string();
        let call_doc_id = create_held_call(&node, "remote-exact", "call-remote-exact").await;
        let access = ConfigAccess::Graphql(
            super::super::AuthenticatedGraphql::new(endpoint, node_identity)
                .await
                .unwrap(),
        );

        let approval_id = write_tool_approval(
            &access,
            &ToolApprovalVerdict {
                tool_call_doc_id: call_doc_id.clone(),
                tool_call_id: "call-remote-exact".to_string(),
                agent_did: "did:test:general".to_string(),
                request_id: Some("req-remote-exact".to_string()),
                approve: true,
                approver_did: node_did.clone(),
                reason: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(approval_id, format!("approval-{call_doc_id}"));

        let local = ConfigAccess::Local(node.clone());
        let rows = approval_rows(&local, &call_doc_id).await.unwrap();
        let [row] = rows.as_slice() else {
            panic!(
                "expected one exact remote approval fact, got {}",
                rows.len()
            );
        };
        assert_eq!(row.approver_did, node_did);
        verify_existing_approval(&local, row).await.unwrap();

        node.shutdown().await;
    }

    #[tokio::test]
    async fn signed_block_route_requires_node_identity_and_verifies_exact_cid() {
        let (node, node_identity, endpoint, _key_dir) =
            remote_approval_node("approval-signed-block-auth").await;
        let call_doc_id = create_held_call(&node, "signed-block-auth", "call-auth").await;
        let exact = crate::document_version::verified_current_signed_document_version(
            &node,
            "AgentToolCall",
            &call_doc_id,
        )
        .await
        .unwrap();
        let api_base = crate::config_client::graphql_api_base(&endpoint).unwrap();
        let signed_url = format!(
            "{api_base}{}?cid={}",
            crate::signed_block_http::SIGNED_BLOCK_HTTP_SUFFIX,
            exact.version.composite_commit_cid
        );

        let anonymous = reqwest::Client::new()
            .get(&signed_url)
            .send()
            .await
            .unwrap();
        assert_eq!(anonymous.status(), reqwest::StatusCode::UNAUTHORIZED);

        let wrong_key_dir = tempfile::tempdir().unwrap();
        let wrong_identity = Arc::new(
            KeyIdentity::load_or_create(wrong_key_dir.path().join("wrong.key"), None).unwrap(),
        );
        let wrong = super::super::AuthenticatedGraphql::new(endpoint.clone(), wrong_identity)
            .await
            .unwrap()
            .get(&signed_url)
            .await
            .unwrap();
        assert_eq!(wrong.status(), reqwest::StatusCode::FORBIDDEN);

        let authenticated =
            super::super::AuthenticatedGraphql::new(endpoint, node_identity.clone())
                .await
                .unwrap();
        let response = authenticated.get(&signed_url).await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(
            HttpGraphqlExecutor::new(authenticated)
                .verified_signer_did(&exact.version.composite_commit_cid)
                .await
                .unwrap(),
            node_identity.did()
        );

        node.shutdown().await;
    }

    #[tokio::test]
    async fn signed_block_route_fails_closed_until_bridge_is_bound() {
        let (node, node_identity, endpoint, _key_dir) =
            remote_approval_node_with_bridge_binding("approval-signed-block-unbound", false).await;
        let api_base = crate::config_client::graphql_api_base(&endpoint).unwrap();
        let signed_url = format!(
            "{api_base}{}?cid=bafy-unbound",
            crate::signed_block_http::SIGNED_BLOCK_HTTP_SUFFIX
        );
        let authenticated = super::super::AuthenticatedGraphql::new(endpoint, node_identity)
            .await
            .unwrap();
        let response = authenticated.get(signed_url).await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);

        node.shutdown().await;
    }

    #[tokio::test]
    async fn remote_write_targets_exact_doc_when_provider_call_ids_collide() {
        let (node, node_identity, endpoint, _key_dir) =
            remote_approval_node("approval-remote-call-id-collision").await;
        let node_did = node_identity.did().to_string();
        let first_doc_id = create_held_call(&node, "collision-a", "provider-call").await;
        let target_doc_id = create_held_call(&node, "collision-b", "provider-call").await;
        let access = ConfigAccess::Graphql(
            super::super::AuthenticatedGraphql::new(endpoint, node_identity)
                .await
                .unwrap(),
        );

        write_tool_approval(
            &access,
            &ToolApprovalVerdict {
                tool_call_doc_id: target_doc_id.clone(),
                tool_call_id: "provider-call".to_string(),
                agent_did: "did:test:general".to_string(),
                request_id: Some("req-collision-b".to_string()),
                approve: true,
                approver_did: node_did,
                reason: None,
            },
        )
        .await
        .unwrap();

        let local = ConfigAccess::Local(node.clone());
        assert!(approval_rows(&local, &first_doc_id)
            .await
            .unwrap()
            .is_empty());
        let rows = approval_rows(&local, &target_doc_id).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tool_call_doc_id, target_doc_id);

        node.shutdown().await;
    }

    #[tokio::test]
    async fn remote_write_fails_before_fact_when_request_and_node_signers_differ() {
        let (node, _node_identity, endpoint, _node_key_dir) =
            remote_approval_node("approval-remote-mismatch-node").await;
        let call_doc_id = create_held_call(&node, "remote-mismatch", "call-remote-mismatch").await;
        let client_key_dir = tempfile::tempdir().unwrap();
        let client_identity = Arc::new(
            KeyIdentity::load_or_create(client_key_dir.path().join("client.key"), None).unwrap(),
        );
        let client_did = client_identity.did().to_string();
        let access = ConfigAccess::Graphql(
            super::super::AuthenticatedGraphql::new(endpoint, client_identity)
                .await
                .unwrap(),
        );

        let error = write_tool_approval(
            &access,
            &ToolApprovalVerdict {
                tool_call_doc_id: call_doc_id.clone(),
                tool_call_id: "call-remote-mismatch".to_string(),
                agent_did: "did:test:general".to_string(),
                request_id: Some("req-remote-mismatch".to_string()),
                approve: true,
                approver_did: client_did,
                reason: None,
            },
        )
        .await
        .expect_err("ambiguous remote signer must fail before immutable approval creation");
        assert!(
            format!("{error:#}").contains("remote mutation signer is ambiguous"),
            "unexpected mismatch error: {error:#}"
        );

        let local = ConfigAccess::Local(node.clone());
        assert!(
            approval_rows(&local, &call_doc_id)
                .await
                .unwrap()
                .is_empty(),
            "signer mismatch must not leave an immutable approval fact"
        );

        node.shutdown().await;
    }
}
