//! Live end-to-end interrupt test against an OpenAI-compatible streaming backend.
//!
//! Normal test runs skip this file because it depends on a reachable live
//! inference service. To run it locally:
//!
//! ```bash
//! GENTS_LIVE_OPENAI=1 \
//! GENTS_LIVE_OPENAI_ENDPOINT=http://100.74.68.88:8000/v1 \
//! GENTS_LIVE_OPENAI_MODEL=Qwen3.5-122B-A10B-NVFP4 \
//! cargo test -p gents --test interrupt_live -- --ignored --nocapture
//! ```

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::{interrupt_request, AgentIdentity, Gents, ToolCeiling};
use gents_protocol::transcript::present_persisted_message;

use crate::support::fixtures::test_identity;
use crate::support::interrupt::{
    create_runtime_request, wait_for_inference_call_state, wait_for_request_lifecycle_state,
    wait_for_response_content_min_len, wait_for_response_doc_id, wait_for_runtime_ready,
    BootedAgent,
};
use crate::support::snapshots::{
    fetch_message_snapshots_for_session, fetch_response_content, fetch_response_interrupted_at,
    fetch_response_snapshot,
};

const DEFAULT_LIVE_ENDPOINT: &str = "http://100.74.68.88:8000/v1";
const DEFAULT_LIVE_MODEL: &str = "Qwen3.5-122B-A10B-NVFP4";
const LIVE_BACKEND_ID: &str = "backend-live-openai-interrupt";
const LIVE_BEHAVIOR_ID: &str = "live-interrupt";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: set GENTS_LIVE_OPENAI=1 and pass --ignored"]
async fn live_interrupt_mid_stream_on_openai_compatible() -> Result<()> {
    if std::env::var("GENTS_LIVE_OPENAI").as_deref() != Ok("1") {
        eprintln!("GENTS_LIVE_OPENAI is not 1; skipping live interrupt smoke");
        return Ok(());
    }

    let endpoint = std::env::var("GENTS_LIVE_OPENAI_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_LIVE_ENDPOINT.to_string());
    let model =
        std::env::var("GENTS_LIVE_OPENAI_MODEL").unwrap_or_else(|_| DEFAULT_LIVE_MODEL.to_string());
    let api_key = std::env::var("GENTS_LIVE_OPENAI_API_KEY").unwrap_or_default();

    let db = crate::support::test_db("live-openai-interrupt").await;
    let agent = boot_live_agent(&db, &endpoint, &model, &api_key).await?;

    let request_id = "req-live-openai-interrupt";
    let session_id = "session-live-openai-interrupt";
    let request_doc_id = create_runtime_request(
        db.node.as_ref(),
        agent.agent_did.as_str(),
        LIVE_BEHAVIOR_ID,
        request_id,
        session_id,
        "Write a long numbered list from 1 to 200. Use one short sentence per item. Start immediately and keep streaming.",
    )
    .await;

    let response_doc_id = wait_for_response_doc_id(db.node.as_ref(), request_id).await;
    let before_interrupt =
        wait_for_response_content_min_len(db.node.as_ref(), &response_doc_id, 40).await;

    interrupt_request(db.node.as_ref(), request_id)
        .await
        .expect("interrupt_request should latch interrupt_requested_at");

    wait_for_request_lifecycle_state(db.node.as_ref(), &request_doc_id, "interrupted").await;
    let call = wait_for_inference_call_state(db.node.as_ref(), request_id, "cancelled").await;
    assert_eq!(call.failure_reason.as_deref(), Some("Cancelled"));

    let final_content = fetch_response_content(&db.node, &response_doc_id).await;
    assert_eq!(
        final_content, "",
        "live interrupt must clear AgentResponse.content after persisting the partial turn"
    );
    let response = fetch_response_snapshot(&db.node, &response_doc_id).await;
    assert_eq!(response.status, "error");
    assert!(
        response.completed_at_present,
        "live interrupt must terminalize AgentResponse.status"
    );
    let messages = fetch_message_snapshots_for_session(&db.node, session_id).await;
    let before_interrupt_prefix = before_interrupt.trim();
    assert!(
        messages.iter().any(|message| {
            message.role == "assistant"
                && present_persisted_message(&message.role, &message.content)
                    .body_markdown
                    .starts_with(before_interrupt_prefix)
        }),
        "live interrupt must preserve content streamed before the interrupt in AgentMessage; before={before_interrupt:?}"
    );
    assert!(
        fetch_response_interrupted_at(&db.node, &response_doc_id)
            .await
            .is_some(),
        "live interrupt must stamp AgentResponse.interrupted_at"
    );

    tokio::time::sleep(Duration::from_millis(750)).await;
    let settled_content = fetch_response_content(&db.node, &response_doc_id).await;
    assert_eq!(
        settled_content, final_content,
        "response live tail should stay empty after interrupted lifecycle is persisted"
    );

    agent.shutdown().await;
    Ok(())
}

async fn boot_live_agent(
    db: &crate::support::TestDb,
    endpoint: &str,
    model: &str,
    api_key: &str,
) -> Result<BootedAgent> {
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("live-openai-interrupt"));
    upsert_live_backend(db.node.as_ref(), endpoint, model, api_key).await;

    let agent = Gents::builder()
        .node(db.node.clone())
        .identity(identity)
        .default_behavior_id(LIVE_BEHAVIOR_ID)
        .tool_ceiling(ToolCeiling::meta_only())
        .behavior(LIVE_BEHAVIOR_ID)
        .backend_id(LIVE_BACKEND_ID)
        .model_name(model)
        .max_output_tokens(512)
        .stream_batch_ms(0)
        .done()
        .build()
        .await?;

    let agent_did = agent.agent_did().to_string();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_ready(db.node.as_ref(), &agent_did).await;

    Ok(BootedAgent::new(shutdown_tx, handle, agent_did))
}

async fn upsert_live_backend(node: &EmbeddedNode, endpoint: &str, model: &str, api_key: &str) {
    let escaped_backend_id = escape_graphql_string(LIVE_BACKEND_ID);
    let escaped_endpoint = escape_graphql_string(endpoint);
    let escaped_model = escape_graphql_string(model);
    let escaped_api_key = escape_graphql_string(api_key);
    let mutation = format!(
        r#"mutation {{
            upsert_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{escaped_backend_id}" }} }},
                add: {{
                    backend_id: "{escaped_backend_id}",
                    name: "{escaped_backend_id}",
                    provider_kind: "OpenAiCompatible",
                    endpoint: "{escaped_endpoint}",
                    api_key: "{escaped_api_key}",
                    api_key_env_var: "",
                    max_concurrent: 2,
                    max_queue_depth: 100,
                    enabled: true,
                    models: ["{escaped_model}"],
                    probe_status: "healthy"
                }},
                update: {{
                    name: "{escaped_backend_id}",
                    provider_kind: "OpenAiCompatible",
                    endpoint: "{escaped_endpoint}",
                    api_key: "{escaped_api_key}",
                    api_key_env_var: "",
                    max_concurrent: 2,
                    max_queue_depth: 100,
                    enabled: true,
                    models: ["{escaped_model}"],
                    probe_status: "healthy"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "upsert live backend failed: {:?}",
        response.errors
    );
}
