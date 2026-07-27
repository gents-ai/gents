use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use gents_desktop_core::client::{ClientCore, DesktopPaths};
use tauri::async_runtime::{spawn, JoinHandle};
use tauri::{AppHandle, Emitter, Runtime, State};

use crate::config::{AgentHomePolicy, AppMeta, BootstrapPolicy, BridgeConfig, HomePolicy};
use crate::snapshot::projection::SnapshotGrants;
use crate::types::ClientUpdateEvent;

/// Resolved native policies bound at plugin init. Webview cannot override these.
#[derive(Debug, Clone)]
pub struct ResolvedBridgePolicy {
    pub desktop_paths: DesktopPaths,
    pub agent_home: Option<PathBuf>,
    pub bootstrap: BootstrapPolicy,
    pub app_meta: AppMeta,
    pub snapshot_grants: SnapshotGrants,
}

pub struct DesktopAppState {
    pub bridge: Mutex<DesktopBridge>,
    pub policy: ResolvedBridgePolicy,
}

pub struct DesktopBridge {
    pub core: Option<Arc<ClientCore>>,
    pub updates_task: Option<JoinHandle<()>>,
}

impl DesktopAppState {
    pub fn new(policy: ResolvedBridgePolicy) -> Self {
        Self {
            bridge: Mutex::new(DesktopBridge {
                core: None,
                updates_task: None,
            }),
            policy,
        }
    }
}

pub fn spawn_client_update_task<R: Runtime>(
    app: AppHandle<R>,
    core: Arc<ClientCore>,
) -> JoinHandle<()> {
    spawn(async move {
        let mut store_updates = core.store_updates();
        let mut health_updates = core.p2p_health_updates();

        loop {
            tokio::select! {
                changed = store_updates.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let _ = app.emit("desktop://client-updated", ClientUpdateEvent { reason: "store" });
                }
                changed = health_updates.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let _ = app.emit("desktop://client-updated", ClientUpdateEvent { reason: "health" });
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

/// Resolve [`HomePolicy`] / [`BootstrapPolicy`] into concrete paths once.
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
    })
}

/// Agent home from bridge state, or fail closed when local runtime is disallowed.
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
