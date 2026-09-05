//! Operator resume composes the existing Goal and request owners in one transaction.
use super::*;
use crate::config_client::ConfigApplyTxn;
use crate::identity::AgentIdentity;
use crate::lifecycle::materialize::{sign_request, RequestSigner};
use crate::lifecycle::queue::{goal_continuation_identity, prepare_goal_continuation};
use crate::request_admission::{verify_request_receipt_signature, SIGNED_REQUEST_FIELDS};
use gents_protocol::row::AgentRequestRow;

#[derive(Debug, Clone, Serialize)]
pub struct GoalResumeReceipt {
    pub goal_id: String,
    pub request_id: String,
    pub doc_id: String,
    pub created: bool,
}

/// Resume the canonical goal and publish its continuation atomically.
/// `from_request_id` identifies the operation across retries, including retries
/// after the child has finished and the goal has advanced again.
pub async fn resume_goal_request(
    access: &crate::ConfigAccess,
    identity: &dyn AgentIdentity,
    agent_did: &str,
    session_id: &str,
    from_request_id: &str,
) -> Result<GoalResumeReceipt> {
    anyhow::ensure!(
        identity.did() == agent_did,
        "goal resume requires the target principal's signing identity"
    );
    let txn = match access {
        crate::ConfigAccess::Local(node) => {
            ConfigApplyTxn::begin_local(
                node,
                Some(::identity::Did::new(identity.did().to_owned())?),
            )
            .await?
        }
        crate::ConfigAccess::Graphql(_) => access.begin_apply_txn().await?,
    };
    match stage_resume(&txn, identity, agent_did, session_id, from_request_id).await {
        Ok(receipt) => {
            if receipt.created {
                txn.commit().await?;
            } else {
                txn.discard().await?;
            }
            Ok(receipt)
        }
        Err(error) => {
            let _ = txn.discard().await;
            Err(error)
        }
    }
}

async fn stage_resume(
    txn: &ConfigApplyTxn<'_>,
    identity: &dyn AgentIdentity,
    agent_did: &str,
    session_id: &str,
    from_request_id: &str,
) -> Result<GoalResumeReceipt> {
    let goal = load_canonical_goal_in_txn(txn, agent_did, session_id)
        .await?
        .context("no canonical goal exists for this owner and session")?;
    let escaped_did = escape_graphql_string(agent_did);
    let escaped_session = escape_graphql_string(session_id);
    let response = txn
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{
        agent_did: {{ _eq: "{escaped_did}" }}, session_id: {{ _eq: "{escaped_session}" }}
    }}, order: [{{ created_at: DESC }}, {{ request_id: DESC }}]) {{ {SIGNED_REQUEST_FIELDS} }} }}"#
        ))
        .await?;
    let requests: Vec<AgentRequestRow> = serde_json::from_value(
        response
            .pointer("/data/AgentRequest")
            .cloned()
            .context("request query omitted rows")?,
    )?;
    let parents: Vec<_> = requests
        .iter()
        .filter(|row| row.request_id == from_request_id)
        .collect();
    anyhow::ensure!(
        parents.len() == 1,
        "resume predecessor must uniquely belong to the goal owner and session"
    );
    let parent_row = parents[0];
    verify_request_receipt_signature(parent_row)?;
    let parent = crate::watcher::AgentRequest::try_from(parent_row.clone())?;
    let behavior = parent
        .behavior_id
        .clone()
        .context("resume predecessor has no behavior binding")?;

    // The stable key is independent of today's sequence. Its historical child
    // is authenticated before any current-status/latest-request checks.
    let key = goal_continuation_identity(&goal.goal_id, from_request_id, 1)?.retry_key;
    let escaped_key = escape_graphql_string(&key);
    let response = txn.execute(&format!(r#"{{ AgentRequest(filter: {{ retry_key: {{ _eq: "{escaped_key}" }} }}) {{ {SIGNED_REQUEST_FIELDS} }} }}"#)).await?;
    let children: Vec<AgentRequestRow> = serde_json::from_value(
        response
            .pointer("/data/AgentRequest")
            .cloned()
            .context("receipt query omitted rows")?,
    )?;
    anyhow::ensure!(children.len() <= 1, "ambiguous goal continuation receipt");
    if let Some(child) = children.first() {
        super::request_head::verify_goal_continuation_receipt(&goal, parent_row, child)?;
        return Ok(GoalResumeReceipt {
            goal_id: goal.goal_id,
            request_id: child.request_id.clone(),
            doc_id: child
                .doc_id
                .clone()
                .context("continuation receipt lacks document ID")?,
            created: false,
        });
    }

    anyhow::ensure!(
        parent_row
            .lifecycle_state
            .is_some_and(RequestLifecycleState::is_terminal),
        "resume predecessor must be terminal"
    );
    anyhow::ensure!(
        latest_goal_request(&goal, &requests).is_some_and(|row| row.doc_id == parent_row.doc_id),
        "resume predecessor is no longer the latest request"
    );
    anyhow::ensure!(
        goal_session_is_idle(&requests),
        "goal session still has unfinished requests"
    );
    let state = goal.state().context("goal has an unknown status")?;
    let post = state
        .step(GoalAction::Resume)
        .context("goal status does not allow operator resume")?;
    let sequence = goal
        .continuation_sequence()
        .checked_add(1)
        .context("goal continuation sequence exhausted")?;
    let now = Utc::now();
    let created_at = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let wrapup = post.wrapup_requested && !post.wrapup_completed;
    let content = crate::trigger_engine::goal_source::continuation_prompt(&goal, None, wrapup);
    let mut create = prepare_goal_continuation(
        &parent,
        behavior,
        &goal.goal_id,
        &content,
        sequence,
        wrapup,
        &created_at,
    )?;
    sign_request(&mut create, RequestSigner::Identity(identity)).await?;
    let doc_id = escape_graphql_string(&goal.doc_id);
    let expected_status = escape_graphql_string(&goal.status);
    let expected_sequence = goal.continuation_sequence();
    let from = escape_graphql_string(from_request_id);
    let timestamp = escape_graphql_string(&now.to_rfc3339());
    let active_time = goal.current_active_time_seconds(now);
    let response = txn.execute(&format!(r#"mutation {{ update_Goal(filter: {{
        _docID: {{ _eq: "{doc_id}" }}, agent_did: {{ _eq: "{escaped_did}" }},
        status: {{ _eq: "{expected_status}" }}, continuation_sequence: {{ _eq: {expected_sequence} }}
    }}, input: {{
        status: "{status}", continuation_sequence: {sequence}, last_continued_from_request_id: "{from}",
        consecutive_blocked_audits: {audits}, wrapup_requested: {wrapup_requested}, wrapup_completed: {wrapup_completed},
        last_blocked_request_id: null, last_blocked_reason: null, last_failure: null,
        infrastructure_retry_count: 0, completion_evidence: null,
        active_time_seconds: {active_time}, active_started_at: "{timestamp}", updated_at: "{timestamp}"
    }}) {{ _docID }} }}"#,
        status = post.status.as_str(), audits = post.blocked_audits,
        wrapup_requested = post.wrapup_requested, wrapup_completed = post.wrapup_completed)).await?;
    anyhow::ensure!(
        response
            .pointer("/data/update_Goal")
            .is_some_and(mutation_returned_rows),
        "goal changed while staging resume"
    );
    let response = txn
        .execute(&create.graphql_mutation().map_err(anyhow::Error::msg)?)
        .await?;
    let child = response
        .pointer("/data/create_AgentRequest")
        .or_else(|| response.pointer("/data/add_AgentRequest"))
        .context("continuation create omitted result")?;
    let doc_id = child
        .get("_docID")
        .or_else(|| child.get(0).and_then(|row| row.get("_docID")))
        .and_then(serde_json::Value::as_str)
        .context("continuation create omitted document ID")?
        .to_owned();
    Ok(GoalResumeReceipt {
        goal_id: goal.goal_id,
        request_id: create.request_id,
        doc_id,
        created: true,
    })
}

#[cfg(test)]
mod contract_tests;
