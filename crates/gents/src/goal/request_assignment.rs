use super::GoalDocument;
use crate::request_admission::verify_request_receipt_signature;
use gents_protocol::row::AgentRequestRow;

pub(super) fn request_belongs_to_goal(goal: &GoalDocument, entry: &AgentRequestRow) -> bool {
    if verify_request_receipt_signature(entry).is_err()
        || entry.node_did.as_deref() != Some(&goal.node_did)
        || entry.session_id.as_deref() != Some(&goal.session_id)
    {
        return false;
    }
    let reference = entry
        .input
        .as_ref()
        .and_then(|input| input.goal_assignment.as_ref());
    match (goal.assignment_root_request_doc_id.as_deref(), reference) {
        (None, None) => true,
        (None, Some(_)) => false,
        (Some(root), None) => entry.doc_id.as_deref() == Some(root),
        (Some(root), Some(reference)) => {
            reference.goal_id == goal.goal_id
                && reference
                    .root_request_doc_id
                    .as_deref()
                    .or(entry.doc_id.as_deref())
                    == Some(root)
        }
    }
}

pub(crate) enum RequestAssignmentObservation {
    None,
    Unknown,
    Assigned(GoalDocument),
}

pub(super) fn observe_request_assignment(
    goals: &[GoalDocument],
    entry: &AgentRequestRow,
) -> RequestAssignmentObservation {
    if verify_request_receipt_signature(entry).is_err() {
        return RequestAssignmentObservation::Unknown;
    }
    let mut candidates = goals.iter().filter(|goal| {
        goal.assignment_root_request_doc_id.is_some() && request_belongs_to_goal(goal, entry)
    });
    if let Some(goal) = candidates.next() {
        return if candidates.next().is_none() {
            RequestAssignmentObservation::Assigned(goal.clone())
        } else {
            RequestAssignmentObservation::Unknown
        };
    }
    if entry
        .input
        .as_ref()
        .is_some_and(|input| input.goal_assignment.is_some())
        || goals.iter().any(|goal| {
            goal.assignment_root_request_doc_id.is_none()
                && entry.node_did.as_deref() == Some(&goal.node_did)
                && entry.session_id.as_deref() == Some(&goal.session_id)
        })
    {
        RequestAssignmentObservation::Unknown
    } else {
        RequestAssignmentObservation::None
    }
}

#[cfg(test)]
mod contract_tests;

fn same_assignment(left: &super::GoalVersion, right: &super::GoalVersion) -> bool {
    left.goal.doc_id == right.goal.doc_id
        && left.goal.node_did == right.goal.node_did
        && left.goal.session_id == right.goal.session_id
        && left.goal.goal_id == right.goal.goal_id
        && left.goal.assignment_root_request_doc_id == right.goal.assignment_root_request_doc_id
}

pub(crate) fn latest_assignment_goals(
    versions: &[super::GoalVersion],
) -> anyhow::Result<Vec<GoalDocument>> {
    use std::collections::{HashMap, HashSet};
    let mut by_commit = HashMap::new();
    for version in versions {
        anyhow::ensure!(
            version.height >= 0
                && !version.commit_cid.is_empty()
                && !version.goal.doc_id.is_empty()
                && version.goal.parsed_status().is_some(),
            "Goal history has an invalid native revision or status"
        );
        anyhow::ensure!(
            by_commit
                .insert(
                    (version.goal.doc_id.as_str(), version.commit_cid.as_str()),
                    version
                )
                .is_none(),
            "Goal history contains a duplicate native revision"
        );
    }
    for version in versions {
        for parent in &version.parent_commit_cids {
            let prior = by_commit
                .get(&(version.goal.doc_id.as_str(), parent.as_str()))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Goal history is incomplete at revision {}",
                        version.commit_cid
                    )
                })?;
            anyhow::ensure!(
                prior.height < version.height,
                "Goal history has a cyclic or invalid native parent"
            );
        }
    }
    let mut superseded = HashSet::new();
    for later in versions {
        let mut pending = later
            .parent_commit_cids
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let mut visited = HashSet::new();
        while let Some(cid) = pending.pop() {
            if !visited.insert(cid) {
                continue;
            }
            let ancestor = by_commit[&(later.goal.doc_id.as_str(), cid)];
            if same_assignment(ancestor, later) {
                superseded.insert((later.goal.doc_id.as_str(), cid));
            }
            pending.extend(ancestor.parent_commit_cids.iter().map(String::as_str));
        }
    }
    let mut unique_assignments = HashSet::new();
    let mut goals = Vec::new();
    for version in versions {
        if superseded.contains(&(version.goal.doc_id.as_str(), version.commit_cid.as_str())) {
            continue;
        }
        let goal = &version.goal;
        anyhow::ensure!(
            unique_assignments.insert((
                &goal.doc_id,
                &goal.node_did,
                &goal.session_id,
                &goal.goal_id,
                &goal.assignment_root_request_doc_id
            )),
            "Goal assignment has concurrent native revisions; refresh before observing its outcome"
        );
        goals.push(goal.clone());
    }
    Ok(goals)
}

pub(super) fn observe_assignment_history(
    versions: &[super::GoalVersion],
    entry: &AgentRequestRow,
) -> anyhow::Result<RequestAssignmentObservation> {
    let goals = latest_assignment_goals(versions)?;
    Ok(observe_request_assignment(&goals, entry))
}

pub(crate) async fn load_request_assignment(
    node: &std::sync::Arc<defra_node::EmbeddedNode>,
    request: &AgentRequestRow,
) -> anyhow::Result<RequestAssignmentObservation> {
    crate::config_client::ConfigAccess::transact_local_readonly(
        node,
        None,
        "goal.assignment_requests",
        |txn| Box::pin(load_request_assignment_in_txn(txn, request)),
    )
    .await
}

pub(crate) async fn load_request_assignment_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    request: &AgentRequestRow,
) -> anyhow::Result<RequestAssignmentObservation> {
    use anyhow::Context;
    let owner = request
        .node_did
        .as_deref()
        .context("assignment request has no owner")?;
    let session = request
        .session_id
        .as_deref()
        .context("assignment request has no session")?;
    let doc = request
        .doc_id
        .as_deref()
        .context("assignment request has no physical identity")?;
    let rows = super::request_head::load_session_requests(txn, owner, session).await?;
    let by_doc = super::request_head::request_rows_by_doc(owner, session, &rows)?;
    let current = by_doc
        .get(doc)
        .context("assignment request is absent from its owner/session")?;
    anyhow::ensure!(
        current.request_id == request.request_id
            && current.purpose == Some(gents_protocol::request_admission::RequestPurpose::Normal),
        "assignment request differs from its physical owner/session observation"
    );
    let versions = super::load_goal_versions_for_session(txn, owner, session).await?;
    let goals = latest_assignment_goals(&versions)?;
    let openings = goals
        .iter()
        .filter_map(|goal| goal.assignment_root_request_doc_id.as_deref())
        .collect();
    let entry = super::request_head::authenticated_assignment_entry(
        owner, session, current, &by_doc, None, &openings,
    )?;
    Ok(observe_request_assignment(&goals, entry))
}

#[cfg(test)]
mod history_tests;
