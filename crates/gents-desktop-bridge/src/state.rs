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
    pub(crate) app_exit: Arc<crate::app_exit::AppExitState>,
    pub bridge: Mutex<DesktopBridge>,
    pub client_lifecycle: Arc<tokio::sync::Mutex<()>>,
    pub managed_server_lifecycle: tokio::sync::Mutex<()>,
    pub policy: ResolvedBridgePolicy,
    pub managed_server: tokio::sync::Mutex<ManagedServerState>,
    pub pending_oauth_credentials: PendingOAuthCredentials,
}

type CredentialKey = (String, String, String);

pub struct IssuedOAuthCredential {
    sequence: u64,
    credential: gents::oauth_credential::OAuthCredential,
}

impl IssuedOAuthCredential {
    pub fn credential(&self) -> &gents::oauth_credential::OAuthCredential {
        &self.credential
    }

    fn key(&self) -> CredentialKey {
        let identity = [
            &self.credential.provider_account_key,
            &self.credential.account_id,
        ]
        .into_iter()
        .flatten()
        .map(|value| value.trim())
        .find(|value| !value.is_empty())
        .unwrap_or_default();
        (
            self.credential.node_did.clone(),
            self.credential.provider.clone(),
            identity.to_owned(),
        )
    }
}

pub enum CredentialSave<T> {
    Saved(T),
    Superseded,
    Failed(anyhow::Error),
}

#[derive(Default)]
struct CredentialSlot {
    write_gate: Arc<tokio::sync::Mutex<()>>,
    newest: u64,
    held: Option<IssuedOAuthCredential>,
}

/// Issued-but-unsaved OAuth credentials, keyed by node DID and credential
/// provider. Never serialized, written to disk, or returned to the webview.
/// Saves and retries for one key run one at a time in issuance order, so an
/// older credential can neither overwrite a newer one nor discard a newer
/// held one.
#[derive(Default)]
pub struct PendingOAuthCredentials {
    issued: std::sync::atomic::AtomicU64,
    slots: Mutex<std::collections::HashMap<CredentialKey, CredentialSlot>>,
}

impl PendingOAuthCredentials {
    pub fn issue(
        &self,
        credential: gents::oauth_credential::OAuthCredential,
    ) -> IssuedOAuthCredential {
        IssuedOAuthCredential {
            sequence: self
                .issued
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1,
            credential,
        }
    }

    fn slots(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::HashMap<CredentialKey, CredentialSlot>> {
        self.slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write_gate(&self, key: &CredentialKey) -> Arc<tokio::sync::Mutex<()>> {
        self.slots()
            .entry(key.clone())
            .or_default()
            .write_gate
            .clone()
    }

    pub async fn save<T, F, Fut>(
        &self,
        issued: IssuedOAuthCredential,
        write: F,
    ) -> CredentialSave<T>
    where
        F: FnOnce(gents::oauth_credential::OAuthCredential) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<T>>,
    {
        let key = issued.key();
        let gate = self.write_gate(&key);
        let _ordered = gate.lock().await;
        {
            let mut slots = self.slots();
            let slot = slots.entry(key.clone()).or_default();
            if issued.sequence < slot.newest {
                return CredentialSave::Superseded;
            }
            slot.newest = issued.sequence;
        }
        let written = write(issued.credential.clone()).await;
        let mut slots = self.slots();
        let slot = slots.entry(key).or_default();
        match written {
            Ok(value) => {
                slot.held = None;
                CredentialSave::Saved(value)
            }
            Err(error) => {
                slot.held = Some(issued);
                CredentialSave::Failed(error)
            }
        }
    }

    pub async fn retry<T, F, Fut>(
        &self,
        node_did: &str,
        provider: &str,
        write: F,
    ) -> Option<(gents::oauth_credential::OAuthCredential, anyhow::Result<T>)>
    where
        F: FnOnce(gents::oauth_credential::OAuthCredential) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<T>>,
    {
        let key = self
            .slots()
            .iter()
            .filter(|((node, held_provider, _), slot)| {
                node == node_did && held_provider == provider && slot.held.is_some()
            })
            .max_by_key(|(_, slot)| slot.held.as_ref().map(|held| held.sequence))
            .map(|(key, _)| key.clone())?;
        let gate = self.write_gate(&key);
        let _ordered = gate.lock().await;
        let credential = self
            .slots()
            .get(&key)?
            .held
            .as_ref()
            .map(|held| held.credential.clone())?;
        let written = write(credential.clone()).await;
        if written.is_ok() {
            if let Some(slot) = self.slots().get_mut(&key) {
                slot.held = None;
            }
        }
        Some((credential, written))
    }

    pub fn held_for(&self, node_did: &str) -> Vec<gents::oauth_credential::OAuthCredential> {
        let mut held: Vec<_> = self
            .slots()
            .iter()
            .filter(|((node, _, _), _)| node == node_did)
            .filter_map(|(_, slot)| slot.held.as_ref().map(|held| held.credential.clone()))
            .collect();
        held.sort_by(|left, right| left.provider.cmp(&right.provider));
        held
    }
}

#[derive(Default)]
pub struct ManagedServerState {
    pub pairing_task: Option<JoinHandle<()>>,
    /// Cancels `pairing_task` before any enrollment document is authored.
    pub pairing_cancel: Option<tokio_util::sync::CancellationToken>,
    pub schema_observation_task: Option<JoinHandle<()>>,
    pub starting: bool,
    pub last_error: Option<String>,
    /// Cancels a start that is waiting outside the lifecycle lock.
    pub start_wait: Option<crate::tauri_commands::managed_server::StartWait>,
    pub crash_loop: crate::tauri_commands::managed_server::CrashLoopWatch,
    pub approval_refused: bool,
    pub incompatible_store: Option<gents::storage_backend::IncompatibleStore>,
}

pub struct DesktopBridge {
    pub core: Option<Arc<ClientCore>>,
    #[cfg(unix)]
    pub(crate) client_host: Option<crate::host::ClientHostService>,
    /// Only native destruction retires a window selection; late IPC cannot
    /// register it again.
    selected_nodes_by_window: BTreeMap<String, Option<String>>,
    pub updates_task: Option<JoinHandle<()>>,
    pub approvals_watch_task: Option<JoinHandle<()>>,
    pub(crate) approvals_prompt_subscriptions: crate::approvals_watch::PromptSubscriptions,
    pub codex_login_cancel: Option<gents_chatgpt_login::ShutdownHandle>,
    pub claude_login_cancel: Option<gents_claude_login::ShutdownHandle>,
    pub grok_login_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
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
            bridge: Mutex::new(DesktopBridge {
                core: None,
                #[cfg(unix)]
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
            pending_oauth_credentials: PendingOAuthCredentials::default(),
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
        let mut bridge = self.bridge.lock().expect("desktop bridge lock poisoned");
        bridge.approvals_prompt_subscriptions.remove_window(label);
        if bridge.selected_nodes_by_window.remove(label).is_some() {
            bridge.apply_window_selection();
        }
    }
}

#[cfg(test)]
#[path = "state/window_selection_tests.rs"]
mod window_selection_tests;

pub fn spawn_client_update_task<R: Runtime>(
    app: AppHandle<R>,
    core: Arc<ClientCore>,
) -> JoinHandle<()> {
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
