use super::*;

#[derive(Clone)]
pub(super) struct ShimState {
    pub(super) codex_home: PathBuf,
    pub(super) trace_path: PathBuf,
    pub(super) cwd: PathBuf,
    pub(super) fs_root: Option<PathBuf>,
    pub(super) node: Arc<EmbeddedNode>,
    pub(super) background_execution_registry: gents::BackgroundExecutionRegistry,
    pub(super) graphql: Arc<str>,
    pub(super) agent_did: Arc<str>,
    pub(super) behavior_id: Arc<str>,
    pub(super) id_counter: Arc<AtomicU64>,
    pub(super) timeout: Duration,
    pub(super) poll_interval: Duration,
    pub(super) sidecar: Arc<Mutex<CodexSidecar>>,
    pub(super) auth_token: Option<Arc<str>>,
}

pub(super) type Outbound = mpsc::UnboundedSender<String>;

/// Codex threads default to memory disabled: this shim does not wire the Codex
/// memory feature, so reporting it as enabled would be dishonest (#494).
pub(crate) const DEFAULT_MEMORY_MODE: &str = "disabled";

#[derive(Default)]
pub(crate) struct CodexSidecar {
    /// Empty threads created by this shim remain process-local until their
    /// first AgentRequest lets the runtime materialize the canonical session.
    pub(crate) created: BTreeSet<String>,
    pub(crate) cwd: BTreeMap<String, PathBuf>,
    pub(crate) loaded: BTreeSet<String>,
    pub(crate) archived: BTreeSet<String>,
    pub(crate) memory_mode: BTreeMap<String, String>,
    pub(crate) settings: BTreeMap<String, String>,
    pub(crate) names: BTreeMap<String, String>,
}

impl CodexSidecar {
    pub(crate) fn memory_mode_or_default(&self, thread_id: &str) -> String {
        self.memory_mode
            .get(thread_id)
            .cloned()
            .unwrap_or_else(|| DEFAULT_MEMORY_MODE.to_string())
    }
}

#[derive(Clone)]
pub(super) struct ConnectionState {
    pub(super) outbound: Outbound,
    pub(super) turn_streams: Arc<Mutex<BTreeMap<String, TurnStreamControl>>>,
    pub(super) fuzzy_file_search_sessions: Arc<Mutex<BTreeMap<String, Vec<String>>>>,
    pub(super) pending_steering_inputs: Arc<Mutex<BTreeMap<String, Vec<codex::UserInput>>>>,
    pub(super) child_thread_streams: Arc<Mutex<BTreeMap<String, ChildThreadStreamControl>>>,
    pub(super) root_continuation_streams:
        Arc<Mutex<BTreeMap<String, RootContinuationStreamControl>>>,
}

#[derive(Clone, Debug)]
pub(super) struct TurnStreamControl {
    pub(super) stream_id: String,
    pub(super) owner_id: Option<String>,
    pub(super) cancel_tx: watch::Sender<bool>,
}

#[derive(Clone, Debug)]
pub(super) struct ChildThreadStreamControl {
    pub(super) watcher_id: String,
    pub(super) abort_handle: tokio::task::AbortHandle,
}

#[derive(Clone, Debug)]
pub(super) struct RootContinuationStreamControl {
    pub(super) watcher_id: String,
    pub(super) abort_handle: tokio::task::AbortHandle,
}

impl ShimState {
    pub(super) fn next_thread_id(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }

    pub(super) fn next_id(&self, prefix: &str) -> String {
        let id = self.id_counter.fetch_add(1, Ordering::Relaxed);
        format!("{prefix}-{id}")
    }

    pub(super) async fn thread_cwd(&self, thread_id: &str) -> PathBuf {
        self.sidecar
            .lock()
            .await
            .cwd
            .get(thread_id)
            .cloned()
            .unwrap_or_else(|| self.cwd.clone())
    }

    pub(super) async fn thread_cwd_override(&self, thread_id: &str) -> Option<PathBuf> {
        self.sidecar.lock().await.cwd.get(thread_id).cloned()
    }

    pub(super) async fn set_thread_cwd(&self, thread_id: &str, cwd: PathBuf) {
        self.sidecar
            .lock()
            .await
            .cwd
            .insert(thread_id.to_string(), cwd);
    }

    pub(super) async fn is_thread_loaded(&self, thread_id: &str) -> bool {
        self.sidecar.lock().await.loaded.contains(thread_id)
    }

    pub(super) async fn set_thread_loaded(&self, thread_id: &str, loaded: bool) {
        let mut guard = self.sidecar.lock().await;
        if loaded {
            guard.loaded.insert(thread_id.to_string());
        } else {
            guard.loaded.remove(thread_id);
        }
    }

    pub(super) async fn loaded_thread_ids(&self) -> Vec<String> {
        let guard = self.sidecar.lock().await;
        guard
            .loaded
            .iter()
            .filter(|thread_id| !guard.archived.contains(*thread_id))
            .cloned()
            .collect()
    }

    pub(super) async fn is_thread_archived(&self, thread_id: &str) -> bool {
        self.sidecar.lock().await.archived.contains(thread_id)
    }

    pub(super) async fn set_thread_archived(&self, thread_id: &str, archived: bool) {
        let mut guard = self.sidecar.lock().await;
        if archived {
            guard.archived.insert(thread_id.to_string());
            guard.loaded.remove(thread_id);
        } else {
            guard.archived.remove(thread_id);
        }
    }

    pub(super) async fn mark_thread_created(&self, thread_id: &str) {
        self.sidecar
            .lock()
            .await
            .created
            .insert(thread_id.to_string());
    }

    pub(super) async fn is_thread_created(&self, thread_id: &str) -> bool {
        self.sidecar.lock().await.created.contains(thread_id)
    }

    pub(super) async fn created_thread_ids(&self) -> Vec<String> {
        self.sidecar.lock().await.created.iter().cloned().collect()
    }

    pub(super) async fn thread_name(&self, thread_id: &str) -> String {
        self.sidecar
            .lock()
            .await
            .names
            .get(thread_id)
            .cloned()
            .unwrap_or_default()
    }

    pub(super) async fn set_thread_name(&self, thread_id: &str, name: &str) {
        self.sidecar
            .lock()
            .await
            .names
            .insert(thread_id.to_string(), name.to_string());
    }

    pub(super) async fn thread_memory_mode(&self, thread_id: &str) -> String {
        self.sidecar.lock().await.memory_mode_or_default(thread_id)
    }

    pub(super) async fn set_thread_memory_mode(&self, thread_id: &str, mode: &str) {
        self.sidecar
            .lock()
            .await
            .memory_mode
            .insert(thread_id.to_string(), mode.to_string());
    }

    pub(super) async fn thread_settings(&self, thread_id: &str) -> String {
        self.sidecar
            .lock()
            .await
            .settings
            .get(thread_id)
            .cloned()
            .unwrap_or_else(|| "{}".to_string())
    }

    pub(super) async fn set_thread_settings(&self, thread_id: &str, settings_json: &str) {
        self.sidecar
            .lock()
            .await
            .settings
            .insert(thread_id.to_string(), settings_json.to_string());
    }
}

impl ConnectionState {
    pub(super) async fn has_turn_stream(&self, thread_id: &str, turn_id: &str) -> bool {
        self.turn_streams
            .lock()
            .await
            .contains_key(&format!("{thread_id}:{turn_id}"))
    }

    pub(super) async fn replace_root_continuation_stream(
        &self,
        thread_id: String,
        watcher_id: String,
        abort_handle: tokio::task::AbortHandle,
    ) {
        let previous = self.root_continuation_streams.lock().await.insert(
            thread_id,
            RootContinuationStreamControl {
                watcher_id,
                abort_handle,
            },
        );
        if let Some(previous) = previous {
            self.clear_turn_streams_owned_by(&previous.watcher_id).await;
            previous.abort_handle.abort();
        }
    }

    pub(super) async fn clear_turn_streams_owned_by(&self, owner_id: &str) {
        self.turn_streams
            .lock()
            .await
            .retain(|_, control| control.owner_id.as_deref() != Some(owner_id));
    }

    pub(super) async fn clear_root_continuation_stream_if_current(
        &self,
        thread_id: &str,
        watcher_id: &str,
    ) {
        let mut streams = self.root_continuation_streams.lock().await;
        if streams
            .get(thread_id)
            .is_some_and(|control| control.watcher_id == watcher_id)
        {
            streams.remove(thread_id);
        }
    }

    pub(super) async fn stop_root_continuation_stream(&self, thread_id: &str) {
        if let Some(control) = self
            .root_continuation_streams
            .lock()
            .await
            .remove(thread_id)
        {
            self.clear_turn_streams_owned_by(&control.watcher_id).await;
            control.abort_handle.abort();
        }
    }

    pub(super) async fn stop_all_root_continuation_streams(&self) {
        let controls = std::mem::take(&mut *self.root_continuation_streams.lock().await);
        for control in controls.into_values() {
            self.clear_turn_streams_owned_by(&control.watcher_id).await;
            control.abort_handle.abort();
        }
    }

    pub(super) async fn replace_child_stream(
        &self,
        thread_id: String,
        watcher_id: String,
        abort_handle: tokio::task::AbortHandle,
    ) {
        let previous = self.child_thread_streams.lock().await.insert(
            thread_id,
            ChildThreadStreamControl {
                watcher_id,
                abort_handle,
            },
        );
        if let Some(previous) = previous {
            previous.abort_handle.abort();
        }
    }

    pub(super) async fn clear_child_stream_if_current(&self, thread_id: &str, watcher_id: &str) {
        let mut streams = self.child_thread_streams.lock().await;
        if streams
            .get(thread_id)
            .is_some_and(|control| control.watcher_id == watcher_id)
        {
            streams.remove(thread_id);
        }
    }

    pub(super) async fn stop_child_stream(&self, thread_id: &str) {
        if let Some(control) = self.child_thread_streams.lock().await.remove(thread_id) {
            control.abort_handle.abort();
        }
    }

    pub(super) async fn stop_all_child_streams(&self) {
        let controls = std::mem::take(&mut *self.child_thread_streams.lock().await);
        for control in controls.into_values() {
            control.abort_handle.abort();
        }
    }

    pub(super) async fn remember_steering_input(
        &self,
        request_id: String,
        input: Vec<codex::UserInput>,
    ) {
        self.pending_steering_inputs
            .lock()
            .await
            .insert(request_id, input);
    }

    pub(super) async fn take_steering_input(
        &self,
        request_id: &str,
    ) -> Option<Vec<codex::UserInput>> {
        self.pending_steering_inputs.lock().await.remove(request_id)
    }
}
