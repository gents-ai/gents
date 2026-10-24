mod goal_control;
mod http;
mod identity;
mod request_input;
mod workspace_permissions;

pub(crate) use goal_control::ensure_goal_stop_supported;
pub(crate) use request_input::ensure_request_input_supported;
pub use workspace_permissions::workspace_permission_support;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gents_protocol::error_chain::ErrorChain;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use ts_rs::TS;

use crate::client::{initialize_local_standard_peer, DesktopPaths};
use gents_protocol::serve_lifecycle::ObservedServeLifecycle;

#[derive(Debug, thiserror::Error)]
pub enum LocalRuntimeError {
    #[error(transparent)]
    HttpClientTask(#[from] tokio::task::JoinError),
    #[error(transparent)]
    HttpClient(#[from] reqwest::Error),
    #[error(transparent)]
    Paths(#[from] crate::client::PathsError),
    #[error(transparent)]
    PeerDirectory(#[from] crate::client::PeerDirectoryError),
    #[error(transparent)]
    Identity(#[from] gents::identity::IdentityError),
    #[error(transparent)]
    Home(#[from] gents::home::HomeError),
    #[error("runtime identity {did} has no exportable signing key")]
    NoExportableSigningKey { did: String },
    #[error("runtime record is not a runtime this desktop hosts")]
    NotHostedRuntime,
    #[error("runtime record has no local Node home")]
    NoLocalNodeHome,
    #[error("reading the co-hosted runtime's state")]
    ReadRuntimeState(#[source] Box<LocalRuntimeError>),
    #[error("runtime home {} does not serve {endpoint} for {node_did}", home.display())]
    HomeNotServing {
        home: PathBuf,
        endpoint: String,
        node_did: String,
    },
    #[error("runtime home identity does not match the recorded node")]
    HomeIdentityMismatch,
    #[error("local runtime keychain identity has no label")]
    KeychainLabelMissing,
    #[error("local runtime Secure Enclave identity has no label")]
    SecureEnclaveLabelMissing,
    #[error("local runtime {} has no key path and unsupported identity backend {backend:?}", home.display())]
    UnsupportedIdentityBackend {
        home: PathBuf,
        backend: Option<String>,
    },
    #[error("local runtime identity does not match configured node DID")]
    ConfiguredDidMismatch,
    #[error("server_address is required")]
    ServerAddressRequired,
    #[error("server_address is not a valid URL: {error}")]
    InvalidServerAddress { error: String },
    #[error("graphql endpoint is required")]
    GraphqlEndpointRequired,
    #[error("graphql endpoint is not a valid URL: {error}")]
    InvalidGraphqlEndpoint { error: String },
    #[error("expected GraphQL endpoint ending in /graphql, got {endpoint}")]
    NotGraphqlEndpoint { endpoint: String },
    #[error("building runtime discovery HTTP client")]
    DiscoveryClient(#[source] Box<LocalRuntimeError>),
    #[error("refusing to dangerously overwrite {}", path.display())]
    RefuseOverwrite { path: PathBuf },
    #[error("refusing to dangerously overwrite the user home directory {}; pass a dedicated desktop home instead", path.display())]
    RefuseOverwriteUserHome { path: PathBuf },
    #[error("dangerously overwriting {}", path.display())]
    OverwriteDesktopHome {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("clearing desktop runtime state {}", path.display())]
    ClearDesktopRuntime {
        path: PathBuf,
        #[source]
        source: gents::home::HomeError,
    },
    #[error("no running local Gents runtime found at {}; run `gents server` first", path.display())]
    NoRunningRuntime {
        path: PathBuf,
        #[source]
        source: Box<LocalRuntimeError>,
    },
    #[error("building local runtime HTTP client")]
    LocalRuntimeClient(#[source] Box<LocalRuntimeError>),
    #[error("local runtime is reachable but did not report a shareable P2P address")]
    NoShareableAddress,
    #[error("local runtime is reachable but did not report a usable P2P peer id")]
    NoUsablePeerId,
    #[error("{}", gents_protocol::serve_lifecycle::outdated_runtime_message(.version.as_deref()))]
    RuntimeOutdated { version: Option<String> },
    #[error("the local Gents runtime at {url} did not finish starting within {seconds} seconds")]
    StillStarting { url: String, seconds: u64 },
    #[error("no local Gents runtime answered at {url} within {seconds} seconds")]
    NoRuntimeAnswer {
        url: String,
        seconds: u64,
        #[source]
        source: Box<LocalRuntimeError>,
    },
    #[error("this home has no usable initialized node DID ({did:?})")]
    UnusableHomeDid { did: String },
    #[error("the runtime answering for this home serves {live_did:?}, not {expected_did}")]
    RuntimeDidMismatch {
        live_did: String,
        expected_did: String,
    },
    #[error("reading P2P shareable address from {api_base}")]
    ReadShareableAddress {
        api_base: String,
        #[source]
        source: Box<LocalRuntimeError>,
    },
    #[error("runtime GraphQL endpoint is reachable but did not report a shareable P2P address")]
    NoGraphqlShareableAddress,
    #[error("runtime reported a shareable P2P address but no usable peer id")]
    NoGraphqlPeerId,
    #[error("sending GraphQL identity query to {graphql}")]
    SendGraphqlIdentity {
        graphql: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("reading GraphQL identity response from {graphql}")]
    ReadGraphqlIdentity {
        graphql: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("GraphQL identity query to {graphql} failed with {status}: {body}")]
    GraphqlIdentityStatus {
        graphql: String,
        status: reqwest::StatusCode,
        body: String,
    },
    #[error("decoding GraphQL identity response from {graphql}")]
    DecodeGraphqlIdentity {
        graphql: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("GraphQL identity query returned errors: {errors}")]
    GraphqlIdentityErrors { errors: serde_json::Value },
    #[error("GraphQL identity query returned no data")]
    GraphqlIdentityNoData,
    #[error("GraphQL identity query returned no Node rows")]
    GraphqlIdentityNoRows,
    #[error("GraphQL identity query must return exactly one enabled Node, found {found}")]
    GraphqlIdentityNodeCount { found: usize },
    #[error("enabled Node has no node_did")]
    EnabledNodeMissingDid,
    #[error("enabled Node has no display_name")]
    EnabledNodeMissingName,
    #[error("local runtime uses p2p_transport={transport}; desktop pairing requires iroh. Restart with `gents server` from a current build.")]
    UnsupportedP2pTransport { transport: String },
    #[error("runtime node DID {runtime:?} does not match initialized node DID {initialized:?}")]
    NodeDidMismatch {
        runtime: String,
        initialized: String,
    },
    #[error("runtime node name {runtime} does not match initialized node name {initialized}")]
    NodeNameMismatch {
        runtime: String,
        initialized: String,
    },
    #[error("reading {}", path.display())]
    ReadJsonFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("decoding {}", path.display())]
    DecodeJsonFile {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("no gents server found at {url}. Start one first with `gents server`. Remote runtimes must be added through authenticated status enrollment in the desktop app.")]
    NoServerFound {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("sending GET request to {url}")]
    SendGet {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("reading GET response body from {url}")]
    ReadGetBody {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("GET {url} failed with {status}: {body}")]
    GetStatus {
        url: String,
        status: reqwest::StatusCode,
        body: String,
    },
    #[error("decoding JSON response from {url}")]
    DecodeGetJson {
        url: String,
        #[source]
        source: serde_json::Error,
    },
    #[error(
        "Cannot verify goal assignment controls. Reconnect this runtime through its HTTP address."
    )]
    GoalControlUnverifiable,
    #[error("The runtime is not ready. Reconnect it before {action}.")]
    RuntimeNotReady { action: &'static str },
    #[error("The extension and runtime use different goal controls. Update them together, then reconnect.")]
    GoalControlContractMismatch,
    #[error(
        "Cannot verify request input support. Reconnect this runtime through its HTTP address."
    )]
    RequestInputUnverifiable,
    #[error("The attached context is invalid. Remove it and capture it again.")]
    InvalidAttachedContext,
    #[error(
        "This runtime does not support this attachment input. Update the runtime and reconnect."
    )]
    InputContractUnsupported,
    #[error(transparent)]
    Attachment(#[from] gents_protocol::request_input::AttachmentError),
    #[error("The runtime is still starting.")]
    WorkspaceRuntimeStarting,
    #[error("The runtime returned invalid workspace permission capabilities.")]
    InvalidWorkspacePermissions(#[source] serde_json::Error),
}

type Result<T, E = LocalRuntimeError> = std::result::Result<T, E>;

use self::http::{http_get_json, p2p_api_base, read_json};
use self::identity::{normalize_optional_string, resolve_p2p_peer_id};

const INIT_CONFIG_FILE_NAME: &str = "init.json";
const RUNTIME_STATE_FILE_NAME: &str = gents::home::RUNTIME_STATE_FILE_NAME;
const LOCAL_STANDARD_SOURCE: &str = "local-standard";
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);
const SERVE_READY_TIMEOUT: Duration = Duration::from_secs(60);
const SERVE_READY_POLL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, Default)]
pub enum OperatorIdentityInteraction {
    #[default]
    Allow,
    Prevent,
}

impl OperatorIdentityInteraction {
    pub(crate) fn endpoint(
        self,
        record: &crate::client::PeerRecord,
    ) -> Option<gents::config_client::GraphqlEndpoint> {
        match self {
            Self::Allow => operator_endpoint(record),
            Self::Prevent => cached_operator_endpoint(record),
        }
    }
}

async fn runtime_http_client() -> Result<&'static reqwest::Client> {
    static CLIENT: tokio::sync::OnceCell<reqwest::Client> = tokio::sync::OnceCell::const_new();
    CLIENT
        .get_or_try_init(|| async {
            tokio::task::spawn_blocking(|| reqwest::Client::builder().timeout(HTTP_TIMEOUT).build())
                .await?
                .map_err(LocalRuntimeError::from)
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
pub(crate) struct StandardRuntimeDiscovery {
    pub node_name: String,
    pub node_did: String,
    pub graphql: String,
    pub p2p_peer_id: String,
    pub p2p_listen_address: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DesktopInitSummary {
    pub status: &'static str,
    pub source: &'static str,
    pub node_home: String,
    pub desktop_home: String,
    pub peer_directory: String,
    pub label: String,
    pub node_name: String,
    pub node_did: String,
    pub graphql: String,
    pub p2p_transport: String,
    pub p2p_peer_id: String,
    pub p2p_listen_address: String,
    pub peer_record_id: String,
    pub next_steps: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct StoredInitConfig {
    node_name: String,
    node_did: String,
    #[serde(default)]
    key_path: Option<String>,
    #[serde(default)]
    identity_backend: Option<String>,
    #[serde(default)]
    keychain_label: Option<String>,
    #[serde(default)]
    secure_enclave_label: Option<String>,
}

/// The operator GraphQL endpoint of a co-hosted runtime, acting as that
/// runtime's node identity.
///
/// The runtime's node access control admits operator writes only from its own
/// node DID. The desktop is that node's same-user host, so it loads the key
/// from the recorded runtime home. When the key cannot be loaded the endpoint
/// is anonymous: DefraDB applies its configured read policy, and operator
/// writes fail with the node's authorization error.
pub fn operator_endpoint(
    record: &crate::client::PeerRecord,
) -> Option<gents::config_client::GraphqlEndpoint> {
    let url = record.operator_graphql()?;
    Some(match load_operator_node_identity(record) {
        Ok(()) => gents::config_client::GraphqlEndpoint::as_principal(url, &record.node_did),
        Err(error) => {
            tracing::warn!(
                node_did = %record.node_did,
                error = %ErrorChain(&error),
                "runtime node identity key unavailable; using anonymous operator access"
            );
            gents::config_client::GraphqlEndpoint::anonymous(url)
        }
    })
}

/// Background observation must not unlock a runtime identity. Cached authority
/// remains bound to the endpoint served by that identity's recorded home;
/// otherwise DefraDB receives an anonymous request and enforces its read policy.
pub fn cached_operator_endpoint(
    record: &crate::client::PeerRecord,
) -> Option<gents::config_client::GraphqlEndpoint> {
    let url = record.operator_graphql()?;
    Some(
        if gents::identity::can_mint_defradb_bearer(&record.node_did)
            && hosted_runtime_home(record).is_ok()
        {
            gents::config_client::GraphqlEndpoint::as_principal(url, &record.node_did)
        } else {
            gents::config_client::GraphqlEndpoint::anonymous(url)
        },
    )
}

/// Only a record for a runtime this desktop hosts qualifies, and only when
/// its home's live `runtime.json` names both the record's endpoint and node,
/// so a bearer is never sent to a remote or re-pointed endpoint.
fn load_operator_node_identity(record: &crate::client::PeerRecord) -> Result<()> {
    if gents::identity::can_mint_defradb_bearer(&record.node_did) {
        return hosted_runtime_home(record).map(drop);
    }
    let identity = operator_signer(record)?;
    if !gents::identity::can_mint_defradb_bearer(identity.did()) {
        return Err(LocalRuntimeError::NoExportableSigningKey {
            did: identity.did().to_owned(),
        });
    }
    Ok(())
}

pub fn hosted_runtime_home(record: &crate::client::PeerRecord) -> Result<&Path> {
    let endpoint = record
        .operator_graphql()
        .ok_or(LocalRuntimeError::NotHostedRuntime)?;
    let home = record
        .local_node_home
        .as_deref()
        .map(str::trim)
        .filter(|home| !home.is_empty())
        .ok_or(LocalRuntimeError::NoLocalNodeHome)?;
    let home = Path::new(home);
    let runtime = read_json::<StoredRuntimeState>(&home.join(RUNTIME_STATE_FILE_NAME))
        .map_err(|source| LocalRuntimeError::ReadRuntimeState(Box::new(source)))?;
    if runtime.graphql.trim() != endpoint || runtime.node_did.trim() != record.node_did {
        return Err(LocalRuntimeError::HomeNotServing {
            home: home.to_path_buf(),
            endpoint: endpoint.to_owned(),
            node_did: record.node_did.clone(),
        });
    }
    Ok(home)
}

/// Signs the operator commands the co-hosted runtime's HTTP routes take.
pub fn operator_signer(
    record: &crate::client::PeerRecord,
) -> Result<Arc<dyn gents::identity::NodeIdentity>> {
    let identity = load_standard_runtime_identity(hosted_runtime_home(record)?)?;
    if identity.did() != record.node_did {
        return Err(LocalRuntimeError::HomeIdentityMismatch);
    }
    Ok(identity)
}

pub(crate) fn load_standard_runtime_identity(
    node_home: &Path,
) -> Result<Arc<dyn gents::identity::NodeIdentity>> {
    use gents::identity::{
        load_macos_keychain_identity, load_macos_secure_enclave_identity, KeyIdentity,
    };
    let config = read_json::<StoredInitConfig>(&node_home.join(INIT_CONFIG_FILE_NAME))?;
    let identity: Arc<dyn gents::identity::NodeIdentity> = if let Some(path) = config
        .key_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Arc::new(KeyIdentity::load_existing(Path::new(path), None)?)
    } else {
        match config.identity_backend.as_deref().map(str::trim) {
            Some("macos-keychain") => Arc::new(load_macos_keychain_identity(
                config
                    .keychain_label
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or(LocalRuntimeError::KeychainLabelMissing)?,
                None,
            )?),
            Some("macos-secure-enclave") => Arc::new(load_macos_secure_enclave_identity(
                config
                    .secure_enclave_label
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or(LocalRuntimeError::SecureEnclaveLabelMissing)?,
                None,
            )?),
            backend => {
                return Err(LocalRuntimeError::UnsupportedIdentityBackend {
                    home: node_home.to_path_buf(),
                    backend: backend.map(str::to_owned),
                })
            }
        }
    };
    if identity.did() != config.node_did.trim() {
        return Err(LocalRuntimeError::ConfiguredDidMismatch);
    }
    Ok(identity)
}

#[derive(Debug, Deserialize)]
struct StoredRuntimeState {
    graphql: String,
    node_name: String,
    node_did: String,
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

pub fn default_node_home() -> Result<PathBuf> {
    Ok(gents::home::default_home_dir()?)
}

pub fn runtime_status_url(server_address: &str) -> Result<String> {
    let trimmed = server_address.trim();
    if trimmed.is_empty() {
        return Err(LocalRuntimeError::ServerAddressRequired);
    }

    let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    let mut url = reqwest::Url::parse(&with_scheme).map_err(|error| {
        LocalRuntimeError::InvalidServerAddress {
            error: error.to_string(),
        }
    })?;
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
        return Err(LocalRuntimeError::GraphqlEndpointRequired);
    }

    let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    let mut url = reqwest::Url::parse(&with_scheme).map_err(|error| {
        LocalRuntimeError::InvalidGraphqlEndpoint {
            error: error.to_string(),
        }
    })?;
    let path = url.path().trim_end_matches('/').to_string();
    if !path.ends_with("/graphql") {
        return Err(LocalRuntimeError::NotGraphqlEndpoint {
            endpoint: endpoint.to_owned(),
        });
    }
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
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
    let client = runtime_http_client()
        .await
        .map_err(|source| LocalRuntimeError::DiscoveryClient(Box::new(source)))?;
    fetch_runtime_connection_payload_with_client(client, endpoint).await
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
        return Err(LocalRuntimeError::RefuseOverwrite {
            path: desktop_root.to_path_buf(),
        });
    }
    if let Some(user_home) = std::env::var_os("HOME").map(PathBuf::from) {
        if desktop_root == user_home {
            return Err(LocalRuntimeError::RefuseOverwriteUserHome {
                path: desktop_root.to_path_buf(),
            });
        }
    }

    std::fs::remove_dir_all(desktop_root).map_err(|source| {
        LocalRuntimeError::OverwriteDesktopHome {
            path: desktop_root.to_path_buf(),
            source,
        }
    })?;
    Ok(())
}

pub fn reset_desktop_runtime_state(paths: &DesktopPaths) -> Result<bool> {
    let node_data_dir = [paths.node_data_dir().to_path_buf()];
    let cleared = gents::home::retire_entries(
        &[gents::home::RetireGroup {
            name: "desktop",
            entries: &node_data_dir,
        }],
        gents::home::RetireDisposition::Delete,
    )
    .map_err(|source| LocalRuntimeError::ClearDesktopRuntime {
        path: paths.node_data_dir().to_path_buf(),
        source,
    })?;
    Ok(!cleared.is_empty())
}

pub async fn init_standard_local_runtime(
    options: DesktopInitOptions,
) -> Result<DesktopInitSummary> {
    options.desktop_paths.ensure_root_dirs().await?;
    let discovery = discover_standard_runtime(&options.node_home).await?;

    let peer = initialize_local_standard_peer(
        options.desktop_paths.peer_directory_path(),
        &options.label,
        &discovery.p2p_listen_address,
        &discovery.node_did,
        &discovery.graphql,
        &options.node_home.display().to_string(),
    )
    .await?;

    Ok(DesktopInitSummary {
        status: "initialized",
        source: LOCAL_STANDARD_SOURCE,
        node_home: options.node_home.display().to_string(),
        desktop_home: options.desktop_paths.root().display().to_string(),
        peer_directory: options
            .desktop_paths
            .peer_directory_path()
            .display()
            .to_string(),
        label: peer.label,
        node_name: discovery.node_name,
        node_did: discovery.node_did,
        graphql: discovery.graphql,
        p2p_transport: "iroh".to_string(),
        p2p_peer_id: discovery.p2p_peer_id,
        p2p_listen_address: discovery.p2p_listen_address,
        peer_record_id: peer.peer_id,
        next_steps: vec![
            "Run `gents-desktop` and leave the desktop app open.".to_string(),
            "Wait for the status bar to show `replication subscriptions armed`.".to_string(),
            "Then submit prompts from Chat, or run `gents chat` in another terminal.".to_string(),
        ],
    })
}

pub(crate) async fn discover_standard_runtime(
    node_home: &Path,
) -> Result<StandardRuntimeDiscovery> {
    let init = read_json::<StoredInitConfig>(&node_home.join(INIT_CONFIG_FILE_NAME))?;
    let runtime = read_json::<StoredRuntimeState>(&node_home.join(RUNTIME_STATE_FILE_NAME))
        .map_err(|source| LocalRuntimeError::NoRunningRuntime {
            path: node_home.join(RUNTIME_STATE_FILE_NAME),
            source: Box::new(source),
        })?;

    validate_runtime_identity(&runtime, &init)?;

    let client = runtime_http_client()
        .await
        .map_err(|source| LocalRuntimeError::LocalRuntimeClient(Box::new(source)))?;
    await_serving_runtime(client, &runtime).await?;
    let api_base = p2p_api_base(&runtime.graphql)?;
    let shareable_address: ShareableAddressResponse =
        http_get_json(client, &format!("{api_base}/p2p/shareable-address")).await?;
    let p2p_listen_address = normalize_optional_string(shareable_address.address.as_deref())
        .ok_or(LocalRuntimeError::NoShareableAddress)?;
    let live_identity =
        http_get_json::<NodeIdentityResponse>(client, &format!("{api_base}/node/identity"))
            .await
            .ok();
    let p2p_peer_id = resolve_p2p_peer_id(
        live_identity
            .as_ref()
            .and_then(|identity| identity.peer_id.as_deref()),
        Some(&p2p_listen_address),
        runtime.p2p_peer_id.as_deref(),
    )
    .ok_or(LocalRuntimeError::NoUsablePeerId)?;

    Ok(StandardRuntimeDiscovery {
        node_name: runtime.node_name,
        node_did: runtime.node_did,
        graphql: runtime.graphql,
        p2p_peer_id,
        p2p_listen_address,
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
                    return Err(LocalRuntimeError::RuntimeOutdated { version });
                }
                ObservedServeLifecycle::Starting => LocalRuntimeError::StillStarting {
                    url: status_url.clone(),
                    seconds: timeout.as_secs(),
                },
            },
            Err(source) => LocalRuntimeError::NoRuntimeAnswer {
                url: status_url.clone(),
                seconds: timeout.as_secs(),
                source: Box::new(source),
            },
        };
        if tokio::time::Instant::now() >= deadline {
            return Err(waiting);
        }
        tokio::time::sleep(SERVE_READY_POLL).await;
    }
}

fn serving_runtime(status: &Value, expected_did: &str) -> Result<ObservedServeLifecycle> {
    if !well_formed_did(expected_did) {
        return Err(LocalRuntimeError::UnusableHomeDid {
            did: expected_did.to_owned(),
        });
    }
    let live_did = status
        .get("node_did")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !well_formed_did(live_did) || live_did != expected_did {
        return Err(LocalRuntimeError::RuntimeDidMismatch {
            live_did: live_did.to_owned(),
            expected_did: expected_did.to_owned(),
        });
    }
    Ok(ObservedServeLifecycle::observe(status))
}

fn well_formed_did(did: &str) -> bool {
    did.strip_prefix("did:")
        .is_some_and(|rest| !rest.is_empty() && !did.chars().any(char::is_whitespace))
}

pub fn render_human_summary(summary: &DesktopInitSummary) -> String {
    let discovery_line = format!(
        "Discovered local Gents runtime: {node_home}",
        node_home = summary.node_home
    );

    format!(
        "\
gents-desktop init complete
{discovery_line}
GraphQL reachable: {graphql}
Node DID: {node_did}
P2P transport: {p2p_transport}
P2P peer ID: {p2p_peer_id}
P2P listen address: {p2p_listen_address}
Saved desktop deployment: {label}
Desktop data dir: {desktop_home}
Peer directory: {peer_directory}

Note: init saves the discovered runtime. The desktop app completes P2P pairing
and replication bootstrap on launch.

Next:
  1. Run `gents-desktop` and leave it open.
  2. Wait for the status bar to show `replication subscriptions armed`.
  3. Then submit prompts from Chat, or run `gents chat` in another terminal.
",
        discovery_line = discovery_line,
        graphql = summary.graphql,
        node_did = summary.node_did,
        p2p_transport = summary.p2p_transport,
        p2p_peer_id = summary.p2p_peer_id,
        p2p_listen_address = summary.p2p_listen_address,
        label = summary.label,
        desktop_home = summary.desktop_home,
        peer_directory = summary.peer_directory,
    )
}

async fn fetch_graphql_runtime_connection_payload(
    client: &reqwest::Client,
    graphql: &str,
) -> Result<Value> {
    let api_base = p2p_api_base(graphql)?;
    let shareable_address: ShareableAddressResponse =
        http_get_json(client, &format!("{api_base}/p2p/shareable-address"))
            .await
            .map_err(|source| LocalRuntimeError::ReadShareableAddress {
                api_base: api_base.clone(),
                source: Box::new(source),
            })?;
    let p2p_listen_address = normalize_optional_string(shareable_address.address.as_deref())
        .ok_or(LocalRuntimeError::NoGraphqlShareableAddress)?;
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
    .ok_or(LocalRuntimeError::NoGraphqlPeerId)?;
    let (node_did, node_name) = fetch_graphql_node_identity(client, graphql).await?;

    Ok(json!({
        "status": "ok",
        "service": "gents",
        "graphql": graphql,
        "desktop_graphql": graphql,
        "node_name": node_name,
        "node_did": node_did,
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

async fn fetch_graphql_node_identity(
    client: &reqwest::Client,
    graphql: &str,
) -> Result<(String, String)> {
    let query = r#"
        query DesktopRuntimeIdentity {
            Node { node_did display_name enabled }
        }
    "#;
    let response = client
        .post(graphql)
        .json(&json!({ "query": query }))
        .send()
        .await
        .map_err(|source| LocalRuntimeError::SendGraphqlIdentity {
            graphql: graphql.to_owned(),
            source,
        })?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|source| LocalRuntimeError::ReadGraphqlIdentity {
            graphql: graphql.to_owned(),
            source,
        })?;
    if !status.is_success() {
        return Err(LocalRuntimeError::GraphqlIdentityStatus {
            graphql: graphql.to_owned(),
            status,
            body: String::from_utf8_lossy(&body).into_owned(),
        });
    }
    let response: GraphqlResponse = serde_json::from_slice(&body).map_err(|source| {
        LocalRuntimeError::DecodeGraphqlIdentity {
            graphql: graphql.to_owned(),
            source,
        }
    })?;
    if let Some(errors) = response
        .errors
        .as_ref()
        .filter(|errors| !errors_is_empty(errors))
    {
        return Err(LocalRuntimeError::GraphqlIdentityErrors {
            errors: errors.clone(),
        });
    }
    let data = response
        .data
        .ok_or(LocalRuntimeError::GraphqlIdentityNoData)?;
    let nodes = data
        .get("Node")
        .and_then(Value::as_array)
        .ok_or(LocalRuntimeError::GraphqlIdentityNoRows)?;
    let enabled = nodes
        .iter()
        .filter(|row| row.get("enabled").and_then(Value::as_bool) == Some(true))
        .collect::<Vec<_>>();
    let [node] = enabled.as_slice() else {
        return Err(LocalRuntimeError::GraphqlIdentityNodeCount {
            found: enabled.len(),
        });
    };
    let node_did = string_at(node, "/node_did")
        .ok_or(LocalRuntimeError::EnabledNodeMissingDid)?
        .to_string();
    let node_name = string_at(node, "/display_name")
        .ok_or(LocalRuntimeError::EnabledNodeMissingName)?
        .to_string();

    Ok((node_did, node_name))
}

fn errors_is_empty(errors: &Value) -> bool {
    match errors {
        Value::Null => true,
        Value::Array(errors) => errors.is_empty(),
        _ => false,
    }
}

fn string_at<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
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
        return Err(LocalRuntimeError::UnsupportedP2pTransport {
            transport: if runtime.p2p_transport.is_empty() {
                "<empty>".to_owned()
            } else {
                runtime.p2p_transport.clone()
            },
        });
    }
    if !well_formed_did(&init.node_did) || runtime.node_did != init.node_did {
        return Err(LocalRuntimeError::NodeDidMismatch {
            runtime: runtime.node_did.clone(),
            initialized: init.node_did.clone(),
        });
    }
    if runtime.node_name != init.node_name {
        return Err(LocalRuntimeError::NodeNameMismatch {
            runtime: runtime.node_name.clone(),
            initialized: init.node_name.clone(),
        });
    }

    Ok(())
}
