#[path = "../runner/live_fixture.rs"]
mod live_fixture;

#[path = "bridge_runner/diagnostics.rs"]
mod diagnostics;
#[path = "bridge_runner/http.rs"]
mod http;

use std::io::Write;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use clap::builder::FalseyValueParser;
use clap::Parser;
use serde::Serialize;
use tokio::io::AsyncReadExt;

use http::BridgeRunnerServer;
use live_fixture::{LiveBackendOverride, LiveBridgeFixture, LiveTargetBackendOverride};

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Debug, Parser)]
struct RunnerArgs {
    #[arg(long)]
    desktop_only: bool,
    /// Serves the test-fixture routes that write past the desktop's own surface.
    #[arg(long, env = "GENTS_TAURI_LIVE", value_parser = FalseyValueParser::new())]
    live: bool,
    /// Keeps the fixture's temporary data after shutdown.
    #[arg(long, env = "GENTS_TAURI_LIVE_RETAIN_DATA", value_parser = FalseyValueParser::new())]
    retain_data: bool,
    #[arg(long, env = "GENTS_DESKTOP_TEST_LOG")]
    log_filter: Option<String>,
    /// JSON object overriding the fixture's sampling, profile and execution limits.
    #[arg(long, env = "GENTS_DESKTOP_LIVE_INFERENCE")]
    inference_settings: Option<String>,
    #[arg(long, env = "GENTS_DESKTOP_LIVE_BACKEND_ENDPOINT")]
    inference_url: Option<String>,
    #[arg(long, env = "GENTS_DESKTOP_LIVE_BACKEND_MODEL")]
    model_name: Option<String>,
    #[arg(long, env = "GENTS_TEST_OPENROUTER_MODEL")]
    fallback_model_name: Option<String>,
    #[arg(long, env = "GENTS_DESKTOP_LIVE_BACKEND_PROVIDER")]
    provider: Option<String>,
    #[arg(
        long,
        env = "GENTS_DESKTOP_LIVE_BACKEND_API_KEY",
        hide_env_values = true
    )]
    api_key: Option<String>,
    #[arg(long, env = "GENTS_DESKTOP_LIVE_BACKEND_API_KEY_ENV_VAR")]
    api_key_env_var: Option<String>,
    #[arg(long, env = "OPENROUTER_API_KEY", hide_env_values = true)]
    openrouter_api_key: Option<String>,
    #[arg(long, env = "GENTS_DESKTOP_LIVE_TARGET_BACKEND_ENDPOINT")]
    agent_target_inference_url: Option<String>,
    #[arg(long, env = "GENTS_DESKTOP_LIVE_TARGET_BACKEND_MODEL")]
    agent_target_model_name: Option<String>,
    #[arg(long, env = "GENTS_DESKTOP_LIVE_TARGET_BACKEND_PROVIDER")]
    agent_target_provider: Option<String>,
    #[arg(
        long,
        env = "GENTS_DESKTOP_LIVE_TARGET_BACKEND_API_KEY",
        hide_env_values = true
    )]
    agent_target_api_key: Option<String>,
    #[arg(long, env = "GENTS_DESKTOP_LIVE_TARGET_BACKEND_API_KEY_ENV_VAR")]
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
    live_fixture::run_on_live_runtime_with_handle(|handle| run(RunnerArgs::parse(), handle))?
}

async fn run(args: RunnerArgs, runtime: tokio::runtime::Handle) -> Result<()> {
    init_tracing(args.log_filter.as_deref());
    let fixture = if args.desktop_only {
        tokio::spawn(LiveBridgeFixture::start_desktop_only(args.retain_data))
            .await
            .map_err(|_| anyhow!("bridge runner panicked during desktop-only startup"))??
    } else {
        let backend_override = LiveBackendOverride {
            inference_url: args.inference_url,
            model_name: args.model_name,
            fallback_model_name: args.fallback_model_name,
            provider: args.provider,
            api_key: args.api_key,
            api_key_env_var: args.api_key_env_var,
            openrouter_api_key: args.openrouter_api_key,
            inference_settings: args.inference_settings,
        };
        let agent_target_override = LiveTargetBackendOverride {
            inference_url: args.agent_target_inference_url,
            model_name: args.agent_target_model_name,
            provider: args.agent_target_provider,
            api_key: args.agent_target_api_key,
            api_key_env_var: args.agent_target_api_key_env_var,
        };
        tokio::spawn(LiveBridgeFixture::start(
            Some(backend_override),
            Some(agent_target_override),
            args.retain_data,
        ))
        .await
        .map_err(|_| anyhow!("bridge runner panicked during startup"))??
    };
    let server = BridgeRunnerServer::start(Arc::clone(&fixture), runtime, args.live).await?;
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

    let result = wait_for_shutdown_signal().await;
    server.stop().await;
    fixture.shutdown().await?;
    result
}

fn init_tracing(filter: Option<&str>) {
    use tracing_subscriber::prelude::*;
    let _ = tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            filter.unwrap_or("warn,gents_desktop_tauri=info"),
        ))
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .compact()
                .without_time(),
        )
        .try_init();
}

async fn wait_for_shutdown_signal() -> Result<()> {
    let mut stdin = tokio::io::stdin();
    let mut buffer = [0_u8; 1];
    loop {
        match stdin.read(&mut buffer).await {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error).context("reading bridge runner stdin"),
        }
    }
}
