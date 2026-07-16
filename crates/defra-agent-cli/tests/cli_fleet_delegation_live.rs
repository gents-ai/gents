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
//! delegation 5/5). Those fixes are now in the pinned defradb rev, so this runs
//! against the workspace pin directly. The convergence checkpoint still dumps
//! doc-state + full daemon logs on timeout (`dump_fleet_doc_state` /
//! `persist_fleet_logs`) for future triage.
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
use codex_app_server_protocol as codex;
use defra_agent::subagent_target_entry;
use futures_util::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

type FleetShimWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

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

const SUBAGENT_SYSTEM_PROMPT: &str = r#"You are a remote research subagent. Answer the assigned question directly in at least five factual paragraphs totaling roughly 500 words. Do not delegate to other subagents. The detail is intentional: this live test observes the response while it is streaming across deployments."#;

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
    codex_shim_port: Option<u16>,
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
    session_id: String,
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
    child_session_id: String,
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
    let fleet = bring_up_fleet(tempdir.path(), fleet_size, &endpoint, &model, true).await?;
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

    let parent_prompt = "Use all four research subagents in parallel with background spawns only. Ask researcher-1 for a detailed five-paragraph report about Mercury, researcher-2 for a detailed five-paragraph report about Venus, researcher-3 for a detailed five-paragraph report about Earth, and researcher-4 for a detailed five-paragraph report about Mars. Make exactly four spawn_subagent calls total, one per researcher, then stop and reply that all four background researchers were delegated.";
    let shim_port = coord
        .codex_shim_port
        .context("coordinator must expose the Codex shim")?;
    let mut parent_ws = fleet_connect_and_initialize_codex(shim_port).await?;
    let parent_session_id = fleet_start_codex_thread(&mut parent_ws, &coord.home).await?;
    let parent_request_id =
        fleet_start_codex_turn(&mut parent_ws, &parent_session_id, parent_prompt).await?;

    // Read the parent stream and independently navigate a child while it is
    // still producing real model output on a remote deployment. This is the
    // end-to-end fence between DEFRA's durable state machine and the native
    // Codex collaboration/thread protocol.
    let (parent_capture, live_child) = tokio::try_join!(
        fleet_capture_parent_turn(&mut parent_ws),
        fleet_observe_live_child(shim_port, &parent_session_id),
    )?;
    assert_eq!(parent_capture.turn.status, codex::TurnStatus::Completed);

    let completed_children = wait_for_all_subagent_children_completed(
        &coord.graphql,
        subagents,
        &parent_session_id,
        &parent_request_id,
        Duration::from_secs(300),
    )
    .await?;

    let expected_child_threads = completed_children
        .values()
        .map(|child| child.child_session_id.clone())
        .collect::<HashSet<_>>();
    anyhow::ensure!(
        expected_child_threads.contains(&live_child.thread_id),
        "live Codex child {} was not one of the runtime-spawned children: {expected_child_threads:?}",
        live_child.thread_id
    );
    anyhow::ensure!(
        !live_child.delta.trim().is_empty(),
        "loaded child {} emitted an empty live delta",
        live_child.thread_id
    );
    assert_fleet_parent_collab_projection(&parent_capture, &expected_child_threads)?;
    assert_fleet_completed_collab_history(shim_port, &parent_session_id, &expected_child_threads)
        .await?;
    assert_fleet_child_thread_is_read_only(shim_port, &live_child.thread_id).await?;

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

#[derive(Debug)]
struct FleetParentTurnCapture {
    turn: codex::Turn,
    collab_items: Vec<codex::ThreadItem>,
}

#[derive(Debug)]
struct FleetLiveChildObservation {
    thread_id: String,
    delta: String,
}

fn fleet_request_id(value: i64) -> codex::RequestId {
    codex::RequestId::Integer(value)
}

async fn fleet_connect_and_initialize_codex(port: u16) -> Result<FleetShimWebSocket> {
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{port}/"))
        .await
        .with_context(|| format!("connecting to fleet Codex shim on port {port}"))?;
    fleet_send_codex_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: fleet_request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "defra-agent-fleet-live-test".to_string(),
                    title: None,
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                capabilities: None,
            },
        },
    )
    .await?;
    let _: codex::InitializeResponse =
        fleet_read_typed_response(&mut ws, fleet_request_id(1)).await?;
    fleet_send_codex_notification(&mut ws, codex::ClientNotification::Initialized).await?;
    Ok(ws)
}

async fn fleet_start_codex_thread(ws: &mut FleetShimWebSocket, cwd: &Path) -> Result<String> {
    fleet_send_codex_request(
        ws,
        codex::ClientRequest::ThreadStart {
            request_id: fleet_request_id(2),
            params: codex::ThreadStartParams {
                cwd: Some(cwd.display().to_string()),
                ..Default::default()
            },
        },
    )
    .await?;
    let response: codex::ThreadStartResponse =
        fleet_read_typed_response(ws, fleet_request_id(2)).await?;
    Ok(response.thread.id)
}

async fn fleet_start_codex_turn(
    ws: &mut FleetShimWebSocket,
    thread_id: &str,
    prompt: &str,
) -> Result<String> {
    fleet_send_codex_request(
        ws,
        codex::ClientRequest::TurnStart {
            request_id: fleet_request_id(3),
            params: codex::TurnStartParams {
                thread_id: thread_id.to_string(),
                input: vec![codex::UserInput::Text {
                    text: prompt.to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        },
    )
    .await?;
    let response: codex::TurnStartResponse =
        fleet_read_typed_response(ws, fleet_request_id(3)).await?;
    Ok(response.turn.id)
}

async fn fleet_capture_parent_turn(ws: &mut FleetShimWebSocket) -> Result<FleetParentTurnCapture> {
    let mut collab_items = Vec::new();
    loop {
        match fleet_read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                match fleet_server_notification(notification)? {
                    codex::ServerNotification::ItemCompleted(completed)
                        if matches!(
                            completed.item,
                            codex::ThreadItem::CollabAgentToolCall { .. }
                        ) =>
                    {
                        collab_items.push(completed.item);
                    }
                    codex::ServerNotification::TurnCompleted(completed) => {
                        return Ok(FleetParentTurnCapture {
                            turn: completed.turn,
                            collab_items,
                        });
                    }
                    _ => {}
                }
            }
            codex::JSONRPCMessage::Error(error) => {
                bail!("fleet Codex shim emitted an error: {}", error.error.message);
            }
            codex::JSONRPCMessage::Request(request) => {
                bail!("fleet Codex shim sent an unexpected request: {request:?}");
            }
            codex::JSONRPCMessage::Response(_) => {}
        }
    }
}

async fn fleet_observe_live_child(
    shim_port: u16,
    parent_thread_id: &str,
) -> Result<FleetLiveChildObservation> {
    let mut list_ws = fleet_connect_and_initialize_codex(shim_port).await?;
    let mut seen = HashSet::new();
    let mut observing = HashSet::new();
    let mut retry_after = HashMap::<String, Instant>::new();
    let mut observers = tokio::task::JoinSet::new();
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut request_sequence = 100_i64;

    loop {
        while let Some(joined) = observers.try_join_next() {
            let (thread_id, result) = joined.context("fleet child observer task panicked")?;
            observing.remove(&thread_id);
            match result? {
                Some(observation) => {
                    observers.abort_all();
                    return Ok(observation);
                }
                None => {
                    retry_after.insert(thread_id, Instant::now() + Duration::from_millis(500));
                }
            }
        }

        if Instant::now() >= deadline {
            observers.abort_all();
            bail!(
                "no live delta was observed from {} navigable runtime-spawned Codex child threads",
                seen.len()
            );
        }

        let request_id = fleet_request_id(request_sequence);
        request_sequence += 1;
        fleet_send_codex_request(
            &mut list_ws,
            codex::ClientRequest::ThreadList {
                request_id: request_id.clone(),
                params: codex::ThreadListParams {
                    cursor: None,
                    limit: Some(200),
                    sort_key: None,
                    sort_direction: None,
                    model_providers: None,
                    source_kinds: Some(vec![codex::ThreadSourceKind::SubAgentThreadSpawn]),
                    archived: None,
                    cwd: None,
                    use_state_db_only: true,
                    search_term: None,
                },
            },
        )
        .await?;
        let response: codex::ThreadListResponse =
            fleet_read_typed_response(&mut list_ws, request_id).await?;
        for thread in response.data {
            seen.insert(thread.id.clone());
            if observing.contains(&thread.id)
                || retry_after
                    .get(&thread.id)
                    .is_some_and(|retry_at| *retry_at > Instant::now())
            {
                continue;
            }
            let thread_id = thread.id;
            observing.insert(thread_id.clone());
            retry_after.remove(&thread_id);
            let parent_thread_id = parent_thread_id.to_string();
            let observer_thread_id = thread_id.clone();
            observers.spawn(async move {
                let result =
                    fleet_observe_child_thread(shim_port, observer_thread_id, parent_thread_id)
                        .await;
                (thread_id, result)
            });
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn fleet_observe_child_thread(
    shim_port: u16,
    thread_id: String,
    parent_thread_id: String,
) -> Result<Option<FleetLiveChildObservation>> {
    let mut ws = fleet_connect_and_initialize_codex(shim_port).await?;
    fleet_send_codex_request(
        &mut ws,
        codex::ClientRequest::ThreadRead {
            request_id: fleet_request_id(10),
            params: codex::ThreadReadParams {
                thread_id: thread_id.clone(),
                include_turns: true,
            },
        },
    )
    .await?;
    let read: codex::ThreadReadResponse =
        fleet_read_typed_response(&mut ws, fleet_request_id(10)).await?;
    let read_json = serde_json::to_value(&read.thread)?;
    anyhow::ensure!(
        read_json
            .pointer("/source/subAgent/thread_spawn/parent_thread_id")
            .and_then(Value::as_str)
            == Some(parent_thread_id.as_str()),
        "child thread {thread_id} did not expose native Codex ancestry: {read_json}"
    );
    let Some(turn_id) = read
        .thread
        .turns
        .iter()
        .rev()
        .find(|turn| turn.status == codex::TurnStatus::InProgress)
        .map(|turn| turn.id.clone())
    else {
        return Ok(None);
    };

    fleet_send_codex_request(
        &mut ws,
        codex::ClientRequest::ThreadResume {
            request_id: fleet_request_id(11),
            params: codex::ThreadResumeParams {
                thread_id: thread_id.clone(),
                cwd: None,
                ..Default::default()
            },
        },
    )
    .await?;

    let observation = tokio::time::timeout(Duration::from_secs(60), async {
        let mut resumed = false;
        let mut terminal = false;
        let mut delta = None::<String>;
        loop {
            match fleet_read_jsonrpc(&mut ws).await? {
                codex::JSONRPCMessage::Response(response)
                    if response.id == fleet_request_id(11) =>
                {
                    let resume: codex::ThreadResumeResponse =
                        serde_json::from_value(response.result)
                            .context("decoding fleet child thread/resume response")?;
                    resumed = true;
                    terminal = !resume.thread.turns.iter().any(|turn| {
                        turn.id == turn_id && turn.status == codex::TurnStatus::InProgress
                    });
                }
                codex::JSONRPCMessage::Notification(notification) => {
                    match fleet_server_notification(notification)? {
                        codex::ServerNotification::AgentMessageDelta(update)
                            if update.thread_id == thread_id
                                && update.turn_id == turn_id
                                && !update.delta.is_empty() =>
                        {
                            delta = Some(update.delta);
                        }
                        codex::ServerNotification::TurnCompleted(completed)
                            if completed.thread_id == thread_id && completed.turn.id == turn_id =>
                        {
                            terminal = true;
                        }
                        _ => {}
                    }
                }
                codex::JSONRPCMessage::Error(error) => {
                    bail!(
                        "fleet child {thread_id} emitted an error while resuming: {}",
                        error.error.message
                    );
                }
                codex::JSONRPCMessage::Request(request) => {
                    bail!("fleet child {thread_id} sent an unexpected request: {request:?}");
                }
                codex::JSONRPCMessage::Response(response) => {
                    bail!(
                        "unexpected response while resuming fleet child {thread_id}: {response:?}"
                    );
                }
            }

            if resumed {
                if let Some(delta) = delta.take() {
                    return Ok(Some(FleetLiveChildObservation { thread_id, delta }));
                }
                if terminal {
                    return Ok(None);
                }
            }
        }
    })
    .await;

    match observation {
        Ok(result) => result,
        Err(_) => Ok(None),
    }
}

fn assert_fleet_parent_collab_projection(
    capture: &FleetParentTurnCapture,
    expected_child_threads: &HashSet<String>,
) -> Result<()> {
    let mut projected = HashSet::new();
    for item in &capture.collab_items {
        let codex::ThreadItem::CollabAgentToolCall {
            tool,
            status,
            receiver_thread_ids,
            model,
            agents_states,
            ..
        } = item
        else {
            continue;
        };
        if *tool != codex::CollabAgentTool::SpawnAgent {
            continue;
        }
        anyhow::ensure!(
            *status == codex::CollabAgentToolCallStatus::Completed,
            "spawn projection was not terminal-completed: {item:?}"
        );
        anyhow::ensure!(
            model
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty()),
            "spawn projection omitted the child model: {item:?}"
        );
        for thread_id in receiver_thread_ids {
            anyhow::ensure!(
                agents_states.contains_key(thread_id),
                "spawn projection omitted agentsStates for {thread_id}: {item:?}"
            );
            projected.insert(thread_id.clone());
        }
    }
    anyhow::ensure!(
        projected == *expected_child_threads,
        "native parent collab projection did not match real runtime children; projected={projected:?} expected={expected_child_threads:?} items={:?}",
        capture.collab_items
    );
    Ok(())
}

async fn assert_fleet_completed_collab_history(
    shim_port: u16,
    parent_thread_id: &str,
    expected_child_threads: &HashSet<String>,
) -> Result<()> {
    let mut ws = fleet_connect_and_initialize_codex(shim_port).await?;
    fleet_send_codex_request(
        &mut ws,
        codex::ClientRequest::ThreadRead {
            request_id: fleet_request_id(20),
            params: codex::ThreadReadParams {
                thread_id: parent_thread_id.to_string(),
                include_turns: true,
            },
        },
    )
    .await?;
    let read: codex::ThreadReadResponse =
        fleet_read_typed_response(&mut ws, fleet_request_id(20)).await?;
    let mut completed = HashSet::new();
    for item in read.thread.turns.iter().flat_map(|turn| &turn.items) {
        if let codex::ThreadItem::CollabAgentToolCall {
            tool: codex::CollabAgentTool::SpawnAgent,
            receiver_thread_ids,
            agents_states,
            ..
        } = item
        {
            for thread_id in receiver_thread_ids {
                if agents_states
                    .get(thread_id)
                    .is_some_and(|state| state.status == codex::CollabAgentStatus::Completed)
                {
                    completed.insert(thread_id.clone());
                }
            }
        }
    }
    anyhow::ensure!(
        completed == *expected_child_threads,
        "completed parent history did not refresh all native agentsStates; completed={completed:?} expected={expected_child_threads:?}"
    );
    Ok(())
}

async fn assert_fleet_child_thread_is_read_only(
    shim_port: u16,
    child_thread_id: &str,
) -> Result<()> {
    let mut ws = fleet_connect_and_initialize_codex(shim_port).await?;
    fleet_send_codex_request(
        &mut ws,
        codex::ClientRequest::TurnStart {
            request_id: fleet_request_id(30),
            params: codex::TurnStartParams {
                thread_id: child_thread_id.to_string(),
                input: vec![codex::UserInput::Text {
                    text: "this write must be rejected".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        },
    )
    .await?;
    loop {
        match fleet_read_jsonrpc(&mut ws).await? {
            codex::JSONRPCMessage::Error(error) if error.id == fleet_request_id(30) => {
                anyhow::ensure!(
                    error.error.message.contains("read-only"),
                    "unexpected child turn/start rejection: {}",
                    error.error.message
                );
                return Ok(());
            }
            codex::JSONRPCMessage::Response(response) if response.id == fleet_request_id(30) => {
                bail!("read-only fleet child accepted turn/start: {response:?}");
            }
            codex::JSONRPCMessage::Notification(_) => {}
            other => bail!("unexpected message awaiting child read-only rejection: {other:?}"),
        }
    }
}

async fn fleet_send_codex_request(
    ws: &mut FleetShimWebSocket,
    request: codex::ClientRequest,
) -> Result<()> {
    let request: codex::JSONRPCRequest = serde_json::from_value(serde_json::to_value(request)?)
        .context("building fleet Codex JSON-RPC request")?;
    fleet_write_jsonrpc(ws, codex::JSONRPCMessage::Request(request)).await
}

async fn fleet_send_codex_notification(
    ws: &mut FleetShimWebSocket,
    notification: codex::ClientNotification,
) -> Result<()> {
    let notification: codex::JSONRPCNotification =
        serde_json::from_value(serde_json::to_value(notification)?)
            .context("building fleet Codex JSON-RPC notification")?;
    fleet_write_jsonrpc(ws, codex::JSONRPCMessage::Notification(notification)).await
}

async fn fleet_write_jsonrpc(
    ws: &mut FleetShimWebSocket,
    message: codex::JSONRPCMessage,
) -> Result<()> {
    let text = serde_json::to_string(&message).context("encoding fleet Codex JSON-RPC")?;
    ws.send(WsMessage::Text(text.into()))
        .await
        .context("sending fleet Codex JSON-RPC websocket frame")
}

async fn fleet_read_typed_response<T>(
    ws: &mut FleetShimWebSocket,
    expected_id: codex::RequestId,
) -> Result<T>
where
    T: DeserializeOwned,
{
    loop {
        match fleet_read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Response(response) if response.id == expected_id => {
                return serde_json::from_value(response.result)
                    .context("decoding fleet Codex response");
            }
            codex::JSONRPCMessage::Error(error) if error.id == expected_id => {
                bail!(
                    "fleet Codex shim returned an error for {expected_id}: {}",
                    error.error.message
                );
            }
            codex::JSONRPCMessage::Notification(_) => {}
            other => {
                bail!("unexpected fleet Codex message while waiting for {expected_id}: {other:?}")
            }
        }
    }
}

async fn fleet_read_jsonrpc(ws: &mut FleetShimWebSocket) -> Result<codex::JSONRPCMessage> {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(90), ws.next())
            .await
            .context("timed out waiting for fleet Codex shim websocket message")?
            .ok_or_else(|| anyhow!("fleet Codex shim websocket closed"))?
            .context("reading fleet Codex shim websocket frame")?;
        let text = match frame {
            WsMessage::Text(text) => text,
            WsMessage::Binary(bytes) => String::from_utf8(bytes.to_vec())
                .context("decoding fleet Codex binary websocket payload")?
                .into(),
            WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
            WsMessage::Close(close) => bail!("fleet Codex shim websocket closed: {close:?}"),
            WsMessage::Frame(_) => bail!("unexpected raw fleet Codex websocket frame"),
        };
        return serde_json::from_str(&text)
            .with_context(|| format!("decoding fleet Codex JSON-RPC message: {text}"));
    }
}

fn fleet_server_notification(
    notification: codex::JSONRPCNotification,
) -> Result<codex::ServerNotification> {
    serde_json::from_value(serde_json::to_value(notification)?)
        .context("decoding fleet Codex server notification")
}

// ===========================================================================
// Cut 5 — workflow orchestration over the fleet (#378 capstone)
// ===========================================================================
//
// The coordinator is a workflow ORCHESTRATOR: it issues one
// `fan_out_and_synthesize` call that fans out across the four remote researcher
// deployments (cross-deployment subagents) and then runs a LOCAL synthesizer
// (cut 1 requires synthesis to be local). The barrier is asserted from the
// coordinator's durable `AgentToolCall` rows as a convergence projection — the
// four fan-out bridges reach terminal only as the remote children's terminal
// states replicate back, and the synthesis bridge exists only after all four.
//
// SCOPE — this is the all-SUCCESS happy-path capstone. GREEN means: 4 distinct
// remote researchers COMPLETED, their reports replicated to the coordinator AND
// reached the synthesizer's input (data-flow fence), synthesis COMPLETED locally,
// and the orchestrator returned a grounded answer. It deliberately does NOT
// cover (these are separate concerns, not regressions):
//   - D10 partial-FAILURE at the fleet level (synthesis over a dead researcher) —
//     proven in Lean/conformance + the cut-1 hermetic tests, not live here;
//   - parent-reclaim idempotency, cross-node cancel/cascade, the deadline-edge
//     final-poll path — exercised by no live test yet.
// A PASS also depends on the configured LLM emitting one fan_out_and_synthesize
// call with four distinct-target tasks + substantive answers; off-shape output
// fails FAST (not a 360s hang) but a model swap can require prompt re-tuning.
// Validated against DeepSeek-V4-Flash (DEFRA_AGENT_LIVE_OPENAI_MODEL=d4f).

const WORKFLOW_COORDINATOR_PROMPT: &str = r#"You are a fleet workflow orchestrator. You have a workflow tool named `fan_out_and_synthesize` and five subagent targets: four remote researchers (researcher-1, researcher-2, researcher-3, researcher-4) and one local target named `synthesizer`. For any fleet request you MUST make exactly one call to `fan_out_and_synthesize` and call no other tool and do not answer directly. Set the top-level `target` to "researcher-1". Provide exactly four tasks and set each task's `target` to researcher-1, researcher-2, researcher-3, and researcher-4 respectively (one task per researcher). Set `synthesis_target` to "synthesizer". Set `synthesis_prompt` to an instruction asking the synthesizer to combine the four researchers' findings into one short paragraph."#;

const WORKFLOW_RESEARCHER_PROMPT: &str = r#"You are a remote research subagent. Answer the assigned question directly in one short factual paragraph. Do not delegate to other subagents."#;

const WORKFLOW_SYNTHESIZER_PROMPT: &str = r#"You are a synthesizer. You are given JSON outcomes from several researchers. Read all of them and write one concise combined paragraph that references each researcher's finding."#;

const FLEET_SYNTHESIZER_BEHAVIOR_ID: &str = "fleet-synthesizer";
const FLEET_SYNTHESIZER_TARGET_NAME: &str = "synthesizer";
/// A separate, deliberately MINIMAL tool selection for the synthesizer — no
/// orchestration, no spawn, no targets — so the synthesis child is a leaf and
/// cannot recurse into fan_out_and_synthesize or spawn_subagent.
const FLEET_SYNTHESIZER_SELECTION_ID: &str = "fleet-synthesizer-tools";
/// A behavior id that exists on NO node — used to fault-inject one researcher.
const FLEET_MISSING_BEHAVIOR_ID: &str = "fleet-missing-behavior";
/// A model name that no backend serves — makes a CLAIMED child fail at inference
/// (the materialized-then-failed D10 path, distinct from the unclaimed path).
const FLEET_BAD_MODEL: &str = "nonexistent-model-d10-fault";

/// Which fault (if any) to inject into the fleet workflow for a D10 test.
#[derive(Clone, Copy, PartialEq)]
enum WorkflowFault {
    /// All researchers healthy (happy-path capstone).
    Healthy,
    /// The last researcher's target names a behavior that exists on no node, so
    /// its child is never CLAIMED and dies at the spawn timeout (unclaimed path).
    UnclaimedTarget,
    /// The last researcher's behavior exists (its child is CLAIMED/materialized)
    /// but uses a bad model, so it FAILS at inference (materialized-failure path).
    MaterializedFailure,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct WorkflowBridgeRow {
    tool_call_id: String,
    lifecycle_state: Option<String>,
    workflow_role: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
    child_request_id: Option<String>,
}

const WORKFLOW_TERMINAL_STATES: &[&str] = &["completed", "failed", "timedOut", "cancelled"];

fn is_workflow_terminal(state: Option<&str>) -> bool {
    state.is_some_and(|s| WORKFLOW_TERMINAL_STATES.contains(&s))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "live: set DEFRA_AGENT_LIVE_OPENAI=1 and pass --ignored"]
async fn five_process_workflow_orchestration_live() -> Result<()> {
    if std::env::var("DEFRA_AGENT_LIVE_OPENAI").as_deref() != Ok("1") {
        tracing::info!("DEFRA_AGENT_LIVE_OPENAI != 1; skipping fleet workflow e2e");
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
    let fleet = bring_up_fleet(tempdir.path(), 5, &endpoint, &model, false).await?;
    let (coord, subagents) = fleet
        .split_first()
        .ok_or_else(|| anyhow!("fleet should contain a coordinator"))?;

    establish_reconciler_pairing(coord, subagents).await?;
    if let Err(error) = wait_for_fleet_pairing(coord, subagents).await {
        dump_fleet_doc_state(&fleet).await;
        persist_fleet_logs(&fleet, "wf-fail");
        dump_fleet_logs(&fleet);
        return Err(error);
    }
    // No-crosswise isolation: researchers pair with the coordinator, never with
    // each other (same property the delegation test asserts).
    assert_no_subagent_data_plane_edges(subagents).await?;

    configure_fleet_workflow_behaviors(tempdir.path(), coord, subagents, WorkflowFault::Healthy)
        .await?;
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

    let parent_prompt = "Use the research fleet via workflow orchestration: ask researcher-1 for one fact about Mercury, researcher-2 about Venus, researcher-3 about Earth, and researcher-4 about Mars, then synthesize a one-paragraph summary comparing the four planets.";
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

    // Convergence projection on the coordinator's authoritative view: the four
    // fan-out bridges (remote children) reach terminal as their states replicate
    // back, and exactly one synthesis bridge appears, terminal, only after them.
    let group = match wait_for_workflow_group_converged(
        &coord.graphql,
        &parent_session_id,
        // Tight budget: the happy path converges in ~50-90s; 180s is generous slack
        // while still surfacing a genuine hang quickly.
        Duration::from_secs(180),
    )
    .await
    {
        Ok(group) => group,
        Err(error) => {
            dump_fleet_doc_state(&fleet).await;
            persist_fleet_logs(&fleet, "wf-fail");
            dump_fleet_logs(&fleet);
            return Err(error);
        }
    };

    // Coordinator-side ordering sanity (parsed timestamps).
    assert_workflow_barrier(&group)?;
    // Remote round-trip: all four researchers COMPLETED on distinct remote
    // deployments and their answers replicated back to the coordinator.
    let reports =
        assert_fan_out_completed_on_remote_deployments(&coord.graphql, &group, subagents).await?;
    // Data flow: the four replicated reports actually reached the synthesizer.
    assert_synthesis_consumed_reports(&coord.graphql, &group, &reports).await?;
    // Cut-1 invariant: synthesis ran locally on the coordinator.
    assert_synthesis_ran_locally(&coord.graphql, &group, &coord.agent_did).await?;
    // The synthesizer is a leaf: it did not recurse into spawn/orchestration.
    assert_synthesis_is_leaf(&coord.graphql, &group).await?;
    // No-crosswise held THROUGH the run: the fan-out did not induce a
    // researcher<->researcher data-plane edge (re-checked post-convergence, not
    // only at setup).
    assert_no_subagent_data_plane_edges(subagents).await?;

    let parent_terminal =
        wait_for_request_terminal(&coord.graphql, &parent_request_id, Duration::from_secs(120))
            .await?;
    assert_eq!(
        parent_terminal, "completed",
        "orchestrator request must complete after the synthesis returns"
    );
    let parent_answer =
        wait_for_assistant_answer(&coord.graphql, &parent_request_id, Duration::from_secs(60))
            .await?;
    anyhow::ensure!(
        !parent_answer.trim().is_empty(),
        "orchestrator must return a non-empty synthesized answer"
    );
    // Content-grounded: the synthesized answer must reference the fan-out subject
    // matter (the four planets), so a non-empty failure paragraph cannot pass.
    let lowered = parent_answer.to_lowercase();
    let planets = ["mercury", "venus", "earth", "mars"]
        .iter()
        .filter(|p| lowered.contains(*p))
        .count();
    anyhow::ensure!(
        planets >= 3,
        "synthesized answer must reference the researched planets (>=3/4); got {planets}: {parent_answer:?}"
    );

    drop(fleet);
    Ok(())
}

/// Configure the coordinator as a workflow orchestrator with a LOCAL synthesizer
/// behavior + four REMOTE researcher targets, and the researchers as report
/// writers. Registers the "allowed subagents" (subagent_targets) the workflow
/// references — the authorization layer that sits on top of P2P pairing.
async fn configure_fleet_workflow_behaviors(
    root: &Path,
    coord: &FleetNode,
    subagents: &[FleetNode],
    fault: WorkflowFault,
) -> Result<()> {
    let coord_prompt = root.join("wf-coordinator-system-prompt.txt");
    fs::write(&coord_prompt, WORKFLOW_COORDINATOR_PROMPT)?;
    configure_behavior_prompt(coord, &coord_prompt, "Fleet Workflow Coordinator")?;

    // Local synthesizer behavior on the coordinator's own DID/deployment, bound to
    // its OWN minimal (no-orchestration, no-spawn) tool selection so it is a leaf.
    let synth_prompt = root.join("wf-synthesizer-system-prompt.txt");
    fs::write(&synth_prompt, WORKFLOW_SYNTHESIZER_PROMPT)?;
    configure_synthesizer_tool_selection(coord)?;
    configure_synthesizer_behavior(coord, &synth_prompt)?;

    let sub_prompt = root.join("wf-researcher-system-prompt.txt");
    fs::write(&sub_prompt, WORKFLOW_RESEARCHER_PROMPT)?;
    let last = subagents.len().saturating_sub(1);
    for (index, subagent) in subagents.iter().enumerate() {
        let display = format!("Fleet Workflow Researcher {}", index + 1);
        // MaterializedFailure: give the LAST researcher a bad model so its child is
        // claimed/materialized but fails at inference.
        if fault == WorkflowFault::MaterializedFailure && index == last {
            configure_behavior_prompt_with_model(subagent, &sub_prompt, &display, FLEET_BAD_MODEL)?;
        } else {
            configure_behavior_prompt(subagent, &sub_prompt, &display)?;
        }
        configure_subagent_target_gate(subagent)?;
    }

    configure_workflow_coordinator_targets(coord, subagents, fault)?;
    Ok(())
}

/// A minimal leaf tool selection for the synthesizer: orchestration OFF, spawn
/// OFF, no targets, no meta-tools, no defra-query. The synthesis child therefore
/// cannot recurse.
fn configure_synthesizer_tool_selection(coord: &FleetNode) -> Result<()> {
    run_cli_json(
        &coord.home,
        &[
            "config",
            "tools",
            "set",
            "--graphql",
            &coord.graphql,
            "--agent-did",
            &coord.agent_did,
            "--selection-id",
            FLEET_SYNTHESIZER_SELECTION_ID,
            "--display-name",
            "Fleet Synthesizer Tools (leaf)",
            "--orchestration-enabled",
            "false",
            "--subagent-spawn-enabled",
            "false",
            "--subagent-background-enabled",
            "false",
            "--subagent-allow-cross-deployment",
            "false",
            "--enable-meta-tools",
            "false",
            "--enable-defra-query",
            "false",
        ],
    )?;
    Ok(())
}

/// Create a second behavior on the coordinator (the local synthesizer). It is a
/// subagent target of `(coord.agent_did, FLEET_SYNTHESIZER_BEHAVIOR_ID)`, so the
/// synthesis child materializes on the coordinator's own deployment.
fn configure_synthesizer_behavior(coord: &FleetNode, prompt_path: &Path) -> Result<()> {
    run_cli_json(
        &coord.home,
        &[
            "config",
            "behavior",
            "set",
            "--graphql",
            &coord.graphql,
            "--agent-did",
            &coord.agent_did,
            "--behavior-id",
            FLEET_SYNTHESIZER_BEHAVIOR_ID,
            "--display-name",
            "Fleet Synthesizer",
            "--system-prompt-file",
            prompt_path
                .to_str()
                .ok_or_else(|| anyhow!("synthesizer prompt path is not UTF-8"))?,
            "--backend-id",
            &coord.backend_id,
            "--model-name",
            &coord.model_name,
            "--tool-selection-id",
            FLEET_SYNTHESIZER_SELECTION_ID,
            "--inference-profile-id",
            &coord.inference_profile_id,
        ],
    )?;
    Ok(())
}

/// The coordinator's "allowed subagents" for the workflow: orchestration enabled
/// + the four remote researchers + the local synthesizer, plus the spawn/
/// background/cross-deployment gates the engine enforces.
fn configure_workflow_coordinator_targets(
    coord: &FleetNode,
    subagents: &[FleetNode],
    fault: WorkflowFault,
) -> Result<()> {
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
        "Fleet Workflow Coordinator Tools".to_string(),
        "--orchestration-enabled".to_string(),
        "true".to_string(),
        "--subagent-spawn-enabled".to_string(),
        "true".to_string(),
        "--subagent-background-enabled".to_string(),
        "true".to_string(),
        "--subagent-allow-cross-deployment".to_string(),
        "true".to_string(),
        "--cross-deployment-spawn-timeout-seconds".to_string(),
        // The spawn timeout is the CLAIM window for healthy children (they claim in
        // seconds), so all paths run it tight. Fault injections go tighter still so
        // an unclaimed child is declared dead promptly.
        if fault == WorkflowFault::Healthy {
            "60".to_string()
        } else {
            "30".to_string()
        },
        "--enable-meta-tools".to_string(),
        "false".to_string(),
        "--enable-defra-query".to_string(),
        "false".to_string(),
    ];
    let last = subagents.len().saturating_sub(1);
    for (index, subagent) in subagents.iter().enumerate() {
        // UnclaimedTarget fault: point the LAST researcher's target at a behavior
        // that exists on no node, so its child is never claimed. (MaterializedFailure
        // keeps the real behavior id — the failure happens at inference, not here.)
        let behavior_id = if fault == WorkflowFault::UnclaimedTarget && index == last {
            FLEET_MISSING_BEHAVIOR_ID
        } else {
            subagent.behavior_id.as_str()
        };
        args.push("--subagent-target".to_string());
        args.push(subagent_target_entry(
            &format!("researcher-{}", index + 1),
            &subagent.agent_did,
            behavior_id,
            Some(format!("Remote fleet researcher {}", index + 1)),
        ));
    }
    // The local synthesizer target lives on the coordinator's own DID.
    args.push("--subagent-target".to_string());
    args.push(subagent_target_entry(
        FLEET_SYNTHESIZER_TARGET_NAME,
        &coord.agent_did,
        FLEET_SYNTHESIZER_BEHAVIOR_ID,
        Some("Local fleet synthesizer".to_string()),
    ));
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    run_cli_json(&coord.home, &refs)?;
    Ok(())
}

struct WorkflowGroup {
    #[allow(dead_code)]
    group_id: String,
    fan_out: Vec<WorkflowBridgeRow>,
    synthesis: WorkflowBridgeRow,
}

/// Poll the coordinator's durable rows until the workflow group converges: one
/// `fan_out_and_synthesize` orchestration call, four terminal `fan_out_child`
/// bridges, and one terminal `synthesis` bridge.
async fn wait_for_workflow_group_converged(
    coord_graphql: &str,
    session_id: &str,
    timeout: Duration,
) -> Result<WorkflowGroup> {
    let deadline = Instant::now() + timeout;
    let mut last = String::new();
    loop {
        if let Some(group) = try_load_converged_group(coord_graphql, session_id, &mut last).await? {
            return Ok(group);
        }
        if Instant::now() >= deadline {
            bail!("workflow group did not converge on the coordinator within {timeout:?}; last state: {last}");
        }
        tokio::time::sleep(Duration::from_millis(750)).await;
    }
}

async fn try_load_converged_group(
    coord_graphql: &str,
    session_id: &str,
    last: &mut String,
) -> Result<Option<WorkflowGroup>> {
    let escaped_session = escape_graphql_string(session_id);
    // limit: 2 (not 1) so a SECOND orchestration call is visible — with limit: 1
    // the `<= 1` check below would be vacuously true.
    let orch_query = format!(
        r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{escaped_session}" }}, tool_name: {{ _eq: "fan_out_and_synthesize" }} }}, limit: 2) {{ tool_call_id lifecycle_state }} }}"#
    );
    let orch = graphql_query(coord_graphql, &orch_query).await?;
    let orch_rows = orch
        .pointer("/data/AgentToolCall")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // Exactly one orchestration call: a second would mean the model did not
    // follow the one-call contract and the group keying would be ambiguous.
    anyhow::ensure!(
        orch_rows.len() <= 1,
        "expected exactly one fan_out_and_synthesize call, saw {}",
        orch_rows.len()
    );
    let Some(orch_row) = orch_rows.first() else {
        *last = "no fan_out_and_synthesize tool call yet".to_string();
        return Ok(None);
    };
    let group_id = orch_row
        .get("tool_call_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let orch_state = orch_row
        .get("lifecycle_state")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let escaped_group = escape_graphql_string(&group_id);
    let bridges_query = format!(
        r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{escaped_session}" }}, workflow_group_id: {{ _eq: "{escaped_group}" }} }}, order: {{ started_at: ASC }}) {{ tool_call_id lifecycle_state workflow_role started_at completed_at child_request_id }} }}"#
    );
    let bridges_resp = graphql_query(coord_graphql, &bridges_query).await?;
    let bridges: Vec<WorkflowBridgeRow> = bridges_resp
        .pointer("/data/AgentToolCall")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|r| serde_json::from_value(r.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    let fan_out: Vec<WorkflowBridgeRow> = bridges
        .iter()
        .filter(|b| b.workflow_role.as_deref() == Some("fan_out_child"))
        .cloned()
        .collect();
    let synthesis: Vec<WorkflowBridgeRow> = bridges
        .iter()
        .filter(|b| b.workflow_role.as_deref() == Some("synthesis"))
        .cloned()
        .collect();
    let is_completed = |b: &WorkflowBridgeRow| b.lifecycle_state.as_deref() == Some("completed");
    let fan_out_completed = fan_out.iter().filter(|b| is_completed(b)).count();
    let synthesis_completed = synthesis.iter().filter(|b| is_completed(b)).count();
    *last = format!(
        "orch={orch_state:?}; fan_out={} (completed {}), synthesis={} (completed {})",
        fan_out.len(),
        fan_out_completed,
        synthesis.len(),
        synthesis_completed,
    );
    // Happy-path capstone: all four fan-out researchers AND the synthesizer must
    // genuinely COMPLETE (not merely reach a terminal state — a failed/dead child
    // is a real failure here, per D10 the engine would still synthesize, but the
    // capstone must prove the success round-trip).
    let all_completed = fan_out.len() == 4
        && fan_out_completed == 4
        && synthesis.len() == 1
        && synthesis_completed == 1;
    if all_completed {
        return Ok(Some(WorkflowGroup {
            group_id,
            fan_out,
            synthesis: synthesis.into_iter().next().expect("one synthesis"),
        }));
    }
    // Fail FAST rather than waiting out the deadline: once the orchestration tool
    // call is terminal the whole workflow has finished, so if it did not all
    // complete, a researcher or the synthesizer failed — surface it loudly.
    if is_workflow_terminal(orch_state.as_deref()) {
        bail!(
            "workflow finished but not all parts completed (happy-path capstone requires 4/4 \
             fan-out + synthesis all 'completed'): {last}"
        );
    }
    Ok(None)
}

/// Coordinator-side ordering check: the synthesis bridge's `started_at` is not
/// before the latest fan-out `completed_at`. NOTE: all three timestamps are the
/// coordinator's own wall clock, written by one sequential engine path, so this
/// is a monotonicity sanity guard — the real barrier proof is the Lean theorem +
/// the durable-row gate in cut 1; the multinode round-trip is proven by the
/// completion + data-flow fences below. Timestamps are PARSED (not string-
/// compared) so a Z-form vs +00:00-form write cannot cause a lexical false-fail,
/// and every fan-out bridge must carry a completed_at.
fn assert_workflow_barrier(group: &WorkflowGroup) -> Result<()> {
    let mut max_completed: Option<chrono::DateTime<chrono::FixedOffset>> = None;
    for bridge in &group.fan_out {
        let raw = bridge
            .completed_at
            .as_deref()
            .ok_or_else(|| anyhow!("fan-out bridge missing completed_at"))?;
        let parsed = chrono::DateTime::parse_from_rfc3339(raw)
            .with_context(|| format!("parsing fan-out completed_at {raw:?}"))?;
        max_completed = Some(max_completed.map_or(parsed, |m| m.max(parsed)));
    }
    let max_completed = max_completed.ok_or_else(|| anyhow!("group has no fan-out bridges"))?;
    let synth_raw = group
        .synthesis
        .started_at
        .as_deref()
        .ok_or_else(|| anyhow!("synthesis bridge missing started_at"))?;
    let synth_started = chrono::DateTime::parse_from_rfc3339(synth_raw)
        .with_context(|| format!("parsing synthesis started_at {synth_raw:?}"))?;
    anyhow::ensure!(
        synth_started >= max_completed,
        "barrier violated: synthesis started {synth_started} before the last fan-out completed {max_completed}"
    );
    Ok(())
}

/// Prove the remote round-trip: every fan-out child ran to COMPLETION on a
/// distinct remote deployment AND its answer replicated back to the coordinator.
/// Returns each child's replicated report so the data-flow fence can confirm it
/// reached the synthesizer.
async fn assert_fan_out_completed_on_remote_deployments(
    coord_graphql: &str,
    group: &WorkflowGroup,
    subagents: &[FleetNode],
) -> Result<Vec<String>> {
    let remote_dids: HashSet<&str> = subagents.iter().map(|s| s.agent_did.as_str()).collect();
    let mut seen = HashSet::new();
    let mut reports = Vec::new();
    for bridge in &group.fan_out {
        let child_request_id = bridge
            .child_request_id
            .as_deref()
            .ok_or_else(|| anyhow!("fan-out bridge missing child_request_id"))?;
        let escaped = escape_graphql_string(child_request_id);
        let query = format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{ agent_did lifecycle_state }} }}"#
        );
        let resp = graphql_query(coord_graphql, &query).await?;
        let req = resp.pointer("/data/AgentRequest/0").ok_or_else(|| {
            anyhow!("fan-out child {child_request_id} not visible on coordinator")
        })?;
        let did = req
            .get("agent_did")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("fan-out child {child_request_id} missing agent_did"))?
            .to_string();
        anyhow::ensure!(
            remote_dids.contains(did.as_str()),
            "fan-out child {child_request_id} ran on {did}, not a remote researcher deployment"
        );
        anyhow::ensure!(
            req.get("lifecycle_state").and_then(Value::as_str) == Some("completed"),
            "fan-out child {child_request_id} (on {did}) did not complete on the coordinator's replicated view"
        );
        // Prove it genuinely RAN on the owning remote node — query that node's OWN
        // db, not just the coordinator's replicated view.
        assert_child_ran_on_owning_node(subagents, child_request_id, &did, "completed").await?;
        // The remote child's ANSWER must replicate back (the round-trip the whole
        // feature depends on). A real replication race would surface here.
        let report =
            wait_for_assistant_answer(coord_graphql, child_request_id, Duration::from_secs(60))
                .await?;
        anyhow::ensure!(
            !report.trim().is_empty(),
            "fan-out child {child_request_id} (on {did}) produced no replicated answer on the coordinator"
        );
        seen.insert(did);
        reports.push(report);
    }
    anyhow::ensure!(
        seen.len() == subagents.len(),
        "fan-out should span all {} remote deployments, saw {}",
        subagents.len(),
        seen.len()
    );
    Ok(reports)
}

/// Data-flow fence: prove the four REMOTE researcher reports actually reached the
/// synthesizer. The synthesis bridge's `args` carry the prompt the runtime built
/// (synthesis_prompt + the JSON of every fan-out outcome). A distinctive
/// alphanumeric chunk of each report must appear in it (robust to JSON escaping).
async fn assert_synthesis_consumed_reports(
    coord_graphql: &str,
    group: &WorkflowGroup,
    reports: &[String],
) -> Result<()> {
    let escaped = escape_graphql_string(&group.synthesis.tool_call_id);
    let query = format!(
        r#"{{ AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{ args }} }}"#
    );
    let resp = graphql_query(coord_graphql, &query).await?;
    let args = resp
        .pointer("/data/AgentToolCall/0/args")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("synthesis bridge args not found"))?
        .to_string();
    let args_alnum = alnum_lower(&args);
    for (i, report) in reports.iter().enumerate() {
        // Both sides must be the SAME rendering: the engine embeds the child's
        // TEXT-ONLY render (render_assistant_message_text) in the synthesis args,
        // so reduce the replicated report to its text before comparing — otherwise
        // the raw message envelope/reasoning would spuriously fail the match.
        let report_text = alnum_lower(&message_answer_text(report));
        anyhow::ensure!(
            report_text.chars().count() >= 24,
            "researcher #{i} report too short to fence ({} alnum chars)",
            report_text.chars().count()
        );
        // Full-text containment (no fixed-offset slice): the entire rendered
        // report must appear in what the synthesizer was handed.
        anyhow::ensure!(
            args_alnum.contains(&report_text),
            "synthesis input did not contain researcher #{i}'s replicated report"
        );
    }
    Ok(())
}

/// Lowercase alphanumeric projection — erases JSON escaping and whitespace so two
/// renderings of the same text compare equal.
fn alnum_lower(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Extract the assistant answer TEXT from a persisted native message JSON
/// (`{"role":"assistant","content":[{"text":"..."}, {reasoning}]}`), matching the
/// engine's text-only render. Falls back to the raw string if not in that shape.
fn message_answer_text(raw: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return raw.to_string();
    };
    let texts: Vec<String> = value
        .get("content")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.get("text")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
                .collect()
        })
        .unwrap_or_default();
    if texts.is_empty() {
        raw.to_string()
    } else {
        texts.join("\n")
    }
}

/// The synthesis child must run LOCALLY on the coordinator (cut-1 invariant:
/// cross-deployment synthesis is rejected). Asserts its replicated AgentRequest
/// is owned by the coordinator's DID.
async fn assert_synthesis_ran_locally(
    coord_graphql: &str,
    group: &WorkflowGroup,
    coord_did: &str,
) -> Result<()> {
    let child_request_id = group
        .synthesis
        .child_request_id
        .as_deref()
        .ok_or_else(|| anyhow!("synthesis bridge missing child_request_id"))?;
    let escaped = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{ agent_did behavior_id }} }}"#
    );
    let resp = graphql_query(coord_graphql, &query).await?;
    let req = resp
        .pointer("/data/AgentRequest/0")
        .ok_or_else(|| anyhow!("synthesis child not visible on coordinator"))?;
    let did = req
        .get("agent_did")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("synthesis child missing agent_did"))?;
    anyhow::ensure!(
        did == coord_did,
        "synthesis ran on {did}, must be local on the coordinator {coord_did}"
    );
    // And under the dedicated synthesizer behavior — not the coordinator's own.
    anyhow::ensure!(
        req.get("behavior_id").and_then(Value::as_str) == Some(FLEET_SYNTHESIZER_BEHAVIOR_ID),
        "synthesis ran under the wrong behavior (expected {FLEET_SYNTHESIZER_BEHAVIOR_ID})"
    );
    Ok(())
}

/// Prove the synthesizer is a LEAF: no AgentRequest is caused by the synthesis
/// child, so it did not recurse into fan_out_and_synthesize / spawn_subagent
/// (the synthesizer is bound to a no-orchestration, no-spawn tool selection).
async fn assert_synthesis_is_leaf(coord_graphql: &str, group: &WorkflowGroup) -> Result<()> {
    let child_request_id = group
        .synthesis
        .child_request_id
        .as_deref()
        .ok_or_else(|| anyhow!("synthesis bridge missing child_request_id"))?;
    let escaped = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{ AgentRequest(filter: {{ caused_by_parent_request_id: {{ _eq: "{escaped}" }} }}) {{ request_id }} }}"#
    );
    let resp = graphql_query(coord_graphql, &query).await?;
    let spawned = resp
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .map(|rows| rows.len())
        .unwrap_or(0);
    anyhow::ensure!(
        spawned == 0,
        "synthesizer must be a leaf (no recursion), but it spawned {spawned} child request(s)"
    );
    Ok(())
}

/// Prove a child request genuinely RAN on its owning remote node — query that
/// node's OWN GraphQL (not the coordinator's replicated view) and assert the
/// request exists there with the expected DID and terminal state.
async fn assert_child_ran_on_owning_node(
    subagents: &[FleetNode],
    child_request_id: &str,
    expected_did: &str,
    expected_state: &str,
) -> Result<()> {
    let node = subagents
        .iter()
        .find(|s| s.agent_did == expected_did)
        .ok_or_else(|| anyhow!("no fleet node owns DID {expected_did}"))?;
    let escaped = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{ agent_did behavior_id lifecycle_state }} }}"#
    );
    let resp = graphql_query(&node.graphql, &query).await?;
    let req = resp.pointer("/data/AgentRequest/0").ok_or_else(|| {
        anyhow!("child {child_request_id} not present on its owning node {expected_did}'s OWN db — it did not run there")
    })?;
    anyhow::ensure!(
        req.get("agent_did").and_then(Value::as_str) == Some(expected_did),
        "child {child_request_id} on node {expected_did} has the wrong agent_did"
    );
    // Same DID is not enough — it must have run under the CONFIGURED behavior.
    anyhow::ensure!(
        req.get("behavior_id").and_then(Value::as_str) == Some(node.behavior_id.as_str()),
        "child {child_request_id} on node {expected_did} ran under the wrong behavior (expected {})",
        node.behavior_id
    );
    anyhow::ensure!(
        req.get("lifecycle_state").and_then(Value::as_str) == Some(expected_state),
        "child {child_request_id} on node {expected_did} is not {expected_state} locally"
    );
    Ok(())
}

/// Prove a child request was NOT materialized on ANY fleet node — pass the full
/// fleet (coordinator included) so the unclaimed path is proven everywhere, not
/// just on the remote researchers.
async fn assert_child_materialized_nowhere(
    nodes: &[FleetNode],
    child_request_id: &str,
) -> Result<()> {
    let escaped = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{ request_id }} }}"#
    );
    for node in nodes {
        let resp = graphql_query(&node.graphql, &query).await?;
        let present = resp
            .pointer("/data/AgentRequest/0")
            .and_then(|r| r.get("request_id"))
            .is_some();
        anyhow::ensure!(
            !present,
            "unclaimed child {child_request_id} was materialized on node {} — it should exist nowhere",
            node.agent_did
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Cut 5 — D10 partial-failure: the fleet workflow must tolerate one dead
// researcher (synthesize over the survivors, parent still completes).
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "live: set DEFRA_AGENT_LIVE_OPENAI=1 and pass --ignored"]
async fn five_process_workflow_d10_partial_failure_live() -> Result<()> {
    if std::env::var("DEFRA_AGENT_LIVE_OPENAI").as_deref() != Ok("1") {
        tracing::info!("DEFRA_AGENT_LIVE_OPENAI != 1; skipping fleet workflow D10 e2e");
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
    let fleet = bring_up_fleet(tempdir.path(), 5, &endpoint, &model, false).await?;
    let (coord, subagents) = fleet
        .split_first()
        .ok_or_else(|| anyhow!("fleet should contain a coordinator"))?;

    establish_reconciler_pairing(coord, subagents).await?;
    if let Err(error) = wait_for_fleet_pairing(coord, subagents).await {
        dump_fleet_doc_state(&fleet).await;
        persist_fleet_logs(&fleet, "wf-d10-fail");
        dump_fleet_logs(&fleet);
        return Err(error);
    }

    // UnclaimedTarget: researcher-4's target points at a behavior that exists on
    // no node, so its child is never claimed. The other three succeed.
    configure_fleet_workflow_behaviors(
        tempdir.path(),
        coord,
        subagents,
        WorkflowFault::UnclaimedTarget,
    )
    .await?;
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

    let parent_prompt = "Use the research fleet via workflow orchestration: ask researcher-1 for one fact about Mercury, researcher-2 about Venus, researcher-3 about Earth, and researcher-4 about Mars, then synthesize a one-paragraph summary of whatever findings came back.";
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

    // Wait until the workflow FINISHED (the orchestration tool call terminalized)
    // — for D10 we do NOT require all fan-out to complete.
    let group = match wait_for_workflow_finished(
        &coord.graphql,
        &parent_session_id,
        // Tight budget: the workflow finishes in ~75s; with the fast-dead fix the
        // broken bridge dies at the 30s spawn timeout, so 180s is generous slack.
        // (Pre-fix this hung to the ~1800s request deadline — caught here AND by
        // the explicit speed fence below.)
        Duration::from_secs(180),
    )
    .await
    {
        Ok(group) => group,
        Err(error) => {
            dump_fleet_doc_state(&fleet).await;
            persist_fleet_logs(&fleet, "wf-d10-fail");
            dump_fleet_logs(&fleet);
            return Err(error);
        }
    };

    // D10: exactly one fan-out researcher FAILED, the other three COMPLETED, and
    // synthesis still ran (over the partial set) and COMPLETED.
    let completed = group
        .fan_out
        .iter()
        .filter(|b| b.lifecycle_state.as_deref() == Some("completed"))
        .count();
    let failed = group.fan_out.len() - completed;
    anyhow::ensure!(
        group.fan_out.len() == 4 && completed == 3 && failed == 1,
        "D10 expected 3 completed + 1 failed fan-out bridge; got {completed} completed, {failed} non-completed of {}",
        group.fan_out.len()
    );
    anyhow::ensure!(
        group.synthesis.lifecycle_state.as_deref() == Some("completed"),
        "synthesis must complete over the partial-failure set"
    );

    // Speed fence (proves the ENGINE FIX, not just D10 synthesis): the dead bridge
    // was declared dead at the SPAWN TIMEOUT (~30s), not the parent request
    // deadline (~1800s). Without the fix the bridge lives ~1800s, blowing this
    // bound — a regression that loses the fast-dead path is caught directly here,
    // not merely as a slow convergence timeout.
    let dead = group
        .fan_out
        .iter()
        .find(|b| b.lifecycle_state.as_deref() != Some("completed"))
        .ok_or_else(|| anyhow!("D10 expected one non-completed fan-out bridge"))?;
    let dead_started = dead
        .started_at
        .as_deref()
        .ok_or_else(|| anyhow!("dead bridge missing started_at"))?;
    let dead_completed = dead
        .completed_at
        .as_deref()
        .ok_or_else(|| anyhow!("dead bridge missing completed_at"))?;
    let dead_lifetime = chrono::DateTime::parse_from_rfc3339(dead_completed)?
        - chrono::DateTime::parse_from_rfc3339(dead_started)?;
    anyhow::ensure!(
        dead_lifetime.num_seconds() < 90,
        "dead bridge must terminalize at the spawn timeout (fast), lived {}s — engine fix regressed?",
        dead_lifetime.num_seconds()
    );
    // Unclaimed path: the child was materialized on NO fleet node (coordinator
    // INCLUDED, not just the remote researchers) — proven against each node's OWN
    // db.
    let dead_crid = dead
        .child_request_id
        .as_deref()
        .ok_or_else(|| anyhow!("dead bridge missing child_request_id"))?;
    assert_child_materialized_nowhere(&fleet, dead_crid).await?;

    // The synthesizer's input must carry a STRUCTURED FAILURE record for the dead
    // researcher (D10) AND the three surviving reports (data flow over survivors).
    let synth_args = fetch_tool_call_args(&coord.graphql, &group.synthesis.tool_call_id).await?;
    // The outcomes are a nested JSON string inside `args`, so the inner quotes are
    // escaped — compare on the alnum projection (strips escaping/whitespace): a
    // healthy outcome folds to "oktrue", the dead one to "okfalse".
    let args_alnum = alnum_lower(&synth_args);
    let oktrue = args_alnum.matches("oktrue").count();
    let okfalse = args_alnum.matches("okfalse").count();
    anyhow::ensure!(
        oktrue == 3 && okfalse == 1,
        "synthesis input must carry exactly 3 healthy (oktrue) + 1 failed (okfalse) outcome; got {oktrue} oktrue / {okfalse} okfalse"
    );
    // The failure record must carry the UNCLAIMED-dead reason — this ties the dead
    // outcome to the injected fault and proves WHY the researcher failed reached
    // the synthesizer (not merely that one did).
    anyhow::ensure!(
        args_alnum.contains("notclaimedbeforespawntimeout"),
        "synthesis input must carry the unclaimed-dead failure reason: {synth_args}"
    );
    let mut survivor_reports = 0;
    for bridge in &group.fan_out {
        if bridge.lifecycle_state.as_deref() != Some("completed") {
            continue;
        }
        let crid = bridge
            .child_request_id
            .as_deref()
            .ok_or_else(|| anyhow!("completed fan-out bridge missing child_request_id"))?;
        let report = alnum_lower(&message_answer_text(
            &wait_for_assistant_answer(&coord.graphql, crid, Duration::from_secs(60)).await?,
        ));
        // Hard assert (not if-skip): a too-short survivor report is a fence
        // failure, and every completed survivor must be accounted for.
        anyhow::ensure!(
            report.chars().count() >= 24,
            "surviving researcher report too short to fence ({} alnum chars)",
            report.chars().count()
        );
        anyhow::ensure!(
            args_alnum.contains(&report),
            "surviving researcher report did not reach the synthesizer"
        );
        survivor_reports += 1;
    }
    anyhow::ensure!(
        survivor_reports == 3,
        "expected all 3 surviving reports in the synthesis input, fenced {survivor_reports}"
    );

    assert_synthesis_ran_locally(&coord.graphql, &group, &coord.agent_did).await?;
    assert_synthesis_is_leaf(&coord.graphql, &group).await?;

    // Despite the failure, the orchestrator request COMPLETES with a non-empty
    // answer (the user gets a result, not an error).
    let parent_terminal =
        wait_for_request_terminal(&coord.graphql, &parent_request_id, Duration::from_secs(120))
            .await?;
    assert_eq!(
        parent_terminal, "completed",
        "orchestrator must still complete despite one failed researcher (D10)"
    );
    let parent_answer =
        wait_for_assistant_answer(&coord.graphql, &parent_request_id, Duration::from_secs(60))
            .await?;
    // Grounded on SURVIVORS: the answer must reference the surviving planets
    // (researcher-4/Mars is the injected fault), so a non-empty failure paragraph
    // cannot pass and the parent provably synthesized over the partial results.
    let lowered = parent_answer.to_lowercase();
    let survivors_named = ["mercury", "venus", "earth"]
        .iter()
        .filter(|p| lowered.contains(*p))
        .count();
    anyhow::ensure!(
        survivors_named >= 2,
        "answer must reference the surviving planets (>=2 of mercury/venus/earth); got {survivors_named}: {parent_answer:?}"
    );

    drop(fleet);
    Ok(())
}

// ---------------------------------------------------------------------------
// Cut 5 — D10 MATERIALIZED failure: a researcher whose child IS claimed
// (behavior exists, deployment materializes it) but FAILS at inference. This
// exercises the OTHER engine failure path (project_child_terminal ->
// bridge_failure), distinct from the unclaimed-dead path above.
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "live: set DEFRA_AGENT_LIVE_OPENAI=1 and pass --ignored"]
async fn five_process_workflow_d10_materialized_failure_live() -> Result<()> {
    if std::env::var("DEFRA_AGENT_LIVE_OPENAI").as_deref() != Ok("1") {
        tracing::info!(
            "DEFRA_AGENT_LIVE_OPENAI != 1; skipping fleet workflow D10 materialized e2e"
        );
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
    let fleet = bring_up_fleet(tempdir.path(), 5, &endpoint, &model, false).await?;
    let (coord, subagents) = fleet
        .split_first()
        .ok_or_else(|| anyhow!("fleet should contain a coordinator"))?;

    establish_reconciler_pairing(coord, subagents).await?;
    if let Err(error) = wait_for_fleet_pairing(coord, subagents).await {
        dump_fleet_doc_state(&fleet).await;
        persist_fleet_logs(&fleet, "wf-d10-mat-fail");
        dump_fleet_logs(&fleet);
        return Err(error);
    }

    // MaterializedFailure: researcher-4's behavior EXISTS (its child is claimed)
    // but uses a bad model, so it fails at inference. The other three succeed.
    configure_fleet_workflow_behaviors(
        tempdir.path(),
        coord,
        subagents,
        WorkflowFault::MaterializedFailure,
    )
    .await?;
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

    let parent_prompt = "Use the research fleet via workflow orchestration: ask researcher-1 for one fact about Mercury, researcher-2 about Venus, researcher-3 about Earth, and researcher-4 about Mars, then synthesize a one-paragraph summary of whatever findings came back.";
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

    let group = match wait_for_workflow_finished(
        &coord.graphql,
        &parent_session_id,
        Duration::from_secs(180),
    )
    .await
    {
        Ok(group) => group,
        Err(error) => {
            dump_fleet_doc_state(&fleet).await;
            persist_fleet_logs(&fleet, "wf-d10-mat-fail");
            dump_fleet_logs(&fleet);
            return Err(error);
        }
    };

    // 3 completed + 1 failed fan-out, synthesis completed over the partial set.
    let completed = group
        .fan_out
        .iter()
        .filter(|b| b.lifecycle_state.as_deref() == Some("completed"))
        .count();
    anyhow::ensure!(
        group.fan_out.len() == 4 && completed == 3,
        "expected 3 completed + 1 failed fan-out bridge; got {completed} completed of {}",
        group.fan_out.len()
    );
    anyhow::ensure!(
        group.synthesis.lifecycle_state.as_deref() == Some("completed"),
        "synthesis must complete over the partial-failure set"
    );

    let failed_bridge = group
        .fan_out
        .iter()
        .find(|b| b.lifecycle_state.as_deref() != Some("completed"))
        .ok_or_else(|| anyhow!("expected one non-completed fan-out bridge"))?;

    // DISTINGUISHING ASSERTION (vs the unclaimed-dead path): the failed
    // researcher's child MATERIALIZED — its AgentRequest exists on the coordinator
    // with lifecycle_state "failed". In the unclaimed path no such request exists.
    let failed_crid = failed_bridge
        .child_request_id
        .as_deref()
        .ok_or_else(|| anyhow!("failed bridge missing child_request_id"))?;
    let escaped = escape_graphql_string(failed_crid);
    let child = graphql_query(
        &coord.graphql,
        &format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{ agent_did lifecycle_state }} }}"#
        ),
    )
    .await?;
    let child_req = child
        .pointer("/data/AgentRequest/0")
        .ok_or_else(|| anyhow!("failed child not visible on coordinator"))?;
    let child_state = child_req.get("lifecycle_state").and_then(Value::as_str);
    anyhow::ensure!(
        child_state == Some("failed"),
        "materialized-failure: the failed researcher's child must exist on the coordinator with lifecycle_state 'failed' (got {child_state:?}) — proving it was claimed then failed, not unclaimed"
    );
    // And it actually ran on its OWNING remote node (its own db), with "failed".
    let failed_did = child_req
        .get("agent_did")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("failed child missing agent_did"))?;
    assert_child_ran_on_owning_node(subagents, failed_crid, failed_did, "failed").await?;

    // Speed: the materialized child fails at inference promptly (not the request
    // deadline) — bounded well under it.
    if let (Some(started), Some(done)) = (
        failed_bridge.started_at.as_deref(),
        failed_bridge.completed_at.as_deref(),
    ) {
        let lifetime = chrono::DateTime::parse_from_rfc3339(done)?
            - chrono::DateTime::parse_from_rfc3339(started)?;
        anyhow::ensure!(
            lifetime.num_seconds() < 150,
            "failed bridge should terminalize promptly, lived {}s",
            lifetime.num_seconds()
        );
    }

    // Synthesis input: exactly 3 healthy + 1 failed outcome, the failure surfaced
    // as a "failed" status (not "dead"), and all 3 survivor reports present.
    let synth_args = fetch_tool_call_args(&coord.graphql, &group.synthesis.tool_call_id).await?;
    let args_alnum = alnum_lower(&synth_args);
    let oktrue = args_alnum.matches("oktrue").count();
    let okfalse = args_alnum.matches("okfalse").count();
    anyhow::ensure!(
        oktrue == 3 && okfalse == 1,
        "synthesis input must carry exactly 3 oktrue + 1 okfalse; got {oktrue} / {okfalse}"
    );
    anyhow::ensure!(
        args_alnum.contains("statusfailed"),
        "the failed researcher's outcome must carry status 'failed' (materialized path): {synth_args}"
    );
    let mut survivor_reports = 0;
    for bridge in &group.fan_out {
        if bridge.lifecycle_state.as_deref() != Some("completed") {
            continue;
        }
        let crid = bridge
            .child_request_id
            .as_deref()
            .ok_or_else(|| anyhow!("completed fan-out bridge missing child_request_id"))?;
        let report = alnum_lower(&message_answer_text(
            &wait_for_assistant_answer(&coord.graphql, crid, Duration::from_secs(60)).await?,
        ));
        anyhow::ensure!(
            report.chars().count() >= 24,
            "surviving researcher report too short to fence ({} alnum chars)",
            report.chars().count()
        );
        anyhow::ensure!(
            args_alnum.contains(&report),
            "surviving researcher report did not reach the synthesizer"
        );
        survivor_reports += 1;
    }
    anyhow::ensure!(
        survivor_reports == 3,
        "expected all 3 surviving reports in the synthesis input, fenced {survivor_reports}"
    );

    assert_synthesis_ran_locally(&coord.graphql, &group, &coord.agent_did).await?;
    assert_synthesis_is_leaf(&coord.graphql, &group).await?;

    let parent_terminal =
        wait_for_request_terminal(&coord.graphql, &parent_request_id, Duration::from_secs(120))
            .await?;
    assert_eq!(
        parent_terminal, "completed",
        "orchestrator must still complete despite one inference-failed researcher (D10)"
    );
    let parent_answer =
        wait_for_assistant_answer(&coord.graphql, &parent_request_id, Duration::from_secs(60))
            .await?;
    let lowered = parent_answer.to_lowercase();
    let survivors_named = ["mercury", "venus", "earth"]
        .iter()
        .filter(|p| lowered.contains(*p))
        .count();
    anyhow::ensure!(
        survivors_named >= 2,
        "answer must reference the surviving planets (>=2 of mercury/venus/earth); got {survivors_named}: {parent_answer:?}"
    );

    drop(fleet);
    Ok(())
}

// ---------------------------------------------------------------------------
// Cut 5 — local synthesizer deleted MID-RUN (between fan-out and synthesis):
// proves the spawn-time TOCTOU guard fires, no synthesis child is written, and
// the orchestration tool call fails with the correct `serviceUnavailable`
// failure class (not the generic `external`).
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "live: set DEFRA_AGENT_LIVE_OPENAI=1 and pass --ignored"]
async fn five_process_workflow_synthesizer_deleted_midrun_live() -> Result<()> {
    if std::env::var("DEFRA_AGENT_LIVE_OPENAI").as_deref() != Ok("1") {
        tracing::info!("DEFRA_AGENT_LIVE_OPENAI != 1; skipping fleet workflow mid-run-delete e2e");
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
    let fleet = bring_up_fleet(tempdir.path(), 5, &endpoint, &model, false).await?;
    let (coord, subagents) = fleet
        .split_first()
        .ok_or_else(|| anyhow!("fleet should contain a coordinator"))?;

    establish_reconciler_pairing(coord, subagents).await?;
    if let Err(error) = wait_for_fleet_pairing(coord, subagents).await {
        dump_fleet_doc_state(&fleet).await;
        persist_fleet_logs(&fleet, "wf-midrun-fail");
        dump_fleet_logs(&fleet);
        return Err(error);
    }

    // Healthy config: the synthesizer EXISTS at invocation (so the invocation-time
    // guard passes); we delete it while fan-out runs.
    configure_fleet_workflow_behaviors(tempdir.path(), coord, subagents, WorkflowFault::Healthy)
        .await?;
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

    let parent_prompt = "Use the research fleet via workflow orchestration: ask researcher-1 for one fact about Mercury, researcher-2 about Venus, researcher-3 about Earth, and researcher-4 about Mars, then synthesize a one-paragraph summary comparing the four planets.";
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

    // Wait until fan-out has STARTED (a fan_out_child bridge exists) — the
    // invocation-time guard has already passed — then delete the synthesizer.
    // Fan-out runs for many seconds before the barrier, so synthesis has not yet
    // spawned: the deletion lands in that window and the spawn-time guard fires.
    wait_for_fan_out_started(&coord.graphql, &parent_session_id, Duration::from_secs(120)).await?;
    delete_behavior_doc(&coord.graphql, FLEET_SYNTHESIZER_BEHAVIOR_ID).await?;
    // Confirm it is actually gone before relying on the guard.
    anyhow::ensure!(
        !behavior_exists(&coord.graphql, FLEET_SYNTHESIZER_BEHAVIOR_ID).await?,
        "synthesizer behavior should be deleted"
    );

    // The orchestration tool call must terminalize FAILED with serviceUnavailable.
    let (state, failure_class) = wait_for_orchestration_terminal(
        &coord.graphql,
        &parent_session_id,
        Duration::from_secs(180),
    )
    .await?;
    anyhow::ensure!(
        state != "completed",
        "orchestration must not complete when the synthesizer is deleted mid-run (got {state})"
    );
    anyhow::ensure!(
        failure_class.as_deref() == Some("serviceUnavailable"),
        "deleted local synthesis target must fail as serviceUnavailable, got {failure_class:?}"
    );

    // No synthesis child was ever written (the guard returns before the bridge).
    let synth_bridges = graphql_query(
        &coord.graphql,
        &format!(
            r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{}" }}, workflow_role: {{ _eq: "synthesis" }} }}) {{ tool_call_id }} }}"#,
            escape_graphql_string(&parent_session_id)
        ),
    )
    .await?;
    let synth_count = synth_bridges
        .pointer("/data/AgentToolCall")
        .and_then(Value::as_array)
        .map(|r| r.len())
        .unwrap_or(0);
    anyhow::ensure!(
        synth_count == 0,
        "no synthesis bridge should be written when the synthesizer is gone, saw {synth_count}"
    );

    // The parent request terminalizes PROMPTLY (not hung to the deadline) — the
    // tool failure is returned to the model, which handles it and finishes the
    // turn, so "completed" here is correct: the fast terminalization is the
    // property, not the parent's terminal state.
    let _ = wait_for_request_terminal(&coord.graphql, &parent_request_id, Duration::from_secs(60))
        .await?;

    drop(fleet);
    Ok(())
}

/// Wait until at least one fan-out child bridge exists (fan-out has begun).
async fn wait_for_fan_out_started(
    coord_graphql: &str,
    session_id: &str,
    timeout: Duration,
) -> Result<()> {
    let escaped = escape_graphql_string(session_id);
    let deadline = Instant::now() + timeout;
    loop {
        let resp = graphql_query(
            coord_graphql,
            &format!(
                r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{escaped}" }}, workflow_role: {{ _eq: "fan_out_child" }} }}, limit: 1) {{ tool_call_id }} }}"#
            ),
        )
        .await?;
        if resp.pointer("/data/AgentToolCall/0").is_some() {
            return Ok(());
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "fan-out did not start within {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Wait until the orchestration tool call is terminal; return (lifecycle_state,
/// tool_failure_class).
async fn wait_for_orchestration_terminal(
    coord_graphql: &str,
    session_id: &str,
    timeout: Duration,
) -> Result<(String, Option<String>)> {
    let escaped = escape_graphql_string(session_id);
    let deadline = Instant::now() + timeout;
    loop {
        let resp = graphql_query(
            coord_graphql,
            &format!(
                r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{escaped}" }}, tool_name: {{ _eq: "fan_out_and_synthesize" }} }}, limit: 1) {{ lifecycle_state tool_failure_class }} }}"#
            ),
        )
        .await?;
        if let Some(row) = resp.pointer("/data/AgentToolCall/0") {
            let state = row.get("lifecycle_state").and_then(Value::as_str);
            if is_workflow_terminal(state) {
                return Ok((
                    state.unwrap_or_default().to_string(),
                    row.get("tool_failure_class")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                ));
            }
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "orchestration did not terminalize within {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Delete an AgentBehavior document (so `load_agent_behavior` returns None).
async fn delete_behavior_doc(graphql: &str, behavior_id: &str) -> Result<()> {
    let escaped = escape_graphql_string(behavior_id);
    let resp = graphql_query(
        graphql,
        &format!(
            r#"mutation {{ delete_AgentBehavior(filter: {{ behavior_id: {{ _eq: "{escaped}" }} }}) {{ _docID }} }}"#
        ),
    )
    .await?;
    let deleted = resp
        .pointer("/data/delete_AgentBehavior")
        .and_then(Value::as_array)
        .map(|r| r.len())
        .unwrap_or(0);
    anyhow::ensure!(
        deleted >= 1,
        "expected to delete behavior {behavior_id}, deleted {deleted}"
    );
    Ok(())
}

async fn behavior_exists(graphql: &str, behavior_id: &str) -> Result<bool> {
    let escaped = escape_graphql_string(behavior_id);
    let resp = graphql_query(
        graphql,
        &format!(
            r#"{{ AgentBehavior(filter: {{ behavior_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{ behavior_id }} }}"#
        ),
    )
    .await?;
    Ok(resp.pointer("/data/AgentBehavior/0").is_some())
}

/// Wait until the workflow has FINISHED on the coordinator: the single
/// `fan_out_and_synthesize` orchestration call is terminal, with its four
/// fan-out bridges and one synthesis bridge present (terminal-completed counts
/// are asserted by the caller; D10 does not require all to complete).
async fn wait_for_workflow_finished(
    coord_graphql: &str,
    session_id: &str,
    timeout: Duration,
) -> Result<WorkflowGroup> {
    let escaped_session = escape_graphql_string(session_id);
    let deadline = Instant::now() + timeout;
    let mut last = "starting".to_string();
    loop {
        if Instant::now() >= deadline {
            bail!("workflow did not finish on the coordinator within {timeout:?}; last: {last}");
        }
        // limit: 2 so a second orchestration call is visible (the uniqueness
        // assert below is vacuous under limit: 1).
        let orch_query = format!(
            r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{escaped_session}" }}, tool_name: {{ _eq: "fan_out_and_synthesize" }} }}, limit: 2) {{ tool_call_id lifecycle_state }} }}"#
        );
        let orch = graphql_query(coord_graphql, &orch_query).await?;
        let orch_rows = orch
            .pointer("/data/AgentToolCall")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        anyhow::ensure!(
            orch_rows.len() <= 1,
            "expected exactly one fan_out_and_synthesize call, saw {}",
            orch_rows.len()
        );
        if let Some(orch_row) = orch_rows.first() {
            let orch_state = orch_row.get("lifecycle_state").and_then(Value::as_str);
            let group_id = orch_row
                .get("tool_call_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if is_workflow_terminal(orch_state) {
                let escaped_group = escape_graphql_string(&group_id);
                let bridges_query = format!(
                    r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{escaped_session}" }}, workflow_group_id: {{ _eq: "{escaped_group}" }} }}, order: {{ started_at: ASC }}) {{ tool_call_id lifecycle_state workflow_role started_at completed_at child_request_id }} }}"#
                );
                let bridges_resp = graphql_query(coord_graphql, &bridges_query).await?;
                let bridges: Vec<WorkflowBridgeRow> = bridges_resp
                    .pointer("/data/AgentToolCall")
                    .and_then(Value::as_array)
                    .map(|rows| {
                        rows.iter()
                            .filter_map(|r| serde_json::from_value(r.clone()).ok())
                            .collect()
                    })
                    .unwrap_or_default();
                let fan_out: Vec<WorkflowBridgeRow> = bridges
                    .iter()
                    .filter(|b| b.workflow_role.as_deref() == Some("fan_out_child"))
                    .cloned()
                    .collect();
                let synthesis: Vec<WorkflowBridgeRow> = bridges
                    .iter()
                    .filter(|b| b.workflow_role.as_deref() == Some("synthesis"))
                    .cloned()
                    .collect();
                anyhow::ensure!(
                    synthesis.len() == 1,
                    "expected exactly one synthesis bridge, saw {}",
                    synthesis.len()
                );
                let synthesis = synthesis.into_iter().next().ok_or_else(|| {
                    anyhow!("workflow finished (orch={orch_state:?}) with no synthesis bridge")
                })?;
                return Ok(WorkflowGroup {
                    group_id,
                    fan_out,
                    synthesis,
                });
            }
            last = format!("orch={orch_state:?} (not terminal)");
        } else {
            last = "no fan_out_and_synthesize tool call yet".to_string();
        }
        tokio::time::sleep(Duration::from_millis(750)).await;
    }
}

/// Fetch a tool call's persisted `args` (the runtime-built prompt/input).
async fn fetch_tool_call_args(coord_graphql: &str, tool_call_id: &str) -> Result<String> {
    let escaped = escape_graphql_string(tool_call_id);
    let query = format!(
        r#"{{ AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{ args }} }}"#
    );
    let resp = graphql_query(coord_graphql, &query).await?;
    Ok(resp
        .pointer("/data/AgentToolCall/0/args")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string())
}

async fn bring_up_fleet(
    root: &Path,
    count: usize,
    model_endpoint: &str,
    model_name: &str,
    coordinator_codex_shim: bool,
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

        let codex_shim_port = if index == 0 && coordinator_codex_shim {
            Some(allocate_port()?)
        } else {
            None
        };
        let mut serve_args = P2P_LOOPBACK_ARGS
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>();
        if let Some(shim_port) = codex_shim_port {
            serve_args.extend([
                "--codex-shim".to_string(),
                "--codex-shim-port".to_string(),
                shim_port.to_string(),
                "--codex-shim-poll-ms".to_string(),
                "100".to_string(),
                "--codex-shim-timeout-secs".to_string(),
                "900".to_string(),
            ]);
        }
        let serve_arg_refs = serve_args.iter().map(String::as_str).collect::<Vec<_>>();
        let (mut serve, readiness) =
            spawn_server_with_ready_json(&home, port, &serve_arg_refs, FAST_RECONCILE_ENVS)?;
        wait_for_port(port, &mut serve)?;
        if let Some(shim_port) = codex_shim_port {
            wait_for_port(shim_port, &mut serve)?;
        }
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
            codex_shim_port,
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
    configure_behavior_prompt_with_model(node, prompt_path, display_name, &node.model_name)
}

/// Like `configure_behavior_prompt`, but with an explicit model name — used to
/// inject a bad model (FLEET_BAD_MODEL) so a CLAIMED child fails at inference.
fn configure_behavior_prompt_with_model(
    node: &FleetNode,
    prompt_path: &Path,
    display_name: &str,
    model_name: &str,
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
            model_name,
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
                    child_session_id: child.session_id.clone(),
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
                    session_id
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
            session_id: row
                .get("session_id")
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
