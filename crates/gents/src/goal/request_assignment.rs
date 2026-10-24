use super::GoalDocument;
use crate::request_admission::verify_request_receipt_signature;
use gents_protocol::row::AgentRequestRow;

pub(super) fn request_belongs_to_goal(goal: &GoalDocument, entry: &AgentRequestRow) -> bool {
    verify_request_receipt_signature(entry).is_ok() && verified_entry_belongs_to_goal(goal, entry)
}

fn verified_entry_belongs_to_goal(goal: &GoalDocument, entry: &AgentRequestRow) -> bool {
    if entry.node_did.as_deref() != Some(&goal.node_did)
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
    Assigned(Box<GoalDocument>),
}

pub(super) fn observe_request_assignment(
    goals: &[GoalDocument],
    entry: &AgentRequestRow,
) -> RequestAssignmentObservation {
    if verify_request_receipt_signature(entry).is_err() {
        return RequestAssignmentObservation::Unknown;
    }
    let mut candidates = goals.iter().filter(|goal| {
        goal.assignment_root_request_doc_id.is_some() && verified_entry_belongs_to_goal(goal, entry)
    });
    if let Some(goal) = candidates.next() {
        return if candidates.next().is_none() {
            RequestAssignmentObservation::Assigned(Box::new(goal.clone()))
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RequestAssignmentScope {
    Unassigned,
    Unresolved { entry_doc_id: String },
    Referenced(gents_protocol::request_input::GoalAssignmentInput),
}

fn request_assignment_scope(
    entry: &AgentRequestRow,
    observation: &RequestAssignmentObservation,
) -> anyhow::Result<RequestAssignmentScope> {
    use anyhow::Context;
    use gents_protocol::request_input::GoalAssignmentInput;
    verify_request_receipt_signature(entry)?;
    let entry_doc_id = entry
        .doc_id
        .as_deref()
        .context("assignment entry has no document ID")?;
    if let Some(reference) = entry
        .input
        .as_ref()
        .and_then(|input| input.goal_assignment.as_ref())
    {
        return Ok(RequestAssignmentScope::Referenced(
            reference.continuation(entry_doc_id),
        ));
    }
    Ok(match observation {
        RequestAssignmentObservation::None => RequestAssignmentScope::Unassigned,
        RequestAssignmentObservation::Unknown => RequestAssignmentScope::Unresolved {
            entry_doc_id: entry_doc_id.to_owned(),
        },
        RequestAssignmentObservation::Assigned(goal) => {
            RequestAssignmentScope::Referenced(GoalAssignmentInput {
                goal_id: goal.goal_id.clone(),
                root_request_doc_id: Some(
                    goal.assignment_root_request_doc_id
                        .clone()
                        .context("observed Goal assignment has no physical opening")?,
                ),
            })
        }
    })
}

#[cfg(test)]
mod contract_tests;

fn supersedes_assignment(left: &super::GoalVersion, right: &super::GoalVersion) -> bool {
    left.goal.doc_id == right.goal.doc_id
        && left.goal.node_did == right.goal.node_did
        && left.goal.session_id == right.goal.session_id
        && left.goal.goal_id == right.goal.goal_id
        && (left.goal.assignment_root_request_doc_id == right.goal.assignment_root_request_doc_id
            || left.goal.assignment_root_request_doc_id.is_none()
                && right.goal.assignment_root_request_doc_id.is_some())
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum GoalHistoryError {
    #[error("Goal history has an invalid native revision or status")]
    InvalidRevision,
    #[error("Goal history contains a duplicate native revision")]
    DuplicateRevision,
    #[error("Goal history is incomplete at revision {commit_cid}")]
    IncompleteRevision { commit_cid: String },
    #[error("Goal history has a cyclic or invalid native parent")]
    InvalidParent,
    #[error(
        "Goal assignment has concurrent native revisions; refresh before observing its outcome"
    )]
    ConcurrentRevisions,
}

pub(crate) fn latest_assignment_goals(
    versions: &[super::GoalVersion],
) -> Result<Vec<GoalDocument>, GoalHistoryError> {
    use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
    let mut by_commit = RapidHashMap::new();
    for version in versions {
        if !(version.height >= 0
            && !version.commit_cid.is_empty()
            && !version.goal.doc_id.is_empty()
            && version.goal.parsed_status().is_some())
        {
            return Err(GoalHistoryError::InvalidRevision);
        }
        if by_commit
            .insert(
                (version.goal.doc_id.as_str(), version.commit_cid.as_str()),
                version,
            )
            .is_some()
        {
            return Err(GoalHistoryError::DuplicateRevision);
        }
    }
    for version in versions {
        for parent in &version.parent_commit_cids {
            let prior = by_commit
                .get(&(version.goal.doc_id.as_str(), parent.as_str()))
                .ok_or_else(|| GoalHistoryError::IncompleteRevision {
                    commit_cid: version.commit_cid.clone(),
                })?;
            if prior.height >= version.height {
                return Err(GoalHistoryError::InvalidParent);
            }
        }
    }
    let mut superseded = RapidHashSet::new();
    for later in versions {
        let mut pending = later
            .parent_commit_cids
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let mut visited = RapidHashSet::new();
        while let Some(cid) = pending.pop() {
            if !visited.insert(cid) {
                continue;
            }
            let ancestor = by_commit[&(later.goal.doc_id.as_str(), cid)];
            if supersedes_assignment(ancestor, later) {
                superseded.insert((later.goal.doc_id.as_str(), cid));
            }
            pending.extend(ancestor.parent_commit_cids.iter().map(String::as_str));
        }
    }
    let mut unique_assignments = RapidHashSet::new();
    let mut goals = Vec::new();
    for version in versions {
        if superseded.contains(&(version.goal.doc_id.as_str(), version.commit_cid.as_str())) {
            continue;
        }
        let goal = &version.goal;
        if !unique_assignments.insert((
            &goal.doc_id,
            &goal.node_did,
            &goal.session_id,
            &goal.goal_id,
            &goal.assignment_root_request_doc_id,
        )) {
            return Err(GoalHistoryError::ConcurrentRevisions);
        }
        goals.push(goal.clone());
    }
    Ok(goals)
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
    let snapshot = assignment_snapshot_for_request(txn, request).await?;
    Ok(snapshot.observation(request)?.1)
}

async fn assignment_snapshot_for_request(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    request: &AgentRequestRow,
) -> anyhow::Result<RequestAssignments> {
    use anyhow::Context;
    load_request_assignments_in_txn(
        txn,
        request
            .node_did
            .as_deref()
            .context("assignment request has no owner")?,
        request
            .session_id
            .as_deref()
            .context("assignment request has no session")?,
    )
    .await
}

pub(crate) struct RequestAssignments {
    owner: String,
    session: String,
    rows: Vec<AgentRequestRow>,
    index: rapidhash::RapidHashMap<String, usize>,
    goals: Vec<GoalDocument>,
    scopes: std::sync::Mutex<rapidhash::RapidHashMap<String, RequestAssignmentScope>>,
}

impl RequestAssignments {
    pub(crate) fn request(&self, doc: &str) -> anyhow::Result<&AgentRequestRow> {
        use anyhow::Context;
        let row = &self.rows[*self
            .index
            .get(doc)
            .context("request is absent from its owner/session observation")?];
        anyhow::ensure!(
            row.purpose == Some(gents_protocol::request_admission::RequestPurpose::Normal),
            "assignment request is auxiliary"
        );
        verify_request_receipt_signature(row)?;
        Ok(row)
    }

    fn entry(&self, doc: &str) -> anyhow::Result<&AgentRequestRow> {
        use anyhow::Context;
        let by_doc =
            super::request_head::request_rows_by_doc(&self.owner, &self.session, &self.rows)?;
        let current = by_doc
            .get(doc)
            .context("assignment request is absent from its owner/session")?;
        let openings = super::request_head::assignment_openings(&self.goals);
        super::request_head::authenticated_assignment_entry(
            &self.owner,
            &self.session,
            current,
            &by_doc,
            None,
            &openings,
        )
    }

    fn observation(
        &self,
        request: &AgentRequestRow,
    ) -> anyhow::Result<(&AgentRequestRow, RequestAssignmentObservation)> {
        use anyhow::Context;
        let doc = request
            .doc_id
            .as_deref()
            .context("assignment request has no physical identity")?;
        let current = self
            .rows
            .iter()
            .find(|row| row.doc_id.as_deref() == Some(doc))
            .context("assignment request is absent from its owner/session")?;
        anyhow::ensure!(
            current.request_id == request.request_id
                && current.purpose
                    == Some(gents_protocol::request_admission::RequestPurpose::Normal)
                && request.node_did.as_deref() == Some(self.owner.as_str())
                && request.session_id.as_deref() == Some(self.session.as_str()),
            "assignment request differs from its physical owner/session observation"
        );
        let entry = self.entry(doc)?;
        Ok((entry, observe_request_assignment(&self.goals, entry)))
    }

    pub(crate) fn scope_for_request(
        &self,
        doc: &str,
        request_id: &str,
    ) -> anyhow::Result<RequestAssignmentScope> {
        anyhow::ensure!(
            self.request(doc)?.request_id == request_id,
            "assignment request logical identity differs from its physical observation"
        );
        self.scope(doc)
    }

    pub(crate) fn scope(&self, request_doc_id: &str) -> anyhow::Result<RequestAssignmentScope> {
        use std::sync::PoisonError;
        if let Some(scope) = self
            .scopes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(request_doc_id)
        {
            return Ok(scope.clone());
        }
        let entry = self.entry(request_doc_id)?;
        let scope =
            request_assignment_scope(entry, &observe_request_assignment(&self.goals, entry))?;
        self.scopes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(request_doc_id.to_owned(), scope.clone());
        Ok(scope)
    }
}

pub(crate) async fn load_request_assignments_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    session: &str,
) -> anyhow::Result<RequestAssignments> {
    let rows = super::request_head::load_session_requests(txn, owner, session).await?;
    let versions = super::load_goal_versions_for_session(txn, owner, session).await?;
    let by_doc = super::request_head::request_rows_by_doc(owner, session, &rows)?;
    let index = rows
        .iter()
        .enumerate()
        .filter_map(|(position, row)| {
            let doc = row.doc_id.as_deref()?;
            by_doc
                .get(doc)
                .is_some_and(|indexed| std::ptr::eq(*indexed, row))
                .then(|| (doc.to_owned(), position))
        })
        .collect();
    Ok(RequestAssignments {
        owner: owner.to_owned(),
        session: session.to_owned(),
        rows,
        index,
        goals: latest_assignment_goals(&versions)?,
        scopes: Default::default(),
    })
}

#[cfg(test)]
mod history_tests;
