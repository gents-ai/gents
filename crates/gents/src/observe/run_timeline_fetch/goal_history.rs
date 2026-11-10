use super::*;

pub(super) async fn load_timeline_goal_versions_for_session(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
) -> Result<Vec<TimelineGoalVersionRow>> {
    Ok(
        crate::goal::load_goal_versions_for_session(access, node_did, session_id)
            .await?
            .into_iter()
            .map(|version| {
                let goal = version.goal;
                TimelineGoalVersionRow {
                    goal_doc_id: goal.doc_id,
                    goal_id: goal.goal_id,
                    session_id: goal.session_id,
                    node_did: goal.node_did,
                    commit_cid: version.commit_cid,
                    height: version.height,
                    parent_commit_cids: version.parent_commit_cids,
                    status: goal.status,
                    consecutive_blocked_audits: goal
                        .consecutive_blocked_audits
                        .unwrap_or_default()
                        .max(0),
                    wrapup_requested: goal.wrapup_requested.unwrap_or(false),
                    wrapup_completed: goal.wrapup_completed.unwrap_or(false),
                    token_budget: goal.token_budget,
                    tokens_used: goal.tokens_used.unwrap_or_default().max(0),
                    last_blocked_request_id: goal.last_blocked_request_id,
                    last_blocked_reason: goal.last_blocked_reason,
                    last_failure: goal.last_failure,
                    completion_evidence: goal.completion_evidence,
                    created_at: goal.created_at,
                    updated_at: goal.updated_at,
                }
            })
            .collect(),
    )
}
