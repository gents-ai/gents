//! Five-process fleet e2e for network membership, data-plane pairing, and
//! cross-deployment subagent delegation.
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
use chrono::SecondsFormat;
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
    ("DEFRA_AGENT_REGISTRY_STALE_MS", "5000"),
    ("DEFRA_AGENT_ENDPOINT_HEARTBEAT_MS", "1000"),
];

const NETWORK_CONTROL_TEMPLATE: &str = "network-control";

// Cross-deployment subagent delegation needs both the target-owned child
// AgentRequest and the parent-owned bridge AgentToolCall to reach the target
// node. The existing "conversation" template is peer-DID scoped and would carry
// the child request but not the parent bridge, so this capstone uses the
// unscoped conversation collection set for the delegation data plane. The
// membership gate is still enforced by DataPlanePairingDesired materialization.
const DATA_PLANE_TEMPLATE: &str = "backup";

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

const COORDINATOR_SYSTEM_PROMPT: &str = r#"You are a fleet coordinator. You have four remote research subagents named researcher-1, researcher-2, researcher-3, and researcher-4. For any user request asking you to use the fleet, you must call the spawn_subagent tool at least twice, using two different researcher names, and each call must set await_mode to "background". Do not use foreground. After the background subagents complete, summarize their results briefly."#;

const SUBAGENT_SYSTEM_PROMPT: &str = r#"You are a remote research subagent. Answer the assigned question directly in one short paragraph. Do not delegate to other subagents."#;

struct FleetNode {
    home: PathBuf,
    graphql: String,
    agent_did: String,
    peer_id: String,
    address: String,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "live: set DEFRA_AGENT_LIVE_OPENAI=1 and pass --ignored"]
async fn five_process_fleet_discovery_join_pairing_delegation() -> Result<()> {
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

    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let fleet = bring_up_fleet(tempdir.path(), 5, &endpoint, &model).await?;
    let (coord, subagents) = fleet
        .split_first()
        .ok_or_else(|| anyhow!("fleet should contain a coordinator"))?;

    network_create(coord, "Fleet Delegation E2E")?;
    for subagent in subagents {
        network_grant(coord, &subagent.agent_did)?;
        let token = mint_network_control_invite(coord, &subagent.agent_did)?;
        join_network_control(subagent, &token)?;
        wait_for_pairing_applied(&subagent.graphql, &coord.peer_id, Duration::from_secs(120))
            .await?;
    }

    wait_for_network_convergence(&fleet, Duration::from_secs(180)).await?;

    for subagent in subagents {
        upsert_data_plane_pairing(
            &coord.graphql,
            &subagent.peer_id,
            &subagent.agent_did,
            &subagent.address,
        )
        .await?;
        upsert_data_plane_pairing(
            &subagent.graphql,
            &coord.peer_id,
            &coord.agent_did,
            &coord.address,
        )
        .await?;
    }

    for subagent in subagents {
        wait_for_applied_collection(
            &coord.graphql,
            &subagent.peer_id,
            "AgentRequest",
            Duration::from_secs(120),
        )
        .await?;
        wait_for_applied_collection(
            &subagent.graphql,
            &coord.peer_id,
            "AgentRequest",
            Duration::from_secs(120),
        )
        .await?;
    }
    assert_no_subagent_data_plane_edges(subagents).await?;

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

    let parent_prompt = "Use at least two different research subagents in parallel. Ask one for one concise fact about Mercury and another for one concise fact about Venus. Use background spawns only, then summarize the two results.";
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

    let bridges = wait_for_spawn_bridges(
        &coord.graphql,
        &parent_session_id,
        2,
        Duration::from_secs(180),
    )
    .await?;
    let mut child_owners = HashMap::new();
    for bridge in bridges.iter().take(2) {
        anyhow::ensure!(
            bridge.await_mode.as_deref() == Some("background"),
            "bridge {} must use background mode: {bridge:?}",
            bridge.tool_call_id
        );
        anyhow::ensure!(
            bridge.lifecycle_state != "failed",
            "bridge {} failed before child materialization: {bridge:?}",
            bridge.tool_call_id
        );
        let (owner, child) = wait_for_child_on_any_subagent(
            subagents,
            &bridge.child_request_id,
            Duration::from_secs(180),
        )
        .await?
        .with_context(|| {
            format!(
                "child request {} from bridge {} did not materialize on any subagent",
                bridge.child_request_id, bridge.tool_call_id
            )
        })?;
        assert_eq!(child.request_id, bridge.child_request_id);
        assert_eq!(child.agent_did, owner.agent_did);
        assert_eq!(child.behavior_id, owner.behavior_id);
        assert_eq!(
            child.caused_by_parent_request_id.as_deref(),
            Some(parent_request_id.as_str())
        );
        assert_eq!(
            child.caused_by_parent_tool_call_id.as_deref(),
            Some(bridge.tool_call_id.as_str())
        );
        assert_eq!(child.caused_by_trigger_kind.as_deref(), Some("subagent"));
        anyhow::ensure!(
            child.lifecycle_state.as_deref() != Some("failed"),
            "child {} was already failed when observed: {child:?}",
            child.request_id
        );
        child_owners.insert(child.request_id.clone(), owner.agent_did.clone());

        let child_terminal =
            wait_for_request_terminal(&owner.graphql, &child.request_id, Duration::from_secs(240))
                .await?;
        assert_eq!(
            child_terminal, "completed",
            "child {} on {} must complete",
            child.request_id, owner.agent_did
        );
        let child_answer =
            wait_for_assistant_answer(&owner.graphql, &child.request_id, Duration::from_secs(60))
                .await?;
        anyhow::ensure!(
            !child_answer.trim().is_empty(),
            "child {} produced an empty response",
            child.request_id
        );
    }
    anyhow::ensure!(
        child_owners.values().collect::<HashSet<_>>().len() >= 2,
        "expected at least two distinct subagent owners, saw {child_owners:?}"
    );

    let parent_terminal =
        wait_for_request_terminal(&coord.graphql, &parent_request_id, Duration::from_secs(240))
            .await?;
    anyhow::ensure!(
        is_terminal(&parent_terminal),
        "parent request must terminalize, got {parent_terminal}"
    );

    assert_subagents_have_no_spawn_targets(subagents).await?;

    let revoked = &subagents[0];
    network_revoke(coord, &revoked.agent_did)?;
    wait_for_pairing_applied_gone(&coord.graphql, &revoked.peer_id, Duration::from_secs(120))
        .await?;

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

        nodes.push(FleetNode {
            home,
            graphql,
            agent_did,
            peer_id,
            address,
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

fn network_create(node: &FleetNode, name: &str) -> Result<String> {
    let out = run_cli_json(
        &node.home,
        &[
            "p2p", "network", "create", "--name", name, "--output", "json",
        ],
    )?;
    anyhow::ensure!(
        out.get("status").and_then(Value::as_str) == Some("network_created"),
        "network create output: {out}"
    );
    required_output_string(&out, "network_id")
}

fn network_grant(admin: &FleetNode, member_did: &str) -> Result<()> {
    let out = run_cli_json(
        &admin.home,
        &["p2p", "network", "grant", member_did, "--output", "json"],
    )?;
    anyhow::ensure!(
        out.get("status").and_then(Value::as_str) == Some("membership_granted"),
        "network grant output: {out}"
    );
    Ok(())
}

fn network_revoke(admin: &FleetNode, member_did: &str) -> Result<()> {
    let out = run_cli_json(
        &admin.home,
        &["p2p", "network", "revoke", member_did, "--output", "json"],
    )?;
    anyhow::ensure!(
        out.get("status").and_then(Value::as_str) == Some("membership_revoked"),
        "network revoke output: {out}"
    );
    Ok(())
}

fn mint_network_control_invite(admin: &FleetNode, member_did: &str) -> Result<String> {
    let invite = run_cli_json(
        &admin.home,
        &[
            "p2p",
            "pairings",
            "invite",
            "--template",
            NETWORK_CONTROL_TEMPLATE,
            "--member-did",
            member_did,
        ],
    )?;
    anyhow::ensure!(
        invite.get("status").and_then(Value::as_str) == Some("invite_created"),
        "invite output: {invite}"
    );
    required_output_string(&invite, "token")
}

fn join_network_control(node: &FleetNode, token: &str) -> Result<()> {
    let out = run_cli_json(
        &node.home,
        &[
            "p2p",
            "pairings",
            "join",
            token,
            "--wait",
            "--timeout",
            "120s",
        ],
    )?;
    anyhow::ensure!(
        matches!(
            out.get("status").and_then(Value::as_str),
            Some("pairing_joined" | "pairing_exists")
        ),
        "join output: {out}"
    );
    anyhow::ensure!(
        out.get("template").and_then(Value::as_str) == Some(NETWORK_CONTROL_TEMPLATE),
        "join must use network-control template: {out}"
    );
    Ok(())
}

fn required_output_string(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("output missing {key}: {value}"))
}

async fn wait_for_network_convergence(nodes: &[FleetNode], timeout: Duration) -> Result<()> {
    for node in nodes {
        wait_for_membership_count(&node.graphql, nodes.len(), timeout).await?;
        wait_for_endpoint_count(&node.graphql, nodes.len(), timeout).await?;
    }

    for node in nodes {
        for peer in nodes.iter().filter(|peer| peer.peer_id != node.peer_id) {
            wait_for_pairing_applied(&node.graphql, &peer.peer_id, timeout).await?;
        }
    }
    Ok(())
}

async fn wait_for_membership_count(
    graphql: &str,
    expected: usize,
    timeout: Duration,
) -> Result<()> {
    wait_until(timeout, || async {
        let response = graphql_query(
            graphql,
            r#"{ NetworkMembership(filter: { status: { _eq: "active" } }) { member_did } }"#,
        )
        .await?;
        let count = distinct_string_count(&response, "/data/NetworkMembership", "member_did");
        anyhow::ensure!(
            count >= expected,
            "saw {count} active memberships, expected {expected}: {response}"
        );
        Ok(())
    })
    .await
}

async fn wait_for_endpoint_count(graphql: &str, expected: usize, timeout: Duration) -> Result<()> {
    wait_until(timeout, || async {
        let response = graphql_query(graphql, r#"{ PeerEndpoint { did } }"#).await?;
        let count = distinct_string_count(&response, "/data/PeerEndpoint", "did");
        anyhow::ensure!(
            count >= expected,
            "saw {count} endpoints, expected {expected}: {response}"
        );
        Ok(())
    })
    .await
}

fn distinct_string_count(response: &Value, pointer: &str, field: &str) -> usize {
    response
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get(field).and_then(Value::as_str))
                .collect::<HashSet<_>>()
                .len()
        })
        .unwrap_or_default()
}

async fn upsert_data_plane_pairing(
    graphql: &str,
    peer_id: &str,
    agent_did: &str,
    address: &str,
) -> Result<()> {
    let peer_id = escape_graphql_string(peer_id);
    let agent_did = escape_graphql_string(agent_did);
    let address = escape_graphql_string(address);
    let template = escape_graphql_string(DATA_PLANE_TEMPLATE);
    let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true));
    let collections = graphql_string_array(CONVERSATION_COLLECTIONS);
    let mutation = format!(
        r#"mutation {{
            upsert_DataPlanePairingDesired(
                filter: {{ peer_id: {{ _eq: "{peer_id}" }} }},
                add: {{
                    peer_id: "{peer_id}",
                    agent_did: "{agent_did}",
                    collections: {collections},
                    replicator_addresses: ["{address}"],
                    template: "{template}",
                    created_at: "{now}",
                    updated_at: "{now}"
                }},
                update: {{
                    agent_did: "{agent_did}",
                    collections: {collections},
                    replicator_addresses: ["{address}"],
                    template: "{template}",
                    updated_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    );
    graphql_query(graphql, &mutation).await?;
    Ok(())
}

fn graphql_string_array(values: &[&str]) -> String {
    assert!(
        !values.is_empty(),
        "empty GraphQL lists must not be emitted"
    );
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

async fn wait_for_applied_collection(
    graphql: &str,
    peer_id: &str,
    collection: &str,
    timeout: Duration,
) -> Result<()> {
    wait_until(timeout, || async {
        let row = applied_row(graphql, peer_id).await?.with_context(|| {
            format!("PeerPairingApplied({peer_id}) missing while waiting for {collection}")
        })?;
        let has_collection = row
            .get("collections")
            .and_then(Value::as_array)
            .is_some_and(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|value| value == collection)
            });
        anyhow::ensure!(
            has_collection,
            "PeerPairingApplied({peer_id}) missing collection {collection}: {row}"
        );
        Ok(())
    })
    .await
}

async fn wait_for_pairing_applied_gone(
    graphql: &str,
    peer_id: &str,
    timeout: Duration,
) -> Result<()> {
    wait_until(timeout, || async {
        anyhow::ensure!(
            applied_row(graphql, peer_id).await?.is_none(),
            "PeerPairingApplied({peer_id}) still exists"
        );
        Ok(())
    })
    .await
}

async fn applied_row(graphql: &str, peer_id: &str) -> Result<Option<Value>> {
    let peer_id = escape_graphql_string(peer_id);
    let response = graphql_query(
        graphql,
        &format!(
            r#"{{
                PeerPairingApplied(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}, limit: 1) {{
                    peer_id
                    collections
                    replicator_addresses
                    replicator_filter
                }}
            }}"#
        ),
    )
    .await?;
    Ok(response
        .pointer("/data/PeerPairingApplied")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned())
}

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
                "subagent {} unexpectedly has subagent data-plane edge: {row}",
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

async fn wait_for_spawn_bridges(
    graphql: &str,
    session_id: &str,
    minimum: usize,
    timeout: Duration,
) -> Result<Vec<BridgeRow>> {
    wait_until_value(timeout, || async {
        let bridges = fetch_spawn_bridges(graphql, session_id).await?;
        anyhow::ensure!(
            bridges.len() >= minimum,
            "saw {} spawn bridges, expected {minimum}",
            bridges.len()
        );
        Ok(bridges)
    })
    .await
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

async fn wait_for_child_on_any_subagent<'a>(
    subagents: &'a [FleetNode],
    request_id: &str,
    timeout: Duration,
) -> Result<Option<(&'a FleetNode, ChildRow)>> {
    wait_until_value(timeout, || async {
        for subagent in subagents {
            if let Some(row) = fetch_child_request(&subagent.graphql, request_id).await? {
                return Ok(Some((subagent, row)));
            }
        }
        bail!("child {request_id} not visible on any subagent yet");
    })
    .await
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

async fn wait_until<F, Fut>(timeout: Duration, mut f: F) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let deadline = Instant::now() + timeout;
    let mut last_error = "condition not evaluated".to_string();
    loop {
        if Instant::now() >= deadline {
            bail!("timed out after {:?}: {last_error}", timeout);
        }
        match f().await {
            Ok(()) => return Ok(()),
            Err(error) => last_error = error.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
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
