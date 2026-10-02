use std::fs;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use gents::config_client::GraphqlEndpoint;
use gents::identity::{
    load_macos_keychain_identity, load_macos_secure_enclave_identity, AgentIdentity, KeyIdentity,
};

use crate::shared::{StoredInitConfig, StoredRuntimeState};
use crate::{DEFAULT_HTTP_PORT, RUNTIME_STATE_FILE_NAME};

pub(crate) fn resolve_home_dir(explicit: Option<&Path>) -> PathBuf {
    explicit
        .map(Path::to_path_buf)
        .unwrap_or_else(default_home_dir)
}

fn default_home_dir() -> PathBuf {
    // Without a user home directory or GENTS_HOME, the working directory's
    // `.gents` is the only home left to use.
    gents::home::default_home_dir().unwrap_or_else(|_| PathBuf::from(".gents"))
}

pub(crate) fn is_default_home(home_dir: &Path) -> bool {
    let canonical = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical(home_dir) == canonical(&default_home_dir())
}

// default_data_dir, default_key_path, init_config_path, write_init_config,
// and read_init_config now live in `gents::home` (moved so `gc-cell` can
// write the same `init.json` shape from outside this binary); these are
// thin delegations that keep every call site in this crate unchanged.

pub(crate) fn default_data_dir(home_dir: &Path) -> PathBuf {
    gents::home::default_data_dir(home_dir)
}

pub(crate) fn default_key_path(home_dir: &Path, agent_name: &str) -> PathBuf {
    gents::home::default_key_path(home_dir, agent_name)
}

pub(crate) fn init_config_path(home_dir: &Path) -> PathBuf {
    gents::home::init_config_path(home_dir)
}

pub(crate) fn runtime_state_path(home_dir: &Path) -> PathBuf {
    home_dir.join(RUNTIME_STATE_FILE_NAME)
}

pub(crate) fn write_init_config(home_dir: &Path, state: &StoredInitConfig) -> Result<()> {
    gents::home::write_init_config(home_dir, state)
}

pub(crate) fn read_init_config(home_dir: &Path) -> Result<Option<StoredInitConfig>> {
    gents::home::read_init_config(home_dir)
}

/// Load and register the signer recorded by an initialized home.
///
/// Every embedded-node entry point uses this same loader so opening the data
/// directory outside `gents server` does not silently produce unsigned commits.
pub(crate) fn load_initialized_home_identity(
    home_dir: &Path,
    config: &StoredInitConfig,
) -> Result<Arc<dyn AgentIdentity>> {
    let expected_did = config.agent_did.trim();
    if expected_did.is_empty() {
        anyhow::bail!("initialized home {} has no agent DID", home_dir.display());
    }

    let identity: Arc<dyn AgentIdentity> = if let Some(key_path) = config
        .key_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let key_path = PathBuf::from(key_path);
        if !key_path.exists() {
            anyhow::bail!(
                "initialized home agent DID {expected_did} requires identity key {} to already exist",
                key_path.display()
            );
        }
        Arc::new(
            KeyIdentity::load_existing(&key_path, None)
                .with_context(|| format!("loading identity key {}", key_path.display()))?,
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
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "initialized home {} uses macos-keychain but has no keychain_label",
                            home_dir.display()
                        )
                    })?;
                Arc::new(
                    load_macos_keychain_identity(label, None)
                        .with_context(|| format!("loading macOS keychain identity {label}"))?,
                )
            }
            Some("macos-secure-enclave") => {
                let label = config
                    .secure_enclave_label
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "initialized home {} uses macos-secure-enclave but has no secure_enclave_label",
                            home_dir.display()
                        )
                    })?;
                Arc::new(
                    load_macos_secure_enclave_identity(label, None).with_context(|| {
                        format!("loading macOS Secure Enclave identity {label}")
                    })?,
                )
            }
            backend => anyhow::bail!(
                "initialized home {} has no key_path and unsupported identity_backend {backend:?}",
                home_dir.display()
            ),
        }
    };

    if identity.did() != expected_did {
        anyhow::bail!(
            "initialized home agent DID {expected_did} does not match loaded identity DID {}",
            identity.did()
        );
    }
    Ok(identity)
}

pub(crate) fn write_runtime_state(home_dir: &Path, state: &StoredRuntimeState) -> Result<()> {
    fs::create_dir_all(home_dir)
        .with_context(|| format!("creating home directory {}", home_dir.display()))?;
    let path = runtime_state_path(home_dir);
    let contents = serde_json::to_vec_pretty(state).context("encoding local runtime state JSON")?;
    // Replaced, never truncated in place: a server killed mid-write would
    // otherwise leave an empty file every later command fails to decode.
    crate::native_service::atomic_write(&path, &contents)
        .with_context(|| format!("writing runtime state {}", path.display()))
}

pub(crate) fn read_runtime_state(home_dir: &Path) -> Result<Option<StoredRuntimeState>> {
    let path = runtime_state_path(home_dir);
    if !path.exists() {
        return Ok(None);
    }
    let bytes =
        fs::read(&path).with_context(|| format!("reading runtime state {}", path.display()))?;
    let state = serde_json::from_slice(&bytes)
        .with_context(|| format!("decoding runtime state {}", path.display()))?;
    Ok(Some(state))
}

pub(crate) fn clear_runtime_state(home_dir: &Path) -> Result<bool> {
    let path = runtime_state_path(home_dir);
    if path.exists() {
        fs::remove_file(&path)
            .with_context(|| format!("removing stale runtime state {}", path.display()))?;
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

    anyhow::ensure!(
        home.is_none(),
        "home {} has no running server recorded; start `gents server --home {}` or pass --graphql explicitly",
        home_dir.display(),
        home_dir.display()
    );

    Ok(home_graphql_endpoint(
        &home_dir,
        format!("http://127.0.0.1:{DEFAULT_HTTP_PORT}/api/v0/graphql"),
    ))
}

/// `url` acting as the home's principal when it is this home's own runtime
/// endpoint or a loopback address and this process can load the principal's
/// signing key; otherwise anonymous.
///
/// A served home admits HTTP writes, schema changes and P2P administration
/// only from its own principal. DefraDB checks a bearer's audience against
/// the `Host` header the sender chose, and bearers carry no nonce, so a
/// bearer handed to another host could be replayed against any node that
/// trusts this DID until it expires. Anonymous access reads, and a served
/// home refuses everything else with an authorization error.
pub(crate) fn home_graphql_endpoint(home_dir: &Path, url: impl Into<String>) -> GraphqlEndpoint {
    let url = url.into();
    let principal = match read_init_config(home_dir) {
        Ok(Some(config)) if endpoint_serves_home(home_dir, &url) => {
            load_initialized_home_identity(home_dir, &config)
                .map(|identity| identity.did().to_string())
                .map_err(|error| format!("{error:#}"))
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

/// Whether `url` is this home's recorded runtime endpoint or on loopback.
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

pub(crate) fn resolve_agent_did(home: Option<&Path>, explicit: Option<&str>) -> Result<String> {
    if let Some(agent_did) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(agent_did.to_string());
    }

    let home_dir = resolve_home_dir(home);
    if let Some(runtime_state) = read_runtime_state(&home_dir)? {
        return Ok(runtime_state.agent_did);
    }
    if let Some(init_config) = read_init_config(&home_dir)? {
        return Ok(init_config.agent_did);
    }

    anyhow::bail!(
        "agent DID is required; run `gents init`, start `gents server`, then retry `gents status`, or pass --agent-did explicitly"
    )
}

pub(crate) fn display_host(host: IpAddr) -> String {
    match host {
        IpAddr::V4(addr) if addr == Ipv4Addr::UNSPECIFIED => "127.0.0.1".to_string(),
        _ => host.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{endpoint_serves_home, resolve_graphql_endpoint};

    #[test]
    fn an_explicit_unserved_home_has_no_graphql_endpoint() {
        let home = tempfile::tempdir().unwrap();
        let error = resolve_graphql_endpoint(None, Some(home.path())).unwrap_err();
        let message = error.to_string();
        assert!(message.contains(&home.path().display().to_string()));
        assert!(message.contains("gents server --home"));
        assert!(message.contains("--graphql"));
    }

    #[test]
    fn malformed_runtime_state_does_not_fall_back_to_another_node() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(crate::RUNTIME_STATE_FILE_NAME), "{").unwrap();
        let error = resolve_graphql_endpoint(None, Some(home.path())).unwrap_err();
        assert!(error.to_string().contains("decoding runtime state"));
    }

    #[test]
    fn an_explicit_endpoint_takes_precedence_over_home_runtime_state() {
        let home = tempfile::tempdir().unwrap();
        let url = "https://runtime.example.com/api/v0/graphql";
        for state in [None, Some("{")] {
            if let Some(state) = state {
                std::fs::write(home.path().join(crate::RUNTIME_STATE_FILE_NAME), state).unwrap();
            }
            let endpoint = resolve_graphql_endpoint(Some(url), Some(home.path())).unwrap();
            assert_eq!(endpoint.url(), url);
        }
    }

    #[test]
    fn an_explicit_home_uses_its_recorded_endpoint() {
        let home = tempfile::tempdir().unwrap();
        let url = "http://127.0.0.1:28191/api/v0/graphql";
        std::fs::write(
            home.path().join(crate::RUNTIME_STATE_FILE_NAME),
            serde_json::to_vec(&serde_json::json!({
                "home": home.path(),
                "graphql": url,
                "agent_name": "trial",
                "agent_did": "did:key:z6Mk",
                "default_behavior_id": "subject",
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            resolve_graphql_endpoint(None, Some(home.path()))
                .unwrap()
                .url(),
            url
        );
    }

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
                "agent_name": "a",
                "agent_did": "did:key:z6Mk",
                "default_behavior_id": "b",
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
