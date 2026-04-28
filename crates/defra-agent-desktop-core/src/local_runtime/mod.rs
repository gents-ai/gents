mod http;
mod identity;
mod pairing;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::client::{DesktopPaths, PeerDirectory};

use self::http::{http_get_json, p2p_api_base, read_json};
use self::identity::{normalize_optional_string, resolve_p2p_peer_id};

const INIT_CONFIG_FILE_NAME: &str = "init.json";
const RUNTIME_STATE_FILE_NAME: &str = "runtime.json";
const LOCAL_STANDARD_SOURCE: &str = "local-standard";
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct DesktopInitOptions {
    pub agent_home: PathBuf,
    pub desktop_paths: DesktopPaths,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DesktopInitSummary {
    pub status: &'static str,
    pub source: &'static str,
    pub agent_home: String,
    pub desktop_home: String,
    pub peer_directory: String,
    pub label: String,
    pub agent_name: String,
    pub agent_did: String,
    pub graphql: String,
    pub p2p_transport: String,
    pub p2p_peer_id: String,
    pub p2p_listen_address: String,
    pub peer_record_id: String,
    pub next_steps: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct StoredInitConfig {
    agent_name: String,
    agent_did: String,
}

#[derive(Debug, Deserialize)]
struct StoredRuntimeState {
    graphql: String,
    agent_name: String,
    agent_did: String,
    #[serde(default)]
    p2p_transport: String,
    #[serde(default)]
    p2p_peer_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NodeIdentityResponse {
    #[serde(default)]
    peer_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ShareableAddressResponse {
    #[serde(default)]
    address: Option<String>,
}

pub fn default_agent_home() -> Result<PathBuf> {
    let home = dirs::home_dir().context("unable to resolve home directory")?;
    Ok(home.join(".defra-agent"))
}

pub fn dangerously_overwrite_desktop_home(desktop_root: &Path) -> Result<()> {
    if !desktop_root.exists() {
        return Ok(());
    }

    if desktop_root.as_os_str().is_empty() || desktop_root == Path::new("/") {
        anyhow::bail!(
            "refusing to dangerously overwrite {}",
            desktop_root.display()
        );
    }
    if let Some(user_home) = std::env::var_os("HOME").map(PathBuf::from) {
        if desktop_root == user_home {
            anyhow::bail!(
                "refusing to dangerously overwrite the user home directory {}; pass a dedicated desktop home instead",
                desktop_root.display()
            );
        }
    }

    std::fs::remove_dir_all(desktop_root)
        .with_context(|| format!("dangerously overwriting {}", desktop_root.display()))?;
    Ok(())
}

pub fn reset_desktop_runtime_state(paths: &DesktopPaths) -> Result<bool> {
    let node_data_dir = paths.node_data_dir();
    if !node_data_dir.exists() {
        return Ok(false);
    }

    std::fs::remove_dir_all(node_data_dir)
        .with_context(|| format!("clearing desktop runtime state {}", node_data_dir.display()))?;
    Ok(true)
}

pub async fn init_standard_local_runtime(
    options: DesktopInitOptions,
) -> Result<DesktopInitSummary> {
    options.desktop_paths.ensure_root_dirs().await?;
    let init = read_json::<StoredInitConfig>(&options.agent_home.join(INIT_CONFIG_FILE_NAME))?;
    let runtime =
        read_json::<StoredRuntimeState>(&options.agent_home.join(RUNTIME_STATE_FILE_NAME))
            .with_context(|| {
                format!(
            "no running local defra-agent runtime found at {}; run `defra-agent server` first",
            options.agent_home.join(RUNTIME_STATE_FILE_NAME).display()
        )
            })?;

    validate_runtime_identity(&runtime, &init)?;

    let client = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .context("building local runtime HTTP client")?;
    let api_base = p2p_api_base(&runtime.graphql)?;
    let shareable_address: ShareableAddressResponse =
        http_get_json(&client, &format!("{api_base}/p2p/shareable-address")).await?;
    let p2p_listen_address = normalize_optional_string(shareable_address.address.as_deref())
        .context("local runtime is reachable but did not report a shareable P2P address")?;
    let live_identity =
        http_get_json::<NodeIdentityResponse>(&client, &format!("{api_base}/node/identity"))
            .await
            .ok();
    let p2p_peer_id = resolve_p2p_peer_id(
        live_identity
            .as_ref()
            .and_then(|identity| identity.peer_id.as_deref()),
        Some(&p2p_listen_address),
        runtime.p2p_peer_id.as_deref(),
    )
    .context("local runtime is reachable but did not report a usable P2P peer id")?;

    let mut peer_directory =
        PeerDirectory::load(options.desktop_paths.peer_directory_path()).await?;
    let peer = peer_directory
        .upsert_local_standard_peer(
            &options.label,
            &p2p_listen_address,
            &runtime.agent_did,
            &runtime.graphql,
        )
        .await?;

    Ok(DesktopInitSummary {
        status: "initialized",
        source: LOCAL_STANDARD_SOURCE,
        agent_home: options.agent_home.display().to_string(),
        desktop_home: options.desktop_paths.root().display().to_string(),
        peer_directory: options
            .desktop_paths
            .peer_directory_path()
            .display()
            .to_string(),
        label: peer.label,
        agent_name: runtime.agent_name,
        agent_did: runtime.agent_did,
        graphql: runtime.graphql,
        p2p_transport: "iroh".to_string(),
        p2p_peer_id,
        p2p_listen_address,
        peer_record_id: peer.peer_id,
        next_steps: vec![
            "Run `defra-agent-desktop` and leave the desktop app open.".to_string(),
            "Wait for the status bar to show `replication subscriptions armed`.".to_string(),
            "Then submit prompts from Chat, or run `defra-agent chat` in another terminal."
                .to_string(),
        ],
    })
}

pub fn render_human_summary(summary: &DesktopInitSummary) -> String {
    format!(
        "\
defra-agent-desktop init complete
Discovered local defra-agent runtime: {agent_home}
GraphQL reachable: {graphql}
Agent DID: {agent_did}
P2P transport: {p2p_transport}
P2P peer ID: {p2p_peer_id}
P2P listen address: {p2p_listen_address}
Saved desktop deployment: {label}
Desktop data dir: {desktop_home}
Peer directory: {peer_directory}

Note: init saves the discovered runtime. The desktop app completes P2P pairing
and replication bootstrap on launch.

Next:
  1. Run `defra-agent-desktop` and leave it open.
  2. Wait for the status bar to show `replication subscriptions armed`.
  3. Then submit prompts from Chat, or run `defra-agent chat` in another terminal.
",
        agent_home = summary.agent_home,
        graphql = summary.graphql,
        agent_did = summary.agent_did,
        p2p_transport = summary.p2p_transport,
        p2p_peer_id = summary.p2p_peer_id,
        p2p_listen_address = summary.p2p_listen_address,
        label = summary.label,
        desktop_home = summary.desktop_home,
        peer_directory = summary.peer_directory,
    )
}

pub(crate) use pairing::complete_runtime_pairing;

fn validate_runtime_identity(runtime: &StoredRuntimeState, init: &StoredInitConfig) -> Result<()> {
    if runtime.p2p_transport != "iroh" {
        anyhow::bail!(
            "local runtime uses p2p_transport={}; desktop pairing requires iroh. Restart with `defra-agent server` from a current build.",
            if runtime.p2p_transport.is_empty() {
                "<empty>"
            } else {
                runtime.p2p_transport.as_str()
            }
        );
    }
    if runtime.agent_did != init.agent_did {
        anyhow::bail!(
            "runtime agent DID {} does not match initialized agent DID {}",
            runtime.agent_did,
            init.agent_did
        );
    }
    if runtime.agent_name != init.agent_name {
        anyhow::bail!(
            "runtime agent name {} does not match initialized agent name {}",
            runtime.agent_name,
            init.agent_name
        );
    }

    Ok(())
}
