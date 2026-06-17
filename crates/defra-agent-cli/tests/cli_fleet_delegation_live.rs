//! Five-process fleet e2e for filtered conversation pairing and
//! cross-deployment live subagent delegation, driven entirely by the daemon
//! reconcilers (no direct REST replicator install). Each coordinator<->subagent
//! edge is an independent two-node reconcile with two documents: (1) a v5
//! network-control join (P2P mesh + control-plane document gossip) and (2) an
//! operator conversation `DataPlanePairingDesired` row (the doc sync delegation
//! needs). See `establish_reconciler_pairing`.
//!
//! Requires the defradb iroh fixes in sourcenetwork/defradb.rs#1045 (addr
//! hygiene + observed-addr reverse-dial fallback + spawning the dial off the
//! command loop). The load-bearing one is the spawn: defradb's iroh command
//! loop awaited the blocking `endpoint.connect()` inline, starving `accept()`,
//! so two peers dialing each other in-window deadlocked — the #511 wall. With
//! #1045 this converges reliably (2-node 8/8, 5-node substrate 5/5, full
//! delegation 5/5). The convergence checkpoint still dumps doc-state + full
//! daemon logs on timeout (`dump_fleet_doc_state` / `persist_fleet_logs`) for
//! future triage. Until #1045 merges and the workspace `defradb` rev is bumped,
//! this passes only against a local `[patch]` of defradb.
//!
//! Normal test runs compile this file but skip the live test. To run:
//!
//! ```bash
//! DEFRA_AGENT_LIVE_OPENAI=1 \
//! DEFRA_AGENT_LIVE_OPENAI_ENDPOINT="http://host:8000/v1" \
//! DEFRA_AGENT_LIVE_OPENAI_MODEL="model-name" \
//!   cargo test -p defra-agent-cli --test cli_fleet_delegation_live -- --ignored --nocapture
//! ```

mod support;
use support::*;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use defra_agent::subagent_target_entry;
use serde_json::Value;
use uuid::Uuid;

const P2P_LOOPBACK_ARGS: &[&str] = &[
    "--p2p-bind-addr",
    "127.0.0.1",
    "--p2p-port",
    "0",
    "--p2p-relay-mode",
    "disabled",
    "--p2p-discovery",
    "disabled",
];

const FAST_RECONCILE_ENVS: &[(&str, &str)] = &[
    ("DEFRA_AGENT_REGISTRY_HEARTBEAT_MS", "1000"),
    ("DEFRA_AGENT_PAIRING_SWEEP_MS", "1000"),
    ("DEFRA_AGENT_REGISTRY_STALE_MS", "300000"),
    ("DEFRA_AGENT_ENDPOINT_HEARTBEAT_MS", "1000"),
    // Reconcile-level tracing aids triage when dump_fleet_logs fires on a
    // convergence timeout, without transport noise (the daemon mutes iroh/p2p to
    // warn regardless via with_default_transport_noise_filters).
    ("RUST_LOG", "warn,defra_agent::agent::p2p_reconcile=debug"),
];

const CONVERSATION_COLLECTIONS: &[&str] = &[
    "AgentRequest",
    "AgentResponse",
    "AgentMessage",
    "AgentToolCall",
    "AgentToolResult",
    "AgentSession",
    "AgentConversation",
    "CompactionEntry",
];

const COORDINATOR_SYSTEM_PROMPT: &str = r#"You are a fleet coordinator. You have four remote research subagents named researcher-1, researcher-2, researcher-3, and researcher-4. For any user request asking you to use the fleet, call the spawn_subagent tool exactly once for each of those four researcher names, and each call must set await_mode to "background". Do not use foreground. Do not call spawn_subagent more than four total times. Do not call any other tool. After the four background calls are issued, reply briefly that all four researchers were delegated."#;

const SUBAGENT_SYSTEM_PROMPT: &str = r#"You are a remote research subagent. Answer the assigned question directly in one short paragraph. Do not delegate to other subagents."#;

struct FleetNode {
    home: PathBuf,
    graphql: String,
    agent_did: String,
    peer_id: String,
    /// Readiness-reported address (host:port form). Retained for debugging; the
    /// data-plane row uses `shareable` instead (see below).
    #[allow(dead_code)]
    address: String,
    /// The node's shareable address in the SAME form the runtime advertises via
    /// PeerEndpoint (`/p2p/shareable-address`, an iroh endpoint ticket). Used for
    /// the data-plane row so it matches the control-plane (network-derived)
    /// address and the merged replicator holds ONE address per peer, not two.
    shareable: String,
    behavior_id: String,
    tool_selection_id: String,
    backend_id: String,
    inference_profile_id: String,
    model_name: String,
    #[allow(dead_code)]
    serve: ServeProcess,
}

#[derive(Debug, Clone)]
struct BridgeRow {
    tool_call_id: String,
    lifecycle_state: String,
    child_request_id: String,
    await_mode: Option<String>,
}

#[derive(Debug, Clone)]
struct ChildRow {
    request_id: String,
    agent_did: String,
    behavior_id: String,
    lifecycle_state: Option<String>,
    caused_by_parent_request_id: Option<String>,
    caused_by_parent_tool_call_id: Option<String>,
    caused_by_trigger_kind: Option<String>,
}

#[derive(Debug, Clone)]
struct CompletedChild {
    tool_call_id: String,
    child_request_id: String,
    owner_agent_did: String,
    owner_behavior_id: String,
    owner_answer: String,
    coordinator_answer: String,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "live: set DEFRA_AGENT_LIVE_OPENAI=1 and pass --ignored"]
async fn five_process_filtered_conversation_delegation_live() -> Result<()> {
    if std::env::var("DEFRA_AGENT_LIVE_OPENAI").as_deref() != Ok("1") {
        tracing::info!("DEFRA_AGENT_LIVE_OPENAI != 1; skipping fleet live e2e");
        return Ok(());
    }

    let endpoint = std::env::var("DEFRA_AGENT_LIVE_OPENAI_ENDPOINT")
        .or_else(|_| std::env::var("DEFRA_AGENT_CLI_E2E_MODEL_ENDPOINT"))
        .unwrap_or_else(|_| DEFAULT_MODEL_ENDPOINT.to_string());
    let model = std::env::var("DEFRA_AGENT_LIVE_OPENAI_MODEL")
        .or_else(|_| std::env::var("DEFRA_AGENT_CLI_E2E_MODEL_NAME"))
        .unwrap_or_else(|_| DEFAULT_MODEL_NAME.to_string());
    assert_endpoint_reachable(&endpoint).await?;

    // Fleet size is env-overridable for substrate diagnostics (default 5 = the
    // full delegation fleet). DEFRA_AGENT_FLEET_SUBSTRATE_ONLY=1 returns right
    // after the pairing convergence checkpoint, skipping inference — used to
    // bisect the reconciler/transport substrate independent of the model.
    let fleet_size: usize = std::env::var("DEFRA_AGENT_FLEET_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let substrate_only = std::env::var("DEFRA_AGENT_FLEET_SUBSTRATE_ONLY").as_deref() == Ok("1");

    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let fleet = bring_up_fleet(tempdir.path(), fleet_size, &endpoint, &model).await?;
    let (coord, subagents) = fleet
        .split_first()
        .ok_or_else(|| anyhow!("fleet should contain a coordinator"))?;

    // Reconciler-driven pairing (no direct REST replicator install). Each
    // coordinator<->subagent edge is an independent two-node reconcile with TWO
    // documents, exactly as the runtime is meant to be driven:
    //   Layer 1 — P2P mesh + network control-plane gossip: the coordinator is the
    //     network admin (genesis); it grants each subagent and issues a single-use
    //     v5 invite; each subagent joins. join is document-only — it writes a
    //     network-control PeerPairingDesired row and the DAEMON's pairing
    //     reconciler does the connect + control-plane replicator install, so the
    //     network membership/endpoint docs gossip and members learn the mesh.
    //   Layer 2 — conversation document sync: operator writes a conversation
    //     DataPlanePairingDesired row on BOTH sides of each edge. Once Layer 1 has
    //     replicated membership (the master gate), the reconciler honors it and
    //     installs the filtered conversation replicator merged onto the substrate
    //     edge — the doc sync subagent delegation needs.
    establish_reconciler_pairing(coord, subagents).await?;

    // Convergence checkpoint (pre-inference): the daemon reconciler must reach
    // PeerPairingApplied with a non-null replicator_addresses on each of the 4
    // bidirectional coordinator<->subagent edges. This is where a transport
    // failure would surface (PairingTransport MODE A connect-fails / B replicator
    // dial / C cid-filter); on timeout we dump daemon logs to triage by signature.
    if let Err(error) = wait_for_fleet_pairing(coord, subagents).await {
        dump_fleet_doc_state(&fleet).await;
        persist_fleet_logs(&fleet, "fail");
        dump_fleet_logs(&fleet);
        return Err(error);
    }
    assert_no_subagent_data_plane_edges(subagents).await?;

    if substrate_only {
        // Diagnostic: persist each daemon's full captured log so the convergence
        // timeline can be analyzed even on a passing run (the temp logs are
        // dropped with the fleet otherwise).
        persist_fleet_logs(&fleet, "pass");
        tracing::info!(
            fleet_size,
            "reconciler-driven pairing converged on all edges; \
             DEFRA_AGENT_FLEET_SUBSTRATE_ONLY=1 set, skipping delegation"
        );
        drop(fleet);
        return Ok(());
    }

    configure_fleet_behaviors(tempdir.path(), coord, subagents).await?;
    wait_for_runtime_quiescence(&coord.graphql, &coord.agent_did, 2, Duration::from_secs(6))
        .await?;
    for subagent in subagents {
        wait_for_runtime_quiescence(
            &subagent.graphql,
            &subagent.agent_did,
            2,
            Duration::from_secs(6),
        )
        .await?;
    }

    let parent_prompt = "Use all four research subagents in parallel with background spawns only. Ask researcher-1 for one concise fact about Mercury, researcher-2 for one concise fact about Venus, researcher-3 for one concise fact about Earth, and researcher-4 for one concise fact about Mars. Make exactly four spawn_subagent calls total, one per researcher, then stop and reply that all four background researchers were delegated.";
    let submit = run_cli_json(
        &coord.home,
        &[
            "request",
            "submit",
            "--graphql",
            &coord.graphql,
            "--agent-did",
            &coord.agent_did,
            "--behavior-id",
            &coord.behavior_id,
            "--content",
            parent_prompt,
            "--no-wait",
        ],
    )?;
    let parent_request_id = required_output_string(&submit, "request_id")?;
    let parent_session_id = required_output_string(&submit, "session_id")?;

    let completed_children = wait_for_all_subagent_children_completed(
        &coord.graphql,
        subagents,
        &parent_session_id,
        &parent_request_id,
        Duration::from_secs(300),
    )
    .await?;

    let parent_terminal =
        wait_for_request_terminal(&coord.graphql, &parent_request_id, Duration::from_secs(240))
            .await?;
    assert_eq!(
        parent_terminal, "completed",
        "parent request must complete successfully"
    );
    let parent_answer =
        wait_for_assistant_answer(&coord.graphql, &parent_request_id, Duration::from_secs(60))
            .await?;
    anyhow::ensure!(
        !parent_answer.trim().is_empty(),
        "parent request completed with an empty response"
    );

    assert_subagent_store_scopes(coord, subagents, &completed_children).await?;
    assert_subagents_have_no_spawn_targets(subagents).await?;

    drop(fleet);
    Ok(())
}

async fn bring_up_fleet(
    root: &Path,
    count: usize,
    model_endpoint: &str,
    model_name: &str,
) -> Result<Vec<FleetNode>> {
    let mut nodes = Vec::with_capacity(count);
    for index in 0..count {
        let label = if index == 0 {
            "coordinator".to_string()
        } else {
            format!("subagent-{index}")
        };
        let home = root.join(&label);
        fs::create_dir_all(&home)?;
        let port = allocate_port()?;
        let graphql = graphql_url(port);
        let agent_name = format!("fleet-{label}-{}", Uuid::new_v4().simple());

        let init = run_init_json(
            &home,
            &[
                "--agent-name",
                &agent_name,
                "--model-name",
                model_name,
                "--max-concurrent",
                "4",
                "--max-queue-depth",
                "16",
                model_endpoint,
            ],
        )?;
        let agent_did = agent_did_from_init(&init)?;
        let behavior_id = init_string(&init, "default_behavior_id")?;
        let tool_selection_id = init_string(&init, "tool_selection_id")?;
        let backend_id = init_string(&init, "backend_id")?;
        let inference_profile_id = init_string(&init, "inference_profile_id")?;
        let model_name = init_string(&init, "model_name")?;

        let (mut serve, readiness) =
            spawn_server_with_ready_json(&home, port, P2P_LOOPBACK_ARGS, FAST_RECONCILE_ENVS)?;
        wait_for_port(port, &mut serve)?;
        wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;
        let peer_id = readiness
            .get("p2p_peer_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| anyhow!("{label} readiness missing p2p_peer_id: {readiness}"))?;
        let address = shareable_address_from_readiness(&readiness, &peer_id)
            .with_context(|| format!("{label} readiness missing P2P address: {readiness}"))?;
        let shareable = fetch_shareable_address(&graphql)
            .await
            .with_context(|| format!("{label} fetching shareable P2P address"))?;

        nodes.push(FleetNode {
            home,
            graphql,
            agent_did,
            peer_id,
            address,
            shareable,
            behavior_id,
            tool_selection_id,
            backend_id,
            inference_profile_id,
            model_name,
            serve,
        });
    }
    Ok(nodes)
}

fn init_string(init: &Value, key: &str) -> Result<String> {
    let nested = format!("/init/{key}");
    init.get(key)
        .or_else(|| init.pointer(&nested))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("init output missing {key}: {init}"))
}

fn shareable_address_from_readiness(readiness: &Value, peer_id: &str) -> Option<String> {
    let raw = readiness
        .get("p2p_shareable_address")
        .and_then(Value::as_str)
        .or_else(|| {
            readiness
                .get("p2p_listen_addresses")
                .and_then(Value::as_array)
                .and_then(|rows| rows.iter().find_map(Value::as_str))
        })?
        .trim();
    if raw.is_empty() {
        None
    } else if raw.contains("/p2p/") {
        Some(raw.to_string())
    } else {
        Some(format!("{raw}/p2p/{peer_id}"))
    }
}

fn required_output_string(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("output missing {key}: {value}"))
}

/// Fetch the node's shareable P2P address (iroh endpoint ticket) from the live
/// `/p2p/shareable-address` endpoint — the SAME form the PeerEndpoint heartbeat
/// publishes and the network-control layer derives. Retries briefly so a node
/// that has not yet learned a dialable direct address (Fix A returns None until
/// it has one) is given a moment to settle.
async fn fetch_shareable_address(graphql: &str) -> Result<String> {
    let api_base = graphql
        .strip_suffix("/graphql")
        .with_context(|| format!("unexpected GraphQL endpoint shape: {graphql}"))?;
    let url = format!("{api_base}/p2p/shareable-address");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("building shareable-address client")?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(resp) = client.get(&url).send().await {
            if let Ok(value) = resp.json::<Value>().await {
                if let Some(addr) = value
                    .get("address")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                {
                    return Ok(addr.to_string());
                }
            }
        }
        if Instant::now() >= deadline {
            bail!("timed out fetching shareable address from {url}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Drive both reconciler documents for every coordinator<->subagent edge, then
/// return. All work is document writes (CLI join / GraphQL upsert); the daemon
/// reconcilers do the connect + replicator install.
async fn establish_reconciler_pairing(coord: &FleetNode, subagents: &[FleetNode]) -> Result<()> {
    // Layer 1: genesis + grants + single-use v5 invites + joins.
    run_cli_json(
        &coord.home,
        &[
            "p2p",
            "network",
            "create",
            "--name",
            "Fleet One",
            "--output",
            "json",
        ],
    )
    .context("coordinator network create")?;
    for subagent in subagents {
        run_cli_json(
            &coord.home,
            &[
                "p2p",
                "network",
                "grant",
                &subagent.agent_did,
                "--output",
                "json",
            ],
        )
        .with_context(|| format!("granting membership to {}", subagent.agent_did))?;
    }
    for subagent in subagents {
        // The invite MUST carry the network-control template: `p2p pairings
        // invite` otherwise defaults to "conversation", and a conversation Push
        // edge replicates only conversation collections — it never gossips
        // PeerEndpoint/NetworkMembership, so the coordinator never learns the
        // joiner's endpoint, never derives the reverse mesh edge, and the
        // membership gate for the Layer-2 conversation rows never opens.
        let invite = run_cli_json(
            &coord.home,
            &[
                "p2p",
                "pairings",
                "invite",
                "--member-did",
                &subagent.agent_did,
                "--template",
                "network-control",
            ],
        )
        .with_context(|| format!("minting v5 invite for {}", subagent.agent_did))?;
        let token = invite
            .get("token")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("invite for {} missing token: {invite}", subagent.agent_did))?;
        let joined = run_cli_json(&subagent.home, &["p2p", "pairings", "join", token])
            .with_context(|| format!("{} joining fleet", subagent.agent_did))?;
        let status = joined.get("status").and_then(Value::as_str);
        anyhow::ensure!(
            matches!(status, Some("pairing_joined") | Some("pairing_exists")),
            "unexpected join status for {}: {joined}",
            subagent.agent_did
        );
    }

    // Layer 2: conversation data-plane row on both sides of each edge. Each node
    // pushes its OWN agent_did's docs to the peer (the scoped filter the prior
    // direct install used and the conversation template's `agent_did` scope).
    for subagent in subagents {
        // Use the peer's SHAREABLE address (same form the network-control layer
        // derives from PeerEndpoint), so the merged per-peer replicator holds ONE
        // address, not two — eliminating the reinstall churn (and dial flake) the
        // two-address mismatch caused.
        upsert_conversation_data_plane(
            &coord.graphql,
            &subagent.peer_id,
            &coord.agent_did,
            &subagent.shareable,
        )
        .await?;
        upsert_conversation_data_plane(
            &subagent.graphql,
            &coord.peer_id,
            &subagent.agent_did,
            &coord.shareable,
        )
        .await?;
    }
    Ok(())
}

/// Operator-write a conversation `DataPlanePairingDesired` row: `peer_id` is who
/// to dial, `address` is the peer's expected shareable address, and `agent_did`
/// is the local scope filter (docs with this DID are pushed to the peer). The
/// reconciler resolves the `conversation` template into a filtered Push
/// replicator, gated on the peer being a materializable network member (Layer 1);
/// the signed materialized `PeerEndpoint` remains authoritative for the actual
/// dial address.
async fn upsert_conversation_data_plane(
    graphql: &str,
    peer_id: &str,
    agent_did: &str,
    address: &str,
) -> Result<()> {
    let peer_id = escape_graphql_string(peer_id);
    let agent_did = escape_graphql_string(agent_did);
    let address = escape_graphql_string(address);
    let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let collections = CONVERSATION_COLLECTIONS
        .iter()
        .map(|collection| format!("\"{collection}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let mutation = format!(
        r#"mutation {{
            upsert_DataPlanePairingDesired(
                filter: {{ peer_id: {{ _eq: "{peer_id}" }} }},
                add: {{
                    peer_id: "{peer_id}",
                    agent_did: "{agent_did}",
                    collections: [{collections}],
                    replicator_addresses: ["{address}"],
                    template: "conversation",
                    created_at: "{now}",
                    updated_at: "{now}"
                }},
                update: {{
                    agent_did: "{agent_did}",
                    collections: [{collections}],
                    replicator_addresses: ["{address}"],
                    template: "conversation",
                    updated_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    );
    graphql_query(graphql, &mutation).await?;
    Ok(())
}

/// Wait for the daemon reconciler to install the replicator on all 4
/// bidirectional coordinator<->subagent edges.
async fn wait_for_fleet_pairing(coord: &FleetNode, subagents: &[FleetNode]) -> Result<()> {
    for subagent in subagents {
        // Join direction first (joiner -> inviter) — the proven 2-node primitive.
        wait_for_replicator_installed(&subagent.graphql, &coord.peer_id, Duration::from_secs(120))
            .await
            .with_context(|| {
                format!(
                    "{} -> coordinator conversation replicator",
                    subagent.agent_did
                )
            })?;
        // Reverse edge (inviter -> joiner) — depends on the joiner's endpoint
        // having replicated to the coordinator (network derivation).
        wait_for_replicator_installed(&coord.graphql, &subagent.peer_id, Duration::from_secs(120))
            .await
            .with_context(|| {
                format!(
                    "coordinator -> {} conversation replicator",
                    subagent.agent_did
                )
            })?;
    }
    Ok(())
}

/// On a pairing-convergence failure, dump the durable pairing/network document
/// state on every node — far more decisive than logs for "why no wiring":
/// shows which desired/applied rows exist, which memberships/endpoints have
/// replicated, and where the bootstrap chain stalled.
async fn dump_fleet_doc_state(fleet: &[FleetNode]) {
    let query = r#"{
        AgentNetwork { network_id admin_did }
        NetworkMembership { member_did status }
        PeerEndpoint { did }
        PeerPairingDesired { peer_id source template replicator_addresses }
        DataPlanePairingDesired { peer_id template replicator_addresses }
        PeerPairingApplied { peer_id collections replicator_addresses }
    }"#;
    for node in fleet {
        match graphql_query(&node.graphql, query).await {
            Ok(response) => {
                let data = response.get("data").unwrap_or(&response);
                eprintln!(
                    "\n##### DOC STATE {} (peer={}) #####\n{}",
                    node.agent_did,
                    node.peer_id,
                    serde_json::to_string_pretty(data).unwrap_or_default()
                );
            }
            Err(error) => eprintln!("(doc-state query failed for {}: {error})", node.agent_did),
        }
    }
}

/// Poll `PeerPairingApplied` for `peer_id` until its `replicator_addresses` is
/// non-empty — i.e. the reconciler actually installed the replicator (the
/// PairingTransport `ReplicatorLiveness` property, concretely).
async fn wait_for_replicator_installed(
    graphql: &str,
    peer_id: &str,
    timeout: Duration,
) -> Result<()> {
    let escaped = escape_graphql_string(peer_id);
    let query = format!(
        r#"{{ PeerPairingApplied(filter: {{ peer_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{ peer_id collections replicator_addresses }} }}"#
    );
    let deadline = Instant::now() + timeout;
    let mut last = Value::Null;
    loop {
        let response = graphql_query(graphql, &query).await?;
        if let Some(row) = response
            .pointer("/data/PeerPairingApplied")
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
        {
            last = row.clone();
            let installed = row
                .get("replicator_addresses")
                .and_then(Value::as_array)
                .is_some_and(|addrs| {
                    addrs
                        .iter()
                        .any(|a| a.as_str().is_some_and(|s| !s.is_empty()))
                });
            if installed {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for replicator install on edge peer={peer_id} (graphql={graphql}); last PeerPairingApplied row: {last}"
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Persist each daemon's FULL captured stdout+stderr to `/tmp/fleet_<label>_<suffix>.log`
/// for offline timeline/trace analysis (the temp logs are dropped with the fleet).
fn persist_fleet_logs(fleet: &[FleetNode], suffix: &str) {
    for (idx, node) in fleet.iter().enumerate() {
        if let Ok((stdout, stderr)) = node.serve.captured_output() {
            let label = if idx == 0 {
                "coordinator".to_string()
            } else {
                format!("subagent-{idx}")
            };
            let path = format!("/tmp/fleet_{label}_{suffix}.log");
            let _ = std::fs::write(
                &path,
                format!(
                    "# {} peer={}\n=== STDOUT ===\n{stdout}\n=== STDERR ===\n{stderr}\n",
                    node.agent_did, node.peer_id
                ),
            );
            eprintln!("wrote {path} ({} stderr bytes)", stderr.len());
        }
    }
}

/// Dump each daemon's captured log tail — called on a pairing-convergence
/// failure so the transport mode (A: "Address Lookup failed"/dial timeout;
/// C: "collection not found"/filter) is visible without re-running.
fn dump_fleet_logs(fleet: &[FleetNode]) {
    let tail = |text: &str| {
        let lines: Vec<&str> = text.lines().collect();
        let start = lines.len().saturating_sub(120);
        lines[start..].join("\n")
    };
    for node in fleet {
        match node.serve.captured_output() {
            Ok((stdout, stderr)) => {
                eprintln!(
                    "\n===== {} ({}) stderr tail =====\n{}",
                    node.agent_did,
                    node.graphql,
                    tail(&stderr)
                );
                if !stdout.trim().is_empty() {
                    eprintln!(
                        "----- {} stdout tail -----\n{}",
                        node.agent_did,
                        tail(&stdout)
                    );
                }
            }
            Err(error) => eprintln!("(could not read logs for {}: {error})", node.agent_did),
        }
    }
}

/// No-crosswise is a DATA-PLANE property: subagents must never get a conversation
/// `DataPlanePairingDesired` edge to another subagent (the star). Control-plane
/// `PeerPairingDesired` mesh edges between members are expected and allowed.
async fn assert_no_subagent_data_plane_edges(subagents: &[FleetNode]) -> Result<()> {
    let sub_peer_ids = subagents
        .iter()
        .map(|node| node.peer_id.as_str())
        .collect::<HashSet<_>>();
    for node in subagents {
        let response = graphql_query(
            &node.graphql,
            r#"{ DataPlanePairingDesired { peer_id agent_did template } }"#,
        )
        .await?;
        let rows = response
            .pointer("/data/DataPlanePairingDesired")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for row in rows {
            let peer_id = row.get("peer_id").and_then(Value::as_str).unwrap_or("");
            anyhow::ensure!(
                !sub_peer_ids.contains(peer_id),
                "subagent {} unexpectedly has a data-plane edge to another subagent: {row}",
                node.agent_did
            );
        }
    }
    Ok(())
}

async fn configure_fleet_behaviors(
    root: &Path,
    coord: &FleetNode,
    subagents: &[FleetNode],
) -> Result<()> {
    let coord_prompt = root.join("coordinator-system-prompt.txt");
    fs::write(&coord_prompt, COORDINATOR_SYSTEM_PROMPT)?;
    configure_behavior_prompt(coord, &coord_prompt, "Fleet Coordinator")?;

    let sub_prompt = root.join("subagent-system-prompt.txt");
    fs::write(&sub_prompt, SUBAGENT_SYSTEM_PROMPT)?;
    for (index, subagent) in subagents.iter().enumerate() {
        configure_behavior_prompt(
            subagent,
            &sub_prompt,
            &format!("Fleet Researcher {}", index + 1),
        )?;
        configure_subagent_target_gate(subagent)?;
    }
    configure_coordinator_targets(coord, subagents)?;
    Ok(())
}

fn configure_behavior_prompt(
    node: &FleetNode,
    prompt_path: &Path,
    display_name: &str,
) -> Result<()> {
    run_cli_json(
        &node.home,
        &[
            "config",
            "behavior",
            "set",
            "--graphql",
            &node.graphql,
            "--agent-did",
            &node.agent_did,
            "--behavior-id",
            &node.behavior_id,
            "--display-name",
            display_name,
            "--system-prompt-file",
            prompt_path
                .to_str()
                .ok_or_else(|| anyhow!("system prompt path is not UTF-8"))?,
            "--backend-id",
            &node.backend_id,
            "--model-name",
            &node.model_name,
            "--tool-selection-id",
            &node.tool_selection_id,
            "--inference-profile-id",
            &node.inference_profile_id,
        ],
    )?;
    Ok(())
}

fn configure_subagent_target_gate(node: &FleetNode) -> Result<()> {
    run_cli_json(
        &node.home,
        &[
            "config",
            "tools",
            "set",
            "--graphql",
            &node.graphql,
            "--agent-did",
            &node.agent_did,
            "--selection-id",
            &node.tool_selection_id,
            "--display-name",
            "Fleet Researcher Tools",
            "--clear-subagent-targets",
            "--subagent-spawn-enabled",
            "false",
            "--subagent-background-enabled",
            "false",
            "--subagent-allow-cross-deployment",
            "true",
            "--enable-meta-tools",
            "false",
            "--enable-defra-query",
            "false",
        ],
    )?;
    Ok(())
}

fn configure_coordinator_targets(coord: &FleetNode, subagents: &[FleetNode]) -> Result<()> {
    let mut args = vec![
        "config".to_string(),
        "tools".to_string(),
        "set".to_string(),
        "--graphql".to_string(),
        coord.graphql.clone(),
        "--agent-did".to_string(),
        coord.agent_did.clone(),
        "--selection-id".to_string(),
        coord.tool_selection_id.clone(),
        "--display-name".to_string(),
        "Fleet Coordinator Tools".to_string(),
        "--subagent-spawn-enabled".to_string(),
        "true".to_string(),
        "--subagent-background-enabled".to_string(),
        "true".to_string(),
        "--subagent-steering-enabled".to_string(),
        "false".to_string(),
        "--subagent-allow-cross-deployment".to_string(),
        "true".to_string(),
        "--cross-deployment-spawn-timeout-seconds".to_string(),
        "180".to_string(),
        "--enable-meta-tools".to_string(),
        "false".to_string(),
        "--enable-defra-query".to_string(),
        "false".to_string(),
    ];
    for (index, subagent) in subagents.iter().enumerate() {
        args.push("--subagent-target".to_string());
        args.push(subagent_target_entry(
            &format!("researcher-{}", index + 1),
            &subagent.agent_did,
            &subagent.behavior_id,
            Some(format!("Remote fleet researcher {}", index + 1)),
        ));
    }
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    run_cli_json(&coord.home, &refs)?;
    Ok(())
}

async fn wait_for_all_subagent_children_completed(
    coord_graphql: &str,
    subagents: &[FleetNode],
    parent_session_id: &str,
    parent_request_id: &str,
    timeout: Duration,
) -> Result<HashMap<String, CompletedChild>> {
    wait_until_value(timeout, || async {
        let bridges = fetch_spawn_bridges(coord_graphql, parent_session_id).await?;
        anyhow::ensure!(
            bridges.len() >= subagents.len(),
            "saw {} spawn bridges, expected at least {}",
            bridges.len(),
            subagents.len()
        );

        let mut completed_by_owner = HashMap::new();
        let mut pending = Vec::new();
        for bridge in &bridges {
            anyhow::ensure!(
                bridge.await_mode.as_deref() == Some("background"),
                "bridge {} must use background mode: {bridge:?}",
                bridge.tool_call_id
            );
            anyhow::ensure!(
                bridge.lifecycle_state != "failed",
                "bridge {} failed before child completion: {bridge:?}",
                bridge.tool_call_id
            );

            let Some((owner, child)) =
                find_child_on_any_subagent(subagents, &bridge.child_request_id).await?
            else {
                pending.push(format!(
                    "child {} from bridge {} not materialized",
                    bridge.child_request_id, bridge.tool_call_id
                ));
                continue;
            };

            assert_child_lineage(&child, owner, bridge, parent_request_id)?;

            let child_state = child
                .lifecycle_state
                .clone()
                .or(fetch_request_lifecycle(&owner.graphql, &child.request_id).await?)
                .unwrap_or_else(|| "unknown".to_string());
            if child_state != "completed" {
                pending.push(format!(
                    "child {} on {} not completed yet: {child_state}",
                    child.request_id, owner.agent_did
                ));
                continue;
            }

            if bridge.lifecycle_state != "completed" {
                pending.push(format!(
                    "bridge {} for child {} not completed yet: {}",
                    bridge.tool_call_id, child.request_id, bridge.lifecycle_state
                ));
                continue;
            }

            let owner_answer = fetch_assistant_answer(&owner.graphql, &child.request_id).await?;
            if owner_answer.trim().is_empty() {
                pending.push(format!(
                    "child {} on {} has no owner-side assistant answer yet",
                    child.request_id, owner.agent_did
                ));
                continue;
            }
            let coordinator_answer = fetch_assistant_answer(coord_graphql, &child.request_id).await?;
            if coordinator_answer.trim().is_empty() {
                pending.push(format!(
                    "child {} on {} has no coordinator-side replicated answer yet",
                    child.request_id, owner.agent_did
                ));
                continue;
            }

            completed_by_owner.insert(
                owner.agent_did.clone(),
                CompletedChild {
                    tool_call_id: bridge.tool_call_id.clone(),
                    child_request_id: child.request_id.clone(),
                    owner_agent_did: owner.agent_did.clone(),
                    owner_behavior_id: owner.behavior_id.clone(),
                    owner_answer,
                    coordinator_answer,
                },
            );
        }

        let expected = subagents
            .iter()
            .map(|node| node.agent_did.as_str())
            .collect::<HashSet<_>>();
        let seen = completed_by_owner
            .keys()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let missing = expected.difference(&seen).copied().collect::<Vec<_>>();
        anyhow::ensure!(
            missing.is_empty(),
            "completed children missing subagent owners {missing:?}; pending: {pending:?}; completed owners: {:?}",
            completed_by_owner.keys().collect::<Vec<_>>()
        );

        Ok(completed_by_owner)
    })
    .await
}

fn assert_child_lineage(
    child: &ChildRow,
    owner: &FleetNode,
    bridge: &BridgeRow,
    parent_request_id: &str,
) -> Result<()> {
    assert_eq!(child.request_id, bridge.child_request_id);
    assert_eq!(child.agent_did, owner.agent_did);
    assert_eq!(child.behavior_id, owner.behavior_id);
    assert_eq!(
        child.caused_by_parent_request_id.as_deref(),
        Some(parent_request_id)
    );
    assert_eq!(
        child.caused_by_parent_tool_call_id.as_deref(),
        Some(bridge.tool_call_id.as_str())
    );
    assert_eq!(child.caused_by_trigger_kind.as_deref(), Some("subagent"));
    anyhow::ensure!(
        !matches!(
            child.lifecycle_state.as_deref(),
            Some("failed" | "dead" | "interrupted" | "superseded")
        ),
        "child {} reached non-completed terminal state when observed: {child:?}",
        child.request_id
    );
    Ok(())
}

async fn fetch_spawn_bridges(graphql: &str, session_id: &str) -> Result<Vec<BridgeRow>> {
    let session_id = escape_graphql_string(session_id);
    let response = graphql_query(
        graphql,
        &format!(
            r#"{{
                AgentToolCall(
                    filter: {{
                        session_id: {{ _eq: "{session_id}" }},
                        tool_name: {{ _eq: "spawn_subagent" }}
                    }},
                    order: {{ started_at: ASC }}
                ) {{
                    tool_call_id
                    lifecycle_state
                    child_request_id
                    await_mode
                }}
            }}"#
        ),
    )
    .await?;
    let rows = response
        .pointer("/data/AgentToolCall")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let child_request_id = row
                .get("child_request_id")
                .and_then(Value::as_str)?
                .trim()
                .to_string();
            if child_request_id.is_empty() {
                return None;
            }
            Some(BridgeRow {
                tool_call_id: row
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                lifecycle_state: row
                    .get("lifecycle_state")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                child_request_id,
                await_mode: row
                    .get("await_mode")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            })
        })
        .collect())
}

async fn find_child_on_any_subagent<'a>(
    subagents: &'a [FleetNode],
    request_id: &str,
) -> Result<Option<(&'a FleetNode, ChildRow)>> {
    for subagent in subagents {
        if let Some(row) = fetch_child_request(&subagent.graphql, request_id).await? {
            return Ok(Some((subagent, row)));
        }
    }
    Ok(None)
}

async fn fetch_child_request(graphql: &str, request_id: &str) -> Result<Option<ChildRow>> {
    let request_id = escape_graphql_string(request_id);
    let response = graphql_query(
        graphql,
        &format!(
            r#"{{
                AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{
                    request_id
                    agent_did
                    behavior_id
                    lifecycle_state
                    caused_by_parent_request_id
                    caused_by_parent_tool_call_id
                    caused_by_trigger_kind
                }}
            }}"#
        ),
    )
    .await?;
    Ok(response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .map(|row| ChildRow {
            request_id: row
                .get("request_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            agent_did: row
                .get("agent_did")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            behavior_id: row
                .get("behavior_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            lifecycle_state: row
                .get("lifecycle_state")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            caused_by_parent_request_id: row
                .get("caused_by_parent_request_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            caused_by_parent_tool_call_id: row
                .get("caused_by_parent_tool_call_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            caused_by_trigger_kind: row
                .get("caused_by_trigger_kind")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        }))
}

async fn wait_for_request_terminal(
    graphql: &str,
    request_id: &str,
    timeout: Duration,
) -> Result<String> {
    wait_until_value(timeout, || async {
        let state = fetch_request_lifecycle(graphql, request_id)
            .await?
            .with_context(|| format!("AgentRequest({request_id}) not found"))?;
        anyhow::ensure!(
            is_terminal(&state),
            "AgentRequest({request_id}) not terminal yet: {state}"
        );
        Ok(state)
    })
    .await
}

async fn fetch_request_lifecycle(graphql: &str, request_id: &str) -> Result<Option<String>> {
    let request_id = escape_graphql_string(request_id);
    let response = graphql_query(
        graphql,
        &format!(
            r#"{{
                AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{
                    lifecycle_state
                }}
            }}"#
        ),
    )
    .await?;
    Ok(response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("lifecycle_state"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned))
}

async fn wait_for_assistant_answer(
    graphql: &str,
    request_id: &str,
    timeout: Duration,
) -> Result<String> {
    wait_until_value(timeout, || async {
        let answer = fetch_assistant_answer(graphql, request_id).await?;
        anyhow::ensure!(
            !answer.trim().is_empty(),
            "AgentResponse({request_id}) is empty"
        );
        Ok(answer)
    })
    .await
}

async fn fetch_assistant_answer(graphql: &str, request_id: &str) -> Result<String> {
    let escaped = escape_graphql_string(request_id);
    let response = graphql_query(
        graphql,
        &format!(
            r#"{{
                AgentResponse(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{
                    content
                    session_id
                }}
            }}"#
        ),
    )
    .await?;
    if let Some(row) = response
        .pointer("/data/AgentResponse")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
    {
        if let Some(content) = row
            .get("content")
            .and_then(Value::as_str)
            .filter(|content| !content.trim().is_empty())
        {
            return Ok(content.to_string());
        }
        if let Some(session_id) = row.get("session_id").and_then(Value::as_str) {
            return fetch_latest_assistant_message(graphql, session_id).await;
        }
    }
    Ok(String::new())
}

async fn fetch_latest_assistant_message(graphql: &str, session_id: &str) -> Result<String> {
    let session_id = escape_graphql_string(session_id);
    let response = graphql_query(
        graphql,
        &format!(
            r#"{{
                AgentMessage(
                    filter: {{ session_id: {{ _eq: "{session_id}" }}, role: {{ _eq: "assistant" }} }},
                    order: {{ sequence: DESC }},
                    limit: 1
                ) {{ content }}
            }}"#
        ),
    )
    .await?;
    Ok(response
        .pointer("/data/AgentMessage")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("content"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string())
}

async fn assert_subagent_store_scopes(
    coord: &FleetNode,
    subagents: &[FleetNode],
    completed_children: &HashMap<String, CompletedChild>,
) -> Result<()> {
    for subagent in subagents {
        let completed = completed_children
            .get(&subagent.agent_did)
            .with_context(|| format!("missing completed child for {}", subagent.agent_did))?;
        assert_eq!(completed.owner_agent_did, subagent.agent_did);
        assert_eq!(completed.owner_behavior_id, subagent.behavior_id);
        anyhow::ensure!(
            !completed.owner_answer.trim().is_empty()
                && !completed.coordinator_answer.trim().is_empty(),
            "completed child {} has empty answer(s): {completed:?}",
            completed.child_request_id
        );
        let local_child = fetch_child_request(&subagent.graphql, &completed.child_request_id)
            .await?
            .with_context(|| {
                format!(
                    "subagent {} missing its completed child {} from bridge {}",
                    subagent.agent_did, completed.child_request_id, completed.tool_call_id
                )
            })?;
        assert_eq!(local_child.agent_did, subagent.agent_did);
        assert_eq!(local_child.behavior_id, subagent.behavior_id);
        assert_eq!(local_child.lifecycle_state.as_deref(), Some("completed"));

        let allowed = HashSet::from([coord.agent_did.as_str(), subagent.agent_did.as_str()]);
        for collection in CONVERSATION_COLLECTIONS {
            let agent_dids = fetch_collection_agent_dids(&subagent.graphql, collection).await?;
            let unexpected = agent_dids
                .iter()
                .filter(|did| {
                    let did = did.trim();
                    did.is_empty() || !allowed.contains(did)
                })
                .cloned()
                .collect::<Vec<_>>();
            anyhow::ensure!(
                unexpected.is_empty(),
                "subagent {} store leaked unexpected agent_did values in {collection}: {:?}; allowed: {:?}",
                subagent.agent_did,
                unexpected,
                allowed
            );
        }
    }
    Ok(())
}

async fn fetch_collection_agent_dids(graphql: &str, collection: &str) -> Result<Vec<String>> {
    let response = graphql_query(graphql, &format!("{{ {collection} {{ agent_did }} }}")).await?;
    Ok(response
        .pointer(&format!("/data/{collection}"))
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    row.get("agent_did")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default())
}

async fn assert_subagents_have_no_spawn_targets(subagents: &[FleetNode]) -> Result<()> {
    for subagent in subagents {
        let selection_id = escape_graphql_string(&subagent.tool_selection_id);
        let response = graphql_query(
            &subagent.graphql,
            &format!(
                r#"{{
                    ToolSelection(filter: {{ selection_id: {{ _eq: "{selection_id}" }} }}, limit: 1) {{
                        subagent_targets
                        subagent_spawn_enabled
                        subagent_allow_cross_deployment
                    }}
                }}"#
            ),
        )
        .await?;
        let row = first_graphql_row(&response, "ToolSelection")?;
        let target_count = row
            .get("subagent_targets")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or_default();
        anyhow::ensure!(
            target_count == 0,
            "subagent {} should not have onward targets: {row}",
            subagent.agent_did
        );
        anyhow::ensure!(
            row.get("subagent_spawn_enabled").and_then(Value::as_bool) == Some(false),
            "subagent {} should have spawn disabled: {row}",
            subagent.agent_did
        );
    }
    Ok(())
}

fn is_terminal(state: &str) -> bool {
    matches!(
        state,
        "completed" | "failed" | "dead" | "interrupted" | "superseded"
    )
}

async fn assert_endpoint_reachable(endpoint: &str) -> Result<()> {
    let url = format!("{}/models", endpoint.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("building live endpoint probe client")?;
    let response = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("probing live endpoint {url}"))?;
    anyhow::ensure!(
        response.status().is_success(),
        "live endpoint {url} returned {}",
        response.status()
    );
    Ok(())
}

async fn wait_until_value<T, F, Fut>(timeout: Duration, mut f: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let deadline = Instant::now() + timeout;
    let mut last_error = "condition not evaluated".to_string();
    loop {
        if Instant::now() >= deadline {
            bail!("timed out after {:?}: {last_error}", timeout);
        }
        match f().await {
            Ok(value) => return Ok(value),
            Err(error) => last_error = error.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
