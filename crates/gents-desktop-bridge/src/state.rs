use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use gents_desktop_core::client::{ClientCore, DesktopPaths};
use tauri::async_runtime::{spawn, JoinHandle};
use tauri::{AppHandle, Emitter, Runtime, State};
use tokio::sync::watch;

use crate::config::{
    AgentHomePolicy, AppMeta, BootstrapPolicy, BridgeConfig, HomePolicy, ManagedServerPolicy,
};
use crate::snapshot::projection::SnapshotGrants;
use crate::types::ClientUpdateEvent;

#[derive(Debug, Clone)]
pub struct ResolvedBridgePolicy {
    pub desktop_paths: DesktopPaths,
    pub agent_home: Option<PathBuf>,
    pub bootstrap: BootstrapPolicy,
    pub app_meta: AppMeta,
    pub snapshot_grants: SnapshotGrants,
    pub managed_server: ManagedServerPolicy,
}

/// Outcome of a single-flight `desktop_client_start`.
///
/// Concurrent start callers share one in-flight open of the embedded node so a
/// second `NodeBuilder` cannot race the first for the persistent store.
#[derive(Debug, Clone)]
pub enum ClientStartProgress {
    Pending,
    Ready,
    Failed(String),
}

pub struct DesktopAppState {
    pub bridge: Mutex<DesktopBridge>,
    /// Serializes start *install* / shutdown mutations against bridge state.
    /// Long-running node open does **not** hold this lock (see single-flight
    /// `start_inflight` instead) so a cancelled Tauri command cannot drop the
    /// lock while the store is still opening on a background thread.
    pub client_lifecycle: tokio::sync::Mutex<()>,
    /// Serializes managed server start/stop operations. Startup intentionally
    /// spans provisioning and server readiness, so the state flag alone is
    /// not sufficient to prevent two callers from racing the port bind.
    pub managed_server_lifecycle: tokio::sync::Mutex<()>,
    pub policy: ResolvedBridgePolicy,
    pub managed_server: tokio::sync::Mutex<ManagedServerState>,
}

#[derive(Default)]
pub struct ManagedServerState {
    pub server: Option<gents_server::server_host::RunningServer>,
    pub starting: bool,
    pub last_error: Option<String>,
}

pub struct DesktopBridge {
    pub core: Option<Arc<ClientCore>>,
    pub updates_task: Option<JoinHandle<()>>,
    /// Cancel handle for an in-flight ChatGPT/Codex login server, so a closed
    /// browser can be aborted instead of hanging the callback wait.
    pub codex_login_cancel: Option<gents_chatgpt_login::ShutdownHandle>,
    /// Cancel flag for an in-flight Grok device-code login poll.
    pub grok_login_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Shared progress for an in-flight client start. The sender is owned by
    /// the detached starter task; waiters hold receivers and do not open a
    /// second node.
    pub start_inflight: Option<watch::Sender<ClientStartProgress>>,
}

impl DesktopAppState {
    pub fn new(policy: ResolvedBridgePolicy) -> Self {
        Self {
            bridge: Mutex::new(DesktopBridge {
                core: None,
                updates_task: None,
                codex_login_cancel: None,
                grok_login_cancel: None,
                start_inflight: None,
            }),
            client_lifecycle: tokio::sync::Mutex::new(()),
            managed_server_lifecycle: tokio::sync::Mutex::new(()),
            policy,
            managed_server: tokio::sync::Mutex::new(ManagedServerState::default()),
        }
    }
}

pub fn spawn_client_update_task<R: Runtime>(
    app: AppHandle<R>,
    core: Arc<ClientCore>,
) -> JoinHandle<()> {
    spawn(async move {
        let mut store_updates = core.store_change_updates();
        let mut health_updates = core.p2p_health_updates();

        loop {
            tokio::select! {
                changed = store_updates.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let notice = *store_updates.borrow_and_update();
                    let _ = app.emit("desktop://client-updated", ClientUpdateEvent::store(notice));
                }
                changed = health_updates.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let _ = app.emit("desktop://client-updated", ClientUpdateEvent::coarse("health"));
                }
            }
        }
    })
}

pub fn current_core(state: &State<'_, DesktopAppState>) -> Option<Arc<ClientCore>> {
    state
        .bridge
        .lock()
        .expect("desktop bridge lock poisoned")
        .core
        .clone()
}

pub fn resolve_policy(
    config: &BridgeConfig,
    app_data_dir: Option<PathBuf>,
) -> Result<ResolvedBridgePolicy, String> {
    let desktop_paths = match &config.home {
        HomePolicy::Default => DesktopPaths::discover().map_err(|e| e.to_string())?,
        HomePolicy::AppDataDir { subdirectory } => {
            let base = app_data_dir.ok_or_else(|| {
                "HomePolicy::AppDataDir requires the host app data directory".to_string()
            })?;
            DesktopPaths::from_root(base.join(subdirectory))
        }
        HomePolicy::FixedRoot(root) => DesktopPaths::from_root(root.clone()),
    };

    let agent_home = match &config.bootstrap {
        BootstrapPolicy::PairedRemoteOnly => None,
        BootstrapPolicy::LocalRuntimeAllowed { agent_home } => Some(match agent_home {
            AgentHomePolicy::Default => gents_desktop_core::local_runtime::default_agent_home()
                .map_err(|e| e.to_string())?,
            AgentHomePolicy::Fixed(path) => path.clone(),
        }),
    };

    Ok(ResolvedBridgePolicy {
        desktop_paths,
        agent_home,
        bootstrap: config.bootstrap.clone(),
        app_meta: config.app_meta.clone(),
        snapshot_grants: config.snapshot_grants,
        managed_server: config.managed_server,
    })
}

pub fn require_agent_home(
    state: &State<'_, DesktopAppState>,
) -> Result<PathBuf, crate::error::BridgeError> {
    state.policy.agent_home.clone().ok_or_else(|| {
        crate::error::BridgeError::new(
            crate::error::BridgeErrorCode::Unsupported,
            "local agent home is not available under PairedRemoteOnly bootstrap policy",
        )
    })
}

pub fn snapshot_grants(state: &State<'_, DesktopAppState>) -> SnapshotGrants {
    state.policy.snapshot_grants
}
