mod http;
mod identity;
mod pairing;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::client::{DesktopPaths, PeerDirectory};

use self::http::{http_get_json, p2p_api_base, read_json};
use self::identity::{normalize_optional_string, resolve_p2p_peer_id};

const INIT_CONFIG_FILE_NAME: &str = "init.json";
const RUNTIME_STATE_FILE_NAME: &str = "runtime.json";
const LOCAL_STANDARD_SOURCE: &str = "local-standard";
const SERVER_STATUS_SOURCE: &str = "server-status";
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct DesktopInitOptions {
    pub agent_home: PathBuf,
    pub desktop_paths: DesktopPaths,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct StatusEndpointInitOptions {
    pub desktop_paths: DesktopPaths,
    pub status_endpoint: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DesktopInitSummary {
    pub status: &'static str,
    pub source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_endpoint: Option<String>,
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

#[derive(Debug, Deserialize)]
struct GraphqlResponse {
    #[serde(default)]
    data: Option<Value>,
    #[serde(default)]
    errors: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StatusEndpointConnection {
    label: String,
    agent_name: String,
    agent_did: String,
    graphql: String,
    p2p_transport: String,
    p2p_peer_id: String,
    p2p_listen_address: String,
}

pub fn default_agent_home() -> Result<PathBuf> {
    let home = dirs::home_dir().context("unable to resolve home directory")?;
    Ok(home.join(".defra-agent"))
}

pub fn runtime_status_url(server_address: &str) -> Result<String> {
    let trimmed = server_address.trim();
    if trimmed.is_empty() {
        anyhow::bail!("server_address is required");
    }

    let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    let mut url = reqwest::Url::parse(&with_scheme)
        .map_err(|error| anyhow::anyhow!("server_address is not a valid URL: {error}"))?;
    let path = url.path().trim_end_matches('/');
    if path.is_empty() || path == "/" || path == "/api/v0" || path == "/api/v0/graphql" {
        url.set_path("/status");
    } else if !path.ends_with("/status") {
        url.set_path(&format!("{path}/status"));
    }
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

pub fn runtime_graphql_url(endpoint: &str) -> Result<String> {
    let trimmed = endpoint.trim();
    if trimmed.is_empty() {
        anyhow::bail!("graphql endpoint is required");
    }

    let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    let mut url = reqwest::Url::parse(&with_scheme)
        .map_err(|error| anyhow::anyhow!("graphql endpoint is not a valid URL: {error}"))?;
    let path = url.path().trim_end_matches('/').to_string();
    if !path.ends_with("/graphql") {
        anyhow::bail!("expected GraphQL endpoint ending in /graphql, got {endpoint}");
    }
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

pub fn runtime_discovery_url(endpoint: &str) -> Result<String> {
    runtime_graphql_url(endpoint).or_else(|_| runtime_status_url(endpoint))
}

pub fn graphql_endpoint_for_desktop_access(
    status_payload: &Value,
    status_url: &str,
) -> Option<String> {
    let status_url = reqwest::Url::parse(status_url).ok()?;
    let raw_graphql = string_at(status_payload, "/desktop_graphql")
        .or_else(|| string_at(status_payload, "/graphql"))
        .or_else(|| string_at(status_payload, "/checks/graphql/endpoint"))
        .or_else(|| string_at(status_payload, "/runtime_state/graphql"));

    if let Some(raw_graphql) = raw_graphql {
        let mut graphql_url = reqwest::Url::parse(raw_graphql).ok()?;
        if url_host_is_loopback_or_unspecified(&graphql_url)
            && !url_host_is_loopback_or_unspecified(&status_url)
        {
            let _ = graphql_url.set_scheme(status_url.scheme());
            if graphql_url.set_host(status_url.host_str()).is_err() {
                return Some(raw_graphql.to_string());
            }
            if graphql_url.set_port(status_url.port()).is_err() {
                return Some(raw_graphql.to_string());
            }
        }
        return Some(graphql_url.to_string());
    }

    let mut graphql_url = status_url;
    graphql_url.set_path("/api/v0/graphql");
    graphql_url.set_query(None);
    graphql_url.set_fragment(None);
    Some(graphql_url.to_string())
}

pub fn augment_peer_status_payload_for_desktop(mut payload: Value, status_url: &str) -> Value {
    let desktop_graphql = graphql_endpoint_for_desktop_access(&payload, status_url);
    if let (Some(map), Some(desktop_graphql)) = (payload.as_object_mut(), desktop_graphql) {
        map.entry("desktop_graphql")
            .or_insert_with(|| Value::String(desktop_graphql));
    }
    payload
}

pub async fn fetch_runtime_connection_payload(endpoint: &str) -> Result<Value> {
    let client = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .context("building runtime discovery HTTP client")?;
    fetch_runtime_connection_payload_with_client(&client, endpoint).await
}

pub async fn fetch_runtime_connection_payload_with_client(
    client: &reqwest::Client,
    endpoint: &str,
) -> Result<Value> {
    if let Ok(graphql) = runtime_graphql_url(endpoint) {
        return fetch_graphql_runtime_connection_payload(client, &graphql).await;
    }

    let status_url = runtime_status_url(endpoint)?;
    let payload: Value = http_get_json(client, &status_url).await?;
    Ok(augment_peer_status_payload_for_desktop(
        payload,
        &status_url,
    ))
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
        status_endpoint: None,
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

pub async fn init_status_endpoint_runtime(
    options: StatusEndpointInitOptions,
) -> Result<DesktopInitSummary> {
    options.desktop_paths.ensure_root_dirs().await?;
    let client = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .context("building status endpoint HTTP client")?;
    let discovery_url = runtime_discovery_url(&options.status_endpoint)?;
    let payload = fetch_runtime_connection_payload_with_client(&client, &options.status_endpoint)
        .await
        .with_context(|| format!("discovering runtime from {}", options.status_endpoint))?;
    let connection =
        extract_status_endpoint_connection(&payload, &discovery_url, options.label.as_deref())?;

    let mut peer_directory =
        PeerDirectory::load(options.desktop_paths.peer_directory_path()).await?;
    let peer = peer_directory
        .upsert_saved_peer_with_graphql(
            &connection.label,
            &connection.p2p_listen_address,
            &connection.agent_did,
            Some(&connection.graphql),
        )
        .await?;

    Ok(DesktopInitSummary {
        status: "initialized",
        source: SERVER_STATUS_SOURCE,
        status_endpoint: Some(discovery_url),
        agent_home: String::new(),
        desktop_home: options.desktop_paths.root().display().to_string(),
        peer_directory: options
            .desktop_paths
            .peer_directory_path()
            .display()
            .to_string(),
        label: peer.label,
        agent_name: connection.agent_name,
        agent_did: connection.agent_did,
        graphql: connection.graphql,
        p2p_transport: connection.p2p_transport,
        p2p_peer_id: connection.p2p_peer_id,
        p2p_listen_address: connection.p2p_listen_address,
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
    let discovery_line = match summary.status_endpoint.as_deref() {
        Some(status_endpoint) => {
            format!("Discovered defra-agent runtime from discovery endpoint: {status_endpoint}")
        }
        None => format!(
            "Discovered local defra-agent runtime: {agent_home}",
            agent_home = summary.agent_home
        ),
    };

    format!(
        "\
defra-agent-desktop init complete
{discovery_line}
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
        discovery_line = discovery_line,
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

async fn fetch_graphql_runtime_connection_payload(
    client: &reqwest::Client,
    graphql: &str,
) -> Result<Value> {
    let api_base = p2p_api_base(graphql)?;
    let shareable_address: ShareableAddressResponse =
        http_get_json(client, &format!("{api_base}/p2p/shareable-address"))
            .await
            .with_context(|| format!("reading P2P shareable address from {api_base}"))?;
    let p2p_listen_address = normalize_optional_string(shareable_address.address.as_deref())
        .context(
            "runtime GraphQL endpoint is reachable but did not report a shareable P2P address",
        )?;
    let live_identity =
        http_get_json::<NodeIdentityResponse>(client, &format!("{api_base}/node/identity"))
            .await
            .ok();
    let p2p_peer_id = resolve_p2p_peer_id(
        live_identity
            .as_ref()
            .and_then(|identity| identity.peer_id.as_deref()),
        Some(&p2p_listen_address),
        None,
    )
    .context("runtime reported a shareable P2P address but no usable peer id")?;
    let (agent_did, agent_name) = fetch_graphql_agent_identity(client, graphql).await?;

    Ok(json!({
        "status": "ok",
        "service": "defra-agent",
        "graphql": graphql,
        "desktop_graphql": graphql,
        "agent_name": agent_name,
        "agent_did": agent_did,
        "p2p": {
            "enabled": true,
            "p2p_transport": "iroh",
            "p2p_peer_id": p2p_peer_id,
            "p2p_shareable_address": p2p_listen_address,
        },
        "p2p_enabled": true,
        "p2p_transport": "iroh",
        "p2p_peer_id": p2p_peer_id,
        "p2p_shareable_address": p2p_listen_address,
    }))
}

async fn fetch_graphql_agent_identity(
    client: &reqwest::Client,
    graphql: &str,
) -> Result<(String, String)> {
    let query = r#"
        query DesktopRuntimeIdentity {
            AgentPrincipal { agent_did display_name enabled }
            AgentRuntime { agent_did }
        }
    "#;
    let response = client
        .post(graphql)
        .json(&json!({ "query": query }))
        .send()
        .await
        .with_context(|| format!("sending GraphQL identity query to {graphql}"))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .with_context(|| format!("reading GraphQL identity response from {graphql}"))?;
    if !status.is_success() {
        anyhow::bail!(
            "GraphQL identity query to {graphql} failed with {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    let response: GraphqlResponse = serde_json::from_slice(&body)
        .with_context(|| format!("decoding GraphQL identity response from {graphql}"))?;
    if let Some(errors) = response
        .errors
        .as_ref()
        .filter(|errors| !errors_is_empty(errors))
    {
        anyhow::bail!("GraphQL identity query returned errors: {errors}");
    }
    let data = response
        .data
        .context("GraphQL identity query returned no data")?;
    let principal = data
        .get("AgentPrincipal")
        .and_then(Value::as_array)
        .and_then(|rows| {
            rows.iter()
                .find(|row| row.get("enabled").and_then(Value::as_bool) != Some(false))
                .or_else(|| rows.first())
        });
    let runtime = data
        .get("AgentRuntime")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first());
    let agent_did = principal
        .and_then(|row| string_at(row, "/agent_did"))
        .or_else(|| runtime.and_then(|row| string_at(row, "/agent_did")))
        .context("GraphQL identity query returned no agent_did")?
        .to_string();
    let agent_name = principal
        .and_then(|row| string_at(row, "/display_name"))
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| fallback_agent_name(&agent_did));

    Ok((agent_did, agent_name))
}

fn errors_is_empty(errors: &Value) -> bool {
    match errors {
        Value::Null => true,
        Value::Array(errors) => errors.is_empty(),
        _ => false,
    }
}

fn extract_status_endpoint_connection(
    payload: &Value,
    status_url: &str,
    label: Option<&str>,
) -> Result<StatusEndpointConnection> {
    let agent_did = string_at(payload, "/agent_did")
        .or_else(|| string_at(payload, "/runtime_state/agent_did"))
        .or_else(|| string_at(payload, "/runtime/agent_did"))
        .context("status response did not include agent_did")?
        .to_string();
    let agent_name = string_at(payload, "/agent_name")
        .or_else(|| string_at(payload, "/runtime_state/agent_name"))
        .or_else(|| string_at(payload, "/runtime/agent_name"))
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| fallback_agent_name(&agent_did));
    let graphql = string_at(payload, "/desktop_graphql")
        .map(ToOwned::to_owned)
        .or_else(|| graphql_endpoint_for_desktop_access(payload, status_url))
        .context("status response did not include a usable GraphQL endpoint")?;
    p2p_api_base(&graphql).with_context(|| {
        format!("status response GraphQL endpoint is not usable for P2P pairing: {graphql}")
    })?;

    let p2p_listen_address = string_at(payload, "/p2p_shareable_address")
        .or_else(|| string_at(payload, "/p2p/p2p_shareable_address"))
        .or_else(|| string_at(payload, "/runtime_state/p2p_shareable_address"))
        .or_else(|| first_string_at(payload, "/p2p_listen_addresses"))
        .or_else(|| first_string_at(payload, "/p2p/p2p_listen_addresses"))
        .or_else(|| first_string_at(payload, "/runtime_state/p2p_listen_addresses"))
        .context("status response did not include a shareable P2P address")?
        .to_string();
    let p2p_peer_id = string_at(payload, "/p2p_peer_id")
        .or_else(|| string_at(payload, "/p2p/p2p_peer_id"))
        .or_else(|| string_at(payload, "/runtime_state/p2p_peer_id"))
        .map(ToOwned::to_owned)
        .or_else(|| resolve_p2p_peer_id(None, Some(&p2p_listen_address), None))
        .context("status response did not include a usable P2P peer id")?;
    let p2p_transport = string_at(payload, "/p2p_transport")
        .or_else(|| string_at(payload, "/p2p/p2p_transport"))
        .or_else(|| string_at(payload, "/runtime_state/p2p_transport"))
        .unwrap_or("iroh")
        .to_string();
    if p2p_transport != "iroh" {
        anyhow::bail!(
            "status response uses p2p_transport={p2p_transport}; desktop pairing requires iroh"
        );
    }
    let label = normalize_optional_string(label).unwrap_or_else(|| agent_name.clone());

    Ok(StatusEndpointConnection {
        label,
        agent_name,
        agent_did,
        graphql,
        p2p_transport,
        p2p_peer_id,
        p2p_listen_address,
    })
}

fn string_at<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn first_string_at<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .find(|value| !value.is_empty())
        })
}

fn fallback_agent_name(agent_did: &str) -> String {
    agent_did
        .rsplit(':')
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or(agent_did)
        .to_string()
}

fn url_host_is_loopback_or_unspecified(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .map(|addr| addr.is_loopback() || addr.is_unspecified())
        .unwrap_or(false)
}

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
