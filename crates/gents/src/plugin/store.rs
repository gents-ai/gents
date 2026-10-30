use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

fn plugins_root(home: &Path) -> PathBuf {
    home.join(crate::store::local::home::PLUGINS_DIR_NAME)
}

fn store_dir(home: &Path) -> PathBuf {
    plugins_root(home).join("store")
}

fn installed_dir(home: &Path) -> PathBuf {
    plugins_root(home).join("installed")
}

fn store_path(home: &Path, digest_hex: &str) -> Result<PathBuf> {
    anyhow::ensure!(
        digest_hex.len() == 64 && digest_hex.chars().all(|c| c.is_ascii_hexdigit()),
        "invalid plugin digest {digest_hex:?}: expected 64 hex characters"
    );
    Ok(store_dir(home).join(format!("{digest_hex}.afb")))
}

/// Where the bytes of an installed plugin live, for a caller that names
/// the artifact to the desktop (a ux plugin produced by it) without
/// reading it here.
pub fn artifact_path(home: &Path, digest_hex: &str) -> Result<PathBuf> {
    store_path(home, digest_hex)
}

fn checked_component(kind: &str, value: &str) -> Result<()> {
    anyhow::ensure!(
        !value.is_empty()
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
        "invalid plugin {kind} {value:?}: use letters, digits, underscore or hyphen"
    );
    Ok(())
}

pub fn parse_coordinate(coordinate: &str) -> Result<(&str, &str)> {
    let (namespace, name) = coordinate
        .split_once('/')
        .with_context(|| format!("{coordinate:?} is not a plugin; name it as namespace/name"))?;
    checked_component("namespace", namespace)?;
    checked_component("name", name)?;
    Ok((namespace, name))
}

fn record_path(home: &Path, namespace: &str, name: &str) -> Result<PathBuf> {
    checked_component("namespace", namespace)?;
    checked_component("name", name)?;
    Ok(installed_dir(home)
        .join(namespace)
        .join(format!("{name}.json")))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledPlugin {
    pub namespace: String,
    pub name: String,
    pub version: String,
    pub digest: String,
    pub language: String,
    pub declaration: crate::pack::PackPlugin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granted: Option<crate::plugin::Manifold>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pack_coordinate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pack_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_binding: Option<super::model_calls::ModelBinding>,
    /// Each collection of `declaration.data` and the field sets of its
    /// `@index(unique: true)` declarations in the pack's own SDL, computed at
    /// install so a call never reads SDL.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub data_unique_keys: std::collections::BTreeMap<String, Vec<Vec<String>>>,
    /// Each collection of `declaration.data` and its field names in the pack's
    /// own SDL, from the same install-time parse as `data_unique_keys`; a
    /// write to a collection absent here is refused.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub data_collection_fields: std::collections::BTreeMap<String, Vec<String>>,
}

impl InstalledPlugin {
    /// The data grant this record carries, or `None` when the plugin declares
    /// no data.
    pub fn data_grant(&self) -> Option<crate::plugin::data::PluginData> {
        self.declaration
            .data
            .clone()
            .map(|declaration| crate::plugin::data::PluginData {
                declaration,
                collection_fields: self.data_collection_fields.clone(),
                unique_keys: self.data_unique_keys.clone(),
            })
    }

    pub fn ceiling(&self) -> crate::plugin::Manifold {
        self.granted
            .clone()
            .unwrap_or_else(crate::plugin::Manifold::sealed)
    }
}

pub fn check_plugin_ownership(
    home: &Path,
    namespace: &str,
    name: &str,
    pack_coordinate: &str,
) -> Result<()> {
    let existing = match read_record(home, namespace, name) {
        Ok(existing) => existing,
        Err(error) if is_not_found(&error) => return Ok(()),
        Err(error) => return Err(error).context("reading existing plugin ownership"),
    };
    match existing.owner_pack_coordinate {
        Some(owner) if owner != pack_coordinate => Err(anyhow::anyhow!(
            "{namespace}/{name} is already installed by pack {owner}; installing it from {pack_coordinate} \
             would overwrite that pack's plugin. Remove {owner} first, or install it again to replace its own plugin."
        )),
        _ => Ok(()),
    }
}

pub(crate) fn is_not_found(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
    })
}

pub fn owns_plugin_record(
    home: &Path,
    namespace: &str,
    name: &str,
    pack_coordinate: &str,
    digest: &str,
) -> bool {
    read_record(home, namespace, name).is_ok_and(|record| {
        match record.owner_pack_coordinate.as_deref() {
            Some(owner) => owner == pack_coordinate,
            None => record.digest == digest,
        }
    })
}

pub fn remove_record_if_owned(
    home: &Path,
    namespace: &str,
    name: &str,
    pack_coordinate: &str,
    digest: &str,
) -> Result<bool> {
    let guard = try_lock_store(home)?;
    remove_record_if_owned_locked(&guard, home, namespace, name, pack_coordinate, digest)
}

pub(crate) fn remove_record_if_owned_locked(
    guard: &StoreGuard,
    home: &Path,
    namespace: &str,
    name: &str,
    pack_coordinate: &str,
    digest: &str,
) -> Result<bool> {
    guard.for_home(home)?;
    let record = match read_record(home, namespace, name) {
        Ok(record) => record,
        Err(error) if is_not_found(&error) => return Ok(false),
        Err(error) => return Err(error).context("reading plugin ownership before removal"),
    };
    let owned = match record.owner_pack_coordinate.as_deref() {
        Some(owner) => owner == pack_coordinate,
        None => record.digest == digest,
    };
    if !owned {
        return Ok(false);
    }
    let path = record_path(home, namespace, name)?;
    std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    Ok(true)
}

pub fn grant_on_install(
    home: &Path,
    namespace: &str,
    plugin: &crate::pack::PackPlugin,
    consent: bool,
) -> Result<Option<crate::plugin::Manifold>> {
    let previous = match read_record(home, namespace, &plugin.name) {
        Ok(record) => Some(record),
        Err(error) if is_not_found(&error) => None,
        Err(error) => return Err(error).context("reading the existing plugin grant"),
    };
    let previous_limits = previous
        .as_ref()
        .filter(|record| record.granted.is_some())
        .and_then(|record| record.declaration.limits.as_ref());
    anyhow::ensure!(
        super::authority::limits_consented(plugin.limits.as_ref(), previous_limits, consent),
        "plugin {} asks for increased resource limits; install it with --grant-authority to allow that",
        plugin.name
    );
    let previous_ceiling = previous.as_ref().map(InstalledPlugin::ceiling);
    let granted = super::authority::grant_for(plugin, previous_ceiling.as_ref(), consent)?;
    Ok((granted != super::Manifold::sealed() || plugin.limits.is_some()).then_some(granted))
}

pub fn store_bytes(home: &Path, digest_hex: &str, bytes: &[u8]) -> Result<()> {
    let path = store_path(home, digest_hex)?;
    let dir = store_dir(home);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    crate::pack_registry::stage_and_persist(&dir, &path, bytes)
}

pub fn content_digest_hex(bytes: &[u8]) -> String {
    format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(bytes))
}

pub fn read_bytes(home: &Path, digest_hex: &str) -> Result<Vec<u8>> {
    let path = store_path(home, digest_hex)?;
    std::fs::read(&path)
        .with_context(|| format!("reading the installed plugin bytes at {}", path.display()))
}

pub struct StoreGuard {
    home: PathBuf,
    _lock: crate::file_lock::FileLock,
}

impl StoreGuard {
    /// Prevents a held lock for one home from authorizing mutations in another.
    pub fn for_home(&self, home: &Path) -> Result<()> {
        anyhow::ensure!(
            self.home.as_path() == home,
            "the plugin store lock in use belongs to {}, not {}",
            self.home.display(),
            home.display()
        );
        Ok(())
    }
}

/// The shared install lock and exclusive release lock prevent deleting bytes
/// after a concurrent install writes them but before its record is visible.
pub fn lock_store(home: &Path, exclusive: bool) -> Result<StoreGuard> {
    let path = plugins_root(home).join(".store.lock");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    let lock = if exclusive {
        crate::file_lock::FileLock::exclusive(file)
    } else {
        crate::file_lock::FileLock::shared(file)
    }
    .with_context(|| format!("locking {}", path.display()))?;
    Ok(StoreGuard {
        home: home.to_owned(),
        _lock: lock,
    })
}

/// Async mutation owners use a nonblocking lock attempt to avoid parking a Tokio worker.
pub fn try_lock_store(home: &Path) -> Result<StoreGuard> {
    let path = plugins_root(home).join(".store.lock");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    let lock = match crate::file_lock::FileLock::try_exclusive(file) {
        Ok(lock) => lock,
        Err(std::fs::TryLockError::WouldBlock) => {
            anyhow::bail!("plugin store is busy; retry this pack operation")
        }
        Err(std::fs::TryLockError::Error(error)) => {
            return Err(error).with_context(|| format!("locking {}", path.display()));
        }
    };
    Ok(StoreGuard {
        home: home.to_owned(),
        _lock: lock,
    })
}

pub fn release_unreferenced_bytes(home: &Path, digests: &BTreeSet<String>) -> Result<Vec<String>> {
    let _lock = try_lock_store(home)?;
    let referenced: BTreeSet<String> = list_records(home)?
        .into_iter()
        .map(|record| record.digest)
        .collect();
    let mut released = Vec::new();
    for digest in digests {
        if referenced.contains(digest) {
            continue;
        }
        let hex = digest
            .strip_prefix("sha256:")
            .with_context(|| format!("plugin byte digest {digest:?} has no sha256: prefix"))?;
        let path = store_path(home, hex)?;
        match std::fs::remove_file(&path) {
            Ok(()) => released.push(digest.clone()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("removing {}", path.display()))
            }
        }
    }
    released.sort();
    Ok(released)
}

pub fn write_record(home: &Path, record: &InstalledPlugin) -> Result<()> {
    let path = record_path(home, &record.namespace, &record.name)?;
    let parent = path
        .parent()
        .context("installed-plugin record has no parent")?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let bytes =
        serde_json::to_vec_pretty(record).context("encoding the installed-plugin record")?;
    // Readers take no lock: each write stages a temporary file and renames
    // it into place, so a reader only ever observes a complete record.
    let mut staged = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("staging a plugin record in {}", parent.display()))?;
    std::io::Write::write_all(&mut staged, &bytes).context("writing the staged record")?;
    staged
        .persist(&path)
        .map_err(|error| error.error)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

pub fn read_record(home: &Path, namespace: &str, name: &str) -> Result<InstalledPlugin> {
    let path = record_path(home, namespace, name)?;
    let bytes = std::fs::read(&path).with_context(|| {
        format!(
            "{namespace}/{name} has no installed-plugin record at {}",
            path.display()
        )
    })?;
    serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
}

pub fn remove_record_locked(
    guard: &StoreGuard,
    home: &Path,
    namespace: &str,
    name: &str,
) -> Result<InstalledPlugin> {
    guard.for_home(home)?;
    remove_record(home, namespace, name)
}

pub fn remove_record(home: &Path, namespace: &str, name: &str) -> Result<InstalledPlugin> {
    let record = read_record(home, namespace, name)?;
    let path = record_path(home, namespace, name)?;
    std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    Ok(record)
}

pub fn list_records(home: &Path) -> Result<Vec<InstalledPlugin>> {
    let root = installed_dir(home);
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for namespace_entry in
        std::fs::read_dir(&root).with_context(|| format!("reading {}", root.display()))?
    {
        let namespace_path = namespace_entry?.path();
        if !namespace_path.is_dir() {
            continue;
        }
        for record_entry in std::fs::read_dir(&namespace_path)
            .with_context(|| format!("reading {}", namespace_path.display()))?
        {
            let path = record_entry?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let bytes =
                std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            records.push(
                serde_json::from_slice::<InstalledPlugin>(&bytes)
                    .with_context(|| format!("parsing {}", path.display()))?,
            );
        }
    }
    records.sort_by(|a, b| (&a.namespace, &a.name).cmp(&(&b.namespace, &b.name)));
    Ok(records)
}

pub fn bound_to_profile(home: &Path, node_did: &str, profile_id: &str) -> Result<Vec<String>> {
    Ok(list_records(home)?
        .into_iter()
        .filter(|record| {
            record.model_binding.as_ref().is_some_and(|binding| {
                binding.node_did == node_did && binding.profile_id == profile_id
            })
        })
        .map(|record| format!("{}/{}", record.namespace, record.name))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_coordinate_that_would_escape_the_plugins_directory_is_refused() {
        let home = Path::new("/tmp/plugin-store-test");
        for (namespace, name) in [
            ("../../etc", "passwd"),
            ("gents", "../../../escape"),
            ("..", "x"),
            ("", "x"),
            ("gents", ""),
            ("ge/nts", "x"),
        ] {
            record_path(home, namespace, name)
                .expect_err(&format!("{namespace:?}/{name:?} must be refused"));
        }
        record_path(home, "gents", "format_check").expect("an ordinary coordinate is fine");
    }

    #[test]
    fn a_digest_that_is_not_a_digest_is_refused() {
        let home = Path::new("/tmp/plugin-store-test");
        for digest in ["../escape", "", "zz", &"f".repeat(63), &"g".repeat(64)] {
            store_path(home, digest).expect_err(&format!("{digest:?} must be refused"));
        }
        store_path(home, &"a".repeat(64)).expect("a real digest is fine");
    }

    fn sample_record(
        namespace: &str,
        name: &str,
        owner_pack_coordinate: Option<&str>,
    ) -> InstalledPlugin {
        InstalledPlugin {
            namespace: namespace.to_owned(),
            name: name.to_owned(),
            version: "1.0.0".into(),
            digest: format!("sha256:{}", "a".repeat(64)),
            language: "rust".into(),
            declaration: crate::pack::PackPlugin {
                name: name.to_owned(),
                description: "test".into(),
                artifact: format!("plugins/{name}.afb"),
                source: None,
                language: "rust".into(),
                input_schema: serde_json::json!({"type": "object"}),
                manifold: None,
                instructions: None,
                bind_dir: None,
                limits: None,
                model_slot: None,
                service: None,
                ..Default::default()
            },
            granted: None,
            instructions: None,
            owner_pack_coordinate: owner_pack_coordinate.map(str::to_owned),
            owner_pack_digest: owner_pack_coordinate.map(|_| "sha256:pack".to_owned()),
            model_binding: None,
            data_unique_keys: Default::default(),
            data_collection_fields: Default::default(),
        }
    }

    #[test]
    fn ownership_refuses_a_different_pack_coordinate() {
        let home = tempfile::tempdir().unwrap();
        write_record(
            home.path(),
            &sample_record("acme", "echo", Some("acme/widget")),
        )
        .unwrap();

        let error = check_plugin_ownership(home.path(), "acme", "echo", "acme/gadget").unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("acme/widget"), "{message}");
        assert!(message.contains("acme/gadget"), "{message}");
    }

    #[test]
    fn ownership_allows_the_same_coordinate_and_an_unowned_or_absent_record() {
        let home = tempfile::tempdir().unwrap();
        check_plugin_ownership(home.path(), "acme", "echo", "acme/widget").unwrap();

        write_record(
            home.path(),
            &sample_record("acme", "echo", Some("acme/widget")),
        )
        .unwrap();
        check_plugin_ownership(home.path(), "acme", "echo", "acme/widget").unwrap();

        write_record(home.path(), &sample_record("acme", "unowned", None)).unwrap();
        check_plugin_ownership(home.path(), "acme", "unowned", "acme/anything").unwrap();
    }

    #[test]
    fn owns_plugin_record_is_true_only_for_the_owning_coordinate() {
        let home = tempfile::tempdir().unwrap();
        write_record(
            home.path(),
            &sample_record("acme", "widget-echo", Some("acme/widget")),
        )
        .unwrap();
        write_record(
            home.path(),
            &sample_record("acme", "gadget-echo", Some("acme/gadget")),
        )
        .unwrap();
        write_record(home.path(), &sample_record("acme", "legacy-echo", None)).unwrap();
        let legacy_digest = format!("sha256:{}", "a".repeat(64));

        assert!(owns_plugin_record(
            home.path(),
            "acme",
            "widget-echo",
            "acme/widget",
            &legacy_digest
        ));
        assert!(!owns_plugin_record(
            home.path(),
            "acme",
            "gadget-echo",
            "acme/widget",
            &legacy_digest
        ));
        assert!(owns_plugin_record(
            home.path(),
            "acme",
            "legacy-echo",
            "acme/widget",
            &legacy_digest
        ));
        assert!(!owns_plugin_record(
            home.path(),
            "acme",
            "legacy-echo",
            "acme/widget",
            "sha256:another-artifact"
        ));
        assert!(!owns_plugin_record(
            home.path(),
            "acme",
            "not-installed",
            "acme/widget",
            &legacy_digest
        ));
    }

    #[test]
    fn guarded_removal_rechecks_owner_under_the_store_lock() {
        let home = tempfile::tempdir().unwrap();
        write_record(
            home.path(),
            &sample_record("acme", "echo", Some("acme/widget")),
        )
        .unwrap();

        assert!(!remove_record_if_owned(
            home.path(),
            "acme",
            "echo",
            "acme/gadget",
            "sha256:unused",
        )
        .unwrap());
        assert!(read_record(home.path(), "acme", "echo").is_ok());

        assert!(remove_record_if_owned(
            home.path(),
            "acme",
            "echo",
            "acme/widget",
            "sha256:unused",
        )
        .unwrap());
        assert!(is_not_found(
            &read_record(home.path(), "acme", "echo").unwrap_err()
        ));
    }

    #[test]
    fn malformed_plugin_record_is_not_treated_as_missing_ownership() {
        let home = tempfile::tempdir().unwrap();
        let path = record_path(home.path(), "acme", "echo").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let malformed = b"{not-json";
        std::fs::write(&path, malformed).unwrap();

        assert!(check_plugin_ownership(home.path(), "acme", "echo", "acme/widget").is_err());
        assert!(remove_record_if_owned(
            home.path(),
            "acme",
            "echo",
            "acme/widget",
            "sha256:unused",
        )
        .is_err());
        assert_eq!(std::fs::read(path).unwrap(), malformed);
    }

    #[test]
    fn release_keeps_bytes_another_record_references_and_frees_the_rest() {
        let home = tempfile::tempdir().unwrap();
        let shared_digest = format!("sha256:{}", "a".repeat(64));
        let orphaned_digest = format!("sha256:{}", "b".repeat(64));
        store_bytes(home.path(), &"a".repeat(64), b"shared bytes").unwrap();
        store_bytes(home.path(), &"b".repeat(64), b"orphaned bytes").unwrap();

        write_record(
            home.path(),
            &InstalledPlugin {
                digest: shared_digest.clone(),
                ..sample_record("acme", "kept-one", Some("acme/widget"))
            },
        )
        .unwrap();
        write_record(
            home.path(),
            &InstalledPlugin {
                digest: shared_digest.clone(),
                ..sample_record("zeta", "kept-two", Some("zeta/widget"))
            },
        )
        .unwrap();

        let mut candidates = BTreeSet::new();
        candidates.insert(shared_digest.clone());
        candidates.insert(orphaned_digest.clone());
        let released = release_unreferenced_bytes(home.path(), &candidates).unwrap();

        assert_eq!(released, vec![orphaned_digest]);
        assert!(
            read_bytes(home.path(), &"a".repeat(64)).is_ok(),
            "shared bytes stay"
        );
        assert!(
            read_bytes(home.path(), &"b".repeat(64)).is_err(),
            "orphaned bytes are released"
        );
    }
}

#[cfg(test)]
#[path = "store_guard_tests.rs"]
mod store_guard_tests;
