use anyhow::Result;
use gents_protocol::request_admission::AgentRequestAdmissionRecord;
use gents_store::graphql::escape_graphql_string;
use uuid::Uuid;

use super::prepare::{prepare_request_submission, RequestSubmission};
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn goal_backed_desktop_submission_commits_goal_claim_and_signed_request_together(
) -> Result<()> {
    let home = crate::client_test_support::ServedRuntimeHome::start().await;
    let agent = home.publish_chat_agent().await;
    let user = home.user().clone();
    let session_id = Uuid::new_v4().to_string();
    let result = async {
        let access = home.user_access();
        let node_did = home.node_did();
        let mut options = SubmitRequestOptions {
            retry_key: Some(format!("desktop-goal-request:{session_id}")),
            ..SubmitRequestOptions::default()
        };
        options.input.goal_assignment = Some(gents_protocol::request_input::GoalAssignmentInput {
            goal_id: gents_protocol::goal::deterministic_goal_id(node_did, &session_id),
            root_request_doc_id: None,
        });
        options.input = gents_session::request::lifecycle::prepare_user_message_input(
            &access,
            node_did,
            &session_id,
            options.input,
            gents_protocol::request_input::QueueDelivery::Queue,
        )
        .await?;
        let (submitted, create) = prepare_request_submission(
            &access,
            &gents_observe::client_projection::store::ClientStore::default(),
            RequestSubmission {
                session_id: &session_id,
                node_did,
                requester_did: user.did(),
                agent_did: Some(&agent.agent_did),
                content: "finish the task",
            },
            user.as_ref(),
            AgentRequestAdmissionRecord::client(user.did()),
            options,
        )
        .await?;
        gents_automation::operations::goal::start(
            &access,
            node_did,
            &gents_protocol::operations::goal::GoalStartInput {
                session_id: session_id.clone(),
                objective: "prove the desktop goal path".to_owned(),
                token_budget: Some(2_000),
            },
            &create,
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
