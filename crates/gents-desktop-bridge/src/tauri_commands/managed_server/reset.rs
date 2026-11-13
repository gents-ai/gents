use super::native::service_action;
use super::native::{native_service, run_native};
use super::pairing::drain_managed_runtime_pairing;
use super::port::{matching_external_server, user_status_signer};
use super::status::emit_status;
use crate::config::ManagedServerPolicy;
use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::DesktopAppState;
use crate::tauri_commands::service_executable::resolve_service_executable;
use crate::types::{HomeResetDisposition, ManagedServerResetResult};
use gents_desktop_core::home_reset::*;
use gents_service::args::ServiceAction;
use gents_service::repair::crash_loop::CrashLoopWatch;
use gents_service::repair::port::read_initialized_did;
use std::path::Path;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Runtime};

/// Reads the facts `home_reset::local_setup_unfinished` judges: the configured
/// home's recorded identity and this desktop's saved peers.
pub(super) async fn unfinished_local_setup(
    state: &DesktopAppState,
    node_home: Option<&Path>,
) -> bool {
    let Some(node_home) = node_home else {
        return false;
    };
    // Peers that cannot be read are not evidence that none exist, so an
    // unreadable directory never offers to start over.
    let Ok(peers) = gents_desktop_core::client::load_peer_records(
        state.policy.desktop_paths.peer_directory_path(),
    )
    .await
    else {
        return false;
    };
    local_setup_unfinished(
        node_home,
        read_initialized_did(node_home).await.as_deref(),
        peers.iter().map(|peer| peer.node_did.as_str()),
    )
}

pub(super) async fn plan_state_reset(
    state: &DesktopAppState,
    full_reset: bool,
) -> Result<HomeResetPlan, BridgeError> {
    let runtime_home = state
        .policy
        .node_home
        .as_deref()
        .filter(|_| state.policy.managed_server == ManagedServerPolicy::Allowed);
    if full_reset {
        let user_home = resolve_user_home(dirs::home_dir())?;
        let home = runtime_home.ok_or_else(|| {
            BridgeError::new(
                BridgeErrorCode::Unsupported,
                "Full reset requires a locally managed runtime.",
            )
        })?;
        let config = gents_service::native_service::NativeServiceConfig::new(
            home.to_path_buf(),
            std::env::current_exe().map_err(|error| BridgeError::untyped(error.to_string()))?,
        )
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
        let platform = gents_service::native_service::NativeServicePlatform::current()
            .map_err(|error| BridgeError::untyped(error.to_string()))?;
        return plan_full_reset(
            Some(home),
            &state.policy.desktop_paths,
            &user_home,
            &config.definition_path(platform),
        );
    }
    let runtime_store = state.managed_server.lock().await.incompatible_store.clone();
    let client_store = state
        .bridge
        .lock()
        .expect("desktop bridge lock poisoned")
        .incompatible_client_store
        .clone();
    let user_home = resolve_user_home(dirs::home_dir())?;
    plan_home_reset(
        runtime_home,
        runtime_store,
        client_store,
        &state.policy.desktop_paths,
        &user_home,
        client_binding(state).await,
        unfinished_local_setup(state, runtime_home).await,
    )
}

async fn client_binding(state: &DesktopAppState) -> ClientBinding {
    gents_service::repair::client::binding_from_paths(
        &state.policy.desktop_paths,
        state.policy.node_home.as_deref(),
    )
    .await
    .unwrap_or(ClientBinding::Unbound)
}

pub(in crate::tauri_commands) async fn retire_orphaned_client_state(
    state: &DesktopAppState,
) -> Result<gents_protocol::operations::install_repair::RepairClientReceipt, BridgeError> {
    if state.policy.managed_server != ManagedServerPolicy::Allowed {
        return Ok(Default::default());
    }
    let _lifecycle = state.managed_server_lifecycle.lock().await;
    super::repair::repair_client_state(
        state,
        &mut Vec::new(),
        gents_protocol::operations::install_repair::RepairClientScope::Orphaned,
    )
    .await
}

pub(super) async fn retire_incompatible_home<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
    confirmation: &str,
    disposition: HomeResetDisposition,
    full_reset: bool,
) -> Result<ManagedServerResetResult, BridgeError>
where
    R: Runtime,
{
    let plan = plan_state_reset(state, full_reset).await?;
    plan.check_confirmation(confirmation, disposition)?;
    let retiring = match plan.runtime.as_ref() {
        Some((home, _)) => Some((home.clone(), read_initialized_did(home).await)),
        None => None,
    };
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string();
    let backup = match disposition {
        HomeResetDisposition::Archive => {
            let backup = plan.backup_path(&stamp)?;
            plan.preflight(&backup)?;
            Some(backup)
        }
        HomeResetDisposition::Delete => None,
    };

    if full_reset {
        run_native(native_service(app, state)?, |service| {
            service_action(
                &service,
                ServiceAction::Stop {
                    keep_enabled: false,
                },
            )
        })
        .await?;
        let native = run_native(native_service(app, state)?, |service| service.status()).await?;
        ensure_managed_runtime_stopped(native.is_active_or_transitioning())?;
    }
    let _store_lock = match plan.runtime.as_ref() {
        Some((home, _)) => {
            if !full_reset {
                run_native(native_service(app, state)?, |service| {
                    service_action(&service, ServiceAction::Uninstall)
                })
                .await?;
            }
            let native =
                run_native(native_service(app, state)?, |service| service.status()).await?;
            ensure_managed_runtime_stopped(native.is_active_or_transitioning())?;
            ensure_home_not_served(home, user_status_signer(state, home).await.as_ref()).await?;
            Some(lock_home_for_retirement(home)?)
        }
        None => None,
    };
    let _client = if plan.retires_client_state() {
        drain_managed_runtime_pairing(state).await;
        Some(crate::tauri_commands::lifecycle::exclusion::exclude_client(state).await?)
    } else {
        None
    };
    let settled = plan_state_reset(state, full_reset).await?;
    settled.check_confirmation(confirmation, disposition)?;

    if full_reset {
        let executable = resolve_service_executable(app, state.policy.desktop_paths.root())?;
        executable.install()?;
        let executable = executable.service_path().to_path_buf();
        let home = state.policy.node_home.clone().ok_or_else(|| {
            BridgeError::new(
                BridgeErrorCode::Unsupported,
                "No managed runtime home is configured.",
            )
        })?;
        let desktop = state.policy.desktop_paths.root().to_path_buf();
        let confirmation = confirmation.to_owned();
        tokio::task::spawn_blocking(move || {
            launch_reset_helper(&executable, &home, &desktop, &confirmation, disposition)
        })
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))??;
        app.exit(0);
        return Ok(settled.preview());
    }
    let mut result = settled.retire(disposition, backup.as_deref())?;
    if let Some((home, node_did)) = retiring.as_ref() {
        result.retired_pairings = retire_desktop_pairing(state, home, node_did.as_deref()).await;
    }
    drop(_client);
    drop(_store_lock);
    {
        let mut managed = state.managed_server.lock().await;
        managed.incompatible_store = None;
        managed.last_error = None;
        managed.crash_loop = CrashLoopWatch::default();
    }
    state
        .bridge
        .lock()
        .expect("desktop bridge lock poisoned")
        .incompatible_client_store = None;
    tracing::info!(
        target: "gents_desktop::managed_server",
        disposition = ?disposition,
        backup = ?result.backup_path,
        retired = result.retired_paths.len(),
        retained = result.retained_paths.len(),
        pairings = result.retired_pairings,
        "retired a local home and the desktop's pairing with it"
    );
    let _ = app.emit(
        crate::contract::CLIENT_UPDATED_EVENT,
        crate::types::ClientUpdateEvent::coarse("lifecycle"),
    );
    emit_status(app, state).await;
    Ok(result)
}

/// The desktop's own node store is retired in this same operation and the
/// client is still excluded here, so the pairing is dropped rather than queued.
/// A failure leaves a stale record the user can see; the retirement itself has
/// already completed and is not undone for it.
pub(super) async fn retire_desktop_pairing(
    state: &DesktopAppState,
    node_home: &Path,
    node_did: Option<&str>,
) -> u32 {
    match gents_desktop_core::client::retire_local_node_records(
        state.policy.desktop_paths.peer_directory_path(),
        node_home,
        node_did,
    )
    .await
    {
        Ok(retired) => retired.len() as u32,
        Err(error) => {
            tracing::warn!(
                target: "gents_desktop::managed_server",
                ?error,
                "the desktop's pairing with the retired node was left in place"
            );
            0
        }
    }
}

fn launch_reset_helper(
    executable: &Path,
    home: &Path,
    desktop: &Path,
    confirmation: &str,
    disposition: HomeResetDisposition,
) -> Result<(), BridgeError> {
    launch_reset_helper_with_timeout(
        executable,
        home,
        desktop,
        confirmation,
        disposition,
        Duration::from_secs(15),
    )
}

pub(super) fn launch_reset_helper_with_timeout(
    executable: &Path,
    home: &Path,
    desktop: &Path,
    confirmation: &str,
    disposition: HomeResetDisposition,
    timeout: Duration,
) -> Result<(), BridgeError> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let launch = || -> anyhow::Result<()> {
        let mut command = Command::new(executable);
        command
            .args(["runtime", "desktop", "reset", "--home"])
            .arg(home)
            .arg("--confirm")
            .arg(confirmation)
            .arg("--wait-for-desktop")
            .arg("--desktop-pid")
            .arg(std::process::id().to_string())
            .arg("--desktop-executable")
            .arg(std::env::current_exe()?)
            .env("GENTS_DESKTOP_HOME", desktop)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if disposition == HomeResetDisposition::Delete {
            command.arg("--delete");
        }
        let mut child = command.spawn()?;
        let mut stdout = child.stdout.take().expect("piped reset stdout");
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut ready = [0];
            let read = stdout.read_exact(&mut ready);
            let _ = send.send(read.is_ok() && ready[0] == b'R');
        });
        if receive.recv_timeout(timeout) != Ok(true) {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("The reset helper could not prepare; your local state has not been retired. Run gents runtime desktop reset to review and retry.");
        }
        Ok(())
    };
    launch().map_err(|error| BridgeError::new(BridgeErrorCode::Backend, format!("{error:#}")))
}

async fn ensure_home_not_served(
    home: &Path,
    user: Option<&gents_store::config_client::ClientSigningCapability>,
) -> Result<(), BridgeError> {
    let config = gents_service::server_host::ServerConfig::standard(home.to_path_buf());
    let managed_address = std::net::SocketAddr::new(config.http_addr, config.http_port);
    if matching_external_server(home, user).await?.is_some() {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "A managed server is still listening. Stop it before resetting its store.",
        ));
    }
    let endpoint_state =
        std::net::TcpStream::connect_timeout(&managed_address, Duration::from_millis(150));
    ensure_managed_endpoint_stopped(managed_address, endpoint_state.map(|_| ()))
}

pub(super) fn ensure_managed_runtime_stopped(running: bool) -> Result<(), BridgeError> {
    if running {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "Stop the managed server before resetting its local store.",
        ));
    }
    Ok(())
}

pub(super) fn ensure_managed_endpoint_stopped(
    address: std::net::SocketAddr,
    observation: std::io::Result<()>,
) -> Result<(), BridgeError> {
    match observation {
        Ok(()) => Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "A managed server is still listening. Stop it before resetting its store.",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => Ok(()),
        Err(error) => Err(BridgeError::new(
            BridgeErrorCode::Backend,
            format!("Could not prove the managed endpoint {address} is stopped: {error}"),
        )),
    }
}
