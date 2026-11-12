use gents_protocol::operations::install_repair::{Blocker, HomeState};
use std::fs;
use std::path::{Path, PathBuf};

use gents_protocol::error_chain::ErrorChain;
use gents_protocol::operations::install_repair::IncompatibleStoreKind;
use gents_store::local::home::{
    default_data_dir, home_entries, keys_dir, lock_home_store, read_init_config, HomeError,
    BOOTSTRAP_CONSUMED_FILE_NAME, INIT_PENDING_FILE_NAME,
};

use crate::home_state::init_config_path;
use crate::native_service::ServiceRefusal;

pub fn broken(home: HomeState) -> Option<HomeState> {
    super::planning::needs_archive(home).then_some(home)
}

pub fn classify_observation(
    o: &gents_protocol::operations::install_repair::HomeObservation,
) -> HomeState {
    if !o.present || (!o.owned_entries && !o.bootstrap_consumed) {
        HomeState::Absent
    } else if o.pending || (o.bootstrap_consumed && !o.settings_present) {
        HomeState::Interrupted
    } else if !o.settings_readable {
        HomeState::UnreadableSettings
    } else if !o.settings_present {
        if !o.data_present && !o.keys_present {
            HomeState::Absent
        } else {
            HomeState::MissingSettings
        }
    } else if o.missing_key_refusal {
        HomeState::MissingStoreKey
    } else {
        HomeState::Healthy
    }
}

pub fn classify_home(
    home: &Path,
    last_refusal: Option<ServiceRefusal>,
) -> Result<HomeState, Blocker> {
    use gents_protocol::operations::install_repair::HomeObservation;
    let unreadable = |error: &dyn std::error::Error| Blocker::HomeUnreadable {
        home: home.to_path_buf(),
        reason: ErrorChain(error).to_string(),
    };
    let present = home.try_exists().map_err(|e| unreadable(&e))?;
    let entries = if present {
        Some(home_entries(home, &[]).map_err(|e| unreadable(&e))?)
    } else {
        None
    };
    let bootstrap_consumed = home
        .join(BOOTSTRAP_CONSUMED_FILE_NAME)
        .try_exists()
        .map_err(|e| unreadable(&e))?;
    let pending = home
        .join(INIT_PENDING_FILE_NAME)
        .try_exists()
        .map_err(|e| unreadable(&e))?;
    let settings_present = init_config_path(home)
        .try_exists()
        .map_err(|e| unreadable(&e))?;
    let settings_readable = if !present || pending || (bootstrap_consumed && !settings_present) {
        true
    } else {
        match read_init_config::<crate::args::ToolPackageArg, crate::args::ToolCeilingArg>(home) {
            Ok(_) => true,
            Err(HomeError::DecodeInitConfig { .. }) => false,
            Err(error) => return Err(unreadable(&error)),
        }
    };
    Ok(classify_observation(&HomeObservation {
        present,
        owned_entries: entries.is_some_and(|e| !e.owned.is_empty()),
        bootstrap_consumed,
        pending,
        settings_present,
        settings_readable,
        data_present: default_data_dir(home)
            .try_exists()
            .map_err(|e| unreadable(&e))?,
        keys_present: keys_dir(home).try_exists().map_err(|e| unreadable(&e))?,
        missing_key_refusal: last_refusal
            == Some(ServiceRefusal::Store(
                IncompatibleStoreKind::MissingStoreKey,
            )),
    }))
}

pub fn unless_being_set_up(
    home: &Path,
    state: HomeState,
    node_serving: bool,
) -> Result<HomeState, Blocker> {
    if node_serving || state != HomeState::Interrupted {
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
fn beside(home: &Path) -> PathBuf {
    home.components().collect()
}
pub fn aside_label(from: Option<&semver::Version>) -> String {
    from.map_or_else(|| "broken".to_string(), |from| format!("from-{from}"))
}
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
pub async fn set_aside_home(home: &Path) -> Result<PathBuf, Blocker> {
    set_aside_home_as(home, aside_label(None)).await
}
pub async fn set_aside_home_as(home: &Path, label: String) -> Result<PathBuf, Blocker> {
    let from = beside(home);
    let stamp = chrono::Utc::now().format("%Y-%m-%d-%H%M%S").to_string();
    tokio::task::spawn_blocking(move || {
        let _store = lock_home_store(&from).map_err(|error| match error {
            HomeError::LockHeld(_) => Blocker::HomeBusy { home: from.clone() },
            error => Blocker::CannotSetAside {
                home: from.clone(),
                reason: error.to_string(),
            },
        })?;
        let to = set_aside_target(&from, &label, &stamp);
        fs::rename(&from, &to)
            .map(|()| to)
            .map_err(|error| Blocker::CannotSetAside {
                home: from,
                reason: error.to_string(),
            })
    })
    .await
    .unwrap_or_else(|error| {
        Err(Blocker::CannotSetAside {
            home: home.to_path_buf(),
            reason: error.to_string(),
        })
    })
}
