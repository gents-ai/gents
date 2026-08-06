use anyhow::{Context, Result};

use crate::commands::chat::submit_chat_turn;

use super::backend::{pick_backend, write_backend_args};
use super::fleet::{delegate, desktop, pair, spawn_server, wait_http, wait_runtime_ready, Fleet};
use super::setup::{init_agent, seed_demo_skills};
use super::util::{prompt, short, StdinLines};
use super::NODE_A_NAME;

pub(super) fn print_welcome(fleet: &Fleet) {
    println!();
    println!("✓ Your demo agent is live.");
    println!("  skills:  summarize, fleet-guide");
    println!("  graphql: {}", fleet.graphql_a);
    println!();
    println!(
        "Commands: chat · skill · status · pair · delegate · desktop · reconfigure · help · down"
    );
    println!("Type `chat` to talk to your agent, or `help` for the full list.");
}

pub(super) async fn run_shell(fleet: &mut Fleet, reader: &mut StdinLines) -> Result<()> {
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
            "desktop" => {
                if let Err(error) = desktop(fleet).await {
                    println!("  desktop failed: {error}");
                }
            }
            "reconfigure" => {
                if let Err(error) = reconfigure(fleet, reader).await {
                    println!("  reconfigure failed: {error}");
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

fn print_help() {
    println!("  chat         talk to your agent (skills available; `/back` to exit)");
    println!("  skill        list the demo skills");
    println!("  status       show the fleet state");
    println!("  pair         spin up a 2nd node (worker) and pair it");
    println!("  delegate     enable cross-node subagent delegation (run after `pair`)");
    println!("  desktop      open the desktop app paired to your demo node(s)");
    println!("  reconfigure  switch the inference backend (starts a fresh agent)");
    println!("  down         stop and exit (state is saved; `--reset` to wipe)");
}

async fn chat_loop(graphql: &str, agent_did: &str, reader: &mut StdinLines) -> Result<()> {
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
        if let Err(error) =
            submit_chat_turn(graphql, agent_did, &session_id, None, message, 90, 1).await
        {
            println!("  (chat error: {error})");
        }
    }
    Ok(())
}

async fn reconfigure(fleet: &mut Fleet, reader: &mut StdinLines) -> Result<()> {
    println!("  Reconfigure starts a fresh demo agent with a new backend");
    println!("  (this clears the current agent and any paired node).");
    if let Some(mut worker) = fleet.node_b.take() {
        let _ = worker.server.start_kill();
    }
    let backend = pick_backend(None, reader).await?;
    println!("  Setting up your demo agent (backend: {})…", backend.label);

    let _ = fleet.server_a.start_kill();
    let _ = fleet.server_a.wait().await;

    let did = init_agent(
        &fleet.bin,
        &fleet.home_a,
        &fleet.work_a,
        &backend,
        NODE_A_NAME,
    )
    .await?;
    std::fs::create_dir_all(&fleet.work_a)
        .with_context(|| format!("creating demo tool root {}", fleet.work_a.display()))?;
    write_backend_args(&fleet.home_a.join("demo-backend.json"), &backend.init_args);

    let mut server = spawn_server(
        &fleet.bin,
        &fleet.home_a,
        fleet.base_port,
        &fleet.home_a.join("server.log"),
    )?;
    wait_http(
        &format!("http://127.0.0.1:{}/healthz", fleet.base_port),
        &mut server,
    )
    .await?;
    wait_runtime_ready(&fleet.graphql_a, &did, &mut server).await?;
    seed_demo_skills(&fleet.bin, &fleet.graphql_a, &did).await;

    fleet.server_a = server;
    fleet.did_a = did;
    fleet.backend = backend;
    println!(
        "  ✓ Reconfigured. Your fresh demo agent ({}) is live.",
        short(&fleet.did_a)
    );
    Ok(())
}
