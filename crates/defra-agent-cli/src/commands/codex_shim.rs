use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use codex_app_server_protocol as codex;
use defra_agent::defra_node::EmbeddedNode;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch, Mutex};
use tokio::task::JoinHandle;

mod background;
mod bound_behavior;
mod command_projection;
mod compat;
mod handlers;
mod history_projection;
mod host_runtime;
mod progress;
mod protocol;
mod store;
mod thread_projection;
mod thread_routes;
mod trace;
mod turn;
mod turn_projection;

const JSONRPC_INVALID_REQUEST: i64 = -32600;
const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;
const JSONRPC_INVALID_PARAMS: i64 = -32602;
const JSONRPC_INTERNAL_ERROR: i64 = -32603;

#[derive(Clone)]
struct ShimState {
    codex_home: PathBuf,
    trace_path: PathBuf,
    cwd: PathBuf,
    fs_root: Option<PathBuf>,
    node: Arc<EmbeddedNode>,
    background_execution_registry: defra_agent::BackgroundExecutionRegistry,
    graphql: Arc<str>,
    agent_did: Arc<str>,
    behavior_id: Arc<str>,
    id_counter: Arc<AtomicU64>,
    timeout: Duration,
    poll_interval: Duration,
    sidecar: Arc<Mutex<CodexSidecar>>,
}

type Outbound = mpsc::UnboundedSender<String>;

/// Codex threads default to memory disabled: this shim does not wire the Codex
/// memory feature, so reporting it as enabled would be dishonest (#494).
pub(crate) const DEFAULT_MEMORY_MODE: &str = "disabled";

#[derive(Default)]
pub(crate) struct CodexSidecar {
    pub(crate) cwd: BTreeMap<String, PathBuf>,
    pub(crate) loaded: BTreeSet<String>,
    pub(crate) archived: BTreeSet<String>,
    pub(crate) memory_mode: BTreeMap<String, String>,
    pub(crate) settings: BTreeMap<String, String>,
    pub(crate) goal: BTreeMap<String, crate::commands::codex_shim::thread_projection::StoredGoal>,
}

impl CodexSidecar {
    /// The thread's memory mode, falling back to [`DEFAULT_MEMORY_MODE`] when the
    /// thread has no explicit `ThreadMemoryModeSet` override.
    pub(crate) fn memory_mode_or_default(&self, thread_id: &str) -> String {
        self.memory_mode
            .get(thread_id)
            .cloned()
            .unwrap_or_else(|| DEFAULT_MEMORY_MODE.to_string())
    }
}

#[derive(Clone)]
struct ConnectionState {
    outbound: Outbound,
    turn_streams: Arc<Mutex<BTreeMap<String, TurnStreamControl>>>,
    fuzzy_file_search_sessions: Arc<Mutex<BTreeMap<String, Vec<String>>>>,
    pending_steering_inputs: Arc<Mutex<BTreeMap<String, Vec<codex::UserInput>>>>,
}

#[derive(Clone, Debug)]
struct TurnStreamControl {
    cancel_tx: watch::Sender<bool>,
}

pub(crate) struct CodexShimBindArgs {
    pub(crate) home: PathBuf,
    pub(crate) fs_root: Option<PathBuf>,
    pub(crate) node: Arc<EmbeddedNode>,
    pub(crate) background_execution_registry: defra_agent::BackgroundExecutionRegistry,
    pub(crate) graphql: String,
    pub(crate) agent_did: String,
    pub(crate) behavior_id: Option<String>,
    pub(crate) bind_addr: std::net::IpAddr,
    pub(crate) port: u16,
    pub(crate) timeout_secs: u64,
    pub(crate) poll_ms: u64,
}

pub(crate) struct BoundCodexShim {
    addr: SocketAddr,
    codex_home: PathBuf,
    trace_path: PathBuf,
    listener: TcpListener,
    app: Router,
}

impl BoundCodexShim {
    pub(crate) fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub(crate) fn codex_home(&self) -> &Path {
        &self.codex_home
    }

    pub(crate) fn trace_path(&self) -> &Path {
        &self.trace_path
    }

    pub(crate) fn spawn(self) -> JoinHandle<Result<()>> {
        tokio::spawn(self.serve())
    }

    async fn serve(self) -> Result<()> {
        axum::serve(self.listener, self.app)
            .await
            .context("serving Codex TUI shim")
    }
}

pub(crate) async fn bind_codex_shim(args: CodexShimBindArgs) -> Result<BoundCodexShim> {
    if args.bind_addr.is_unspecified() {
        anyhow::bail!(
            "refusing to bind unauthenticated Codex shim on {}; bind loopback or a specific trusted private/Tailscale IP instead",
            args.bind_addr
        );
    }

    let codex_home = args.home.join("codex-ui");
    let codex_log_dir = codex_home.join("log");
    fs::create_dir_all(&codex_log_dir)
        .with_context(|| format!("creating Codex UI log dir {}", codex_log_dir.display()))?;
    let trace_path = codex_log_dir.join("codex-shim-events.jsonl");

    let bound_behavior_id = bound_behavior::resolve_bound_behavior_id(
        args.node.as_ref(),
        args.behavior_id.as_deref(),
        &args.agent_did,
    )
    .await;
    bound_behavior::load_bound_inference_profile_id(args.node.as_ref(), &bound_behavior_id)
        .await
        .with_context(|| format!("validating Codex shim bound behavior {bound_behavior_id:?}"))?;

    let state = ShimState {
        codex_home: codex_home.clone(),
        trace_path: trace_path.clone(),
        cwd: std::env::current_dir().context("resolving current working directory")?,
        fs_root: args.fs_root,
        node: args.node,
        background_execution_registry: args.background_execution_registry,
        graphql: Arc::from(args.graphql.clone()),
        agent_did: Arc::from(args.agent_did.clone()),
        behavior_id: Arc::from(bound_behavior_id),
        id_counter: Arc::new(AtomicU64::new(1)),
        timeout: Duration::from_secs(args.timeout_secs),
        poll_interval: Duration::from_millis(args.poll_ms.max(1)),
        sidecar: Arc::new(Mutex::new(CodexSidecar::default())),
    };

    let app = Router::new()
        .route("/", get(ws_upgrade))
        .with_state(state.clone());
    let addr = SocketAddr::new(args.bind_addr, args.port);
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding Codex shim on {addr}"))?;

    Ok(BoundCodexShim {
        addr,
        codex_home,
        trace_path,
        listener,
        app,
    })
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<ShimState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: ShimState) {
    tracing::info!("Codex shim WebSocket connected");
    trace::shim_event(&state.trace_path, "websocket connected");
    let (mut sender, mut receiver) = socket.split();
    let (outbound, mut outbound_rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(text) = outbound_rx.recv().await {
            if sender.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });
    let connection = ConnectionState {
        outbound,
        turn_streams: Arc::new(Mutex::new(BTreeMap::new())),
        fuzzy_file_search_sessions: Arc::new(Mutex::new(BTreeMap::new())),
        pending_steering_inputs: Arc::new(Mutex::new(BTreeMap::new())),
    };

    while let Some(message) = receiver.next().await {
        let Ok(message) = message else {
            break;
        };
        let Message::Text(text) = message else {
            continue;
        };

        let Ok(payload) = serde_json::from_str::<codex::JSONRPCMessage>(&text) else {
            tracing::warn!("dropping invalid Codex shim JSON-RPC message");
            continue;
        };

        let result = match payload {
            codex::JSONRPCMessage::Request(request) => {
                handlers::handle_request(&connection, &state, request).await
            }
            codex::JSONRPCMessage::Notification(notification) => {
                tracing::trace!(?notification, "Codex shim received client notification");
                Ok(())
            }
            codex::JSONRPCMessage::Response(response) => {
                tracing::trace!(?response, "Codex shim received client response");
                Ok(())
            }
            codex::JSONRPCMessage::Error(error) => {
                tracing::trace!(?error, "Codex shim received client error");
                Ok(())
            }
        };

        if let Err(err) = result {
            tracing::warn!(%err, "Codex shim request handling failed");
            break;
        }
    }

    connection.fuzzy_file_search_sessions.lock().await.clear();
    connection.pending_steering_inputs.lock().await.clear();
    writer.abort();
}

impl ShimState {
    fn next_thread_id(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }

    fn next_id(&self, prefix: &str) -> String {
        let id = self.id_counter.fetch_add(1, Ordering::Relaxed);
        format!("{prefix}-{id}")
    }

    async fn thread_cwd(&self, thread_id: &str) -> PathBuf {
        self.sidecar
            .lock()
            .await
            .cwd
            .get(thread_id)
            .cloned()
            .unwrap_or_else(|| self.cwd.clone())
    }

    async fn thread_cwd_override(&self, thread_id: &str) -> Option<PathBuf> {
        self.sidecar.lock().await.cwd.get(thread_id).cloned()
    }

    async fn set_thread_cwd(&self, thread_id: &str, cwd: PathBuf) {
        self.sidecar
            .lock()
            .await
            .cwd
            .insert(thread_id.to_string(), cwd);
    }

    async fn is_thread_loaded(&self, thread_id: &str) -> bool {
        self.sidecar.lock().await.loaded.contains(thread_id)
    }

    async fn set_thread_loaded(&self, thread_id: &str, loaded: bool) {
        let mut guard = self.sidecar.lock().await;
        if loaded {
            guard.loaded.insert(thread_id.to_string());
        } else {
            guard.loaded.remove(thread_id);
        }
    }

    async fn loaded_thread_ids(&self) -> Vec<String> {
        let guard = self.sidecar.lock().await;
        guard
            .loaded
            .iter()
            .filter(|thread_id| !guard.archived.contains(*thread_id))
            .cloned()
            .collect()
    }

    async fn is_thread_archived(&self, thread_id: &str) -> bool {
        self.sidecar.lock().await.archived.contains(thread_id)
    }

    async fn set_thread_archived(&self, thread_id: &str, archived: bool) {
        let mut guard = self.sidecar.lock().await;
        if archived {
            guard.archived.insert(thread_id.to_string());
            guard.loaded.remove(thread_id);
        } else {
            guard.archived.remove(thread_id);
        }
    }

    async fn thread_memory_mode(&self, thread_id: &str) -> String {
        self.sidecar.lock().await.memory_mode_or_default(thread_id)
    }

    async fn set_thread_memory_mode(&self, thread_id: &str, mode: &str) {
        self.sidecar
            .lock()
            .await
            .memory_mode
            .insert(thread_id.to_string(), mode.to_string());
    }

    async fn thread_settings(&self, thread_id: &str) -> String {
        self.sidecar
            .lock()
            .await
            .settings
            .get(thread_id)
            .cloned()
            .unwrap_or_else(|| "{}".to_string())
    }

    async fn set_thread_settings(&self, thread_id: &str, settings_json: &str) {
        self.sidecar
            .lock()
            .await
            .settings
            .insert(thread_id.to_string(), settings_json.to_string());
    }

    async fn thread_goal(
        &self,
        thread_id: &str,
    ) -> Option<crate::commands::codex_shim::thread_projection::StoredGoal> {
        self.sidecar.lock().await.goal.get(thread_id).cloned()
    }

    async fn set_thread_goal(
        &self,
        thread_id: &str,
        goal: crate::commands::codex_shim::thread_projection::StoredGoal,
    ) {
        self.sidecar
            .lock()
            .await
            .goal
            .insert(thread_id.to_string(), goal);
    }

    async fn clear_thread_goal(&self, thread_id: &str) -> bool {
        self.sidecar.lock().await.goal.remove(thread_id).is_some()
    }
}

impl ConnectionState {
    async fn remember_steering_input(&self, request_id: String, input: Vec<codex::UserInput>) {
        self.pending_steering_inputs
            .lock()
            .await
            .insert(request_id, input);
    }

    async fn take_steering_input(&self, request_id: &str) -> Option<Vec<codex::UserInput>> {
        self.pending_steering_inputs.lock().await.remove(request_id)
    }
}

#[cfg(test)]
mod tests {
    use super::{CodexSidecar, DEFAULT_MEMORY_MODE};

    #[test]
    fn memory_mode_defaults_to_disabled_for_unknown_thread() {
        let sidecar = CodexSidecar::default();
        assert_eq!(sidecar.memory_mode_or_default("never-set"), "disabled");
        assert_eq!(DEFAULT_MEMORY_MODE, "disabled");
    }

    #[test]
    fn memory_mode_returns_explicit_override_when_set() {
        let mut sidecar = CodexSidecar::default();
        sidecar
            .memory_mode
            .insert("t1".to_string(), "enabled".to_string());
        assert_eq!(sidecar.memory_mode_or_default("t1"), "enabled");
        assert_eq!(sidecar.memory_mode_or_default("t2"), "disabled");
    }
}
