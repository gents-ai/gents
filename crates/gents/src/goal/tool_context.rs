use super::*;
use gents_protocol::row::AgentRequestRow;

pub(crate) struct GoalToolContext {
    pub request: AgentRequestRow,
    pub assignment: RequestAssignmentObservation,
    current: Option<GoalDocument>,
}

impl GoalToolContext {
    pub(crate) fn can_write(&self) -> bool {
        matches!(&self.assignment, RequestAssignmentObservation::Assigned(goal)
            if self.current.as_ref().is_some_and(|current| same_goal_assignment(goal, current)))
    }

    pub(super) fn can_create(&self) -> bool {
        match self.assignment {
            RequestAssignmentObservation::None => self.current.is_none(),
            RequestAssignmentObservation::Unknown => false,
            RequestAssignmentObservation::Assigned(_) => self.can_write(),
        }
    }

    /// The caller's signed request, its ancestry and its assignment input are
    /// already committed, so only the canonical Goal can move during a call.
    /// A later observation of it replaces this one.
    pub(crate) fn observe_current(&mut self, current: Option<GoalDocument>) {
        self.current = current;
    }

    /// The Goal a reply describes: the canonical row while the caller still
    /// holds the assignment, otherwise the caller's retained assignment.
    pub(crate) fn observed_goal(&self) -> Option<&GoalDocument> {
        let RequestAssignmentObservation::Assigned(assigned) = &self.assignment else {
            return None;
        };
        Some(if self.can_write() {
            self.current.as_ref().unwrap_or(assigned)
        } else {
            assigned
        })
    }
}

pub(super) fn same_goal_assignment(left: &GoalDocument, right: &GoalDocument) -> bool {
    left.doc_id == right.doc_id
        && left.goal_id == right.goal_id
        && left.node_did == right.node_did
        && left.session_id == right.session_id
        && left.assignment_root_request_doc_id == right.assignment_root_request_doc_id
}

pub(crate) async fn load_goal_tool_context(
    access: &crate::ConfigAccess,
    owner: &str,
    session: &str,
    request_id: &str,
    request_doc_id: Option<&str>,
) -> Result<GoalToolContext> {
    let doc = request_doc_id.context("Goal tool caller has no physical request identity")?;
    access
        .transact_readonly("goal.tool_context", |txn| {
            Box::pin(load_goal_tool_context_in_txn(
                txn, owner, session, request_id, doc,
            ))
        })
        .await
}

/// Charged session usage is summed from the same rows and the same snapshot as
/// the attribution it will be written against, and only for a caller that may
/// write it.
pub(crate) async fn load_goal_tool_context_with_usage(
    access: &crate::ConfigAccess,
    owner: &str,
    session: &str,
    request_id: &str,
    request_doc_id: Option<&str>,
) -> Result<(GoalToolContext, Option<SessionTokenUsage>)> {
    let doc = request_doc_id.context("Goal tool caller has no physical request identity")?;
    access
        .transact_readonly("goal.tool_context", |txn| {
            Box::pin(async move {
                let (context, assignments) =
                    observe_goal_tool_context_in_txn(txn, owner, session, request_id, doc).await?;
                let usage = if context.can_write() {
                    Some(
                        crate::tools::native::session_goal_usage_from_rows(
                            txn,
                            owner,
                            assignments.session_rows(),
                        )
                        .await
                        .context("summing goal inference usage")?,
                    )
                } else {
                    None
                };
                Ok((context, usage))
            })
        })
        .await
}

pub(super) async fn load_goal_tool_context_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    session: &str,
    request_id: &str,
    request_doc_id: &str,
) -> Result<GoalToolContext> {
    Ok(
        observe_goal_tool_context_in_txn(txn, owner, session, request_id, request_doc_id)
            .await?
            .0,
    )
}

async fn observe_goal_tool_context_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    session: &str,
    request_id: &str,
    request_doc_id: &str,
) -> Result<(
    GoalToolContext,
    super::request_assignment::RequestAssignments,
)> {
    let assignments =
        super::request_assignment::load_request_assignments_in_txn(txn, owner, session).await?;
    let (request, assignment) = assignments.observe_physical_request(request_doc_id, request_id)?;
    let context = GoalToolContext {
        request: request.clone(),
        assignment,
        current: load_canonical_goal_in_txn(txn, owner, session).await?,
    };
    Ok((context, assignments))
}
