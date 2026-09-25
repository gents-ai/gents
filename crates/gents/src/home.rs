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

/// The home gents uses when none is named: `GENTS_HOME` when set, otherwise
/// `.gents` in the user's home directory. The CLI and the desktop app both
/// resolve it here, so what one installs the other sees.
pub fn default_home_dir() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("GENTS_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(home));
    }
    Ok(dirs::home_dir()
        .context("unable to resolve the user's home directory; set GENTS_HOME")?
        .join(".gents"))
}

const DATA_DIR_NAME: &str = "data";
const KEYS_DIR_NAME: &str = "keys";
/// The runtime's persisted serving state (`gents server`).
pub const RUNTIME_STATE_FILE_NAME: &str = "runtime.json";
/// The runtime's persisted P2P transport key.
pub const P2P_SECRET_KEY_FILE_NAME: &str = "p2p-secret-key";
/// Installed and cached packs.
pub const PACKS_DIR_NAME: &str = "packs";
/// Installed plugins.
pub const PLUGINS_DIR_NAME: &str = "plugins";
/// Registry logins saved by `gents pack login`, one per registry URL.
pub const REGISTRY_DIR_NAME: &str = "registry";
/// The Codex shim's own home.
pub const CODEX_UI_DIR_NAME: &str = "codex-ui";
/// Frozen eval runs and optimization job directories (`gents eval`).
pub const EVAL_DIR_NAME: &str = "eval";

/// Every top-level entry a gents runtime writes in its home. Writers name
/// these entries through this module, and retiring a home (after an upgrade
/// that cannot open it) moves or deletes exactly these names; anything else
/// in the home is left in place. A new top-level entry belongs here first.
///
/// The test `runtime_writers_name_home_entries_only_from_the_inventory` is a
/// syntactic ratchet over writers, not complete enforcement (see its docs).
pub const RUNTIME_HOME_ENTRIES: &[&str] = &[
    DATA_DIR_NAME,
    INIT_CONFIG_FILE_NAME,
    KEYS_DIR_NAME,
    RUNTIME_STATE_FILE_NAME,
    P2P_SECRET_KEY_FILE_NAME,
    PACKS_DIR_NAME,
    PLUGINS_DIR_NAME,
    REGISTRY_DIR_NAME,
    CODEX_UI_DIR_NAME,
    EVAL_DIR_NAME,
];

/// The default DefraDB data directory under a gents home.
pub fn default_data_dir(home_dir: &Path) -> PathBuf {
    home_dir.join(DATA_DIR_NAME)
}

/// The exclusive lock a process holds on a data directory while it has the
/// store open. The OS releases it when the holder exits, however it exits.
///
/// The lock is a `flock` on the lock file's open file description, which a
/// child forked while the lock is held shares until it execs (the descriptor
/// is close-on-exec). Dropping a `StoreLock` therefore releases the store
/// only once every such child has exec'd or exited: a process that forks (a
/// `pre_exec` spawn, or glibc's vfork-based spawn from another thread) may
/// still exclude a new holder briefly after the drop.
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
    let canonical = fs::canonicalize(data_dir)
        .with_context(|| format!("resolving data directory {}", data_dir.display()))?;
    let (Some(parent), Some(name)) = (canonical.parent(), canonical.file_name()) else {
        anyhow::bail!(
            "data directory {} cannot be the filesystem root",
            canonical.display()
        );
    };
    lock_path(
        home_dir,
        parent.join(format!("{}.lock", name.to_string_lossy())),
    )
}

/// Takes the store lock of a home's default data directory whether or not
/// that directory exists yet, without creating it. With the directory
/// present this is [`lock_store`]; without it, the lock sits where
/// [`lock_store`] will look once the directory is created under the
/// canonical home, so a runtime or `init` that creates the store afterwards
/// is excluded by the same lock.
pub fn lock_home_store(home_dir: &Path) -> Result<StoreLock> {
    let data_dir = default_data_dir(home_dir);
    match fs::symlink_metadata(&data_dir) {
        Ok(_) => lock_store(home_dir, &data_dir),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // The lock file is deliberately not a runtime home entry: a
            // reset leaves it in place while held.
            let lock_dir = fs::canonicalize(home_dir)
                .with_context(|| format!("resolving home {}", home_dir.display()))?;
            lock_path(home_dir, lock_dir.join(format!("{DATA_DIR_NAME}.lock")))
        }
        Err(error) => {
            Err(error).with_context(|| format!("inspecting data directory {}", data_dir.display()))
        }
    }
}

/// A store lock this process could not take because another holder has it.
///
/// `holder_pid` is absent when no pid can be read or parsed from the lock
/// file.
#[derive(Debug)]
pub struct StoreLockHeld {
    pub home: PathBuf,
    pub holder_pid: Option<u32>,
}

impl std::fmt::Display for StoreLockHeld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let holder = self
            .holder_pid
            .map(|pid| format!(" (process {pid})"))
            .unwrap_or_default();
        let home = self.home.display();
        write!(
            f,
            "another Gents runtime{holder} is already using {home}. Stop it first: `gents service stop --home {home}` if it runs as the background service, or Ctrl-C in the terminal running `gents server`"
        )
    }
}

impl std::error::Error for StoreLockHeld {}

fn lock_path(home_dir: &Path, path: PathBuf) -> Result<StoreLock> {
    use std::io::{Read as _, Seek as _, Write as _};

    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    // Never follow a planted symlink: truncating below would clobber its
    // target. A planted FIFO must not block the open either.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options
        .open(&path)
        .with_context(|| format!("opening store lock {}", path.display()))?;
    if !file
        .metadata()
        .with_context(|| format!("inspecting store lock {}", path.display()))?
        .is_file()
    {
        anyhow::bail!("store lock {} is not a regular file", path.display());
    }
    match file.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => {
            let mut holder = String::new();
            let _ = (&mut file).take(32).read_to_string(&mut holder);
            return Err(anyhow::Error::new(StoreLockHeld {
                home: home_dir.to_path_buf(),
                holder_pid: holder.trim().parse::<u32>().ok(),
            }));
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
    home_dir
        .join(KEYS_DIR_NAME)
        .join(format!("{agent_name}.key"))
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

/// The top-level entries of a gents home, split into those the home's
/// runtime owns and those left in place.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HomeEntries {
    /// Present entries named in [`RUNTIME_HOME_ENTRIES`]. Symbolic links are
    /// listed as links; their targets are never part of the home.
    pub owned: Vec<PathBuf>,
    /// Everything else: other agents' homes, backups, user files, a store
    /// lock, and any owned name that another home's `init.json` refers into
    /// or that contains an `exclude`d path.
    pub retained: Vec<PathBuf>,
}

/// Lists a home's top-level entries without following symbolic links.
///
/// Only the fixed runtime inventory is owned. A default home (`~/.gents`) may
/// hold other agents' homes and the user's own files; they are retained, and
/// so is an owned entry that an immediate child home's `init.json` (its
/// `home`, `key_path` or `tool_root`) points into.
pub fn home_entries(home_dir: &Path, exclude: &[PathBuf]) -> Result<HomeEntries> {
    let mut entries = HomeEntries::default();
    let listing = match fs::read_dir(home_dir) {
        Ok(listing) => listing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
        Err(error) => {
            return Err(error).with_context(|| format!("listing home {}", home_dir.display()))
        }
    };
    let mut candidates = Vec::new();
    for entry in listing {
        let entry = entry.with_context(|| format!("listing home {}", home_dir.display()))?;
        let path = entry.path();
        let owned_name = entry
            .file_name()
            .to_str()
            .is_some_and(|name| RUNTIME_HOME_ENTRIES.contains(&name));
        if owned_name {
            candidates.push(path);
        } else {
            entries.retained.push(path);
        }
    }
    let mut references: Vec<PathBuf> = exclude.to_vec();
    for retained in &entries.retained {
        references.extend(child_home_references(retained));
    }
    for candidate in candidates {
        if references
            .iter()
            .any(|reference| reference.starts_with(&candidate))
        {
            entries.retained.push(candidate);
        } else {
            entries.owned.push(candidate);
        }
    }
    entries.owned.sort();
    entries.retained.sort();
    Ok(entries)
}

/// Paths an immediate child home's `init.json` names. An unreadable or
/// unparsable record names nothing; the directory itself is retained anyway.
fn child_home_references(directory: &Path) -> Vec<PathBuf> {
    let Ok(bytes) = fs::read(init_config_path(directory)) else {
        return Vec::new();
    };
    let Ok(record) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Vec::new();
    };
    ["home", "key_path", "tool_root"]
        .iter()
        .filter_map(|field| record.get(field).and_then(serde_json::Value::as_str))
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            let path = PathBuf::from(value);
            fs::canonicalize(&path).unwrap_or(path)
        })
        .collect()
}

/// What [`retire_entries`] does with the entries it is given.
#[derive(Debug, Clone, Copy)]
pub enum RetireDisposition<'a> {
    /// Move every entry into `backup/<group>/`. `backup` must not exist yet
    /// and must be on the same filesystem (see [`archive_preflight`]):
    /// entries are renamed, never copied, so file contents and modes are
    /// carried over without being read. Backup directories are private to
    /// the user.
    Archive { backup: &'a Path },
    /// Remove every entry. Symbolic links are removed, not followed.
    Delete,
}

/// A named set of entries retired together, e.g. a runtime home's own
/// entries or the desktop client's state.
#[derive(Debug, Clone, Copy)]
pub struct RetireGroup<'a> {
    pub name: &'a str,
    pub entries: &'a [PathBuf],
}

/// Fails unless every present entry can be renamed into `backup`: renames do
/// not cross filesystems, and discovering that after the owning service was
/// stopped would leave a half-retired home.
pub fn archive_preflight(groups: &[RetireGroup<'_>], backup: &Path) -> Result<()> {
    archive_preflight_with(groups, backup, device_of)
}

#[cfg(unix)]
fn device_of(path: &Path) -> std::io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    fs::symlink_metadata(path).map(|metadata| metadata.dev())
}

#[cfg(not(unix))]
fn device_of(_path: &Path) -> std::io::Result<u64> {
    Ok(0)
}

fn archive_preflight_with(
    groups: &[RetireGroup<'_>],
    backup: &Path,
    device: impl Fn(&Path) -> std::io::Result<u64>,
) -> Result<()> {
    let parent = backup
        .parent()
        .with_context(|| format!("backup {} has no parent directory", backup.display()))?;
    let target = device(parent).with_context(|| format!("inspecting {}", parent.display()))?;
    for entry in groups.iter().flat_map(|group| group.entries.iter()) {
        match device(entry) {
            Ok(source) if source == target => {}
            Ok(_) => anyhow::bail!(
                "{} is on a different filesystem than the backup location {}; nothing was changed",
                entry.display(),
                parent.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("inspecting {}", entry.display()))
            }
        }
    }
    Ok(())
}

/// Archives or deletes the given entries and returns the ones that existed.
///
/// Only a missing entry is skipped; any other failure to inspect one is an
/// error. Archiving is all-or-nothing: if any step fails, the entries already
/// moved are renamed back and the backup directory is removed again.
/// Deleting stops at the first failure and reports what was already removed.
pub fn retire_entries(
    groups: &[RetireGroup<'_>],
    disposition: RetireDisposition<'_>,
) -> Result<Vec<PathBuf>> {
    retire_entries_with(groups, disposition, |path| {
        fs::symlink_metadata(path).map(|metadata| metadata.is_dir())
    })
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

/// `inspect` reports whether an entry is a real directory (not a link).
fn retire_entries_with(
    groups: &[RetireGroup<'_>],
    disposition: RetireDisposition<'_>,
    inspect: impl Fn(&Path) -> std::io::Result<bool>,
) -> Result<Vec<PathBuf>> {
    let present = |entry: &Path| -> Result<Option<bool>> {
        match inspect(entry) {
            Ok(is_dir) => Ok(Some(is_dir)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("inspecting {}", entry.display())),
        }
    };
    match disposition {
        RetireDisposition::Archive { backup } => {
            private_dir(backup).with_context(|| format!("creating backup {}", backup.display()))?;
            let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
            let result = (|| -> Result<()> {
                for group in groups {
                    let target_dir = backup.join(group.name);
                    private_dir(&target_dir)
                        .with_context(|| format!("creating {}", target_dir.display()))?;
                    for entry in group.entries {
                        if present(entry)?.is_none() {
                            continue;
                        }
                        let name = entry.file_name().with_context(|| {
                            format!("{} has no file name to archive", entry.display())
                        })?;
                        let target = target_dir.join(name);
                        fs::rename(entry, &target).with_context(|| {
                            format!("moving {} to {}", entry.display(), target.display())
                        })?;
                        moved.push((entry.clone(), target));
                    }
                }
                Ok(())
            })();
            match result {
                Ok(()) => Ok(moved.into_iter().map(|(source, _)| source).collect()),
                Err(error) => {
                    let mut unrestored = Vec::new();
                    for (source, target) in moved.iter().rev() {
                        if fs::rename(target, source).is_err() {
                            unrestored.push(target.display().to_string());
                        }
                    }
                    if unrestored.is_empty() {
                        for group in groups {
                            let _ = fs::remove_dir(backup.join(group.name));
                        }
                        let _ = fs::remove_dir(backup);
                        Err(error.context("archiving was rolled back; nothing was moved"))
                    } else {
                        Err(error.context(format!(
                            "archiving failed and these entries could not be moved back: {}",
                            unrestored.join(", ")
                        )))
                    }
                }
            }
        }
        RetireDisposition::Delete => {
            let mut removed: Vec<PathBuf> = Vec::new();
            let done = |removed: &[PathBuf]| {
                removed
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            for entry in groups.iter().flat_map(|group| group.entries.iter()) {
                let is_dir = match present(entry) {
                    Ok(Some(is_dir)) => is_dir,
                    Ok(None) => continue,
                    Err(error) => {
                        return Err(error.context(format!(
                            "deleting stopped after deleting [{}]",
                            done(&removed)
                        )))
                    }
                };
                let outcome = if is_dir {
                    fs::remove_dir_all(entry)
                } else {
                    fs::remove_file(entry)
                };
                if let Err(error) = outcome {
                    return Err(anyhow::Error::new(error).context(format!(
                        "deleting {} failed after deleting [{}]",
                        entry.display(),
                        done(&removed)
                    )));
                }
                removed.push(entry.clone());
            }
            Ok(removed)
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn a_lock_path_that_is_not_a_regular_file_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        fs::create_dir_all(&data).unwrap();
        let fifo = std::ffi::CString::new(
            temp.path()
                .join("data.lock")
                .into_os_string()
                .into_encoded_bytes(),
        )
        .unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let error = lock_store(temp.path(), &data).expect_err("a FIFO is not a lock file");
        assert!(error.to_string().contains("not a regular file"), "{error}");
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

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn home_entries_own_only_the_runtime_inventory() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".gents");
        write(&home.join("init.json"), "{}");
        write(&home.join("data/MANIFEST"), "REGOMAN");
        write(&home.join("keys/agent.key"), "key");
        write(&home.join("runtime.json"), "{}");
        write(&home.join("packs/registry-cache/x.tar.gz"), "pack");
        write(&home.join("data.lock"), "123");
        write(&home.join("backups/legacy-store-1/data/MANIFEST"), "old");
        write(&home.join("notes.md"), "mine");
        write(&home.join(".env"), "SECRET=1");
        write(&home.join("grok-port-home/init.json"), "{}");
        write(
            &home.join("half-onboarded/data/MANIFEST"),
            "live store, no init.json yet",
        );
        write(&home.join("a/b/c/d/e/init.json"), "{}");
        write(&home.join("desktop/peers.json"), "{}");
        #[cfg(unix)]
        std::os::unix::fs::symlink(temp.path(), home.join("keys-link")).unwrap();

        let entries = home_entries(&home, &[]).unwrap();

        assert_eq!(
            entries.owned,
            vec![
                home.join("data"),
                home.join("init.json"),
                home.join("keys"),
                home.join("packs"),
                home.join("runtime.json"),
            ]
        );
        for retained in [
            ".env",
            "a",
            "backups",
            "data.lock",
            "desktop",
            "grok-port-home",
            "half-onboarded",
            "notes.md",
        ] {
            assert!(
                entries.retained.contains(&home.join(retained)),
                "{retained}"
            );
        }
        assert_eq!(
            home_entries(&temp.path().join("absent"), &[]).unwrap(),
            HomeEntries::default()
        );
    }

    #[test]
    fn owned_entries_another_home_or_an_exclusion_points_into_are_retained() {
        let temp = tempfile::tempdir().unwrap();
        let home = fs::canonicalize(temp.path()).unwrap().join(".gents");
        write(&home.join("keys/local.key"), "mine");
        write(&home.join("keys/other.key"), "theirs");
        write(&home.join("plugins/p/manifest.json"), "{}");
        write(&home.join("data/MANIFEST"), "store");
        write(
            &home.join("other/init.json"),
            &serde_json::json!({
                "home": home.join("other"),
                "key_path": home.join("keys/other.key"),
                "tool_root": home.join("plugins/p"),
            })
            .to_string(),
        );

        let entries = home_entries(&home, &[home.join("data/client")]).unwrap();

        assert!(entries.owned.is_empty(), "{:?}", entries.owned);
        for retained in ["data", "keys", "other", "plugins"] {
            assert!(
                entries.retained.contains(&home.join(retained)),
                "{retained}"
            );
        }
    }

    /// A syntactic ratchet, not complete writer enforcement: it scans every
    /// crate's production sources line by line for `.join(...)` onto the
    /// receiver names `home`, `home_dir`, `agent_home`, `gents_home` and
    /// `home_path`, stopping at a file's first `#[cfg(test)]`. There, a
    /// literal must be in the inventory, a constant (resolved by unqualified
    /// name, so duplicate names resolve by directory order) must name an
    /// inventory entry, and a computed (`format!`) name is refused. Joins
    /// split across lines or onto other receiver names are not seen, so it
    /// catches the common ways a new top-level entry appears, not all.
    #[test]
    fn runtime_writers_name_home_entries_only_from_the_inventory() {
        /// Crates whose `home` is not a Gents home (a fixture plugin's own
        /// application data directory).
        const NOT_GENTS_HOMES: &[&str] = &["fixture-domain-plugin"];
        let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let receiver = r"\b(?:home|home_dir|agent_home|gents_home|home_path)\s*\.join\(\s*";
        let literal = regex::Regex::new(&format!(r#"{receiver}"([^"]+)""#)).unwrap();
        let constant =
            regex::Regex::new(&format!(r"{receiver}((?:[a-z_]+::)*[A-Z][A-Z0-9_]*)\s*\)")).unwrap();
        let computed = regex::Regex::new(&format!(r"{receiver}format!")).unwrap();
        let const_def = regex::Regex::new(
            r#"const\s+([A-Z][A-Z0-9_]*)\s*:\s*&str\s*=\s*(?:"([^"]*)"|(?:[a-z_]+::)*([A-Z][A-Z0-9_]*))\s*;"#,
        )
        .unwrap();

        let mut sources = Vec::new();
        for krate in fs::read_dir(&crates).unwrap() {
            let krate = krate.unwrap().path();
            let name = krate.file_name().unwrap().to_string_lossy().into_owned();
            if NOT_GENTS_HOMES.contains(&name.as_str()) {
                continue;
            }
            let mut pending = vec![krate.join("src")];
            while let Some(dir) = pending.pop() {
                let Ok(listing) = fs::read_dir(&dir) else {
                    continue;
                };
                for entry in listing {
                    let path = entry.unwrap().path();
                    if path.is_dir() {
                        if path.file_name().is_some_and(|name| name != "tests") {
                            pending.push(path);
                        }
                    } else if path.extension().is_some_and(|ext| ext == "rs") {
                        sources.push(path);
                    }
                }
            }
        }

        // Constant name -> string value, following one level of aliasing
        // (`const X: &str = gents::home::Y;`).
        let mut values = std::collections::HashMap::<String, String>::new();
        let mut aliases = std::collections::HashMap::<String, String>::new();
        for path in &sources {
            let text = fs::read_to_string(path).unwrap();
            for capture in const_def.captures_iter(&text) {
                match (capture.get(2), capture.get(3)) {
                    (Some(value), _) => {
                        values.insert(capture[1].to_string(), value.as_str().to_string());
                    }
                    (None, Some(target)) => {
                        aliases.insert(capture[1].to_string(), target.as_str().to_string());
                    }
                    _ => {}
                }
            }
        }
        let resolve = |name: &str| -> Option<String> {
            let name = name.rsplit("::").next().unwrap_or(name);
            values.get(name).cloned().or_else(|| {
                aliases
                    .get(name)
                    .and_then(|target| values.get(target).cloned())
            })
        };

        let mut unlisted = Vec::new();
        for path in &sources {
            let file = path.file_name().unwrap().to_string_lossy();
            if file.contains("test") {
                continue;
            }
            let text = fs::read_to_string(path).unwrap();
            for (line_number, line) in text.lines().enumerate() {
                if line.contains("#[cfg(test)]") {
                    break;
                }
                let at = || format!("{}:{}", path.display(), line_number + 1);
                for capture in literal.captures_iter(line) {
                    let top = capture[1].split('/').next().unwrap_or_default();
                    if !RUNTIME_HOME_ENTRIES.contains(&top) {
                        unlisted.push(format!("{}: {top}", at()));
                    }
                }
                for capture in constant.captures_iter(line) {
                    match resolve(&capture[1]) {
                        Some(value) if RUNTIME_HOME_ENTRIES.contains(&value.as_str()) => {}
                        Some(value) => {
                            unlisted.push(format!("{}: {} = {value}", at(), &capture[1]))
                        }
                        None => unlisted.push(format!("{}: unresolved {}", at(), &capture[1])),
                    }
                }
                if computed.is_match(line) {
                    unlisted.push(format!("{}: computed name", at()));
                }
            }
        }
        assert!(
            unlisted.is_empty(),
            "home entries written outside RUNTIME_HOME_ENTRIES: {unlisted:#?}"
        );
    }

    #[test]
    fn archive_moves_groups_into_private_dirs_and_delete_removes_links_without_following_them() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let outside = temp.path().join("outside");
        write(&home.join("data/MANIFEST"), "store");
        write(&home.join("init.json"), "{}");
        write(&outside.join("keep.txt"), "keep");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, home.join("keys")).unwrap();
        let owned = home_entries(&home, &[]).unwrap().owned;

        let backup = temp.path().join("home-backup");
        archive_preflight(
            &[RetireGroup {
                name: "home",
                entries: &owned,
            }],
            &backup,
        )
        .unwrap();
        let moved = retire_entries(
            &[RetireGroup {
                name: "home",
                entries: &owned,
            }],
            RetireDisposition::Archive { backup: &backup },
        )
        .unwrap();
        assert_eq!(moved, owned);
        assert_eq!(
            fs::read_to_string(backup.join("home/data/MANIFEST")).unwrap(),
            "store"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for dir in [backup.clone(), backup.join("home")] {
                let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o700, "{}", dir.display());
            }
        }
        assert!(!home.join("data").exists());
        assert!(
            retire_entries(
                &[RetireGroup {
                    name: "home",
                    entries: &owned,
                }],
                RetireDisposition::Archive { backup: &backup },
            )
            .is_err(),
            "an existing backup is never merged into"
        );

        let archived = home_entries(&backup.join("home"), &[]).unwrap().owned;
        let removed = retire_entries(
            &[RetireGroup {
                name: "home",
                entries: &archived,
            }],
            RetireDisposition::Delete,
        )
        .unwrap();
        assert_eq!(removed, archived);
        assert_eq!(
            fs::read_to_string(outside.join("keep.txt")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn a_failed_archive_moves_everything_back() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        write(&home.join("data/MANIFEST"), "store");
        let entries = vec![
            home.join("data"),
            temp.path().join("absent"),
            PathBuf::from("/"),
        ];
        let backup = temp.path().join("backup");

        let error = retire_entries(
            &[RetireGroup {
                name: "home",
                entries: &entries,
            }],
            RetireDisposition::Archive { backup: &backup },
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains("rolled back"), "{error:#}");
        assert_eq!(
            fs::read_to_string(home.join("data/MANIFEST")).unwrap(),
            "store"
        );
        assert!(!backup.exists());
    }

    /// Only a missing entry is skipped. An entry that cannot be inspected
    /// (permission denied, I/O error) stops the retirement with an account of
    /// what already happened. Injected, so it holds on privileged runners.
    #[test]
    fn an_uninspectable_entry_stops_retirement_with_an_account() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        write(&home.join("data/MANIFEST"), "store");
        write(&home.join("runtime.json"), "{}");
        let entries = vec![home.join("data"), home.join("runtime.json")];
        let denied = home.join("runtime.json");
        let inspect = |path: &Path| {
            if path == denied {
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            } else {
                fs::symlink_metadata(path).map(|metadata| metadata.is_dir())
            }
        };
        let groups = [RetireGroup {
            name: "home",
            entries: &entries,
        }];

        let backup = temp.path().join("backup");
        let error = retire_entries_with(
            &groups,
            RetireDisposition::Archive { backup: &backup },
            inspect,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("rolled back"), "{error:#}");
        assert!(home.join("data/MANIFEST").is_file());
        assert!(!backup.exists());

        let error = retire_entries_with(&groups, RetireDisposition::Delete, inspect).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("after deleting ["), "{message}");
        assert!(
            message.contains(&home.join("data").display().to_string()),
            "{message}"
        );
        assert!(
            home.join("runtime.json").is_file(),
            "the uninspectable entry stays"
        );
    }

    #[test]
    fn archive_preflight_refuses_a_cross_filesystem_rename_before_anything_moves() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        write(&home.join("data/MANIFEST"), "store");
        let entries = vec![home.join("data"), home.join("absent")];
        let backup = temp.path().join("backup");
        let groups = [RetireGroup {
            name: "home",
            entries: &entries,
        }];
        archive_preflight_with(&groups, &backup, |_| Ok(1)).unwrap();
        let error = archive_preflight_with(&groups, &backup, |path| {
            if path == home.join("data") {
                Ok(2)
            } else if path == home.join("absent") {
                Err(std::io::Error::from(std::io::ErrorKind::NotFound))
            } else {
                Ok(1)
            }
        })
        .unwrap_err();
        assert!(
            error.to_string().contains("different filesystem"),
            "{error}"
        );
        assert!(home.join("data/MANIFEST").is_file());
    }

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
