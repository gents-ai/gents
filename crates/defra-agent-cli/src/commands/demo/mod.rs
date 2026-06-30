//! `defra-agent demo` — an interactive, self-contained fleet demo that ships in
//! the binary. Stage 1: boot a single curated local agent (read-only tools +
//! demo skills) backed by a real model (interactive first-run backend picker,
//! persisted), and drop into an interactive `demo>` shell. `pair`/`delegate`
//! (2-node + cross-node delegation) and `desktop` (paired desktop client) come
//! in later stages.

use std::io::{BufRead, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

use defra_agent::graphql::escape_graphql_string;

use crate::cli::args::DemoArgs;
use crate::commands::chat::submit_chat_turn;
use crate::graphql_access::post_graphql;

const NODE_A_NAME: &str = "demo";

/// How the demo's inference backend was resolved, ready to pass to `init`.
struct BackendChoice {
    init_args: Vec<String>,
    label: String,
}

pub(crate) async fn demo(args: DemoArgs) -> Result<()> {
    let bin = std::env::current_exe().context("resolving the defra-agent binary path")?;
    let home = resolve_home(args.home.clone());
    if args.reset && home.exists() {
        std::fs::remove_dir_all(&home).ok();
    }
    let work = home.join("work");

    // First run sets up; later runs resume the saved agent. The backend args are
    // persisted so a resumed session (and a paired node B) reuse the same backend.
    let backend_file = home.join("demo-backend.json");
    let (agent_did, first_run, backend_init_args) = match read_agent_did(&home) {
        Some(did) => {
            println!("Resuming your demo agent ({}).", short(&did));
            (did, false, read_backend_args(&backend_file))
        }
        None => {
            let backend = resolve_backend(&args).await?;
            println!("\nSetting up your demo agent (backend: {})…", backend.label);
            let did = init_agent(&bin, &home, &work, &backend).await?;
            write_backend_args(&backend_file, &backend.init_args);
            (did, true, backend.init_args)
        }
    };

    // The tool root must exist when the server boots; `init --dangerously-overwrite`
    // wipes the home, so (re)create it here for both first-run and resume.
    std::fs::create_dir_all(&work)
        .with_context(|| format!("creating demo tool root {}", work.display()))?;

    let port = args.http_port;
    let graphql = format!("http://127.0.0.1:{port}/api/v0/graphql");
    let mut server = spawn_server(&bin, &home, port, &home.join("server.log"))?;
    wait_http(&format!("http://127.0.0.1:{port}/healthz"), &mut server).await?;

    if first_run {
        seed_demo_skills(&bin, &graphql, &agent_did).await;
    }

    let mut fleet = Fleet {
        bin: bin.clone(),
        home_a: home.clone(),
        graphql_a: graphql.clone(),
        did_a: agent_did.clone(),
        base_port: port,
        backend_init_args,
        node_b: None,
    };
    print_welcome(&graphql);
    let mut reader = BufReader::new(tokio::io::stdin()).lines();
    let result = run_shell(&mut fleet, &mut reader).await;

    fleet.teardown();
    let _ = server.start_kill();
    println!(
        "Stopped. Your demo agent is saved at {} (run `defra-agent demo` again to resume, or `--reset` to start over).",
        home.display()
    );
    result
}

/// The running demo fleet: node A (always) plus an optional paired node B.
struct Fleet {
    bin: PathBuf,
    home_a: PathBuf,
    graphql_a: String,
    did_a: String,
    base_port: u16,
    backend_init_args: Vec<String>,
    node_b: Option<NodeB>,
}

struct NodeB {
    graphql: String,
    did: String,
    server: Child,
}

impl Fleet {
    fn teardown(&mut self) {
        if let Some(b) = self.node_b.as_mut() {
            let _ = b.server.start_kill();
        }
    }
}

fn write_backend_args(path: &Path, args: &[String]) {
    if let Ok(json) = serde_json::to_string(args) {
        let _ = std::fs::write(path, json);
    }
}

fn read_backend_args(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn resolve_home(explicit: Option<PathBuf>) -> PathBuf {
    explicit.unwrap_or_else(|| {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(".defra-agent-demo")
    })
}

fn read_agent_did(home: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(home.join("init.json")).ok()?;
    let json: Value = serde_json::from_str(&raw).ok()?;
    json.get("agent_did")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

async fn init_agent(
    bin: &Path,
    home: &Path,
    work: &Path,
    backend: &BackendChoice,
) -> Result<String> {
    let mut init_args: Vec<String> = vec![
        "init".into(),
        "--home".into(),
        path_arg(home),
        "--dangerously-overwrite".into(),
        "--agent-name".into(),
        NODE_A_NAME.into(),
        "--tool-root".into(),
        path_arg(work),
        "--disable-defra-query".into(),
    ];
    init_args.extend(backend.init_args.iter().cloned());
    let init = run_cli_json(bin, &init_args).await?;
    init.get("agent_did")
        .and_then(Value::as_str)
        .context("init did not return agent_did")
        .map(ToString::to_string)
}

// ---- backend resolution -----------------------------------------------------

async fn resolve_backend(args: &DemoArgs) -> Result<BackendChoice> {
    // Non-interactive paths first (flags / env).
    if let Some(url) = &args.inference_url {
        return Ok(custom_url_backend(
            url,
            args.model.as_deref(),
            args.api_key.as_deref(),
        ));
    }
    if let Some(preset) = &args.backend_preset {
        return Ok(preset_backend(
            preset,
            args.model.as_deref(),
            args.api_key.as_deref(),
        ));
    }
    if args.api_key.is_some() || std::env::var("OPENAI_API_KEY").is_ok() {
        return Ok(openai_backend(
            args.model.as_deref(),
            args.api_key.as_deref(),
        ));
    }
    // Interactive first-run picker.
    pick_backend(args).await
}

async fn pick_backend(args: &DemoArgs) -> Result<BackendChoice> {
    let local = detect_local().await;
    println!("\nHow do you want to run inference for the demo?");
    println!("  1) OpenAI API key   (paste it; stored locally in the demo home)");
    if let Some((url, _)) = &local {
        println!("  2) local server     (detected at {url})");
    } else {
        println!("  2) local server     (e.g. ollama / llama-server)");
    }
    println!("  3) custom URL");
    let choice = prompt_line("> ")?;
    match choice.trim() {
        "1" | "" => {
            let key = prompt_secret("Paste your OpenAI API key (hidden): ")?;
            let key = key.trim();
            if key.is_empty() {
                bail!("no API key entered");
            }
            Ok(openai_backend(args.model.as_deref(), Some(key)))
        }
        "2" => {
            let (url, model) = match local {
                Some(found) => found,
                None => {
                    let url = prompt_line("Local server base URL [http://127.0.0.1:11434/v1]: ")?;
                    let url = non_empty(&url)
                        .unwrap_or("http://127.0.0.1:11434/v1")
                        .to_string();
                    let model = probe_models(&url).await.unwrap_or_default();
                    (url, model)
                }
            };
            let model = args.model.clone().or(non_empty(&model).map(str::to_string));
            Ok(custom_url_backend(&url, model.as_deref(), None))
        }
        "3" => {
            let url = prompt_line("Backend base URL (incl. /v1): ")?;
            let url = url.trim();
            if url.is_empty() {
                bail!("no URL entered");
            }
            let model = prompt_line("Model name: ")?;
            Ok(custom_url_backend(url, non_empty(&model), None))
        }
        other => bail!("unrecognized choice: {other}"),
    }
}

async fn detect_local() -> Option<(String, String)> {
    for url in ["http://127.0.0.1:8080/v1", "http://127.0.0.1:11434/v1"] {
        if let Some(model) = probe_models(url).await {
            return Some((url.to_string(), model));
        }
    }
    None
}

fn openai_backend(model: Option<&str>, api_key: Option<&str>) -> BackendChoice {
    let model = model.unwrap_or("gpt-4.1-mini").to_string();
    let mut init_args = vec![
        "--backend-preset".into(),
        "openai".into(),
        "--model-name".into(),
        model.clone(),
    ];
    if let Some(key) = api_key {
        init_args.push("--api-key".into());
        init_args.push(key.to_string());
    }
    BackendChoice {
        init_args,
        label: format!("openai · {model}"),
    }
}

fn preset_backend(preset: &str, model: Option<&str>, api_key: Option<&str>) -> BackendChoice {
    let model = model.unwrap_or("gpt-4.1-mini").to_string();
    let mut init_args = vec![
        "--backend-preset".into(),
        preset.to_string(),
        "--model-name".into(),
        model.clone(),
    ];
    if let Some(key) = api_key {
        init_args.push("--api-key".into());
        init_args.push(key.to_string());
    }
    BackendChoice {
        init_args,
        label: format!("{preset} · {model}"),
    }
}

fn custom_url_backend(url: &str, model: Option<&str>, api_key: Option<&str>) -> BackendChoice {
    let model = model.unwrap_or("demo-model").to_string();
    let mut init_args = vec![
        "--inference-url".into(),
        url.to_string(),
        "--model-name".into(),
        model.clone(),
    ];
    if let Some(key) = api_key {
        init_args.push("--api-key".into());
        init_args.push(key.to_string());
    }
    BackendChoice {
        init_args,
        label: format!("{url} · {model}"),
    }
}

/// GET `{base}/models`; return the first advertised model id if reachable.
async fn probe_models(base: &str) -> Option<String> {
    let response = reqwest::Client::new()
        .get(format!("{base}/models"))
        .timeout(Duration::from_millis(700))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body: Value = response.json().await.ok()?;
    body.pointer("/data/0/id")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

// ---- node lifecycle ---------------------------------------------------------

fn spawn_server(bin: &Path, home: &Path, port: u16, log: &Path) -> Result<Child> {
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

async fn wait_http(url: &str, server: &mut Child) -> Result<()> {
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

async fn seed_demo_skills(bin: &Path, graphql: &str, agent_did: &str) {
    let skills = [
        (
            "summarize",
            "Summarize",
            "Distill long text into a concise summary.",
            "When asked to summarize: capture the key points and decisions, drop filler, \
             and keep it short and faithful to the source.",
        ),
        (
            "fleet-guide",
            "Fleet Guide",
            "Explain this defra-agent demo and suggest what to try next.",
            "You are running inside `defra-agent demo`, an agent whose state lives in a DefraDB \
             control plane. Explain P2P pairing and cross-node subagent delegation in plain \
             terms, and suggest the user try `pair` then `delegate` in the demo shell.",
        ),
    ];
    for (id, name, description, instructions) in skills {
        let args = [
            "config",
            "skill",
            "add",
            "--graphql",
            graphql,
            "--agent-did",
            agent_did,
            "--skill-id",
            id,
            "--name",
            name,
            "--scope",
            "principal",
            "--description",
            description,
            "--instructions",
            instructions,
        ];
        if let Err(error) = run_cli_text(bin, &args.map(String::from)).await {
            eprintln!("  (skill {id} not seeded: {error})");
        }
    }
}

// ---- shell ------------------------------------------------------------------

fn print_welcome(graphql: &str) {
    println!();
    println!("✓ Your demo agent is live.");
    println!("  skills:  summarize, fleet-guide");
    println!("  graphql: {graphql}");
    println!();
    println!("Commands: chat · skill · status · pair · delegate · help · down");
    println!("Type `chat` to talk to your agent, or `help` for the full list.");
}

async fn run_shell(
    fleet: &mut Fleet,
    reader: &mut tokio::io::Lines<BufReader<tokio::io::Stdin>>,
) -> Result<()> {
    loop {
        prompt("demo> ");
        let Some(line) = reader.next_line().await? else {
            break;
        };
        match line.trim() {
            "" => continue,
            "help" | "?" => print_help(),
            "chat" => chat_loop(&fleet.graphql_a, &fleet.did_a, reader).await?,
            "status" => print_status(fleet),
            "skill" | "skills" => {
                println!("  skills: summarize, fleet-guide (ask the agent to use one in `chat`)")
            }
            "pair" => {
                if let Err(error) = pair(fleet).await {
                    println!("  pair failed: {error}");
                }
            }
            "delegate" => {
                if let Err(error) = delegate(fleet).await {
                    println!("  delegate failed: {error}");
                }
            }
            "down" | "quit" | "exit" => break,
            other => println!("  unknown command: {other} (try `help`)"),
        }
    }
    Ok(())
}

fn print_status(fleet: &Fleet) {
    println!(
        "  node A: live · agent {} · {}",
        short(&fleet.did_a),
        fleet.graphql_a
    );
    match &fleet.node_b {
        Some(b) => {
            println!("  node B: live · agent {} · {}", short(&b.did), b.graphql);
            println!("  paired: yes · run `delegate` for cross-node subagent delegation");
        }
        None => println!("  node B: not paired (run `pair` to add a 2nd node)"),
    }
}

// ---- pair (2-node fleet) ----------------------------------------------------

async fn pair(fleet: &mut Fleet) -> Result<()> {
    if fleet.node_b.is_some() {
        println!("  already paired.");
        return Ok(());
    }
    let bin = fleet.bin.clone();
    let home_a = fleet.home_a.clone();
    let home_b = home_a.join("node-b");
    let work_b = home_b.join("work");
    let port_b = fleet.base_port + 1;
    let graphql_b = format!("http://127.0.0.1:{port_b}/api/v0/graphql");

    println!("  initializing node B (worker)…");
    let mut init_b: Vec<String> = vec![
        "init".into(),
        "--home".into(),
        path_arg(&home_b),
        "--dangerously-overwrite".into(),
        "--agent-name".into(),
        "worker".into(),
        "--tool-root".into(),
        path_arg(&work_b),
        "--disable-defra-query".into(),
    ];
    init_b.extend(fleet.backend_init_args.iter().cloned());
    let did_b = run_cli_json(&bin, &init_b)
        .await?
        .get("agent_did")
        .and_then(Value::as_str)
        .context("node B init did not return agent_did")?
        .to_string();
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
    upsert_data_plane(&fleet.graphql_a, &peer_b, &fleet.did_a, &addr_b).await?;
    upsert_data_plane(&graphql_b, &peer_a, &did_b, &addr_a).await?;

    println!("  waiting for replicators…");
    wait_replicator(&graphql_b, &peer_a).await?;
    wait_replicator(&fleet.graphql_a, &peer_b).await?;

    fleet.node_b = Some(NodeB {
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

const DATA_PLANE_COLLECTIONS: &str = r#"["AgentRequest","AgentResponse","AgentMessage","AgentToolCall","AgentToolResult","AgentSession","AgentConversation","CompactionEntry"]"#;

async fn upsert_data_plane(
    graphql: &str,
    peer_id: &str,
    local_did: &str,
    address: &str,
) -> Result<()> {
    let peer = escape_graphql_string(peer_id);
    let did = escape_graphql_string(local_did);
    let addr = escape_graphql_string(address);
    let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let cols = DATA_PLANE_COLLECTIONS;
    let mutation = format!(
        r#"mutation {{
  upsert_DataPlanePairingDesired(
    filter: {{ peer_id: {{ _eq: "{peer}" }} }},
    add: {{ peer_id: "{peer}", agent_did: "{did}", collections: {cols}, replicator_addresses: ["{addr}"], template: "conversation", created_at: "{now}", updated_at: "{now}" }},
    update: {{ agent_did: "{did}", collections: {cols}, replicator_addresses: ["{addr}"], template: "conversation", updated_at: "{now}" }}
  ) {{ _docID }}
}}"#
    );
    post_graphql(graphql, &mutation).await?;
    Ok(())
}

async fn wait_replicator(graphql: &str, peer_id: &str) -> Result<()> {
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
            if armed {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    bail!("timed out waiting for the replicator for peer {peer_id}")
}

// ---- delegate (cross-node subagent) -----------------------------------------

async fn delegate(fleet: &Fleet) -> Result<()> {
    let Some(worker) = &fleet.node_b else {
        bail!("not paired — run `pair` first");
    };
    println!("  configuring cross-node delegation…");
    // Worker (node B) must accept cross-deployment spawns from the orchestrator.
    config_tools(
        &fleet.bin,
        &worker.graphql,
        &worker.did,
        &[
            "--enable-defra-query",
            "false",
            "--subagent-allow-cross-deployment",
            "true",
        ],
    )
    .await?;
    let worker_gen = runtime_generation(&worker.graphql).await;
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
            "false",
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

/// Build an owned arg vector from string slices (paths must already be strings).
fn cli(args: &[&str]) -> Vec<String> {
    args.iter().map(|value| value.to_string()).collect()
}

async fn chat_loop(
    graphql: &str,
    agent_did: &str,
    reader: &mut tokio::io::Lines<BufReader<tokio::io::Stdin>>,
) -> Result<()> {
    let session_id = uuid::Uuid::new_v4().to_string();
    println!("  (chatting — type `/back` to return to the demo shell)");
    loop {
        prompt("you> ");
        let Some(line) = reader.next_line().await? else {
            break;
        };
        let message = line.trim();
        if message.is_empty() {
            continue;
        }
        if matches!(message, "/back" | "/exit" | "back") {
            break;
        }
        // Reuse the chat command's streaming so replies stream token-by-token.
        if let Err(error) =
            submit_chat_turn(graphql, agent_did, &session_id, None, message, 90, 1).await
        {
            println!("  (chat error: {error})");
        }
    }
    Ok(())
}

fn print_help() {
    println!("  chat      talk to your agent (skills available; `/back` to exit)");
    println!("  skill     list the demo skills");
    println!("  status    show the fleet state");
    println!("  pair      spin up a 2nd node and pair it (next stage)");
    println!("  delegate  cross-node subagent delegation (later stage)");
    println!("  down      stop and exit (state is saved; `--reset` to wipe)");
}

// ---- small helpers ----------------------------------------------------------

fn prompt(text: &str) {
    print!("{text}");
    let _ = std::io::stdout().flush();
}

fn prompt_line(text: &str) -> Result<String> {
    prompt(text);
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line)
}

/// Read a line without echoing it (best-effort via `stty`; falls back to a
/// visible read when stdin is not a terminal, e.g. piped input).
fn prompt_secret(text: &str) -> Result<String> {
    prompt(text);
    let hidden = std::process::Command::new("stty")
        .arg("-echo")
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line);
    if hidden {
        let _ = std::process::Command::new("stty").arg("echo").status();
        println!();
    }
    read?;
    Ok(line)
}

fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn short(did: &str) -> String {
    if did.len() > 16 {
        format!("{}…", &did[..16])
    } else {
        did.to_string()
    }
}

fn path_arg(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

async fn run_cli_text(bin: &Path, args: &[String]) -> Result<String> {
    let output = Command::new(bin)
        .args(args)
        .output()
        .await
        .context("running defra-agent subcommand")?;
    if !output.status.success() {
        bail!(
            "defra-agent {} failed: {}",
            args.first().cloned().unwrap_or_default(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

async fn run_cli_json(bin: &Path, args: &[String]) -> Result<Value> {
    let stdout = run_cli_text(bin, args).await?;
    serde_json::from_str(stdout.trim()).with_context(|| {
        format!(
            "parsing JSON from defra-agent {}",
            args.first().cloned().unwrap_or_default()
        )
    })
}
