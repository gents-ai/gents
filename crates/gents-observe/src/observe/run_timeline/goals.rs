use std::collections::BTreeMap;

use super::events::{RunTimelineEvent, TimelineGoalParentState, TimelineGoalTransitionEvent};
use super::rows::TimelineGoalVersionRow;
use super::values::first_owned;

pub(super) fn goal_transition_events(
    versions: &[TimelineGoalVersionRow],
    root_session_id: Option<&str>,
) -> Vec<RunTimelineEvent> {
    let versions_by_cid = versions
        .iter()
        .map(|version| (version.commit_cid.as_str(), version))
        .collect::<BTreeMap<_, _>>();
    let mut ordered = versions.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        (&left.goal_doc_id, left.height, &left.commit_cid).cmp(&(
            &right.goal_doc_id,
            right.height,
            &right.commit_cid,
        ))
    });

    ordered
        .into_iter()
        .filter(|version| Some(version.session_id.as_str()) == root_session_id)
        .filter_map(|version| {
            let state = version.semantic_state();
            let mut parents = version
                .parent_commit_cids
                .iter()
                .filter_map(|cid| {
                    let parent = versions_by_cid.get(cid.as_str())?;
                    (parent.goal_doc_id == version.goal_doc_id).then(|| TimelineGoalParentState {
                        commit_cid: cid.clone(),
                        state: parent.semantic_state(),
                    })
                })
                .collect::<Vec<_>>();
            parents.sort_by(|left, right| left.commit_cid.cmp(&right.commit_cid));

            if !parents.is_empty() && parents.iter().all(|parent| parent.state == state) {
                return None;
            }

            Some(RunTimelineEvent::GoalTransition(
                TimelineGoalTransitionEvent {
                    goal_id: version.goal_id.clone(),
                    session_id: version.session_id.clone(),
                    node_did: version.node_did.clone(),
                    commit_cid: version.commit_cid.clone(),
                    height: version.height,
                    parents,
                    state,
                    token_budget: version.token_budget,
                    tokens_used: version.tokens_used,
                    last_blocked_request_id: version.last_blocked_request_id.clone(),
                    last_blocked_reason: version.last_blocked_reason.clone(),
                    last_failure: version.last_failure.clone(),
                    completion_evidence: version.completion_evidence.clone(),
                    timestamp: first_owned([
                        version.updated_at.as_deref(),
                        version.created_at.as_deref(),
                    ]),
                },
            ))
        })
        .collect()
}
