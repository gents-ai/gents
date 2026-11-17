use anyhow::{Context, Result};
use gents_store::client_journal::paths::DesktopPaths;

pub(super) async fn open_or_create_client_store_key(
    paths: &DesktopPaths,
    options: &super::JournalOptions,
) -> Result<gents_store::local::store_key::StoreKey> {
    use gents_store::local::store_key::{upgrade, StoreEncryption};
    let choice = options.store_key_custody;
    let record = read_client_store_record(paths).await?;
    let data = paths.node_data_dir().to_owned();
    let pending = tokio::task::spawn_blocking(move || upgrade::pending_record(&data)).await??;
    if let (Some(record), Some(pending)) = (&record, &pending) {
        anyhow::ensure!(
            record == pending,
            "client encryption metadata does not match its pending upgrade"
        );
    }
    if pending.is_some() || (record.is_none() && paths.node_data_dir().join("MANIFEST").exists()) {
        if pending.is_none() {
            gents_store::local::storage_backend::refuse_other_version_schema(
                paths.node_data_dir(),
                None,
            )
            .await?;
        }
        return Err(gents_protocol::operations::install_repair::IncompatibleStore {
            kind: gents_protocol::operations::install_repair::IncompatibleStoreKind::UnencryptedStore,
            data_path: paths.node_data_dir().to_path_buf(),
        }.into());
    }
    let record = match record {
        Some(record) => record,
        None => {
            let owned = paths.clone();
            let record = tokio::task::spawn_blocking(move || {
                let record = StoreEncryption::prepare(
                    choice,
                    owned.store_key_path(),
                    owned.node_data_dir(),
                )?;
                write_client_store_record(&owned, &record)?;
                Ok::<_, anyhow::Error>(record)
            })
            .await??;
            return match &options.store_key_provider {
                Some(provider) => {
                    provider
                        .create(&record, paths.store_key_path(), paths.node_data_dir())
                        .await
                }
                None => {
                    let owned = paths.clone();
                    Ok(tokio::task::spawn_blocking(move || {
                        record.initialize(owned.store_key_path(), owned.node_data_dir())
                    })
                    .await??)
                }
            };
        }
    };
    client_store_key(
        options,
        &record,
        paths.store_key_path(),
        paths.node_data_dir(),
    )
    .await
}

/// Whether the client store has data but its key is gone. Never creates a key.
pub async fn client_store_key_missing(paths: &DesktopPaths) -> Result<bool> {
    use gents_protocol::operations::install_repair::IncompatibleStoreKind;
    use gents_store::local::store_key::StoreKeyError;
    let Some(record) = read_client_store_record(paths).await? else {
        return Ok(false);
    };
    if !tokio::fs::try_exists(paths.node_data_dir().join("MANIFEST")).await? {
        return Ok(false);
    }
    let key_file = paths.store_key_path().to_path_buf();
    let data_path = paths.node_data_dir().to_path_buf();
    match tokio::task::spawn_blocking(move || record.load(&key_file, &data_path)).await? {
        Ok(_) => Ok(false),
        Err(StoreKeyError::Incompatible(store))
            if store.kind == IncompatibleStoreKind::MissingStoreKey =>
        {
            Ok(true)
        }
        Err(error) => Err(error).context("reading the client store key"),
    }
}

/// Whether the client store was written by another Gents version whose schema
/// this build cannot take. A held store, a missing or unreadable key and an
/// I/O failure are never reported as one: each keeps its own outcome.
pub async fn client_store_from_other_version(paths: &DesktopPaths) -> Result<bool> {
    use gents_protocol::operations::install_repair::IncompatibleStoreKind;
    use gents_store::local::storage_backend::{
        incompatible_store_kind, refuse_other_version_schema,
    };
    use gents_store::local::store_key::StoreKeyError;
    let data_path = paths.node_data_dir().to_path_buf();
    if !tokio::fs::try_exists(data_path.join("MANIFEST")).await?
        || incompatible_store_kind(&data_path)?.is_some()
    {
        return Ok(false);
    }
    let _store_lock = match gents_store::local::home::lock_store(paths.root(), &data_path) {
        Ok(lock) => lock,
        Err(error) if error.lock_held().is_some() => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let key = match read_client_store_record(paths).await? {
        None if gents_store::local::store_key::upgrade::pending_record(&data_path)?.is_some() => {
            return Ok(true)
        }
        None => return Ok(true),
        Some(record) => {
            let key_file = paths.store_key_path().to_path_buf();
            let data = data_path.clone();
            match tokio::task::spawn_blocking(move || record.load(&key_file, &data)).await? {
                Ok(key) => Some(key),
                Err(StoreKeyError::Incompatible(store))
                    if store.kind == IncompatibleStoreKind::MissingStoreKey =>
                {
                    return Ok(false)
                }
                Err(error) => return Err(error).context("reading the client store key"),
            }
        }
    };
    match refuse_other_version_schema(&data_path, key.as_ref()).await {
        Ok(()) => Ok(false),
        Err(error)
            if error
                .incompatible()
                .is_some_and(|store| store.kind == IncompatibleStoreKind::OtherVersionSchema) =>
        {
            Ok(true)
        }
        Err(error) => Err(error.into()),
    }
}

async fn read_client_store_record(
    paths: &DesktopPaths,
) -> Result<Option<gents_store::local::store_key::StoreEncryption>> {
    let record_path = paths.store_encryption_path();
    match tokio::fs::read(record_path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .with_context(|| format!("decoding {}", record_path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("reading {}", record_path.display())),
    }
}

async fn client_store_key(
    options: &super::JournalOptions,
    record: &gents_store::local::store_key::StoreEncryption,
    key_file: &std::path::Path,
    data_path: &std::path::Path,
) -> Result<gents_store::local::store_key::StoreKey> {
    if let Some(provider) = &options.store_key_provider {
        provider.load(record, key_file, data_path).await
    } else {
        let record = record.clone();
        let key_file = key_file.to_owned();
        let data_path = data_path.to_owned();
        Ok(tokio::task::spawn_blocking(move || record.initialize(&key_file, &data_path)).await??)
    }
}

pub(crate) fn write_client_store_record(
    paths: &DesktopPaths,
    record: &gents_store::local::store_key::StoreEncryption,
) -> Result<()> {
    let record_path = paths.store_encryption_path();
    let staged = tempfile::NamedTempFile::new_in(paths.root())
        .with_context(|| format!("staging {}", record_path.display()))?;
    serde_json::to_writer(staged.as_file(), record)?;
    staged.as_file().sync_all()?;
    staged
        .persist(record_path)
        .with_context(|| format!("writing {}", record_path.display()))?;
    #[cfg(unix)]
    std::fs::File::open(paths.root())?.sync_all()?;
    Ok(())
}
