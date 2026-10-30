use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::{HomeError, Result, INIT_CONFIG_FILE_NAME, KEYS_DIR_NAME};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(deserialize = "ToolPackage: Deserialize<'de>, ToolCeiling: Deserialize<'de>"))]
pub struct StoredInitConfig<ToolPackage = String, ToolCeiling = String> {
    pub home: String,
    pub node_name: String,
    pub node_did: String,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_encryption: Option<crate::store::local::store_key::StoreEncryption>,
}

pub fn default_key_path(home_dir: &Path, node_name: &str) -> PathBuf {
    keys_dir(home_dir).join(format!("{node_name}.key"))
}

pub fn keys_dir(home_dir: &Path) -> PathBuf {
    home_dir.join(KEYS_DIR_NAME)
}

pub fn init_config_path(home_dir: &Path) -> PathBuf {
    home_dir.join(INIT_CONFIG_FILE_NAME)
}

/// Publish and sync the complete custody record before encrypted store writes.
pub fn write_init_config<ToolPackage, ToolCeiling>(
    home_dir: &Path,
    state: &StoredInitConfig<ToolPackage, ToolCeiling>,
) -> Result<()>
where
    ToolPackage: Serialize,
    ToolCeiling: Serialize,
{
    fs::create_dir_all(home_dir).map_err(|source| HomeError::CreateHome {
        path: home_dir.to_path_buf(),
        source,
    })?;
    let path = init_config_path(home_dir);
    let contents = serde_json::to_vec_pretty(state).map_err(HomeError::EncodeInitConfig)?;
    let staged =
        tempfile::NamedTempFile::new_in(home_dir).map_err(|source| HomeError::StageInitConfig {
            path: path.clone(),
            source,
        })?;
    use std::io::Write as _;
    staged.as_file().write_all(&contents)?;
    staged.as_file().sync_all()?;
    staged
        .persist(&path)
        .map_err(|error| HomeError::WriteInitConfig {
            path: path.clone(),
            source: Box::new(error),
        })?;
    #[cfg(unix)]
    fs::File::open(home_dir)?.sync_all()?;
    Ok(())
}

pub fn read_init_config<ToolPackage, ToolCeiling>(
    home_dir: &Path,
) -> Result<Option<StoredInitConfig<ToolPackage, ToolCeiling>>>
where
    ToolPackage: DeserializeOwned,
    ToolCeiling: DeserializeOwned,
{
    let path = init_config_path(home_dir);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path).map_err(|source| HomeError::ReadInitConfig {
        path: path.clone(),
        source,
    })?;
    let state = serde_json::from_slice(&bytes)
        .map_err(|source| HomeError::DecodeInitConfig { path, source })?;
    Ok(Some(state))
}

pub const SECURE_ENCLAVE_IDENTITY_BACKEND: &str = "macos-secure-enclave";

/// Refuse to serve a home whose node cannot own DefraDB node access
/// control: node access control needs the node DID's private key bytes,
/// which a Secure Enclave key never exports, and serving such a home
/// without access control is undecided.
pub fn ensure_home_identity_can_serve(home_dir: &Path) -> Result<()> {
    let Some(config) = read_init_config::<serde_json::Value, serde_json::Value>(home_dir)? else {
        return Ok(());
    };
    if config.identity_backend.as_deref().map(str::trim) == Some(SECURE_ENCLAVE_IDENTITY_BACKEND) {
        return Err(HomeError::SecureEnclaveHome {
            home: home_dir.to_path_buf(),
        });
    }
    Ok(())
}
