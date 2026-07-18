//! The running demo fleet and its operations: node lifecycle (spawn / health /
//! teardown), `pair` (bring up and pair a 2nd node), `delegate` (cross-node
//! subagent delegation), and `desktop` (launch the paired desktop client).

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use tokio::process::{Child, Command};

use defra_agent::agent::p2p_reconcile::{resolve_template, SOURCE_OPERATOR};
use defra_agent::graphql::escape_graphql_string;

use crate::graphql_access::post_graphql;

use super::backend::BackendChoice;
use super::setup::init_agent;
use super::util::{cli, path_arg, run_cli_json, run_cli_text};

/// The running demo fleet: node A (always) plus an optional paired node B.
pub(super) struct Fleet {
    pub(super) bin: PathBuf,
    pub(super) home_a: PathBuf,
    pub(super) work_a: PathBuf,
    pub(super) graphql_a: String,
    pub(super) did_a: String,
    pub(super) base_port: u16,
    pub(super) backend: BackendChoice,
    pub(super) server_a: Child,
    pub(super) node_b: Option<NodeB>,
}

pub(super) struct NodeB {
    pub(super) home: PathBuf,
    pub(super) graphql: String,
    pub(super) did: String,
    pub(super) server: Child,
}

impl Fleet {
    pub(super) fn teardown(&mut self) {
        if let Some(b) = self.node_b.as_mut() {
            let _ = b.server.start_kill();
        }
        let _ = self.server_a.start_kill();
    }
}

// ---- node lifecycle ---------------------------------------------------------

pub(super) fn spawn_server(bin: &Path, home: &Path, port: u16, log: &Path) -> Result<Child> {
    let file = std::fs::File::create(log).with_context(|| format!("creating {}", log.display()))?;
    let errfile = file.try_clone()?;
    let mut cmd = Command::new(bin);
    cmd.args([
        "server",
        "--home",
        &path_arg(home),
        "--http-port",
        &port.to_string(),
        "--no-codex-shim",
        "--p2p-bind-addr",
        "127.0.0.1",
        "--p2p-port",
        "0",
        "--p2p-relay-mode",
        "disabled",
        "--p2p-discovery",
        "disabled",
    ]);
    // The demo backends use the OpenAI chat-completions wire.
    cmd.env("DEFRA_AGENT_OPENAI_CHAT_COMPLETIONS", "1");
    // Fast pairing reconcile so `pair` converges in seconds, not minutes.
    cmd.env("DEFRA_AGENT_REGISTRY_HEARTBEAT_MS", "1000");
    cmd.env("DEFRA_AGENT_PAIRING_SWEEP_MS", "1000");
    cmd.env("DEFRA_AGENT_REGISTRY_STALE_MS", "300000");
    cmd.env("DEFRA_AGENT_ENDPOINT_HEARTBEAT_MS", "1000");
    cmd.stdout(file).stderr(errfile).kill_on_drop(true);
    cmd.spawn().context("spawning demo server")
}

pub(super) async fn wait_http(url: &str, server: &mut Child) -> Result<()> {
    let client = reqwest::Client::new();
    for _ in 0..300 {
        if client
            .get(url)
            .timeout(Duration::from_millis(500))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            return Ok(());
        }
        if let Ok(Some(status)) = server.try_wait() {
            bail!("demo server exited before becoming ready ({status})");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    bail!("timed out waiting for the demo server at {url}")
}

// ---- pair (2-node fleet) ----------------------------------------------------

pub(super) async fn pair(fleet: &mut Fleet) -> Result<()> {
    if fleet.node_b.is_some() {
        println!("  already paired.");
        return Ok(());
    }
    let bin = fleet.bin.clone();
    let home_a = fleet.home_a.clone();
    let backend = fleet.backend.clone();
    let home_b = home_a.join("node-b");
    let work_b = home_b.join("work");
    let port_b = fleet.base_port + 1;
    let graphql_b = format!("http://127.0.0.1:{port_b}/api/v0/graphql");

    println!("  initializing node B (worker)…");
    let did_b = init_agent(&bin, &home_b, &work_b, &backend, "worker").await?;
    std::fs::create_dir_all(&work_b)?;

    println!("  starting node B…");
    let mut server_b = spawn_server(&bin, &home_b, port_b, &home_b.join("server.log"))?;
    wait_http(&format!("http://127.0.0.1:{port_b}/healthz"), &mut server_b).await?;

    let (peer_a, addr_a) = p2p_identity(&bin, &home_a).await?;
    let (peer_b, addr_b) = p2p_identity(&bin, &home_b).await?;

    println!("  creating the network and enrolling node B…");
    run_cli_text(
        &bin,
        &cli(&[
            "p2p",
            "network",
            "create",
            "--home",
            &path_arg(&home_a),
            "--name",
            "Demo Fleet",
            "--output",
            "json",
        ]),
    )
    .await?;
    run_cli_text(
        &bin,
        &cli(&[
            "p2p",
            "network",
            "grant",
            "--home",
            &path_arg(&home_a),
            &did_b,
            "--output",
            "json",
        ]),
    )
    .await?;
    let token = run_cli_json(
        &bin,
        &cli(&[
            "p2p",
            "pairings",
            "invite",
            "--home",
            &path_arg(&home_a),
            "--member-did",
            &did_b,
            "--template",
            "network-control",
        ]),
    )
    .await?
    .get("token")
    .and_then(Value::as_str)
    .context("invite missing token")?
    .to_string();
    run_cli_text(
        &bin,
        &cli(&[
            "p2p",
            "pairings",
            "join",
            "--home",
            &path_arg(&home_b),
            &token,
        ]),
    )
    .await?;

    println!("  installing the conversation data plane…");
    upsert_data_plane(
        &fleet.graphql_a,
        &peer_b,
        &fleet.did_a,
        &addr_b,
        "conversation",
    )
    .await?;
    upsert_data_plane(&graphql_b, &peer_a, &did_b, &addr_a, "conversation").await?;

    println!("  waiting for conversation data-plane replicators…");
    wait_conversation_replicator(&graphql_b, &peer_a, &did_b).await?;
    wait_conversation_replicator(&fleet.graphql_a, &peer_b, &fleet.did_a).await?;

    fleet.node_b = Some(NodeB {
        home: home_b,
        graphql: graphql_b,
        did: did_b,
        server: server_b,
    });
    println!(
        "  ✓ paired. Node B (worker) is live; run `delegate` to enable cross-node delegation."
    );
    Ok(())
}

async fn p2p_identity(bin: &Path, home: &Path) -> Result<(String, String)> {
    let status = run_cli_json(bin, &cli(&["p2p", "status", "--home", &path_arg(home)])).await?;
    let peer = status
        .get("p2p_peer_id")
        .and_then(Value::as_str)
        .context("p2p status missing p2p_peer_id")?
        .to_string();
    let addr = status
        .get("p2p_shareable_address")
        .and_then(Value::as_str)
        .context("p2p status missing p2p_shareable_address")?
        .to_string();
    Ok((peer, addr))
}

async fn upsert_data_plane(
    graphql: &str,
    peer_id: &str,
    local_did: &str,
    address: &str,
    template: &str,
) -> Result<()> {
    let peer = escape_graphql_string(peer_id);
    let did = escape_graphql_string(local_did);
    let addr = escape_graphql_string(address);
    let cols = data_plane_collections_literal(template)?;
    let template = escape_graphql_string(template);
    let source = escape_graphql_string(SOURCE_OPERATOR);
    let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    // Stamp `source: "operator"` on both branches: the filter is peer_id-only,
    // so an operator upsert deliberately takes ownership of a reciprocal- or
    // legacy-null-sourced row for the same peer. Taking ownership is what
    // protects the row — the reciprocal reconciler only refreshes/deletes rows
    // whose source is "reciprocal" and treats every other source as blocked.
    let mutation = format!(
        r#"mutation {{
  upsert_DataPlanePairingDesired(
    filter: {{ peer_id: {{ _eq: "{peer}" }} }},
    add: {{ peer_id: "{peer}", agent_did: "{did}", collections: {cols}, replicator_addresses: ["{addr}"], template: "{template}", source: "{source}", created_at: "{now}", updated_at: "{now}" }},
    update: {{ agent_did: "{did}", collections: {cols}, replicator_addresses: ["{addr}"], template: "{template}", source: "{source}", updated_at: "{now}" }}
  ) {{ _docID }}
}}"#
    );
    post_graphql(graphql, &mutation).await?;
    Ok(())
}

fn data_plane_collections_literal(template: &str) -> Result<String> {
    use defra_agent::agent::p2p_reconcile::templates::APP_COLLECTIONS_TEMPLATE;
    let template = resolve_template(template)
        .with_context(|| format!("unknown data-plane template {template:?}"))?;
    if template.id == APP_COLLECTIONS_TEMPLATE {
        anyhow::bail!(
            "app-collections requires an explicit collection set; fleet upsert_data_plane \
             cannot expand it from the template (see #607 for config-apply ownership)"
        );
    }
    let collections = template
        .collections
        .iter()
        .map(|collection| format!(r#""{}""#, escape_graphql_string(collection)))
        .collect::<Vec<_>>()
        .join(",");
    Ok(format!("[{collections}]"))
}

async fn wait_conversation_replicator(graphql: &str, peer_id: &str, local_did: &str) -> Result<()> {
    let query = format!(
        r#"{{
            PeerPairingApplied(filter: {{ peer_id: {{ _eq: "{}" }} }}, limit: 1) {{
                peer_id
                collections
                replicator_addresses
                replicator_filter
            }}
            DataPlanePairingDesired(filter: {{ peer_id: {{ _eq: "{}" }} }}, limit: 1) {{
                peer_id
                agent_did
                template
                replicator_addresses
            }}
        }}"#,
        escape_graphql_string(peer_id),
        escape_graphql_string(peer_id)
    );
    let mut last = Value::Null;
    for _ in 0..240 {
        if let Ok(resp) = post_graphql(graphql, &query).await {
            last = resp.get("data").cloned().unwrap_or(Value::Null);
            let armed = resp
                .pointer("/data/PeerPairingApplied/0/replicator_addresses")
                .and_then(Value::as_array)
                .map(|addresses| !addresses.is_empty())
                .unwrap_or(false);
            let filtered = resp
                .pointer("/data/PeerPairingApplied/0/replicator_filter")
                .and_then(Value::as_str)
                .is_some_and(|filter| filter_mentions_agent_request(filter, local_did));
            if armed && filtered {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    bail!(
        "timed out waiting for the conversation data-plane replicator for peer {peer_id}; \
         last pairing rows: {last}"
    )
}

fn filter_mentions_agent_request(filter: &str, local_did: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(filter) else {
        return false;
    };
    value
        .get("AgentRequest")
        .and_then(|entry| entry.get("value"))
        .and_then(Value::as_str)
        == Some(local_did)
}

async fn wait_replicator_filter(graphql: &str, peer_id: &str, needles: &[String]) -> Result<()> {
    let query = format!(
        r#"{{ PeerPairingApplied(filter: {{ peer_id: {{ _eq: "{}" }} }}, limit: 1) {{ replicator_addresses replicator_filter }} }}"#,
        escape_graphql_string(peer_id)
    );
    for _ in 0..240 {
        if let Ok(resp) = post_graphql(graphql, &query).await {
            let armed = resp
                .pointer("/data/PeerPairingApplied/0/replicator_addresses")
                .and_then(Value::as_array)
                .map(|addresses| !addresses.is_empty())
                .unwrap_or(false);
            let filter = resp
                .pointer("/data/PeerPairingApplied/0/replicator_filter")
                .and_then(Value::as_str)
                .unwrap_or("");
            if armed && needles.iter().all(|needle| filter.contains(needle)) {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    bail!("timed out waiting for the data-plane filter for peer {peer_id}")
}

// ---- delegate (cross-node subagent) -----------------------------------------

pub(super) async fn delegate(fleet: &Fleet) -> Result<()> {
    let Some(worker) = &fleet.node_b else {
        bail!("not paired — run `pair` first");
    };
    println!("  configuring cross-node delegation…");
    let (peer_a, addr_a) = p2p_identity(&fleet.bin, &fleet.home_a).await?;
    let (peer_b, addr_b) = p2p_identity(&fleet.bin, &worker.home).await?;

    println!("  switching the data plane to subagent delegation templates…");
    upsert_data_plane(
        &fleet.graphql_a,
        &peer_b,
        &fleet.did_a,
        &addr_b,
        "subagent-coordinator",
    )
    .await?;
    upsert_data_plane(
        &worker.graphql,
        &peer_a,
        &worker.did,
        &addr_a,
        "subagent-host",
    )
    .await?;
    wait_replicator_filter(
        &fleet.graphql_a,
        &peer_b,
        &[
            "AgentToolCall".to_string(),
            "spawn_target_did".to_string(),
            worker.did.clone(),
        ],
    )
    .await?;
    // The host's return leg is requester-scoped to the coordinator DID.
    wait_replicator_filter(
        &worker.graphql,
        &peer_a,
        &["AgentRequest".to_string(), fleet.did_a.clone()],
    )
    .await?;

    // Worker (node B) must accept cross-deployment spawns from the orchestrator.
    let worker_gen = runtime_generation(&worker.graphql).await;
    config_tools(
        &fleet.bin,
        &worker.graphql,
        &worker.did,
        &[
            "--enable-defra-query",
            "true",
            "--defra-query-collection",
            "agent-config",
            "--subagent-allow-cross-deployment",
            "true",
        ],
    )
    .await?;
    wait_runtime_reconcile(&worker.graphql, worker_gen).await;

    // Orchestrator (node A): background spawn of a remote `worker` target.
    let target = format!(
        r#"{{"name":"worker","agent_did":"{}","behavior_id":"{}:default","description":"Remote worker subagent"}}"#,
        worker.did, worker.did
    );
    let orch_gen = runtime_generation(&fleet.graphql_a).await;
    config_tools(
        &fleet.bin,
        &fleet.graphql_a,
        &fleet.did_a,
        &[
            "--enable-defra-query",
            "true",
            "--defra-query-collection",
            "agent-config",
            "--subagent-spawn-enabled",
            "true",
            "--subagent-background-enabled",
            "true",
            "--subagent-allow-cross-deployment",
            "true",
            "--subagent-target",
            &target,
        ],
    )
    .await?;
    wait_runtime_reconcile(&fleet.graphql_a, orch_gen).await;

    println!("  ✓ cross-node delegation enabled.");
    println!("  In `chat`, ask the orchestrator to use its worker subagent, e.g.:");
    println!(
        "    Delegate to the worker subagent: summarize what a worker node does, then report back."
    );
    println!("  The child runs on node B (the worker) and its result replicates back.");
    Ok(())
}

async fn config_tools(bin: &Path, graphql: &str, did: &str, extra: &[&str]) -> Result<()> {
    let mut args: Vec<String> = vec![
        "config".into(),
        "tools".into(),
        "set".into(),
        "--graphql".into(),
        graphql.into(),
        "--agent-did".into(),
        did.into(),
        "--selection-id".into(),
        format!("{did}:default-tools"),
    ];
    args.extend(extra.iter().map(|value| value.to_string()));
    run_cli_text(bin, &args).await?;
    Ok(())
}

async fn runtime_generation(graphql: &str) -> i64 {
    post_graphql(graphql, "{ AgentRuntime { active_generation } }")
        .await
        .ok()
        .and_then(|r| {
            r.pointer("/data/AgentRuntime/0/active_generation")
                .and_then(Value::as_i64)
        })
        .unwrap_or(0)
}

async fn wait_runtime_reconcile(graphql: &str, prev_generation: i64) {
    for _ in 0..80 {
        if let Ok(resp) = post_graphql(
            graphql,
            "{ AgentRuntime { active_generation reconcile_phase } }",
        )
        .await
        {
            let generation = resp
                .pointer("/data/AgentRuntime/0/active_generation")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let phase = resp
                .pointer("/data/AgentRuntime/0/reconcile_phase")
                .and_then(Value::as_str)
                .unwrap_or("");
            if generation > prev_generation && phase == "idle" {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

// ---- desktop (paired desktop client) ----------------------------------------

pub(super) async fn desktop(fleet: &Fleet) -> Result<()> {
    let Some(desktop_bin) = resolve_desktop_bin() else {
        println!("  The desktop app (`defra-agent-desktop`) was not found.");
        println!("  Install it, or set DEFRA_AGENT_DESKTOP_BIN, then run `desktop` again.");
        println!(
            "  To seed it by hand: defra-agent-desktop init --status-endpoint {}",
            fleet.graphql_a
        );
        return Ok(());
    };
    let desktop_home = fleet.home_a.join("desktop");

    println!("  seeding desktop deployment(s)…");
    seed_desktop(
        &desktop_bin,
        &desktop_home,
        &fleet.graphql_a,
        "demo (orchestrator)",
        true,
    )
    .await?;
    if let Some(worker) = &fleet.node_b {
        seed_desktop(
            &desktop_bin,
            &desktop_home,
            &worker.graphql,
            "worker",
            false,
        )
        .await?;
    }

    println!("  launching the desktop app…");
    let mut cmd = std::process::Command::new(&desktop_bin);
    cmd.env("DEFRA_AGENT_DESKTOP_HOME", path_arg(&desktop_home));
    // Loopback demo nodes are already paired by the CLI; don't re-pair remotely.
    cmd.env("DEFRA_AGENT_DESKTOP_PAIR_REMOTE_P2P", "0");
    cmd.spawn().context("launching the desktop app")?;

    println!("  ✓ Desktop app launched — it pairs with your demo node(s) over P2P.");
    println!("    It opens in a separate window; keep this demo running. Open the Fleet");
    println!("    Dashboard to see your node(s); Chat mirrors what you do here.");
    Ok(())
}

async fn seed_desktop(
    desktop_bin: &Path,
    desktop_home: &Path,
    graphql: &str,
    label: &str,
    overwrite: bool,
) -> Result<()> {
    let mut args: Vec<String> = vec![
        "init".into(),
        "--desktop-home".into(),
        path_arg(desktop_home),
        "--status-endpoint".into(),
        graphql.into(),
        "--label".into(),
        label.into(),
    ];
    if overwrite {
        args.push("--dangerously-overwrite".into());
    }
    run_cli_text(desktop_bin, &args).await?;
    Ok(())
}

/// Locate the `defra-agent-desktop` launcher: explicit env, then a sibling of
/// this binary, then PATH.
fn resolve_desktop_bin() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("DEFRA_AGENT_DESKTOP_BIN") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(sibling) = exe.parent().map(|dir| dir.join("defra-agent-desktop")) {
            if sibling.is_file() {
                return Some(sibling);
            }
        }
    }
    let name = "defra-agent-desktop";
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}
