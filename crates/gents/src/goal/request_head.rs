//! Shared validation of Goal continuation receipts and request ordering.
use super::*;
use crate::lifecycle::queue::{goal_continuation_identity, prepare_goal_continuation};
use crate::request_admission::{
    verify_request_receipt_signature, verify_runtime_local_control_receipt,
};
use gents_protocol::request_admission::RequestPurpose;
use gents_protocol::row::AgentRequestRow;

/// Authenticate causal ancestry independently of the current producer defaults.
/// Historical children can have different inherited optional fields; their
/// original target signature still binds the exact physical predecessor.
pub(crate) fn verify_goal_continuation_edge(
    node_did: &str,
    session_id: &str,
    goal_id: &str,
    parent_row: &AgentRequestRow,
    child: &AgentRequestRow,
) -> Result<(i64, bool)> {
    anyhow::ensure!(
        parent_row.purpose == Some(RequestPurpose::Normal)
            && child.purpose == Some(RequestPurpose::Normal)
            && parent_row.node_did.as_deref() == Some(node_did)
            && parent_row.session_id.as_deref() == Some(session_id)
            && child.session_id.as_deref() == Some(session_id),
        "continuation predecessor is outside the goal owner/session"
    );
    verify_request_receipt_signature(parent_row)?;
    let parent_doc = parent_row
        .doc_id
        .as_deref()
        .context("continuation predecessor has no document ID")?;
    verify_runtime_local_control_receipt(
        child,
        node_did,
        &parent_row.request_id,
        parent_row.requester_did.as_deref().unwrap_or(node_did),
    )?;
    anyhow::ensure!(
        child.caused_by_parent_request_doc_id.as_deref() == Some(parent_doc)
            && child
                .doc_id
                .as_deref()
                .is_some_and(|id| !id.is_empty() && id != parent_doc)
            && child.caused_by_trigger_kind.as_deref() == Some(GOAL_TRIGGER_KIND)
            && child.caused_by_trigger_id.as_deref() == Some(goal_id),
        "continuation receipt has a different physical goal edge"
    );
    let facts = child
        .input
        .as_ref()
        .and_then(|input| input.goal_continuation.as_ref())
        .context("continuation receipt lacks its original issuance facts")?;
    let sequence = facts.sequence;
    let identity = goal_continuation_identity(goal_id, &parent_row.request_id, sequence)?;
    anyhow::ensure!(
        child.request_id == identity.request_id
            && child.retry_key.as_deref() == Some(identity.retry_key.as_str()),
        "continuation receipt has a different deterministic identity"
    );
    let wrapup = facts.wrapup;
    Ok((sequence, wrapup))
}

pub(super) fn verify_goal_continuation_receipt(
    goal: &GoalDocument,
    parent_row: &AgentRequestRow,
    child: &AgentRequestRow,
) -> Result<()> {
    let (sequence, wrapup) = verify_goal_continuation_edge(
        &goal.node_did,
        &goal.session_id,
        &goal.goal_id,
        parent_row,
        child,
    )?;
    let parent = crate::watcher::AgentRequest::try_from(parent_row.clone())?;
    let agent_id = parent.agent_id.clone();
    let expected = prepare_goal_continuation(
        &parent,
        agent_id,
        &goal.goal_id,
        child
            .content
            .as_deref()
            .context("continuation receipt lacks content")?,
        sequence,
        wrapup,
        child
            .created_at
            .as_deref()
            .context("continuation receipt lacks creation time")?,
        child
            .request_hop
            .and_then(|hop| u32::try_from(hop).ok())
            .context("continuation receipt lacks its hop")?,
    )?;
    let actual: GoalBackedRequestFingerprint =
        serde_json::from_value(serde_json::to_value(child)?)?;
    anyhow::ensure!(
        actual == GoalBackedRequestFingerprint::from_create(&expected)?,
        "continuation receipt conflicts with the goal and physical predecessor binding"
    );
    Ok(())
}

/// Preserve canonical query order among heads, but never select an authenticated
/// Goal continuation's physical ancestor ahead of that child. Only matching
/// candidate edges pay for signature verification; ordinary latest leaves do not.
pub(crate) fn latest_goal_request<'a>(
    goal: &GoalDocument,
    rows: &'a [AgentRequestRow],
) -> Option<&'a AgentRequestRow> {
    let members = goal
        .assignment_root_request_doc_id
        .as_deref()
        .map(|root| {
            authenticated_goal_request_members(&goal.node_did, &goal.session_id, root, rows)
        })
        .transpose()
        .ok()?;
    latest_scoped_request(
        &goal.node_did,
        &goal.session_id,
        Some(&goal.goal_id),
        rows,
        members
            .as_ref()
            .map(|membership| membership.member_doc_ids.as_slice()),
    )
}

/// Preserve original signed physical ancestry across canonical Goal replacement.
/// Association with a current Goal is checked separately by the graph owner.
pub(crate) fn latest_authenticated_session_request<'a>(
    node_did: &str,
    session_id: &str,
    rows: &'a [AgentRequestRow],
) -> Option<&'a AgentRequestRow> {
    latest_scoped_request(node_did, session_id, None, rows, None)
}

fn latest_scoped_request<'a>(
    node_did: &str,
    session_id: &str,
    goal_id: Option<&str>,
    rows: &'a [AgentRequestRow],
    allowed_docs: Option<&[String]>,
) -> Option<&'a AgentRequestRow> {
    let in_scope = |row: &&AgentRequestRow| {
        allowed_docs.is_none_or(|docs| row.doc_id.as_ref().is_some_and(|doc| docs.contains(doc)))
            && row.purpose == Some(RequestPurpose::Normal)
            && row.node_did.as_deref() == Some(node_did)
            && row.session_id.as_deref() == Some(session_id)
    };
    rows.iter().filter(in_scope).find(|parent| {
        !rows.iter().filter(in_scope).any(|child| {
            let Some(original_goal_id) = child.caused_by_trigger_id.as_deref() else {
                return false;
            };
            parent.doc_id.is_some()
                && child.doc_id != parent.doc_id
                && child.caused_by_parent_request_doc_id == parent.doc_id
                && child.caused_by_parent_request_id.as_deref() == Some(parent.request_id.as_str())
                && child.caused_by_trigger_kind.as_deref() == Some(GOAL_TRIGGER_KIND)
                && goal_id.is_none_or(|expected| expected == original_goal_id)
                && verify_goal_continuation_edge(
                    node_did,
                    session_id,
                    original_goal_id,
                    parent,
                    child,
                )
                .is_ok()
        })
    })
}

/// Read-only membership of one admitted request and its authenticated Goal
/// continuations. The entry supplies activation context; this grants no new
/// execution or document permission and does not require a GraphRun.
pub(crate) struct AuthenticatedGoalRequestMembers<'a> {
    pub(crate) entry: &'a AgentRequestRow,
    pub(crate) member_doc_ids: Vec<String>,
    /// Verified physical Goal edges within this membership, child -> parent.
    pub(crate) parents: std::collections::BTreeMap<String, String>,
}

/// `rows` must include complete signed request projections for the session.
/// Invalid ancestry of the current request is an error. Unrelated or malformed
/// candidate rows cannot contribute writes to its logical invocation. Each
/// historical edge retains its own signed Goal ID across Goal replacement.
pub(crate) fn authenticated_goal_request_members<'a>(
    node_did: &str,
    session_id: &str,
    current_doc_id: &str,
    rows: &'a [AgentRequestRow],
) -> Result<AuthenticatedGoalRequestMembers<'a>> {
    let mut by_doc = std::collections::HashMap::new();
    for row in rows.iter().filter(|row| {
        row.node_did.as_deref() == Some(node_did) && row.session_id.as_deref() == Some(session_id)
    }) {
        let Some(doc) = row.doc_id.as_deref().filter(|doc| !doc.is_empty()) else {
            continue;
        };
        anyhow::ensure!(
            by_doc.insert(doc, row).is_none(),
            "duplicate physical request document in ancestry observation"
        );
    }
    let current = *by_doc
        .get(current_doc_id)
        .context("current request is absent from its owner/session observation")?;
    let entry = authenticated_entry(node_did, session_id, current, &by_doc)?;
    let entry_doc = entry
        .doc_id
        .as_deref()
        .context("entry has no document ID")?;
    let mut member_doc_ids = by_doc
        .iter()
        .filter_map(|(doc, row)| {
            authenticated_entry(node_did, session_id, row, &by_doc)
                .ok()
                .filter(|root| root.doc_id.as_deref() == Some(entry_doc))
                .map(|_| (*doc).to_owned())
        })
        .collect::<Vec<_>>();
    member_doc_ids.sort_unstable();
    let mut parents = std::collections::BTreeMap::new();
    for doc in &member_doc_ids {
        if doc == entry_doc {
            continue;
        }
        // Successful resolution to entry already verified this exact edge.
        let parent = by_doc[doc.as_str()]
            .caused_by_parent_request_doc_id
            .as_deref()
            .context("authenticated Goal member has no physical parent")?;
        parents.insert(doc.clone(), parent.to_owned());
    }
    Ok(AuthenticatedGoalRequestMembers {
        entry,
        member_doc_ids,
        parents,
    })
}

fn authenticated_entry<'a>(
    node_did: &str,
    session_id: &str,
    mut row: &'a AgentRequestRow,
    by_doc: &std::collections::HashMap<&'a str, &'a AgentRequestRow>,
) -> Result<&'a AgentRequestRow> {
    let mut visited = std::collections::HashSet::new();
    loop {
        let doc = row
            .doc_id
            .as_deref()
            .context("request has no document ID")?;
        anyhow::ensure!(visited.insert(doc), "cyclic Goal request ancestry");
        if row.caused_by_trigger_kind.as_deref() != Some(GOAL_TRIGGER_KIND) {
            verify_request_receipt_signature(row)?;
            return Ok(row);
        }
        let goal_id = row
            .caused_by_trigger_id
            .as_deref()
            .context("Goal continuation has no original Goal ID")?;
        let parent_doc = row
            .caused_by_parent_request_doc_id
            .as_deref()
            .context("Goal continuation has no physical parent")?;
        let parent = *by_doc
            .get(parent_doc)
            .context("Goal continuation parent is absent from its owner/session observation")?;
        verify_goal_continuation_edge(node_did, session_id, goal_id, parent, row)?;
        row = parent;
    }
}

/// A continuation cannot bypass an unfinished or unrecognized request row.
pub(crate) fn goal_session_is_idle(rows: &[AgentRequestRow]) -> bool {
    rows.iter().all(|row| {
        row.purpose == Some(RequestPurpose::TitleAudit)
            || row
                .lifecycle_state
                .is_some_and(RequestLifecycleState::is_terminal)
    })
}

#[cfg(test)]
#[path = "request_head_tests.rs"]
mod tests;

pub(crate) fn assignment_allows(assigned: Option<&str>, observed: Option<&str>) -> bool {
    assigned.is_none_or(|root| observed == Some(root))
}

pub(super) async fn retire_stale_assignment_continuations_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    goal: &GoalDocument,
    only_doc: Option<&str>,
    now: &str,
) -> Result<bool> {
    let Some(root) = goal.assignment_root_request_doc_id.as_deref() else {
        return Ok(true);
    };
    let owner = escape_graphql_string(&goal.node_did);
    let session = escape_graphql_string(&goal.session_id);
    let response = txn.execute_local_response(&format!(
        r#"{{AgentRequest(filter: {{node_did: {{_eq: "{owner}"}}, session_id: {{_eq: "{session}"}}}}) {{{}}}}}"#,
        crate::request_admission::SIGNED_REQUEST_FIELDS
    )).await?;
    let requests: Vec<AgentRequestRow> = crate::graphql::rows(&response, "AgentRequest")?;
    let pending = requests
        .iter()
        .filter(|row| {
            row.caused_by_trigger_kind.as_deref() == Some(GOAL_TRIGGER_KIND)
                && row.lifecycle_state == Some(RequestLifecycleState::Pending)
                && only_doc.is_none_or(|doc| row.doc_id.as_deref() == Some(doc))
        })
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return Ok(true);
    }
    let binding =
        authenticated_goal_request_members(&goal.node_did, &goal.session_id, root, &requests)?;
    anyhow::ensure!(
        binding.entry.doc_id.as_deref() == Some(root),
        "Task assignment root is a continuation"
    );
    let mut allowed = true;
    for row in pending {
        let doc = row
            .doc_id
            .as_deref()
            .context("Goal continuation has no document ID")?;
        let members =
            authenticated_goal_request_members(&goal.node_did, &goal.session_id, doc, &requests)?;
        if assignment_allows(Some(root), members.entry.doc_id.as_deref()) {
            continue;
        }
        allowed = false;
        let reason = "superseded by a newer Task Goal assignment";
        let mutation = crate::lifecycle::queue::supersede_pending_mutation(
            doc,
            &goal.node_did,
            &binding.entry.request_id,
            root,
            reason,
        );
        let response = txn.execute_local_response(&mutation).await?;
        if response
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentRequest"))
            .is_some_and(crate::graphql::response_has_documents)
        {
            crate::trigger_engine::durable::publish_request_outcome(
                txn,
                &goal.node_did,
                &row.request_id,
                "superseded",
                reason,
                now,
            )
            .await?;
        }
    }
    Ok(allowed)
}

pub(crate) async fn assignment_request_id_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    goal: &GoalDocument,
) -> Result<Option<String>> {
    let Some(root) = goal.assignment_root_request_doc_id.as_deref() else {
        return Ok(None);
    };
    let response = txn.execute_local_response(&format!(
        r#"{{AgentRequest(filter: {{_docID: {{_eq: "{}"}}, node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}}}) {{request_id}}}}"#,
        escape_graphql_string(root), escape_graphql_string(&goal.node_did), escape_graphql_string(&goal.session_id)
    )).await?;
    let requests: Vec<AgentRequestRow> = crate::graphql::rows(&response, "AgentRequest")?;
    anyhow::ensure!(
        requests.len() == 1,
        "Goal assignment has no unique request in its owner/session"
    );
    Ok(Some(requests[0].request_id.clone()))
}
