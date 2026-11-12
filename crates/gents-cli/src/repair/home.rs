use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use gents::store::local::home::{
    default_data_dir, home_entries, keys_dir, lock_home_store, read_init_config, HomeError,
    BOOTSTRAP_CONSUMED_FILE_NAME, INIT_PENDING_FILE_NAME,
};
use gents::store::local::storage_backend::IncompatibleStoreKind;
use gents_protocol::error_chain::ErrorChain;

use crate::home_state::init_config_path;
use crate::native_service::ServiceRefusal;
use crate::repair::Blocker;
use crate::shared::StoredInitConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokenHome {
    SetupStoppedMidway,
    InitConfigUnreadable,
    InitConfigMissing,
    StoreKeyMissing,
    /// The default home's own stopped service last refused its data as an
    /// older version's: a program upgraded in place leaves no other trace.
    OlderVersionData,
}

impl fmt::Display for BrokenHome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::SetupStoppedMidway => "its setup stopped midway",
            Self::InitConfigUnreadable => "its settings file (init.json) can't be read",
            Self::InitConfigMissing => "its settings file (init.json) is missing",
            Self::StoreKeyMissing => "the key for its data is missing",
            Self::OlderVersionData => "its data is from an older Gents version",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeState {
    Absent,
    Complete,
    Broken(BrokenHome),
}

pub fn classify_home(
    home: &Path,
    last_refusal: Option<ServiceRefusal>,
) -> Result<HomeState, Blocker> {
    let unreadable = |error: &dyn std::error::Error| Blocker::HomeUnreadable {
        home: home.to_path_buf(),
        reason: ErrorChain(error).to_string(),
    };
    if !home.try_exists().map_err(|error| unreadable(&error))? {
        return Ok(HomeState::Absent);
    }
    let entries = home_entries(home, &[]).map_err(|error| unreadable(&error))?;
    let bootstrap_consumed = home.join(BOOTSTRAP_CONSUMED_FILE_NAME).exists();
    if entries.owned.is_empty() && !bootstrap_consumed {
        return Ok(HomeState::Absent);
    }
    if home.join(INIT_PENDING_FILE_NAME).exists()
        || (bootstrap_consumed && !init_config_path(home).exists())
    {
        return Ok(HomeState::Broken(BrokenHome::SetupStoppedMidway));
    }
    let init_config: Result<Option<StoredInitConfig>, HomeError> = read_init_config(home);
    match init_config {
        Err(HomeError::DecodeInitConfig { .. }) => {
            Ok(HomeState::Broken(BrokenHome::InitConfigUnreadable))
        }
        Err(error) => Err(unreadable(&error)),
        // No settings, data or keys: what is left is a deleted folder's debris,
        // such as the markers a dying node recreates mid-delete, so setup
        // starts as for a missing folder and nothing is reported as repaired.
        Ok(None) if !default_data_dir(home).exists() && !keys_dir(home).exists() => {
            Ok(HomeState::Absent)
        }
        Ok(None) => Ok(HomeState::Broken(BrokenHome::InitConfigMissing)),
        Ok(Some(_))
            if last_refusal
                == Some(ServiceRefusal::Store(
                    IncompatibleStoreKind::MissingStoreKey,
                )) =>
        {
            Ok(HomeState::Broken(BrokenHome::StoreKeyMissing))
        }
        Ok(Some(_)) => Ok(HomeState::Complete),
    }
}

/// `state`, unless it reads as a setup that stopped midway while setup still
/// runs. Setup holds the store lock until it finishes, but a node serving the
/// home holds the same lock, so the lock counts only while none serves it.
pub fn unless_being_set_up(
    home: &Path,
    state: HomeState,
    node_serving: bool,
) -> Result<HomeState, Blocker> {
    if node_serving || state != HomeState::Broken(BrokenHome::SetupStoppedMidway) {
        return Ok(state);
    }
    match lock_home_store(home) {
        Ok(_) => Ok(state),
        Err(HomeError::LockHeld(_)) => Err(Blocker::HomeBusy {
            home: home.to_path_buf(),
        }),
        Err(error) => Err(Blocker::HomeUnreadable {
            home: home.to_path_buf(),
            reason: ErrorChain(&error).to_string(),
        }),
    }
}

/// `home` without a trailing separator, so the name lands beside the home
/// rather than inside it.
fn beside(home: &Path) -> PathBuf {
    home.components().collect()
}

/// What a set-aside home's name says it holds: the data of `from`, a version
/// this one couldn't open, or else a broken home.
pub fn aside_label(from: Option<&semver::Version>) -> String {
    from.map_or_else(|| "broken".to_string(), |from| format!("from-{from}"))
}

/// [`aside_label`] for data an older version wrote, `from` when it is known.
pub fn older_label(from: Option<&semver::Version>) -> String {
    match from {
        Some(_) => aside_label(from),
        None => "from-older".to_string(),
    }
}

pub fn set_aside_target(home: &Path, label: &str, stamp: &str) -> PathBuf {
    let home = beside(home);
    let named = |suffix: String| {
        let mut name = home.as_os_str().to_os_string();
        name.push(format!(".{label}-{stamp}{suffix}"));
        PathBuf::from(name)
    };
    let mut target = named(String::new());
    let mut attempt = 1;
    while fs::symlink_metadata(&target).is_ok() {
        attempt += 1;
        target = named(format!("-{attempt}"));
    }
    target
}

/// Renames the home beside itself so the next setup starts in an empty folder.
/// A symlinked home moves the link, never the data it points to. Any failure
/// leaves the home where it was.
///
/// This is not a bootstrap replay: Lean
/// `Identity.NodeProvisioning.bootstrap_crash_cannot_replay` forbids granting
/// the crashed reservation again, and the renamed folder keeps its
/// `.node-bootstrap-consumed`. A new home at the old path is a new reservation
/// with fresh evidence. It is the one exception to the marker staying at the
/// path (`RUNTIME_HOME_RETAINED_ENTRIES`): the whole folder moves, identity
/// keys and data included, so the next setup provisions a new node and confers
/// nothing from the old one.
pub async fn set_aside_home(home: &Path) -> Result<PathBuf, Blocker> {
    set_aside_home_as(home, aside_label(None)).await
}

/// [`set_aside_home`], naming the folder by `label` (see [`aside_label`]).
pub async fn set_aside_home_as(home: &Path, label: String) -> Result<PathBuf, Blocker> {
    let from = beside(home);
    let stamp = chrono::Utc::now().format("%Y-%m-%d-%H%M%S").to_string();
    tokio::task::spawn_blocking(move || {
        let to = set_aside_target(&from, &label, &stamp);
        fs::rename(&from, &to).map(|()| to)
    })
    .await
    .unwrap_or_else(|join| Err(std::io::Error::other(join)))
    .map_err(|error| Blocker::CannotSetAside {
        home: home.to_path_buf(),
        reason: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete(home: &Path) {
        fs::create_dir_all(home.join("data")).unwrap();
        fs::write(
            home.join("init.json"),
            serde_json::json!({
                "home": home,
                "node_name": "local",
                "node_did": "did:key:zlocal",
                "key_path": null,
                "tool_ceiling": "Readwrite",
                "tool_root": null,
            })
            .to_string(),
        )
        .unwrap();
    }

    fn broken(state: Result<HomeState, Blocker>) -> BrokenHome {
        match state {
            Ok(HomeState::Broken(broken)) => broken,
            other => panic!("expected a broken home, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_folder_is_absent() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(
            classify_home(&temp.path().join("home"), None),
            Ok(HomeState::Absent)
        );
    }

    #[test]
    fn an_empty_folder_is_absent() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("notes.md"), "mine").unwrap();
        assert_eq!(classify_home(temp.path(), None), Ok(HomeState::Absent));
    }

    #[test]
    fn a_pending_marker_is_a_setup_that_stopped_midway() {
        let temp = tempfile::tempdir().unwrap();
        complete(temp.path());
        fs::write(temp.path().join(INIT_PENDING_FILE_NAME), "").unwrap();
        assert_eq!(
            broken(classify_home(temp.path(), None)),
            BrokenHome::SetupStoppedMidway
        );
    }

    #[test]
    fn a_setup_in_progress_is_busy_not_broken() {
        let temp = tempfile::tempdir().unwrap();
        complete(temp.path());
        fs::write(temp.path().join(INIT_PENDING_FILE_NAME), "").unwrap();
        let stopped = classify_home(temp.path(), None).unwrap();
        assert_eq!(stopped, HomeState::Broken(BrokenHome::SetupStoppedMidway));
        let held = gents::store::local::home::lock_home_store(temp.path()).unwrap();
        let state = unless_being_set_up(temp.path(), stopped.clone(), false);
        assert_eq!(
            state,
            Err(Blocker::HomeBusy {
                home: temp.path().to_path_buf()
            })
        );
        assert_eq!(
            state.unwrap_err().to_string(),
            format!(
                "{} is being set up right now. Try again when it finishes.",
                temp.path().display()
            )
        );
        // A running node holds the same lock: its home is stopped and set aside.
        assert_eq!(
            unless_being_set_up(temp.path(), stopped.clone(), true),
            Ok(stopped.clone())
        );
        drop(held);
        assert_eq!(
            unless_being_set_up(temp.path(), stopped.clone(), false),
            Ok(stopped)
        );
    }

    #[test]
    fn a_consumed_bootstrap_without_settings_is_a_setup_that_stopped_midway() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join(BOOTSTRAP_CONSUMED_FILE_NAME), "").unwrap();
        assert_eq!(
            broken(classify_home(temp.path(), None)),
            BrokenHome::SetupStoppedMidway
        );
    }

    #[test]
    fn garbage_settings_are_unreadable() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("init.json"), "{").unwrap();
        let state = broken(classify_home(temp.path(), None));
        assert_eq!(state, BrokenHome::InitConfigUnreadable);
        assert_eq!(
            state.to_string(),
            "its settings file (init.json) can't be read"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_settings_block_with_their_reason() {
        use std::os::unix::fs::PermissionsExt as _;
        let temp = tempfile::tempdir().unwrap();
        complete(temp.path());
        let settings = temp.path().join("init.json");
        fs::set_permissions(&settings, fs::Permissions::from_mode(0o000)).unwrap();
        let state = classify_home(temp.path(), None);
        fs::set_permissions(&settings, fs::Permissions::from_mode(0o600)).unwrap();
        match state {
            Err(blocker @ Blocker::HomeUnreadable { .. }) => assert!(
                blocker
                    .to_string()
                    .ends_with(". Check its permissions, then try again."),
                "{blocker}"
            ),
            other => panic!("expected HomeUnreadable, got {other:?}"),
        }
    }

    #[test]
    fn a_deleted_folders_debris_is_a_missing_folder() {
        let temp = tempfile::tempdir().unwrap();
        let markers = temp.path().join("plugin-approvals/v3");
        fs::create_dir_all(&markers).unwrap();
        fs::write(markers.join(".listening"), "").unwrap();
        assert_eq!(classify_home(temp.path(), None), Ok(HomeState::Absent));
    }

    #[test]
    fn data_without_settings_is_missing_settings() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("data")).unwrap();
        let state = broken(classify_home(temp.path(), None));
        assert_eq!(state, BrokenHome::InitConfigMissing);
        assert_eq!(
            state.to_string(),
            "its settings file (init.json) is missing"
        );
    }

    #[test]
    fn a_missing_key_refusal_marks_the_store_key_missing() {
        let temp = tempfile::tempdir().unwrap();
        complete(temp.path());
        assert_eq!(classify_home(temp.path(), None), Ok(HomeState::Complete));
        let state = broken(classify_home(
            temp.path(),
            Some(ServiceRefusal::Store(
                IncompatibleStoreKind::MissingStoreKey,
            )),
        ));
        assert_eq!(state, BrokenHome::StoreKeyMissing);
        assert_eq!(state.to_string(), "the key for its data is missing");
    }

    #[test]
    fn same_second_set_asides_get_a_suffix() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".gents");
        assert_eq!(
            set_aside_target(&home, "broken", "STAMP"),
            temp.path().join(".gents.broken-STAMP")
        );
        fs::create_dir(temp.path().join(".gents.broken-STAMP")).unwrap();
        let target = set_aside_target(&home, "broken", "STAMP");
        assert!(target.to_string_lossy().ends_with("-2"), "{target:?}");
        fs::create_dir(&target).unwrap();
        assert_eq!(
            set_aside_target(&home, "broken", "STAMP"),
            temp.path().join(".gents.broken-STAMP-3")
        );
    }

    #[test]
    fn data_an_older_version_left_is_named_after_it() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".gents");
        let label = aside_label(Some(&semver::Version::new(0, 19, 0)));
        assert_eq!(
            set_aside_target(&home, &label, "STAMP"),
            temp.path().join(".gents.from-0.19.0-STAMP")
        );
        fs::create_dir(temp.path().join(".gents.from-0.19.0-STAMP")).unwrap();
        assert_eq!(
            set_aside_target(&home, &label, "STAMP"),
            temp.path().join(".gents.from-0.19.0-STAMP-2")
        );
        let rc = semver::Version::parse("1.0.0-rc1").unwrap();
        assert_eq!(aside_label(Some(&rc)), "from-1.0.0-rc1");
        assert_eq!(aside_label(None), "broken");
    }

    #[test]
    fn a_trailing_slash_sets_aside_beside_the_home() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".gents");
        fs::create_dir(&home).unwrap();
        let mut slashed = home.clone().into_os_string();
        slashed.push("/");
        assert_eq!(
            set_aside_target(Path::new(&slashed), "broken", "STAMP"),
            temp.path().join(".gents.broken-STAMP")
        );
    }

    #[tokio::test]
    async fn set_aside_moves_everything_and_keeps_nothing_behind() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".gents");
        complete(&home);
        let to = set_aside_home(&home).await.unwrap();
        assert!(!home.exists());
        assert!(to.join("init.json").exists());
        assert!(to.join("data").is_dir());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_folder_that_cannot_be_renamed_is_left_alone() {
        use std::os::unix::fs::PermissionsExt as _;
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("parent");
        let home = parent.join(".gents");
        complete(&home);
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o555)).unwrap();
        let result = set_aside_home(&home).await;
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
        match result {
            Err(Blocker::CannotSetAside { home: blocked, .. }) => assert_eq!(blocked, home),
            other => panic!("expected CannotSetAside, got {other:?}"),
        }
        assert!(home.join("init.json").exists());
        assert!(home.join("data").is_dir());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_home_moves_the_link_not_the_data() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("drive");
        complete(&target);
        let home = temp.path().join(".gents");
        std::os::unix::fs::symlink(&target, &home).unwrap();
        let to = set_aside_home(&home).await.unwrap();
        assert!(fs::symlink_metadata(&home).is_err());
        assert!(fs::symlink_metadata(&to).unwrap().file_type().is_symlink());
        assert!(target.join("init.json").exists());
        assert!(target.join("data").is_dir());
    }
}
