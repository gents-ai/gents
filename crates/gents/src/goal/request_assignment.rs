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

pub(crate) enum RequestAssignmentObservation<'a> {
    None,
    Unknown,
    Assigned(&'a GoalDocument),
}

pub(super) fn observe_request_assignment<'a>(
    goals: &'a [GoalDocument],
    entry: &AgentRequestRow,
) -> RequestAssignmentObservation<'a> {
    if verify_request_receipt_signature(entry).is_err() {
        return RequestAssignmentObservation::Unknown;
    }
    let mut candidates = goals.iter().filter(|goal| {
        goal.assignment_root_request_doc_id.is_some() && request_belongs_to_goal(goal, entry)
    });
    if let Some(goal) = candidates.next() {
        return if candidates.next().is_none() {
            RequestAssignmentObservation::Assigned(goal)
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
