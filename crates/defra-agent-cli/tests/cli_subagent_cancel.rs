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
use defra_agent::tool_call_lifecycle::{AwaitMode, CancelPolicy, ToolCallLifecycle};
use defra_agent::{ensure_runtime_schemas, load_tool_selection, upsert_tool_selection};
use serde_json::{json, Value};
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subagent_cancel_local_cascades_bridge_lifecycle_dispatch() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-local-subagent-cancel-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &format!("cli-subagent-cancel-local-{}", Uuid::new_v4().simple()),
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let behavior_id = init
        .get("default_behavior_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing default_behavior_id: {init}"))?
        .to_string();

    let parent_request_id = format!("parent-{}", Uuid::new_v4().simple());
    let child_request_id = format!("child-{}", Uuid::new_v4().simple());
    let grandchild_request_id = format!("grandchild-{}", Uuid::new_v4().simple());
    let parent_session_id = format!("parent-session-{}", Uuid::new_v4().simple());
    let child_session_id = format!("child-session-{}", Uuid::new_v4().simple());
    let grandchild_session_id = format!("grandchild-session-{}", Uuid::new_v4().simple());
    let parent_tool_call_id = format!("spawn-child-{}", Uuid::new_v4().simple());
    let child_tool_call_id = format!("spawn-grandchild-{}", Uuid::new_v4().simple());

    {
        let node = open_local_node(&home_dir).await?;
        ensure_runtime_schemas(node.as_ref()).await?;
        defra_agent::migration::ensure_tool_call_migrations(node.clone()).await?;
        defra_agent::migration::ensure_subagent_extensions_migrations(node.clone()).await?;

        create_local_processing_request(
            node.as_ref(),
            &parent_request_id,
            &agent_did,
            &behavior_id,
            &parent_session_id,
            0,
            None,
        )
        .await?;
        create_local_processing_request(
            node.as_ref(),
            &child_request_id,
            &agent_did,
            &behavior_id,
            &child_session_id,
            1,
            Some((&parent_request_id, &parent_tool_call_id)),
        )
        .await?;
        create_local_processing_request(
            node.as_ref(),
            &grandchild_request_id,
            &agent_did,
            &behavior_id,
            &grandchild_session_id,
            2,
            Some((&child_request_id, &child_tool_call_id)),
        )
        .await?;

        create_running_subagent_bridge(
            node.clone(),
            &parent_request_id,
            &parent_session_id,
            &parent_tool_call_id,
            &child_request_id,
            AwaitMode::Foreground,
        )
        .await?;
        create_running_subagent_bridge(
            node.clone(),
            &child_request_id,
            &child_session_id,
            &child_tool_call_id,
            &grandchild_request_id,
            AwaitMode::Background,
        )
        .await?;
    }

    let cancel = run_cli_json(
        &home_dir,
        &[
            "subagent",
            "cancel",
            &child_request_id,
            "--cause",
            "deadline",
            "--output",
            "json",
        ],
    )?;
    assert_eq!(
        cancel.get("cause").and_then(Value::as_str),
        Some("deadline")
    );
    let interrupted_ids = cancel
        .get("interrupted_request_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("cancel output missing interrupted_request_ids: {cancel}"))?
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(
        interrupted_ids.contains(&child_request_id.as_str()),
        "local cancel output should include target child request {child_request_id}: {cancel}"
    );
    assert!(
        interrupted_ids.contains(&grandchild_request_id.as_str()),
        "local cancel output should include cascaded grandchild request {grandchild_request_id}: {cancel}"
    );
    assert!(
        !interrupted_ids.contains(&parent_request_id.as_str()),
        "local cancel should cancel the parent bridge but not interrupt the parent request: {cancel}"
    );

    let node = open_local_node(&home_dir).await?;
    assert!(
        local_request_interrupt_requested_at(node.as_ref(), &child_request_id)
            .await?
            .is_some(),
        "target child request should be interrupted"
    );
    assert!(
        local_request_interrupt_requested_at(node.as_ref(), &grandchild_request_id)
            .await?
            .is_some(),
        "cascaded grandchild request should be interrupted"
    );
    assert!(
        local_request_interrupt_requested_at(node.as_ref(), &parent_request_id)
            .await?
            .is_none(),
        "parent request should not be interrupted when cancelling the child subagent"
    );
    assert_eq!(
        local_tool_lifecycle_state(node.as_ref(), &parent_session_id, &parent_tool_call_id).await?,
        "cancelled"
    );
    assert_eq!(
        local_tool_lifecycle_state(node.as_ref(), &child_session_id, &child_tool_call_id).await?,
        "cancelled"
    );
    assert_eq!(
        local_tool_cancel_cause(node.as_ref(), &parent_session_id, &parent_tool_call_id).await?,
        "deadline"
    );
    assert_eq!(
        local_tool_cancel_cause(node.as_ref(), &child_session_id, &child_tool_call_id).await?,
        "deadline"
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

async fn open_local_node(home_dir: &std::path::Path) -> Result<Arc<EmbeddedNode>> {
    let data_dir = home_dir.join(".defra-agent").join("data");
    Ok(Arc::new(
        EmbeddedNode::builder()
            .data_path(&data_dir)
            .with_storage_backend(StorageBackend::RocksDb)
            .build()
            .await
            .with_context(|| format!("opening embedded node at {}", data_dir.display()))?,
    ))
}

async fn create_local_processing_request(
    node: &EmbeddedNode,
    request_id: &str,
    agent_did: &str,
    behavior_id: &str,
    session_id: &str,
    subagent_depth: u32,
    parent_link: Option<(&str, &str)>,
) -> Result<()> {
    let created_at = chrono::Utc::now().to_rfc3339();
    let execution_origin = if parent_link.is_some() {
        "subagent"
    } else {
        "interactive"
    };
    let parent_fields = parent_link
        .map(|(parent_request_id, parent_tool_call_id)| {
            format!(
                r#",
                caused_by_parent_request_id: "{}",
                caused_by_parent_tool_call_id: "{}""#,
                support::escape_graphql_string(parent_request_id),
                support::escape_graphql_string(parent_tool_call_id),
            )
        })
        .unwrap_or_default();
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "{agent_did}",
                behavior_id: "{behavior_id}",
                session_id: "{session_id}",
                retry_parent_request: "",
                retry_root_request: "{request_id}",
                superseded_by_request: "",
                content: "local subagent cancel fixture",
                metadata: "",
                status: "processing",
                lifecycle_state: "processing",
                backend_id: "",
                execution_origin: "{execution_origin}",
                failure_reason: "",
                created_at: "{created_at}",
                retry_count: 0,
                max_retries: 0,
                subagent_depth: {subagent_depth}{parent_fields}
            }}) {{ _docID }}
        }}"#,
        request_id = support::escape_graphql_string(request_id),
        agent_did = support::escape_graphql_string(agent_did),
        behavior_id = support::escape_graphql_string(behavior_id),
        session_id = support::escape_graphql_string(session_id),
        created_at = support::escape_graphql_string(&created_at),
    );
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        bail!("create local AgentRequest failed: {:?}", response.errors);
    }
    Ok(())
}

async fn create_running_subagent_bridge(
    node: Arc<EmbeddedNode>,
    request_id: &str,
    session_id: &str,
    tool_call_id: &str,
    child_request_id: &str,
    await_mode: AwaitMode,
) -> Result<()> {
    let mut lifecycle = ToolCallLifecycle::new_subagent(
        node,
        request_id.to_string(),
        session_id.to_string(),
        tool_call_id.to_string(),
        1,
        "spawn_subagent".to_string(),
        "{}".to_string(),
        chrono::Utc::now() + chrono::Duration::minutes(5),
        await_mode,
        CancelPolicy::Cascade,
        child_request_id.to_string(),
    );
    lifecycle.start_running().await
}

async fn local_request_interrupt_requested_at(
    node: &EmbeddedNode,
    request_id: &str,
) -> Result<Option<String>> {
    let response = local_query(
        node,
        &format!(
            r#"{{
                AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    interrupt_requested_at
                }}
            }}"#,
            support::escape_graphql_string(request_id),
        ),
    )
    .await?;
    let row = first_graphql_row(&response, "AgentRequest")?;
    Ok(row
        .get("interrupt_requested_at")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned))
}

async fn local_tool_lifecycle_state(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> Result<String> {
    let response = local_query(
        node,
        &format!(
            r#"{{
                AgentToolCall(
                    filter: {{
                        session_id: {{ _eq: "{}" }},
                        tool_call_id: {{ _eq: "{}" }}
                    }},
                    limit: 1
                ) {{
                    lifecycle_state
                }}
            }}"#,
            support::escape_graphql_string(session_id),
            support::escape_graphql_string(tool_call_id),
        ),
    )
    .await?;
    let row = first_graphql_row(&response, "AgentToolCall")?;
    row.get("lifecycle_state")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("AgentToolCall {session_id}:{tool_call_id} missing state: {row}"))
}

async fn local_tool_cancel_cause(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> Result<String> {
    let response = local_query(
        node,
        &format!(
            r#"{{
                AgentToolCall(
                    filter: {{
                        session_id: {{ _eq: "{}" }},
                        tool_call_id: {{ _eq: "{}" }}
                    }},
                    limit: 1
                ) {{
                    cancel_cause
                }}
            }}"#,
            support::escape_graphql_string(session_id),
            support::escape_graphql_string(tool_call_id),
        ),
    )
    .await?;
    let row = first_graphql_row(&response, "AgentToolCall")?;
    row.get("cancel_cause")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("AgentToolCall {session_id}:{tool_call_id} missing cause: {row}"))
}

async fn local_query(node: &EmbeddedNode, query: &str) -> Result<Value> {
    let response = node.execute(query).await;
    if response.has_errors() {
        bail!("local GraphQL query failed: {:?}", response.errors);
    }
    Ok(json!({
        "data": response.data.unwrap_or(Value::Null),
    }))
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
