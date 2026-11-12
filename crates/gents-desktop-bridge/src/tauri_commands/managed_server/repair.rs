use super::native::{launchable_native_service, native_service};
use super::pairing::drain_managed_runtime_pairing;
use super::port::capitalize;
use crate::config::ManagedServerPolicy;
use crate::error::BridgeError;
use crate::state::DesktopAppState;
use crate::types::{HomeResetDisposition, ManagedServerRepairOutcome, ManagedServerRepairResult};
use gents::store::local::storage_backend::IncompatibleStoreKind;
use gents_desktop_core::client::{
    client_store_from_other_version, client_store_key_missing, set_aside_unreadable_peer_directory,
};
use gents_desktop_core::home_reset::{plan_home_reset, resolve_user_home, ClientBinding};
use gents_desktop_core::preference::MANAGED_SERVER_CONFIG;
use gents_server::repair::{
    prepare, set_aside_refused, sets_aside_refused, Blocker, Finding, HomeChoice, Prepared,
    Progress, RepairStep, Report, ServiceMove, ServiceProgram,
};
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Manager as _, Runtime};

struct Quiet;

impl Progress for Quiet {
    fn finding(&mut self, _: &Finding) {}
    fn step(&mut self, _: usize, _: usize, _: RepairStep) {}
}

pub(crate) async fn repair_managed_server<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<ManagedServerRepairResult, BridgeError>
where
    R: Runtime,
{
    let service_defined = state.policy.managed_server == ManagedServerPolicy::Allowed
        && state.policy.node_home.is_some()
        && gents_server::native_service::NativeServicePlatform::current()
            .ok()
            .zip(native_service(app, state).ok())
            .is_some_and(|(platform, service)| service.config().definition_path(platform).exists());
    let handle = app.clone();
    let program: ServiceProgram = Arc::new(move || {
        service_executable(&handle, &handle.state::<DesktopAppState>()).map_err(|error| {
            Blocker::ServiceUpdateFailed {
                reason: error.message,
            }
        })
    });
    repair_with(state, service_defined, |choice, refused| async move {
        if refused {
            set_aside_refused(&choice, &mut Quiet).await
        } else {
            prepare(&choice, &program, ServiceMove::BeforeSetup, &mut Quiet).await
        }
    })
    .await
}

/// The program a reinstalled service runs: the desktop's runtime, never the
/// app itself, checked and copied as before a start.
pub(super) fn service_executable<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<PathBuf, BridgeError>
where
    R: Runtime,
{
    Ok(launchable_native_service(app, state)?
        .config()
        .executable
        .clone())
}

/// `prepare` is also told whether a launch or status read already saw the
/// node refuse the home as older data, which is set aside rather than asked
/// about on `IncompatibleHomeScreen`.
pub(super) async fn repair_with<P, F>(
    state: &DesktopAppState,
    service_defined: bool,
    prepare: P,
) -> Result<ManagedServerRepairResult, BridgeError>
where
    P: FnOnce(HomeChoice, bool) -> F,
    F: Future<Output = anyhow::Result<Report<Prepared>>>,
{
    let paths = &state.policy.desktop_paths;
    let preference_present = paths.root().join(MANAGED_SERVER_CONFIG).exists();
    let Some(node_home) = state.policy.node_home.clone().filter(|_| {
        state.policy.managed_server == ManagedServerPolicy::Allowed
            && (service_defined || preference_present)
    }) else {
        return Ok(ManagedServerRepairResult {
            repaired: Vec::new(),
            outcome: ManagedServerRepairOutcome::NoLocalNode,
            blocker: None,
            saved_peers: Vec::new(),
            local_node_lost: false,
        });
    };
    let _lifecycle = state.managed_server_lifecycle.lock().await;
    let choice = HomeChoice {
        home: node_home,
        explicit: false,
    };
    let refused = state
        .managed_server
        .lock()
        .await
        .incompatible_store
        .as_ref()
        .is_some_and(|store| sets_aside_refused(&choice, store.kind));
    let report = prepare(choice, refused)
        .await
        .map_err(|error| BridgeError::untyped(format!("{error:#}")))?;
    if refused && matches!(report.outcome, Prepared::NeedsSetup) {
        state.managed_server.lock().await.incompatible_store = None;
    }
    // A node serving a deleted folder is part of the lost local node.
    let stale_node = report.repaired.contains(&Finding::StaleNode);
    let mut repaired: Vec<String> = report
        .repaired
        .iter()
        .filter(|finding| **finding != Finding::StaleNode)
        .map(|finding| capitalize(finding.to_string()))
        .collect();
    let (mut outcome, mut blocker) = match report.outcome {
        Prepared::Ready => (ManagedServerRepairOutcome::Ready, None),
        Prepared::NeedsSetup => (ManagedServerRepairOutcome::NeedsSetup, None),
        Prepared::Blocked(gents_server::repair::Blocker::BackgroundApprovalPending { .. }) => {
            (ManagedServerRepairOutcome::Ready, None)
        }
        Prepared::Blocked(blocker) => (
            ManagedServerRepairOutcome::Blocked,
            Some(capitalize(blocker.to_string())),
        ),
        Prepared::Decide(decision) => (
            ManagedServerRepairOutcome::Blocked,
            Some(capitalize(decision.why.to_string())),
        ),
    };

    let peers_path = paths.peer_directory_path();
    let mut local_node_lost = false;
    let late = async {
        if set_aside_unreadable_peer_directory(peers_path)
            .await?
            .is_some()
        {
            repaired.push(
                "Saved connections couldn't be read and were set aside; add your remote nodes again."
                    .to_owned(),
            );
        }
        let retired = super::reset::retire_orphaned_client_state_locked(state)
            .await
            .map_err(|error| anyhow::anyhow!(error.message))?;
        local_node_lost = retired.lost_local_node() || stale_node;
        if client_store_is_from_other_version(state).await? {
            repaired.push(
                set_aside_other_version_client_store(state)
                    .await
                    .map_err(|error| anyhow::anyhow!(error.message))?,
            );
        }
        if client_store_key_missing(paths).await? {
            archive_client_store(state, IncompatibleStoreKind::MissingStoreKey)
                .await
                .map_err(|error| anyhow::anyhow!(error.message))?;
            repaired
                .push("The desktop's local data couldn't be unlocked and was set aside.".to_owned());
        }
        gents_desktop_core::client::load_peer_records(peers_path).await
    }
    .await;
    let saved_peers = match late {
        Ok(peers) => crate::snapshot::saved_peer_views(&peers),
        Err(error) => {
            outcome = ManagedServerRepairOutcome::Blocked;
            blocker = Some(capitalize(format!("{error:#}")));
            gents_desktop_core::client::load_peer_records(peers_path)
                .await
                .map(|peers| crate::snapshot::saved_peer_views(&peers))
                .unwrap_or_default()
        }
    };
    Ok(ManagedServerRepairResult {
        repaired,
        outcome,
        blocker,
        saved_peers,
        local_node_lost,
    })
}

/// Whether the client store was written by another Gents version.
pub(in crate::tauri_commands) async fn client_store_is_from_other_version(
    state: &DesktopAppState,
) -> anyhow::Result<bool> {
    // A client start opens the store under this lock, so the probe never
    // holds the store while a start needs it.
    let _lifecycle = state.client_lifecycle.lock().await;
    client_store_from_other_version(&state.policy.desktop_paths).await
}

/// Sets aside a client store another Gents version wrote, keeping the client's
/// identity, and returns the banner line naming the backup.
pub(in crate::tauri_commands) async fn set_aside_other_version_client_store(
    state: &DesktopAppState,
) -> Result<String, BridgeError> {
    let backup = archive_client_store(state, IncompatibleStoreKind::OtherVersionSchema).await?;
    Ok(format!(
        "The desktop's local data was written by another Gents version and can't be opened, so it was moved to {}. Your identity was kept.",
        backup.display()
    ))
}

pub(super) async fn archive_client_store(
    state: &DesktopAppState,
    kind: IncompatibleStoreKind,
) -> Result<PathBuf, BridgeError> {
    use gents::store::local::storage_backend::IncompatibleStore;
    drain_managed_runtime_pairing(state).await;
    let _client = crate::tauri_commands::lifecycle::exclusion::exclude_client(state).await?;
    let paths = &state.policy.desktop_paths;
    let user_home = resolve_user_home(dirs::home_dir())?;
    let plan = plan_home_reset(
        None,
        None,
        Some(IncompatibleStore {
            kind,
            data_path: paths.node_data_dir().to_path_buf(),
        }),
        paths,
        &user_home,
        ClientBinding::Unbound,
    )?
    .keeping_managed_server_preference();
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string();
    let backup = plan.backup_path(&stamp)?;
    plan.preflight(&backup)?;
    let result = plan.retire(HomeResetDisposition::Archive, Some(&backup))?;
    state
        .bridge
        .lock()
        .expect("desktop bridge lock poisoned")
        .incompatible_client_store = None;
    tracing::info!(
        target: "gents_desktop::managed_server",
        backup = ?result.backup_path,
        retired = result.retired_paths.len(),
        ?kind,
        "archived a desktop client store this build cannot open"
    );
    Ok(backup)
}
