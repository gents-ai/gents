use std::fs;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::shared::{StoredInitConfig, StoredRuntimeState};
use crate::{
    DEFAULT_AGENT_NAME, DEFAULT_HTTP_PORT, INIT_CONFIG_FILE_NAME, RUNTIME_STATE_FILE_NAME,
};

pub(crate) fn resolve_home_dir(explicit: Option<&Path>) -> PathBuf {
    explicit
        .map(Path::to_path_buf)
        .unwrap_or_else(default_home_dir)
}

fn default_home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".defra-agent")
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

    Ok(format!("did:defra-agent:{DEFAULT_AGENT_NAME}"))
}

pub(crate) fn display_host(host: IpAddr) -> String {
    match host {
        IpAddr::V4(addr) if addr == Ipv4Addr::UNSPECIFIED => "127.0.0.1".to_string(),
        _ => host.to_string(),
    }
}
