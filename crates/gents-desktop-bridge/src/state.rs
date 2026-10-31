use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use gents_desktop_core::client::{ClientCore, DesktopPaths};
use tauri::async_runtime::{spawn, JoinHandle};
use tauri::{AppHandle, Emitter, Runtime, State};
use tokio::sync::watch;

use crate::config::{BootstrapPolicy, BridgeConfig, HomePolicy, NodeHomePolicy};
use crate::snapshot::projection::SnapshotGrants;
use crate::types::ClientUpdateEvent;

pub use crate::config::ResolvedBridgePolicy;

#[derive(Debug, Clone)]
pub enum ClientStartProgress {
    Pending,
    Ready,
    Failed(crate::error::BridgeError),
}

pub struct DesktopAppState {
    pub(crate) explorer_origin: Mutex<Option<tauri::Url>>,
    pub(crate) app_exit: Arc<crate::app_exit::AppExitState>,
    pub bridge: Mutex<DesktopBridge>,
    pub client_lifecycle: Arc<tokio::sync::Mutex<()>>,
    pub managed_server_lifecycle: tokio::sync::Mutex<()>,
    pub policy: ResolvedBridgePolicy,
    pub managed_server: tokio::sync::Mutex<ManagedServerState>,
    pub pending_oauth_credentials: Arc<PendingOAuthCredentials>,
}

pub use crate::accounts::{CredentialSave, IssuedOAuthCredential, PendingOAuthCredentials};

#[derive(Default)]
pub struct ManagedServerState {
    pub pairing_task: Option<JoinHandle<()>>,
    /// Cancels `pairing_task` before any enrollment document is authored.
    pub pairing_cancel: Option<tokio_util::sync::CancellationToken>,
    pub schema_observation_task: Option<JoinHandle<()>>,
    pub starting: bool,
    pub last_error: Option<String>,
    /// Cancels a start that is waiting outside the lifecycle lock.
    pub start_wait: Option<crate::tauri_commands::managed_server::start_wait::StartWait>,
    pub crash_loop: crate::tauri_commands::managed_server::status::CrashLoopWatch,
    pub approval_refused: bool,
    pub incompatible_store: Option<gents::storage_backend::IncompatibleStore>,
}

pub struct ProviderLoginWait {
    pub(crate) window: String,
    pub(crate) cancel: tokio_util::sync::CancellationToken,
}

pub struct DesktopBridge {
    pub core: Option<Arc<ClientCore>>,
    #[cfg(any(unix, windows))]
    pub(crate) client_host: Option<crate::host::ClientHostService>,
    /// Only native destruction retires a window selection; late IPC cannot
    /// register it again.
    selected_nodes_by_window: BTreeMap<String, Option<String>>,
    pub updates_task: Option<JoinHandle<()>>,
    pub approvals_watch_task: Option<JoinHandle<()>>,
    pub(crate) approvals_prompt_subscriptions: crate::approvals_watch::PromptSubscriptions,
    pub codex_login_cancel: Option<ProviderLoginWait>,
    pub claude_login_cancel: Option<ProviderLoginWait>,
    pub grok_login_cancel: Option<ProviderLoginWait>,
    /// Shared progress for an in-flight client start; waiters must not open a
    /// second node.
    pub start_inflight: Option<watch::Sender<ClientStartProgress>>,
    pub incompatible_client_store: Option<gents::storage_backend::IncompatibleStore>,
}

impl DesktopBridge {
    fn selected_node_did(&self) -> Option<String> {
        if self.selected_nodes_by_window.len() == 1 {
            self.selected_nodes_by_window
                .values()
                .next()
                .cloned()
                .flatten()
        } else {
            None
        }
    }

    fn apply_window_selection(&self) {
        if let Some(core) = &self.core {
            let selected = self.selected_node_did();
            core.set_selected_node_did(selected);
        }
    }

    pub(crate) fn install_core(&mut self, core: Arc<ClientCore>) {
        core.set_selected_node_did(self.selected_node_did());
        self.core = Some(core);
    }

    pub(crate) fn has_window(&self, label: &str) -> bool {
        self.selected_nodes_by_window.contains_key(label)
    }

    pub(crate) fn set_window_selected_node(&mut self, label: &str, node_did: Option<String>) {
        let Some(selected) = self.selected_nodes_by_window.get_mut(label) else {
            return;
        };
        *selected = node_did;
        self.apply_window_selection();
    }
}

impl DesktopAppState {
    pub fn new(policy: ResolvedBridgePolicy) -> Self {
        Self {
            explorer_origin: Mutex::new(None),
            bridge: Mutex::new(DesktopBridge {
                core: None,
                #[cfg(any(unix, windows))]
                client_host: None,
                selected_nodes_by_window: BTreeMap::new(),
                updates_task: None,
                approvals_watch_task: None,
                approvals_prompt_subscriptions: Default::default(),
                codex_login_cancel: None,
                claude_login_cancel: None,
                grok_login_cancel: None,
                start_inflight: None,
                incompatible_client_store: None,
            }),
            client_lifecycle: Arc::new(tokio::sync::Mutex::new(())),
            app_exit: Arc::new(crate::app_exit::AppExitState::default()),
            managed_server_lifecycle: tokio::sync::Mutex::new(()),
            policy,
            managed_server: tokio::sync::Mutex::new(ManagedServerState::default()),
            pending_oauth_credentials: Arc::default(),
        }
    }

    pub(crate) fn register_window(&self, label: &str) {
        let mut bridge = self.bridge.lock().expect("desktop bridge lock poisoned");
        bridge
            .selected_nodes_by_window
            .entry(label.to_owned())
            .or_default();
        bridge.apply_window_selection();
    }

    pub(crate) fn clear_plugin_approval_subscriptions(&self, label: &str) {
        self.bridge
            .lock()
            .expect("desktop bridge lock poisoned")
            .approvals_prompt_subscriptions
            .remove_window(label);
    }

    pub(crate) fn window_event(&self, label: &str, event: &tauri::WindowEvent) {
        if !matches!(event, tauri::WindowEvent::Destroyed) {
            return;
        }
        if label == crate::navigation::EXPLORER_WINDOW {
            *self
                .explorer_origin
                .lock()
                .expect("explorer origin lock poisoned") = None;
        }
        let mut bridge = self.bridge.lock().expect("desktop bridge lock poisoned");
        bridge.approvals_prompt_subscriptions.remove_window(label);
        for wait in [
            &bridge.codex_login_cancel,
            &bridge.claude_login_cancel,
            &bridge.grok_login_cancel,
        ]
        .into_iter()
        .flatten()
        {
            if wait.window == label {
                wait.cancel.cancel();
            }
        }
        if bridge.selected_nodes_by_window.remove(label).is_some() {
            bridge.apply_window_selection();
        }
    }
}

#[cfg(test)]
#[path = "state/window_selection_tests.rs"]
mod window_selection_tests;

pub fn spawn_client_update_task<R>(app: AppHandle<R>, core: Arc<ClientCore>) -> JoinHandle<()>
where
    R: Runtime,
{
    spawn(async move {
        let mut store_updates = core.store_change_updates();
        let mut sync_updates = core.sync_state_updates();

        loop {
            tokio::select! {
                changed = store_updates.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let notice = *store_updates.borrow_and_update();
                    let _ = app.emit("desktop://client-updated", ClientUpdateEvent::store(notice));
                }
                changed = sync_updates.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let _ = app.emit("desktop://client-updated", ClientUpdateEvent::coarse("health"));
                }
            }
        }
    })
}

pub fn current_core(state: &DesktopAppState) -> Option<Arc<ClientCore>> {
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

    let node_home = match &config.bootstrap {
        BootstrapPolicy::PairedRemoteOnly => None,
        BootstrapPolicy::LocalRuntimeAllowed { node_home } => Some(match node_home {
            NodeHomePolicy::Default => {
                gents_desktop_core::local_runtime::default_node_home().map_err(|e| e.to_string())?
            }
            NodeHomePolicy::Fixed(path) => path.clone(),
        }),
    };

    Ok(ResolvedBridgePolicy {
        desktop_paths,
        node_home,
        bootstrap: config.bootstrap.clone(),
        app_meta: config.app_meta.clone(),
        snapshot_grants: config.snapshot_grants,
        managed_server: config.managed_server,
        client_host: config.client_host,
        store_key_custody: config.store_key_custody,
    })
}

pub fn require_node_home(
    state: &State<'_, DesktopAppState>,
) -> Result<PathBuf, crate::error::BridgeError> {
    state.policy.node_home.clone().ok_or_else(|| {
        crate::error::BridgeError::new(
            crate::error::BridgeErrorCode::Unsupported,
            "local node home is not available under PairedRemoteOnly bootstrap policy",
        )
    })
}

pub fn snapshot_grants(state: &State<'_, DesktopAppState>) -> SnapshotGrants {
    state.policy.snapshot_grants
}
