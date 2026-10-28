use super::*;
use crate::store_key::StoreKeyCustodyChoice;

type Result<T, E = Box<dyn std::error::Error + Send + Sync>> = std::result::Result<T, E>;

async fn plaintext(data: &Path) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let store = RegolithStore::open(data)?;
    let mut txn = store.new_txn(false).await?;
    let mut entries = Vec::new();
    for n in 0_u32..1500 {
        let mut key = vec![b"dbhsp ea"[(n % 8) as usize], 0, 255];
        key.extend_from_slice(&n.to_be_bytes());
        let value = if n % 13 == 0 {
            Vec::new()
        } else {
            format!("original-secret-{n}").into_bytes()
        };
        txn.set(&key, &value).await?;
        entries.push((key, value));
    }
    txn.commit().await?;
    store.close().await?;
    drop(store);
    for relative in [
        "task-hooks/request.json",
        "background-processes/tool.json",
        "unrelated/nested/note",
    ] {
        let path = data.join(relative);
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(path, relative.as_bytes())?;
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink("unrelated/nested/note", data.join("note-link"))?;
    Ok(entries)
}

fn prepare(data: &Path, key_file: &Path) -> Result<(StoreEncryption, StoreKey)> {
    let record =
        StoreEncryption::prepare(StoreKeyCustodyChoice::File, key_file, &staging_path(data))?;
    begin(data, &record)?;
    let key = record.initialize(key_file, &key_store_path(data)?)?;
    Ok((record, key))
}

async fn assert_contents(
    data: &Path,
    key: &StoreKey,
    entries: &[(Vec<u8>, Vec<u8>)],
) -> Result<()> {
    assert!(RegolithStore::open(data).is_err());
    assert!(RegolithStore::open_with_options(
        data,
        crate::storage_backend::regolith_options().with_encryption_key([0x55; 32])
    )
    .is_err());
    let encrypted = RegolithStore::open_with_options(
        data,
        crate::storage_backend::regolith_options().with_encryption_key(*key.0),
    )?;
    let decrypted = encrypted.new_txn(true).await?;
    for (k, v) in entries {
        assert_eq!(decrypted.get(k).await?.as_deref(), Some(v.as_slice()));
    }
    decrypted.discard();
    encrypted.close().await?;
    drop(encrypted);
    assert_engine_files_hide_entries(data, entries)?;
    for relative in [
        "task-hooks/request.json",
        "background-processes/tool.json",
        "unrelated/nested/note",
    ] {
        assert_eq!(fs::read(data.join(relative))?, relative.as_bytes());
    }
    #[cfg(unix)]
    assert_eq!(
        fs::read_link(data.join("note-link"))?,
        PathBuf::from("unrelated/nested/note")
    );
    Ok(())
}

fn assert_engine_files_hide_entries(data: &Path, entries: &[(Vec<u8>, Vec<u8>)]) -> Result<()> {
    if !data.join("MANIFEST").is_file() {
        return Err(format!(
            "encrypted engine MANIFEST is missing from {}",
            data.display()
        )
        .into());
    }

    fn scan(path: &Path, secrets: &[&[u8]]) -> Result<()> {
        if path.is_dir() {
            for entry in fs::read_dir(path)? {
                scan(&entry?.path(), secrets)?;
            }
        } else {
            let bytes = fs::read(path)?;
            for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
                if bytes.windows(secret.len()).any(|window| window == *secret) {
                    return Err(format!(
                        "plaintext store entry found in engine file {}",
                        path.display()
                    )
                    .into());
                }
            }
        }
        Ok(())
    }

    let secrets = entries
        .iter()
        .flat_map(|(key, value)| [key.as_slice(), value.as_slice()])
        .collect::<Vec<_>>();
    for entry in fs::read_dir(data)? {
        let entry = entry?;
        if engine_entry(&entry.file_name()) {
            scan(&entry.path(), &secrets)?;
        }
    }
    Ok(())
}

#[test]
fn every_recovery_observation_matches_lean() {
    for case in &crate::lean_vocab_test::lean_contract_snapshot().store_encryption_upgrade_cases {
        let phase: Phase = serde_json::from_value(case["phase"].clone()).unwrap();
        let action = next(
            phase,
            case["source"].as_bool().unwrap(),
            case["stage"].as_bool().unwrap(),
            case["retired"].as_bool().unwrap(),
        );
        assert_eq!(
            serde_json::to_value(action).unwrap(),
            case["action"],
            "{case}"
        );
        assert_eq!(
            serde_json::to_value(after_recheck(case["equal"].as_bool().unwrap())).unwrap(),
            case["rechecked_phase"]
        );
        assert_eq!(
            may_finish(
                phase,
                case["metadata"].as_bool().unwrap(),
                case["accepted"].as_bool().unwrap()
            ),
            case["may_finish"].as_bool().unwrap()
        );
    }
}

#[tokio::test]
async fn upgrades_every_namespace_and_sidecar_and_survives_reopen() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let entries = plaintext(&data).await?;
    let key_file = temp.path().join("store.aes256");
    let (record, key) = prepare(&data, &key_file)?;
    let mut progress = Vec::new();
    encrypt_existing_with_progress(&data, &record, &key, &mut |stage| progress.push(stage)).await?;
    for phase in ["store_encryption_copy", "store_encryption_verify"] {
        assert!(
            progress.iter().filter(|stage| **stage == phase).count() > 1,
            "a multi-batch upgrade must report ongoing {phase} progress"
        );
    }
    assert_eq!(progress.first(), Some(&"store_encryption_copy"));
    assert_eq!(progress.last(), Some(&"store_encryption_verify"));
    assert_eq!(pending_record(&data)?, Some(record.clone()));
    assert!(retired_path(&data).join("MANIFEST").exists());
    assert_contents(&data, &key, &entries).await?;
    progress.clear();
    encrypt_existing_with_progress(&data, &record, &key, &mut |stage| progress.push(stage)).await?;
    assert!(
        progress.is_empty(),
        "opening an installed upgrade performs no copy/verification work"
    );
    assert_contents(&data, &key, &entries).await?;
    finish(&data)?;
    assert!(pending_record(&data)?.is_none());
    assert!(!retired_path(&data).exists());
    let reloaded = record.load(&key_file, &data)?;
    assert_contents(&data, &reloaded, &entries).await
}

#[tokio::test]
async fn resumes_each_verified_publication_boundary_without_losing_the_source() -> Result<()> {
    for renames in 0..=2 {
        let temp = tempfile::tempdir()?;
        let data = temp.path().join("data");
        let mut entries = plaintext(&data).await?;
        let (record, key) = prepare(&data, &temp.path().join("key"))?;
        copy_and_verify(&data, &staging_path(&data), &key, &mut |_| {}).await?;
        save(
            &data,
            &Journal {
                version: 1,
                record: record.clone(),
                phase: Phase::Verified,
            },
            false,
        )?;
        if renames == 0 {
            let reopened = RegolithStore::open(&data)?;
            let mut write = reopened.new_txn(false).await?;
            write
                .set(b"late-commit", b"from an older binary after crash")
                .await?;
            write.commit().await?;
            reopened.close().await?;
            drop(reopened);
            entries.push((
                b"late-commit".to_vec(),
                b"from an older binary after crash".to_vec(),
            ));
            fs::write(data.join("late-sidecar"), "survives")?;
        }
        if renames > 0 {
            fs::rename(&data, retired_path(&data))?;
            sync_parent(&data)?;
        }
        if renames > 1 {
            fs::rename(staging_path(&data), &data)?;
            sync_parent(&data)?;
        }
        if renames == 1 {
            fs::create_dir(&data)?;
            fs::write(data.join("competing-store"), "keep")?;
            assert!(encrypt_existing(&data, &record, &key).await.is_err());
            assert_eq!(fs::read(data.join("competing-store"))?, b"keep");
            fs::remove_file(data.join("competing-store"))?;
        }
        assert_eq!(
            key_store_path(&data)?,
            if renames == 2 {
                data.clone()
            } else {
                staging_path(&data)
            }
        );
        encrypt_existing(&data, &record, &key).await?;
        assert_contents(&data, &key, &entries).await?;
        if renames == 0 {
            assert_eq!(fs::read(data.join("late-sidecar"))?, b"survives");
        }
        finish(&data)?;
    }
    Ok(())
}

#[tokio::test]
async fn partial_copy_restarts_and_a_lost_materialized_key_is_never_regenerated() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let mut entries = plaintext(&data).await?;
    let key_file = temp.path().join("key");
    let (record, key) = prepare(&data, &key_file)?;
    let stage = RegolithStore::open_with_options(
        staging_path(&data),
        crate::storage_backend::regolith_options().with_encryption_key(*key.0),
    )?;
    let mut write = stage.new_txn(false).await?;
    write.set(b"stale-entry", b"must not survive retry").await?;
    write.commit().await?;
    stage.close().await?;
    drop(stage);
    let key_bytes = fs::read(&key_file)?;
    fs::remove_file(&key_file)?;
    assert!(record
        .initialize(&key_file, &key_store_path(&data)?)
        .is_err());
    assert!(!key_file.exists());
    fs::write(&key_file, key_bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&key_file, fs::Permissions::from_mode(0o600))?;
    }
    let source = RegolithStore::open(&data)?;
    let mut write = source.new_txn(false).await?;
    let removed = entries.pop().unwrap();
    write.delete(&removed.0).await?;
    write.commit().await?;
    source.close().await?;
    drop(source);
    encrypt_existing(&data, &record, &key).await?;
    assert_contents(&data, &key, &entries).await?;
    let store = RegolithStore::open_with_options(
        &data,
        crate::storage_backend::regolith_options().with_encryption_key(*key.0),
    )?;
    let read = store.new_txn(true).await?;
    assert!(read.get(b"stale-entry").await?.is_none());
    assert!(read.get(&removed.0).await?.is_none());
    read.discard();
    store.close().await?;
    Ok(())
}

#[tokio::test]
async fn unowned_artifacts_preserve_original_data() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    plaintext(&data).await?;
    let record = StoreEncryption::prepare(
        StoreKeyCustodyChoice::File,
        &temp.path().join("key"),
        &staging_path(&data),
    )?;
    fs::create_dir(staging_path(&data))?;
    fs::write(staging_path(&data).join("unowned"), "keep")?;
    assert!(begin(&data, &record).is_err());
    assert_eq!(fs::read(staging_path(&data).join("unowned"))?, b"keep");
    assert!(pending_record(&data)?.is_none());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_sidecar_that_cannot_be_copied_keeps_the_original_and_can_retry() -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let entries = plaintext(&data).await?;
    let (record, key) = prepare(&data, &temp.path().join("key"))?;
    let fifo = data.join("external-command-pipe");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes())?;
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(encrypt_existing(&data, &record, &key).await.is_err());
    assert!(data.join("MANIFEST").exists());
    assert!(!retired_path(&data).exists());
    assert!(finish(&data).is_err());
    let original = RegolithStore::open(&data)?;
    let read = original.new_txn(true).await?;
    assert_eq!(
        read.get(&entries[1].0).await?.as_deref(),
        Some(entries[1].1.as_slice())
    );
    read.discard();
    original.close().await?;
    drop(original);
    fs::remove_file(fifo)?;
    encrypt_existing(&data, &record, &key).await?;
    assert_contents(&data, &key, &entries).await
}
