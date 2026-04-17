pub(crate) struct MockModelEndpoint {
    endpoint: String,
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

#[derive(Clone)]
pub(crate) enum MockModelMode {
    Text,
    ToolLoop { final_text: String },
}

impl MockModelEndpoint {
    fn start(model_name: &str) -> Result<Self> {
        Self::start_with_mode(model_name, MockModelMode::Text)
    }

    fn start_tool_loop(model_name: &str, final_text: impl Into<String>) -> Result<Self> {
        Self::start_with_mode(
            model_name,
            MockModelMode::ToolLoop {
                final_text: final_text.into(),
            },
        )
    }

    fn start_with_mode(model_name: &str, mode: MockModelMode) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let model_name = model_name.to_string();
        let mode_for_thread = mode.clone();
        let handle = thread::spawn(move || {
            while !stop_for_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let request = match read_http_request(&mut stream) {
                            Ok(request) => request,
                            Err(_) => {
                                let _ = stream.shutdown(Shutdown::Both);
                                continue;
                            }
                        };
                        let (status, content_type, body) = if request.method == "GET"
                            && (request.path == "/v1/models" || request.path == "/models")
                        {
                            (
                                "200 OK",
                                "application/json",
                                format!(r#"{{"data":[{{"id":"{model_name}"}}]}}"#),
                            )
                        } else if request.method == "POST"
                            && (request.path == "/v1/chat/completions"
                                || request.path == "/chat/completions")
                        {
                            let body = match &mode_for_thread {
                                MockModelMode::Text => mock_completion_sse("mock response"),
                                MockModelMode::ToolLoop { final_text } => {
                                    if request_has_tool_result_message(&request.body) {
                                        let text = extract_desktop_tool_token(&request.body)
                                            .unwrap_or_else(|| final_text.clone());
                                        mock_completion_sse(&text)
                                    } else {
                                        mock_tool_call_sse("read_file", r#"{"path":"notes.txt"}"#)
                                    }
                                }
                            };
                            ("200 OK", "text/event-stream", body)
                        } else {
                            (
                                "404 Not Found",
                                "application/json",
                                r#"{"error":"not found"}"#.to_string(),
                            )
                        };
                        let _ = write_http_response(&mut stream, status, content_type, &body);
                        let _ = stream.shutdown(Shutdown::Both);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            endpoint: format!("http://127.0.0.1:{port}/v1"),
            port,
            stop,
            handle: Some(handle),
        })
    }

    fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

pub(crate) fn request_has_tool_result_message(body: &str) -> bool {
    body.contains(r#""role":"tool""#) || body.contains(r#""role": "tool""#)
}

pub(crate) fn extract_desktop_tool_token(body: &str) -> Option<String> {
    let marker = "DESKTOP_TOOL_TOKEN_";
    let start = body.find(marker)?;
    let token = body[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect::<String>();
    (!token.is_empty()).then_some(token)
}

pub(crate) fn mock_tool_call_sse(tool_name: &str, arguments: &str) -> String {
    let chunk_1 = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call-read-file",
                    "function": {
                        "name": tool_name,
                        "arguments": ""
                    }
                }]
            },
            "finish_reason": null
        }],
        "usage": null
    });
    let chunk_2 = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": null,
                    "function": {
                        "name": null,
                        "arguments": arguments
                    }
                }]
            },
            "finish_reason": null
        }],
        "usage": null
    });
    let chunk_3 = serde_json::json!({
        "choices": [{
            "delta": {
                "content": null,
                "tool_calls": []
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 16,
            "completion_tokens": 4,
            "total_tokens": 20
        }
    });
    format!(
        "data: {}\n\ndata: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        serde_json::to_string(&chunk_1).expect("serialize mock tool chunk 1"),
        serde_json::to_string(&chunk_2).expect("serialize mock tool chunk 2"),
        serde_json::to_string(&chunk_3).expect("serialize mock tool chunk 3"),
    )
}

pub(crate) fn mock_completion_sse(text: &str) -> String {
    let chunk_1 = serde_json::json!({
        "choices": [{
            "delta": {
                "content": text
            },
            "finish_reason": null
        }],
        "usage": null
    });
    let chunk_2 = serde_json::json!({
        "choices": [{
            "delta": {
                "content": null,
                "tool_calls": []
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 24,
            "completion_tokens": 6,
            "total_tokens": 30
        }
    });
    format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        serde_json::to_string(&chunk_1).expect("serialize mock completion chunk 1"),
        serde_json::to_string(&chunk_2).expect("serialize mock completion chunk 2"),
    )
}

impl Drop for MockModelEndpoint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub(crate) struct RunningAgent {
    did: String,
    tool_token: String,
    tool_root: std::path::PathBuf,
    shutdown_tx: watch::Sender<bool>,
    run_task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl RunningAgent {
    fn write_tool_file(&self, relative_path: &str, contents: &str) -> Result<()> {
        let path = self.tool_root.join(relative_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating live tool directory {}", parent.display()))?;
        }
        std::fs::write(&path, format!("{contents}\n"))
            .with_context(|| format!("writing live tool fixture {}", path.display()))?;
        Ok(())
    }

    async fn shutdown(self) -> Result<()> {
        let _ = self.shutdown_tx.send(true);
        self.run_task.await??;
        Ok(())
    }
}

#[derive(Debug)]
#[derive(Clone)]
pub(crate) struct AgentBackendConfig {
    endpoint: String,
    model_name: String,
    provider_kind: BackendProviderKind,
    api_key: Option<String>,
    api_key_env_var: Option<String>,
}

impl AgentBackendConfig {
    fn mock(endpoint: &str) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            model_name: "default".to_string(),
            provider_kind: BackendProviderKind::OpenAiCompatible,
            api_key: None,
            api_key_env_var: None,
        }
    }

    fn live_from_env() -> Result<Self> {
        let endpoint = optional_env("DEFRA_AGENT_DESKTOP_LIVE_BACKEND_ENDPOINT");
        let model_name = optional_env("DEFRA_AGENT_DESKTOP_LIVE_BACKEND_MODEL")
            .or_else(|| optional_env("DEFRA_AGENT_TEST_OPENROUTER_MODEL"))
            .unwrap_or_else(|| "openai/gpt-4o-mini".to_string());
        let provider_kind = optional_env("DEFRA_AGENT_DESKTOP_LIVE_BACKEND_PROVIDER");
        let api_key = optional_env("DEFRA_AGENT_DESKTOP_LIVE_BACKEND_API_KEY");
        let api_key_env_var = optional_env("DEFRA_AGENT_DESKTOP_LIVE_BACKEND_API_KEY_ENV_VAR");

        if endpoint.is_some()
            || provider_kind.is_some()
            || api_key.is_some()
            || api_key_env_var.is_some()
        {
            if let Some(env_var_name) = api_key_env_var.as_deref() {
                std::env::var(env_var_name).with_context(|| {
                    format!(
                        "set {env_var_name} because DEFRA_AGENT_DESKTOP_LIVE_BACKEND_API_KEY_ENV_VAR points at it"
                    )
                })?;
            }

            return Ok(Self {
                endpoint: endpoint.context(
                    "set DEFRA_AGENT_DESKTOP_LIVE_BACKEND_ENDPOINT for the live desktop smoke test",
                )?,
                model_name,
                provider_kind: BackendProviderKind::parse_optional(provider_kind.as_deref())?,
                api_key,
                api_key_env_var,
            });
        }

        if std::env::var("OPENROUTER_API_KEY").is_ok() {
            return Ok(Self {
                endpoint: "https://openrouter.ai/api/v1".to_string(),
                model_name,
                provider_kind: BackendProviderKind::OpenRouter,
                api_key: None,
                api_key_env_var: Some("OPENROUTER_API_KEY".to_string()),
            });
        }

        anyhow::bail!(
            "set DEFRA_AGENT_DESKTOP_LIVE_BACKEND_ENDPOINT or OPENROUTER_API_KEY to run the live desktop smoke test"
        );
    }
}

pub(crate) fn test_runtime() -> Result<Arc<Runtime>> {
    Ok(Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(4)
            .build()?,
    ))
}

pub(crate) fn shutdown_core(runtime: &Runtime, core: ClientCore) -> Result<()> {
    runtime.block_on(core.shutdown())
}

pub(crate) async fn spawn_backed_agent(
    node: Arc<EmbeddedNode>,
    key_path: impl Into<std::path::PathBuf>,
    name: &str,
    backend: &AgentBackendConfig,
) -> Result<RunningAgent> {
    let key_path = key_path.into();
    let tool_root = key_path
        .parent()
        .map(|parent| parent.join("tool-root"))
        .unwrap_or_else(|| std::env::temp_dir().join(format!("defra-agent-tools-{name}")));
    std::fs::create_dir_all(&tool_root)
        .with_context(|| format!("creating live tool root {}", tool_root.display()))?;
    let tool_token = format!("DESKTOP_TOOL_TOKEN_{}", uuid::Uuid::new_v4().simple());
    std::fs::write(tool_root.join("notes.txt"), format!("{tool_token}\n")).with_context(|| {
        format!(
            "writing live tool fixture {}",
            tool_root.join("notes.txt").display()
        )
    })?;

    let identity = Arc::new(SimpleIdentity::new(name, key_path, None));
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        &format!("{name}-backend"),
        backend,
    )
    .await?;
    let did = identity.did().to_string();
    let agent = DefraAgent::from_default_behavior_documents(
        Arc::clone(&node),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::readwrite(tool_root.clone()),
            ..Default::default()
        },
    )
    .await?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let run_task = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_process_state(node.as_ref(), &did, "ready").await?;
    Ok(RunningAgent {
        did,
        tool_token,
        tool_root,
        shutdown_tx,
        run_task,
    })
}

pub(crate) async fn bind_default_behavior_backend(
    node: &EmbeddedNode,
    agent_did: &str,
    backend_id: &str,
    backend: &AgentBackendConfig,
) -> Result<()> {
    let bootstrap = ensure_agent_principal(node, agent_did).await?;
    let escaped_backend_id = escape_graphql_string(backend_id);
    let escaped_endpoint = escape_graphql_string(&backend.endpoint);
    let escaped_provider_kind = escape_graphql_string(backend.provider_kind.as_str());
    let escaped_model_name = escape_graphql_string(&backend.model_name);
    let api_key_field = graphql_optional_string_field("api_key", backend.api_key.as_deref());
    let api_key_env_var_field =
        graphql_optional_string_field("api_key_env_var", backend.api_key_env_var.as_deref());
    let mutation = format!(
        r#"mutation {{
            upsert_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{escaped_backend_id}" }} }},
                add: {{
                    backend_id: "{escaped_backend_id}",
                    name: "{escaped_backend_id}",
                    provider_kind: "{escaped_provider_kind}",
                    endpoint: "{escaped_endpoint}",
                    {api_key_field}
                    {api_key_env_var_field}
                    max_concurrent: 2,
                    max_queue_depth: 100,
                    enabled: true,
                    models: ["{escaped_model_name}"],
                    probe_status: "healthy"
                }},
                update: {{
                    name: "{escaped_backend_id}",
                    provider_kind: "{escaped_provider_kind}",
                    endpoint: "{escaped_endpoint}",
                    {api_key_field}
                    {api_key_env_var_field}
                    max_concurrent: 2,
                    max_queue_depth: 100,
                    enabled: true,
                    models: ["{escaped_model_name}"],
                    probe_status: "healthy"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        anyhow::bail!("upsert inference backend failed: {:?}", response.errors);
    }

    let mut default_behavior = load_agent_behavior(node, &bootstrap.default_behavior.behavior_id)
        .await?
        .expect("default behavior document");
    default_behavior.backend_id = Some(backend_id.to_string());
    default_behavior.model_name = Some(backend.model_name.clone());
    upsert_agent_behavior(node, &default_behavior).await?;
    Ok(())
}

pub(crate) async fn wait_for_runtime_process_state(
    node: &EmbeddedNode,
    agent_did: &str,
    expected_process_state: &str,
) -> Result<()> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let query = format!(
            r#"{{
                AgentRuntime(
                    filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }},
                    limit: 1
                ) {{
                    process_state
                }}
            }}"#
        );
        let response = node.execute(&query).await;
        if response.has_errors() {
            anyhow::bail!("AgentRuntime query failed: {:?}", response.errors);
        }
        let process_state = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRuntime"))
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("process_state"))
            .and_then(Value::as_str);
        if process_state == Some(expected_process_state) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "timed out waiting for AgentRuntime {agent_did} to reach process_state={expected_process_state}; last={process_state:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
