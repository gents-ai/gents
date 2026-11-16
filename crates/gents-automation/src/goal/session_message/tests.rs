use super::SessionGoals;
use crate::goal::operator_resume_test_support::Fixture;
use gents_session::session::message::{SessionGoal, SessionMessageGoals};

#[tokio::test]
async fn the_session_goal_is_the_canonical_goal() {
    let f = Fixture::new(&serde_json::json!({"status":"active","sequence":0,
        "last_continued_from":null,"latest_request":10,"children":[],"blocked_audits":0,
        "wrapup_requested":false,"wrapup_completed":false,"tokens_used":37,"token_budget":1000}))
    .await;
    let canonical = f.current_goal().await;
    let (node_did, session_id) = (canonical.node_did.clone(), canonical.session_id.clone());
    let read = f
        .agent_access()
        .transact_readonly("test.session_message_goal", move |txn| {
            let (node_did, session_id) = (node_did.clone(), session_id.clone());
            Box::pin(async move {
                SessionGoals
                    .session_goal_in_txn(txn, &node_did, &session_id)
                    .await
            })
        })
        .await
        .unwrap();
    assert_eq!(
        read,
        Some(SessionGoal {
            goal_id: canonical.goal_id,
            status: canonical.status,
            objective: canonical.objective,
            tokens_used: canonical.tokens_used,
            token_budget: canonical.token_budget,
            active_time_seconds: canonical.active_time_seconds,
            assignment_root_request_doc_id: canonical.assignment_root_request_doc_id,
        })
    );
    f.node.shutdown().await.expect("shut down test node");
}
