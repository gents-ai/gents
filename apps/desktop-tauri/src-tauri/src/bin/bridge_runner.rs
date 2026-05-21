#[path = "../runner/live_fixture.rs"]
mod live_fixture;

mod bridge {
    #[allow(dead_code)]
    pub mod types {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/bridge/types.rs"));
    }
    pub mod commands {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/bridge/commands.rs"
        ));
    }
    #[allow(dead_code)]
    pub mod cause_derivation {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/bridge/cause_derivation.rs"
        ));
    }
    #[allow(dead_code)]
    pub mod snapshot {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/bridge/snapshot.rs"
        ));
    }
    pub mod cascade {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/bridge/cascade.rs"
        ));
    }
}

#[path = "bridge_runner/diagnostics.rs"]
mod diagnostics;
#[path = "bridge_runner/http.rs"]
mod http;

use std::io::{Read, Write};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use serde::Serialize;

use http::BridgeRunnerServer;
use live_fixture::{LiveBackendOverride, LiveBridgeFixture};

#[derive(Debug, Parser)]
struct RunnerArgs {
    #[arg(long)]
    desktop_only: bool,
    #[arg(long)]
    inference_url: Option<String>,
    #[arg(long)]
    model_name: Option<String>,
    #[arg(long)]
    provider: Option<String>,
    #[arg(long)]
    api_key: Option<String>,
    #[arg(long)]
    api_key_env_var: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReadyMessage {
    kind: &'static str,
    base_url: String,
    deployment_label: String,
    agent_did: String,
    tool_root: String,
}

fn main() -> Result<()> {
    let args = RunnerArgs::parse();
    let fixture = if args.desktop_only {
        std::panic::catch_unwind(LiveBridgeFixture::start_desktop_only)
            .map_err(|_| anyhow!("bridge runner panicked during desktop-only startup"))??
    } else {
        let backend_override = LiveBackendOverride {
            inference_url: args.inference_url,
            model_name: args.model_name,
            provider: args.provider,
            api_key: args.api_key,
            api_key_env_var: args.api_key_env_var,
        };
        std::panic::catch_unwind(|| LiveBridgeFixture::start(Some(backend_override)))
            .map_err(|_| anyhow!("bridge runner panicked during startup"))??
    };
    let server = BridgeRunnerServer::start(Arc::clone(&fixture))?;
    let ready = ReadyMessage {
        kind: "ready",
        base_url: server.base_url(),
        deployment_label: fixture.deployment_label().to_string(),
        agent_did: fixture.agent_did().to_string(),
        tool_root: fixture.tool_root().display().to_string(),
    };
    println!("{}", serde_json::to_string(&ready)?);
    std::io::stdout().flush().ok();

    let result = wait_for_shutdown_signal();
    server.stop();
    fixture.runtime().block_on(fixture.shutdown())?;
    result
}

fn wait_for_shutdown_signal() -> Result<()> {
    let mut stdin = std::io::stdin();
    let mut buffer = [0_u8; 1];
    loop {
        match stdin.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error).context("reading bridge runner stdin"),
        }
    }
}
