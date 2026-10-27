use chrono::{DateTime, Utc};
use gents_protocol::{
    goal_control::SessionGoalObservation,
    row::{AgentRequestRow, MailboxItemRow},
    session::SessionRequestObservation,
};

/// The caller supplies the verified runtime Goal observation and the session's
/// current request observation. Missing signed ancestry establishes no wait.
pub fn awaiting_goal_question_ids(
    observation: &SessionGoalObservation,
    latest: &SessionRequestObservation,
    requests: &[AgentRequestRow],
    items: &[MailboxItemRow],
    now: DateTime<Utc>,
) -> Vec<String> {
    let Some(goal) = observation
        .goal
        .as_ref()
        .filter(|goal| goal.snapshot.status == "active")
    else {
        return Vec::new();
    };
    let Ok(members) = super::request_head::authenticated_goal_request_members(
        &observation.node_did,
        &observation.session_id,
        &latest.request_doc_id,
        requests,
    ) else {
        return Vec::new();
    };
    let assigned = members
        .entry
        .input
        .as_ref()
        .and_then(|input| input.goal_assignment.as_ref());
    if members.entry.doc_id.is_none()
        || members.entry.doc_id != goal.assignment_root_request_doc_id
        || !assigned.is_some_and(|reference| reference.goal_id == goal.snapshot.goal_id)
    {
        return Vec::new();
    }
    let Some(parent) = requests.iter().find(|request| {
        request.doc_id.as_deref() == Some(latest.request_doc_id.as_str())
            && request.request_id == latest.request_id
            && request.node_did.as_deref() == Some(observation.node_did.as_str())
            && request.session_id.as_deref() == Some(observation.session_id.as_str())
            && request.requester_did == observation.requester_did
            && request.lifecycle_state == Some(latest.lifecycle_state)
    }) else {
        return Vec::new();
    };
    crate::mailbox::awaiting_question_ids(items, parent, now)
}
