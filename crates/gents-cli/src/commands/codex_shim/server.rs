use super::*;

#[derive(Clone)]
pub(crate) struct CodexShimBindArgs {
    pub(crate) home: PathBuf,
    pub(crate) fs_root: Option<PathBuf>,
    pub(crate) node: Arc<EmbeddedNode>,
    pub(crate) background_execution_registry: gents::BackgroundExecutionRegistry,
    pub(crate) graphql: String,
    pub(crate) agent_did: String,
    pub(crate) behavior_id: Option<String>,
    pub(crate) auth_token: Option<String>,
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
    agent_did: String,
    behavior_id: String,
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

    pub(crate) fn agent_did(&self) -> &str {
        &self.agent_did
    }

    pub(crate) fn behavior_id(&self) -> &str {
        &self.behavior_id
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

pub(crate) async fn resolve_codex_shim_behavior_id(
    node: &EmbeddedNode,
    override_behavior_id: Option<&str>,
    agent_did: &str,
) -> Result<String> {
    bound_behavior::resolve_bound_behavior_id(node, override_behavior_id, agent_did).await
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

    let codex_home = args.home.join("codex-ui");
    let codex_log_dir = codex_home.join("log");
    fs::create_dir_all(&codex_log_dir)
        .with_context(|| format!("creating Codex UI log dir {}", codex_log_dir.display()))
        .map_err(CodexShimBindError::HostResource)?;
    let trace_path = codex_log_dir.join("codex-shim-events.jsonl");
    let agent_did = args.agent_did.clone();
    let auth_required = args.auth_token.is_some();

    let bound_behavior_id = bound_behavior::resolve_bound_behavior_id(
        args.node.as_ref(),
        args.behavior_id.as_deref(),
        &args.agent_did,
    )
    .await
    .map_err(CodexShimBindError::DependencyMissing)?;
    bound_behavior::load_bound_inference_profile_id(args.node.as_ref(), &bound_behavior_id)
        .await
        .with_context(|| format!("validating Codex shim bound behavior {bound_behavior_id:?}"))
        .map_err(CodexShimBindError::DependencyMissing)?;

    let state = ShimState {
        codex_home: codex_home.clone(),
        trace_path: trace_path.clone(),
        cwd: std::env::current_dir()
            .context("resolving current working directory")
            .map_err(CodexShimBindError::HostResource)?,
        fs_root: args.fs_root,
        node: args.node,
        background_execution_registry: args.background_execution_registry,
        graphql: Arc::from(args.graphql.clone()),
        agent_did: Arc::from(args.agent_did.clone()),
        behavior_id: Arc::from(bound_behavior_id.clone()),
        id_counter: Arc::new(AtomicU64::new(1)),
        timeout: Duration::from_secs(args.timeout_secs),
        poll_interval: Duration::from_millis(args.poll_ms.max(1)),
        sidecar: Arc::new(Mutex::new(CodexSidecar::default())),
        auth_token: args.auth_token.map(Arc::from),
    };

    let app = Router::new()
        .route("/", get(ws_upgrade))
        .with_state(state.clone());
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
        agent_did,
        behavior_id: bound_behavior_id,
        auth_required,
    })
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
