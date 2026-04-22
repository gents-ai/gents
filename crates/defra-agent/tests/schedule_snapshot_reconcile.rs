//! Event-driven reconcile coverage for Task + Schedule inserts.
//!
//! # Expected status vs the plan
//!
//! This test is written at the end of Task 20 and is **expected to FAIL**
//! until Task 21 lands: the control watcher only subscribes to
//! AgentPrincipal / AgentBehavior / ToolSelectionDocument / InferenceProfile /
//! InferenceBackend updates today. Task + Schedule update events hit
//! `apply_control_update` and return `ControlUpdateOutcome::Irrelevant`, so
//! the snapshot is never re-resolved and `active_generation` stays flat.
//!
//! Task 21 extends `apply_control_update` (and the event subscription) to
//! recognize Task and Schedule updates — at which point inserting a Task +
//! Schedule after startup drives the debounce + resolve + activate pipeline,
//! which bumps `active_generation` and grows `active_schedules`.
//!
//! # What this test actually asserts
//!
//! Integration tests live outside the `defra-agent` crate, so they cannot
//! observe `ActiveRuntimeSnapshot::active_schedules()` directly (that
//! accessor is `pub(crate)`). The public observable is the `AgentRuntime`
//! doc's `active_generation` field, which bumps 1:1 with every activated
//! snapshot. The in-crate counterpart to `active_schedules.len() == 1`
//! already lives in `src/agent/document_view/tests.rs`
//! (`resolve_returns_active_schedule_for_enabled_task_and_schedule`); the
//! new thing Task 20 / 21 validate is the **event-driven reload path**,
//! which is exactly what `active_generation` observes.

use std::sync::Arc;
use std::time::Duration;
use std::{
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    thread::JoinHandle,
};

use defra_agent::{
    ensure_agent_principal, graphql::escape_graphql_string, load_agent_behavior,
    upsert_agent_behavior, AgentIdentity, DefraAgent, DocumentRuntimeOptions, SimpleIdentity,
    ToolCeiling,
};

mod support;

use support::snapshots::{fetch_runtime_snapshot, RuntimeSnapshot};
use support::test_db;

fn test_identity(name: &str) -> SimpleIdentity {
    let path = std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4()));
    SimpleIdentity::new(name, path, None)
}

struct MockModelEndpoint {
    endpoint: String,
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl MockModelEndpoint {
    fn start(model_name: &str) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = stop.clone();
        let model_name = model_name.to_string();
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
                        let (status, body) = if request.method == "GET"
                            && (request.path == "/v1/models" || request.path == "/models")
                        {
                            ("200 OK", format!(r#"{{"data":[{{"id":"{model_name}"}}]}}"#))
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

struct HttpRequestData {
    method: String,
    path: String,
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
    Ok(HttpRequestData { method, path })
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

async fn bind_default_behavior_backend(
    node: &defra_agent::defra_node::EmbeddedNode,
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
    assert!(
        !response.has_errors(),
        "upsert InferenceBackend failed: {:?}",
        response.errors
    );

    let mut default_behavior = load_agent_behavior(node, &bootstrap.default_behavior.behavior_id)
        .await
        .unwrap()
        .expect("default behavior document");
    default_behavior.backend_id = Some(backend_id.to_string());
    upsert_agent_behavior(node, &default_behavior)
        .await
        .unwrap();
}

async fn create_task(
    node: &defra_agent::defra_node::EmbeddedNode,
    task_id: &str,
    behavior_id: &str,
    prompt_template: &str,
) {
    let escaped_task_id = escape_graphql_string(task_id);
    let escaped_behavior_id = escape_graphql_string(behavior_id);
    let escaped_prompt_template = escape_graphql_string(prompt_template);
    let mutation = format!(
        r#"mutation {{
            create_Task(input: {{
                task_id: "{escaped_task_id}",
                name: "{escaped_task_id}",
                behavior_id: "{escaped_behavior_id}",
                prompt_template: "{escaped_prompt_template}",
                enabled: true
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create Task failed: {:?}",
        response.errors
    );
}

async fn create_schedule(
    node: &defra_agent::defra_node::EmbeddedNode,
    schedule_id: &str,
    task_id: &str,
) {
    let escaped_schedule_id = escape_graphql_string(schedule_id);
    let escaped_task_id = escape_graphql_string(task_id);
    let mutation = format!(
        r#"mutation {{
            create_Schedule(input: {{
                schedule_id: "{escaped_schedule_id}",
                task_id: "{escaped_task_id}",
                interval_secs: 60,
                enabled: true,
                concurrency: "serial"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create Schedule failed: {:?}",
        response.errors
    );
}

async fn create_event_trigger(
    node: &defra_agent::defra_node::EmbeddedNode,
    trigger_id: &str,
    task_id: &str,
    source_collection: &str,
    event_kind: &str,
) {
    let escaped_trigger_id = escape_graphql_string(trigger_id);
    let escaped_task_id = escape_graphql_string(task_id);
    let escaped_source_collection = escape_graphql_string(source_collection);
    let escaped_event_kind = escape_graphql_string(event_kind);
    let mutation = format!(
        r#"mutation {{
            create_EventTrigger(input: {{
                trigger_id: "{escaped_trigger_id}",
                task_id: "{escaped_task_id}",
                source_collection: "{escaped_source_collection}",
                event_kind: "{escaped_event_kind}",
                enabled: true,
                concurrency: "serial",
                fire_count: 0
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create EventTrigger failed: {:?}",
        response.errors
    );
}

async fn wait_for_runtime_snapshot<F>(
    node: &defra_agent::defra_node::EmbeddedNode,
    agent_did: &str,
    predicate: F,
) -> RuntimeSnapshot
where
    F: Fn(&RuntimeSnapshot) -> bool,
{
    // Generous deadline: control watcher debounce is 5s plus a settle window.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(snapshot) = fetch_runtime_snapshot(node, agent_did).await {
            if predicate(&snapshot) {
                return snapshot;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for runtime snapshot for {agent_did}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Task + Schedule inserts after startup should drive a snapshot reload and
/// bump `active_generation`. Fails until Task 21 wires `apply_control_update`
/// for Task / Schedule events.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schedule_insert_bumps_active_generation() {
    let db = test_db("schedule-snapshot-reconcile").await;
    let identity = Arc::new(test_identity("schedule-snapshot-reconcile"));
    let mock_endpoint = MockModelEndpoint::start("default").unwrap();
    bind_default_behavior_backend(
        db.node.as_ref(),
        identity.did(),
        "backend-schedule-snapshot-reconcile",
        mock_endpoint.endpoint(),
    )
    .await;
    let agent = DefraAgent::from_default_behavior_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let agent_did = agent.agent_did().to_string();
    let default_behavior_id = agent.default_behavior_id().to_string();

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));

    // Capture the baseline snapshot generation after startup. This is the
    // `generation` value that a post-startup Task + Schedule insert must
    // exceed if the event-driven reload is wired.
    let startup = wait_for_runtime_snapshot(db.node.as_ref(), &agent_did, |snapshot| {
        snapshot.process_state == "ready"
            && snapshot.reconcile_phase == "idle"
            && snapshot.active_generation >= 1
            && snapshot.last_reconcile_result == "startup"
    })
    .await;
    let initial_generation = startup.active_generation;
    assert!(
        startup.last_reconcile_error.is_empty(),
        "startup reconcile should be clean, got error={:?}",
        startup.last_reconcile_error
    );

    // Insert one Task + one Schedule post-startup. The Task binds to the
    // default behavior so the Schedule resolves as *active* (not unavailable)
    // once the control watcher triggers a reload.
    create_task(
        db.node.as_ref(),
        "task-reconcile-alpha",
        &default_behavior_id,
        "alpha prompt",
    )
    .await;
    create_schedule(
        db.node.as_ref(),
        "schedule-reconcile-alpha",
        "task-reconcile-alpha",
    )
    .await;

    // Expected verdict: FAIL today (Task 20) because `apply_control_update`
    // treats Task / Schedule as `Irrelevant` — no debounce, no reload, no
    // generation bump. PASS after Task 21 wires the Task / Schedule update
    // path, at which point `active_generation` bumps to `initial + 1`.
    let reconciled = wait_for_runtime_snapshot(db.node.as_ref(), &agent_did, |snapshot| {
        snapshot.process_state == "ready"
            && snapshot.reconcile_phase == "idle"
            && snapshot.active_generation > initial_generation
            && snapshot.last_reconcile_result == "applied"
    })
    .await;
    assert_eq!(reconciled.default_behavior_id, default_behavior_id);
    assert!(
        reconciled.last_reconcile_error.is_empty(),
        "post-insert reconcile should be clean, got error={:?}",
        reconciled.last_reconcile_error
    );
    assert!(
        reconciled.active_generation > initial_generation,
        "active_generation should bump after Task+Schedule insert (initial={initial_generation}, observed={})",
        reconciled.active_generation
    );

    let _ = shutdown_tx.send(true);
    handle.await.unwrap().unwrap();
}

// This test exercises the control_watcher -> apply_control_update ->
// generation_bump pipeline for EventTrigger documents. Expected to pass
// after Task 16 landed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_trigger_insert_bumps_active_generation() {
    let db = test_db("event-trigger-snapshot-reconcile").await;
    let identity = Arc::new(test_identity("event-trigger-snapshot-reconcile"));
    let mock_endpoint = MockModelEndpoint::start("default").unwrap();
    bind_default_behavior_backend(
        db.node.as_ref(),
        identity.did(),
        "backend-event-trigger-snapshot-reconcile",
        mock_endpoint.endpoint(),
    )
    .await;
    let agent = DefraAgent::from_default_behavior_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let agent_did = agent.agent_did().to_string();
    let default_behavior_id = agent.default_behavior_id().to_string();

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));

    // Capture the baseline snapshot generation after startup. This is the
    // `generation` value that a post-startup EventTrigger insert must exceed
    // if the event-driven reload is wired.
    let startup = wait_for_runtime_snapshot(db.node.as_ref(), &agent_did, |snapshot| {
        snapshot.process_state == "ready"
            && snapshot.reconcile_phase == "idle"
            && snapshot.active_generation >= 1
            && snapshot.last_reconcile_result == "startup"
    })
    .await;
    let initial_generation = startup.active_generation;
    assert!(
        startup.last_reconcile_error.is_empty(),
        "startup reconcile should be clean, got error={:?}",
        startup.last_reconcile_error
    );

    // Insert one Task + one EventTrigger post-startup. The Task binds to the
    // default behavior so the EventTrigger resolves as *active* once the
    // control watcher triggers a reload.
    create_task(
        db.node.as_ref(),
        "task-event-trigger-alpha",
        &default_behavior_id,
        "alpha prompt",
    )
    .await;
    create_event_trigger(
        db.node.as_ref(),
        "event-trigger-alpha",
        "task-event-trigger-alpha",
        "AgentMessage",
        "create",
    )
    .await;

    // After Task 16 wires `apply_control_update` to dispatch on EventTrigger
    // doc IDs, the post-insert reload bumps `active_generation` beyond the
    // startup baseline and marks the reconcile as "applied".
    let reconciled = wait_for_runtime_snapshot(db.node.as_ref(), &agent_did, |snapshot| {
        snapshot.process_state == "ready"
            && snapshot.reconcile_phase == "idle"
            && snapshot.active_generation > initial_generation
            && snapshot.last_reconcile_result == "applied"
    })
    .await;
    assert_eq!(reconciled.default_behavior_id, default_behavior_id);
    assert!(
        reconciled.last_reconcile_error.is_empty(),
        "post-insert reconcile should be clean, got error={:?}",
        reconciled.last_reconcile_error
    );
    assert!(
        reconciled.active_generation > initial_generation,
        "active_generation should bump after Task+EventTrigger insert (initial={initial_generation}, observed={})",
        reconciled.active_generation
    );
    assert_eq!(
        reconciled.last_reconcile_result, "applied",
        "last_reconcile_result should be 'applied' after EventTrigger insert"
    );

    let _ = shutdown_tx.send(true);
    handle.await.unwrap().unwrap();
}
