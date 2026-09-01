use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use tokio::process::{Child, Command};

use gents::graphql::escape_graphql_string;
use gents_protocol::row::{
    decode_behavior_readiness_snapshot, AgentBehaviorReadinessRow, BehaviorReadinessSnapshot,
};

use crate::graphql_access::post_graphql;

use super::backend::BackendChoice;
use super::util::path_arg;

pub(super) struct Fleet {
    pub(super) bin: PathBuf,
    pub(super) home_a: PathBuf,
    pub(super) work_a: PathBuf,
    pub(super) graphql_a: String,
    pub(super) did_a: String,
    pub(super) base_port: u16,
    pub(super) backend: BackendChoice,
    pub(super) server_a: Child,
}

impl Fleet {
    pub(super) fn teardown(&mut self) {
        let _ = self.server_a.start_kill();
    }
}

pub(super) fn spawn_server(bin: &Path, home: &Path, port: u16, log: &Path) -> Result<Child> {
    spawn_server_with_args(bin, home, port, log, &[])
}

/// `extra` appends server flags (e.g. `--apply-root <pack>`) after the shared
/// demo defaults.
pub(super) fn spawn_server_with_args(
    bin: &Path,
    home: &Path,
    port: u16,
    log: &Path,
    extra: &[&str],
) -> Result<Child> {
    spawn_server_with_args_and_env(bin, home, port, log, extra, &[])
}

pub(super) fn spawn_server_with_args_and_env(
    bin: &Path,
    home: &Path,
    port: u16,
    log: &Path,
    extra: &[&str],
    environment: &[(&str, String)],
) -> Result<Child> {
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
    cmd.args(extra);
    cmd.envs(environment.iter().map(|(key, value)| (*key, value)));
    cmd.env("GENTS_OPENAI_CHAT_COMPLETIONS", "1");
    cmd.env("GENTS_REGISTRY_HEARTBEAT_MS", "1000");
    cmd.env("GENTS_PAIRING_SWEEP_MS", "1000");
    cmd.env("GENTS_REGISTRY_STALE_MS", "300000");
    cmd.env("GENTS_ENDPOINT_HEARTBEAT_MS", "1000");
    cmd.stdout(file).stderr(errfile).kill_on_drop(true);
    cmd.spawn().context("spawning demo server")
}

/// `/healthz` only proves that HTTP is up; gate commands on the authoritative
/// runtime behavior-readiness projection.
pub(super) async fn wait_runtime_ready(
    graphql: &str,
    agent_did: &str,
    server: &mut Child,
) -> Result<()> {
    let query = format!(
        r#"{{ AgentBehaviorReadiness(filter: {{ agent_did: {{ _eq: "{}" }} }}, limit: 1) {{ agent_did snapshot_json updated_at }} }}"#,
        escape_graphql_string(agent_did)
    );
    for _ in 0..360 {
        if let Ok(resp) = post_graphql(graphql, &query).await {
            if readiness_snapshot(&resp, agent_did).is_some_and(|snapshot| {
                snapshot.process_state.accepts_work()
                    && snapshot.active_generation > 0
                    && snapshot.router_generation == snapshot.active_generation
            }) {
                return Ok(());
            }
        }
        if let Ok(Some(status)) = server.try_wait() {
            bail!("demo server exited before its runtime became ready ({status})");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    bail!("timed out waiting for the demo runtime at {graphql} to become ready")
}

fn readiness_snapshot(
    response: &Value,
    expected_agent_did: &str,
) -> Option<BehaviorReadinessSnapshot> {
    let row = response
        .pointer("/data/AgentBehaviorReadiness/0")
        .cloned()
        .and_then(|row| serde_json::from_value::<AgentBehaviorReadinessRow>(row).ok())?;
    decode_behavior_readiness_snapshot(&row, expected_agent_did).ok()
}

pub(super) async fn wait_http(url: &str, server: &mut Child) -> Result<()> {
    let client = reqwest::Client::new();
    for _ in 0..600 {
        if client
            .get(url)
            .timeout(Duration::from_millis(500))
            .send()
            .await
            .map(|response| response.status().is_success())
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

pub(super) async fn desktop(fleet: &Fleet) -> Result<()> {
    let Some(desktop_bin) = resolve_desktop_bin() else {
        println!("  The desktop app (`gents-desktop`) was not found.");
        println!("  Install it, or set GENTS_DESKTOP_BIN, then run `desktop` again.");
        return Ok(());
    };
    let desktop_home = fleet.home_a.join("desktop");

    println!("  launching the desktop app…");
    let mut cmd = std::process::Command::new(&desktop_bin);
    cmd.env("GENTS_DESKTOP_HOME", path_arg(&desktop_home));
    cmd.spawn().context("launching the desktop app")?;

    println!("  ✓ Desktop app launched.");
    println!(
        "    In Fleet, enter {} and request authenticated enrollment.",
        fleet.graphql_a
    );
    println!("    Approve the pending request on this runtime before Chat becomes available.");
    Ok(())
}

fn resolve_desktop_bin() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("GENTS_DESKTOP_BIN") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(sibling) = exe.parent().map(|dir| dir.join("gents-desktop")) {
            if sibling.is_file() {
                return Some(sibling);
            }
        }
    }
    let name = "gents-desktop";
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}
