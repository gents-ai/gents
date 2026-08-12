use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, Notify};

use crate::managed_exec::{
    spawn_managed_process, ManagedExecKind, SpawnManagedProcessRequest,
};

use super::admit::admit_command;
use super::catalog::CatalogServer;
use super::client::LspClient;

const MAX_PER_SESSION: usize = 4;
const MAX_GLOBAL: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PoolKey {
    pub session_id: String,
    pub behavior_id: String,
    pub workspace_root: PathBuf,
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
    leases: AtomicUsize,
    last_used: Instant,
    client: Option<Arc<LspClient>>,
    ready: Arc<Notify>,
    start_error: Option<String>,
}

pub(crate) struct LspLease {
    client: Arc<LspClient>,
    entry: Arc<Mutex<PoolEntry>>,
}

impl LspLease {
    pub fn client(&self) -> &LspClient {
        &self.client
    }
}

impl Drop for LspLease {
    fn drop(&mut self) {
        if let Ok(entry) = self.entry.try_lock() {
            entry.leases.fetch_sub(1, Ordering::SeqCst);
        }
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
        let entry = slot.lock().await;
        if entry.state != EntryState::Ready {
            return None;
        }
        let client = entry.client.clone()?;
        entry.leases.fetch_add(1, Ordering::SeqCst);
        Some(LspLease {
            client,
            entry: slot.clone(),
        })
    }

    pub(crate) async fn get_or_start(
        &self,
        key: PoolKey,
        server: &CatalogServer,
        tool_root: &Path,
        cwd: &Path,
        env: Option<std::collections::HashMap<String, String>>,
    ) -> Result<LspLease, String> {
        let (slot, starter) = {
            let mut map = self.inner.lock().await;
            if let Some(existing) = map.get(&key) {
                (existing.clone(), false)
            } else {
                if !self.has_zero_lease_capacity(&map, &key) {
                    return Err("language-server client cap reached".into());
                }
                let slot = Arc::new(Mutex::new(PoolEntry {
                    state: EntryState::Starting,
                    leases: AtomicUsize::new(0),
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
                {
                    let entry = slot.lock().await;
                    if entry.state == EntryState::Ready {
                        if let Some(client) = &entry.client {
                            entry.leases.fetch_add(1, Ordering::SeqCst);
                            return Ok(LspLease {
                                client: client.clone(),
                                entry: slot.clone(),
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
                    drop(entry);
                    ready.notified().await;
                }
            }
        }

        let admitted = admit_command(&server.command, tool_root).map_err(|err| err.diagnostic())?;
        let mut argv = vec![admitted.to_string_lossy().into_owned()];
        argv.extend(server.args.iter().cloned());
        let process = spawn_managed_process(SpawnManagedProcessRequest {
            argv,
            cwd: cwd.to_path_buf(),
            environment: env,
            tool_name: Some("lsp".into()),
            kind: ManagedExecKind::PersistentService,
        })
        .await?;
        let client = LspClient::start(process, server.name.clone())?;
        match client.initialize().await {
            Ok(_) => {
                let mut entry = slot.lock().await;
                entry.client = Some(Arc::new(client));
                entry.state = EntryState::Ready;
                entry.last_used = Instant::now();
                entry.leases.fetch_add(1, Ordering::SeqCst);
                entry.ready.notify_waiters();
                Ok(LspLease {
                    client: entry.client.clone().expect("just set"),
                    entry: slot.clone(),
                })
            }
            Err(error) => {
                let mut entry = slot.lock().await;
                entry.start_error = Some(error.clone());
                entry.state = EntryState::Retiring;
                entry.ready.notify_waiters();
                self.inner.lock().await.remove(&key);
                Err(error)
            }
        }
    }

    fn has_zero_lease_capacity(
        &self,
        map: &HashMap<PoolKey, Arc<Mutex<PoolEntry>>>,
        incoming: &PoolKey,
    ) -> bool {
        let session_count = map
            .keys()
            .filter(|key| key.session_id == incoming.session_id && key.behavior_id == incoming.behavior_id)
            .count();
        session_count < MAX_PER_SESSION && map.len() < MAX_GLOBAL
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
}


