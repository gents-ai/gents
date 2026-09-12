//! Home path helpers and the persisted `init.json` shape, shared by the CLI
//! and other provisioners.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

const INIT_CONFIG_FILE_NAME: &str = "init.json";

/// A gents home's persisted `init.json`: home path, agent name, agent DID,
/// key path, identity backend, and tool policy. Written once at `gents
/// init` (or, for a gents-cloud cell, at cell provisioning) and read back
/// by every entry point that opens the home afterward.
// The explicit deserialize bound replaces serde_derive's default bound
// inference, which (a known serde_derive limitation) adds `ToolPackage:
// Default` merely because `tool_package` is a `#[serde(default)]` field
// mentioning it, even though the generated code only ever calls
// `Option::<ToolPackage>::default()` (`None`, no `ToolPackage: Default`
// bound required by `Option`'s own blanket impl). Without this override,
// `gents-cli`'s `ToolPackageArg`/`ToolCeilingArg` (which do not, and
// should not, derive `Default`) could not deserialize this struct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(deserialize = "ToolPackage: Deserialize<'de>, ToolCeiling: Deserialize<'de>"))]
pub struct StoredInitConfig<ToolPackage = String, ToolCeiling = String> {
    pub home: String,
    pub agent_name: String,
    pub agent_did: String,
    pub key_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keychain_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secure_enclave_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_package: Option<ToolPackage>,
    pub tool_ceiling: ToolCeiling,
    pub tool_root: Option<String>,
}

/// The default DefraDB data directory under a gents home.
pub fn default_data_dir(home_dir: &Path) -> PathBuf {
    home_dir.join("data")
}

/// The default identity key path under a gents home, for the named agent.
pub fn default_key_path(home_dir: &Path, agent_name: &str) -> PathBuf {
    home_dir.join("keys").join(format!("{agent_name}.key"))
}

/// The path `init.json` lives at under a gents home.
pub fn init_config_path(home_dir: &Path) -> PathBuf {
    home_dir.join(INIT_CONFIG_FILE_NAME)
}

/// Writes `state` to `<home_dir>/init.json`, creating `home_dir` if it
/// does not already exist.
pub fn write_init_config<ToolPackage: Serialize, ToolCeiling: Serialize>(
    home_dir: &Path,
    state: &StoredInitConfig<ToolPackage, ToolCeiling>,
) -> Result<()> {
    fs::create_dir_all(home_dir)
        .with_context(|| format!("creating home directory {}", home_dir.display()))?;
    let path = init_config_path(home_dir);
    let contents = serde_json::to_vec_pretty(state).context("encoding local init config JSON")?;
    fs::write(&path, contents)
        .with_context(|| format!("writing init config {}", path.display()))?;
    Ok(())
}

/// Reads `<home_dir>/init.json`, or `None` if the home has not been
/// initialized yet.
pub fn read_init_config<ToolPackage: DeserializeOwned, ToolCeiling: DeserializeOwned>(
    home_dir: &Path,
) -> Result<Option<StoredInitConfig<ToolPackage, ToolCeiling>>> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> StoredInitConfig<String, String> {
        StoredInitConfig {
            home: "/home/user/.gents".to_string(),
            agent_name: "default".to_string(),
            agent_did: "did:key:z6Mk...".to_string(),
            key_path: Some("/home/user/.gents/keys/default.key".to_string()),
            identity_backend: None,
            keychain_label: None,
            secure_enclave_label: None,
            tool_package: Some("Readonly".to_string()),
            tool_ceiling: "Readonly".to_string(),
            tool_root: None,
        }
    }

    #[test]
    fn default_data_dir_nests_under_home() {
        assert_eq!(
            default_data_dir(Path::new("/home/user/.gents")),
            PathBuf::from("/home/user/.gents/data")
        );
    }

    #[test]
    fn default_key_path_nests_under_keys_by_agent_name() {
        assert_eq!(
            default_key_path(Path::new("/home/user/.gents"), "default"),
            PathBuf::from("/home/user/.gents/keys/default.key")
        );
    }

    #[test]
    fn init_config_path_is_init_json_under_home() {
        assert_eq!(
            init_config_path(Path::new("/home/user/.gents")),
            PathBuf::from("/home/user/.gents/init.json")
        );
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let state = sample();

        write_init_config(dir.path(), &state).unwrap();
        let loaded: StoredInitConfig<String, String> = read_init_config(dir.path())
            .unwrap()
            .expect("init.json must exist");

        assert_eq!(loaded, state);
    }

    #[test]
    fn read_returns_none_for_an_uninitialized_home() {
        let dir = tempfile::tempdir().unwrap();
        let loaded: Option<StoredInitConfig<String, String>> =
            read_init_config(dir.path()).unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn tool_package_is_omitted_from_json_when_absent() {
        let mut state = sample();
        state.tool_package = None;
        let json = serde_json::to_value(&state).unwrap();
        assert!(!json.as_object().unwrap().contains_key("tool_package"));
    }
}
