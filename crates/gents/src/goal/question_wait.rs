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
    let ids = crate::session::mailbox::awaiting_question_ids(items, parent, now);
    if ids.is_empty() {
        return ids;
    }
    let Ok(members) = super::request_head::authenticated_goal_request_members(
        &observation.node_did,
        &observation.session_id,
        &latest.request_doc_id,
        requests,
    ) else {
        return Vec::new();
    };
    if members.entry.doc_id.is_none()
        || members.entry.doc_id != goal.assignment_root_request_doc_id
        || !super::request_assignment::entry_matches_assignment(
            &goal.snapshot.goal_id,
            goal.assignment_root_request_doc_id.as_deref(),
            members.entry,
        )
    {
        return Vec::new();
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_protocol::goal_control::{GoalRevision, ObservedGoal};
    use gents_protocol::request_admission::RequestPurpose;
    use gents_protocol::request_lifecycle::RequestLifecycleState;
    use serde_json::json;

    #[test]
    fn questionless_observations_do_not_verify_the_retained_request_graph() {
        let now = Utc::now();
        let document: super::super::GoalDocument = serde_json::from_value(json!({
            "_docID":"goal-document", "goal_id":"goal", "node_did":"owner", "session_id":"chat",
            "assignment_root_request_doc_id":"physical-999", "status":"active"
        }))
        .unwrap();
        let observation = SessionGoalObservation {
            version: 1,
            node_did: "owner".into(),
            session_id: "chat".into(),
            session_doc_id: "session-document".into(),
            requester_did: Some("person".into()),
            observed_at: now.to_rfc3339(),
            signature: Vec::new(),
            goal: Some(ObservedGoal {
                revision: GoalRevision {
                    document: document.doc_id.clone(),
                    composite: "commit".into(),
                },
                assignment_root_request_doc_id: document.assignment_root_request_doc_id.clone(),
                snapshot: super::super::snapshot(&document, now),
            }),
        };
        let requests = (0..1000)
            .map(|index| AgentRequestRow {
                doc_id: Some(format!("physical-{index}")),
                request_id: format!("request-{index}"),
                purpose: Some(RequestPurpose::Normal),
                node_did: Some("owner".into()),
                session_id: Some("chat".into()),
                requester_did: Some("person".into()),
                agent_id: Some("worker".into()),
                lifecycle_state: Some(RequestLifecycleState::Completed),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        let latest = SessionRequestObservation {
            request_doc_id: "physical-999".into(),
            request_id: "request-999".into(),
            lifecycle_state: RequestLifecycleState::Completed,
        };
        let unrelated: MailboxItemRow = serde_json::from_value(json!({
            "_docID":"question", "item_key":"question", "requester_did":"person", "node_did":"owner",
            "target_node_did":"owner", "target_agent_id":"worker", "session_id":"other-chat", "request_id":"other-request",
            "status":"open", "kind":"ask", "action":"start_request", "title":"Other work",
            "payload":"unrelated payload", "source_kind":"agent", "source_id":"source", "created_at":now.to_rfc3339()
        })).unwrap();
        for items in [Vec::new(), vec![unrelated]] {
            let (ids, verifications) =
                crate::request::admission::count_request_receipt_verifications(|| {
                    awaiting_goal_question_ids(&observation, &latest, &requests, &items, now)
                });
            assert!(ids.is_empty());
            assert_eq!(verifications, 0);
        }
    }
}
