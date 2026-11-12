use std::time::Duration;

use gents::DocumentRuntimeOptions;
use gents_protocol::job::JobResultRequest;
use gents_store::graphql::escape_graphql_string;
use gents_toolcalls::background::status::ProcessControlScope;
use gents_toolcalls::plugin::data::StampContext;
use gents_toolcalls::plugin::job_calls::{result as read_result, JobCaller};
use serde_json::json;

use super::plugin_goal_live::{completed_live_job, submit_with_cwd, LiveJobHarness};
use super::plugin_job_support::PLUGIN;
use crate::support::live_inference::{
    boot_live_agent_with_options, wait_for_request_terminal, wait_for_rows,
};

fn caller(h: &LiveJobHarness, session_id: &str) -> JobCaller {
    let agent_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        h.db.node.clone(),
        h.actors.fixture.binding.agent_did.parse().unwrap(),
        h.actors.fixture.agent.as_ref(),
    )
    .unwrap();
    JobCaller::new(
        h.db.node.clone(),
        ProcessControlScope {
            request_id: h.request_id.clone(),
            session_id: session_id.into(),
            node_did: h.live.node_did.clone(),
            requester_did: Some(h.live.user_did().into()),
        },
        StampContext::ToolOutput {
            caller_did: h.actors.fixture.binding.agent_did.clone(),
            session_id: session_id.into(),
            request_doc_id: h.parent_doc_id.clone(),
            tool_call_doc_id: h.row["_docID"].as_str().unwrap().into(),
        },
    )
    .with_agent_access(agent_access)
}

struct Notification {
    sequence: u32,
    request_doc_id: String,
}

async fn notification(h: &LiveJobHarness) -> Notification {
    let row_doc_id = h.row["_docID"].as_str().unwrap();
    let query = format!(
        r#"{{AgentMessage(filter: {{session_id: {{_eq: "{}"}}}}) {{_docID publication}}}}"#,
        escape_graphql_string(&h.session_id),
    );
    let messages = wait_for_rows(
        &h.live.access,
        "AgentMessage",
        &query,
        Duration::from_secs(120),
        |data| {
            data["AgentMessage"].as_array().is_some_and(|rows| {
                rows.iter().any(|row| {
                    row["publication"]["kind"] == "tool_delivery"
                        && row["publication"]["tool_call_doc_id"] == row_doc_id
                })
            })
        },
    )
    .await;
    let row = messages["AgentMessage"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| {
            row["publication"]["kind"] == "tool_delivery"
                && row["publication"]["tool_call_doc_id"] == row_doc_id
        })
        .unwrap();
    let (header, _) = gents_session::session::load_canonical_message(
        &h.live.access,
        row["_docID"].as_str().unwrap(),
        &h.live.node_did,
        Some(h.live.user_did()),
    )
    .await
    .unwrap();
    Notification {
        sequence: header.sequence,
        request_doc_id: header.request_doc_id.unwrap(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_job_result_refuses_a_different_session_scope() {
    let h = completed_live_job("live-job-result-scope", json!("done")).await;
    let owner = caller(&h, &h.session_id);
    let foreign = caller(&h, "another-session");
    let request = JobResultRequest {
        row_doc_id: h.row["_docID"].as_str().unwrap().into(),
        offset: 0,
        limit: u64::MAX,
    };
    assert!(read_result(&owner, PLUGIN, &request, usize::MAX)
        .await
        .is_ok());
    assert!(read_result(&foreign, PLUGIN, &request, usize::MAX)
        .await
        .is_err());
    h.runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_resumed_prompt_follows_the_persisted_completion_notification() {
    let h = completed_live_job("live-session-notification-order", json!("completed")).await;
    let delivered = notification(&h).await;
    let wake_query = format!(
        r#"{{AgentRequest(docID: "{}") {{request_id}}}}"#,
        escape_graphql_string(&delivered.request_doc_id),
    );
    let wake = h.live.access.execute(&wake_query).await.unwrap();
    assert_eq!(
        wait_for_request_terminal(
            &h.live.access,
            wake["data"]["AgentRequest"][0]["request_id"]
                .as_str()
                .unwrap(),
            Duration::from_secs(120),
        )
        .await,
        "completed"
    );
    h.runtime.shutdown().await;
    let request_id = "live-session-notification-resumed";
    let prompt = "Acknowledge this resumed request briefly without calling tools.";
    let request_doc_id = submit_with_cwd(
        &h.live.access,
        &h.actors.fixture,
        &h.live.node_did,
        &h.session_id,
        request_id,
        prompt.into(),
        &h.cwd,
    )
    .await;
    let (runtime, _) = boot_live_agent_with_options(
        &h.db,
        h.db.node_identity.clone(),
        DocumentRuntimeOptions {
            plugin_home: Some(h.plugin_home.path().to_owned()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        wait_for_request_terminal(&h.live.access, request_id, Duration::from_secs(120)).await,
        "completed"
    );
    let messages = h
        .live
        .access
        .execute(&format!(
            r#"{{AgentMessage(filter: {{request_doc_id: {{_eq: "{}"}}}}) {{_docID}}}}"#,
            escape_graphql_string(&request_doc_id),
        ))
        .await
        .unwrap();
    let mut authored = None;
    for row in messages["data"]["AgentMessage"].as_array().unwrap() {
        let (header, message) = gents_session::session::load_canonical_message(
            &h.live.access,
            row["_docID"].as_str().unwrap(),
            &h.live.node_did,
            Some(h.live.user_did()),
        )
        .await
        .unwrap();
        if message.rag_text().is_some_and(|text| text.contains(prompt)) {
            authored = Some(header.sequence);
        }
    }
    assert!(authored.expect("resumed authored prompt was persisted") > delivered.sequence);
    runtime.shutdown().await;
}
