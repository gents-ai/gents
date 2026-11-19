mod http;
mod workspace_permissions;

pub use workspace_permissions::workspace_permission_support;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use gents_protocol::runtime_connection::RuntimeConnection;
use gents_protocol::serve_lifecycle::ObservedServeLifecycle;
use gents_store::client_journal::paths::DesktopPaths;
use gents_store::client_journal::runtime_directory::RuntimeDirectory;

use self::http::{http_get_json, read_json};

const INIT_CONFIG_FILE_NAME: &str = "init.json";
const RUNTIME_STATE_FILE_NAME: &str = gents_store::local::home::RUNTIME_STATE_FILE_NAME;
/// The recorded runtime endpoint may be stale or unresponsive.
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);
/// Startup reports failure if the runtime never reaches serving readiness.
const SERVE_READY_TIMEOUT: Duration = Duration::from_secs(60);
/// Space readiness probes while waiting for an external runtime to start.
const SERVE_READY_POLL: Duration = Duration::from_millis(500);

async fn runtime_http_client() -> Result<&'static reqwest::Client> {
    static CLIENT: tokio::sync::OnceCell<reqwest::Client> = tokio::sync::OnceCell::const_new();
    CLIENT
        .get_or_try_init(|| async {
            tokio::task::spawn_blocking(|| {
                reqwest::Client::builder()
                    .pool_idle_timeout(gents_protocol::graphql::POOLED_CONNECTION_IDLE_LIMIT)
                    .timeout(HTTP_TIMEOUT)
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
            })
            .await?
            .map_err(anyhow::Error::from)
        })
        .await
}

#[derive(Debug, Clone)]
pub struct DesktopInitOptions {
    pub node_home: PathBuf,
    pub desktop_paths: DesktopPaths,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct StandardRuntimeDiscovery {
    pub node_name: String,
    pub node_did: String,
    pub graphql: String,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DesktopInitSummary {
    pub status: &'static str,
    pub node_home: String,
    pub desktop_home: String,
    pub runtime_directory: String,
    pub label: String,
    pub node_name: String,
    pub node_did: String,
    pub graphql: String,
    pub next_steps: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct StoredInitConfig {
    node_name: String,
    node_did: String,
}

/// Document reads and edits preserve the already-loaded client principal. This
/// endpoint never opens the co-hosted runtime key or substitutes its authority.
pub fn operator_endpoint(
    record: &gents_protocol::runtime_connection::RuntimeConnection,
    capability: &gents_store::config_client::ClientSigningCapability,
) -> Option<gents_store::config_client::GraphqlEndpoint> {
    let url = record.graphql.as_str();
    gents_store::config_client::runtime_endpoints::RuntimeEndpoints::new(capability.clone())
        .endpoint(&record.node_did, url)
        .ok()
}

pub fn hosted_runtime_home(
    record: &gents_protocol::runtime_connection::RuntimeConnection,
) -> Result<&Path> {
    let endpoint = record.graphql.as_str();
    let home = record
        .local_node_home
        .as_deref()
        .map(str::trim)
        .filter(|home| !home.is_empty())
        .context("runtime record has no local Node home")?;
    let home = Path::new(home);
    let runtime = read_json::<StoredRuntimeState>(&home.join(RUNTIME_STATE_FILE_NAME))
        .context("reading the co-hosted runtime's state")?;
    anyhow::ensure!(
        runtime.graphql.trim() == endpoint && runtime.node_did.trim() == record.node_did,
        "runtime home {} does not serve {endpoint} for {}",
        home.display(),
        record.node_did
    );
    Ok(home)
}

#[derive(Debug, Deserialize)]
struct StoredRuntimeState {
    graphql: String,
    node_name: String,
    node_did: String,
}

pub fn default_node_home() -> Result<PathBuf> {
    Ok(gents_store::local::home::default_home_dir()?)
}

fn has_http_scheme(endpoint: &str) -> bool {
    endpoint.split_once("://").is_some_and(|(scheme, _)| {
        scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
    })
}

pub fn runtime_status_url(server_address: &str) -> Result<String> {
    let trimmed = server_address.trim();
    if trimmed.is_empty() {
        anyhow::bail!("server_address is required");
    }

    let with_scheme = if has_http_scheme(trimmed) {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    let mut url = reqwest::Url::parse(&with_scheme)
        .map_err(|error| anyhow::anyhow!("server_address is not a valid URL: {error}"))?;
    let path = url.path().trim_end_matches('/');
    if let Some(prefix) = path.strip_suffix("/api/v0/graphql") {
        url.set_path(&format!("{prefix}/status"));
    } else if path.is_empty() || path == "/" || path == "/api/v0" {
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

    let with_scheme = if has_http_scheme(trimmed) {
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

pub fn graphql_endpoint_for_desktop_access(status_url: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(status_url).ok()?;
    let base = url.path().strip_suffix("/status")?;
    url.set_path(&format!("{base}/api/v0/graphql"));
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}

pub fn normalize_runtime_connection_payload(mut payload: Value, status_url: &str) -> Value {
    if let Some(graphql) = graphql_endpoint_for_desktop_access(status_url) {
        if let Some(object) = payload.as_object_mut() {
            object.insert("graphql".into(), Value::String(graphql));
        }
    }
    payload
}

/// Why the User could not read a co-hosted runtime's private status. The read
/// never falls back to the runtime's node key or to an unsigned request, so
/// each cause is reported to the caller instead of being masked.
#[derive(Debug, thiserror::Error)]
pub enum HostedRuntimeStatusError {
    /// This desktop holds no User signing capability to present.
    #[error("runtime status is unavailable without this client's signing identity")]
    NoUserSigner,
    /// The runtime refused the User's signed status read.
    #[error("runtime {node_did} denied this client's status read (HTTP {status})")]
    Denied { node_did: String, status: u16 },
    #[error(fmt = gents_protocol::error_chain::held)]
    Failed(#[from] anyhow::Error),
}

/// Private status is read as the desktop's User principal through the
/// client-signed endpoint, as every other desktop read is (HttpAccess.lean
/// `readsAs`: the private part of `/status` admits the node's admin and reads
/// as its caller). The co-hosted runtime's node key is never loaded.
pub async fn fetch_hosted_runtime_status(
    record: &gents_protocol::runtime_connection::RuntimeConnection,
    user: Option<&gents_store::config_client::ClientSigningCapability>,
) -> Result<Option<Value>, HostedRuntimeStatusError> {
    if record.graphql.is_empty() {
        return Ok(None);
    }
    let user = user.ok_or(HostedRuntimeStatusError::NoUserSigner)?;
    let endpoint = operator_endpoint(record, user).ok_or(HostedRuntimeStatusError::NoUserSigner)?;
    match read_runtime_status(&endpoint, &record.node_did).await? {
        RuntimeStatusRead::Payload(payload) => Ok(Some(payload)),
        RuntimeStatusRead::Refused(status) => Err(HostedRuntimeStatusError::Denied {
            node_did: record.node_did.clone(),
            status: status.as_u16(),
        }),
    }
}

pub async fn hosted_status_signer(
    desktop: &DesktopPaths,
    running: Option<gents_store::config_client::ClientSigningCapability>,
) -> Option<gents_store::config_client::ClientSigningCapability> {
    if let Some(administrator) = load_status_signer(desktop.cli_client()).await {
        return Some(administrator);
    }
    if running.is_some() {
        return running;
    }
    load_status_signer(desktop.clone()).await
}

async fn load_status_signer(
    paths: DesktopPaths,
) -> Option<gents_store::config_client::ClientSigningCapability> {
    use gents_store::identity::NodeIdentity as _;
    match tokio::task::spawn_blocking(move || {
        gents_store::client_journal::principal_identity::PrincipalIdentity::load_existing(&paths)
            .and_then(|user| Ok(user.client_signing_capability()?))
    })
    .await
    {
        Ok(Ok(capability)) => Some(capability),
        Ok(Err(error)) => {
            tracing::debug!(error = %format!("{error:#}"), "no administering User key to read hosted runtime status");
            None
        }
        Err(error) => {
            tracing::warn!(error = %error, "loading the hosted runtime status signer failed");
            None
        }
    }
}

/// Observe the selected runtime using the caller's existing HTTP identity.
/// A response from another node cannot establish liveness for this target.
pub async fn fetch_runtime_status_for_node(
    endpoint: &gents_store::config_client::GraphqlEndpoint,
    node_did: &str,
) -> Result<Value> {
    match read_runtime_status(endpoint, node_did).await? {
        RuntimeStatusRead::Payload(payload) => Ok(payload),
        RuntimeStatusRead::Refused(status) => Err(anyhow::Error::from(
            gents_store::client_error::BridgeError::new(
                gents_store::client_error::BridgeErrorCode::PermissionDenied,
                format!("runtime status refused this client (HTTP {status})"),
            ),
        )),
    }
}

enum RuntimeStatusRead {
    Payload(Value),
    /// The runtime answered 401 or 403 to the caller's signed request.
    Refused(reqwest::StatusCode),
}

async fn read_runtime_status(
    endpoint: &gents_store::config_client::GraphqlEndpoint,
    node_did: &str,
) -> Result<RuntimeStatusRead> {
    anyhow::ensure!(
        !endpoint.is_anonymous(),
        "runtime status requires an authenticated caller"
    );
    let status_url = runtime_status_url(endpoint.url())?;
    let client = runtime_http_client().await?;
    let result = match endpoint
        .send_resolving(|endpoint| endpoint.authorize(client.get(&status_url)))
        .await
    {
        Ok(result) => result,
        Err(error) => {
            if let Some(status) = error
                .transport_error()
                .and_then(reqwest::Error::status)
                .filter(|status| {
                    matches!(
                        *status,
                        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
                    )
                })
            {
                return Ok(RuntimeStatusRead::Refused(status));
            }
            return Err(error.into());
        }
    };
    let response = result.map_err(|error| {
        gents_store::client_error::BridgeError::runtime_http_error(
            error,
            format!("sending runtime status request to {status_url}"),
        )
    })?;
    if matches!(
        response.status(),
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
    ) {
        let status = response.status();
        let body = response.bytes().await.unwrap_or_default();
        if let Some(update) = gents_store::client_error::BridgeError::home_refusal(status, &body) {
            return Err(update.into());
        }
        return Ok(RuntimeStatusRead::Refused(status));
    }
    let response = response.error_for_status()?;
    anyhow::ensure!(
        response.status().is_success(),
        "runtime status returned HTTP {}",
        response.status(),
    );
    let payload: Value = response.json().await.map_err(|error| {
        gents_store::client_error::BridgeError::runtime_http_error(
            error,
            "decoding authenticated runtime status".to_owned(),
        )
    })?;
    anyhow::ensure!(
        payload.get("node_did").and_then(Value::as_str) == Some(node_did),
        "runtime status belongs to a different node"
    );
    Ok(RuntimeStatusRead::Payload(
        normalize_runtime_connection_payload(payload, &status_url),
    ))
}

pub async fn fetch_runtime_connection_payload(endpoint: &str) -> Result<Value> {
    let client = runtime_http_client()
        .await
        .context("building runtime discovery HTTP client")?;
    fetch_runtime_connection_payload_with_client(client, endpoint).await
}

pub async fn fetch_runtime_connection_payload_with_client(
    client: &reqwest::Client,
    endpoint: &str,
) -> Result<Value> {
    let status_url = runtime_status_url(endpoint)?;
    let payload: Value = http_get_json(client, &status_url).await?;
    Ok(normalize_runtime_connection_payload(payload, &status_url))
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
    let node_data_dir = [paths.node_data_dir().to_path_buf()];
    let cleared = gents_store::local::home::retire_entries(
        &[gents_store::local::home::RetireGroup {
            name: "desktop",
            entries: &node_data_dir,
        }],
        gents_store::local::home::RetireDisposition::Delete,
    )
    .with_context(|| {
        format!(
            "clearing desktop runtime state {}",
            paths.node_data_dir().display()
        )
    })?;
    Ok(!cleared.is_empty())
}

pub async fn init_standard_local_runtime(
    options: DesktopInitOptions,
) -> Result<DesktopInitSummary> {
    options.desktop_paths.ensure_root_dirs().await?;
    let discovery = discover_standard_runtime(&options.node_home).await?;
    let directory = RuntimeDirectory::open(options.desktop_paths.runtime_directory_path()).await?;
    directory
        .upsert(RuntimeConnection {
            node_did: discovery.node_did.clone(),
            label: options.label.clone(),
            graphql: discovery.graphql.clone(),
            local_node_home: Some(options.node_home.display().to_string()),
            access_request: None,
            access_state: None,
        })
        .await?;
    Ok(DesktopInitSummary {
        status: "initialized",
        node_home: options.node_home.display().to_string(),
        desktop_home: options.desktop_paths.root().display().to_string(),
        runtime_directory: options
            .desktop_paths
            .runtime_directory_path()
            .display()
            .to_string(),
        label: options.label,
        node_name: discovery.node_name,
        node_did: discovery.node_did,
        graphql: discovery.graphql,
        next_steps: vec!["Open the application and request access to this runtime.".into()],
    })
}

pub async fn discover_standard_runtime(node_home: &Path) -> Result<StandardRuntimeDiscovery> {
    let init = read_json::<StoredInitConfig>(&node_home.join(INIT_CONFIG_FILE_NAME))?;
    let runtime = read_json::<StoredRuntimeState>(&node_home.join(RUNTIME_STATE_FILE_NAME))
        .with_context(|| {
            format!(
                "no running local Gents runtime found at {}; run `gents runtime server` first",
                node_home.join(RUNTIME_STATE_FILE_NAME).display()
            )
        })?;

    validate_runtime_identity(&runtime, &init)?;

    let client = runtime_http_client()
        .await
        .context("building local runtime HTTP client")?;
    await_serving_runtime(client, &runtime).await?;
    Ok(StandardRuntimeDiscovery {
        node_name: runtime.node_name,
        node_did: runtime.node_did,
        graphql: runtime.graphql,
    })
}

/// Discovery proceeds only once the live `/status` reports this identity
/// ready; this is the one place discovery binds the saved route to the live DID.
async fn await_serving_runtime(
    client: &reqwest::Client,
    runtime: &StoredRuntimeState,
) -> Result<()> {
    await_serving_runtime_within(client, runtime, SERVE_READY_TIMEOUT).await
}

async fn await_serving_runtime_within(
    client: &reqwest::Client,
    runtime: &StoredRuntimeState,
    timeout: Duration,
) -> Result<()> {
    let status_url = runtime_status_url(&runtime.graphql)?;
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let waiting = match http_get_json::<Value>(client, &status_url).await {
            Ok(status) => match serving_runtime(&status, &runtime.node_did)? {
                ObservedServeLifecycle::Ready => return Ok(()),
                ObservedServeLifecycle::Outdated { version } => {
                    anyhow::bail!(
                        "{}",
                        gents_protocol::serve_lifecycle::outdated_runtime_message(
                            version.as_deref()
                        )
                    )
                }
                ObservedServeLifecycle::Starting => anyhow::anyhow!(
                    "the local Gents runtime at {status_url} did not finish starting within {} seconds",
                    timeout.as_secs()
                ),
            },
            Err(error) => error.context(format!(
                "no local Gents runtime answered at {status_url} within {} seconds",
                timeout.as_secs()
            )),
        };
        if tokio::time::Instant::now() >= deadline {
            return Err(waiting);
        }
        tokio::time::sleep(SERVE_READY_POLL).await;
    }
}

fn serving_runtime(status: &Value, expected_did: &str) -> Result<ObservedServeLifecycle> {
    if !well_formed_did(expected_did) {
        anyhow::bail!("this home has no usable initialized node DID ({expected_did:?})");
    }
    let live_did = status
        .get("node_did")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !well_formed_did(live_did) || live_did != expected_did {
        anyhow::bail!(
            "the runtime answering for this home serves {live_did:?}, not {expected_did}"
        );
    }
    Ok(ObservedServeLifecycle::observe(status))
}

fn well_formed_did(did: &str) -> bool {
    did.strip_prefix("did:")
        .is_some_and(|rest| !rest.is_empty() && !did.chars().any(char::is_whitespace))
}

pub fn render_human_summary(summary: &DesktopInitSummary) -> String {
    format!("Runtime connection saved\nNode: {}\nGraphQL: {}\nClient directory: {}\nOpen the application to request access.\n", summary.node_did, summary.graphql, summary.runtime_directory)
}

fn validate_runtime_identity(runtime: &StoredRuntimeState, init: &StoredInitConfig) -> Result<()> {
    if !well_formed_did(&init.node_did) || runtime.node_did != init.node_did {
        anyhow::bail!(
            "runtime node DID {:?} does not match initialized node DID {:?}",
            runtime.node_did,
            init.node_did
        );
    }
    if runtime.node_name != init.node_name {
        anyhow::bail!(
            "runtime node name {} does not match initialized node name {}",
            runtime.node_name,
            init.node_name
        );
    }

    Ok(())
}
