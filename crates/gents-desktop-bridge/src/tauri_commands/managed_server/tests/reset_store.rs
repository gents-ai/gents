use super::*;

fn record_reset_store_key(
    home: &Path,
    desktop: &gents_desktop_core::client::DesktopPaths,
    record: &gents::store_key::StoreEncryption,
) {
    let mut init: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join("init.json")).unwrap()).unwrap();
    init["store_encryption"] = serde_json::to_value(record).unwrap();
    write(&home.join("init.json"), &init.to_string());
    write(
        desktop.store_encryption_path(),
        &serde_json::to_string(record).unwrap(),
    );
}

#[test]
fn reset_file_store_keys_allow_fresh_setup_and_archives_remain_readable() {
    for disposition in [HomeResetDisposition::Delete, HomeResetDisposition::Archive] {
        let temp = tempfile::tempdir().unwrap();
        let temp = std::fs::canonicalize(temp.path()).unwrap();
        let (home, desktop) = dirty_home(&temp);
        let key_file = gents::store_key::home_key_file(&home);
        std::fs::remove_file(home.join("data/MANIFEST")).unwrap();
        std::fs::remove_file(desktop.node_data_dir().join("MANIFEST")).unwrap();
        let record = gents::store_key::StoreEncryption::prepare(
            gents::store_key::StoreKeyCustodyChoice::File,
            &key_file,
            &home.join("data"),
        )
        .unwrap();
        record_reset_store_key(&home, &desktop, &record);
        record.initialize(&key_file, &home.join("data")).unwrap();
        record
            .initialize(desktop.store_key_path(), desktop.node_data_dir())
            .unwrap();
        write(&home.join("data/MANIFEST"), "REGOMAN old lineage");
        write(
            &desktop.node_data_dir().join("MANIFEST"),
            "REGOMAN old client",
        );
        write(&home.join("keys/other.key"), "other identity");
        let plan = plan_home_reset(
            Some(&home),
            Some(lineage_refusal(home.join("data"))),
            None,
            &desktop,
            not_the_user_home(),
            ClientBinding::Unbound,
        )
        .unwrap();
        assert!(plan
            .preview()
            .delete_paths
            .contains(&key_file.to_string_lossy().into_owned()));
        let backup = plan.backup_path("store-keys").unwrap();
        plan.retire_with_key_cleanup(
            disposition,
            (disposition == HomeResetDisposition::Archive).then_some(backup.as_path()),
            |_, _| panic!("file custody never calls Keychain cleanup"),
        )
        .unwrap();
        if disposition == HomeResetDisposition::Archive {
            record
                .load(
                    &backup.join("home/keys/store.aes256"),
                    &backup.join("home/data"),
                )
                .unwrap();
            record
                .load(
                    &backup.join("desktop/store.key"),
                    &backup.join("desktop/node"),
                )
                .unwrap();
        } else {
            assert_eq!(
                std::fs::read_to_string(home.join("keys/other.key")).unwrap(),
                "other identity"
            );
        }
        let fresh = gents::store_key::StoreEncryption::prepare(
            gents::store_key::StoreKeyCustodyChoice::File,
            &key_file,
            &home.join("data"),
        )
        .expect("fresh setup after retirement");
        write(&home.join("init.json"), r#"{"node_name":"fresh"}"#);
        record_reset_store_key(&home, &desktop, &fresh);
        for (key_file, data) in [
            (key_file, home.join("data")),
            (
                desktop.store_key_path().to_path_buf(),
                desktop.node_data_dir().to_path_buf(),
            ),
        ] {
            fresh
                .initialize(&key_file, &data)
                .expect("fresh setup after retirement");
        }
    }
}

#[test]
fn reset_keychain_custody_keeps_archives_and_retryable_deletion_records() {
    for disposition in [HomeResetDisposition::Delete, HomeResetDisposition::Archive] {
        let temp = tempfile::tempdir().unwrap();
        let temp = std::fs::canonicalize(temp.path()).unwrap();
        let (home, desktop) = dirty_home(&temp);
        let record = gents::store_key::StoreEncryption {
            version: gents::store_key::STORE_ENCRYPTION_VERSION,
            custody: gents::store_key::StoreKeyCustody::MacosKeychain {
                keychain_label: "test-only-label".into(),
            },
        };
        record_reset_store_key(&home, &desktop, &record);
        let plan = plan_home_reset(
            Some(&home),
            Some(lineage_refusal(home.join("data"))),
            None,
            &desktop,
            not_the_user_home(),
            ClientBinding::Unbound,
        )
        .unwrap();
        let backup = plan.backup_path("keychain").unwrap();
        if disposition == HomeResetDisposition::Archive {
            plan.retire_with_key_cleanup(disposition, Some(&backup), |_, _| {
                panic!("archive must keep Keychain keys")
            })
            .unwrap();
            assert!(backup.join("home/init.json").exists());
            assert!(backup.join("desktop/store-encryption.json").exists());
            continue;
        }
        assert!(plan
            .retire_with_key_cleanup(disposition, None, |_, _| anyhow::bail!("Keychain locked"))
            .is_err());
        assert!(!home.join("data").exists());
        assert!(!desktop.node_data_dir().exists());
        assert!(home.join("init.json").exists());
        assert!(desktop.store_encryption_path().exists());
        let retry = plan_home_reset(
            Some(&home),
            Some(lineage_refusal(home.join("data"))),
            Some(lineage_refusal(desktop.node_data_dir().to_path_buf())),
            &desktop,
            not_the_user_home(),
            ClientBinding::Unbound,
        )
        .unwrap();
        let mut deleted = Vec::new();
        retry
            .retire_with_key_cleanup(disposition, None, |record, path| {
                deleted.push((record.clone(), path.to_path_buf()));
                Ok(())
            })
            .unwrap();
        assert_eq!(deleted.len(), 2);
        assert!(deleted.iter().all(|(deleted, _)| deleted == &record));
        assert!(!home.join("init.json").exists());
        assert!(!desktop.store_encryption_path().exists());
    }
}

#[test]
fn reset_pending_upgrade_retires_its_artifacts_and_recorded_custody_together() {
    use gents::store_key::{upgrade, StoreEncryption, StoreKeyCustodyChoice};
    for choice in [StoreKeyCustodyChoice::File, StoreKeyCustodyChoice::Keychain] {
        for disposition in [HomeResetDisposition::Delete, HomeResetDisposition::Archive] {
            let temp = tempfile::tempdir().unwrap();
            let temp = std::fs::canonicalize(temp.path()).unwrap();
            let (home, desktop) = dirty_home(&temp);
            write(&home.join("keys/other.key"), "keep");
            let stores = [
                (home.join("data"), gents::store_key::home_key_file(&home)),
                (
                    desktop.node_data_dir().to_path_buf(),
                    desktop.store_key_path().to_path_buf(),
                ),
            ];
            for (data, key_file) in &stores {
                let record =
                    StoreEncryption::prepare(choice, key_file, &upgrade::staging_path(data))
                        .unwrap();
                upgrade::begin(data, &record).unwrap();
                if choice == StoreKeyCustodyChoice::File {
                    record
                        .initialize(key_file, &upgrade::key_store_path(data).unwrap())
                        .unwrap();
                }
                write(&upgrade::staging_path(data).join("partial-copy"), "staged");
            }
            let plan = plan_home_reset(
                Some(&home),
                Some(lineage_refusal(home.join("data"))),
                None,
                &desktop,
                not_the_user_home(),
                ClientBinding::Unbound,
            )
            .unwrap();
            let preview = plan.preview();
            for (data, _) in &stores {
                for entry in upgrade::retirement_entries(data).unwrap() {
                    let path = entry.to_string_lossy().into_owned();
                    assert!(preview.planned_paths.contains(&path));
                    assert!(preview.delete_paths.contains(&path));
                    assert!(!preview.retained_paths.contains(&path));
                }
            }
            let backup = plan.backup_path("pending-upgrade").unwrap();
            let mut deleted_keys = 0;
            plan.retire_with_key_cleanup(
                disposition,
                (disposition == HomeResetDisposition::Archive).then_some(backup.as_path()),
                |_, _| {
                    deleted_keys += 1;
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(
                deleted_keys,
                if choice == StoreKeyCustodyChoice::Keychain
                    && disposition == HomeResetDisposition::Delete
                {
                    2
                } else {
                    0
                }
            );
            for (data, _) in &stores {
                assert!(upgrade::pending_record(data).unwrap().is_none());
                assert!(!upgrade::staging_path(data).exists());
            }
            if disposition == HomeResetDisposition::Archive {
                for (group, name) in [("home", "data"), ("desktop", "node")] {
                    let archived = backup.join(group).join(name);
                    let record = upgrade::pending_record(&archived).unwrap().unwrap();
                    assert!(upgrade::staging_path(&archived)
                        .join("partial-copy")
                        .exists());
                    if choice == StoreKeyCustodyChoice::File {
                        let key = if group == "home" {
                            backup.join("home/keys/store.aes256")
                        } else {
                            backup.join("desktop/store.key")
                        };
                        record.load(&key, &archived).unwrap();
                    }
                }
            } else {
                assert_eq!(
                    std::fs::read_to_string(home.join("keys/other.key")).unwrap(),
                    "keep"
                );
            }
        }
    }
}

#[test]
fn a_foreign_store_is_never_deleted_by_the_bridge() {
    let temp = tempfile::tempdir().unwrap();
    let temp = std::fs::canonicalize(temp.path()).unwrap();
    let (home, desktop) = dirty_home(&temp);
    let plan = plan_home_reset(
        Some(&home),
        Some(gents::storage_backend::IncompatibleStore {
            kind: gents::storage_backend::IncompatibleStoreKind::ForeignVersion,
            data_path: home.join("data"),
        }),
        None,
        &desktop,
        not_the_user_home(),
        ClientBinding::Unbound,
    )
    .unwrap();
    let preview = plan.preview();
    assert_eq!(preview.delete_confirmation, None);
    assert!(preview.delete_paths.is_empty());
    let forged = format!(
        "DELETE {} PERMANENTLY [{}]",
        home.display(),
        plan.digest(HomeResetDisposition::Delete)
    );
    assert_eq!(
        plan.check_confirmation(&forged, HomeResetDisposition::Delete)
            .unwrap_err()
            .code,
        BridgeErrorCode::InvalidArgument
    );
    plan.check_confirmation(&preview.confirmation, HomeResetDisposition::Archive)
        .unwrap();
}

#[test]
fn an_insecure_old_key_scopes_the_home_like_a_refused_store() {
    let temp = tempfile::tempdir().unwrap();
    let temp = std::fs::canonicalize(temp.path()).unwrap();
    let (home, desktop) = dirty_home(&temp);
    let refused = refused_runtime_store(
        &home,
        Some(gents::storage_backend::IncompatibleStoreKind::InsecureKey),
    );
    assert_eq!(refused.data_path, home.join("keys"));
    let preview = plan_home_reset(
        Some(&home),
        Some(refused),
        None,
        &desktop,
        not_the_user_home(),
        ClientBinding::Unbound,
    )
    .unwrap()
    .preview();
    assert!(preview.stores[0].unsafe_key);
    assert!(preview.stores[0].older);
    assert!(preview.delete_confirmation.is_some());
    assert!(preview.stores[0].detail.contains("unsafe file permissions"));
}

#[test]
fn an_unknown_or_unresolvable_user_home_refuses_the_reset() {
    let temp = tempfile::tempdir().unwrap();
    for missing in [None, Some(PathBuf::new())] {
        let error = resolve_user_home(missing).unwrap_err();
        assert_eq!(error.code, BridgeErrorCode::InvalidArgument);
        assert!(
            error.message.contains("cannot be determined"),
            "{}",
            error.message
        );
    }
    let error = resolve_user_home(Some(temp.path().join("absent"))).unwrap_err();
    assert!(
        error.message.contains("cannot be resolved"),
        "{}",
        error.message
    );
    assert_eq!(
        resolve_user_home(Some(temp.path().to_path_buf())).unwrap(),
        std::fs::canonicalize(temp.path()).unwrap()
    );
}

#[test]
fn the_store_lock_neither_moves_nor_changes_the_confirmation() {
    let temp = tempfile::tempdir().unwrap();
    let temp = std::fs::canonicalize(temp.path()).unwrap();
    let (home, desktop) = dirty_home(&temp);
    let refused = || Some(lineage_refusal(home.join("data")));
    let preview = plan_home_reset(
        Some(&home),
        refused(),
        None,
        &desktop,
        not_the_user_home(),
        ClientBinding::Unbound,
    )
    .unwrap()
    .preview();

    let lock = gents::home::lock_store(&home, &home.join("data")).unwrap();
    let locked = plan_home_reset(
        Some(&home),
        refused(),
        None,
        &desktop,
        not_the_user_home(),
        ClientBinding::Unbound,
    )
    .unwrap();
    locked
        .check_confirmation(&preview.confirmation, HomeResetDisposition::Archive)
        .unwrap();
    let reset = locked
        .retire(
            HomeResetDisposition::Archive,
            Some(&locked.backup_path("20260924T000000.000Z").unwrap()),
        )
        .unwrap();
    assert!(lock.path().is_file(), "the held lock stays in place");
    assert!(!home.join("data").exists());
    assert!(!reset
        .retired_paths
        .iter()
        .any(|path| path.ends_with("data.lock")));
}

#[tokio::test]
async fn an_opener_cannot_create_the_store_while_a_storeless_home_is_retired() {
    use std::sync::Arc;
    let temp = tempfile::tempdir().unwrap();
    let temp = std::fs::canonicalize(temp.path()).unwrap();
    let (home, desktop) = dirty_home(&temp);
    std::fs::remove_dir_all(home.join("data")).unwrap();
    let refused = refused_runtime_store(
        &home,
        Some(gents::storage_backend::IncompatibleStoreKind::InsecureKey),
    );

    let lock = lock_home_for_retirement(&home).unwrap();
    let plan = plan_home_reset(
        Some(&home),
        Some(refused),
        None,
        &desktop,
        not_the_user_home(),
        ClientBinding::Unbound,
    )
    .unwrap();
    let preview = plan.preview();
    assert!(!preview
        .planned_paths
        .iter()
        .any(|path| path.ends_with("/data")));

    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let opener = {
        let barrier = Arc::clone(&barrier);
        let home = home.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            std::fs::create_dir_all(home.join("data")).unwrap();
            gents::home::lock_store(&home, &home.join("data")).is_ok()
        })
    };
    barrier.wait().await;
    let opened = opener.await.unwrap();
    let reset = plan
        .retire(
            HomeResetDisposition::Archive,
            Some(&plan.backup_path("20260924T000000.000Z").unwrap()),
        )
        .unwrap();

    assert!(!opened, "the opener met the reset's lock");
    assert!(!reset
        .retired_paths
        .iter()
        .any(|path| path.ends_with("data.lock") || path.ends_with("/data")));
    assert!(!home.join("init.json").exists());
    assert!(!home.join("keys").exists());
    assert!(
        home.join("data").is_dir(),
        "a store created under the lock is not retired"
    );
    drop(lock);
    assert!(gents::home::lock_store(&home, &home.join("data")).is_ok());
}

#[test]
fn broad_roots_are_never_retired() {
    let temp = tempfile::tempdir().unwrap();
    let temp = std::fs::canonicalize(temp.path()).unwrap();
    let (home, desktop) = dirty_home(&temp);
    let refused = Some(lineage_refusal(home.join("data")));

    for user_home in [home.clone(), home.join("person")] {
        assert_eq!(
            plan_home_reset(
                Some(&home),
                refused.clone(),
                None,
                &desktop,
                &user_home,
                ClientBinding::Unbound
            )
            .unwrap_err()
            .code,
            BridgeErrorCode::InvalidArgument
        );
    }
    let user_home = desktop.root().join("person");
    assert_eq!(
        plan_home_reset(
            Some(&home),
            refused.clone(),
            None,
            &desktop,
            &user_home,
            ClientBinding::Unbound
        )
        .unwrap_err()
        .code,
        BridgeErrorCode::InvalidArgument
    );
    #[cfg(unix)]
    {
        let alias = temp.join("alias");
        std::os::unix::fs::symlink(&temp, &alias).unwrap();
        let aliased = std::fs::canonicalize(alias.join(".gents")).unwrap();
        assert_eq!(
            plan_home_reset(
                Some(&home),
                refused.clone(),
                None,
                &desktop,
                &aliased,
                ClientBinding::Unbound
            )
            .unwrap_err()
            .code,
            BridgeErrorCode::InvalidArgument
        );
    }
    assert!(ensure_retirable_root(Path::new("/"), "home", not_the_user_home()).is_err());
    assert!(
        home.join("data/MANIFEST").is_file(),
        "planning deletes nothing"
    );
}

#[test]
fn a_store_from_another_build_is_not_reported_as_older() {
    let temp = tempfile::tempdir().unwrap();
    let temp = std::fs::canonicalize(temp.path()).unwrap();
    let (home, desktop) = dirty_home(&temp);
    let preview = plan_home_reset(
        Some(&home),
        Some(gents::storage_backend::IncompatibleStore {
            kind: gents::storage_backend::IncompatibleStoreKind::ForeignVersion,
            data_path: home.join("data"),
        }),
        None,
        &desktop,
        not_the_user_home(),
        ClientBinding::Unbound,
    )
    .unwrap()
    .preview();
    assert!(!preview.stores[0].older);
    assert!(preview.stores[0].detail.contains("different Gents version"));
}

#[test]
fn reset_rejects_healthy_store_wrong_confirmation_and_unknown_endpoint() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data = home.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("MANIFEST"), b"REGOMAN current").unwrap();
    let desktop = gents_desktop_core::client::DesktopPaths::from_root(temp.path().join("desktop"));
    assert_eq!(
        plan_home_reset(
            Some(&home),
            None,
            None,
            &desktop,
            not_the_user_home(),
            ClientBinding::Unbound
        )
        .unwrap_err()
        .code,
        BridgeErrorCode::InvalidArgument
    );
    std::fs::remove_file(data.join("MANIFEST")).unwrap();
    std::fs::write(data.join("data.lark"), "legacy").unwrap();
    let plan = plan_home_reset(
        Some(&home),
        None,
        None,
        &desktop,
        not_the_user_home(),
        ClientBinding::Unbound,
    )
    .unwrap();
    assert_eq!(
        plan.check_confirmation("RESET SOME OTHER HOME", HomeResetDisposition::Archive)
            .unwrap_err()
            .code,
        BridgeErrorCode::InvalidArgument
    );
    let address = "127.0.0.1:9191".parse().unwrap();
    assert_eq!(
        ensure_managed_endpoint_stopped(
            address,
            Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "unknown")),
        )
        .unwrap_err()
        .code,
        BridgeErrorCode::Backend
    );
    assert_eq!(
        ensure_managed_endpoint_stopped(address, Ok(()))
            .unwrap_err()
            .code,
        BridgeErrorCode::InvalidArgument
    );
    ensure_managed_endpoint_stopped(
        address,
        Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "stopped",
        )),
    )
    .unwrap();
    assert_eq!(
        ensure_managed_runtime_stopped(true).unwrap_err().code,
        BridgeErrorCode::InvalidArgument
    );
    ensure_managed_runtime_stopped(false).unwrap();
}

#[cfg(unix)]
#[test]
fn reset_rejects_symlinked_homes_and_stores() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("data.lark"), "legacy").unwrap();
    symlink(&outside, home.join("data")).unwrap();
    let desktop = gents_desktop_core::client::DesktopPaths::from_root(temp.path().join("desktop"));
    assert_eq!(
        plan_home_reset(
            Some(&home),
            None,
            None,
            &desktop,
            not_the_user_home(),
            ClientBinding::Unbound
        )
        .unwrap_err()
        .code,
        BridgeErrorCode::PathEscapesRoot
    );
    let linked_home = temp.path().join("linked-home");
    symlink(&home, &linked_home).unwrap();
    assert_eq!(
        plan_home_reset(
            Some(&linked_home),
            None,
            None,
            &desktop,
            not_the_user_home(),
            ClientBinding::Unbound
        )
        .unwrap_err()
        .code,
        BridgeErrorCode::PathEscapesRoot
    );
}
