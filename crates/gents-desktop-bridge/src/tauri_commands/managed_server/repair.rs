use super::native::native_service;
use super::pairing::drain_managed_runtime_pairing;
use super::port::capitalize;
use crate::config::ManagedServerPolicy;
use crate::error::BridgeError;
use crate::state::DesktopAppState;
use crate::types::{ManagedServerRepairOutcome, ManagedServerRepairResult};
use gents_desktop_core::client::client_store_from_other_version;
use gents_desktop_core::home_reset::resolve_user_home;
use gents_desktop_core::preference::MANAGED_SERVER_CONFIG;
use gents_protocol::operations::install_repair::IncompatibleStoreKind;
use gents_protocol::operations::install_repair::RepairInput;
use gents_protocol::operations::install_repair::{
    Blocker, Finding, HomeChoice, RepairClientScope, RepairOutcome, RepairReceipt, RepairStep,
    ServiceMove,
};
use gents_service::repair::{execute, sets_aside_refused, Progress, ServiceProgram};
use std::future::Future;
use std::path::PathBuf;
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
        && gents_service::native_service::NativeServicePlatform::current()
            .ok()
            .zip(native_service(app, state).ok())
            .is_some_and(|(platform, service)| service.config().definition_path(platform).exists());
    let handle = app.clone();
    let program = ServiceProgram::new(move || {
        service_executable(&handle, &handle.state::<DesktopAppState>()).map_err(|error| {
            Blocker::ServiceUpdateFailed {
                reason: error.message,
            }
        })
    });
    repair_with(state, service_defined, |choice, refused| async move {
        let input = if refused {
            RepairInput::Refused { choice }
        } else {
            RepairInput::Prepare {
                choice,
                service_move: ServiceMove::BeforeSetup,
            }
        };
        execute(input, &program, &mut Quiet, None)
            .await
            .map_err(anyhow::Error::from)
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
    use crate::tauri_commands::service_executable::{
        ensure_installable_location, resolve_service_executable, ServiceExecutable,
    };
    let resolved = resolve_service_executable(app, state.policy.desktop_paths.root())?;
    match resolved {
        ServiceExecutable::InPlace(path) => {
            ensure_installable_location(&path)?;
            Ok(path)
        }
        ServiceExecutable::Packaged { source, .. } => {
            gents_service::service_update::stage_repair_executable(
                &source,
                state.policy.desktop_paths.root(),
            )
            .map_err(|error| BridgeError::untyped(error.to_string()))
        }
    }
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
    F: Future<Output = anyhow::Result<RepairReceipt>>,
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
    if refused && matches!(report.outcome, RepairOutcome::NeedsSetup) {
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
        RepairOutcome::Ready | RepairOutcome::Answering(_) => {
            (ManagedServerRepairOutcome::Ready, None)
        }
        RepairOutcome::NeedsSetup => (ManagedServerRepairOutcome::NeedsSetup, None),
        RepairOutcome::Blocked(
            gents_protocol::operations::install_repair::Blocker::BackgroundApprovalPending {
                ..
            },
        ) => (ManagedServerRepairOutcome::Ready, None),
        RepairOutcome::Blocked(blocker) => (
            ManagedServerRepairOutcome::Blocked,
            Some(capitalize(blocker.to_string())),
        ),
        RepairOutcome::Decide(decision) => (
            ManagedServerRepairOutcome::Blocked,
            Some(capitalize(decision.why.to_string())),
        ),
    };

    let peers_path = paths.peer_directory_path();
    let mut local_node_lost = stale_node;
    let late = async {
        let client = repair_client_state(state, &mut repaired, RepairClientScope::All).await?;
        local_node_lost |= client.local_node_lost;
        gents_desktop_core::client::load_peer_records(peers_path)
            .await
            .map_err(|error| BridgeError::untyped(error.to_string()))
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
    let client =
        repair_client_state(state, &mut Vec::new(), RepairClientScope::Store { kind }).await?;
    client
        .findings
        .into_iter()
        .find_map(|finding| match finding {
            Finding::ClientStoreSetAside { kept_at } => Some(kept_at),
            _ => None,
        })
        .ok_or_else(|| {
            BridgeError::untyped(format!(
                "the client store no longer requires repair for {kind:?}"
            ))
        })
}

pub(super) struct ClientLifecycle<'a>(pub(super) &'a DesktopAppState);

#[async_trait::async_trait]
impl gents_service::repair::ClientExclusion for ClientLifecycle<'_> {
    async fn exclude(
        &self,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>, gents_service::repair::RepairFailure> {
        drain_managed_runtime_pairing(self.0).await;
        crate::tauri_commands::lifecycle::exclusion::exclude_client(self.0)
            .await
            .map_err(|error| anyhow::anyhow!(error.message).into())
    }
}

pub(super) async fn repair_client_state(
    state: &DesktopAppState,
    notes: &mut Vec<String>,
    scope: RepairClientScope,
) -> Result<gents_protocol::operations::install_repair::RepairClientReceipt, BridgeError> {
    let input = gents_protocol::operations::install_repair::RepairClientInput {
        scope,
        root: state.policy.desktop_paths.root().to_path_buf(),
        user_home: resolve_user_home(dirs::home_dir())?,
        node_home: state
            .policy
            .node_home
            .clone()
            .filter(|_| state.policy.managed_server == ManagedServerPolicy::Allowed),
    };
    let receipt = execute(
        RepairInput::Client { input },
        &ServiceProgram::new(gents_service::repair::current_program),
        &mut Quiet,
        Some(&ClientLifecycle(state)),
    )
    .await
    .map_err(|error| BridgeError::untyped(error.to_string()))?;
    let client = receipt
        .client
        .ok_or_else(|| BridgeError::untyped("client repair returned no receipt"))?;
    notes.extend(
        client
            .findings
            .iter()
            .map(|finding| capitalize(finding.to_string())),
    );
    if let RepairOutcome::Blocked(blocker) = receipt.outcome {
        return Err(BridgeError::untyped(blocker.to_string()));
    }
    Ok(client)
}
