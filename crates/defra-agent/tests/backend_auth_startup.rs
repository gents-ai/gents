mod support;

use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Result;
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::{
    ensure_runtime_schemas, AgentIdentity, DefraAgent, DocumentRuntimeOptions,
    ProcessLifecycleObserver, ProcessLifecycleState, ToolCeiling,
};
use serde_json::Value;
use tokio::sync::watch;

use support::fixtures::{bind_default_behavior_backend, test_behavior, test_identity};
use support::http_mock::{read_http_request, write_http_response, HttpRequestData};
use support::waits::wait_for_runtime_process_state;

struct MockModelEndpoint {
    endpoint: String,
    port: u16,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<HttpRequestData>>>,
    handle: Option<JoinHandle<()>>,
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
        let requests = Arc::new(Mutex::new(Vec::new()));
        let requests_for_thread = requests.clone();
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
                        requests_for_thread
                            .lock()
                            .expect("mock request log mutex poisoned")
                            .push(request.clone());
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
                        } else if request.method == "GET"
                            && (request.path == "/v1/key" || request.path == "/key")
                        {
                            if authorized {
                                ("200 OK", r#"{"data":{"label":"test-key"}}"#.to_string())
                            } else {
                                (
                                    "401 Unauthorized",
                                    r#"{"error":"unauthorized"}"#.to_string(),
                                )
                            }
                        } else if request.method == "POST"
                            && (request.path == "/v1/chat/completions"
                                || request.path == "/chat/completions")
                        {
                            if authorized {
                                (
                                    "200 OK",
                                    format!(
                                        r#"{{
                                            "id":"chatcmpl-test",
                                            "provider":"Mock",
                                            "object":"chat.completion",
                                            "created":1710000000,
                                            "model":"{model_name}",
                                            "choices":[{{
                                                "index":0,
                                                "finish_reason":"stop",
                                                "message":{{
                                                    "role":"assistant",
                                                    "content":"mock response",
                                                    "refusal":null,
                                                    "reasoning":null
                                                }}
                                            }}],
                                            "usage":{{
                                                "prompt_tokens":10,
                                                "completion_tokens":2,
                                                "total_tokens":12
                                            }}
                                        }}"#
                                    ),
                                )
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
            requests,
            handle: Some(handle),
        })
    }

    fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn recorded_requests(&self) -> Vec<HttpRequestData> {
        self.requests
            .lock()
            .expect("mock request log mutex poisoned")
            .clone()
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

#[tokio::test]
async fn run_agent_uses_backend_specific_api_key_env_var_for_startup_probe() -> Result<()> {
    use std::ffi::OsString;
    use std::sync::LazyLock;

    static ENV_VAR_LOCK: LazyLock<tokio::sync::Mutex<()>> =
        LazyLock::new(|| tokio::sync::Mutex::new(()));

    struct TestEnvGuard {
        saved: Vec<(&'static str, Option<OsString>)>,
    }
    impl TestEnvGuard {
        fn new(names: &[&'static str]) -> Self {
            let saved = names
                .iter()
                .map(|name| (*name, std::env::var_os(name)))
                .collect();
            Self { saved }
        }
        fn set(&mut self, name: &'static str, value: &str) {
            unsafe {
                std::env::set_var(name, value);
            }
        }
    }
    impl Drop for TestEnvGuard {
        fn drop(&mut self) {
            for (name, value) in self.saved.iter().rev() {
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
    }

    let _env_guard = ENV_VAR_LOCK.lock().await;
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

    let mut env = TestEnvGuard::new(&["DEFRA_AGENT_TEST_RUNTIME_BACKEND_KEY"]);
    env.set("DEFRA_AGENT_TEST_RUNTIME_BACKEND_KEY", "backend-key");
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

#[tokio::test]
async fn openrouter_oneshot_uses_provider_request_preferences() -> Result<()> {
    use defra_agent::BackendProviderKind;

    let node = Arc::new(EmbeddedNode::builder().build().await?);
    ensure_runtime_schemas(node.as_ref()).await?;
    let mock_endpoint = MockModelEndpoint::start_with_required_bearer(
        "openai/gpt-4o-mini",
        Some("openrouter-key"),
    )?;
    let mut behavior = test_behavior("openrouter-oneshot", "backend-openrouter", None);
    behavior.backend_provider_kind = BackendProviderKind::OpenRouter;
    behavior.backend_endpoint = mock_endpoint.endpoint().to_string();
    behavior.backend_api_key = Some("openrouter-key".to_string());
    behavior.model_name = "openai/gpt-4o-mini".to_string();

    let result =
        defra_agent::run_openai_oneshot(node, &behavior, "Say hello in one sentence.").await?;
    assert_eq!(result.response_text, "mock response");

    let completion_request = mock_endpoint
        .recorded_requests()
        .into_iter()
        .find(|request| request.method == "POST" && request.path.ends_with("/chat/completions"))
        .expect("completion request should be recorded");
    let body: Value = serde_json::from_str(&completion_request.body)?;

    assert_eq!(body["provider"]["require_parameters"], true);
    assert_eq!(body["model"], "openai/gpt-4o-mini");

    Ok(())
}
