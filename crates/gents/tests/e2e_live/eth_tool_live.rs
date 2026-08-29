//! Live EthTool qualification: GLM-5.3-Flash uses `{tool_id}_query` against
//! Base Sepolia (live chain data, live inference).
//!
//! ```bash
//! GENTS_ETH_LIVE=1 cargo test -p gents --features live-e2e --test e2e_live \
//!   eth_tool_live_model_queries_base_sepolia \
//!   -- --ignored --test-threads=1 --nocapture
//! ```
//!
//! Defaults: workstation-1 `http://100.73.235.38:8001/v1` model
//! `GLM-5.3-Flash-NVFP4`, RPC `https://sepolia.base.org` (chain id 84532).
//! Override with `GENTS_ETH_LIVE_ENDPOINT`, `GENTS_ETH_LIVE_MODEL`,
//! `GENTS_ETH_LIVE_RPC`, `GENTS_ETH_LIVE_CHAIN_ID`.

use std::sync::Arc;
use std::time::Duration;

use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::{
    load_agent_behavior, upsert_agent_behavior, upsert_tool_selection, AgentIdentity,
    DocumentRuntimeOptions, Gents, ToolCeiling, ToolSelectionDocument,
};
use serde::Deserialize;

use crate::steward_loop_live::{wait_for_assistant_answer, wait_for_request_terminal};
use crate::support::fixtures::test_identity;
use crate::support::interrupt::{create_runtime_request, wait_for_runtime_ready, BootedAgent};
use crate::support::test_db;

const TOOL_ID: &str = "base-sepolia";
const BACKEND_ID: &str = "backend-eth-live";
const DEFAULT_ENDPOINT: &str = "http://100.73.235.38:8001/v1";
const DEFAULT_MODEL: &str = "GLM-5.3-Flash-NVFP4";
const DEFAULT_RPC: &str = "https://sepolia.base.org";
const DEFAULT_CHAIN_ID: i64 = 84532;

fn live_enabled() -> bool {
    std::env::var("GENTS_ETH_LIVE").as_deref() == Ok("1")
}

fn live_endpoint() -> String {
    std::env::var("GENTS_ETH_LIVE_ENDPOINT").unwrap_or_else(|_| DEFAULT_ENDPOINT.to_string())
}

fn live_model() -> String {
    std::env::var("GENTS_ETH_LIVE_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string())
}

fn live_rpc() -> String {
    std::env::var("GENTS_ETH_LIVE_RPC").unwrap_or_else(|_| DEFAULT_RPC.to_string())
}

fn live_chain_id() -> i64 {
    std::env::var("GENTS_ETH_LIVE_CHAIN_ID")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_CHAIN_ID)
}

async fn assert_endpoint_reachable(endpoint: &str) {
    let url = format!("{}/models", endpoint.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("reqwest client");
    match tokio::time::timeout(Duration::from_secs(20), client.get(&url).send()).await {
        Ok(Ok(response)) if response.status().is_success() => {}
        Ok(Ok(response)) => panic!("endpoint {url} returned {}", response.status()),
        Ok(Err(error)) => panic!("endpoint {url} unreachable: {error}"),
        Err(_) => panic!("endpoint {url} timed out"),
    }
}

async fn assert_rpc_reachable(rpc_url: &str, chain_id: i64) {
    let client = reqwest::Client::builder()
        .user_agent(gents::ETH_USER_AGENT)
        .timeout(Duration::from_secs(20))
        .build()
        .expect("rpc client");
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_chainId",
        "params": []
    });
    let response = client
        .post(rpc_url)
        .json(&body)
        .send()
        .await
        .unwrap_or_else(|error| panic!("eth RPC {rpc_url} unreachable: {error}"));
    let status = response.status();
    let text = response.text().await.expect("rpc body");
    assert!(
        status.is_success(),
        "eth RPC {rpc_url} failed {status}: {text}"
    );
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("rpc json");
    let hex = parsed["result"]
        .as_str()
        .unwrap_or_else(|| panic!("eth_chainId missing result: {text}"));
    let actual = u64::from_str_radix(hex.trim_start_matches("0x"), 16)
        .unwrap_or_else(|_| panic!("bad chain id {hex}"));
    assert_eq!(
        actual, chain_id as u64,
        "live RPC chain id {actual} != configured {chain_id}"
    );
}

async fn bind_glm_backend(node: &EmbeddedNode, identity: &dyn AgentIdentity) -> (String, String) {
    let agent_did = identity.did().to_string();
    let bootstrap = gents::ensure_agent_principal(node, &agent_did)
        .await
        .expect("ensure principal");
    let behavior_id = bootstrap.default_behavior.behavior_id.clone();

    let backend_id = escape_graphql_string(BACKEND_ID);
    let endpoint = escape_graphql_string(&live_endpoint());
    let model = escape_graphql_string(&live_model());
    let mutation = format!(
        r#"mutation {{
            upsert_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{backend_id}" }} }},
                add: {{
                    backend_id: "{backend_id}",
                    name: "{backend_id}",
                    provider_kind: "OpenAiCompatible",
                    endpoint: "{endpoint}",
                    api_key: "",
                    api_key_env_var: "",
                    max_concurrent: 4,
                    max_queue_depth: 100,
                    enabled: true,
                    models: ["{model}"],
                    probe_status: "healthy"
                }},
                update: {{
                    name: "{backend_id}",
                    provider_kind: "OpenAiCompatible",
                    endpoint: "{endpoint}",
                    api_key: "",
                    api_key_env_var: "",
                    max_concurrent: 4,
                    max_queue_depth: 100,
                    enabled: true,
                    models: ["{model}"],
                    probe_status: "healthy"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "upsert glm backend failed: {:?}",
        response.errors
    );

    let mut behavior = load_agent_behavior(node, &behavior_id)
        .await
        .expect("load default behavior")
        .expect("default behavior document exists after bootstrap");
    behavior.backend_id = Some(BACKEND_ID.to_string());
    behavior.model_name = Some(live_model());
    behavior.inference_profile_id = Some(gents::default_inference_profile_id_for_behavior(
        &behavior_id,
    ));
    behavior.enabled = true;
    behavior.system_prompt = Some(
        "You are an Ethereum operator. You have the native tool base-sepolia_query. \
         When asked for chain data, call that tool. Do not guess block numbers."
            .to_string(),
    );
    upsert_agent_behavior(node, &behavior)
        .await
        .expect("point default behavior at glm");
    (agent_did, behavior_id)
}

async fn create_eth_tool(node: &EmbeddedNode, agent_did: &str) {
    let tool_id = escape_graphql_string(TOOL_ID);
    let agent_did = escape_graphql_string(agent_did);
    let rpc_url = escape_graphql_string(&live_rpc());
    let chain_id = live_chain_id();
    let mutation = format!(
        r#"mutation {{
            create_EthTool(input: {{
                tool_id: "{tool_id}",
                agent_did: "{agent_did}",
                display_name: "Base Sepolia",
                enabled: true,
                chain_id: {chain_id},
                rpc_url: "{rpc_url}",
                query_methods: ["eth_blockNumber", "eth_chainId", "eth_getBalance", "eth_gasPrice"],
                calls: null,
                key_binding_id: null,
                created_at: "2026-08-29T00:00:00Z"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create EthTool failed: {:?}",
        response.errors
    );
}

#[derive(Clone, Deserialize, Debug)]
struct ToolCallRow {
    tool_name: Option<String>,
    args: Option<String>,
    result: Option<String>,
}

async fn fetch_tool_calls(node: &EmbeddedNode, request_id: &str) -> Vec<ToolCallRow> {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentToolCall(filter: {{ request_id: {{ _eq: "{escaped}" }} }}) {{
                tool_name
                args
                result
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "tool call query failed: {:?}",
        resp.errors
    );
    resp.data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|rows| rows.as_array())
        .map(|rows| {
            rows.iter()
                .cloned()
                .map(|value| serde_json::from_value(value).expect("decode AgentToolCall row"))
                .collect()
        })
        .unwrap_or_default()
}

async fn wait_for_eth_query_tool_call(
    node: &EmbeddedNode,
    request_id: &str,
    timeout: Duration,
) -> ToolCallRow {
    let deadline = tokio::time::Instant::now() + timeout;
    let expected = format!("{TOOL_ID}_query");
    loop {
        let rows = fetch_tool_calls(node, request_id).await;
        if let Some(row) = rows.into_iter().find(|row| {
            row.tool_name.as_deref() == Some(expected.as_str())
                && row
                    .result
                    .as_deref()
                    .is_some_and(|result| !result.trim().is_empty())
        }) {
            return row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {expected} tool call on {request_id}"
        );
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: set GENTS_ETH_LIVE=1 and pass --ignored"]
async fn eth_tool_live_model_queries_base_sepolia() {
    assert!(
        live_enabled(),
        "set GENTS_ETH_LIVE=1 and pass --ignored to run the live EthTool qualification"
    );

    let endpoint = live_endpoint();
    let rpc = live_rpc();
    let chain_id = live_chain_id();
    assert_endpoint_reachable(&endpoint).await;
    assert_rpc_reachable(&rpc, chain_id).await;

    let db = test_db("eth-tool-live").await;
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("eth-tool-live"));
    let (agent_did, behavior_id) = bind_glm_backend(db.node.as_ref(), identity.as_ref()).await;
    create_eth_tool(db.node.as_ref(), &agent_did).await;

    upsert_tool_selection(
        db.node.as_ref(),
        &ToolSelectionDocument {
            selection_id: "eth-live-tools".to_string(),
            agent_did: agent_did.clone(),
            enable_file_tools: Some(false),
            enable_bash: Some(false),
            eth_tool_ids: Some(vec![TOOL_ID.to_string()]),
            ..Default::default()
        },
    )
    .await
    .expect("upsert eth tool selection");

    let mut behavior = load_agent_behavior(db.node.as_ref(), &behavior_id)
        .await
        .expect("load behavior")
        .expect("behavior exists");
    behavior.tool_selection_id = Some("eth-live-tools".to_string());
    upsert_agent_behavior(db.node.as_ref(), &behavior)
        .await
        .expect("bind eth tool selection");

    let agent = Gents::from_default_behavior_documents(
        db.node.clone(),
        Arc::clone(&identity),
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .expect("boot agent");
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_ready(db.node.as_ref(), &agent_did).await;
    let _booted = BootedAgent::new(shutdown_tx, handle, agent_did.clone());

    let request_id = "eth-live-block-1";
    create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        request_id,
        "eth-live-session-1",
        "What is the current Base Sepolia block number? Use the base-sepolia_query tool with method eth_blockNumber and empty params. Then tell me the hex result.",
    )
    .await;

    let terminal =
        wait_for_request_terminal(db.node.as_ref(), request_id, Duration::from_secs(180)).await;
    assert_eq!(
        terminal, "completed",
        "live eth query request must complete; last={terminal}"
    );

    let call =
        wait_for_eth_query_tool_call(db.node.as_ref(), request_id, Duration::from_secs(30)).await;
    let args = call.args.unwrap_or_default();
    assert!(
        args.contains("eth_blockNumber"),
        "model must call eth_blockNumber, args={args}"
    );
    let result = call.result.unwrap_or_default();
    assert!(
        result.contains("0x"),
        "live RPC result must be a hex quantity, result={result}"
    );

    let answer =
        wait_for_assistant_answer(db.node.as_ref(), request_id, Duration::from_secs(30)).await;
    assert!(
        !answer.trim().is_empty(),
        "model must persist a non-empty answer after the live query"
    );
}
