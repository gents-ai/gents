//! `defra-agent demo` — an interactive, self-contained fleet demo that ships in
//! the binary. It boots a single curated local agent (read-only tools + demo
//! skills) backed by a real model (interactive first-run backend picker,
//! persisted), and drops into an interactive `demo>` shell: `chat` with the
//! agent, `pair` a 2nd node (worker), `delegate` cross-node subagent work,
//! `desktop` to open the paired desktop client, and `reconfigure` the backend.
//!
//! The command is split into focused modules:
//! - [`backend`] — resolve/persist the inference backend (no mock, ever)
//! - [`setup`] — persistent home, curated `init`, demo skills
//! - [`fleet`] — node lifecycle plus `pair`/`delegate`/`desktop`
//! - [`shell`] — the interactive `demo>` REPL, chat, and `reconfigure`
//! - [`util`] — shared subprocess/prompt plumbing (one async stdin reader)

mod backend;
mod fleet;
mod setup;
mod shell;
mod util;

use anyhow::{Context, Result};

use crate::cli::args::DemoArgs;

use backend::{read_backend_args, resolve_backend, write_backend_args, BackendChoice};
use fleet::{spawn_server, wait_http, Fleet};
use setup::{init_agent, read_agent_did, resolve_home, seed_demo_skills};
use shell::{print_welcome, run_shell};
use util::short;

const NODE_A_NAME: &str = "demo";

pub(crate) async fn demo(args: DemoArgs) -> Result<()> {
    let bin = std::env::current_exe().context("resolving the defra-agent binary path")?;
    let home = resolve_home(args.home.clone());
    if args.reset && home.exists() {
        std::fs::remove_dir_all(&home).ok();
    }
    let work = home.join("work");
    let backend_file = home.join("demo-backend.json");

    // One owned stdin reader drives the picker, the shell, and reconfigure.
    let mut reader = crate::prompt::stdin_lines();

    // First run sets up; later runs resume the saved agent. The backend args are
    // persisted so a resumed session (and a paired node B) reuse the same backend.
    let (agent_did, first_run, backend) = match read_agent_did(&home) {
        Some(did) => {
            println!("Resuming your demo agent ({}).", short(&did));
            let backend = BackendChoice {
                init_args: read_backend_args(&backend_file),
                label: "saved".into(),
            };
            (did, false, backend)
        }
        None => {
            let backend = resolve_backend(&args, &mut reader).await?;
            println!("\nSetting up your demo agent (backend: {})…", backend.label);
            let did = init_agent(&bin, &home, &work, &backend, NODE_A_NAME).await?;
            write_backend_args(&backend_file, &backend.init_args);
            (did, true, backend)
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
        bin,
        home_a: home.clone(),
        work_a: work,
        graphql_a: graphql,
        did_a: agent_did,
        base_port: port,
        backend,
        server_a: server,
        node_b: None,
    };
    print_welcome(&fleet);
    let result = run_shell(&mut fleet, &mut reader).await;

    fleet.teardown();
    println!(
        "Stopped. Your demo agent is saved at {} (run `defra-agent demo` again to resume, or `--reset` to start over).",
        home.display()
    );
    result
}
