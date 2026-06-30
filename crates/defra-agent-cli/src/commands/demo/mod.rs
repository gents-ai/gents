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

use crate::cli::args::DemoArgs;
use crate::commands::chat::submit_chat_turn;

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

    // First run sets up; later runs resume the saved agent.
    let (agent_did, first_run) = match read_agent_did(&home) {
        Some(did) => {
            println!("Resuming your demo agent ({}).", short(&did));
            (did, false)
        }
        None => {
            let backend = resolve_backend(&args).await?;
            println!("\nSetting up your demo agent (backend: {})…", backend.label);
            (init_agent(&bin, &home, &work, &backend).await?, true)
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

    print_welcome(&graphql);
    let result = run_shell(&graphql, &agent_did).await;

    let _ = server.start_kill();
    println!("Stopped. Your demo agent is saved at {} (run `defra-agent demo` again to resume, or `--reset` to start over).", home.display());
    result
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

async fn run_shell(graphql: &str, agent_did: &str) -> Result<()> {
    let mut reader = BufReader::new(tokio::io::stdin()).lines();
    loop {
        prompt("demo> ");
        let Some(line) = reader.next_line().await? else {
            break;
        };
        match line.trim() {
            "" => continue,
            "help" | "?" => print_help(),
            "chat" => chat_loop(graphql, agent_did, &mut reader).await?,
            "status" => println!("  node A: live · agent {} · {graphql}", short(agent_did)),
            "skill" | "skills" => {
                println!("  skills: summarize, fleet-guide (ask the agent to use one in `chat`)")
            }
            "pair" => println!("  `pair` (add a 2nd node) lands in the next stage."),
            "delegate" => println!("  `delegate` (cross-node subagent) lands in a later stage."),
            "down" | "quit" | "exit" => break,
            other => println!("  unknown command: {other} (try `help`)"),
        }
    }
    Ok(())
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
