use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Result;
use defra_agent::compaction::CompactionStrategy;
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::{
    ensure_agent_principal, ensure_runtime_schemas, load_agent_behavior, upsert_agent_behavior,
    AgentIdentity, BackendProviderKind, BehaviorConfig, BehaviorToolConfig, DefraAgent,
    DocumentRuntimeOptions, ProcessLifecycleObserver, ProcessLifecycleState, SimpleIdentity,
    ToolCeiling,
};
use serde_json::Value;
use tokio::sync::watch;

struct MockModelEndpoint {
    endpoint: String,
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

struct HttpRequestData {
    method: String,
    path: String,
    headers: HashMap<String, String>,
}

#[derive(Default)]
struct RecordingObserver {
    states: Mutex<Vec<ProcessLifecycleState>>,
}

impl ProcessLifecycleObserver for RecordingObserver {
    fn on_process_state_change(&self, state: ProcessLifecycleState) {
        self.states
            .lock()
            .expect("recording observer mutex poisoned")
            .push(state);
    }
}

impl MockModelEndpoint {
    fn start_with_required_bearer(
        model_name: &str,
        required_bearer: Option<&str>,
    ) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = stop.clone();
        let model_name = model_name.to_string();
        let required_bearer = required_bearer.map(ToOwned::to_owned);
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
                        let authorized = required_bearer.as_ref().is_none_or(|expected| {
                            request
                                .headers
                                .get("authorization")
                                .is_some_and(|value| value == &format!("Bearer {expected}"))
                        });
                        let (status, body) = if request.method == "GET"
                            && (request.path == "/v1/models" || request.path == "/models")
                        {
                            if authorized {
                                ("200 OK", format!(r#"{{"data":[{{"id":"{model_name}"}}]}}"#))
                            } else {
                                (
                                    "401 Unauthorized",
                                    r#"{"error":"unauthorized"}"#.to_string(),
                                )
                            }
                        } else {
                            ("404 Not Found", r#"{"error":"not found"}"#.to_string())
                        };
                        let _ = write_http_response(&mut stream, status, "application/json", &body);
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

impl Drop for MockModelEndpoint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn test_identity(name: &str) -> SimpleIdentity {
    let path = std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4()));
    SimpleIdentity::new(name, path, None)
}

fn test_behavior(
    name: &str,
    backend_id: &str,
    backend_api_key_env_var: Option<&str>,
) -> BehaviorConfig {
    BehaviorConfig {
        name: name.to_string(),
        identity: Arc::new(test_identity(name)),
        backend_id: Some(backend_id.to_string()),
        backend_provider_kind: BackendProviderKind::OpenAiCompatible,
        backend_endpoint: "http://localhost:8000/v1".to_string(),
        backend_api_key: None,
        backend_api_key_env_var: backend_api_key_env_var.map(ToOwned::to_owned),
        backend_supports_tool_calls: true,
        backend_supports_streaming: true,
        backend_supports_structured_outputs: false,
        backend_supports_json_schema: false,
        model_name: defra_agent::config::DEFAULT_MODEL_NAME.to_string(),
        context_window: defra_agent::config::DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: defra_agent::config::DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: defra_agent::config::DEFAULT_MAX_TURNS,
        system_prompt: String::new(),
        tools: BehaviorToolConfig::default(),
        compaction_threshold: defra_agent::config::DEFAULT_COMPACTION_THRESHOLD,
        compaction_strategy: CompactionStrategy::StripThenSummarize,
        stream_batch_ms: defra_agent::config::DEFAULT_STREAM_BATCH_MS,
        deadline_duration: Duration::from_secs(defra_agent::config::DEFAULT_DEADLINE_DURATION_SECS),
    }
}

async fn bind_default_behavior_backend(
    node: &EmbeddedNode,
    agent_did: &str,
    backend_id: &str,
    endpoint: &str,
) {
    let bootstrap = ensure_agent_principal(node, agent_did).await.unwrap();
    let escaped_backend_id = escape_graphql_string(backend_id);
    let escaped_endpoint = escape_graphql_string(endpoint);
    let mutation = format!(
        r#"mutation {{
            upsert_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{escaped_backend_id}" }} }},
                add: {{
                    backend_id: "{escaped_backend_id}",
                    name: "{escaped_backend_id}",
                    endpoint: "{escaped_endpoint}",
                    max_concurrent: 1,
                    enabled: true,
                    models: ["default"],
                    probe_status: "healthy"
                }},
                update: {{
                    name: "{escaped_backend_id}",
                    endpoint: "{escaped_endpoint}",
                    max_concurrent: 1,
                    enabled: true,
                    probe_status: "healthy"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let mut default_behavior = load_agent_behavior(node, &bootstrap.default_behavior.behavior_id)
        .await
        .unwrap()
        .expect("default behavior document");
    default_behavior.backend_id = Some(backend_id.to_string());
    upsert_agent_behavior(node, &default_behavior)
        .await
        .unwrap();
}

async fn wait_for_runtime_process_state(
    node: &EmbeddedNode,
    agent_did: &str,
    expected_process_state: &str,
) {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let query = format!(
            r#"{{
                AgentRuntime(filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }}, limit: 1) {{
                    process_state
                }}
            }}"#
        );
        let response = node.execute(&query).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let process_state = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRuntime"))
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("process_state"))
            .and_then(Value::as_str);
        if process_state == Some(expected_process_state) {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn read_http_request(stream: &mut TcpStream) -> anyhow::Result<HttpRequestData> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut buffer = Vec::new();
    let mut temp = [0u8; 1024];
    let header_end = loop {
        let read = stream.read(&mut temp)?;
        if read == 0 {
            anyhow::bail!("connection closed before headers");
        }
        buffer.extend_from_slice(&temp[..read]);
        if let Some(index) = find_subslice(&buffer, b"\r\n\r\n") {
            break index + 4;
        }
    };
    let header_text = String::from_utf8_lossy(&buffer[..header_end]);
    let mut lines = header_text.split("\r\n").filter(|line| !line.is_empty());
    let request_line = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing request line"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing request method"))?
        .to_string();
    let path = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing request path"))?
        .to_string();
    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect();

    Ok(HttpRequestData {
        method,
        path,
        headers,
    })
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn write_http_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> anyhow::Result<()> {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()?;
    Ok(())
}

#[test]
fn behavior_config_prefers_raw_backend_api_key() {
    let mut behavior = test_behavior("behavior-raw", "backend-raw", Some("IGNORED_ENV_KEY"));
    behavior.backend_api_key = Some("raw-key".to_string());

    unsafe {
        std::env::set_var("AGENT_DAEMON_API_KEY", "legacy-key");
        std::env::set_var("IGNORED_ENV_KEY", "env-key");
    }
    let resolved = behavior.resolve_backend_api_key().expect("resolve api key");
    unsafe {
        std::env::remove_var("AGENT_DAEMON_API_KEY");
        std::env::remove_var("IGNORED_ENV_KEY");
    }

    assert_eq!(resolved.as_deref(), Some("raw-key"));
}

#[test]
fn behavior_config_prefers_backend_specific_api_key_env_var() {
    let behavior = test_behavior(
        "behavior-a",
        "backend-a",
        Some("DEFRA_AGENT_TEST_BACKEND_KEY"),
    );

    unsafe {
        std::env::set_var("AGENT_DAEMON_API_KEY", "legacy-key");
        std::env::set_var("DEFRA_AGENT_TEST_BACKEND_KEY", "backend-key");
    }
    let resolved = behavior.resolve_backend_api_key().expect("resolve api key");
    unsafe {
        std::env::remove_var("AGENT_DAEMON_API_KEY");
        std::env::remove_var("DEFRA_AGENT_TEST_BACKEND_KEY");
    }

    assert_eq!(resolved.as_deref(), Some("backend-key"));
}

#[test]
fn behavior_config_falls_back_to_legacy_global_api_key_env_var() {
    let behavior = test_behavior("behavior-b", "backend-b", None);

    unsafe {
        std::env::set_var("AGENT_DAEMON_API_KEY", "legacy-key");
    }
    let resolved = behavior.resolve_backend_api_key().expect("resolve api key");
    unsafe {
        std::env::remove_var("AGENT_DAEMON_API_KEY");
    }

    assert_eq!(resolved.as_deref(), Some("legacy-key"));
}

#[tokio::test]
async fn run_agent_uses_backend_specific_api_key_env_var_for_startup_probe() -> Result<()> {
    let node = Arc::new(EmbeddedNode::builder().build().await?);
    ensure_runtime_schemas(node.as_ref()).await?;
    let identity = Arc::new(test_identity("startup-probe-backend-auth"));
    let mock_endpoint =
        MockModelEndpoint::start_with_required_bearer("default", Some("backend-key"))?;
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-startup-auth",
        mock_endpoint.endpoint(),
    )
    .await;

    let escaped_backend_id = escape_graphql_string("backend-startup-auth");
    let mutation = format!(
        r#"mutation {{
            update_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{escaped_backend_id}" }} }},
                input: {{ api_key_env_var: "DEFRA_AGENT_TEST_RUNTIME_BACKEND_KEY" }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    unsafe {
        std::env::remove_var("AGENT_DAEMON_API_KEY");
        std::env::set_var("DEFRA_AGENT_TEST_RUNTIME_BACKEND_KEY", "backend-key");
    }
    let observer = Arc::new(RecordingObserver::default());
    let agent = DefraAgent::from_default_behavior_documents(
        node.clone(),
        identity.clone(),
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            process_state_observer: Some(observer.clone()),
            ..Default::default()
        },
    )
    .await?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let run_task = tokio::spawn(agent.run(shutdown_rx));

    wait_for_runtime_process_state(node.as_ref(), identity.did(), "ready").await;
    let _ = shutdown_tx.send(true);
    run_task.await??;
    unsafe {
        std::env::remove_var("DEFRA_AGENT_TEST_RUNTIME_BACKEND_KEY");
    }

    let observed = observer
        .states
        .lock()
        .expect("recording observer mutex poisoned")
        .clone();
    assert_eq!(
        observed,
        vec![
            ProcessLifecycleState::Recovering,
            ProcessLifecycleState::Ready,
            ProcessLifecycleState::ShuttingDown,
            ProcessLifecycleState::Shutdown,
        ]
    );

    Ok(())
}
