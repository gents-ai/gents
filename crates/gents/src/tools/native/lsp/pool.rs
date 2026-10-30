use rapidhash::{HashMapExt, RapidHashMap};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, Notify};

use crate::drop_cleanup::DropCleanup;
use crate::tools::exec::{spawn_managed_process, ManagedExecKind, SpawnManagedProcessRequest};
use crate::tools::native::prepare_managed_command;

use super::catalog::CatalogServer;
use super::client::LspClient;
use super::LspToolConfig;

const MAX_PER_SESSION: usize = 4;
const MAX_GLOBAL: usize = 16;
const SWEEP_INTERVAL: Duration = Duration::from_secs(60);
pub(crate) const INIT_BACKOFF: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PoolKey {
    pub session_id: String,
    pub agent_id: String,
    pub workspace_root: std::path::PathBuf,
    pub server_name: String,
    pub config_digest: String,
    /// Bounded servers must retain the runtime credential namespace under which
    /// they were launched, including when workspace consent is unconfigured.
    pub runtime_home: Option<std::path::PathBuf>,
    pub operator_guard_home: Option<std::path::PathBuf>,
}

impl PoolKey {
    fn validate_operator_guard(&self) -> Result<(), String> {
        let runtime_home = crate::tools::call::lifecycle::runtime::current_tool_runtime_context()
            .and_then(|context| context.runtime_home);
        if runtime_home != self.runtime_home {
            return Err("language-server runtime protection changed; retry the action".into());
        }
        let current = crate::plugin::approval::workspace::command_guard_home()
            .map_err(|error| format!("{error:#}"))?;
        if current != self.operator_guard_home {
            return Err("language-server operator protection changed; retry the action".into());
        }
        if let Some(home) = &self.operator_guard_home {
            crate::plugin::allowed::CommandProtection::capture(
                home,
                crate::tools::native::workspace_write_sandbox_enforced(),
            )
            .map_err(|error| format!("{error:#}"))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryState {
    Starting,
    Ready,
    Retiring,
}

enum ClientStartError {
    Admission(String),
    Initialization(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PoolServerState {
    Starting,
    Ready,
    Retiring,
    Failed(String),
}

struct PoolEntry {
    state: EntryState,
    leases: Arc<AtomicUsize>,
    last_used: Instant,
    idle_timeout: Duration,
    client: Option<Arc<LspClient>>,
    credential_snapshot: Option<Vec<std::path::PathBuf>>,
    ready: Arc<Notify>,
    start_error: Option<String>,
}

pub(crate) struct LspLease {
    client: Arc<LspClient>,
    leases: Arc<AtomicUsize>,
    slot: Arc<Mutex<PoolEntry>>,
    runtime: DropCleanup,
}

impl LspLease {
    pub fn client(&self) -> &LspClient {
        &self.client
    }
}

impl Drop for LspLease {
    fn drop(&mut self) {
        let prev = self.leases.fetch_sub(1, Ordering::SeqCst);
        if prev == 1 {
            let slot = self.slot.clone();
            self.runtime.spawn(complete_retirement_if_idle(slot));
        }
    }
}

struct StartingCleanup {
    pool: LspPool,
    key: PoolKey,
    slot: Arc<Mutex<PoolEntry>>,
    disarm: Arc<AtomicBool>,
    runtime: DropCleanup,
}

impl Drop for StartingCleanup {
    fn drop(&mut self) {
        if self.disarm.load(Ordering::SeqCst) {
            return;
        }
        let pool = self.pool.clone();
        let key = self.key.clone();
        let slot = self.slot.clone();
        self.runtime.spawn(async move {
            {
                let mut entry = slot.lock().await;
                if entry.state == EntryState::Starting {
                    entry.start_error = Some("language-server start cancelled".into());
                    entry.state = EntryState::Retiring;
                    entry.ready.notify_waiters();
                }
            }
            pool.inner.lock().await.remove(&key);
        });
    }
}

async fn retire_slot(slot: Arc<Mutex<PoolEntry>>) {
    let client = {
        let mut entry = slot.lock().await;
        entry.state = EntryState::Retiring;
        entry.ready.notify_waiters();
        if entry.leases.load(Ordering::SeqCst) == 0 {
            entry.client.take()
        } else {
            None
        }
    };
    if let Some(client) = client {
        client.shutdown_exit().await;
    }
}

async fn lru_idle_ready(
    map: &RapidHashMap<PoolKey, Arc<Mutex<PoolEntry>>>,
    pred: impl Fn(&PoolKey) -> bool,
) -> Option<PoolKey> {
    let mut victim: Option<(PoolKey, Instant)> = None;
    for (key, slot) in map.iter() {
        if !pred(key) {
            continue;
        }
        let entry = slot.lock().await;
        if entry.state == EntryState::Ready
            && entry.leases.load(Ordering::SeqCst) == 0
            && victim
                .as_ref()
                .is_none_or(|(_, used)| entry.last_used < *used)
        {
            victim = Some((key.clone(), entry.last_used));
        }
    }
    victim.map(|(key, _)| key)
}

async fn complete_retirement_if_idle(slot: Arc<Mutex<PoolEntry>>) {
    let client = {
        let mut entry = slot.lock().await;
        if entry.state == EntryState::Retiring && entry.leases.load(Ordering::SeqCst) == 0 {
            entry.client.take()
        } else {
            None
        }
    };
    if let Some(client) = client {
        client.shutdown_exit().await;
    }
}

#[derive(Clone)]
pub struct LspPool {
    inner: Arc<Mutex<RapidHashMap<PoolKey, Arc<Mutex<PoolEntry>>>>>,
    failed: Arc<Mutex<RapidHashMap<PoolKey, (String, Instant)>>>,
    sweep_started: Arc<AtomicBool>,
}

impl Default for LspPool {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(RapidHashMap::new())),
            failed: Arc::new(Mutex::new(RapidHashMap::new())),
            sweep_started: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl LspPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Must be called from within the runtime the pool runs on.
    fn spawn_sweeper_once(&self) {
        if self.sweep_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let pool = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(SWEEP_INTERVAL);
            interval.tick().await;
            loop {
                interval.tick().await;
                pool.sweep_idle().await;
            }
        });
    }

    pub(crate) async fn sweep_idle(&self) {
        let now = Instant::now();
        let victims: Vec<PoolKey> = {
            let map = self.inner.lock().await;
            let mut keys = Vec::new();
            for (key, slot) in map.iter() {
                let entry = slot.lock().await;
                if entry.state == EntryState::Ready
                    && entry.leases.load(Ordering::SeqCst) == 0
                    && now.duration_since(entry.last_used) >= entry.idle_timeout
                {
                    keys.push(key.clone());
                }
            }
            keys
        };
        for key in victims {
            self.retire(&key).await;
        }
    }

    pub(crate) async fn get_ready(&self, key: &PoolKey) -> Option<LspLease> {
        if super::artifact_scope_denied() {
            return None;
        }
        let map = self.inner.lock().await;
        let slot = map.get(key)?.clone();
        drop(map);
        let captured_credentials = slot.lock().await.credential_snapshot.clone();
        let current_credentials = if captured_credentials.is_some() {
            match crate::tools::native::current_managed_command_credential_snapshot(true).await {
                Ok(snapshot) => snapshot,
                Err(_) => {
                    self.retire(key).await;
                    return None;
                }
            }
        } else {
            None
        };
        let mut entry = slot.lock().await;
        if entry.state != EntryState::Ready {
            return None;
        }
        let client = entry.client.clone()?;
        if !client.is_alive() {
            drop(entry);
            self.retire(key).await;
            return None;
        }
        key.validate_operator_guard().ok()?;
        if entry.credential_snapshot != current_credentials {
            drop(entry);
            self.retire(key).await;
            return None;
        }
        entry.leases.fetch_add(1, Ordering::SeqCst);
        entry.last_used = Instant::now();
        Some(LspLease {
            client,
            leases: entry.leases.clone(),
            slot: slot.clone(),
            runtime: DropCleanup::current(),
        })
    }

    pub(crate) async fn get_or_start(
        &self,
        key: PoolKey,
        server: &CatalogServer,
        config: &LspToolConfig,
    ) -> Result<LspLease, String> {
        if super::artifact_scope_denied() {
            return Err(super::ARTIFACT_SCOPE_DENIAL.into());
        }
        key.validate_operator_guard()?;
        let constraints = super::overlay_lsp_constraints(&config.constraints);
        let protects_credentials = constraints.to_spawn_policy().mode.protects_credentials();
        let current_credentials =
            crate::tools::native::current_managed_command_credential_snapshot(protects_credentials)
                .await?;
        self.spawn_sweeper_once();
        {
            let mut failed = self.failed.lock().await;
            if let Some((error, at)) = failed.get(&key) {
                if at.elapsed() < INIT_BACKOFF {
                    return Err(error.clone());
                }
                failed.remove(&key);
            }
        }

        let (slot, starter, evicted) = {
            let mut map = self.inner.lock().await;
            let mut evicted = Vec::new();
            let mut reusable = None;
            if let Some(existing) = map.get(&key).cloned() {
                let dead = {
                    let entry = existing.lock().await;
                    entry.state == EntryState::Ready
                        && (entry
                            .client
                            .as_ref()
                            .is_some_and(|client| !client.is_alive())
                            || entry.credential_snapshot != current_credentials)
                };
                if dead {
                    map.remove(&key);
                    evicted.push(existing);
                } else {
                    reusable = Some(existing);
                }
            }
            if let Some(existing) = reusable {
                (existing, false, evicted)
            } else {
                let Some(cap_victims) = self.take_eviction_victims(&mut map, &key).await else {
                    return Err("language-server client cap reached".into());
                };
                evicted.extend(cap_victims);
                let slot = Arc::new(Mutex::new(PoolEntry {
                    state: EntryState::Starting,
                    leases: Arc::new(AtomicUsize::new(0)),
                    last_used: Instant::now(),
                    idle_timeout: config.idle_timeout,
                    client: None,
                    credential_snapshot: None,
                    ready: Arc::new(Notify::new()),
                    start_error: None,
                }));
                map.insert(key.clone(), slot.clone());
                (slot, true, evicted)
            }
        };
        for victim in evicted {
            retire_slot(victim).await;
        }

        if !starter {
            loop {
                let mut entry = slot.lock().await;
                if entry.state == EntryState::Ready {
                    if let Some(client) = entry.client.clone() {
                        key.validate_operator_guard()?;
                        if entry.credential_snapshot != current_credentials {
                            drop(entry);
                            self.retire(&key).await;
                            return Err(
                                "language-server credential exclusions changed; retry the action"
                                    .into(),
                            );
                        }
                        entry.leases.fetch_add(1, Ordering::SeqCst);
                        entry.last_used = Instant::now();
                        return Ok(LspLease {
                            client,
                            leases: entry.leases.clone(),
                            slot: slot.clone(),
                            runtime: DropCleanup::current(),
                        });
                    }
                }
                if let Some(error) = &entry.start_error {
                    return Err(error.clone());
                }
                if entry.state == EntryState::Retiring {
                    return Err("language-server client is retiring".into());
                }
                let ready = entry.ready.clone();
                let notified = ready.notified();
                tokio::pin!(notified);
                // `notify_waiters` does not retain a permit; register the
                // waiter before releasing the state lock so the starter
                // cannot publish Ready in the gap.
                notified.as_mut().enable();
                drop(entry);
                notified.await;
            }
        }

        let cleanup = StartingCleanup {
            pool: self.clone(),
            key: key.clone(),
            slot: slot.clone(),
            runtime: DropCleanup::current(),
            disarm: Arc::new(AtomicBool::new(false)),
        };
        let started = self.start_client(&key, server, config).await;
        let credentials_current = match &started {
            Ok((_, credential_snapshot)) => {
                crate::tools::native::current_managed_command_credential_snapshot(
                    credential_snapshot.is_some(),
                )
                .await
                .is_ok_and(|current| current == *credential_snapshot)
            }
            Err(_) => false,
        };
        let mut entry = slot.lock().await;
        let started = match started {
            Ok((client, credential_snapshot)) => {
                match (key.validate_operator_guard(), credentials_current) {
                    (Ok(()), true) => Ok((client, credential_snapshot)),
                    (guard_result, false) => {
                        let error = guard_result.err().unwrap_or_else(|| {
                            "language-server credential exclusions changed; retry the action".into()
                        });
                        drop(entry);
                        client.shutdown_exit().await;
                        entry = slot.lock().await;
                        Err(ClientStartError::Admission(error))
                    }
                    (Err(error), true) => {
                        drop(entry);
                        client.shutdown_exit().await;
                        entry = slot.lock().await;
                        Err(ClientStartError::Admission(error))
                    }
                }
            }
            Err(error) => Err(error),
        };
        match started {
            Ok((client, credential_snapshot)) => {
                let client = Arc::new(client);
                let leases = {
                    entry.client = Some(client.clone());
                    entry.credential_snapshot = credential_snapshot;
                    entry.state = EntryState::Ready;
                    entry.last_used = Instant::now();
                    entry.idle_timeout = config.idle_timeout;
                    entry.leases.fetch_add(1, Ordering::SeqCst);
                    entry.ready.notify_waiters();
                    entry.leases.clone()
                };
                drop(entry);
                cleanup.disarm.store(true, Ordering::SeqCst);
                Ok(LspLease {
                    client,
                    leases,
                    slot,
                    runtime: DropCleanup::current(),
                })
            }
            Err(error) => {
                let cache_failure = matches!(error, ClientStartError::Initialization(_));
                let error = match error {
                    ClientStartError::Admission(error)
                    | ClientStartError::Initialization(error) => error,
                };
                entry.start_error = Some(error.clone());
                entry.state = EntryState::Retiring;
                entry.ready.notify_waiters();
                drop(entry);
                self.inner.lock().await.remove(&key);
                cleanup.disarm.store(true, Ordering::SeqCst);
                if cache_failure {
                    self.failed
                        .lock()
                        .await
                        .insert(key, (error.clone(), Instant::now()));
                }
                Err(error)
            }
        }
    }

    async fn start_client(
        &self,
        key: &PoolKey,
        server: &CatalogServer,
        config: &LspToolConfig,
    ) -> Result<(LspClient, Option<Vec<std::path::PathBuf>>), ClientStartError> {
        let workspace = key.workspace_root.clone();
        let constraints = super::overlay_lsp_constraints(&config.constraints);
        let command =
            prepare_managed_command(&workspace, &server.command, &server.args, &constraints)
                .await
                .map_err(|err| ClientStartError::Admission(err.to_string()))?;
        if command.operator_guard_home != key.operator_guard_home {
            return Err(ClientStartError::Admission(
                "language-server operator protection changed; retry the action".into(),
            ));
        }
        tracing::debug!(server = %server.name, sandbox = command.sandbox, "starting language server");
        let mut full_argv = vec![command.program.to_string_lossy().into_owned()];
        full_argv.extend(command.args);
        let process = spawn_managed_process(SpawnManagedProcessRequest {
            argv: full_argv,
            cwd: workspace.clone(),
            environment: Some(command.environment),
            tool_name: Some("lsp".into()),
            kind: ManagedExecKind::PersistentService,
        })
        .await
        .map_err(ClientStartError::Initialization)?;
        let client = LspClient::start(process, server.name.clone(), server, workspace)
            .map_err(ClientStartError::Initialization)?;
        if let Err(error) = client.initialize().await {
            client.shutdown_exit().await;
            return Err(ClientStartError::Initialization(error));
        }
        client
            .wait_until_ready(server.workspace_ready_timings.as_ref())
            .await;
        Ok((client, command.credential_snapshot))
    }

    async fn take_eviction_victims(
        &self,
        map: &mut RapidHashMap<PoolKey, Arc<Mutex<PoolEntry>>>,
        incoming: &PoolKey,
    ) -> Option<Vec<Arc<Mutex<PoolEntry>>>> {
        let session_count = map
            .keys()
            .filter(|key| {
                key.session_id == incoming.session_id && key.agent_id == incoming.agent_id
            })
            .count();
        let mut victims = Vec::new();
        if session_count >= MAX_PER_SESSION {
            let victim = lru_idle_ready(map, |key| {
                key.session_id == incoming.session_id && key.agent_id == incoming.agent_id
            })
            .await?;
            victims.push(map.remove(&victim)?);
        }
        if map.len() >= MAX_GLOBAL {
            let victim = lru_idle_ready(map, |_| true).await?;
            victims.push(map.remove(&victim)?);
        }
        Some(victims)
    }

    pub async fn reload_snapshot(
        &self,
        session_id: &str,
        agent_id: &str,
        workspace: &std::path::Path,
        digest: &str,
    ) -> usize {
        let keys: Vec<PoolKey> = {
            let map = self.inner.lock().await;
            map.keys()
                .filter(|key| {
                    key.session_id == session_id
                        && key.agent_id == agent_id
                        && key.workspace_root == workspace
                        && key.config_digest == digest
                })
                .cloned()
                .collect()
        };
        let count = keys.len();
        for key in keys {
            self.retire(&key).await;
        }
        count
    }

    pub async fn retire(&self, key: &PoolKey) {
        let slot = {
            let mut map = self.inner.lock().await;
            map.remove(key)
        };
        if let Some(slot) = slot {
            retire_slot(slot).await;
        }
    }

    pub async fn close_session(&self, session_id: &str) {
        let keys: Vec<PoolKey> = {
            let map = self.inner.lock().await;
            map.keys()
                .filter(|key| key.session_id == session_id)
                .cloned()
                .collect()
        };
        for key in keys {
            self.retire(&key).await;
        }
        self.failed
            .lock()
            .await
            .retain(|key, _| key.session_id != session_id);
    }

    pub async fn shutdown(&self) {
        let keys: Vec<PoolKey> = {
            let map = self.inner.lock().await;
            map.keys().cloned().collect()
        };
        for key in keys {
            self.retire(&key).await;
        }
        self.failed.lock().await.clear();
    }

    pub async fn has_ready(&self, key: &PoolKey) -> bool {
        if key.validate_operator_guard().is_err() {
            return false;
        }
        let map = self.inner.lock().await;
        let Some(slot) = map.get(key).cloned() else {
            return false;
        };
        drop(map);
        let captured_credentials = slot.lock().await.credential_snapshot.clone();
        let current_credentials = if captured_credentials.is_some() {
            match crate::tools::native::current_managed_command_credential_snapshot(true).await {
                Ok(snapshot) => snapshot,
                Err(_) => {
                    self.retire(key).await;
                    return false;
                }
            }
        } else {
            None
        };
        {
            let entry = slot.lock().await;
            let candidate = entry.state == EntryState::Ready && entry.client.is_some();
            let ready = candidate && entry.credential_snapshot == current_credentials;
            let stale = candidate && !ready;
            drop(entry);
            if stale {
                self.retire(key).await;
            }
            ready
        }
    }

    pub async fn live_count(&self) -> usize {
        self.inner.lock().await.len()
    }

    pub(crate) async fn inspect_session(
        &self,
        session_id: &str,
        agent_id: &str,
        workspace: &std::path::Path,
        digest: &str,
    ) -> Result<RapidHashMap<String, PoolServerState>, String> {
        let runtime_home = crate::tools::call::lifecycle::runtime::current_tool_runtime_context()
            .and_then(|context| context.runtime_home);
        let operator_guard_home = crate::plugin::approval::workspace::command_guard_home()
            .map_err(|error| format!("{error:#}"))?;
        let guard_error = operator_guard_home.as_ref().and_then(|home| {
            crate::plugin::allowed::CommandProtection::capture(
                home,
                crate::tools::native::workspace_write_sandbox_enforced(),
            )
            .err()
            .map(|error| format!("{error:#}"))
        });
        let map = self.inner.lock().await;
        let mut states = RapidHashMap::new();
        for (key, slot) in map.iter() {
            if key.session_id == session_id
                && key.agent_id == agent_id
                && key.workspace_root == workspace
                && key.config_digest == digest
                && key.operator_guard_home == operator_guard_home
                && key.runtime_home == runtime_home
            {
                let entry = slot.lock().await;
                if let Some(error) = &guard_error {
                    states.insert(
                        key.server_name.clone(),
                        PoolServerState::Failed(error.clone()),
                    );
                    continue;
                }
                let state = match entry.state {
                    EntryState::Starting => PoolServerState::Starting,
                    EntryState::Ready
                        if entry
                            .client
                            .as_ref()
                            .is_some_and(|client| !client.is_alive()) =>
                    {
                        PoolServerState::Failed(
                            "language server exited; next action restarts it".into(),
                        )
                    }
                    EntryState::Ready => PoolServerState::Ready,
                    EntryState::Retiring => PoolServerState::Retiring,
                };
                states.insert(key.server_name.clone(), state);
            }
        }
        drop(map);
        let failed = self.failed.lock().await;
        for (key, (error, at)) in failed.iter() {
            if key.session_id == session_id
                && key.agent_id == agent_id
                && key.workspace_root == workspace
                && key.config_digest == digest
                && key.operator_guard_home == operator_guard_home
                && key.runtime_home == runtime_home
                && at.elapsed() < INIT_BACKOFF
            {
                states.insert(
                    key.server_name.clone(),
                    PoolServerState::Failed(error.clone()),
                );
            }
        }
        Ok(states)
    }

    #[cfg(test)]
    pub async fn expire_init_backoffs(&self) {
        let mut failed = self.failed.lock().await;
        for (_, at) in failed.values_mut() {
            *at = Instant::now()
                .checked_sub(INIT_BACKOFF + Duration::from_secs(1))
                .unwrap_or_else(Instant::now);
        }
    }

    #[cfg(test)]
    pub async fn force_last_used(&self, key: &PoolKey, used: Instant) {
        let map = self.inner.lock().await;
        if let Some(slot) = map.get(key) {
            slot.lock().await.last_used = used;
        }
    }

    #[cfg(test)]
    pub async fn force_idle_timeout(&self, key: &PoolKey, timeout: Duration) {
        let map = self.inner.lock().await;
        if let Some(slot) = map.get(key) {
            slot.lock().await.idle_timeout = timeout;
        }
    }
}
