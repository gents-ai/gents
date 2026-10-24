use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gents::config_client::GraphqlEndpoint;
use gents::identity::{
    load_macos_keychain_identity, load_macos_secure_enclave_identity, IdentityError, KeyIdentity,
    NodeIdentity,
};
use gents_protocol::error_chain::ErrorChain;

use crate::native_service::AtomicWriteError;
use crate::shared::{StoredInitConfig, StoredRuntimeState};
use crate::{DEFAULT_HTTP_PORT, RUNTIME_STATE_FILE_NAME};

#[derive(Debug, thiserror::Error)]
pub(crate) enum HomeStateError {
    #[error(transparent)]
    Home(#[from] gents::home::HomeError),
    #[error("initialized home {} has no node DID", home_dir.display())]
    NoNodeDid { home_dir: PathBuf },
    #[error("loading identity key {}", path.display())]
    LoadIdentityKey {
        path: PathBuf,
        #[source]
        source: IdentityError,
    },
    #[error("initialized home node DID {expected_did} requires identity key {} to already exist", path.display())]
    IdentityKeyMissing { expected_did: String, path: PathBuf },
    #[error("initialized home {} uses macos-keychain but has no keychain_label", home_dir.display())]
    KeychainLabelMissing { home_dir: PathBuf },
    #[error("loading macOS keychain identity {label}")]
    LoadKeychainIdentity {
        label: String,
        #[source]
        source: IdentityError,
    },
    #[error("initialized home {} uses macos-secure-enclave but has no secure_enclave_label", home_dir.display())]
    SecureEnclaveLabelMissing { home_dir: PathBuf },
    #[error("loading macOS Secure Enclave identity {label}")]
    LoadSecureEnclaveIdentity {
        label: String,
        #[source]
        source: IdentityError,
    },
    #[error("initialized home {} has no key_path and unsupported identity_backend {backend:?}", home_dir.display())]
    UnsupportedIdentityBackend {
        home_dir: PathBuf,
        backend: Option<String>,
    },
    #[error(
        "initialized home node DID {expected_did} does not match loaded identity DID {loaded_did}"
    )]
    IdentityDidMismatch {
        expected_did: String,
        loaded_did: String,
    },
    #[error("creating home directory {}", path.display())]
    CreateHomeDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("encoding local runtime state JSON")]
    EncodeRuntimeState(#[source] serde_json::Error),
    #[error("writing runtime state {}", path.display())]
    WriteRuntimeState {
        path: PathBuf,
        #[source]
        source: AtomicWriteError,
    },
    #[error("reading runtime state {}", path.display())]
    ReadRuntimeState {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("decoding runtime state {}", path.display())]
    DecodeRuntimeState {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("removing stale runtime state {}", path.display())]
    RemoveRuntimeState {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("home {} has no running server recorded; start `gents server --home {}` or pass --graphql explicitly", home_dir.display(), home_dir.display())]
    NoRunningServer { home_dir: PathBuf },
    #[error("node DID is required; run `gents init`, start `gents server`, then retry `gents status`, or pass --node-did explicitly")]
    NodeDidRequired,
}

type Result<T, E = HomeStateError> = std::result::Result<T, E>;

pub(crate) fn resolve_home_dir(explicit: Option<&Path>) -> PathBuf {
    explicit
        .map(Path::to_path_buf)
        .unwrap_or_else(default_home_dir)
}

fn default_home_dir() -> PathBuf {
    gents::home::default_home_dir().unwrap_or_else(|_| PathBuf::from(".gents"))
}

pub(crate) fn is_default_home(home_dir: &Path) -> bool {
    let canonical = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical(home_dir) == canonical(&default_home_dir())
}

pub(crate) fn default_data_dir(home_dir: &Path) -> PathBuf {
    gents::home::default_data_dir(home_dir)
}

pub(crate) fn default_key_path(home_dir: &Path, node_name: &str) -> PathBuf {
    gents::home::default_key_path(home_dir, node_name)
}

pub(crate) fn init_config_path(home_dir: &Path) -> PathBuf {
    gents::home::init_config_path(home_dir)
}

pub(crate) fn runtime_state_path(home_dir: &Path) -> PathBuf {
    home_dir.join(RUNTIME_STATE_FILE_NAME)
}

pub(crate) fn write_init_config(home_dir: &Path, state: &StoredInitConfig) -> Result<()> {
    Ok(gents::home::write_init_config(home_dir, state)?)
}

pub(crate) fn read_init_config(home_dir: &Path) -> Result<Option<StoredInitConfig>> {
    Ok(gents::home::read_init_config(home_dir)?)
}

/// Load and register the signer recorded by an initialized home; every
/// embedded-node entry point uses this same loader so opening the data
/// directory outside `gents server` never silently produces unsigned commits.
pub(crate) fn load_initialized_home_identity(
    home_dir: &Path,
    config: &StoredInitConfig,
) -> Result<Arc<dyn NodeIdentity>> {
    let expected_did = config.node_did.trim();
    if expected_did.is_empty() {
        return Err(HomeStateError::NoNodeDid {
            home_dir: home_dir.to_path_buf(),
        });
    }

    let identity: Arc<dyn NodeIdentity> = if let Some(key_path) = config
        .key_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let key_path = PathBuf::from(key_path);
        if !key_path.exists() {
            return Err(HomeStateError::IdentityKeyMissing {
                expected_did: expected_did.to_owned(),
                path: key_path,
            });
        }
        Arc::new(
            KeyIdentity::load_existing(&key_path, None).map_err(|source| {
                HomeStateError::LoadIdentityKey {
                    path: key_path.clone(),
                    source,
                }
            })?,
        )
    } else {
        match config
            .identity_backend
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some("macos-keychain") => {
                let label = config
                    .keychain_label
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| HomeStateError::KeychainLabelMissing {
                        home_dir: home_dir.to_path_buf(),
                    })?;
                Arc::new(load_macos_keychain_identity(label, None).map_err(|source| {
                    HomeStateError::LoadKeychainIdentity {
                        label: label.to_owned(),
                        source,
                    }
                })?)
            }
            Some("macos-secure-enclave") => {
                let label = config
                    .secure_enclave_label
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| HomeStateError::SecureEnclaveLabelMissing {
                        home_dir: home_dir.to_path_buf(),
                    })?;
                Arc::new(
                    load_macos_secure_enclave_identity(label, None).map_err(|source| {
                        HomeStateError::LoadSecureEnclaveIdentity {
                            label: label.to_owned(),
                            source,
                        }
                    })?,
                )
            }
            backend => {
                return Err(HomeStateError::UnsupportedIdentityBackend {
                    home_dir: home_dir.to_path_buf(),
                    backend: backend.map(str::to_owned),
                })
            }
        }
    };

    if identity.did() != expected_did {
        return Err(HomeStateError::IdentityDidMismatch {
            expected_did: expected_did.to_owned(),
            loaded_did: identity.did().to_owned(),
        });
    }
    Ok(identity)
}

pub(crate) fn write_runtime_state(home_dir: &Path, state: &StoredRuntimeState) -> Result<()> {
    fs::create_dir_all(home_dir).map_err(|source| HomeStateError::CreateHomeDirectory {
        path: home_dir.to_path_buf(),
        source,
    })?;
    let path = runtime_state_path(home_dir);
    let contents = serde_json::to_vec_pretty(state).map_err(HomeStateError::EncodeRuntimeState)?;
    // Replaced, never truncated in place: a server killed mid-write would
    // otherwise leave an empty file every later command fails to decode.
    crate::native_service::atomic_write(&path, &contents).map_err(|source| {
        HomeStateError::WriteRuntimeState {
            path: path.clone(),
            source,
        }
    })
}

pub(crate) fn read_runtime_state(home_dir: &Path) -> Result<Option<StoredRuntimeState>> {
    let path = runtime_state_path(home_dir);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path).map_err(|source| HomeStateError::ReadRuntimeState {
        path: path.clone(),
        source,
    })?;
    let state =
        serde_json::from_slice(&bytes).map_err(|source| HomeStateError::DecodeRuntimeState {
            path: path.clone(),
            source,
        })?;
    Ok(Some(state))
}

pub(crate) fn clear_runtime_state(home_dir: &Path) -> Result<bool> {
    let path = runtime_state_path(home_dir);
    if path.exists() {
        fs::remove_file(&path).map_err(|source| HomeStateError::RemoveRuntimeState {
            path: path.clone(),
            source,
        })?;
        return Ok(true);
    }
    Ok(false)
}

pub(crate) fn resolve_graphql_endpoint(
    explicit: Option<&str>,
    home: Option<&Path>,
) -> Result<GraphqlEndpoint> {
    let home_dir = resolve_home_dir(home);
    if let Some(graphql) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(home_graphql_endpoint(&home_dir, graphql));
    }

    if let Some(runtime_state) = read_runtime_state(&home_dir)? {
        return Ok(home_graphql_endpoint(&home_dir, runtime_state.graphql));
    }

    if home.is_some() {
        return Err(HomeStateError::NoRunningServer { home_dir });
    }

    Ok(home_graphql_endpoint(
        &home_dir,
        format!("http://127.0.0.1:{DEFAULT_HTTP_PORT}/api/v0/graphql"),
    ))
}

/// `url` acts as the home's principal only when it is this home's own runtime
/// endpoint or a loopback address and this process can load the principal's
/// signing key; otherwise anonymous. Bearers carry no nonce and their
/// audience is the sender-chosen `Host`, so they are refused for any other
/// host, where they could be replayed against any trusting node until expiry.
pub(crate) fn home_graphql_endpoint(home_dir: &Path, url: impl Into<String>) -> GraphqlEndpoint {
    let url = url.into();
    let principal = match read_init_config(home_dir) {
        Ok(Some(config)) if endpoint_serves_home(home_dir, &url) => {
            load_initialized_home_identity(home_dir, &config)
                .map(|identity| identity.did().to_string())
                .map_err(|error| ErrorChain(&error).to_string())
                .and_then(|did| {
                    gents::identity::can_mint_defradb_bearer(&did)
                        .then_some(did)
                        .ok_or_else(|| "identity has no exportable signing key".to_string())
                })
        }
        _ => Err("the endpoint is not this initialized home's runtime".to_string()),
    };
    match principal {
        Ok(did) => GraphqlEndpoint::as_principal(url, did),
        Err(reason) => {
            tracing::debug!(%url, %reason, "reaching the runtime anonymously");
            GraphqlEndpoint::anonymous(url)
        }
    }
}

fn endpoint_serves_home(home_dir: &Path, url: &str) -> bool {
    let url = url.trim();
    if read_runtime_state(home_dir)
        .ok()
        .flatten()
        .is_some_and(|state| state.graphql.trim() == url)
    {
        return true;
    }
    reqwest::Url::parse(url).is_ok_and(|parsed| match parsed.host() {
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    })
}

pub(crate) fn resolve_node_did(home: Option<&Path>, explicit: Option<&str>) -> Result<String> {
    if let Some(node_did) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(node_did.to_string());
    }

    let home_dir = resolve_home_dir(home);
    if let Some(runtime_state) = read_runtime_state(&home_dir)? {
        return Ok(runtime_state.node_did);
    }
    if let Some(init_config) = read_init_config(&home_dir)? {
        return Ok(init_config.node_did);
    }

    Err(HomeStateError::NodeDidRequired)
}

pub(crate) fn display_host(host: IpAddr) -> String {
    match host {
        IpAddr::V4(addr) if addr == Ipv4Addr::UNSPECIFIED => "127.0.0.1".to_string(),
        _ => host.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::endpoint_serves_home;

    #[test]
    fn bearers_go_only_to_the_homes_runtime_or_loopback() {
        let home = tempfile::tempdir().unwrap();
        assert!(endpoint_serves_home(
            home.path(),
            "http://127.0.0.1:9191/api/v0/graphql"
        ));
        assert!(endpoint_serves_home(
            home.path(),
            "http://localhost:9191/api/v0/graphql"
        ));
        assert!(endpoint_serves_home(
            home.path(),
            "http://[::1]:9191/api/v0/graphql"
        ));
        assert!(!endpoint_serves_home(
            home.path(),
            "http://100.69.4.79:9191/api/v0/graphql"
        ));
        assert!(!endpoint_serves_home(
            home.path(),
            "https://runtime.example.com/api/v0/graphql"
        ));

        std::fs::write(
            home.path().join(crate::RUNTIME_STATE_FILE_NAME),
            serde_json::to_vec(&serde_json::json!({
                "home": home.path(),
                "graphql": "http://100.69.4.79:9191/api/v0/graphql",
                "node_name": "a",
                "node_did": "did:key:z6Mk",
                "default_agent_id": "b",
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(endpoint_serves_home(
            home.path(),
            "http://100.69.4.79:9191/api/v0/graphql"
        ));
        assert!(!endpoint_serves_home(
            home.path(),
            "http://100.69.4.80:9191/api/v0/graphql"
        ));
    }
}
