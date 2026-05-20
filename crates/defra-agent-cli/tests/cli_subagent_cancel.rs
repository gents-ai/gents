mod support;
use support::*;

use std::fs;
use std::io::Write;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use defra_agent::defra_node::{EmbeddedNode, StorageBackend};
use defra_agent::{load_tool_selection, upsert_tool_selection};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subagent_cancel_cascades_to_linked_child_request() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-subagent-cancel-{}", Uuid::new_v4().simple());
    let target_prompt = format!("target cascade root {}", Uuid::new_v4().simple());
    let child_prompt = format!("child cascade leaf {}", Uuid::new_v4().simple());
    let mock_endpoint = BlockingSpawnEndpoint::start(&model_name, &target_prompt, &child_prompt)?;

    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &format!("cli-subagent-cancel-{}", Uuid::new_v4().simple()),
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let default_behavior_id = init
        .get("default_behavior_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing default_behavior_id: {init}"))?
        .to_string();
    let tool_selection_id = init
        .get("tool_selection_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing tool_selection_id: {init}"))?;
    mock_endpoint.set_behavior_id(default_behavior_id.clone());
    enable_default_subagents_before_server(&home_dir, tool_selection_id, &default_behavior_id)
        .await?;

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let submit = run_cli_json(
        &home_dir,
        &[
            "request",
            "submit",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--content",
            &target_prompt,
            "--no-wait",
        ],
    )?;
    let parent_request_id = submit
        .get("request_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("submit output missing request_id: {submit}"))?
        .to_string();
    let parent_session_id = submit
        .get("session_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("submit output missing session_id: {submit}"))?
        .to_string();

    let child_request_id =
        wait_for_spawned_child_request(&graphql, &parent_session_id, Duration::from_secs(20))
            .await?;

    let cancel = run_cli_json(
        &home_dir,
        &[
            "subagent",
            "cancel",
            &parent_request_id,
            "--graphql",
            &graphql,
            "--wait",
            "--timeout",
            "25s",
            "--output",
            "json",
        ],
    )?;
    let interrupted_ids = cancel
        .get("interrupted_request_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("cancel output missing interrupted_request_ids: {cancel}"))?
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(
        interrupted_ids.contains(&parent_request_id.as_str()),
        "cancel output should include parent request {parent_request_id}: {cancel}"
    );
    assert!(
        interrupted_ids.contains(&child_request_id.as_str()),
        "cancel output should include child request {child_request_id}: {cancel}"
    );

    assert_eq!(
        request_lifecycle_state(&graphql, &parent_request_id).await?,
        "interrupted"
    );
    assert_eq!(
        request_lifecycle_state(&graphql, &child_request_id).await?,
        "interrupted"
    );
    Ok(())
}

async fn enable_default_subagents_before_server(
    home_dir: &std::path::Path,
    selection_id: &str,
    target_behavior_id: &str,
) -> Result<()> {
    let data_dir = home_dir.join(".defra-agent").join("data");
    let node = EmbeddedNode::builder()
        .data_path(&data_dir)
        .with_storage_backend(StorageBackend::RocksDb)
        .build()
        .await
        .with_context(|| format!("opening embedded node at {}", data_dir.display()))?;
    let mut selection = load_tool_selection(&node, selection_id)
        .await?
        .ok_or_else(|| anyhow!("ToolSelection {selection_id} not found"))?;
    selection.subagent_targets = Some(vec![target_behavior_id.to_string()]);
    selection.subagent_spawn_enabled = Some(true);
    selection.subagent_background_enabled = Some(true);
    upsert_tool_selection(&node, &selection)
        .await
        .context("enable subagent tool selection")?;
    Ok(())
}

async fn wait_for_spawned_child_request(
    graphql: &str,
    parent_session_id: &str,
    timeout: Duration,
) -> Result<String> {
    let deadline = Instant::now() + timeout;
    loop {
        let response = graphql_query(
            graphql,
            &format!(
                r#"{{
                    AgentToolCall(
                        filter: {{
                            session_id: {{ _eq: "{}" }},
                            tool_name: {{ _eq: "spawn_subagent" }},
                            lifecycle_state: {{ _eq: "running" }}
                        }},
                        limit: 1
                    ) {{
                        child_request_id
                    }}
                }}"#,
                support::escape_graphql_string(parent_session_id),
            ),
        )
        .await?;
        if let Ok(row) = first_graphql_row(&response, "AgentToolCall") {
            if let Some(child_request_id) = row
                .get("child_request_id")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
            {
                return Ok(child_request_id.to_string());
            }
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for spawn_subagent bridge in session {parent_session_id}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn request_lifecycle_state(graphql: &str, request_id: &str) -> Result<String> {
    let response = graphql_query(
        graphql,
        &format!(
            r#"{{
                AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    lifecycle_state
                }}
            }}"#,
            support::escape_graphql_string(request_id),
        ),
    )
    .await?;
    let row = first_graphql_row(&response, "AgentRequest")?;
    row.get("lifecycle_state")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("AgentRequest {request_id} missing lifecycle_state: {row}"))
}

struct BlockingSpawnEndpoint {
    endpoint: String,
    port: u16,
    behavior_id: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl BlockingSpawnEndpoint {
    fn start(model_name: &str, target_prompt: &str, child_prompt: &str) -> Result<Self> {
        let listener =
            TcpListener::bind(("127.0.0.1", 0)).context("binding blocking spawn mock")?;
        listener
            .set_nonblocking(true)
            .context("marking blocking spawn mock nonblocking")?;
        let port = listener.local_addr()?.port();
        let behavior_id = Arc::new(Mutex::new("default".to_string()));
        let stop = Arc::new(AtomicBool::new(false));
        let behavior_id_for_thread = behavior_id.clone();
        let stop_for_thread = stop.clone();
        let model_name = model_name.to_string();
        let target_prompt = target_prompt.to_string();
        let child_prompt = child_prompt.to_string();

        let handle = thread::spawn(move || {
            while !stop_for_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let request = match support::mocks::read_http_request(&mut stream) {
                            Ok(request) => request,
                            Err(_) => {
                                let _ = stream.shutdown(Shutdown::Both);
                                continue;
                            }
                        };
                        match (request.method.as_str(), request.path.as_str()) {
                            ("GET", "/v1/models") => {
                                let body = format!(r#"{{"data":[{{"id":"{model_name}"}}]}}"#);
                                let _ = support::mocks::write_http_response(
                                    &mut stream,
                                    "200 OK",
                                    "application/json",
                                    &body,
                                );
                            }
                            ("POST", "/v1/chat/completions") => {
                                handle_chat_request(
                                    &mut stream,
                                    &request.body,
                                    &target_prompt,
                                    &child_prompt,
                                    &behavior_id_for_thread,
                                    &stop_for_thread,
                                );
                            }
                            _ => {
                                let _ = support::mocks::write_http_response(
                                    &mut stream,
                                    "404 Not Found",
                                    "application/json",
                                    r#"{"error":"not found"}"#,
                                );
                            }
                        }
                        let _ = stream.flush();
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
            behavior_id,
            stop,
            handle: Some(handle),
        })
    }

    fn set_behavior_id(&self, behavior_id: String) {
        *self.behavior_id.lock().expect("behavior id lock poisoned") = behavior_id;
    }

    fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

impl Drop for BlockingSpawnEndpoint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn handle_chat_request(
    stream: &mut TcpStream,
    body: &[u8],
    target_prompt: &str,
    child_prompt: &str,
    behavior_id: &Arc<Mutex<String>>,
    stop: &AtomicBool,
) {
    let request_json: Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(_) => {
            let _ = support::mocks::write_http_response(
                stream,
                "400 Bad Request",
                "application/json",
                r#"{"error":"invalid json"}"#,
            );
            return;
        }
    };

    if request_contains_role_text(&request_json, "user", target_prompt)
        && !request_has_tool_result_message(&request_json)
    {
        let behavior_id = behavior_id
            .lock()
            .expect("behavior id lock poisoned")
            .clone();
        let args = serde_json::json!({
            "behavior_id": behavior_id,
            "prompt": child_prompt,
            "await_mode": "background"
        })
        .to_string();
        let sse = tool_call_sse("spawn_subagent", &args);
        let _ = support::mocks::write_http_response(stream, "200 OK", "text/event-stream", &sse);
        return;
    }

    while !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(50));
    }
    let _ = support::mocks::write_http_response(
        stream,
        "503 Service Unavailable",
        "application/json",
        r#"{"error":"mock stopped"}"#,
    );
}
