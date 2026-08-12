use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{Mutex, Notify};

use crate::managed_exec::{spawn_managed_process, ManagedExecKind, SpawnManagedProcessRequest};
use crate::toolset::prepare_managed_command;

use super::catalog::CatalogServer;
use super::client::LspClient;
use super::LspToolConfig;

const MAX_PER_SESSION: usize = 4;
const MAX_GLOBAL: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PoolKey {
    pub session_id: String,
    pub behavior_id: String,
    pub workspace_root: std::path::PathBuf,
    pub server_name: String,
    pub config_digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryState {
    Starting,
    Ready,
    Retiring,
}

struct PoolEntry {
    state: EntryState,
    leases: Arc<AtomicUsize>,
    last_used: Instant,
    client: Option<Arc<LspClient>>,
    ready: Arc<Notify>,
    start_error: Option<String>,
}

pub(crate) struct LspLease {
    client: Arc<LspClient>,
    leases: Arc<AtomicUsize>,
}

impl LspLease {
    pub fn client(&self) -> &LspClient {
        &self.client
    }
}

impl Drop for LspLease {
    fn drop(&mut self) {
        self.leases.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Clone, Default)]
pub struct LspPool {
    inner: Arc<Mutex<HashMap<PoolKey, Arc<Mutex<PoolEntry>>>>>,
}

impl LspPool {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) async fn get_ready(&self, key: &PoolKey) -> Option<LspLease> {
        let map = self.inner.lock().await;
        let slot = map.get(key)?.clone();
        drop(map);
        let mut entry = slot.lock().await;
        if entry.state != EntryState::Ready {
            return None;
        }
        let client = entry.client.clone()?;
        entry.leases.fetch_add(1, Ordering::SeqCst);
        entry.last_used = Instant::now();
        Some(LspLease {
            client,
            leases: entry.leases.clone(),
        })
    }

    pub(crate) async fn get_or_start(
        &self,
        key: PoolKey,
        server: &CatalogServer,
        config: &LspToolConfig,
    ) -> Result<LspLease, String> {
        let (slot, starter) = {
            let mut map = self.inner.lock().await;
            if let Some(existing) = map.get(&key) {
                (existing.clone(), false)
            } else {
                if !self.evict_or_has_capacity(&mut map, &key).await {
                    return Err("language-server client cap reached".into());
                }
                let slot = Arc::new(Mutex::new(PoolEntry {
                    state: EntryState::Starting,
                    leases: Arc::new(AtomicUsize::new(0)),
                    last_used: Instant::now(),
                    client: None,
                    ready: Arc::new(Notify::new()),
                    start_error: None,
                }));
                map.insert(key.clone(), slot.clone());
                (slot, true)
            }
        };

        if !starter {
            loop {
                let mut entry = slot.lock().await;
                if entry.state == EntryState::Ready {
                    if let Some(client) = entry.client.clone() {
                        entry.leases.fetch_add(1, Ordering::SeqCst);
                        entry.last_used = Instant::now();
                        return Ok(LspLease {
                            client,
                            leases: entry.leases.clone(),
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
                drop(entry);
                notified.await;
            }
        }

        let started = self.start_client(&key, server, config).await;
        match started {
            Ok(client) => {
                let client = Arc::new(client);
                let mut entry = slot.lock().await;
                entry.client = Some(client.clone());
                entry.state = EntryState::Ready;
                entry.last_used = Instant::now();
                entry.leases.fetch_add(1, Ordering::SeqCst);
                entry.ready.notify_waiters();
                Ok(LspLease {
                    client,
                    leases: entry.leases.clone(),
                })
            }
            Err(error) => {
                let mut entry = slot.lock().await;
                entry.start_error = Some(error.clone());
                entry.state = EntryState::Retiring;
                entry.ready.notify_waiters();
                drop(entry);
                self.inner.lock().await.remove(&key);
                Err(error)
            }
        }
    }

    async fn start_client(
        &self,
        _key: &PoolKey,
        server: &CatalogServer,
        config: &LspToolConfig,
    ) -> Result<LspClient, String> {
        let (program, argv, env, _sandbox) = prepare_managed_command(
            &config.workspace,
            &server.command,
            &server.args,
            &config.constraints,
        )
        .map_err(|err| err.to_string())?;
        let mut full_argv = vec![program.to_string_lossy().into_owned()];
        full_argv.extend(argv);
        let process = spawn_managed_process(SpawnManagedProcessRequest {
            argv: full_argv,
            cwd: config.workspace.clone(),
            environment: Some(env),
            tool_name: Some("lsp".into()),
            kind: ManagedExecKind::PersistentService,
        })
        .await?;
        let client = LspClient::start(process, server.name.clone(), config, server)?;
        client.initialize().await?;
        Ok(client)
    }

    async fn evict_or_has_capacity(
        &self,
        map: &mut HashMap<PoolKey, Arc<Mutex<PoolEntry>>>,
        incoming: &PoolKey,
    ) -> bool {
        let session_count = map
            .keys()
            .filter(|key| {
                key.session_id == incoming.session_id && key.behavior_id == incoming.behavior_id
            })
            .count();
        if session_count < MAX_PER_SESSION && map.len() < MAX_GLOBAL {
            return true;
        }
        let mut victim: Option<(PoolKey, Instant)> = None;
        for (key, slot) in map.iter() {
            let entry = slot.lock().await;
            if entry.state == EntryState::Ready && entry.leases.load(Ordering::SeqCst) == 0 {
                if victim
                    .as_ref()
                    .is_none_or(|(_, used)| entry.last_used < *used)
                {
                    victim = Some((key.clone(), entry.last_used));
                }
            }
        }
        if let Some((key, _)) = victim {
            if let Some(slot) = map.remove(&key) {
                let mut entry = slot.lock().await;
                entry.state = EntryState::Retiring;
                if let Some(client) = entry.client.take() {
                    client.shutdown_exit().await;
                }
            }
            return true;
        }
        false
    }

    pub async fn retire(&self, key: &PoolKey) {
        let slot = {
            let mut map = self.inner.lock().await;
            map.remove(key)
        };
        if let Some(slot) = slot {
            let mut entry = slot.lock().await;
            entry.state = EntryState::Retiring;
            if entry.leases.load(Ordering::SeqCst) == 0 {
                if let Some(client) = entry.client.take() {
                    client.shutdown_exit().await;
                }
            }
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
    }

    pub async fn shutdown(&self) {
        let keys: Vec<PoolKey> = {
            let map = self.inner.lock().await;
            map.keys().cloned().collect()
        };
        for key in keys {
            self.retire(&key).await;
        }
    }

    pub async fn has_ready(&self, key: &PoolKey) -> bool {
        let map = self.inner.lock().await;
        if let Some(slot) = map.get(key) {
            let entry = slot.lock().await;
            return entry.state == EntryState::Ready && entry.client.is_some();
        }
        false
    }

    pub async fn live_count(&self) -> usize {
        self.inner.lock().await.len()
    }

    pub async fn inspect_session(
        &self,
        session_id: &str,
        behavior_id: &str,
        workspace: &std::path::Path,
        digest: &str,
    ) -> Vec<String> {
        let map = self.inner.lock().await;
        let mut names = Vec::new();
        for (key, slot) in map.iter() {
            if key.session_id == session_id
                && key.behavior_id == behavior_id
                && key.workspace_root == workspace
                && key.config_digest == digest
            {
                let entry = slot.lock().await;
                if entry.state == EntryState::Ready {
                    names.push(key.server_name.clone());
                }
            }
        }
        names
    }
}
