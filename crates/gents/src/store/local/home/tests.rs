use gents_protocol::error_chain::ErrorChain;

#[test]
fn store_lock_contention_classifies_through_every_owner_wrapper() {
    let held = || super::StoreLockHeld {
        home: "/h".into(),
        holder_pid: Some(7),
    };
    let held_by = |error: anyhow::Error| {
        super::store_lock_held(error.context("opening the store").as_ref())
            .and_then(|held| held.holder_pid)
    };
    assert_eq!(held_by(anyhow::anyhow!(held())), Some(7));
    assert_eq!(held_by(super::HomeError::LockHeld(held()).into()), Some(7));
    let key =
        crate::store::local::store_key::StoreKeyError::Home(super::HomeError::LockHeld(held()));
    assert_eq!(key.lock_held().and_then(|held| held.holder_pid), Some(7));
    assert_eq!(held_by(key.into()), Some(7));
    assert_eq!(held_by(super::HomeError::HomeDirUnresolved.into()), None);
}

#[test]
fn secure_enclave_homes_are_refused_before_serving() {
    let home = tempfile::tempdir().unwrap();
    super::ensure_home_identity_can_serve(home.path()).unwrap();
    let init = |backend: serde_json::Value| {
        std::fs::write(
            super::init_config_path(home.path()),
            serde_json::json!({
                "home": home.path(),
                "node_name": "a",
                "node_did": "did:key:z6Mk",
                "key_path": null,
                "identity_backend": backend,
                "tool_ceiling": "readonly",
                "tool_root": null,
            })
            .to_string(),
        )
        .unwrap();
    };
    init(serde_json::json!("macos-keychain"));
    super::ensure_home_identity_can_serve(home.path()).unwrap();
    init(serde_json::json!(super::SECURE_ENCLAVE_IDENTITY_BACKEND));
    let refused = super::ensure_home_identity_can_serve(home.path())
        .unwrap_err()
        .to_string();
    assert!(refused.contains("macos-secure-enclave"), "{refused}");
    assert!(refused.contains("--identity-backend file"), "{refused}");
}

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

use std::fs;

use super::lock::{open_runtime_lock, start_runtime_startup_waiter, start_store_release_waiter};
use super::retire::{archive_preflight_with, retire_entries_with};
use super::*;

#[tokio::test]
async fn store_release_waiter_blocks_until_the_owner_releases_the_lock() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    fs::create_dir_all(&data).unwrap();
    let held = lock_store(temp.path(), &data).unwrap();

    let mut waiter = tokio::spawn({
        let home = temp.path().to_path_buf();
        let data = data.clone();
        async move { wait_for_store_release(&home, &data).await }
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), &mut waiter)
            .await
            .is_err()
    );

    drop(held);
    tokio::time::timeout(std::time::Duration::from_secs(2), waiter)
        .await
        .expect("waiter completes after the store lock is released")
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn cancelling_store_release_wait_does_not_keep_the_eventual_lock() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    fs::create_dir_all(&data).unwrap();
    let held = lock_store(temp.path(), &data).unwrap();

    let (mut receive, worker) = start_store_release_waiter(temp.path(), &data).unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), &mut receive)
            .await
            .is_err()
    );
    drop(receive);
    drop(held);

    tokio::task::spawn_blocking(move || worker.join())
        .await
        .expect("joining store release waiter")
        .expect("store release waiter thread completes");
    let reservation =
        lock_store(temp.path(), &data).expect("cancelled waiter's eventual acquisition is dropped");
    drop(reservation);
}

#[tokio::test]
async fn runtime_startup_reservation_excludes_a_second_preparer_until_ready() {
    let home = tempfile::tempdir().unwrap();
    let first = reserve_runtime_startup(home.path()).await.unwrap();
    let (mut receive, worker) = start_runtime_startup_waiter(home.path()).unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), &mut receive)
            .await
            .is_err()
    );
    drop(first);
    let second = tokio::time::timeout(std::time::Duration::from_secs(2), receive)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::task::spawn_blocking(move || worker.join())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        open_runtime_lock(&home.path().join(RUNTIME_STARTUP_LOCK_FILE_NAME))
            .unwrap()
            .try_lock(),
        Err(fs::TryLockError::WouldBlock)
    ));
    drop(second);
    reserve_runtime_startup(home.path()).await.unwrap();
}

#[tokio::test]
async fn cancelled_runtime_startup_reservation_releases_its_eventual_acquisition() {
    let home = tempfile::tempdir().unwrap();
    let first = reserve_runtime_startup(home.path()).await.unwrap();
    let (mut receive, worker) = start_runtime_startup_waiter(home.path()).unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), &mut receive)
            .await
            .is_err()
    );
    drop(receive);
    drop(first);
    tokio::task::spawn_blocking(move || worker.join())
        .await
        .unwrap()
        .unwrap();
    reserve_runtime_startup(home.path()).await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn runtime_startup_refuses_a_planted_lock_symlink_without_changing_history() {
    let home = tempfile::tempdir().unwrap();
    let history = home.path().join("history");
    fs::write(&history, "saved history").unwrap();
    std::os::unix::fs::symlink(&history, home.path().join(RUNTIME_STARTUP_LOCK_FILE_NAME)).unwrap();
    assert!(reserve_runtime_startup(home.path()).await.is_err());
    assert_eq!(fs::read_to_string(history).unwrap(), "saved history");
}

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
fn permission_locks_remain_in_place_when_owned_state_is_retired() {
    let home = tempfile::tempdir().unwrap();
    let open = |name: &str| {
        fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(home.path().join(name))
            .unwrap()
    };
    let _held: Vec<_> = RUNTIME_HOME_LOCK_FILES
        .iter()
        .map(|name| crate::store::local::file_lock::FileLock::exclusive(open(name)).unwrap())
        .collect();
    fs::write(home.path().join(ALLOWED_DIRS_FILE_NAME), b"{}").unwrap();
    fs::write(home.path().join(WORKSPACE_APPROVALS_FILE_NAME), b"{}").unwrap();
    let staging = home.path().join(PERMISSION_STAGING_DIR_NAME);
    fs::create_dir(&staging).unwrap();
    fs::write(staging.join("pending"), b"{}").unwrap();
    let entries = home_entries(home.path(), &[]).unwrap();
    retire_entries(
        &[RetireGroup {
            name: "runtime",
            entries: &entries.owned,
        }],
        RetireDisposition::Delete,
    )
    .unwrap();
    assert!(!home.path().join(ALLOWED_DIRS_FILE_NAME).exists());
    assert!(!home.path().join(WORKSPACE_APPROVALS_FILE_NAME).exists());
    assert!(!staging.exists());
    for name in RUNTIME_HOME_LOCK_FILES {
        assert!(
            matches!(
                crate::store::local::file_lock::FileLock::try_exclusive(open(name)),
                Err(fs::TryLockError::WouldBlock)
            ),
            "{name}"
        );
    }
}

#[test]
fn archive_accepts_a_group_retired_in_two_passes() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let (logs, record) = (root.join("logs"), root.join("init.json"));
    write(&logs.join("a.log"), "log");
    write(&record, "{}");
    let backup = root.join("backup");
    let (first, second) = (vec![logs.clone()], vec![record.clone()]);
    let groups = [
        RetireGroup {
            name: "home",
            entries: &first,
        },
        RetireGroup {
            name: "home",
            entries: &second,
        },
    ];
    retire_entries(&groups, RetireDisposition::Archive { backup: &backup }).unwrap();
    assert!(backup.join("home/logs/a.log").is_file());
    assert!(backup.join("home/init.json").is_file());
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

#[test]
fn runtime_writers_name_home_entries_only_from_the_inventory() {
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

    let mut values = rapidhash::RapidHashMap::<String, String>::default();
    let mut aliases = rapidhash::RapidHashMap::<String, String>::default();
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
                if !RUNTIME_HOME_ENTRIES.contains(&top) && !RUNTIME_HOME_LOCK_FILES.contains(&top) {
                    unlisted.push(format!("{}: {top}", at()));
                }
            }
            for capture in constant.captures_iter(line) {
                match resolve(&capture[1]) {
                    Some(value)
                        if RUNTIME_HOME_ENTRIES.contains(&value.as_str())
                            || RUNTIME_HOME_LOCK_FILES.contains(&value.as_str()) => {}
                    Some(value) => unlisted.push(format!("{}: {} = {value}", at(), &capture[1])),
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
        "home entries written outside the home inventories: {unlisted:#?}"
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

    assert!(
        ErrorChain(&error).to_string().contains("rolled back"),
        "{}",
        ErrorChain(&error)
    );
    assert_eq!(
        fs::read_to_string(home.join("data/MANIFEST")).unwrap(),
        "store"
    );
    assert!(!backup.exists());
}

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
    assert!(
        ErrorChain(&error).to_string().contains("rolled back"),
        "{}",
        ErrorChain(&error)
    );
    assert!(home.join("data/MANIFEST").is_file());
    assert!(!backup.exists());

    let error = retire_entries_with(&groups, RetireDisposition::Delete, inspect).unwrap_err();
    let message = ErrorChain(&error).to_string();
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
