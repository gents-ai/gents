use anyhow::Result;
use gents_store::graphql::escape_graphql_string;
use uuid::Uuid;

use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn goal_backed_desktop_submission_commits_goal_claim_and_signed_request_together(
) -> Result<()> {
    let home = crate::test_fixtures::ServedRuntimeHome::start().await;
    let agent = home.publish_chat_agent().await;
    let user = home.user().clone();
    let session_id = Uuid::new_v4().to_string();
    let result = async {
        let submitted = super::submit_goal_backed_request(
            &crate::client::ClientStore::default(),
            &home.user_access(),
            &session_id,
            home.node_did(),
            user.did(),
            user.as_ref(),
            AgentRequestAdmissionRecord::client(user.did()),
            "finish the task",
            Some(&agent.agent_did),
            SubmitRequestOptions::default(),
            "prove the desktop goal path",
            Some(2_000),
        )
        .await?;
        let escaped_session = escape_graphql_string(&session_id);
        let request_id = escape_graphql_string(&submitted.request_id);
        let data = home
            .user_access()
            .execute(&format!(
                r#"{{
                    AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{
                        retry_key session_id requester_did
                    }}
                    Goal(filter: {{ session_id: {{ _eq: "{escaped_session}" }} }}) {{
                        session_id objective token_budget
                    }}
                    GoalCreationClaim(filter: {{ session_id: {{ _eq: "{escaped_session}" }} }}) {{
                        session_id objective token_budget
                    }}
                }}"#,
            ))
            .await?["data"]
            .clone();
        for collection in ["AgentRequest", "Goal", "GoalCreationClaim"] {
            assert_eq!(
                data[collection].as_array().map(Vec::len),
                Some(1),
                "{collection} should commit exactly once: {data}"
            );
        }
        assert_eq!(data["Goal"][0]["objective"], "prove the desktop goal path");
        assert_eq!(data["GoalCreationClaim"][0]["token_budget"], 2_000);
        assert_eq!(
            data["AgentRequest"][0]["retry_key"],
            format!("desktop-goal-request:{session_id}")
        );
        Ok::<(), anyhow::Error>(())
    }
    .await;
    home.shutdown().await;
    result
}
