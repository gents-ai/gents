use super::support::*;
use super::*;

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::thread::{self, JoinHandle};

use tracing::instrument::WithSubscriber;

// These timeouts detect deadlocks; they are not latency assertions.
const READMISSION_DEADLOCK_GUARD: Duration = Duration::from_secs(30);
const DEMOTION_DEADLOCK_GUARD: Duration = Duration::from_secs(30);

/// A raw-TCP OpenAI-compatible endpoint: the model list for probes, one
/// streamed assistant answer per chat completion (and a JSON completion for
/// non-streaming callers such as session-title inference), in the byte shapes
/// the full-daemon streaming backend emits.
struct StreamingChatEndpoint {
    endpoint: String,
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl StreamingChatEndpoint {
    fn start(model_name: &str) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = stop.clone();
        let model_name = model_name.to_string();
        let handle = thread::spawn(move || {
            while !stop_for_thread.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                };
                if let Some(request) = read_request_with_body(&mut stream) {
                    let (status, content_type, body) = match (
                        request.method.as_str(),
                        request.path.as_str(),
                    ) {
                        ("GET", "/v1/models") | ("GET", "/models") => (
                            "200 OK",
                            "application/json",
                            format!(r#"{{"data":[{{"id":"{model_name}"}}]}}"#),
                        ),
                        ("POST", "/v1/chat/completions") => {
                            let streaming = request.body.contains("\"stream\":true");
                            if streaming {
                                let text = "plugin readmitted";
                                (
                                    "200 OK",
                                    "text/event-stream",
                                    format!(
                                        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                                        serde_json::json!({
                                            "choices": [{"delta": {"content": text, "tool_calls": []}, "finish_reason": null}],
                                            "usage": null
                                        }),
                                        serde_json::json!({
                                            "choices": [],
                                            "usage": {"prompt_tokens": 8, "completion_tokens": 3, "total_tokens": 11}
                                        })
                                    ),
                                )
                            } else {
                                (
                                    "200 OK",
                                    "application/json",
                                    serde_json::json!({
                                        "id": "chatcmpl-title",
                                        "object": "chat.completion",
                                        "created": 1_710_000_000_u64,
                                        "model": model_name,
                                        "choices": [{
                                            "index": 0,
                                            "finish_reason": "stop",
                                            "message": {
                                                "role": "assistant",
                                                "content": "mock-title",
                                                "refusal": null
                                            }
                                        }],
                                        "usage": {"prompt_tokens": 4, "completion_tokens": 1, "total_tokens": 5}
                                    })
                                    .to_string(),
                                )
                            }
                        }
                        _ => (
                            "404 Not Found",
                            "application/json",
                            r#"{"error":"not found"}"#.to_string(),
                        ),
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();
                }
                let _ = stream.shutdown(Shutdown::Both);
            }
        });
        Self {
            endpoint: format!("http://127.0.0.1:{port}/v1"),
            port,
            stop,
            handle: Some(handle),
        }
    }

    fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

impl Drop for StreamingChatEndpoint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct RequestWithBody {
    method: String,
    path: String,
    body: String,
}

fn read_request_with_body(stream: &mut TcpStream) -> Option<RequestWithBody> {
    stream.set_nonblocking(false).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    let mut buffer = Vec::new();
    let mut temp = [0u8; 1024];
    let header_end = loop {
        let read = stream.read(&mut temp).ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&temp[..read]);
        if let Some(index) = find_subslice(&buffer, b"\r\n\r\n") {
            break index + 4;
        }
    };
    let header_text = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let content_length = header_text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    while buffer.len() < header_end + content_length {
        let read = stream.read(&mut temp).ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&temp[..read]);
    }
    let mut lines = header_text.split("\r\n").filter(|line| !line.is_empty());
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let body =
        String::from_utf8_lossy(&buffer[header_end..header_end + content_length]).to_string();
    Some(RequestWithBody { method, path, body })
}

async fn wait_for_readiness(
    node: &defra_node::EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
    expected_state: BehaviorReadinessState,
) -> BehaviorReadinessSnapshot {
    let deadline = tokio::time::Instant::now() + READMISSION_DEADLOCK_GUARD;
    loop {
        let readiness = fetch_behavior_readiness(node, agent_did).await;
        if readiness
            .behaviors
            .iter()
            .any(|entry| entry.behavior_id == behavior_id && entry.state == expected_state)
        {
            return readiness;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for behavior {behavior_id} to reach state {expected_state:?}; last readiness: {readiness:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// #2338 end to end: a behavior whose Tools document names a missing plugin
/// burns its build budget and is demoted; installing that plugin through the
/// real pack-install owner mid-run re-admits the behavior on the next
/// reconcile, and a request then completes against the behavior.
#[tokio::test]
async fn demoted_behavior_is_readmitted_when_its_named_plugin_installs_midrun() {
    crate::test_support::enable_scoped_event_capture();
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("plugin-readmission"));
    let mock_endpoint = StreamingChatEndpoint::start("default");
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-plugin-readmit",
        mock_endpoint.endpoint(),
    )
    .await;
    let plugin_home = tempfile::tempdir().unwrap();
    let agent = crate::Gents::from_default_behavior_documents(
        node.clone(),
        identity.clone(),
        crate::agent::DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            plugin_home: Some(plugin_home.path().to_path_buf()),
            retry_policy: crate::retry::RetryPolicy {
                max_retries: 3,
                base_delay_ms: 5,
                max_delay_ms: 10,
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let agent_did = identity.did().to_string();
    let behavior_id = agent.default_behavior_id().to_string();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let run = tokio::spawn(agent.run(shutdown_rx));

    wait_for_runtime_process_state(node.as_ref(), &agent_did, "ready").await;
    wait_for_readiness(
        node.as_ref(),
        &agent_did,
        &behavior_id,
        BehaviorReadinessState::Ready,
    )
    .await;

    // Name a plugin the host has not installed: the reconciled slot rebuilds,
    // its tool build fails closed on the missing plugin, and the exhausted
    // build budget demotes the behavior while the process stays Ready.
    let tools: Tools = serde_json::from_value(serde_json::json!({
        "tools_id": format!("{behavior_id}:tools"),
        "agent_did": agent_did,
        "integrations": {"plugins": [{"plugin": "fixture/list_files"}]},
    }))
    .unwrap();
    crate::config_client::write_tools_document(
        &crate::config_client::ConfigAccess::Local(node.clone()),
        &tools,
    )
    .await
    .unwrap();

    let demoted_deadline = tokio::time::Instant::now() + DEMOTION_DEADLOCK_GUARD;
    let demoted_readiness = loop {
        let readiness = fetch_behavior_readiness(node.as_ref(), &agent_did).await;
        let entry = readiness
            .behaviors
            .iter()
            .find(|entry| entry.behavior_id == behavior_id);
        if let Some(entry) = entry {
            if entry.state == BehaviorReadinessState::Unavailable
                && entry.reason == Some(BehaviorReadinessUnavailableReason::ExecutorStartFailed)
            {
                break readiness;
            }
        }
        assert!(
            tokio::time::Instant::now() < demoted_deadline,
            "timed out waiting for the behavior to be demoted on its missing plugin; last readiness: {readiness:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert_eq!(
        demoted_readiness.process_state,
        BehaviorReadinessProcessState::Ready,
        "a demoted behavior degrades readiness without stopping the process"
    );
    let messages = crate::test_support::captured_behavior_demotions("fixture/list_files");
    assert_eq!(
        messages.len(),
        1,
        "exactly one demotion of the missing-plugin behavior is expected: {messages:?}"
    );
    assert!(
        messages[0].contains("install a plugin"),
        "the demotion message must point at installing the missing plugin: {}",
        messages[0]
    );
    assert!(
        messages[0].contains("next reconcile"),
        "the demotion message must name re-admission on the next reconcile: {}",
        messages[0]
    );

    // Install the plugin mid-run through the real pack-install owner: the
    // plugin store record, its bytes, and the only document an install writes.
    let (_pack_guard, pack_root) =
        crate::test_support::fixture_pack_copy("bind_plugin_fixture", &serde_json::json!({}));
    let (pack_bytes, _) = crate::pack_archive::pack_dir(&pack_root).unwrap();
    let archive = crate::pack_archive::PackArchive::from_bytes(&pack_bytes).unwrap();
    let access = crate::config_client::ConfigAccess::Local(node.clone());
    let empty_config: crate::document_config::PackConfig =
        serde_json::from_value(serde_json::json!({"agent_principal": {"agent_did": agent_did}}))
            .unwrap();
    let prepared = crate::pack::prepare_document_pack_install(
        &access,
        &agent_did,
        archive.manifest(),
        &empty_config,
        &BTreeMap::new(),
        &[],
        &|_| None,
    )
    .await
    .unwrap();
    crate::pack::install_prepared_document_pack(
        &access,
        &agent_did,
        &archive,
        &prepared,
        Some(plugin_home.path()),
        false,
        crate::pack::DriftPolicy::Refuse,
    )
    .await
    .unwrap();

    // The install record wakes the reconciler, the resolved plugin identity
    // changes the fingerprint, and the recreated slot builds and turns ready.
    let readmitted_deadline = tokio::time::Instant::now() + READMISSION_DEADLOCK_GUARD;
    let readmitted = loop {
        let readiness = fetch_behavior_readiness(node.as_ref(), &agent_did).await;
        let entry = readiness
            .behaviors
            .iter()
            .find(|entry| entry.behavior_id == behavior_id);
        if let Some(entry) = entry {
            if entry.state == BehaviorReadinessState::Ready && entry.reason.is_none() {
                break readiness;
            }
        }
        assert!(
            tokio::time::Instant::now() < readmitted_deadline,
            "timed out waiting for the behavior to be re-admitted after its plugin installed; last readiness: {readiness:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert!(
        readmitted.active_generation >= 3,
        "the Tools write and the install each applied a generation: {readmitted:?}"
    );

    // The re-admitted behavior completes a request against the mock endpoint.
    let request_doc_id = create_agent_request(
        node.as_ref(),
        &agent_did,
        "req-plugin-readmission",
        "session-plugin-readmission",
        "hello",
    )
    .await;
    wait_for_request_state(node.as_ref(), &request_doc_id, "completed").await;

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(READMISSION_DEADLOCK_GUARD, run)
        .await
        .expect("agent task should join")
        .expect("run task should join")
        .expect("agent run should return ok");
}
