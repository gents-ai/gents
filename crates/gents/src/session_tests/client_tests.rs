use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
use gents_session::request::lifecycle::client::*;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::{KeyIdentity, NodeIdentity};
use std::sync::Arc;

async fn create_client_agent(
    access: &ConfigAccess,
    runtime_did: &str,
    agent: &gents_protocol::agent_identity::AgentIdentityBinding,
) -> String {
    access
            .transact_write("test.client.created_agent", |txn| {
                Box::pin(async move {
                    let created = txn
                        .execute_client_signed_create(
                            "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
                            &serde_json::json!({"input":{"node_did":runtime_did,"agent_did":agent.agent_did,"creation_nonce":agent.creation_nonce,"inference_profile_id":"fixture","enabled":true}}),
                            gents_protocol::schemas::AGENT,
                        )
                        .await?;
                    Ok::<_, anyhow::Error>(gents_store::graphql::created_doc_id(&created, "Agent")?)
                })
            })
            .await
            .unwrap()
}

mod claim_tests;
