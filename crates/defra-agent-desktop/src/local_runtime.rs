use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::client::{DesktopPaths, PeerDirectory};

const INIT_CONFIG_FILE_NAME: &str = "init.json";
const RUNTIME_STATE_FILE_NAME: &str = "runtime.json";
const LOCAL_STANDARD_SOURCE: &str = "local-standard";

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
    #[serde(default)]
    p2p_listen_addresses: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct NodeIdentityResponse {
    #[serde(default)]
    peer_id: Option<String>,
}

pub fn default_agent_home() -> Result<PathBuf> {
    let home = dirs::home_dir().context("unable to resolve home directory")?;
    Ok(home.join(".defra-agent"))
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

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("building local runtime HTTP client")?;
    let api_base = p2p_api_base(&runtime.graphql)?;
    let identity: NodeIdentityResponse =
        http_get_json(&client, &format!("{api_base}/node/identity")).await?;
    let live_listen_addresses: Vec<String> =
        http_get_json(&client, &format!("{api_base}/p2p/info")).await?;
    let p2p_peer_id = identity
        .peer_id
        .or_else(|| runtime.p2p_peer_id.clone())
        .filter(|value| !value.trim().is_empty())
        .context("local runtime is reachable but did not report a P2P peer id")?;
    let p2p_listen_address = live_listen_addresses
        .into_iter()
        .chain(runtime.p2p_listen_addresses.iter().cloned())
        .find(|value| !value.trim().is_empty())
        .context("local runtime is reachable but did not report a P2P listen address")?;

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
            "Wait for the status bar to show `replication: subscriptions armed`.".to_string(),
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
  2. Wait for the status bar to show `replication: subscriptions armed`.
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

pub(crate) async fn complete_runtime_pairing(
    graphql: &str,
    desktop_listen_address: &str,
    collections: Vec<String>,
) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("building local runtime pairing HTTP client")?;
    let api_base = p2p_api_base(graphql)?;
    http_post_json(
        &client,
        &format!("{api_base}/p2p/connect"),
        &vec![desktop_listen_address.to_string()],
    )
    .await?;
    http_post_json(
        &client,
        &format!("{api_base}/p2p/collections"),
        &collections,
    )
    .await?;
    http_post_json(
        &client,
        &format!("{api_base}/p2p/replicators"),
        &P2pReplicatorRequest {
            addresses: vec![desktop_listen_address.to_string()],
            collections,
        },
    )
    .await
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("decoding {}", path.display()))
}

fn p2p_api_base(graphql: &str) -> Result<String> {
    graphql
        .trim()
        .strip_suffix("/graphql")
        .map(ToOwned::to_owned)
        .with_context(|| format!("expected GraphQL endpoint ending in /graphql, got {graphql}"))
}

async fn http_get_json<T: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    url: &str,
) -> Result<T> {
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("sending GET request to {url}"))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .with_context(|| format!("reading GET response body from {url}"))?;
    if !status.is_success() {
        anyhow::bail!(
            "GET {url} failed with {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    serde_json::from_slice(&body).with_context(|| format!("decoding JSON response from {url}"))
}

async fn http_post_json<B: Serialize>(client: &reqwest::Client, url: &str, body: &B) -> Result<()> {
    let response = client
        .post(url)
        .json(body)
        .send()
        .await
        .with_context(|| format!("sending POST request to {url}"))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("reading POST response body from {url}"))?;
    if !status.is_success() {
        anyhow::bail!(
            "POST {url} failed with {status}: {}",
            String::from_utf8_lossy(&bytes)
        );
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct P2pReplicatorRequest {
    collections: Vec<String>,
    addresses: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_summary() -> DesktopInitSummary {
        DesktopInitSummary {
            status: "initialized",
            source: LOCAL_STANDARD_SOURCE,
            agent_home: "/tmp/agent".to_string(),
            desktop_home: "/tmp/desktop".to_string(),
            peer_directory: "/tmp/desktop/peers.json".to_string(),
            label: "Local Agent".to_string(),
            agent_name: "default".to_string(),
            agent_did: "did:defra-agent:default".to_string(),
            graphql: "http://127.0.0.1:9191/graphql".to_string(),
            p2p_transport: "iroh".to_string(),
            p2p_peer_id: "peer-runtime".to_string(),
            p2p_listen_address: "iroh://peer-runtime".to_string(),
            peer_record_id: "peer-runtime".to_string(),
            next_steps: vec![
                "Run `defra-agent-desktop` and leave the desktop app open.".to_string(),
                "Wait for the status bar to show `replication: subscriptions armed`.".to_string(),
                "Then submit prompts from Chat, or run `defra-agent chat` in another terminal."
                    .to_string(),
            ],
        }
    }

    #[test]
    fn init_summary_tells_demo_to_wait_for_desktop_bootstrap() {
        let summary = sample_summary();
        assert!(summary
            .next_steps
            .iter()
            .any(|step| step.contains("replication: subscriptions armed")));

        let rendered = render_human_summary(&summary);
        assert!(rendered.contains("desktop app completes P2P pairing"));
        assert!(rendered.contains("replication: subscriptions armed"));
        assert!(rendered.contains("Then submit prompts from Chat"));
    }
}
