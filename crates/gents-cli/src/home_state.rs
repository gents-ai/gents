use std::fs;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use gents::identity::{
    load_macos_keychain_identity, load_macos_secure_enclave_identity, AgentIdentity, KeyIdentity,
};

use crate::shared::{StoredInitConfig, StoredRuntimeState};
use crate::{DEFAULT_HTTP_PORT, INIT_CONFIG_FILE_NAME, RUNTIME_STATE_FILE_NAME};

pub(crate) fn resolve_home_dir(explicit: Option<&Path>) -> PathBuf {
    explicit
        .map(Path::to_path_buf)
        .unwrap_or_else(default_home_dir)
}

fn default_home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".gents")
}

pub(crate) fn default_data_dir(home_dir: &Path) -> PathBuf {
    home_dir.join("data")
}

pub(crate) fn default_key_path(home_dir: &Path, agent_name: &str) -> PathBuf {
    home_dir.join("keys").join(format!("{agent_name}.key"))
}

pub(crate) fn init_config_path(home_dir: &Path) -> PathBuf {
    home_dir.join(INIT_CONFIG_FILE_NAME)
}

pub(crate) fn runtime_state_path(home_dir: &Path) -> PathBuf {
    home_dir.join(RUNTIME_STATE_FILE_NAME)
}

pub(crate) fn write_init_config(home_dir: &Path, state: &StoredInitConfig) -> Result<()> {
    fs::create_dir_all(home_dir)
        .with_context(|| format!("creating home directory {}", home_dir.display()))?;
    let path = init_config_path(home_dir);
    let contents = serde_json::to_vec_pretty(state).context("encoding local init config JSON")?;
    fs::write(&path, contents)
        .with_context(|| format!("writing init config {}", path.display()))?;
    Ok(())
}

pub(crate) fn read_init_config(home_dir: &Path) -> Result<Option<StoredInitConfig>> {
    let path = init_config_path(home_dir);
    if !path.exists() {
        return Ok(None);
    }
    let bytes =
        fs::read(&path).with_context(|| format!("reading init config {}", path.display()))?;
    let state = serde_json::from_slice(&bytes)
        .with_context(|| format!("decoding init config {}", path.display()))?;
    Ok(Some(state))
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
            KeyIdentity::load_or_create(&key_path, None)
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
    fs::write(&path, contents)
        .with_context(|| format!("writing runtime state {}", path.display()))?;
    Ok(())
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
) -> Result<String> {
    if let Some(graphql) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(graphql.to_string());
    }

    let home_dir = resolve_home_dir(home);
    if let Some(runtime_state) = read_runtime_state(&home_dir)? {
        return Ok(runtime_state.graphql);
    }

    Ok(format!(
        "http://127.0.0.1:{DEFAULT_HTTP_PORT}/api/v0/graphql"
    ))
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
