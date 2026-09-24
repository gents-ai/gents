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
    /// OAuth credentials issued by a completed provider sign-in whose save to
    /// the agent's canonical configuration failed. Held only in memory so the
    /// user can retry the save without repeating the browser login.
    pub pending_oauth_credentials: PendingOAuthCredentials,
}

type CredentialKey = (String, String);

/// A credential issued by a completed sign-in, ordered by issuance for its
/// (agent DID, credential provider) key.
pub struct IssuedOAuthCredential {
    sequence: u64,
    credential: gents::oauth_credential::OAuthCredential,
}

impl IssuedOAuthCredential {
    pub fn credential(&self) -> &gents::oauth_credential::OAuthCredential {
        &self.credential
    }

    fn key(&self) -> CredentialKey {
        (
            self.credential.agent_did.clone(),
            self.credential.provider.clone(),
        )
    }
}

pub enum CredentialSave<T> {
    Saved(T),
    /// A newer sign-in for the same key was already saved or held.
    Superseded,
    Failed(anyhow::Error),
}

#[derive(Default)]
struct CredentialSlot {
    write_gate: Arc<tokio::sync::Mutex<()>>,
    newest: u64,
    held: Option<IssuedOAuthCredential>,
}

/// Issued-but-unsaved OAuth credentials, keyed by agent DID and credential
/// provider. Never serialized, never written to disk, and never returned to
/// the webview; the process exit discards them. Saves and retries for one key
/// run one at a time in issuance order, so an older credential can neither
/// overwrite a newer one in the store nor discard a newer held one.
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

    /// Writes `issued` unless a newer credential for its key was already saved
    /// or held. A failed write holds it for retry; a successful one releases
    /// any older held credential.
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

    /// Writes the credential held for this key, if any, and releases it once
    /// stored. Returns `None` when nothing is held.
    pub async fn retry<T, F, Fut>(
        &self,
        agent_did: &str,
        provider: &str,
        write: F,
    ) -> Option<(gents::oauth_credential::OAuthCredential, anyhow::Result<T>)>
    where
        F: FnOnce(gents::oauth_credential::OAuthCredential) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<T>>,
    {
        let key = (agent_did.to_string(), provider.to_string());
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

    pub fn held_for(&self, agent_did: &str) -> Vec<gents::oauth_credential::OAuthCredential> {
        let mut held: Vec<_> = self
            .slots()
            .iter()
            .filter(|((agent, _), _)| agent == agent_did)
            .filter_map(|(_, slot)| slot.held.as_ref().map(|held| held.credential.clone()))
            .collect();
        held.sort_by(|left, right| left.provider.cmp(&right.provider));
        held
    }
}

#[derive(Default)]
pub struct ManagedServerState {
    pub pairing_task: Option<JoinHandle<()>>,
    pub starting: bool,
    pub last_error: Option<String>,
    /// Cancels a start that is waiting outside the lifecycle lock.
    pub start_wait: Option<crate::tauri_commands::managed_server::StartWait>,
}

pub struct DesktopBridge {
    pub core: Option<Arc<ClientCore>>,
    pub updates_task: Option<JoinHandle<()>>,
    /// Cancel handle for an in-flight ChatGPT/Codex login server, so a closed
    /// browser can be aborted instead of hanging the callback wait.
    pub codex_login_cancel: Option<gents_chatgpt_login::ShutdownHandle>,
    /// Cancel handle for an in-flight Claude subscription login server.
    pub claude_login_cancel: Option<gents_claude_login::ShutdownHandle>,
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
                claude_login_cancel: None,
                grok_login_cancel: None,
                start_inflight: None,
            }),
            client_lifecycle: tokio::sync::Mutex::new(()),
            managed_server_lifecycle: tokio::sync::Mutex::new(()),
            policy,
            managed_server: tokio::sync::Mutex::new(ManagedServerState::default()),
            pending_oauth_credentials: PendingOAuthCredentials::default(),
        }
    }
}

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
