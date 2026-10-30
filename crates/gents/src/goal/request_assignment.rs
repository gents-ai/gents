use super::GoalDocument;
use crate::request::admission::verify_request_receipt_signature;
use gents_protocol::row::AgentRequestRow;

/// Historical display uses the immutable signed request, independently of the
/// current Goal and lifecycle. Missing or refused receipts establish no marker.
pub(super) fn verified_opening_goal_id(entry: &AgentRequestRow) -> Option<&str> {
    let assignment = entry.input.as_ref()?.goal_assignment.as_ref()?;
    (assignment.root_request_doc_id.is_none()
        && entry.purpose == Some(gents_protocol::request_admission::RequestPurpose::Normal)
        && !assignment.goal_id.trim().is_empty()
        && verify_request_receipt_signature(entry).is_ok())
    .then_some(assignment.goal_id.as_str())
}

pub(super) fn request_belongs_to_goal(goal: &GoalDocument, entry: &AgentRequestRow) -> bool {
    verify_request_receipt_signature(entry).is_ok() && verified_entry_belongs_to_goal(goal, entry)
}

fn verified_entry_belongs_to_goal(goal: &GoalDocument, entry: &AgentRequestRow) -> bool {
    if entry.node_did.as_deref() != Some(&goal.node_did)
        || entry.session_id.as_deref() != Some(&goal.session_id)
    {
        return false;
    }
    entry_matches_assignment(
        &goal.goal_id,
        goal.assignment_root_request_doc_id.as_deref(),
        entry,
    )
}

/// The caller authenticates the entry and fixes its node/session scope first.
pub(super) fn entry_matches_assignment(
    goal_id: &str,
    root_request_doc_id: Option<&str>,
    entry: &AgentRequestRow,
) -> bool {
    let reference = entry
        .input
        .as_ref()
        .and_then(|input| input.goal_assignment.as_ref());
    match (root_request_doc_id, reference) {
        (None, None) => true,
        (None, Some(_)) => false,
        (Some(root), None) => entry.doc_id.as_deref() == Some(root),
        (Some(root), Some(reference)) => {
            reference.goal_id == goal_id
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

fn supersedes_assignment(
    left: &super::GoalAssignmentVersion,
    right: &super::GoalAssignmentVersion,
) -> bool {
    left.identity.doc_id == right.identity.doc_id
        && left.identity.node_did == right.identity.node_did
        && left.identity.session_id == right.identity.session_id
        && left.identity.goal_id == right.identity.goal_id
        && (left.identity.assignment_root_request_doc_id
            == right.identity.assignment_root_request_doc_id
            || left.identity.assignment_root_request_doc_id.is_none()
                && right.identity.assignment_root_request_doc_id.is_some())
}

pub(super) fn latest_assignment_versions(
    versions: &[super::GoalAssignmentVersion],
) -> anyhow::Result<Vec<&super::GoalAssignmentVersion>> {
    use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
    let mut by_commit = RapidHashMap::new();
    for version in versions {
        anyhow::ensure!(
            version.height >= 0
                && !version.commit_cid.is_empty()
                && !version.identity.doc_id.is_empty()
                && super::GoalStatus::parse(&version.identity.status).is_some(),
            "Goal history has an invalid native revision or status"
        );
        anyhow::ensure!(
            by_commit
                .insert(
                    (
                        version.identity.doc_id.as_str(),
                        version.commit_cid.as_str()
                    ),
                    version
                )
                .is_none(),
            "Goal history contains a duplicate native revision"
        );
    }
    for version in versions {
        for parent in &version.parent_commit_cids {
            let prior = by_commit
                .get(&(version.identity.doc_id.as_str(), parent.as_str()))
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
            let ancestor = by_commit[&(later.identity.doc_id.as_str(), cid)];
            if supersedes_assignment(ancestor, later) {
                superseded.insert((later.identity.doc_id.as_str(), cid));
            }
            pending.extend(ancestor.parent_commit_cids.iter().map(String::as_str));
        }
    }
    let mut unique_assignments = RapidHashSet::new();
    let mut goals = Vec::new();
    for version in versions {
        if superseded.contains(&(
            version.identity.doc_id.as_str(),
            version.commit_cid.as_str(),
        )) {
            continue;
        }
        let goal = &version.identity;
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
        goals.push(version);
    }
    Ok(goals)
}

pub(crate) fn latest_assignment_goals(
    versions: &[super::GoalAssignmentVersion],
) -> anyhow::Result<Vec<GoalDocument>> {
    use anyhow::Context;
    latest_assignment_versions(versions)?
        .into_iter()
        .map(|version| {
            version
                .snapshot
                .clone()
                .context("Goal assignment frontier has no native snapshot")
        })
        .collect()
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
    let versions = super::load_assignment_versions_for_session(txn, owner, session).await?;
    request_assignments(owner, session, rows, latest_assignment_goals(&versions)?)
}

fn request_assignments(
    owner: &str,
    session: &str,
    rows: Vec<AgentRequestRow>,
    goals: Vec<GoalDocument>,
) -> anyhow::Result<RequestAssignments> {
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
        goals,
        scopes: Default::default(),
    })
}

pub(crate) struct RequestAssignmentScopes<'a, R>
where
    R: crate::config_client::ConfigRead,
{
    access: &'a R,
    requests: RequestAssignments,
    history_loaded: bool,
    scopes: rapidhash::RapidHashMap<String, RequestAssignmentScope>,
}

impl<R> RequestAssignmentScopes<'_, R>
where
    R: crate::config_client::ConfigRead,
{
    pub(crate) fn request(&self, doc: &str) -> anyhow::Result<&AgentRequestRow> {
        self.requests.request(doc)
    }

    pub(crate) async fn scope(&mut self, doc: &str) -> anyhow::Result<RequestAssignmentScope> {
        if let Some(scope) = self.scopes.get(doc) {
            return Ok(scope.clone());
        }
        self.requests.request(doc)?;
        let scope = match signed_scope(&self.requests, doc) {
            Ok(Some(scope)) => scope,
            _ => {
                if !self.history_loaded {
                    let versions = super::load_assignment_versions_for_session(
                        self.access,
                        &self.requests.owner,
                        &self.requests.session,
                    )
                    .await?;
                    self.requests.goals = latest_assignment_goals(&versions)?;
                    self.history_loaded = true;
                }
                self.requests.scope(doc)?
            }
        };
        self.scopes.insert(doc.to_owned(), scope.clone());
        Ok(scope)
    }

    pub(crate) async fn scope_for_request(
        &mut self,
        doc: &str,
        request_id: &str,
    ) -> anyhow::Result<RequestAssignmentScope> {
        anyhow::ensure!(
            self.requests.request(doc)?.request_id == request_id,
            "assignment request logical identity differs from its physical observation"
        );
        self.scope(doc).await
    }
}

/// A signed physical assignment determines scope independently of Goal status.
/// Authenticate the existing continuation chain before using that reference;
/// legacy or incomplete ancestry still requires the full historical owner.
fn signed_scope(
    requests: &RequestAssignments,
    doc: &str,
) -> anyhow::Result<Option<RequestAssignmentScope>> {
    let by_doc = super::request_head::request_rows_by_doc(
        &requests.owner,
        &requests.session,
        &requests.rows,
    )?;
    let current = requests.request(doc)?;
    let entry = super::request_head::authenticated_entry(
        &requests.owner,
        &requests.session,
        current,
        &by_doc,
        None,
    )?;
    if entry
        .input
        .as_ref()
        .is_some_and(|input| input.goal_assignment.is_some())
    {
        request_assignment_scope(entry, &RequestAssignmentObservation::Unknown).map(Some)
    } else {
        Ok(None)
    }
}

pub(crate) async fn load_request_assignment_scopes<'a, R>(
    access: &'a R,
    owner: &str,
    session: &str,
) -> anyhow::Result<RequestAssignmentScopes<'a, R>>
where
    R: crate::config_client::ConfigRead,
{
    let rows = super::request_head::load_session_requests(access, owner, session).await?;
    Ok(RequestAssignmentScopes {
        access,
        requests: request_assignments(owner, session, rows, Vec::new())?,
        history_loaded: false,
        scopes: Default::default(),
    })
}

#[cfg(test)]
mod scope_tests;

#[cfg(test)]
mod history_tests;
