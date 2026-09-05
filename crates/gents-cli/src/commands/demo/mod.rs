mod backend;
mod fleet;
pub(crate) mod pack;
pub(crate) mod secscan;
mod setup;
mod shell;
mod util;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::cli::args::DemoArgs;

use backend::{read_backend_args, resolve_backend, write_backend_args, BackendChoice};
use fleet::{desktop, spawn_server, wait_http, wait_runtime_ready, Fleet};
use setup::{init_agent, read_agent_did, resolve_home, seed_demo_skills};
use shell::{print_welcome, run_shell};
use util::short;

const NODE_A_NAME: &str = "demo";

pub(crate) async fn demo(args: DemoArgs) -> Result<()> {
    let bin = std::env::current_exe().context("resolving the gents binary path")?;
    let home = resolve_home(args.home.clone());
    if args.reset && home.exists() {
        std::fs::remove_dir_all(&home).ok();
    }
    let work = home.join("work");
    let backend_file = home.join("demo-backend.json");

    let mut reader = BufReader::new(tokio::io::stdin()).lines();

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

    std::fs::create_dir_all(&work)
        .with_context(|| format!("creating demo tool root {}", work.display()))?;

    let port = args.http_port;
    let graphql = format!("http://127.0.0.1:{port}/api/v0/graphql");
    let mut server = spawn_server(&bin, &home, port, &home.join("server.log"))?;
    wait_http(&format!("http://127.0.0.1:{port}/healthz"), &mut server).await?;
    wait_runtime_ready(&graphql, &agent_did, &mut server).await?;

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
    };
    print_welcome(&fleet);
    let result = async {
        if args.desktop {
            desktop(&fleet).await?;
        }
        run_shell(&mut fleet, &mut reader).await
    }
    .await;

    fleet.teardown();
    println!(
        "Stopped. Your demo agent is saved at {} (run `gents demo` again to resume, or `--reset` to start over).",
        home.display()
    );
    result
}
