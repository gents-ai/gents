use crate::error::{BridgeError, BridgeErrorCode};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use ts_rs::TS;

use crate::preference::{StoredManagedServer, MANAGED_SERVER_CONFIG};
const ARCHIVE_CONSEQUENCE: &str = "The selected local state is moved into a timestamped backup. Nothing is imported into the new home.";
const DELETE_CONSEQUENCE: &str =
    "The selected local state is permanently deleted. Nothing is imported into the new home.";
const FULL_ARCHIVE_CONSEQUENCE: &str = "The managed home, client state and service definition are backed up. Browser storage and caches are permanently cleared and are not included in the backup.";
const FULL_DELETE_CONSEQUENCE: &str = "The managed home, client state and service definition are permanently deleted. Browser storage and caches are permanently cleared.";

/// How the desktop client state relates to the managed home, as
/// `managed-server.json`'s `reviewedFor` records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientBinding {
    /// No reviewed home is remembered, or its home cannot be read.
    Unbound,
    /// The client was paired with this exact home and identity.
    BoundToHome,
    /// The reviewed home is gone or now carries another identity: the
    /// pairing names a node that no longer exists here.
    Orphaned,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ManagedServerResetResult {
    /// The managed runtime home whose own entries are in scope, if any.
    pub managed_home: Option<String>,
    /// The desktop client state root whose client entries are in scope, if any.
    pub desktop_home: Option<String>,
    /// The stores this version refused, and why.
    pub stores: Vec<IncompatibleStoreView>,
    /// Confirmation text for archiving.
    pub confirmation: String,
    /// Confirmation text for permanent deletion; absent when deletion is not
    /// offered (a store another, possibly newer, version wrote).
    pub delete_confirmation: Option<String>,
    pub consequence: String,
    pub completed: bool,
    /// What a completed reset did.
    pub disposition: Option<HomeResetDisposition>,
    /// Where an archive moved the retired entries.
    pub backup_path: Option<String>,
    /// Entries an archive moves, exactly.
    pub planned_paths: Vec<String>,
    /// Entries a deletion removes, exactly. Refusal recovery can archive
    /// more keys; explicit full reset preserves unrelated keys in both modes.
    pub delete_paths: Vec<String>,
    /// Entries a completed reset archived or deleted.
    pub retired_paths: Vec<String>,
    /// Entries of the managed home left untouched: other nodes' homes,
    /// backups, user files and anything outside the runtime's inventory.
    pub retained_paths: Vec<String>,
    /// Browser data cleared through WebKit in both modes; never archived.
    pub browser_paths: Vec<String>,
    /// Browser data whose native clearing completed during this reset.
    pub cleared_browser_paths: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, TS)]
#[serde(rename_all = "lowercase")]
#[ts(rename_all = "lowercase")]
pub enum HomeResetDisposition {
    Archive,
    Delete,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, TS)]
#[serde(rename_all = "lowercase")]
#[ts(rename_all = "lowercase")]
pub enum IncompatibleStoreScope {
    /// The managed runtime's store in its home.
    Runtime,
    /// The desktop client's own store.
    Client,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct IncompatibleStoreView {
    pub scope: IncompatibleStoreScope,
    pub path: String,
    pub detail: String,
    /// The store is known to come from an earlier release. A store another
    /// (possibly newer) build wrote is not offered for deletion by default.
    pub older: bool,
    /// An identity key an older build wrote with unsafe permissions; `path`
    /// is the key (or the home's key directory).
    pub unsafe_key: bool,
}

/// The canonical user home a reset measures its roots against. Without it a
/// broad root cannot be recognized, so the reset is refused rather than
/// guessed at.
pub fn resolve_user_home(user_home: Option<PathBuf>) -> Result<PathBuf, BridgeError> {
    let user_home = user_home
        .filter(|home| !home.as_os_str().is_empty())
        .ok_or_else(|| {
            BridgeError::new(
                BridgeErrorCode::InvalidArgument,
                "The user home directory cannot be determined; reset was refused.",
            )
        })?;
    std::fs::canonicalize(&user_home).map_err(|error| {
        BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            format!(
                "The user home directory {} cannot be resolved ({error}); reset was refused.",
                user_home.display()
            ),
        )
    })
}

/// A root whose entries a reset may retire: not the filesystem root, and
/// neither the user's home directory nor any ancestor of it. `root` and
/// `user_home` are canonical, so aliases of either compare equal.
pub fn ensure_retirable_root(
    root: &Path,
    label: &str,
    user_home: &Path,
) -> Result<(), BridgeError> {
    let broad = root.parent().is_none() || user_home.starts_with(root);
    if broad {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            format!(
                "The {label} {} is too broad to reset; reset was refused.",
                root.display()
            ),
        ));
    }
    Ok(())
}

fn canonical_root(path: &Path, label: &str) -> Result<PathBuf, BridgeError> {
    std::fs::canonicalize(path).map_err(|error| {
        BridgeError::new(
            BridgeErrorCode::Backend,
            format!("Cannot resolve {label} {}: {error}", path.display()),
        )
    })
}

/// The exact owned entries retired by a refused-store or explicit full reset.
#[derive(Debug)]
pub struct HomeResetPlan {
    /// The canonical managed home and its refused store. A home without a
    /// refused store is in scope when explicitly reset or when the retired
    /// client state was paired with it.
    pub runtime: Option<(
        PathBuf,
        Option<gents::store::local::storage_backend::IncompatibleStore>,
    )>,
    client: Option<gents::store::local::storage_backend::IncompatibleStore>,
    /// The client state is retired because its paired node is gone.
    orphaned_client: bool,
    desktop_root: PathBuf,
    runtime_entries: gents::store::local::home::HomeEntries,
    /// The runtime entries a Delete removes: the owned entries, except that
    /// `keys/` is narrowed to the managed home's identity and recorded store key (other homes
    /// may keep keys there). Explicit full reset also uses this scope for
    /// archiving; refused-store recovery can archive the whole key directory.
    delete_home_entries: Vec<PathBuf>,
    /// `keys/`, removed after a Delete only if nothing else is left in it.
    delete_prunes_keys: Option<PathBuf>,
    archive_home_entries: Vec<PathBuf>,
    archive_keys: Vec<PathBuf>,
    /// Present client-state entries in scope.
    client_entries: Vec<PathBuf>,
    store_keys: Vec<RetiredStoreKey>,
    full_reset: bool,
    host_entries: Vec<(String, PathBuf)>,
    browser_entries: Vec<PathBuf>,
    /// The decoded records that prove each root is Gents'; retired last.
    ownership_records: Vec<PathBuf>,
}

/// Keychain custody outlives filesystem retirement. Its record stays until
/// deletion succeeds, so a denied Keychain operation can be retried. Archives
/// retain both the key and its record so the archived store remains readable.
#[derive(Debug)]
struct RetiredStoreKey {
    record: gents::store::local::store_key::StoreEncryption,
    key_file: PathBuf,
    record_files: Vec<PathBuf>,
}

fn recorded_store_key(
    record_file: PathBuf,
    key_file: PathBuf,
    home: bool,
) -> Option<RetiredStoreKey> {
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&record_file).ok()?).ok()?;
    let value = if home {
        value.get("store_encryption")?.clone()
    } else {
        value
    };
    Some(RetiredStoreKey {
        record: serde_json::from_value(value).ok()?,
        key_file,
        record_files: vec![record_file],
    })
}

fn add_store_key(keys: &mut Vec<RetiredStoreKey>, key: RetiredStoreKey) {
    if let Some(existing) = keys
        .iter_mut()
        .find(|existing| existing.record == key.record && existing.key_file == key.key_file)
    {
        existing.record_files.extend(key.record_files);
    } else {
        keys.push(key);
    }
}

/// The regular files the desktop client wrote that decode, through their
/// owners, as its records: the identity record, the store's custody record
/// or pending upgrade journal, and a preference reviewed for a node. A path
/// that merely exists proves nothing.
fn decoded_desktop_records(paths: &crate::client::DesktopPaths, preference: &Path) -> Vec<PathBuf> {
    let read = |path: &Path| {
        std::fs::symlink_metadata(path)
            .is_ok_and(|metadata| metadata.is_file())
            .then(|| std::fs::read(path).ok())
            .flatten()
    };
    let journal = gents::store::local::store_key::upgrade::journal_path(paths.node_data_dir());
    [
        (
            paths.principal_metadata_path().to_path_buf(),
            crate::client::PrincipalIdentity::recorded_did(paths.principal_metadata_path())
                .is_some(),
        ),
        (
            paths.store_encryption_path().to_path_buf(),
            read(paths.store_encryption_path()).is_some_and(|bytes| {
                serde_json::from_slice::<gents::store::local::store_key::StoreEncryption>(&bytes)
                    .is_ok()
            }),
        ),
        (
            journal.clone(),
            read(&journal).is_some()
                && gents::store::local::store_key::upgrade::pending_record(paths.node_data_dir())
                    .is_ok_and(|record| record.is_some()),
        ),
        (
            preference.to_path_buf(),
            read(preference)
                .and_then(|bytes| serde_json::from_slice::<StoredManagedServer>(&bytes).ok())
                .and_then(|stored| stored.reviewed_for)
                .is_some_and(|reviewed| {
                    reviewed.node_did.starts_with("did:key:")
                        && !reviewed.node_home.trim().is_empty()
                }),
        ),
    ]
    .into_iter()
    .filter_map(|(path, decoded)| decoded.then_some(path))
    .collect()
}

fn upgrade_retirement(
    data: &Path,
    key_file: PathBuf,
) -> Result<(Vec<PathBuf>, Option<RetiredStoreKey>), BridgeError> {
    let read = || -> anyhow::Result<_> {
        let entries = gents::store::local::store_key::upgrade::retirement_entries(data)?;
        let key = gents::store::local::store_key::upgrade::pending_record(data)?.map(|record| {
            RetiredStoreKey {
                record,
                key_file,
                record_files: vec![gents::store::local::store_key::upgrade::journal_path(data)],
            }
        });
        Ok((entries, key))
    };
    read().map_err(|error| {
        BridgeError::new(
            BridgeErrorCode::Backend,
            format!("Inspecting store upgrade: {error:#}"),
        )
    })
}

/// The managed home's own key file inside its `keys/` directory, as its
/// `init.json` names it (`key_path`, else the default for `node_name`).
fn own_home_key(home: &Path, keys: &Path) -> Option<PathBuf> {
    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(gents::store::local::home::init_config_path(home)).ok()?,
    )
    .ok()?;
    let key = record
        .get("key_path")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            record
                .get("node_name")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(|name| gents::store::local::home::default_key_path(home, name))
        })?;
    let parent = std::fs::canonicalize(key.parent()?).ok()?;
    let name = key.file_name()?;
    (parent == keys).then(|| keys.join(name))
}

pub fn lock_home_for_retirement(
    home: &Path,
) -> Result<gents::store::local::home::StoreLock, BridgeError> {
    gents::store::local::home::lock_home_store(home).map_err(|error| {
        BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            gents_protocol::error_chain::ErrorChain(&error).to_string(),
        )
    })
}

fn is_store_lock(path: &Path) -> bool {
    path.parent().is_some_and(|home| {
        gents::store::local::home::default_data_dir(home)
            .file_name()
            .is_some_and(|data| {
                path.file_name()
                    .is_some_and(|name| *name == *format!("{}.lock", data.to_string_lossy()))
            })
    })
}

pub fn present(path: &Path) -> Result<bool, BridgeError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(BridgeError::new(
            BridgeErrorCode::Backend,
            format!("Inspecting {}: {error}", path.display()),
        )),
    }
}

pub fn plan_home_reset(
    runtime_home: Option<&Path>,
    runtime_store: Option<gents::store::local::storage_backend::IncompatibleStore>,
    client_store: Option<gents::store::local::storage_backend::IncompatibleStore>,
    desktop_paths: &crate::client::DesktopPaths,
    user_home: &Path,
    binding: ClientBinding,
) -> Result<HomeResetPlan, BridgeError> {
    plan_reset(
        runtime_home,
        runtime_store,
        client_store,
        desktop_paths,
        user_home,
        binding,
        false,
    )
}

/// Explicit reset owns only the selected home, its client, and the desktop's
/// service definition. Browser data is listed separately for WebKit clearing
/// in either mode; only WebKit completion authorizes reporting it cleared.
/// Service shutdown and client exclusion precede filesystem retirement.
pub fn plan_full_reset(
    runtime_home: Option<&Path>,
    desktop_paths: &crate::client::DesktopPaths,
    user_home: &Path,
    service_definition: &Path,
) -> Result<HomeResetPlan, BridgeError> {
    let mut plan = plan_reset(
        runtime_home,
        None,
        None,
        desktop_paths,
        user_home,
        ClientBinding::Unbound,
        true,
    )?;
    let mut paths = vec![("service", service_definition.to_path_buf())];
    if cfg!(target_os = "macos") {
        paths.extend([
            (
                "webkit",
                user_home.join("Library/WebKit/com.source-inc.gents"),
            ),
            (
                "cache",
                user_home.join("Library/Caches/com.source-inc.gents"),
            ),
        ]);
    }
    for (name, path) in paths {
        let exists = present(&path)?;
        if name == "service" && !exists {
            continue;
        }
        let path = if exists {
            reject_symlink(&path, name)?;
            canonical_root(&path, name)?
        } else {
            path
        };
        ensure_retirable_root(&path, name, user_home)?;
        let overlaps = |other: &Path| path.starts_with(other) || other.starts_with(&path);
        let runtime_root = plan
            .runtime
            .as_ref()
            .map(|(home, _)| home.as_path())
            .or(runtime_home);
        if overlaps(&plan.desktop_root)
            || runtime_root.is_some_and(overlaps)
            || plan.host_entries.iter().any(|(_, other)| overlaps(other))
            || plan.browser_entries.iter().any(|other| overlaps(other))
        {
            return Err(BridgeError::new(BridgeErrorCode::InvalidArgument,
                format!("The {name} path {} overlaps a managed home or another reset group; choose separate paths before resetting.", path.display())));
        }
        if name == "service" {
            plan.host_entries.push((name.into(), path));
        } else {
            plan.browser_entries.push(path);
        }
    }
    let planned: Vec<_> = plan.planned(HomeResetDisposition::Archive).collect();
    for (index, path) in planned.iter().enumerate() {
        if planned[index + 1..]
            .iter()
            .any(|other| path.starts_with(other) || other.starts_with(path))
        {
            return Err(BridgeError::new(BridgeErrorCode::InvalidArgument,
                "Reset groups overlap; choose separate managed home and desktop paths before resetting."));
        }
    }
    for path in plan
        .client_entries
        .iter()
        .chain(plan.host_entries.iter().map(|(_, path)| path))
        .chain(&plan.browser_entries)
    {
        if plan
            .runtime_entries
            .retained
            .iter()
            .any(|retained| retained.starts_with(path))
        {
            return Err(BridgeError::new(BridgeErrorCode::InvalidArgument,
                "Reset would include a retained home or user file; choose separate managed home and desktop paths."));
        }
    }
    Ok(plan)
}

fn plan_reset(
    runtime_home: Option<&Path>,
    runtime_store: Option<gents::store::local::storage_backend::IncompatibleStore>,
    client_store: Option<gents::store::local::storage_backend::IncompatibleStore>,
    desktop_paths: &crate::client::DesktopPaths,
    user_home: &Path,
    binding: ClientBinding,
    full_reset: bool,
) -> Result<HomeResetPlan, BridgeError> {
    let inspect = |path: &Path| {
        gents::store::local::storage_backend::incompatible_store_kind(path).map_err(|error| {
            BridgeError::new(
                BridgeErrorCode::Backend,
                format!("Inspecting {}: {error}", path.display()),
            )
        })
    };
    let runtime_home = match runtime_home {
        Some(configured_home) if present(configured_home)? => {
            reject_symlink(configured_home, "managed home")?;
            let home = canonical_root(configured_home, "managed home")?;
            let data = gents::store::local::home::default_data_dir(&home);
            reject_symlink_if_present(&data, "managed data")?;
            reject_symlink_if_present(
                &gents::store::local::home::init_config_path(&home),
                "managed init marker",
            )?;
            let marked = inspect(&data)?.map(|kind| {
                gents::store::local::storage_backend::IncompatibleStore {
                    kind,
                    data_path: data.clone(),
                }
            });
            marked
                .or(runtime_store.map(|store| match store.kind {
                    gents::store::local::storage_backend::IncompatibleStoreKind::InsecureKey => {
                        store
                    }
                    _ => gents::store::local::storage_backend::IncompatibleStore {
                        data_path: data.clone(),
                        ..store
                    },
                }))
                .map_or(Some((home.clone(), None)), |store| {
                    Some((home, Some(store)))
                })
        }
        _ => None,
    };
    reject_symlink_if_present(desktop_paths.root(), "desktop client state")?;
    reject_symlink_if_present(desktop_paths.node_data_dir(), "desktop client store")?;
    let client = inspect(desktop_paths.node_data_dir())?
        .map(
            |kind| gents::store::local::storage_backend::IncompatibleStore {
                kind,
                data_path: desktop_paths.node_data_dir().to_path_buf(),
            },
        )
        .or(client_store);
    let orphaned_client = binding == ClientBinding::Orphaned;
    let runtime = runtime_home.filter(|(home, store)| {
        // Starting fresh retires the client's pairing with this home; left in
        // place, first-run setup would provision onto it and relaunch the
        // runtime on a store that may not open.
        full_reset
            || store.is_some()
            || (client.is_some()
                && binding == ClientBinding::BoundToHome
                && gents::store::local::home::init_config_path(home).is_file())
    });
    if !full_reset && runtime.is_none() && client.is_none() && !orphaned_client {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "No local store is marked as legacy or incompatible; reset was refused.",
        ));
    }
    let desktop_root = if present(desktop_paths.root())? {
        canonical_root(desktop_paths.root(), "desktop client state")?
    } else {
        desktop_paths.root().to_path_buf()
    };
    if let Some((home, _)) = runtime.as_ref() {
        ensure_retirable_root(home, "managed home", user_home)?;
    }
    let retires_client_state =
        full_reset || runtime.is_some() || client.is_some() || orphaned_client;
    if retires_client_state {
        ensure_retirable_root(&desktop_root, "desktop client state", user_home)?;
    }
    let mut runtime_entries = match runtime.as_ref() {
        Some((home, _)) => {
            gents::store::local::home::home_entries(home, std::slice::from_ref(&desktop_root))
                .map_err(|error| {
                    BridgeError::new(
                        BridgeErrorCode::Backend,
                        format!(
                            "Listing {}: {}",
                            home.display(),
                            gents_protocol::error_chain::ErrorChain(&error)
                        ),
                    )
                })?
        }
        None => gents::store::local::home::HomeEntries::default(),
    };
    // A home proves it is a Gents home only through a readable init marker;
    // names from the runtime inventory alone also occur in user projects.
    let initialized = match runtime.as_ref() {
        Some((home, _)) => {
            let record = gents::store::local::home::read_init_config::<
                serde_json::Value,
                serde_json::Value,
            >(home)
            .ok()
            .flatten();
            if let Some(tool_root) = record
                .as_ref()
                .and_then(|record| record.tool_root.as_deref())
                .map(str::trim)
                .filter(|root| !root.is_empty())
            {
                let tool_root =
                    std::fs::canonicalize(tool_root).unwrap_or_else(|_| PathBuf::from(tool_root));
                if tool_root == *home
                    || runtime_entries
                        .owned
                        .iter()
                        .any(|entry| tool_root.starts_with(entry))
                {
                    return Err(BridgeError::new(
                        BridgeErrorCode::InvalidArgument,
                        format!(
                            "The managed home {} shares its files with the tool root {}; resetting it could remove workspace files, so reset was refused.",
                            home.display(),
                            tool_root.display()
                        ),
                    ));
                }
            }
            record.is_some()
        }
        None => false,
    };
    let home_record = runtime
        .as_ref()
        .filter(|_| initialized)
        .map(|(home, _)| gents::store::local::home::init_config_path(home));
    let mut client_entries = Vec::new();
    // Like a runtime home, a desktop home proves itself only through a record
    // the desktop wrote; its entry names alone also occur in user projects.
    let mut desktop_owned = client.is_some();
    let mut ownership_records = Vec::new();
    if retires_client_state {
        // Entries are derived from the canonical root, never from the raw
        // configured path.
        let canonical = crate::client::DesktopPaths::from_root(&desktop_root);
        let preference = desktop_root.join(MANAGED_SERVER_CONFIG);
        ownership_records.extend(decoded_desktop_records(&canonical, &preference));
        desktop_owned |= !ownership_records.is_empty();
        for entry in canonical
            .client_state_entries()
            .into_iter()
            .chain(std::iter::once(preference))
        {
            if desktop_owned && present(&entry)? {
                client_entries.push(entry);
            }
        }
    }
    if full_reset {
        let lock = crate::client::DesktopPaths::from_root(&desktop_root)
            .peer_directory_path()
            .with_extension("lock");
        client_entries.retain(|entry| *entry != lock);
    }
    if full_reset && desktop_owned && present(&desktop_root.join("logs"))? {
        client_entries.push(desktop_root.join("logs"));
    }
    let mut delete_home_entries = Vec::new();
    let mut delete_prunes_keys = None;
    let mut store_keys = Vec::new();
    if retires_client_state && desktop_owned {
        let canonical = crate::client::DesktopPaths::from_root(&desktop_root);
        if let Some(key) = recorded_store_key(
            canonical.store_encryption_path().to_path_buf(),
            canonical.store_key_path().to_path_buf(),
            false,
        ) {
            add_store_key(&mut store_keys, key);
        }
        let (entries, key) = upgrade_retirement(
            canonical.node_data_dir(),
            canonical.store_key_path().to_path_buf(),
        )?;
        client_entries.extend(entries);
        if let Some(key) = key {
            add_store_key(&mut store_keys, key);
        }
    }
    if let Some((home, _)) = runtime.as_ref() {
        let (entries, upgrade_key) = upgrade_retirement(
            &gents::store::local::home::default_data_dir(home),
            gents::store::local::store_key::home_key_file(home),
        )?;
        runtime_entries
            .retained
            .retain(|path| !entries.contains(path));
        runtime_entries.owned.extend(entries);
        if let Some(key) = recorded_store_key(
            gents::store::local::home::init_config_path(home),
            gents::store::local::store_key::home_key_file(home),
            true,
        ) {
            add_store_key(&mut store_keys, key);
        }
        if let Some(key) = upgrade_key {
            add_store_key(&mut store_keys, key);
        }
        for key in store_keys
            .iter()
            .filter(|key| key.key_file == gents::store::local::store_key::home_key_file(home))
        {
            if matches!(
                key.record.custody,
                gents::store::local::store_key::StoreKeyCustody::File
            ) && std::fs::canonicalize(home.join("keys"))
                .is_ok_and(|keys| keys == home.join("keys"))
                && present(&key.key_file)?
                && (!full_reset || runtime_entries.owned.contains(&home.join("keys")))
            {
                delete_home_entries.push(key.key_file.clone());
            }
        }
        let keys = home.join("keys");
        for entry in &runtime_entries.owned {
            if *entry != keys {
                delete_home_entries.push(entry.clone());
                continue;
            }
            if let Some(key) = own_home_key(home, &keys) {
                if present(&key)? {
                    delete_home_entries.push(key);
                }
            }
            if initialized || !full_reset {
                delete_prunes_keys = Some(keys.clone());
            }
        }
    }
    delete_home_entries.sort();
    delete_home_entries.dedup();
    if let Some((home, _)) = runtime.as_ref().filter(|_| full_reset && !initialized) {
        if let Some(entry) = delete_home_entries.first() {
            return Err(BridgeError::new(
                BridgeErrorCode::InvalidArgument,
                format!(
                    "{} has no readable Gents init.json, so {} cannot be shown to belong to Gents; reset was refused.",
                    home.display(),
                    entry.display()
                ),
            ));
        }
    }
    let mut archive_home_entries = Vec::new();
    let mut archive_keys = Vec::new();
    if full_reset {
        let keys = runtime.as_ref().map(|(home, _)| home.join("keys"));
        for path in &delete_home_entries {
            if keys.as_deref() == path.parent() {
                archive_keys.push(path.clone());
            } else {
                archive_home_entries.push(path.clone());
            }
        }
        if let Some(keys) = keys.filter(|keys| {
            runtime_entries.owned.contains(keys)
                && std::fs::symlink_metadata(keys).is_ok_and(|metadata| metadata.is_dir())
        }) {
            for entry in std::fs::read_dir(&keys).map_err(|error| {
                BridgeError::new(
                    BridgeErrorCode::Backend,
                    format!("Listing {}: {error}", keys.display()),
                )
            })? {
                let path = entry
                    .map_err(|error| BridgeError::new(BridgeErrorCode::Backend, error.to_string()))?
                    .path();
                if !archive_keys.contains(&path) {
                    runtime_entries.retained.push(path);
                }
            }
            runtime_entries.retained.sort();
        }
    }
    Ok(HomeResetPlan {
        runtime,
        client,
        orphaned_client,
        desktop_root,
        runtime_entries,
        delete_home_entries,
        delete_prunes_keys,
        archive_home_entries,
        archive_keys,
        client_entries,
        store_keys,
        full_reset,
        host_entries: Vec::new(),
        browser_entries: Vec::new(),
        ownership_records: home_record.into_iter().chain(ownership_records).collect(),
    })
}

impl HomeResetPlan {
    /// The desktop client state is retired with its runtime: its pairing
    /// names the runtime identity being retired.
    pub fn retires_client_state(&self) -> bool {
        self.full_reset || self.client.is_some() || self.runtime.is_some() || self.orphaned_client
    }

    /// Only records that decoded as evidence wait for the final pass; every
    /// other entry, including undecoded records, goes first.
    fn is_ownership_record(&self, path: &Path) -> bool {
        self.ownership_records.iter().any(|record| record == path)
    }

    fn refused_stores(
        &self,
    ) -> impl Iterator<Item = &gents::store::local::storage_backend::IncompatibleStore> {
        self.runtime
            .iter()
            .filter_map(|(_, store)| store.as_ref())
            .chain(&self.client)
    }

    fn anchor(&self) -> &Path {
        self.runtime
            .as_ref()
            .map_or(self.desktop_root.as_path(), |(home, _)| home.as_path())
    }

    fn home_entries(&self, disposition: HomeResetDisposition) -> &[PathBuf] {
        match disposition {
            HomeResetDisposition::Archive if self.full_reset => &self.delete_home_entries,
            HomeResetDisposition::Archive => &self.runtime_entries.owned,
            HomeResetDisposition::Delete => &self.delete_home_entries,
        }
    }

    fn planned(&self, disposition: HomeResetDisposition) -> impl Iterator<Item = &PathBuf> {
        self.home_entries(disposition)
            .iter()
            .chain(&self.client_entries)
            .chain(self.host_entries.iter().map(|(_, path)| path))
    }

    /// Recovery offers deletion only for known older stores; a store another
    /// build wrote may still be wanted by that build. Explicit full reset
    /// allows deletion because the user chooses to retire even healthy state.
    ///
    /// A paired runtime retired without a refusal was never opened, so its
    /// store is unclassified and may belong to a newer build: archive only.
    fn deletable(&self) -> bool {
        if self.full_reset {
            return true;
        }
        self.runtime
            .as_ref()
            .is_none_or(|(_, store)| store.is_some())
            && self.refused_stores().next().is_some()
            && self.refused_stores().all(|store| store.kind.is_older())
    }

    /// A digest of everything the action would touch or leave, so a
    /// confirmation authorizes exactly the previewed set.
    pub fn digest(&self, disposition: HomeResetDisposition) -> String {
        let mut hasher = blake3::Hasher::new();
        let mut line = |label: &str, path: &Path| {
            hasher.update(label.as_bytes());
            hasher.update(path.as_os_str().as_encoded_bytes());
            hasher.update(b"\n");
        };
        line(
            if self.full_reset {
                "full-reset"
            } else {
                "refused-reset"
            },
            self.anchor(),
        );
        for (name, path) in &self.host_entries {
            line(name, path);
        }
        for path in &self.browser_entries {
            line("clear-browser", path);
        }
        for path in self.home_entries(disposition) {
            line("home", path);
        }
        for path in &self.client_entries {
            line("desktop", path);
        }
        // The store lock appears once the reset takes it; it is never retired.
        for path in self
            .runtime_entries
            .retained
            .iter()
            .filter(|path| !is_store_lock(path))
        {
            line("retained", path);
        }
        for key in &self.store_keys {
            hasher.update(&serde_json::to_vec(&key.record).expect("store encryption serializes"));
        }
        hasher.finalize().to_hex()[..12].to_string()
    }

    pub fn confirmation(&self, disposition: HomeResetDisposition) -> String {
        let anchor = self.anchor().to_string_lossy();
        let digest = self.digest(disposition);
        match disposition {
            HomeResetDisposition::Archive => {
                format!("RESET {anchor} AND ARCHIVE LOCAL HISTORY [{digest}]")
            }
            HomeResetDisposition::Delete => format!("DELETE {anchor} PERMANENTLY [{digest}]"),
        }
    }

    pub fn check_confirmation(
        &self,
        supplied: &str,
        disposition: HomeResetDisposition,
    ) -> Result<(), BridgeError> {
        if disposition == HomeResetDisposition::Delete && !self.deletable() {
            return Err(BridgeError::new(
                BridgeErrorCode::InvalidArgument,
                "A store another Gents version wrote is not deleted from here; back it up or keep it.",
            ));
        }
        if supplied != self.confirmation(disposition) {
            return Err(BridgeError::new(
                BridgeErrorCode::InvalidArgument,
                "The home changed since it was reviewed, or the confirmation does not match this action. Review it again.",
            ));
        }
        Ok(())
    }

    pub fn preview(&self) -> ManagedServerResetResult {
        let view = |scope, store: &gents::store::local::storage_backend::IncompatibleStore| {
            IncompatibleStoreView {
                scope,
                path: store.data_path.to_string_lossy().into_owned(),
                detail: store.to_string(),
                older: store.kind.is_older(),
                unsafe_key: store.kind
                    == gents::store::local::storage_backend::IncompatibleStoreKind::InsecureKey,
            }
        };
        let paths = |paths: &mut dyn Iterator<Item = &PathBuf>| {
            paths
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        ManagedServerResetResult {
            managed_home: self
                .runtime
                .as_ref()
                .map(|(home, _)| home.to_string_lossy().into_owned()),
            desktop_home: self
                .retires_client_state()
                .then(|| self.desktop_root.to_string_lossy().into_owned()),
            stores: self
                .runtime
                .iter()
                .filter_map(|(_, store)| store.as_ref())
                .map(|store| view(IncompatibleStoreScope::Runtime, store))
                .chain(
                    self.client
                        .iter()
                        .map(|store| view(IncompatibleStoreScope::Client, store)),
                )
                .collect(),
            confirmation: self.confirmation(HomeResetDisposition::Archive),
            delete_confirmation: self
                .deletable()
                .then(|| self.confirmation(HomeResetDisposition::Delete)),
            consequence: if self.full_reset {
                FULL_ARCHIVE_CONSEQUENCE
            } else {
                ARCHIVE_CONSEQUENCE
            }
            .into(),
            completed: false,
            disposition: None,
            backup_path: None,
            planned_paths: paths(&mut self.planned(HomeResetDisposition::Archive)),
            delete_paths: if self.deletable() {
                paths(&mut self.planned(HomeResetDisposition::Delete))
            } else {
                Vec::new()
            },
            retired_paths: Vec::new(),
            retained_paths: paths(&mut self.runtime_entries.retained.iter()),
            browser_paths: paths(&mut self.browser_entries.iter()),
            cleared_browser_paths: Vec::new(),
        }
    }

    /// A fresh sibling of the anchor: `<anchor>-backup-<stamp>`.
    pub fn backup_path(&self, stamp: &str) -> Result<PathBuf, BridgeError> {
        let anchor = self.anchor();
        let (Some(parent), Some(name)) = (anchor.parent(), anchor.file_name()) else {
            return Err(BridgeError::new(
                BridgeErrorCode::InvalidArgument,
                "The home has no parent directory to hold a backup.",
            ));
        };
        let base = format!("{}-backup-{stamp}", name.to_string_lossy());
        let mut backup = parent.join(&base);
        for suffix in 1..100_u8 {
            if !present(&backup)? {
                return Ok(backup);
            }
            backup = parent.join(format!("{base}-{suffix}"));
        }
        Err(BridgeError::new(
            BridgeErrorCode::Backend,
            format!("No free backup name next to {}.", anchor.display()),
        ))
    }

    fn groups(
        &self,
        disposition: HomeResetDisposition,
    ) -> Vec<gents::store::local::home::RetireGroup<'_>> {
        let mut groups = vec![
            gents::store::local::home::RetireGroup {
                name: "home",
                entries: if self.full_reset && disposition == HomeResetDisposition::Archive {
                    &self.archive_home_entries
                } else {
                    self.home_entries(disposition)
                },
            },
            gents::store::local::home::RetireGroup {
                name: "desktop",
                entries: &self.client_entries,
            },
        ];
        if self.full_reset && disposition == HomeResetDisposition::Archive {
            groups.push(gents::store::local::home::RetireGroup {
                name: "keys",
                entries: &self.archive_keys,
            });
        }
        groups.extend(self.host_entries.iter().map(|(name, path)| {
            gents::store::local::home::RetireGroup {
                name,
                entries: std::slice::from_ref(path),
            }
        }));
        groups
    }

    pub fn preflight(&self, backup: &Path) -> Result<(), BridgeError> {
        gents::store::local::home::archive_preflight(
            &self.groups(HomeResetDisposition::Archive),
            backup,
        )
        .map_err(|error| {
            BridgeError::new(
                BridgeErrorCode::Backend,
                gents_protocol::error_chain::ErrorChain(&error).to_string(),
            )
        })
    }

    pub fn retire(
        &self,
        disposition: HomeResetDisposition,
        backup: Option<&Path>,
    ) -> Result<ManagedServerResetResult, BridgeError> {
        self.retire_with_key_cleanup(disposition, backup, |record, key_file| {
            Ok(record.delete_key(key_file)?)
        })
    }

    pub fn retire_with_key_cleanup(
        &self,
        disposition: HomeResetDisposition,
        backup: Option<&Path>,
        mut delete_key: impl FnMut(
            &gents::store::local::store_key::StoreEncryption,
            &Path,
        ) -> anyhow::Result<()>,
    ) -> Result<ManagedServerResetResult, BridgeError> {
        let retire_as = match (disposition, backup) {
            (HomeResetDisposition::Archive, Some(backup)) => {
                self.preflight(backup)?;
                gents::store::local::home::RetireDisposition::Archive { backup }
            }
            (HomeResetDisposition::Delete, None) => {
                gents::store::local::home::RetireDisposition::Delete
            }
            _ => {
                return Err(BridgeError::new(
                    BridgeErrorCode::InvalidArgument,
                    "An archive needs a backup location and a deletion has none.",
                ))
            }
        };
        let keychain_keys: Vec<_> = self
            .store_keys
            .iter()
            .filter(|key| {
                disposition == HomeResetDisposition::Delete
                    && matches!(
                        key.record.custody,
                        gents::store::local::store_key::StoreKeyCustody::MacosKeychain { .. }
                    )
            })
            .collect();
        let keep_record = |path: &&PathBuf| {
            !keychain_keys
                .iter()
                .any(|key| key.record_files.contains(*path))
        };
        let home_entries: Vec<_> = self
            .home_entries(disposition)
            .iter()
            .filter(keep_record)
            .filter(|path| {
                !(self.full_reset
                    && disposition == HomeResetDisposition::Archive
                    && self.archive_keys.contains(path))
            })
            .cloned()
            .collect();
        let client_entries: Vec<_> = self
            .client_entries
            .iter()
            .filter(keep_record)
            .cloned()
            .collect();
        let mut groups = vec![
            gents::store::local::home::RetireGroup {
                name: "home",
                entries: &home_entries,
            },
            gents::store::local::home::RetireGroup {
                name: "desktop",
                entries: &client_entries,
            },
        ];
        if self.full_reset && disposition == HomeResetDisposition::Archive {
            groups.push(gents::store::local::home::RetireGroup {
                name: "keys",
                entries: &self.archive_keys,
            });
        }
        groups.extend(self.host_entries.iter().map(|(name, path)| {
            gents::store::local::home::RetireGroup {
                name,
                entries: std::slice::from_ref(path),
            }
        }));
        // Ownership records go last: an interrupted retirement keeps the
        // evidence that lets the next plan finish the remaining entries.
        let (dependents, records): (Vec<_>, Vec<_>) = groups
            .iter()
            .map(|group| {
                let (records, dependents): (Vec<_>, Vec<_>) = group
                    .entries
                    .iter()
                    .cloned()
                    .partition(|path| self.is_ownership_record(path));
                ((group.name, dependents), (group.name, records))
            })
            .unzip();
        let ordered: Vec<_> = dependents.into_iter().chain(records).collect();
        let groups: Vec<_> = ordered
            .iter()
            .map(|(name, entries)| gents::store::local::home::RetireGroup { name, entries })
            .collect();
        let mut retired =
            gents::store::local::home::retire_entries(&groups, retire_as).map_err(|error| {
                BridgeError::new(
                    BridgeErrorCode::Backend,
                    gents_protocol::error_chain::ErrorChain(&error).to_string(),
                )
            })?;
        for key in keychain_keys {
            delete_key(&key.record, &key.key_file).map_err(|error| {
                BridgeError::new(
                    BridgeErrorCode::Backend,
                    format!("Removing retired store key: {error:#}"),
                )
            })?;
            retired.extend(
                gents::store::local::home::retire_entries(
                    &[gents::store::local::home::RetireGroup {
                        name: "key_record",
                        entries: &key.record_files,
                    }],
                    gents::store::local::home::RetireDisposition::Delete,
                )
                .map_err(|error| {
                    BridgeError::new(
                        BridgeErrorCode::Backend,
                        gents_protocol::error_chain::ErrorChain(&error).to_string(),
                    )
                })?,
            );
        }
        if disposition == HomeResetDisposition::Delete {
            if let Some(keys) = self.delete_prunes_keys.as_ref() {
                // Only an empty directory is removed; another home's key
                // file keeps it in place.
                if std::fs::remove_dir(keys).is_ok() {
                    retired.push(keys.clone());
                }
            }
        }
        Ok(ManagedServerResetResult {
            consequence: match (self.full_reset, disposition) {
                (true, HomeResetDisposition::Archive) => FULL_ARCHIVE_CONSEQUENCE,
                (true, HomeResetDisposition::Delete) => FULL_DELETE_CONSEQUENCE,
                (false, HomeResetDisposition::Archive) => ARCHIVE_CONSEQUENCE,
                (false, HomeResetDisposition::Delete) => DELETE_CONSEQUENCE,
            }
            .into(),
            completed: true,
            disposition: Some(disposition),
            backup_path: backup.map(|path| path.to_string_lossy().into_owned()),
            retired_paths: retired
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            ..self.preview()
        })
    }
}

fn reject_symlink(path: &Path, label: &str) -> Result<(), BridgeError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        BridgeError::new(
            BridgeErrorCode::Backend,
            format!("Inspecting {label} {}: {error}", path.display()),
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(BridgeError::new(
            BridgeErrorCode::PathEscapesRoot,
            format!("{label} must not be a symbolic link"),
        ));
    }
    Ok(())
}

fn reject_symlink_if_present(path: &Path, label: &str) -> Result<(), BridgeError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(BridgeError::new(
            BridgeErrorCode::PathEscapesRoot,
            format!("{label} must not be a symbolic link"),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(BridgeError::new(
            BridgeErrorCode::Backend,
            format!("Inspecting {label} {}: {error}", path.display()),
        )),
    }
}

/// Held across browser startup and shutdown, including setup screens without a
/// client store. Lock files are never retired: replacing their inode would let
/// another process open the state while retirement is still in progress.
pub fn lock_desktop_process(
    paths: &crate::client::DesktopPaths,
    wait: bool,
) -> anyhow::Result<DesktopProcessLock> {
    lock_file(paths, ".desktop-process.lock", wait).map(|lock| DesktopProcessLock { _lock: lock })
}

/// [`lock_desktop_process`]'s lock. Tauri keeps one managed value per type,
/// so each lock a desktop holds has its own type.
#[derive(Debug)]
pub struct DesktopProcessLock {
    _lock: gents::store::local::file_lock::FileLock,
}

/// [`lock_browser_store`]'s lock; see [`DesktopProcessLock`].
#[derive(Debug)]
pub struct BrowserStoreLock {
    _lock: gents::store::local::file_lock::FileLock,
}

/// A reset reserves the next process-lock acquisition before asking the app to
/// exit. Startup takes this lock first, so it cannot overtake the waiting reset.
pub fn lock_reset(
    paths: &crate::client::DesktopPaths,
) -> anyhow::Result<gents::store::local::file_lock::FileLock> {
    lock_file(paths, ".reset.lock", false)
}

/// The locks desktop bootstrap takes on the client's state: the client
/// store's lock, taken before the store opens, then the peer directory's
/// writer lease. A desktop without the process lock is excluded by these.
pub fn lock_client_state(
    paths: &crate::client::DesktopPaths,
) -> anyhow::Result<(
    gents::store::local::home::StoreLock,
    gents::store::local::file_lock::FileLock,
)> {
    use anyhow::Context;
    let store = gents::store::local::home::lock_store_at(paths.root(), paths.node_data_dir())
        .context("Quit Gents before resetting its desktop state")?;
    let peers = paths.peer_directory_path().with_extension("lock");
    reject_symlink_if_present(&peers, "peer directory lock")?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&peers)
        .with_context(|| format!("opening {}", peers.display()))?;
    let peers = gents::store::local::file_lock::FileLock::try_exclusive(file)
        .context("Quit Gents before resetting its desktop state")?;
    Ok((store, peers))
}

/// WebKit keeps one website-data store per app identity (or per isolated
/// store identifier), shared by every desktop home. Each desktop holds this
/// lock while it runs and browser clearing takes it, so the store is never
/// cleared under a running desktop, whatever its home. An isolated store's
/// lock lives in the temporary directory its test run shares.
pub fn lock_browser_store(isolated_store: Option<&str>) -> anyhow::Result<BrowserStoreLock> {
    use anyhow::Context;
    let directory = match isolated_store {
        Some(_) => std::env::temp_dir(),
        None => dirs::data_local_dir()
            .context("unable to resolve a local application data directory")?
            .join("gents"),
    };
    lock_browser_store_in(&directory, isolated_store)
}

fn lock_browser_store_in(
    directory: &Path,
    isolated_store: Option<&str>,
) -> anyhow::Result<BrowserStoreLock> {
    let name = match isolated_store {
        Some(identifier) => format!("browser-store-{identifier}.lock"),
        None => "browser-store.lock".into(),
    };
    lock_in(directory, &name, false).map(|lock| BrowserStoreLock { _lock: lock })
}

fn lock_file(
    paths: &crate::client::DesktopPaths,
    name: &str,
    wait: bool,
) -> anyhow::Result<gents::store::local::file_lock::FileLock> {
    lock_in(paths.root(), name, wait)
}

fn lock_in(
    directory: &Path,
    name: &str,
    wait: bool,
) -> anyhow::Result<gents::store::local::file_lock::FileLock> {
    use anyhow::Context;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory)?;
    let path = directory.join(name);
    reject_symlink_if_present(&path, "desktop lock")?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    if wait {
        Ok(gents::store::local::file_lock::FileLock::exclusive(file)?)
    } else {
        gents::store::local::file_lock::FileLock::try_exclusive(file).context("Gents is running or a reset is in progress; quit Gents or wait for the reset to finish")
    }
}

/// The CLI defaults to the home the desktop actually reviewed. An unreadable
/// preference requires an explicit home, so corruption cannot select a
/// different node for a destructive reset.
pub fn reviewed_reset_home(paths: &crate::client::DesktopPaths) -> anyhow::Result<Option<PathBuf>> {
    use anyhow::Context;
    let path = paths.root().join(MANAGED_SERVER_CONFIG);
    let bytes =
        match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context(
                "Reading desktop preference; specify --home to choose the reset target explicitly",
            ),
        };
    let stored: StoredManagedServer = serde_json::from_slice(&bytes).context(
        "Reading desktop preference; specify --home to choose the reset target explicitly",
    )?;
    Ok(stored
        .reviewed_for
        .map(|reviewed| reviewed.node_home)
        .filter(|home| !home.trim().is_empty())
        .map(PathBuf::from))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::DesktopPaths;

    fn write(path: &Path, value: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, value).unwrap();
    }

    fn init_record(home: &Path, tool_root: Option<&Path>) {
        write(
            &home.join("init.json"),
            &serde_json::json!({
                "home": home,
                "node_name": "local",
                "node_did": "did:key:zlocal",
                "key_path": null,
                "tool_ceiling": "workspace",
                "tool_root": tool_root,
            })
            .to_string(),
        );
    }

    fn fixture(user: &Path) -> (PathBuf, DesktopPaths, PathBuf) {
        let home = user.join(".gents");
        init_record(&home, None);
        write(&home.join("keys/local.key"), "local identity");
        write(&home.join("keys/other.key"), "another home's identity");
        write(&home.join("other-node/init.json"), "other home");
        let desktop = DesktopPaths::from_root(user.join("desktop"));
        write(
            desktop.principal_metadata_path(),
            r#"{"did":"did:key:z6MkTest","public_key_bytes":[1,2,3]}"#,
        );
        write(&desktop.root().join(MANAGED_SERVER_CONFIG), "preferences");
        write(&desktop.root().join("logs/client.log"), "diagnostics");
        let service = user.join("Library/LaunchAgents/ai.gents.runtime.plist");
        write(&service, "service definition");
        for dir in ["WebKit", "Caches"] {
            write(
                &user.join(format!("Library/{dir}/com.source-inc.gents/session")),
                "browser state",
            );
        }
        (home, desktop, service)
    }

    #[test]
    fn full_archive_includes_durable_state_and_leaves_browser_clearing_to_webkit() {
        let temp = tempfile::tempdir().unwrap();
        let user = temp.path().canonicalize().unwrap();
        let (home, desktop, service) = fixture(&user);
        let plan = plan_full_reset(Some(&home), &desktop, &user, &service).unwrap();
        let preview = plan.preview();
        assert!(preview.delete_confirmation.is_some());
        assert!(preview.stores.is_empty());
        let backup = plan.backup_path("test").unwrap();
        plan.check_confirmation(&preview.confirmation, HomeResetDisposition::Archive)
            .unwrap();
        let result = plan
            .retire(HomeResetDisposition::Archive, Some(&backup))
            .unwrap();
        assert!(result.completed);
        assert_eq!(
            std::fs::read_to_string(backup.join("service/ai.gents.runtime.plist")).unwrap(),
            "service definition"
        );
        assert!(backup.join("desktop/logs/client.log").is_file());
        assert!(backup.join("home/init.json").is_file());
        assert!(home.join("other-node/init.json").is_file());
        assert!(home.join("keys/other.key").is_file());
        assert!(backup.join("keys/local.key").is_file());
        assert!(!home.join("keys/local.key").exists());
        assert!(!service.exists());
        if cfg!(target_os = "macos") {
            assert!(!backup.join("webkit").exists());
            assert!(!backup.join("cache").exists());
            assert!(user
                .join("Library/WebKit/com.source-inc.gents/session")
                .is_file());
            assert_eq!(preview.browser_paths.len(), 2);
            assert!(result.cleared_browser_paths.is_empty());
        }
    }

    #[test]
    fn full_delete_preserves_other_homes_and_their_keys() {
        let temp = tempfile::tempdir().unwrap();
        let user = temp.path().canonicalize().unwrap();
        let (home, desktop, service) = fixture(&user);
        let plan = plan_full_reset(Some(&home), &desktop, &user, &service).unwrap();
        let preview = plan.preview();
        plan.check_confirmation(
            preview.delete_confirmation.as_deref().unwrap(),
            HomeResetDisposition::Delete,
        )
        .unwrap();
        plan.retire(HomeResetDisposition::Delete, None).unwrap();
        assert!(!home.join("init.json").exists());
        assert!(!home.join("keys/local.key").exists());
        assert!(home.join("keys/other.key").is_file());
        assert!(home.join("other-node/init.json").is_file());
        assert!(!desktop.principal_metadata_path().exists());
        assert!(!service.exists());
    }

    #[test]
    fn full_reset_preserves_keys_referenced_by_another_known_home() {
        for disposition in [HomeResetDisposition::Archive, HomeResetDisposition::Delete] {
            let temp = tempfile::tempdir().unwrap();
            let user = temp.path().canonicalize().unwrap();
            let (home, desktop, service) = fixture(&user);
            write(
                &home.join("other-node/init.json"),
                &serde_json::json!({
                    "key_path": home.join("keys/local.key")
                })
                .to_string(),
            );
            let plan = plan_full_reset(Some(&home), &desktop, &user, &service).unwrap();
            let backup = plan.backup_path("shared-key").unwrap();
            plan.retire(
                disposition,
                (disposition == HomeResetDisposition::Archive).then_some(backup.as_path()),
            )
            .unwrap();
            assert!(home.join("keys/local.key").is_file());
            assert!(home.join("keys/other.key").is_file());
            assert!(home.join("other-node/init.json").is_file());
        }
    }

    #[test]
    fn full_reset_retires_orphaned_service_without_a_runtime_home() {
        let temp = tempfile::tempdir().unwrap();
        let user = temp.path().canonicalize().unwrap();
        let desktop = DesktopPaths::from_root(user.join("desktop"));
        let service = user.join("service.plist");
        write(&service, "service");
        let plan = plan_full_reset(Some(&user.join("missing")), &desktop, &user, &service).unwrap();
        plan.retire(HomeResetDisposition::Delete, None).unwrap();
        assert!(!service.exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn browser_clearing_cannot_overlap_custom_homes_or_process_locks() {
        let temp = tempfile::tempdir().unwrap();
        let user = temp.path().canonicalize().unwrap();
        let browser = user.join("Library/WebKit/com.source-inc.gents");
        write(&browser.join("session"), "browser state");
        let desktop = DesktopPaths::from_root(browser.join("desktop"));
        let _process = lock_desktop_process(&desktop, false).unwrap();
        let service = user.join("service.plist");
        assert!(plan_full_reset(None, &desktop, &user, &service).is_err());
        assert!(desktop.root().join(".desktop-process.lock").exists());
        let desktop = DesktopPaths::from_root(user.join("desktop"));
        let home = browser.join("home");
        write(&home.join("other-node/init.json"), "other node");
        assert!(plan_full_reset(Some(&home), &desktop, &user, &service).is_err());
        assert!(home.join("other-node/init.json").exists());
    }

    #[test]
    fn client_store_cannot_subsume_a_managed_home_or_its_retained_children() {
        let temp = tempfile::tempdir().unwrap();
        let user = temp.path().canonicalize().unwrap();
        let desktop = DesktopPaths::from_root(user.join("desktop"));
        write(
            desktop.principal_metadata_path(),
            r#"{"did":"did:key:z6MkTest","public_key_bytes":[1,2,3]}"#,
        );
        let home = desktop.node_data_dir().join("managed");
        write(&home.join("other-node/init.json"), "retained node");
        assert!(
            plan_full_reset(Some(&home), &desktop, &user, &user.join("service.plist")).is_err()
        );
        write(&home.join("init.json"), "{}");
        assert!(
            plan_full_reset(Some(&home), &desktop, &user, &user.join("service.plist")).is_err()
        );
        assert!(home.join("other-node/init.json").exists());
    }

    #[test]
    fn a_changed_host_inventory_requires_another_confirmation() {
        let temp = tempfile::tempdir().unwrap();
        let user = temp.path().canonicalize().unwrap();
        let (home, desktop, service) = fixture(&user);
        let preview = plan_full_reset(Some(&home), &desktop, &user, &service)
            .unwrap()
            .preview();
        std::fs::remove_file(&service).unwrap();
        let settled = plan_full_reset(Some(&home), &desktop, &user, &service).unwrap();
        assert!(settled
            .check_confirmation(&preview.confirmation, HomeResetDisposition::Archive)
            .is_err());
        assert!(home.join("init.json").exists());
    }

    #[test]
    fn full_reset_retains_lock_inodes_and_excludes_other_processes() {
        let temp = tempfile::tempdir().unwrap();
        let user = temp.path().canonicalize().unwrap();
        let (home, desktop, service) = fixture(&user);
        let process = lock_desktop_process(&desktop, false).unwrap();
        let reservation = lock_reset(&desktop).unwrap();
        assert!(lock_reset(&desktop).is_err());
        assert!(lock_desktop_process(&desktop, false).is_err());
        let plan = plan_full_reset(Some(&home), &desktop, &user, &service).unwrap();
        plan.retire(HomeResetDisposition::Delete, None).unwrap();
        assert!(lock_reset(&desktop).is_err());
        assert!(lock_desktop_process(&desktop, false).is_err());
        drop(process);
        drop(reservation);
        assert!(lock_desktop_process(&desktop, false).is_ok());
        assert!(lock_reset(&desktop).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn full_reset_retains_peer_lock_through_a_desktop_path_alias() {
        let temp = tempfile::tempdir().unwrap();
        let user = temp.path().canonicalize().unwrap();
        let (home, desktop, service) = fixture(&user);
        let lock = desktop.peer_directory_path().with_extension("lock");
        write(&lock, "");
        let alias = user.join("user-alias");
        std::os::unix::fs::symlink(&user, &alias).unwrap();
        let alias =
            DesktopPaths::from_root(alias.join(desktop.root().strip_prefix(&user).unwrap()));
        let plan = plan_full_reset(Some(&home), &alias, &user, &service).unwrap();
        assert!(!plan
            .preview()
            .planned_paths
            .contains(&lock.display().to_string()));
        plan.retire(HomeResetDisposition::Delete, None).unwrap();
        assert!(lock.exists());
    }

    #[test]
    fn helper_cannot_retire_durable_state_before_desktop_exits() {
        let temp = tempfile::tempdir().unwrap();
        let user = temp.path().canonicalize().unwrap();
        let (home, desktop, service) = fixture(&user);
        let process = lock_desktop_process(&desktop, false).unwrap();
        let (ready_send, ready_receive) = std::sync::mpsc::channel();
        let worker_desktop = desktop.clone();
        let browser = user.join("Library/WebKit/com.source-inc.gents/session");
        let worker = std::thread::spawn(move || {
            let _reservation = lock_reset(&worker_desktop).unwrap();
            ready_send.send(()).unwrap();
            let _process = lock_desktop_process(&worker_desktop, true).unwrap();
            let plan = plan_full_reset(Some(&home), &worker_desktop, &user, &service).unwrap();
            plan.retire(HomeResetDisposition::Delete, None).unwrap();
        });
        ready_receive
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        assert!(desktop.principal_metadata_path().exists());
        assert!(browser.exists());
        assert!(
            lock_reset(&desktop).is_err(),
            "a new desktop cannot overtake the helper"
        );
        drop(process);
        worker.join().unwrap();
        assert!(!desktop.principal_metadata_path().exists());
        assert!(
            browser.exists(),
            "filesystem retirement never clears browser data"
        );
    }

    #[test]
    fn cli_defaults_to_the_reviewed_home_and_requires_explicit_target_for_corruption() {
        let temp = tempfile::tempdir().unwrap();
        let paths = DesktopPaths::from_root(temp.path());
        assert_eq!(reviewed_reset_home(&paths).unwrap(), None);
        write(
            &temp.path().join(MANAGED_SERVER_CONFIG),
            r#"{"nodeName":"local","reviewedFor":{"nodeHome":"/a/custom/home","nodeDid":"did:key:example"}}"#,
        );
        assert_eq!(
            reviewed_reset_home(&paths).unwrap(),
            Some(PathBuf::from("/a/custom/home"))
        );
        write(&temp.path().join(MANAGED_SERVER_CONFIG), "invalid json");
        assert!(reviewed_reset_home(&paths).is_err());
    }

    #[test]
    fn full_reset_refuses_a_directory_without_a_gents_init_marker() {
        for marker in [None, Some("{}")] {
            let temp = tempfile::tempdir().unwrap();
            let user = temp.path().canonicalize().unwrap();
            let (_, desktop, service) = fixture(&user);
            let project = user.join("project");
            for entry in ["data/rows.csv", "plugins/mine.txt", "eval/notes.md"] {
                write(&project.join(entry), "user file");
            }
            if let Some(marker) = marker {
                write(&project.join("init.json"), marker);
            }
            let error = plan_full_reset(Some(&project), &desktop, &user, &service).unwrap_err();
            assert!(error.message.contains("init.json"), "{}", error.message);
            for entry in ["data/rows.csv", "plugins/mine.txt", "eval/notes.md"] {
                assert!(project.join(entry).is_file(), "{entry}");
            }
        }
    }

    #[test]
    fn full_reset_keeps_store_and_log_names_in_an_unrelated_desktop_folder() {
        let temp = tempfile::tempdir().unwrap();
        let user = temp.path().canonicalize().unwrap();
        let (home, _, service) = fixture(&user);
        let project = DesktopPaths::from_root(user.join("project"));
        write(&project.node_data_dir().join("index.js"), "user file");
        write(&project.root().join("logs/build.log"), "user file");
        let plan = plan_full_reset(Some(&home), &project, &user, &service).unwrap();
        let preview = plan.preview();
        for path in preview.planned_paths.iter().chain(&preview.delete_paths) {
            assert!(!Path::new(path).starts_with(project.root()), "{path}");
        }
        plan.retire(HomeResetDisposition::Delete, None).unwrap();
        assert!(project.node_data_dir().join("index.js").is_file());
        assert!(project.root().join("logs/build.log").is_file());
        assert!(!home.join("init.json").exists());
    }

    #[test]
    fn desktop_record_names_without_desktop_contents_prove_nothing() {
        let names = [
            "principal.json",
            "store-encryption.json",
            MANAGED_SERVER_CONFIG,
            "peers.json",
            "store.key",
        ];
        for name in names {
            for contents in [
                None,
                Some("hello"),
                Some("{}"),
                Some(r#"{"did":"x"}"#),
                Some(r#"{"reviewedFor":{"nodeDid":"did:"}}"#),
                Some(r#"{"reviewedFor":{"nodeDid":"did:key:z","nodeHome":"/h"}}"#),
                Some(r#"{"version":1}"#),
            ] {
                let temp = tempfile::tempdir().unwrap();
                let user = temp.path().canonicalize().unwrap();
                let (home, _, service) = fixture(&user);
                let project = DesktopPaths::from_root(user.join("project"));
                write(&project.node_data_dir().join("index.js"), "user file");
                write(&project.root().join("logs/build.log"), "user file");
                let record = project.root().join(name);
                match contents {
                    Some(contents) => write(&record, contents),
                    None => write(&record.join("inside"), "user file"),
                }
                let plan = plan_full_reset(Some(&home), &project, &user, &service).unwrap();
                let preview = plan.preview();
                for path in preview.planned_paths.iter().chain(&preview.delete_paths) {
                    assert!(
                        !Path::new(path).starts_with(project.root()),
                        "{name} {contents:?}: {path}"
                    );
                }
                plan.retire(HomeResetDisposition::Delete, None).unwrap();
                assert!(project.node_data_dir().join("index.js").is_file());
                assert!(project.root().join("logs/build.log").is_file());
                assert!(record.exists());
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn interrupted_retirement_keeps_evidence_so_a_retry_finishes_logs() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let user = temp.path().canonicalize().unwrap();
        let (home, desktop, service) = fixture(&user);
        let logs = desktop.root().join("logs");
        std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o500)).unwrap();
        let plan = plan_full_reset(Some(&home), &desktop, &user, &service).unwrap();
        assert!(plan.retire(HomeResetDisposition::Delete, None).is_err());
        std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(desktop.principal_metadata_path().is_file());
        assert!(home.join("init.json").is_file());
        let retry = plan_full_reset(Some(&home), &desktop, &user, &service).unwrap();
        assert!(retry
            .preview()
            .delete_paths
            .contains(&logs.display().to_string()));
        retry.retire(HomeResetDisposition::Delete, None).unwrap();
        assert!(!logs.exists());
        assert!(!desktop.principal_metadata_path().exists());
        assert!(!home.join("init.json").exists());
    }

    fn evidenced_fixture(user: &Path) -> (PathBuf, DesktopPaths, PathBuf) {
        let (home, desktop, service) = fixture(user);
        write(
            desktop.store_encryption_path(),
            r#"{"version":1,"custody":"file"}"#,
        );
        write(desktop.store_key_path(), "client store key");
        write(desktop.peer_directory_path(), "{}");
        (home, desktop, service)
    }

    #[test]
    fn a_retry_finishes_after_an_interrupted_evidence_pass() {
        let first = tempfile::tempdir().unwrap();
        let first = first.path().canonicalize().unwrap();
        let (home, desktop, service) = evidenced_fixture(&first);
        let plan = plan_full_reset(Some(&home), &desktop, &first, &service).unwrap();
        let records = plan.ownership_records.clone();
        assert_eq!(
            records,
            [
                home.join("init.json"),
                desktop.principal_metadata_path().to_path_buf(),
                desktop.store_encryption_path().to_path_buf(),
            ]
        );
        let order = plan
            .retire(HomeResetDisposition::Delete, None)
            .unwrap()
            .retired_paths;
        let boundary = order
            .iter()
            .position(|path| records.contains(&PathBuf::from(path)))
            .unwrap();
        assert!(order[boundary..]
            .iter()
            .all(|path| records.contains(&PathBuf::from(path))));
        for dependent in [
            desktop.store_key_path(),
            desktop.peer_directory_path(),
            &desktop.root().join(MANAGED_SERVER_CONFIG),
            &desktop.root().join("logs"),
        ] {
            assert!(order[..boundary].contains(&dependent.display().to_string()));
        }

        // Interrupt a second, identical reset right after its first record.
        let second = tempfile::tempdir().unwrap();
        let second = second.path().canonicalize().unwrap();
        let (home, desktop, service) = evidenced_fixture(&second);
        for path in &order[..=boundary] {
            let path = second.join(Path::new(path).strip_prefix(&first).unwrap());
            if path.is_dir() {
                std::fs::remove_dir_all(&path).unwrap();
            } else {
                std::fs::remove_file(&path).unwrap();
            }
        }
        let retry = plan_full_reset(Some(&home), &desktop, &second, &service).unwrap();
        retry.retire(HomeResetDisposition::Delete, None).unwrap();
        for path in &order {
            let path = second.join(Path::new(path).strip_prefix(&first).unwrap());
            assert!(!path.exists(), "{} survived the retry", path.display());
        }
    }

    #[test]
    fn a_pending_upgrade_journal_alone_proves_desktop_ownership() {
        use gents::store::local::store_key::{upgrade, StoreEncryption, StoreKeyCustodyChoice};
        let temp = tempfile::tempdir().unwrap();
        let user = temp.path().canonicalize().unwrap();
        let (home, _, service) = fixture(&user);
        let desktop = DesktopPaths::from_root(user.join("upgrading"));
        write(&desktop.node_data_dir().join("MANIFEST"), "REGOMAN client");
        write(desktop.principal_metadata_path(), "{}");
        write(&desktop.root().join("logs/client.log"), "diagnostics");
        let data = desktop.node_data_dir();
        let record = StoreEncryption::prepare(
            StoreKeyCustodyChoice::File,
            desktop.store_key_path(),
            &upgrade::staging_path(data),
        )
        .unwrap();
        upgrade::begin(data, &record).unwrap();
        let journal = upgrade::journal_path(data);
        let plan = plan_full_reset(Some(&home), &desktop, &user, &service).unwrap();
        assert!(plan.ownership_records.contains(&journal));
        assert!(!plan
            .ownership_records
            .contains(&desktop.principal_metadata_path().to_path_buf()));
        plan.retire(HomeResetDisposition::Delete, None).unwrap();
        assert!(!data.exists());
        assert!(!journal.exists());
        assert!(!desktop.root().join("logs").exists());
    }

    #[test]
    fn a_home_sharing_its_tool_root_keeps_the_users_files() {
        for shared in ["", "plugins/work"] {
            let temp = tempfile::tempdir().unwrap();
            let user = temp.path().canonicalize().unwrap();
            let (home, desktop, service) = fixture(&user);
            write(&home.join("plugins/work/mine.txt"), "user file");
            write(&home.join("eval/notes.md"), "user file");
            let tool_root = if shared.is_empty() {
                home.clone()
            } else {
                home.join(shared)
            };
            init_record(&home, Some(&tool_root));
            assert!(plan_full_reset(Some(&home), &desktop, &user, &service).is_err());
            assert!(plan_home_reset(
                Some(&home),
                Some(gents::store::local::storage_backend::IncompatibleStore {
                    kind: gents::store::local::storage_backend::IncompatibleStoreKind::LegacyLark,
                    data_path: home.join("data"),
                }),
                None,
                &desktop,
                &user,
                ClientBinding::Unbound,
            )
            .is_err());
            assert!(home.join("plugins/work/mine.txt").is_file());
            assert!(home.join("eval/notes.md").is_file());
        }
    }

    #[test]
    fn a_retired_home_can_be_reviewed_again_to_finish_browser_clearing() {
        let temp = tempfile::tempdir().unwrap();
        let user = temp.path().canonicalize().unwrap();
        let (home, desktop, service) = fixture(&user);
        let plan = plan_full_reset(Some(&home), &desktop, &user, &service).unwrap();
        let backup = plan.backup_path("first").unwrap();
        plan.retire(HomeResetDisposition::Archive, Some(&backup))
            .unwrap();
        let again = plan_full_reset(Some(&home), &desktop, &user, &service).unwrap();
        assert!(again
            .preview()
            .planned_paths
            .iter()
            .all(|path| !Path::new(path).starts_with(&home)));
    }

    #[test]
    fn client_state_lock_excludes_a_bootstrapping_desktop() {
        let temp = tempfile::tempdir().unwrap();
        let desktop = DesktopPaths::from_root(temp.path().join("desktop"));
        std::fs::create_dir_all(desktop.node_data_dir()).unwrap();
        // Bootstrap takes the store lock before the peer directory lease.
        let store =
            gents::store::local::home::lock_store(desktop.root(), desktop.node_data_dir()).unwrap();
        assert!(lock_client_state(&desktop).is_err());
        drop(store);
        let held = lock_client_state(&desktop).unwrap();
        assert!(
            gents::store::local::home::lock_store(desktop.root(), desktop.node_data_dir()).is_err()
        );
        drop(held);

        let fresh = DesktopPaths::from_root(temp.path().join("fresh"));
        std::fs::create_dir_all(fresh.root()).unwrap();
        let held = lock_client_state(&fresh).unwrap();
        assert!(!fresh.node_data_dir().exists());
        std::fs::create_dir_all(fresh.node_data_dir()).unwrap();
        assert!(
            gents::store::local::home::lock_store(fresh.root(), fresh.node_data_dir()).is_err()
        );
        drop(held);
    }

    #[test]
    fn browser_store_exclusion_spans_desktop_homes() {
        let temp = tempfile::tempdir().unwrap();
        let shared = temp.path().join("app-data");
        let running = DesktopPaths::from_root(temp.path().join("running"));
        let resetting = DesktopPaths::from_root(temp.path().join("resetting"));
        let _process = lock_desktop_process(&running, false).unwrap();
        let _browser = lock_browser_store_in(&shared, None).unwrap();
        let _reservation = lock_reset(&resetting).unwrap();
        let _resetting = lock_desktop_process(&resetting, false).unwrap();
        assert!(lock_browser_store_in(&shared, None).is_err());
        assert!(lock_browser_store_in(&shared, Some("00112233445566778899aabbccddeeff")).is_ok());
    }

    #[test]
    fn lock_creation_does_not_change_the_reviewed_inventory() {
        let temp = tempfile::tempdir().unwrap();
        let user = temp.path().canonicalize().unwrap();
        let (home, desktop, service) = fixture(&user);
        let before = plan_full_reset(Some(&home), &desktop, &user, &service)
            .unwrap()
            .preview();
        let _process = lock_desktop_process(&desktop, false).unwrap();
        let _reservation = lock_reset(&desktop).unwrap();
        write(&desktop.peer_directory_path().with_extension("lock"), "");
        let after = plan_full_reset(Some(&home), &desktop, &user, &service).unwrap();
        after
            .check_confirmation(&before.confirmation, HomeResetDisposition::Archive)
            .unwrap();
    }
}
