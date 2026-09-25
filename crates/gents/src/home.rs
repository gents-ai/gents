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

/// The exclusive lock a process holds on a data directory while it has the
/// store open. The OS releases it when the holder exits, however it exits.
#[derive(Debug)]
pub struct StoreLock {
    _file: fs::File,
    path: PathBuf,
}

impl StoreLock {
    /// The lock file. Keep it in place while held: a renamed or removed lock
    /// file no longer excludes a new holder.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Takes the exclusive lock on `data_dir`'s store, so two runtimes never
/// open one store. The lock file sits beside the canonical data directory,
/// so every path that aliases the store (symlinks, `.`) takes the same lock.
/// It records the holder's process id. `data_dir` must exist.
pub fn lock_store(home_dir: &Path, data_dir: &Path) -> Result<StoreLock> {
    use std::io::{Read as _, Seek as _, Write as _};

    let canonical = fs::canonicalize(data_dir)
        .with_context(|| format!("resolving data directory {}", data_dir.display()))?;
    let (Some(parent), Some(name)) = (canonical.parent(), canonical.file_name()) else {
        anyhow::bail!(
            "data directory {} cannot be the filesystem root",
            canonical.display()
        );
    };
    let path = parent.join(format!("{}.lock", name.to_string_lossy()));
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    // Never follow a planted symlink: truncating below would clobber its target.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(&path)
        .with_context(|| format!("opening store lock {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => {
            let mut holder = String::new();
            let _ = file.read_to_string(&mut holder);
            let holder = holder
                .trim()
                .parse::<u32>()
                .map(|pid| format!(" (process {pid})"))
                .unwrap_or_default();
            anyhow::bail!(
                "another Gents runtime{holder} is already using {home}. Stop it first: `gents service stop --home {home}` if it runs as the background service, or Ctrl-C in the terminal running `gents server`",
                home = home_dir.display()
            );
        }
        Err(fs::TryLockError::Error(error)) => {
            return Err(error).with_context(|| format!("locking {}", path.display()))
        }
    }
    file.set_len(0)?;
    file.rewind()?;
    writeln!(file, "{}", std::process::id())?;
    Ok(StoreLock { _file: file, path })
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
    #[test]
    fn a_second_holder_cannot_lock_a_store() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let data = default_data_dir(&home);
        fs::create_dir_all(&data).unwrap();

        let held = lock_store(&home, &data).expect("the first holder locks the store");
        let error = lock_store(&home, &data)
            .expect_err("a second holder must not open the same store")
            .to_string();
        assert!(error.contains("already using"), "{error}");
        assert!(
            error.contains(&format!("process {}", std::process::id())),
            "{error}"
        );
        assert!(error.contains("gents service stop --home"), "{error}");

        drop(held);
        lock_store(&home, &data).expect("the lock is released with its holder");
    }

    #[cfg(unix)]
    #[test]
    fn every_alias_of_a_store_takes_the_same_lock() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real-data");
        fs::create_dir_all(&real).unwrap();
        let link = temp.path().join("link-data");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let held = lock_store(temp.path(), &real).unwrap();
        assert!(
            lock_store(temp.path(), &link).is_err(),
            "a symlinked data directory is the same store"
        );
        assert!(
            lock_store(temp.path(), &real.join("..").join("real-data")).is_err(),
            "a non-canonical path is the same store"
        );
        drop(held);

        // `--data-dir .` names the current directory, which has a name once
        // resolved.
        let current = lock_store(temp.path(), &real.join(".")).unwrap();
        assert_eq!(
            current.path(),
            fs::canonicalize(temp.path())
                .unwrap()
                .join("real-data.lock")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_lock_file_is_refused_without_touching_its_target() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        fs::create_dir_all(&data).unwrap();
        let victim = temp.path().join("victim");
        fs::write(&victim, "precious").unwrap();
        std::os::unix::fs::symlink(&victim, temp.path().join("data.lock")).unwrap();

        assert!(lock_store(temp.path(), &data).is_err());
        assert_eq!(fs::read_to_string(&victim).unwrap(), "precious");
    }

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
