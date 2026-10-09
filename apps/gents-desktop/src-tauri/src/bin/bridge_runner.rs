#[path = "../runner/live_fixture.rs"]
mod live_fixture;

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
use live_fixture::{LiveBackendOverride, LiveBridgeFixture, LiveTargetBackendOverride};

/// glibc per-thread arenas retained DefraDB query churn at about 2.5x the live
/// heap and memcg-OOM-killed the runtime at 512 MiB (#2034).
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

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
    #[arg(long)]
    agent_target_inference_url: Option<String>,
    #[arg(long)]
    agent_target_model_name: Option<String>,
    #[arg(long)]
    agent_target_provider: Option<String>,
    #[arg(long)]
    agent_target_api_key: Option<String>,
    #[arg(long)]
    agent_target_api_key_env_var: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReadyMessage {
    kind: &'static str,
    base_url: String,
    deployment_label: String,
    node_did: String,
    tool_root: String,
    data_root: String,
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
        let agent_target_override = LiveTargetBackendOverride {
            inference_url: args.agent_target_inference_url,
            model_name: args.agent_target_model_name,
            provider: args.agent_target_provider,
            api_key: args.agent_target_api_key,
            api_key_env_var: args.agent_target_api_key_env_var,
        };
        std::panic::catch_unwind(|| {
            LiveBridgeFixture::start(Some(backend_override), Some(agent_target_override))
        })
        .map_err(|_| anyhow!("bridge runner panicked during startup"))??
    };
    let server = BridgeRunnerServer::start(Arc::clone(&fixture))?;
    let ready = ReadyMessage {
        kind: "ready",
        base_url: server.base_url(),
        deployment_label: fixture.deployment_label().to_string(),
        node_did: fixture.node_did().to_string(),
        tool_root: fixture.tool_root().display().to_string(),
        data_root: fixture.data_root().display().to_string(),
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
