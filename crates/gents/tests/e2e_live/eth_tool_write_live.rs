//! Live EthTool write qualification against a local Anvil chain (Foundry,
//! Rust). Hardhat is a fallback if `anvil` is not on PATH.
//!
//! The production write surface cannot take agent-supplied bytecode: `write`
//! declarations require a known `to`, so this harness deploys a tiny Counter
//! as the operator, then GLM uses `send_eth` and `counter_increment`.
//!
//! ```bash
//! GENTS_ETH_LIVE=1 cargo test -p gents --features live-e2e --test e2e_live \
//!   eth_tool_live_model_writes_on_local_chain \
//!   -- --ignored --test-threads=1 --nocapture
//! ```
//!
//! Prefers `anvil` (`GENTS_ANVIL_BIN`, `$PATH`, or `~/.foundry/bin/anvil`).
//! Override the RPC with `GENTS_ETH_WRITE_RPC`. Override inference with
//! `GENTS_ETH_LIVE_ENDPOINT` / `GENTS_ETH_LIVE_MODEL`.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::{
    address_from_secret, attestation_payload, binding_storage_key, encode_attestation,
    generate_secp256k1_secret, upsert_chain_key_binding, upsert_tool_selection, AgentIdentity,
    ChainKeyBindingDocument, ChainKeyMaterialStore, DocumentRuntimeOptions, Gents,
    KeyringChainKeyStore, ToolCeiling, ToolSelectionDocument, KEY_BACKEND_KEYRING,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

use crate::eth_tool_live::{
    assert_endpoint_reachable, bind_glm_backend, fetch_tool_calls, live_enabled, live_endpoint,
};
use crate::steward_loop_live::wait_for_request_terminal;
use crate::support::fixtures::test_identity;
use crate::support::interrupt::{create_runtime_request, wait_for_runtime_ready, BootedAgent};
use crate::support::test_db;

const TOOL_ID: &str = "local";
const BINDING_PREFIX: &str = "eth-write-live";
const RECIPIENT: &str = "0x0000000000000000000000000000000000000b0b";
const FUNDER: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";
const DEPLOYER: &str = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8";
const TRANSFER_WEI: u128 = 1_000_000_000_000_000;
const FUND_WEI: u128 = 10_000_000_000_000_000_000;
const LOCAL_CHAIN_ID: u64 = 31337;
const MAX_FEE_PER_GAS: &str = "100000000000";

/// solc 0.8.24 optimized Counter { uint256 public number; function increment() public { number += 1; } }
const COUNTER_CREATION: &str = "608060405234801561000f575f80fd5b5060c58061001c5f395ff3fe6080604052348015600e575f80fd5b50600436106030575f3560e01c80638381f58a146034578063d09de08a14604d575b5f80fd5b603b5f5481565b60405190815260200160405180910390f35b60536055565b005b60015f8082825460649190606b565b9091555050565b80820180821115608957634e487b7160e01b5f52601160045260245ffd5b9291505056fea264697066735822122000a3e71424d079deaed95e53c48e10615d932a0714eba78a80c57d13e22b571664736f6c63430008180033";
const COUNTER_NUMBER_SELECTOR: &str = "0x8381f58a";

struct LocalChain {
    rpc_url: String,
    chain_id: u64,
    child: Option<Child>,
    _workdir: Option<tempfile::TempDir>,
}

impl Drop for LocalChain {
    fn drop(&mut self) {
        let Some(child) = self.child.as_mut() else {
            return;
        };
        if let Some(pid) = child.id() {
            let _ = std::process::Command::new("kill")
                .args(["-TERM", &format!("-{pid}")])
                .status();
        }
        let _ = child.start_kill();
    }
}

struct ProvisionedKey {
    storage_key: String,
    address: String,
    binding_id: String,
}

impl Drop for ProvisionedKey {
    fn drop(&mut self) {
        let _ = KeyringChainKeyStore.delete(&self.storage_key);
    }
}

struct ApprovalPump {
    cancel: CancellationToken,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for ApprovalPump {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

fn write_rpc_override() -> Option<String> {
    std::env::var("GENTS_ETH_WRITE_RPC")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn ephemeral_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local addr").port()
}

async fn try_rpc(url: &str, method: &str, params: Value) -> Result<Value, String> {
    let client = reqwest::Client::builder()
        .user_agent(gents::ETH_USER_AGENT)
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|error| error.to_string())?;
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let response = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|error| format!("eth RPC {url} {method} unreachable: {error}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| format!("eth RPC {url} {method} body: {error}"))?;
    if !status.is_success() {
        return Err(format!("eth RPC {url} {method} failed {status}: {text}"));
    }
    let parsed: Value = serde_json::from_str(&text)
        .map_err(|error| format!("eth RPC {url} {method} json: {error}: {text}"))?;
    if let Some(error) = parsed.get("error").filter(|error| !error.is_null()) {
        return Err(format!("eth RPC {url} {method} error: {error}"));
    }
    parsed
        .get("result")
        .cloned()
        .ok_or_else(|| format!("eth RPC {url} {method} missing result: {text}"))
}

async fn rpc(url: &str, method: &str, params: Value) -> Value {
    try_rpc(url, method, params)
        .await
        .unwrap_or_else(|error| panic!("{error}"))
}

async fn wait_for_rpc(url: &str, chain_id: u64, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(value) = try_rpc(url, "eth_chainId", json!([])).await {
            if let Some(hex) = value.as_str() {
                if let Ok(actual) = u64::from_str_radix(hex.trim_start_matches("0x"), 16) {
                    if actual == chain_id {
                        return true;
                    }
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn hex_u128(value: u128) -> String {
    format!("0x{value:x}")
}

fn parse_u128_hex(value: &str) -> u128 {
    let hex = value
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    if hex.is_empty() {
        0
    } else {
        u128::from_str_radix(hex, 16).unwrap_or_else(|_| panic!("bad hex quantity {value}"))
    }
}

async fn wait_receipt(url: &str, tx_hash: &str) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let receipt = rpc(url, "eth_getTransactionReceipt", json!([tx_hash])).await;
        if !receipt.is_null() {
            return receipt;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for receipt {tx_hash}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn send_unlocked(
    url: &str,
    from: &str,
    to: Option<&str>,
    value: Option<u128>,
    data: &str,
) -> Value {
    let mut tx = json!({ "from": from });
    if let Some(to) = to {
        tx["to"] = json!(to);
    }
    if let Some(value) = value {
        tx["value"] = json!(hex_u128(value));
    }
    if !data.is_empty() {
        tx["data"] = json!(data);
    }
    let hash = rpc(url, "eth_sendTransaction", json!([tx]))
        .await
        .as_str()
        .expect("eth_sendTransaction hash")
        .to_string();
    wait_receipt(url, &hash).await
}

async fn spawn_hardhat() -> anyhow::Result<(Child, tempfile::TempDir, u16)> {
    let workdir = tempfile::TempDir::new().expect("hardhat workdir");
    std::fs::write(
        workdir.path().join("package.json"),
        r#"{"private":true,"devDependencies":{"hardhat":"2.22.19"}}"#,
    )
    .expect("write hardhat package.json");
    std::fs::write(
        workdir.path().join("hardhat.config.js"),
        "module.exports = { networks: { hardhat: { chainId: 31337 } } };\n",
    )
    .expect("write hardhat.config.js");
    let install = Command::new("npm")
        .args(["install", "--no-fund", "--no-audit", "--silent"])
        .current_dir(workdir.path())
        .stdin(Stdio::null())
        .status()
        .await
        .map_err(|error| anyhow::anyhow!("npm install hardhat: {error}"))?;
    if !install.success() {
        anyhow::bail!(
            "npm install hardhat exited {}",
            install.code().unwrap_or(-1)
        );
    }
    let port = ephemeral_port();
    let mut command = Command::new("npx");
    command
        .args([
            "hardhat",
            "node",
            "--hostname",
            "127.0.0.1",
            "--port",
            &port.to_string(),
        ])
        .current_dir(workdir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let child = command.spawn()?;
    Ok((child, workdir, port))
}

fn anvil_bin() -> Option<std::path::PathBuf> {
    if let Ok(path) = std::env::var("GENTS_ANVIL_BIN") {
        let path = std::path::PathBuf::from(path.trim());
        if path.is_file() {
            return Some(path);
        }
    }
    if let Some(path) = which_bin("anvil") {
        return Some(path);
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let candidate = std::path::PathBuf::from(home).join(".foundry/bin/anvil");
    candidate.is_file().then_some(candidate)
}

fn which_bin(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(name);
        candidate.is_file().then_some(candidate)
    })
}

fn spawn_anvil(bin: &std::path::Path, port: u16) -> anyhow::Result<Child> {
    let mut command = Command::new(bin);
    command
        .args([
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--chain-id",
            &LOCAL_CHAIN_ID.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    Ok(command.spawn()?)
}

async fn start_local_chain() -> LocalChain {
    if let Some(rpc_url) = write_rpc_override() {
        assert!(
            wait_for_rpc(&rpc_url, LOCAL_CHAIN_ID, Duration::from_secs(10)).await,
            "GENTS_ETH_WRITE_RPC={rpc_url} did not serve chain id {LOCAL_CHAIN_ID}"
        );
        return LocalChain {
            rpc_url,
            chain_id: LOCAL_CHAIN_ID,
            child: None,
            _workdir: None,
        };
    }

    if let Some(bin) = anvil_bin() {
        let port = ephemeral_port();
        let rpc_url = format!("http://127.0.0.1:{port}");
        match spawn_anvil(&bin, port) {
            Ok(child) => {
                let chain = LocalChain {
                    rpc_url: rpc_url.clone(),
                    chain_id: LOCAL_CHAIN_ID,
                    child: Some(child),
                    _workdir: None,
                };
                if wait_for_rpc(&rpc_url, LOCAL_CHAIN_ID, Duration::from_secs(20)).await {
                    return chain;
                }
                drop(chain);
            }
            Err(error) => {
                eprintln!("anvil spawn failed ({error:#}); trying hardhat");
            }
        }
    }

    match spawn_hardhat().await {
        Ok((child, workdir, port)) => {
            let rpc_url = format!("http://127.0.0.1:{port}");
            let chain = LocalChain {
                rpc_url: rpc_url.clone(),
                chain_id: LOCAL_CHAIN_ID,
                child: Some(child),
                _workdir: Some(workdir),
            };
            if wait_for_rpc(&rpc_url, LOCAL_CHAIN_ID, Duration::from_secs(120)).await {
                return chain;
            }
            drop(chain);
        }
        Err(error) => {
            panic!(
                "could not start a local chain: anvil not ready and hardhat spawn failed: {error:#}"
            );
        }
    }

    panic!("could not start a local Anvil or Hardhat JSON-RPC on 127.0.0.1")
}

async fn provision_key(node: &EmbeddedNode, identity: &dyn AgentIdentity) -> ProvisionedKey {
    let principal_did = identity.did().to_string();
    let binding_id = format!("{BINDING_PREFIX}-{}", uuid::Uuid::new_v4());
    let secret = generate_secp256k1_secret();
    let address = address_from_secret(&secret).expect("derive agent address");
    let created_at = chrono::Utc::now().to_rfc3339();
    let payload = attestation_payload(
        &binding_id,
        &principal_did,
        &address,
        KEY_BACKEND_KEYRING,
        &created_at,
    );
    let signature = identity
        .sign(&payload)
        .await
        .expect("attest chain key with principal DID");
    let storage_key = binding_storage_key(&principal_did, &binding_id);
    let store = KeyringChainKeyStore;
    let _ = store.delete(&storage_key);
    store
        .store_new(&storage_key, &secret)
        .unwrap_or_else(|error| panic!("store chain key {storage_key}: {error:#}"));
    let loaded = store
        .load(&storage_key)
        .unwrap_or_else(|error| panic!("keyring round-trip load {storage_key}: {error:#}"));
    assert_eq!(
        loaded, secret,
        "stored chain key must round-trip for {storage_key}"
    );
    upsert_chain_key_binding(
        node,
        &ChainKeyBindingDocument {
            binding_id: binding_id.clone(),
            principal_did,
            address: address.clone(),
            key_backend: Some(KEY_BACKEND_KEYRING.to_string()),
            attestation: Some(encode_attestation(&signature)),
            created_at: Some(created_at),
            revoked_at: None,
        },
    )
    .await
    .expect("upsert ChainKeyBinding");
    ProvisionedKey {
        storage_key,
        address,
        binding_id,
    }
}

fn graphql_string_list(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
        .collect::<Vec<_>>()
        .join(", ")
}

async fn create_write_eth_tool(
    node: &EmbeddedNode,
    agent_did: &str,
    rpc_url: &str,
    chain_id: u64,
    binding_id: &str,
    counter: &str,
) {
    let native = json!({
        "kind": "native_transfer",
        "tool_name": "send_eth",
        "description": "Send native ETH from the agent's chain key.",
        "params": {
            "to": { "source": "model", "address_allowlist": [RECIPIENT] },
            "value": { "source": "model", "max": TRANSFER_WEI.to_string() }
        },
        "max_gas": 30_000,
        "max_fee_per_gas": MAX_FEE_PER_GAS
    })
    .to_string();
    let increment = json!({
        "kind": "write",
        "tool_name": "counter_increment",
        "to": counter,
        "signature": "increment()",
        "description": "Increment the local Counter.number storage slot.",
        "params": {},
        "max_gas": 100_000,
        "max_fee_per_gas": MAX_FEE_PER_GAS
    })
    .to_string();
    let number = json!({
        "kind": "read",
        "tool_name": "counter_number",
        "to": counter,
        "signature": "number()",
        "description": "Read Counter.number.",
        "params": {}
    })
    .to_string();
    let calls = graphql_string_list(&[native, increment, number]);
    let methods = graphql_string_list(&[
        "eth_blockNumber".to_string(),
        "eth_chainId".to_string(),
        "eth_getBalance".to_string(),
        "eth_call".to_string(),
    ]);
    let tool_id = escape_graphql_string(TOOL_ID);
    let agent_did = escape_graphql_string(agent_did);
    let rpc_url = escape_graphql_string(rpc_url);
    let binding_id = escape_graphql_string(binding_id);
    let mutation = format!(
        r#"mutation {{
            create_EthTool(input: {{
                tool_id: "{tool_id}",
                agent_did: "{agent_did}",
                display_name: "Local Hardhat",
                enabled: true,
                chain_id: {chain_id},
                rpc_url: "{rpc_url}",
                query_methods: [{methods}],
                calls: [{calls}],
                key_binding_id: "{binding_id}",
                created_at: "2026-08-29T00:00:00Z"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create write EthTool failed: {:?}",
        response.errors
    );
}

fn spawn_approval_pump(node: Arc<EmbeddedNode>, agent_did: String) -> ApprovalPump {
    let cancel = CancellationToken::new();
    let child = cancel.clone();
    let handle = tokio::spawn(async move {
        while !child.is_cancelled() {
            approve_held_calls(node.as_ref(), &agent_did).await;
            tokio::select! {
                _ = child.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_millis(250)) => {}
            }
        }
    });
    ApprovalPump {
        cancel,
        handle: Some(handle),
    }
}

async fn approve_held_calls(node: &EmbeddedNode, agent_did: &str) {
    let escaped = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    lifecycle_state: {{ _eq: "awaitingApproval" }},
                    agent_did: {{ _eq: "{escaped}" }}
                }}
            ) {{
                _docID
                tool_call_id
                request_id
                agent_did
            }}
        }}"#
    );
    #[derive(Deserialize)]
    struct HeldRow {
        #[serde(rename = "_docID")]
        tool_call_doc_id: String,
        tool_call_id: String,
        request_id: Option<String>,
        agent_did: Option<String>,
    }
    let response = node.execute(&query).await;
    if response.has_errors() {
        return;
    }
    let rows: Vec<HeldRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .ok()
        .flatten()
        .unwrap_or_default();
    for row in rows {
        let approval_id = format!("approval-{}-{}", row.tool_call_id, uuid::Uuid::new_v4());
        let approval_id = escape_graphql_string(&approval_id);
        let tool_call_doc_id = escape_graphql_string(&row.tool_call_doc_id);
        let tool_call_id = escape_graphql_string(&row.tool_call_id);
        let agent = escape_graphql_string(row.agent_did.as_deref().unwrap_or(agent_did));
        let approver = escape_graphql_string(agent_did);
        let request_id_field = row
            .request_id
            .as_deref()
            .map(escape_graphql_string)
            .map(|request_id| format!(r#"request_id: "{request_id}","#))
            .unwrap_or_default();
        let created_at = chrono::Utc::now().to_rfc3339();
        let mutation = format!(
            r#"mutation {{
                create_AgentToolApproval(input: {{
                    approval_id: "{approval_id}",
                    tool_call_doc_id: "{tool_call_doc_id}",
                    tool_call_id: "{tool_call_id}",
                    {request_id_field}
                    agent_did: "{agent}",
                    decision: "approved",
                    approver_did: "{approver}",
                    reason: "eth write live e2e auto-approve",
                    created_at: "{created_at}"
                }}) {{ _docID }}
            }}"#
        );
        let _ = node.execute(&mutation).await;
    }
}

async fn wait_for_tool_result(
    node: &EmbeddedNode,
    request_id: &str,
    tool_name: &str,
    timeout: Duration,
) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let rows = fetch_tool_calls(node, request_id).await;
        if let Some(row) = rows.into_iter().find(|row| {
            row.tool_name.as_deref() == Some(tool_name)
                && row
                    .result
                    .as_deref()
                    .is_some_and(|result| !result.trim().is_empty())
        }) {
            return row.result.unwrap_or_default();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {tool_name} result on {request_id}; last calls={:?}",
            fetch_tool_calls(node, request_id).await
        );
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
}

async fn fetch_submissions(node: &EmbeddedNode, principal_did: &str) -> Vec<Value> {
    let escaped = escape_graphql_string(principal_did);
    let query = format!(
        r#"{{
            EthSubmission(filter: {{ principal_did: {{ _eq: "{escaped}" }} }}) {{
                tx_hash
                status
                from_address
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "EthSubmission query failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("EthSubmission"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: set GENTS_ETH_LIVE=1 and pass --ignored"]
async fn eth_tool_live_model_writes_on_local_chain() {
    assert!(
        live_enabled(),
        "set GENTS_ETH_LIVE=1 and pass --ignored to run the live EthTool write qualification"
    );

    let endpoint = live_endpoint();
    assert_endpoint_reachable(&endpoint).await;
    let chain = start_local_chain().await;

    let deploy = send_unlocked(
        &chain.rpc_url,
        DEPLOYER,
        None,
        None,
        &format!("0x{COUNTER_CREATION}"),
    )
    .await;
    let counter = deploy
        .get("contractAddress")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("Counter deploy missing contractAddress: {deploy}"))
        .to_string();
    assert!(
        counter.starts_with("0x") && counter.len() == 42,
        "invalid Counter address {counter}"
    );
    let code = rpc(&chain.rpc_url, "eth_getCode", json!([counter, "latest"]))
        .await
        .as_str()
        .unwrap_or("0x")
        .to_string();
    assert!(
        code.len() > 4,
        "deployed Counter has no code at {counter}: {code}"
    );

    let db = test_db("eth-tool-write-live").await;
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("eth-tool-write-live"));
    let (agent_did, behavior_id) = bind_glm_backend(
        db.node.as_ref(),
        identity.as_ref(),
        "You are an Ethereum operator on a local Hardhat chain. \
         You have send_eth (native transfer), counter_increment (contract write), \
         counter_number (contract read), and local_query (JSON-RPC). \
         When asked to send ETH or increment, call the matching tool immediately. \
         Do not guess transaction hashes.",
    )
    .await;

    let key = provision_key(db.node.as_ref(), identity.as_ref()).await;
    let funded = send_unlocked(
        &chain.rpc_url,
        FUNDER,
        Some(&key.address),
        Some(FUND_WEI),
        "",
    )
    .await;
    assert_eq!(
        funded.get("status").and_then(Value::as_str),
        Some("0x1"),
        "funding the agent key failed: {funded}"
    );
    let agent_balance = parse_u128_hex(
        rpc(
            &chain.rpc_url,
            "eth_getBalance",
            json!([&key.address, "latest"]),
        )
        .await
        .as_str()
        .unwrap_or("0x0"),
    );
    assert!(
        agent_balance >= FUND_WEI,
        "agent {} funded balance {agent_balance} < {FUND_WEI}",
        key.address
    );

    create_write_eth_tool(
        db.node.as_ref(),
        &agent_did,
        &chain.rpc_url,
        chain.chain_id,
        &key.binding_id,
        &counter,
    )
    .await;

    upsert_tool_selection(
        db.node.as_ref(),
        &ToolSelectionDocument {
            selection_id: "eth-write-live-tools".to_string(),
            agent_did: agent_did.clone(),
            enable_file_tools: Some(false),
            enable_bash: Some(false),
            eth_tool_ids: Some(vec![TOOL_ID.to_string()]),
            ..Default::default()
        },
    )
    .await
    .expect("upsert eth write tool selection");

    let mut behavior = gents::load_agent_behavior(db.node.as_ref(), &behavior_id)
        .await
        .expect("load behavior")
        .expect("behavior exists");
    behavior.tool_selection_id = Some("eth-write-live-tools".to_string());
    gents::upsert_agent_behavior(db.node.as_ref(), &behavior)
        .await
        .expect("bind eth write tool selection");

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
    let _approvals = spawn_approval_pump(db.node.clone(), agent_did.clone());

    let transfer_id = "eth-write-live-transfer-1";
    create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        transfer_id,
        "eth-write-live-session-1",
        &format!(
            "Send {TRANSFER_WEI} wei of native ETH to {RECIPIENT} using the send_eth tool. \
             Use exactly to={RECIPIENT} and value={TRANSFER_WEI}. Then stop."
        ),
    )
    .await;
    let terminal =
        wait_for_request_terminal(db.node.as_ref(), transfer_id, Duration::from_secs(180)).await;
    assert_eq!(
        terminal, "completed",
        "native transfer request must complete; last={terminal}"
    );
    let transfer_result = wait_for_tool_result(
        db.node.as_ref(),
        transfer_id,
        "send_eth",
        Duration::from_secs(30),
    )
    .await;
    assert!(
        transfer_result.contains("confirmed_success") && transfer_result.contains("0x"),
        "send_eth must confirm on chain, result={transfer_result}"
    );
    let recipient_balance = parse_u128_hex(
        rpc(
            &chain.rpc_url,
            "eth_getBalance",
            json!([RECIPIENT, "latest"]),
        )
        .await
        .as_str()
        .unwrap_or("0x0"),
    );
    assert_eq!(
        recipient_balance, TRANSFER_WEI,
        "recipient {RECIPIENT} balance {recipient_balance} != {TRANSFER_WEI}"
    );

    let increment_id = "eth-write-live-increment-1";
    create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        increment_id,
        "eth-write-live-session-2",
        "Call the counter_increment tool with no arguments. Then call counter_number and report the value. Then stop.",
    )
    .await;
    let terminal =
        wait_for_request_terminal(db.node.as_ref(), increment_id, Duration::from_secs(180)).await;
    assert_eq!(
        terminal, "completed",
        "counter increment request must complete; last={terminal}"
    );
    let increment_result = wait_for_tool_result(
        db.node.as_ref(),
        increment_id,
        "counter_increment",
        Duration::from_secs(30),
    )
    .await;
    assert!(
        increment_result.contains("confirmed_success") && increment_result.contains("0x"),
        "counter_increment must confirm on chain, result={increment_result}"
    );
    let number = rpc(
        &chain.rpc_url,
        "eth_call",
        json!([{ "to": counter, "data": COUNTER_NUMBER_SELECTOR }, "latest"]),
    )
    .await;
    let number_hex = number.as_str().unwrap_or("0x0");
    assert_eq!(
        parse_u128_hex(number_hex),
        1,
        "Counter.number must be 1 after increment, got {number_hex}"
    );

    let submissions = fetch_submissions(db.node.as_ref(), &agent_did).await;
    let confirmed = submissions
        .iter()
        .filter(|row| row.get("status").and_then(Value::as_str) == Some("confirmed_success"))
        .count();
    assert!(
        confirmed >= 2,
        "expected at least two confirmed EthSubmission rows, got {submissions:?}"
    );
    assert!(
        submissions.iter().any(|row| {
            row.get("from_address")
                .and_then(Value::as_str)
                .is_some_and(|from| from.eq_ignore_ascii_case(&key.address))
        }),
        "EthSubmission must be signed by provisioned key {}, got {submissions:?}",
        key.address
    );
}
