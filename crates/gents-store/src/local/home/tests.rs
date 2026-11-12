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
    let key = crate::local::store_key::StoreKeyError::Home(super::HomeError::LockHeld(held()));
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

#[cfg(unix)]
mod retirement {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Role {
        Inventory,
        Lock,
        Maintenance,
        Foreign,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Reference {
        Unreferenced,
        OtherHome,
        Exclusion,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Presence {
        File,
        Directory,
        Link,
        Vanished,
        Uninspectable,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Disposition {
        Archive,
        ArchiveOntoBackup,
        Delete,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Outcome {
        Retired,
        RolledBack,
        Refused,
        Stopped,
    }

    use gents_lean_vocab::families::home_retirement_retire as family;

    type RetirementScenario = family::Scenario<Disposition, Presence, Reference, Role>;
    type RetirementResult = family::Retirement<Outcome>;

    fn retire_scenario(scenario: &RetirementScenario) -> RetirementResult {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let (home, outside, backup) =
            (root.join("home"), root.join("outside"), root.join("backup"));
        let mut inventory = RUNTIME_HOME_ENTRIES.to_vec();
        inventory.sort_unstable();
        let mut names = [
            inventory.iter(),
            RUNTIME_HOME_LOCK_FILES.iter(),
            RUNTIME_HOME_RETAINED_ENTRIES.iter(),
        ];
        let mut exclude = Vec::new();
        let mut kept = Vec::new();
        let paths: Vec<PathBuf> = scenario
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let path = match item.role {
                    Role::Inventory => home.join(names[0].next().unwrap()),
                    Role::Lock => home.join(names[1].next().unwrap()),
                    Role::Maintenance => home.join(names[2].next().unwrap()),
                    Role::Foreign => home.join(format!("notes-{index}.md")),
                };
                let inside = match item.presence {
                    Presence::Directory => {
                        write(&path.join("child"), "child");
                        path.join("child")
                    }
                    Presence::Link => {
                        let target = outside.join(index.to_string());
                        write(&target.join("keep.txt"), "keep");
                        kept.push(target.join("keep.txt"));
                        fs::create_dir_all(&home).unwrap();
                        std::os::unix::fs::symlink(&target, &path).unwrap();
                        path.join("keep.txt")
                    }
                    Presence::File | Presence::Vanished | Presence::Uninspectable => {
                        write(&path, "entry");
                        path.clone()
                    }
                };
                match item.reference {
                    Reference::Unreferenced => {}
                    Reference::Exclusion => exclude.push(inside),
                    Reference::OtherHome => {
                        let other = home.join(format!("other-home-{index}"));
                        let field = ["key_path", "tool_root"][index % 2];
                        write(
                            &other.join("init.json"),
                            &serde_json::json!({"home": other, field: inside}).to_string(),
                        );
                    }
                }
                path
            })
            .collect();

        let entries = home_entries(&home, &exclude).unwrap();
        for path in &paths {
            assert_ne!(
                entries.owned.contains(path),
                entries.retained.contains(path),
                "{} is listed once",
                path.display()
            );
        }
        let flags = |listed: &[PathBuf]| paths.iter().map(|path| listed.contains(path)).collect();
        let owned: Vec<bool> = flags(&entries.owned);

        let mut denied = Vec::new();
        for (item, path) in scenario.items.iter().zip(&paths) {
            match item.presence {
                Presence::Vanished => fs::remove_file(path).unwrap(),
                Presence::Uninspectable => denied.push(path.clone()),
                _ => {}
            }
        }
        let inspect = |path: &Path| {
            if denied.iter().any(|denied| denied == path) {
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            } else {
                fs::symlink_metadata(path).map(|metadata| metadata.is_dir())
            }
        };
        let first = entries.owned.len().min(1);
        let split = match scenario.passes {
            1 => vec![&entries.owned[..]],
            _ => vec![&entries.owned[..first], &entries.owned[first..]],
        };
        let groups: Vec<RetireGroup<'_>> = split
            .into_iter()
            .map(|entries| RetireGroup {
                name: "home",
                entries,
            })
            .collect();
        let disposition = match scenario.disposition {
            Disposition::Delete => RetireDisposition::Delete,
            Disposition::Archive => RetireDisposition::Archive { backup: &backup },
            Disposition::ArchiveOntoBackup => {
                fs::create_dir(&backup).unwrap();
                RetireDisposition::Archive { backup: &backup }
            }
        };
        let (outcome, reported) = match retire_entries_with(&groups, disposition, inspect) {
            Ok(reported) => (Outcome::Retired, reported),
            Err(HomeError::ArchiveRolledBack { .. }) => (Outcome::RolledBack, Vec::new()),
            Err(HomeError::CreateBackup { .. }) => (Outcome::Refused, Vec::new()),
            Err(HomeError::DeleteStopped { removed, .. }) => (
                Outcome::Stopped,
                removed
                    .split(", ")
                    .filter(|path| !path.is_empty())
                    .map(PathBuf::from)
                    .collect(),
            ),
            Err(error) => panic!("{}", ErrorChain(&error)),
        };

        use std::os::unix::fs::PermissionsExt;
        let mode =
            |path: &Path| u64::from(fs::metadata(path).unwrap().permissions().mode() & 0o777);
        let archived = |path: &PathBuf| {
            fs::symlink_metadata(backup.join("home").join(path.file_name().unwrap())).is_ok()
        };
        RetirementResult {
            outcome,
            owned,
            reported: flags(&reported),
            in_home: paths
                .iter()
                .map(|path| fs::symlink_metadata(path).is_ok())
                .collect(),
            archived: paths.iter().map(archived).collect(),
            backup: backup.exists(),
            backup_mode: (outcome == Outcome::Retired && backup.exists()).then(|| {
                assert_eq!(mode(&backup), mode(&backup.join("home")));
                mode(&backup)
            }),
            outside_intact: kept.iter().all(|path| path.is_file()),
        }
    }

    #[test]
    fn retirement_matches_the_lean_model() {
        for case in family::cases::<Disposition, Outcome, Presence, Reference, Role>() {
            case.check(retire_scenario(&case.input));
        }
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
                if !RUNTIME_HOME_ENTRIES.contains(&top)
                    && !RUNTIME_HOME_LOCK_FILES.contains(&top)
                    && !RUNTIME_HOME_RETAINED_ENTRIES.contains(&top)
                {
                    unlisted.push(format!("{}: {top}", at()));
                }
            }
            for capture in constant.captures_iter(line) {
                match resolve(&capture[1]) {
                    Some(value)
                        if RUNTIME_HOME_ENTRIES.contains(&value.as_str())
                            || RUNTIME_HOME_LOCK_FILES.contains(&value.as_str())
                            || RUNTIME_HOME_RETAINED_ENTRIES.contains(&value.as_str()) => {}
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

fn plant_remembered(config_dir: &std::path::Path, contents: &str) -> std::path::PathBuf {
    let remembered = config_dir.join("gents/home");
    std::fs::create_dir_all(remembered.parent().unwrap()).unwrap();
    std::fs::write(&remembered, contents).unwrap();
    remembered
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Env {
    Absent,
    Empty,
    Set,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Remembered {
    Absent,
    Unreadable,
    NewlineOnly,
    Empty,
    Relative,
    MultiLine,
    CarriageReturn,
    Absolute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Resolution {
    FromEnv,
    FromRemembered,
    DotGents,
    Unreadable,
    Invalid,
    Unresolved,
}

const ENV_HOME: &str = "/srv/env";
const REMEMBERED_HOME: &str = "/srv/remembered";

fn resolve_in_temp(
    env: Env,
    remembered: Remembered,
    config_dir: bool,
    user_home: bool,
) -> Resolution {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config");
    let user = temp.path().join("user");
    fs::create_dir_all(&config).unwrap();
    fs::create_dir_all(&user).unwrap();
    let contents = match remembered {
        Remembered::Absent => None,
        Remembered::Unreadable => {
            fs::create_dir_all(config.join("gents/home")).unwrap();
            None
        }
        Remembered::NewlineOnly => Some("\n"),
        Remembered::Empty => Some(""),
        Remembered::Relative => Some("relative/home\n"),
        Remembered::MultiLine => Some("/a\n/b\n"),
        Remembered::CarriageReturn => Some("/a\r\n"),
        Remembered::Absolute => Some("/srv/remembered\n"),
    };
    if let Some(contents) = contents {
        plant_remembered(&config, contents);
    }
    let env_home = match env {
        Env::Absent => None,
        Env::Empty => Some(std::ffi::OsString::new()),
        Env::Set => Some(std::ffi::OsString::from(ENV_HOME)),
    };
    let resolved = super::default_home_from(
        env_home,
        config_dir.then_some(config.as_path()),
        user_home.then_some(user.as_path()),
    );
    match resolved {
        Ok(home) if home.as_path() == Path::new(ENV_HOME) => Resolution::FromEnv,
        Ok(home) if home.as_path() == Path::new(REMEMBERED_HOME) => Resolution::FromRemembered,
        Ok(home) if home == user.join(".gents") => Resolution::DotGents,
        Ok(home) => panic!("unexpected home {}", home.display()),
        Err(super::HomeError::RememberedHomeRead { .. }) => Resolution::Unreadable,
        Err(super::HomeError::RememberedHomeInvalid { .. }) => Resolution::Invalid,
        Err(super::HomeError::HomeDirUnresolved) => Resolution::Unresolved,
        Err(error) => panic!("{}", ErrorChain(&error)),
    }
}

#[test]
fn home_resolution_matches_the_lean_model() {
    gents_lean_vocab::families::home_resolution_resolve::replay::<Env, Remembered, Resolution>(
        |i| resolve_in_temp(i.env, i.remembered, i.config_dir, i.user_home),
    );
}

#[test]
fn a_directory_at_the_file_path_is_a_read_error() {
    let config = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    let remembered = config.path().join("gents/home");
    std::fs::create_dir_all(&remembered).unwrap();

    let error = super::default_home_from(None, Some(config.path()), Some(user.path())).unwrap_err();

    assert!(
        matches!(&error, super::HomeError::RememberedHomeRead { path, .. } if *path == remembered),
        "{error:?}"
    );
    assert!(error.to_string().contains(&*remembered.to_string_lossy()));
}

#[test]
fn empty_or_relative_contents_are_invalid() {
    let user = tempfile::tempdir().unwrap();
    for contents in ["", "\n", "relative/home\n", "/a\n/b\n", "/a\r\n"] {
        let config = tempfile::tempdir().unwrap();
        let remembered = plant_remembered(config.path(), contents);

        let error =
            super::default_home_from(None, Some(config.path()), Some(user.path())).unwrap_err();

        assert!(
            matches!(&error, super::HomeError::RememberedHomeInvalid { path } if *path == remembered),
            "{contents:?}: {error:?}"
        );
        assert!(error.to_string().contains(&*remembered.to_string_lossy()));
    }
}

#[test]
fn path_with_spaces_round_trips() {
    let config = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    let chosen = std::path::Path::new("/srv/my gents home ");

    super::remember_home_in(config.path(), user.path(), chosen).unwrap();
    let resolved = super::default_home_from(None, Some(config.path()), Some(user.path()));

    assert_eq!(resolved.unwrap(), chosen);
}

#[test]
fn remember_writes_the_absolute_path_at_gents_slash_home() {
    let config = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    let remembered = config.path().join("gents/home");

    super::remember_home_in(
        config.path(),
        user.path(),
        std::path::Path::new("/srv/gents"),
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(&remembered).unwrap(),
        "/srv/gents\n"
    );

    super::remember_home_in(config.path(), user.path(), std::path::Path::new("relative")).unwrap();
    assert_eq!(
        std::fs::read_to_string(&remembered).unwrap(),
        format!(
            "{}\n",
            std::env::current_dir().unwrap().join("relative").display()
        )
    );
}

#[test]
fn remember_refuses_a_path_with_a_line_break() {
    let config = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    for chosen in ["/srv/a\nb", "/srv/a\rb", "/srv/a\n"] {
        let error =
            super::remember_home_in(config.path(), user.path(), std::path::Path::new(chosen))
                .unwrap_err();
        assert!(
            matches!(error, super::HomeError::RememberedHomeWrite { .. }),
            "{chosen:?}: {error:?}"
        );
    }
    assert!(!config.path().join("gents/home").exists());
}

#[test]
fn remembering_dot_gents_removes_the_file() {
    let config = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    let dot_gents = user.path().join(".gents");
    let remembered = plant_remembered(config.path(), "/srv/remembered\n");

    super::remember_home_in(config.path(), user.path(), &dot_gents).unwrap();
    assert!(!remembered.exists());

    super::remember_home_in(config.path(), user.path(), &dot_gents)
        .expect("nothing remembered is already the wanted state");
}

use std::sync::{Mutex, MutexGuard};

static FORK_EXCLUSION: Mutex<()> = Mutex::new(());

fn exclusive() -> MutexGuard<'static, ()> {
    FORK_EXCLUSION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
fn a_second_holder_cannot_lock_a_store() {
    let _exclusive = exclusive();
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
    assert!(
        error.contains("gents runtime service stop --home"),
        "{error}"
    );

    drop(held);
    lock_store(&home, &data).expect("the lock is released with its holder");
}

#[test]
fn a_held_store_lock_is_classified_with_its_home_and_holder() {
    let _guard = exclusive();
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    std::fs::create_dir_all(&data).unwrap();

    let _held = lock_store(home.path(), &data).unwrap();
    let error = lock_store(home.path(), &data).unwrap_err();

    let held = error
        .lock_held()
        .expect("a contended store lock is classified, not just described");
    assert_eq!(held.home, home.path());
    assert_eq!(held.holder_pid, Some(std::process::id()));
}

#[cfg(unix)]
#[test]
fn every_alias_of_a_store_takes_the_same_lock() {
    let _exclusive = exclusive();
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

    let current = lock_store(temp.path(), &real.join(".")).unwrap();
    assert_eq!(
        current.path(),
        fs::canonicalize(temp.path())
            .unwrap()
            .join("real-data.lock")
    );
}

#[test]
fn a_home_lock_without_a_store_excludes_the_store_created_later() {
    let _exclusive = exclusive();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".gents");
    fs::create_dir_all(&home).unwrap();
    let held = lock_home_store(&home).unwrap();
    assert!(
        !default_data_dir(&home).exists(),
        "locking creates no store"
    );

    fs::create_dir(default_data_dir(&home)).unwrap();
    assert!(
        lock_store(&home, &default_data_dir(&home)).is_err(),
        "an opener that creates the store meets the same lock"
    );
    assert_eq!(
        held.path(),
        fs::canonicalize(&home).unwrap().join("data.lock")
    );
    drop(held);
    lock_store(&home, &default_data_dir(&home)).unwrap();
}

#[cfg(unix)]
struct BlockedChild {
    pid: libc::pid_t,
    release: libc::c_int,
}

#[cfg(unix)]
impl BlockedChild {
    fn release(mut self) -> libc::c_int {
        self.release_and_reap()
    }

    fn release_and_reap(&mut self) -> libc::c_int {
        let mut status = 0;
        if self.release >= 0 {
            unsafe { libc::close(self.release) };
            self.release = -1;
            while unsafe { libc::waitpid(self.pid, &mut status, 0) } == -1
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
            }
        }
        status
    }
}

#[cfg(unix)]
impl Drop for BlockedChild {
    fn drop(&mut self) {
        self.release_and_reap();
    }
}

#[test]
fn a_store_outside_the_home_does_not_exclude_the_home_default_store() {
    let _exclusive = exclusive();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".gents");
    fs::create_dir_all(&home).unwrap();
    let outside = temp.path().join("elsewhere-data");
    fs::create_dir_all(&outside).unwrap();
    fs::create_dir_all(default_data_dir(&home)).unwrap();

    let held = lock_store(&home, &outside).unwrap();
    assert_eq!(
        held.path(),
        fs::canonicalize(temp.path())
            .unwrap()
            .join("elsewhere-data.lock")
    );
    let home_store = lock_store(&home, &default_data_dir(&home))
        .expect("a store outside the home leaves the home store free");
    assert_ne!(held.path(), home_store.path());
}

#[cfg(unix)]
#[test]
fn dropping_the_store_lock_releases_it_even_before_a_forked_child_execs() {
    use std::ffi::CString;

    let _exclusive = exclusive();
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    fs::create_dir_all(&data).unwrap();
    let held = lock_store(temp.path(), &data).unwrap();

    // The child can make only async-signal-safe calls between fork and exec.
    let program = CString::new("/usr/bin/true").unwrap();
    let argv = [program.as_ptr(), std::ptr::null()];
    let mut go = [0; 2];
    assert_eq!(unsafe { libc::pipe(go.as_mut_ptr()) }, 0);
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        unsafe {
            libc::close(go[1]);
            let mut byte = 0u8;
            while libc::read(go[0], (&mut byte as *mut u8).cast(), 1) == -1
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
            }
            libc::execv(program.as_ptr(), argv.as_ptr());
            libc::_exit(127);
        }
    }
    unsafe { libc::close(go[0]) };
    if pid < 0 {
        unsafe { libc::close(go[1]) };
        panic!("fork failed: {}", std::io::Error::last_os_error());
    }
    let child = BlockedChild {
        pid,
        release: go[1],
    };

    let error = lock_store(temp.path(), &data)
        .expect_err("the parent still holds the lock")
        .to_string();
    assert!(
        error.contains(&format!("process {}", std::process::id())),
        "the holder is recorded as this process: {error}"
    );

    drop(held);
    let reacquired = lock_store(temp.path(), &data)
        .expect("the child cannot retain a lock its parent explicitly released");
    let status = child.release();
    assert!(
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
        "the child exec'd: {status}"
    );
    assert!(
        lock_store(temp.path(), &data).is_err(),
        "child exec must not release the new owner's lock"
    );
    drop(reacquired);
    lock_store(temp.path(), &data).expect("the new owner released its lock");
}

#[test]
fn archive_moves_the_bootstrap_reservation_with_the_store() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    write(&home.join("data/MANIFEST"), "store");
    write(&home.join("init.json"), "{}");
    write(&home.join(NODE_BOOTSTRAP_CONSUMED_FILE_NAME), "");
    write(&home.join(SERVICE_UPDATES_DIR_NAME).join("receipt"), "kept");
    write(&home.join("data.lock"), "");
    let entries = home_entries(&home, &[]).unwrap();
    assert!(
        entries
            .owned
            .contains(&home.join(NODE_BOOTSTRAP_CONSUMED_FILE_NAME)),
        "{:#?}",
        entries
    );

    let backup = temp.path().join("home-backup");
    retire_entries(
        &[RetireGroup {
            name: "home",
            entries: &entries.owned,
        }],
        RetireDisposition::Archive { backup: &backup },
    )
    .unwrap();

    assert!(backup
        .join("home")
        .join(NODE_BOOTSTRAP_CONSUMED_FILE_NAME)
        .exists());
    assert!(!home.join(NODE_BOOTSTRAP_CONSUMED_FILE_NAME).exists());
    assert!(home.join(SERVICE_UPDATES_DIR_NAME).join("receipt").exists());
    assert!(home.join("data.lock").exists());
}
