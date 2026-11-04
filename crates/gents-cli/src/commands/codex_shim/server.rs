use super::*;
use axum::extract::Path as RoutePath;
use rapidhash::{HashMapExt, RapidHashMap};

#[derive(Clone)]
pub(crate) struct CodexShimBindArgs {
    pub(crate) usage_cache: Arc<gents::inference::usage_observation::UsageCache>,
    pub(crate) home: PathBuf,
    pub(crate) fs_root: Option<PathBuf>,
    pub(crate) node: Arc<EmbeddedNode>,
    pub(crate) background_execution_registry: gents::BackgroundExecutionRegistry,
    pub(crate) graphql: String,
    pub(crate) node_did: String,
    pub(crate) agent_id: Option<String>,
    pub(crate) auth_token: Option<String>,
    pub(crate) bind_addr: std::net::IpAddr,
    pub(crate) port: u16,
    pub(crate) timeout_secs: u64,
    pub(crate) poll_ms: u64,
    pub(crate) runnable: watch::Receiver<Vec<String>>,
    pub(crate) agent_routes: bool,
}

/// What `GET /runtime` answers, so `gents` can tell whether the runtime on the
/// fixed endpoint serves the home it was asked to open.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct RuntimeIdentity {
    pub(crate) node_did: String,
    pub(crate) home: PathBuf,
    /// The published runnable set, or `None` when `/agents/{agent_id}` is not mounted.
    pub(crate) runnable_agents: Option<Vec<String>>,
    /// Admission refuses a placement outside this.
    pub(crate) tool_root: Option<PathBuf>,
    /// Where a thread starts when its client names no directory.
    pub(crate) default_thread_cwd: PathBuf,
}

#[derive(Clone)]
struct RuntimeRouteState {
    identity: RuntimeIdentity,
    shim: ShimState,
    agent_routes: bool,
    runnable: watch::Receiver<Vec<String>>,
    sidecars: Arc<Mutex<RapidHashMap<String, Arc<Mutex<CodexSidecar>>>>>,
}

pub(crate) struct BoundCodexShim {
    addr: SocketAddr,
    codex_home: PathBuf,
    trace_path: PathBuf,
    listener: TcpListener,
    app: Router,
    node_did: String,
    agent_id: String,
    auth_required: bool,
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

    pub(crate) fn node_did(&self) -> &str {
        &self.node_did
    }

    pub(crate) fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub(crate) fn auth_required(&self) -> bool {
        self.auth_required
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

pub(crate) async fn resolve_codex_shim_agent_id(
    node: &EmbeddedNode,
    override_agent_id: Option<&str>,
    node_did: &str,
) -> Result<String> {
    bound_behavior::resolve_bound_agent_id(node, override_agent_id, node_did).await
}

pub(crate) enum CodexShimBindError {
    DependencyMissing(anyhow::Error),
    HostResource(anyhow::Error),
}

impl CodexShimBindError {
    pub(crate) fn error(&self) -> &anyhow::Error {
        match self {
            Self::DependencyMissing(error) | Self::HostResource(error) => error,
        }
    }

    pub(crate) fn is_dependency_missing(&self) -> bool {
        matches!(self, Self::DependencyMissing(_))
    }
}

pub(crate) async fn bind_codex_shim(
    args: CodexShimBindArgs,
) -> std::result::Result<BoundCodexShim, CodexShimBindError> {
    validate_bind_security(args.bind_addr, args.auth_token.as_deref())
        .map_err(CodexShimBindError::HostResource)?;

    let codex_home = args.home.join(gents::store::local::home::CODEX_UI_DIR_NAME);
    let codex_log_dir = codex_home.join("log");
    fs::create_dir_all(&codex_log_dir)
        .with_context(|| format!("creating Codex UI log dir {}", codex_log_dir.display()))
        .map_err(CodexShimBindError::HostResource)?;
    let trace_path = codex_log_dir.join("codex-shim-events.jsonl");
    let node_did = args.node_did.clone();
    let auth_required = args.auth_token.is_some();

    let bound_agent_id = bound_behavior::resolve_bound_agent_id(
        args.node.as_ref(),
        args.agent_id.as_deref(),
        &args.node_did,
    )
    .await
    .map_err(CodexShimBindError::DependencyMissing)?;
    bound_behavior::load_bound_inference_profile_id(&args.node, &args.node_did, &bound_agent_id)
        .await
        .with_context(|| format!("validating Codex shim bound agent {bound_agent_id:?}"))
        .map_err(CodexShimBindError::DependencyMissing)?;

    let thread_cwd = default_thread_cwd(
        std::env::current_dir()
            .context("resolving current working directory")
            .map_err(CodexShimBindError::HostResource)?,
        args.fs_root.as_deref(),
    );
    let identity = RuntimeIdentity {
        node_did: args.node_did.clone(),
        home: crate::absolutize(args.home.clone()).map_err(CodexShimBindError::HostResource)?,
        runnable_agents: None,
        tool_root: args.fs_root.clone(),
        default_thread_cwd: thread_cwd.clone(),
    };
    let agent_routes = args.agent_routes && args.bind_addr.is_loopback();

    let state = ShimState {
        usage_cache: args.usage_cache,
        codex_home: codex_home.clone(),
        trace_path: trace_path.clone(),
        cwd: thread_cwd,
        fs_root: args.fs_root,
        node: args.node,
        background_execution_registry: args.background_execution_registry,
        graphql: gents::store::config_client::GraphqlEndpoint::as_principal(
            args.graphql.clone(),
            args.node_did.clone(),
        ),
        node_did: Arc::from(args.node_did.clone()),
        agent_id: Arc::from(bound_agent_id.clone()),
        id_counter: Arc::new(AtomicU64::new(1)),
        timeout: Duration::from_secs(args.timeout_secs),
        poll_interval: Duration::from_millis(args.poll_ms.max(1)),
        sidecar: Arc::new(Mutex::new(CodexSidecar::default())),
        auth_token: args.auth_token.map(Arc::from),
    };

    let mut sidecars = RapidHashMap::new();
    sidecars.insert(bound_agent_id.clone(), state.sidecar.clone());
    let mut runtime_routes = Router::new().route("/runtime", get(runtime_identity));
    if agent_routes {
        runtime_routes = runtime_routes.route("/agents/{agent_id}", get(agent_ws_upgrade));
    }
    let runtime_routes = runtime_routes.with_state(RuntimeRouteState {
        identity,
        shim: state.clone(),
        agent_routes,
        runnable: args.runnable,
        sidecars: Arc::new(Mutex::new(sidecars)),
    });

    let app = Router::new()
        .route("/", get(ws_upgrade))
        .with_state(state)
        .merge(runtime_routes);
    let addr = SocketAddr::new(args.bind_addr, args.port);
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding Codex shim on {addr}"))
        .map_err(CodexShimBindError::HostResource)?;

    Ok(BoundCodexShim {
        addr,
        codex_home,
        trace_path,
        listener,
        app,
        node_did,
        agent_id: bound_agent_id,
        auth_required,
    })
}

/// A service manager starts the runtime in `/`, and Codex with `--remote` leaves a
/// thread's directory to this server, so the process directory says nothing about
/// where work belongs. Admission refuses a placement outside the tool root, so a
/// thread defaults to the root unless the process runs inside it.
pub(super) fn default_thread_cwd(process_cwd: PathBuf, tool_root: Option<&Path>) -> PathBuf {
    match tool_root {
        Some(root) if !process_cwd.starts_with(root) => root.to_path_buf(),
        _ => process_cwd,
    }
}

pub(super) fn validate_bind_security(
    bind_addr: std::net::IpAddr,
    auth_token: Option<&str>,
) -> Result<()> {
    if bind_addr.is_unspecified() {
        anyhow::bail!(
            "refusing to bind app-server shim on unspecified address {bind_addr}; bind loopback or a specific interface instead"
        );
    }
    if !bind_addr.is_loopback() && auth_token.is_none() {
        anyhow::bail!(
            "refusing to bind unauthenticated app-server shim on {bind_addr}; set --codex-shim-auth-token-env or bind loopback"
        );
    }
    Ok(())
}

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<ShimState>,
    headers: HeaderMap,
) -> Response {
    if !request_is_authorized(&headers, state.auth_token.as_deref()) {
        tracing::warn!(
            target: "gents_cli::commands::codex_shim",
            "rejected unauthorized app-server WebSocket handshake"
        );
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(move |socket| handle_socket(socket, state))
        .into_response()
}

/// Each agent gets its own sidecar: listing walks a sidecar's threads with no
/// agent filter, so a shared one would show one agent's threads to another.
async fn agent_ws_upgrade(
    ws: WebSocketUpgrade,
    RoutePath(agent_id): RoutePath<String>,
    State(routes): State<RuntimeRouteState>,
    headers: HeaderMap,
) -> Response {
    if !request_is_authorized(&headers, routes.shim.auth_token.as_deref()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !gents::codex_shim_binding::admits_connection(
        &agent_id,
        routes.runnable.borrow().iter().map(String::as_str),
    ) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let sidecar = routes
        .sidecars
        .lock()
        .await
        .entry(agent_id.clone())
        .or_default()
        .clone();
    let mut state = routes.shim;
    state.agent_id = Arc::from(agent_id);
    state.sidecar = sidecar;
    ws.on_upgrade(move |socket| handle_socket(socket, state))
        .into_response()
}

async fn runtime_identity(State(state): State<RuntimeRouteState>, headers: HeaderMap) -> Response {
    if !request_is_authorized(&headers, state.shim.auth_token.as_deref()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut identity = state.identity;
    identity.runnable_agents = state.agent_routes.then(|| state.runnable.borrow().clone());
    Json(identity).into_response()
}

pub(super) fn request_is_authorized(headers: &HeaderMap, expected: Option<&str>) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|provided| bool::from(expected.as_bytes().ct_eq(provided.as_bytes())))
}

async fn handle_socket(socket: WebSocket, state: ShimState) {
    tracing::info!(
        target: "gents_cli::commands::codex_shim",
        "Codex shim WebSocket connected"
    );
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
        child_thread_streams: Arc::new(Mutex::new(BTreeMap::new())),
        root_continuation_streams: Arc::new(Mutex::new(BTreeMap::new())),
    };

    while let Some(message) = receiver.next().await {
        let Ok(message) = message else {
            break;
        };
        let Message::Text(text) = message else {
            continue;
        };

        let Ok(payload) = serde_json::from_str::<codex::JSONRPCMessage>(&text) else {
            tracing::warn!(
                target: "gents_cli::commands::codex_shim",
                "dropping invalid Codex shim JSON-RPC message"
            );
            continue;
        };

        let result = match payload {
            codex::JSONRPCMessage::Request(request) => {
                handlers::handle_request(&connection, &state, request).await
            }
            codex::JSONRPCMessage::Notification(notification) => {
                tracing::trace!(
                    target: "gents_cli::commands::codex_shim",
                    ?notification,
                    "Codex shim received client notification"
                );
                Ok(())
            }
            codex::JSONRPCMessage::Response(response) => {
                tracing::trace!(
                    target: "gents_cli::commands::codex_shim",
                    ?response,
                    "Codex shim received client response"
                );
                Ok(())
            }
            codex::JSONRPCMessage::Error(error) => {
                tracing::trace!(
                    target: "gents_cli::commands::codex_shim",
                    ?error,
                    "Codex shim received client error"
                );
                Ok(())
            }
        };

        if let Err(err) = result {
            tracing::warn!(
                target: "gents_cli::commands::codex_shim",
                %err,
                "Codex shim request handling failed"
            );
            break;
        }
    }

    connection.fuzzy_file_search_sessions.lock().await.clear();
    connection.pending_steering_inputs.lock().await.clear();
    connection.stop_all_child_streams().await;
    connection.stop_all_root_continuation_streams().await;
    writer.abort();
}
