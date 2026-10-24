use super::*;
use gents_protocol::error_chain::ErrorChain;
use rapidhash::{HashMapExt, RapidHashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::lean_vocab_test::lean_tool_retry_case;

fn connection_refused() -> McpPoolError {
    McpPoolError::Handshake {
        service_id: String::new(),
        endpoint: String::new(),
        error: Box::new(rmcp::service::ClientInitializeError::ConnectionClosed(
            "connection refused".to_owned(),
        )),
    }
}

#[tokio::test]
async fn list_tools_transport_failure_retries_generated_safe_read_case() {
    let case = lean_tool_retry_case("retry_mcpListTools_unknown_transport_retrySafeRead");
    assert_eq!(case.operation, "mcpListTools");
    assert_eq!(case.failure_class, "transport");
    assert_eq!(case.disposition, "retrySafeRead");

    let connect_attempts = Arc::new(AtomicUsize::new(0));
    let list_calls = Arc::new(AtomicUsize::new(0));
    let connect_attempts_for_fn = Arc::clone(&connect_attempts);
    let list_calls_for_fn = Arc::clone(&list_calls);

    let pool = McpPool::new_with_connector(
        move |_service_id, endpoint, node_did_header, trace_headers| {
            let connect_attempts = Arc::clone(&connect_attempts_for_fn);
            let list_calls = Arc::clone(&list_calls_for_fn);
            async move {
                let attempt = connect_attempts.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(McpConnection {
                    endpoint,
                    node_did_header,
                    trace_context_headers: trace_headers,
                    last_used: super::fresh_last_used(),
                    resume_policy: SessionResumePolicy::detached("read-service"),
                    list_tools_fn: Box::new(move || {
                        let list_calls = Arc::clone(&list_calls);
                        Box::pin(async move {
                            list_calls.fetch_add(1, Ordering::SeqCst);
                            if attempt == 1 {
                                return Err(McpPoolError::ListTools {
                                    error: super::test_service_error(
                                        "transport dropped while listing tools",
                                    ),
                                });
                            }
                            Ok(ListToolsResult::default())
                        })
                    }),
                    call_tool_fn: Box::new(|_params| {
                        Box::pin(async {
                            Err(McpPoolError::CallTool {
                                error: super::test_service_error("call_tool was not expected"),
                            })
                        })
                    }),
                })
            }
        },
    );

    pool.list_tools("read-service", "http://mcp.test/mcp")
        .await
        .expect("Lean safe-read case should retry list_tools transport failure");

    assert_eq!(connect_attempts.load(Ordering::SeqCst), 2, "{:?}", case);
    assert_eq!(list_calls.load(Ordering::SeqCst), 2, "{:?}", case);
}

#[tokio::test]
async fn call_tool_transport_failure_obeys_generated_no_retry_cases_without_idempotency_metadata() {
    let case = lean_tool_retry_case("retry_mcpCall_unknown_transport_doNotRetry");
    assert_eq!(case.operation, "mcpCall");
    assert_eq!(case.failure_class, "transport");
    assert_eq!(case.disposition, "doNotRetry");

    let pool = McpPool::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = Arc::clone(&calls);
    let endpoint = "http://mcp.test/mcp";
    let service_id = format!("mutating-service-{}", case.idempotency);

    {
        let mut guard = pool.inner.write().await;
        guard.insert(
            super::ParkKey::new(&service_id, endpoint, None),
            Arc::new(McpConnection {
                endpoint: endpoint.to_string(),
                node_did_header: None,
                trace_context_headers: RapidHashMap::new(),
                last_used: super::fresh_last_used(),
                resume_policy: SessionResumePolicy::detached(&service_id),
                list_tools_fn: Box::new(|| Box::pin(async { Ok(ListToolsResult::default()) })),
                call_tool_fn: Box::new(move |_params| {
                    let calls = Arc::clone(&calls_for_fn);
                    Box::pin(async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Err(McpPoolError::CallTool {
                            error: super::test_service_error("transport dropped after dispatch"),
                        })
                    })
                }),
            }),
        );
    }

    let error = pool
        .call_tool(
            &service_id,
            endpoint,
            "write_record",
            serde_json::json!({ "id": 1 }),
        )
        .await
        .expect_err("dispatch failure should propagate");

    assert!(error.to_string().contains("transport dropped"));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "generated ToolExecution case {} must not retry call_tool",
        case.name
    );
    assert!(
        pool.inner
            .read()
            .await
            .contains_key(&super::ParkKey::new(&service_id, endpoint, None)),
        "a failed call_tool must not evict and reconnect without idempotency evidence"
    );
}

#[tokio::test]
async fn streamable_http_default_does_not_send_node_did_header() {
    let (endpoint, requests) = spawn_header_capture_mcp_server().await;
    let pool = McpPool::new();

    pool.list_tools("default-service", &endpoint)
        .await
        .expect("mock MCP server should list tools");

    let requests = requests.lock().expect("captures lock");
    assert!(
        requests
            .iter()
            .any(|request| request.method == "tools/list"),
        "expected a tools/list request, got {requests:?}"
    );
    assert!(
        requests
            .iter()
            .all(|request| request.node_did_header.is_none()),
        "default MCP calls must not send {NODE_DID_HEADER}: {requests:?}"
    );
}

#[tokio::test]
async fn streamable_http_opt_in_sends_node_did_header() {
    let (endpoint, requests) = spawn_header_capture_mcp_server().await;
    let pool = McpPool::new();
    let node_did = "did:key:zIdentityAwareAgent";

    pool.list_tools_with_node_did("identity-service", &endpoint, Some(node_did))
        .await
        .expect("mock MCP server should list tools");

    let requests = requests.lock().expect("captures lock");
    assert!(
        requests
            .iter()
            .any(|request| request.method == "tools/list"),
        "expected a tools/list request, got {requests:?}"
    );
    assert!(
        requests
            .iter()
            .all(|request| request.node_did_header.as_deref() == Some(node_did)),
        "opt-in MCP calls must send {NODE_DID_HEADER}: {requests:?}"
    );
}

#[tokio::test]
async fn mcp_pool_reconnects_when_trace_context_changes() {
    let sequence = Arc::new(AtomicUsize::new(0));
    let sequence_for_headers = Arc::clone(&sequence);
    let (endpoint, requests) = spawn_header_capture_mcp_server().await;
    let pool = McpPool::new().with_trace_context_headers(move || {
        let sequence = sequence_for_headers.fetch_add(1, Ordering::SeqCst) + 1;
        RapidHashMap::from_iter([(
            "traceparent".to_string(),
            format!("00-{sequence:032x}-00f067aa0ba902b7-01"),
        )])
    });

    pool.list_tools("trace-refresh-service", &endpoint)
        .await
        .expect("first mock MCP list_tools should succeed");
    pool.list_tools("trace-refresh-service", &endpoint)
        .await
        .expect("second mock MCP list_tools should succeed");

    let requests = requests.lock().expect("captures lock");
    let traceparents = requests
        .iter()
        .filter(|request| request.method == "tools/list")
        .filter_map(|request| request.traceparent_header.as_deref())
        .collect::<Vec<_>>();

    assert!(
        traceparents.len() >= 2,
        "expected two tools/list requests with trace context, got {requests:?}"
    );
    assert_ne!(
        traceparents[0], traceparents[1],
        "cached MCP connections must refresh when the propagated trace context changes"
    );
}

#[tokio::test]
async fn replacing_connection_terminates_old_streamable_http_session() {
    let sequence = Arc::new(AtomicUsize::new(0));
    let sequence_for_headers = Arc::clone(&sequence);
    let (endpoint, http_log) = spawn_session_tracking_mcp_server().await;
    let pool = McpPool::new().with_trace_context_headers(move || {
        let sequence = sequence_for_headers.fetch_add(1, Ordering::SeqCst) + 1;
        RapidHashMap::from_iter([(
            "traceparent".to_string(),
            format!("00-{sequence:032x}-00f067aa0ba902b7-01"),
        )])
    });

    pool.list_tools("session-service", &endpoint)
        .await
        .expect("first mock MCP list_tools should succeed");
    pool.list_tools("session-service", &endpoint)
        .await
        .expect("second mock MCP list_tools should succeed");

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        {
            let log = http_log.lock().expect("http log lock");
            let deleted_sessions: Vec<&str> = log
                .iter()
                .filter(|entry| entry.http_method == "DELETE")
                .filter_map(|entry| entry.session_id.as_deref())
                .collect();
            if deleted_sessions.contains(&"session-1") {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!(
                    "expected a DELETE for the replaced connection's session-1; \
                     without it every reconnect leaks a server-side session. log: {log:?}"
                );
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn evicting_connection_terminates_streamable_http_session() {
    let (endpoint, http_log) = spawn_session_tracking_mcp_server().await;
    let pool = McpPool::new();

    pool.list_tools("evict-service", &endpoint)
        .await
        .expect("mock MCP list_tools should succeed");
    pool.remove("evict-service").await;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        {
            let log = http_log.lock().expect("http log lock");
            if log.iter().any(|entry| entry.http_method == "DELETE") {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!(
                    "expected a session DELETE after McpPool::remove; \
                     evicted connections must not orphan server-side sessions. log: {log:?}"
                );
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn idle_connection_past_ttl_is_replaced_on_next_use() {
    let (endpoint, http_log) = spawn_session_tracking_mcp_server().await;
    let pool = McpPool::new().with_idle_ttl(std::time::Duration::from_millis(50));

    pool.list_tools("ttl-service", &endpoint)
        .await
        .expect("first mock MCP list_tools should succeed");
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    pool.list_tools("ttl-service", &endpoint)
        .await
        .expect("second mock MCP list_tools should succeed");

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        {
            let log = http_log.lock().expect("http log lock");
            let sessions_created = log
                .iter()
                .filter(|entry| entry.rpc_method.as_deref() == Some("initialize"))
                .count();
            let deleted = log.iter().any(|entry| {
                entry.http_method == "DELETE" && entry.session_id.as_deref() == Some("session-1")
            });
            if sessions_created >= 2 && deleted {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!(
                    "expected the idle connection to be replaced (2 initializes) and its \
                     session-1 deleted after the idle TTL elapsed. log: {log:?}"
                );
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn connection_within_idle_ttl_is_reused() {
    let (endpoint, http_log) = spawn_session_tracking_mcp_server().await;
    let pool = McpPool::new().with_idle_ttl(std::time::Duration::from_secs(600));

    pool.list_tools("fresh-service", &endpoint)
        .await
        .expect("first mock MCP list_tools should succeed");
    pool.list_tools("fresh-service", &endpoint)
        .await
        .expect("second mock MCP list_tools should succeed");

    let log = http_log.lock().expect("http log lock");
    let sessions_created = log
        .iter()
        .filter(|entry| entry.rpc_method.as_deref() == Some("initialize"))
        .count();
    assert_eq!(
        sessions_created, 1,
        "a connection inside its idle TTL must be reused, got {log:?}"
    );
}

#[derive(Debug)]
struct CapturedMcpHttpRequest {
    method: String,
    node_did_header: Option<String>,
    traceparent_header: Option<String>,
}

async fn spawn_header_capture_mcp_server() -> (String, Arc<Mutex<Vec<CapturedMcpHttpRequest>>>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock MCP server");
    let addr = listener.local_addr().expect("mock MCP server address");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captures = Arc::clone(&requests);

    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let captures = Arc::clone(&captures);
            tokio::spawn(async move {
                let Ok(request) = read_mcp_http_request(&mut stream).await else {
                    return;
                };
                let response = mcp_http_response(&request.method, request.id);
                captures
                    .lock()
                    .expect("captures lock")
                    .push(CapturedMcpHttpRequest {
                        method: request.method,
                        node_did_header: request.node_did_header,
                        traceparent_header: request.traceparent_header,
                    });
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });

    (format!("http://{addr}/mcp"), requests)
}

#[derive(Debug)]
struct SessionHttpLogEntry {
    http_method: String,
    session_id: Option<String>,
    rpc_method: Option<String>,
}

async fn spawn_session_tracking_mcp_server() -> (String, Arc<Mutex<Vec<SessionHttpLogEntry>>>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock MCP server");
    let addr = listener.local_addr().expect("mock MCP server address");
    let log = Arc::new(Mutex::new(Vec::new()));
    let log_for_server = Arc::clone(&log);
    let session_counter = Arc::new(AtomicUsize::new(0));

    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let log = Arc::clone(&log_for_server);
            let session_counter = Arc::clone(&session_counter);
            tokio::spawn(async move {
                let Ok(request) = read_mcp_http_request(&mut stream).await else {
                    return;
                };
                log.lock()
                    .expect("http log lock")
                    .push(SessionHttpLogEntry {
                        http_method: request.http_method.clone(),
                        session_id: request.session_id_header.clone(),
                        rpc_method: (!request.method.is_empty()).then(|| request.method.clone()),
                    });
                let response = match request.http_method.as_str() {
                    "DELETE" => "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string(),
                    "GET" => {
                        "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_string()
                    }
                    _ => {
                        let session_header = if request.method == "initialize" {
                            let n = session_counter.fetch_add(1, Ordering::SeqCst) + 1;
                            Some(format!("session-{n}"))
                        } else {
                            None
                        };
                        mcp_http_response_with_session(
                            &request.method,
                            request.id,
                            session_header.as_deref(),
                        )
                    }
                };
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });

    (format!("http://{addr}/mcp"), log)
}

fn mcp_http_response_with_session(
    method: &str,
    id: Option<serde_json::Value>,
    session_id: Option<&str>,
) -> String {
    let base = mcp_http_response(method, id);
    match session_id {
        Some(session_id) => base.replacen(
            "\r\nContent-Type:",
            &format!("\r\nMcp-Session-Id: {session_id}\r\nContent-Type:"),
            1,
        ),
        None => base,
    }
}

struct ParsedMcpHttpRequest {
    method: String,
    id: Option<serde_json::Value>,
    node_did_header: Option<String>,
    traceparent_header: Option<String>,
    http_method: String,
    session_id_header: Option<String>,
}

async fn read_mcp_http_request(stream: &mut TcpStream) -> std::io::Result<ParsedMcpHttpRequest> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    let header_end = loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_bytes(&buf, b"\r\n\r\n") {
            break pos;
        }
    };

    let headers = String::from_utf8_lossy(&buf[..header_end]);
    let http_method = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().next())
        .unwrap_or_default()
        .to_string();
    let session_id_header = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("mcp-session-id")
            .then(|| value.trim().to_string())
    });
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    let node_did_header = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case(NODE_DID_HEADER)
            .then(|| value.trim().to_string())
    });
    let traceparent_header = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("traceparent")
            .then(|| value.trim().to_string())
    });

    let body_start = header_end + b"\r\n\r\n".len();
    while buf.len() < body_start + content_length {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = &buf[body_start..body_start + content_length];
    let body: serde_json::Value =
        serde_json::from_slice(body).unwrap_or_else(|_| serde_json::json!({}));
    let method = body
        .get("method")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let id = body.get("id").cloned();

    Ok(ParsedMcpHttpRequest {
        method,
        id,
        node_did_header,
        traceparent_header,
        http_method,
        session_id_header,
    })
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn mcp_http_response(method: &str, id: Option<serde_json::Value>) -> String {
    if method == "notifications/initialized" {
        return "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .to_string();
    }

    let id = id.unwrap_or_else(|| serde_json::json!(0));
    let result = match method {
        "initialize" => serde_json::json!({
            "protocolVersion": "2025-06-18",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "header-capture-mcp", "version": "0.1.0" }
        }),
        "tools/list" => serde_json::json!({ "tools": [] }),
        _ => serde_json::json!({}),
    };
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
    .to_string();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

fn pending_connect_pool() -> McpPool {
    McpPool::new_with_connector(|_service_id, _endpoint, _node_did, _trace_headers| async {
        std::future::pending::<Result<McpConnection, McpPoolError>>().await
    })
}

#[tokio::test(start_paused = true)]
async fn hung_connect_is_internally_bounded() {
    let pool = pending_connect_pool();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(3600),
        pool.list_tools("hf-data", "http://strangenas:9200/mcp"),
    )
    .await
    .expect("list_tools must internally bound a hung MCP connect");
    let error = result.expect_err("hung connect must surface an error");
    let message = format!("{}", ErrorChain(&error));
    assert!(
        message.contains("timed out"),
        "expected a timeout error, got: {message}"
    );
}

#[tokio::test(start_paused = true)]
async fn hung_list_call_is_internally_bounded() {
    let pool = McpPool::new_with_list_tools_handler(|_service_id, _endpoint| async {
        std::future::pending::<Result<ListToolsResult, McpPoolError>>().await
    });
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(3600),
        pool.list_tools("hf-data", "http://strangenas:9200/mcp"),
    )
    .await
    .expect("list_tools must internally bound a hung tools/list call");
    let error = result.expect_err("hung list call must surface an error");
    let message = format!("{}", ErrorChain(&error));
    assert!(
        message.contains("timed out"),
        "expected a timeout error, got: {message}"
    );
}

#[tokio::test(start_paused = true)]
async fn hung_connect_does_not_wedge_other_services() {
    let pool =
        McpPool::new_with_connector(|service_id, endpoint, node_did, trace_headers| async move {
            if service_id == "hf-data" {
                std::future::pending::<()>().await;
            }
            Ok(McpConnection {
                endpoint,
                node_did_header: node_did,
                trace_context_headers: trace_headers,
                list_tools_fn: Box::new(|| Box::pin(async { Ok(ListToolsResult::default()) })),
                call_tool_fn: Box::new(|_params| {
                    Box::pin(async {
                        Err(McpPoolError::CallTool {
                            error: super::test_service_error("call_tool was not expected"),
                        })
                    })
                }),
                last_used: super::fresh_last_used(),
                resume_policy: SessionResumePolicy::detached(&service_id),
            })
        });

    let hung_pool = pool.clone();
    let hung = tokio::spawn(async move {
        let _ = hung_pool
            .list_tools("hf-data", "http://strangenas:9200/mcp")
            .await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let started = tokio::time::Instant::now();
    let healthy = tokio::time::timeout(
        std::time::Duration::from_secs(3600),
        pool.list_tools("x-data", "http://studio-1:9198/mcp"),
    )
    .await
    .expect("a hung connect for one service must not block other services");
    assert!(healthy.is_ok(), "healthy service must list tools");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "healthy service waited {:?} behind the hung connect — connect must \
         not hold the pool lock",
        started.elapsed()
    );

    hung.abort();
}

fn counting_pool(
    connect_attempts: Arc<AtomicUsize>,
    policies: Arc<Mutex<Vec<Arc<SessionResumePolicy>>>>,
    poison_at_creation: bool,
) -> McpPool {
    McpPool::new_with_connector(move |service_id, endpoint, node_did, trace_headers| {
        let connect_attempts = Arc::clone(&connect_attempts);
        let policies = Arc::clone(&policies);
        async move {
            connect_attempts.fetch_add(1, Ordering::SeqCst);
            let resume_policy = SessionResumePolicy::detached(&service_id);
            if poison_at_creation {
                resume_policy.poison_for_test();
            }
            policies
                .lock()
                .expect("policies lock")
                .push(Arc::clone(&resume_policy));
            Ok(McpConnection {
                endpoint,
                node_did_header: node_did,
                trace_context_headers: trace_headers,
                last_used: super::fresh_last_used(),
                resume_policy,
                list_tools_fn: Box::new(|| Box::pin(async { Ok(ListToolsResult::default()) })),
                call_tool_fn: Box::new(|_params| {
                    Box::pin(async {
                        Err(McpPoolError::CallTool {
                            error: super::test_service_error("call_tool was not expected"),
                        })
                    })
                }),
            })
        }
    })
}

#[tokio::test]
async fn poisoned_session_is_replaced_on_next_use() {
    let connect_attempts = Arc::new(AtomicUsize::new(0));
    let policies = Arc::new(Mutex::new(Vec::new()));
    let pool = counting_pool(Arc::clone(&connect_attempts), Arc::clone(&policies), false);

    pool.list_tools("resume-service", "http://mcp.test/mcp")
        .await
        .expect("first list_tools should succeed");
    assert_eq!(connect_attempts.load(Ordering::SeqCst), 1);

    policies.lock().expect("policies lock")[0].poison_for_test();

    pool.list_tools("resume-service", "http://mcp.test/mcp")
        .await
        .expect("list_tools after poisoning should succeed on a fresh session");
    assert_eq!(
        connect_attempts.load(Ordering::SeqCst),
        2,
        "a poisoned session must be replaced, not reused"
    );

    pool.list_tools("resume-service", "http://mcp.test/mcp")
        .await
        .expect("healthy fresh session should be reused");
    assert_eq!(
        connect_attempts.load(Ordering::SeqCst),
        2,
        "the healthy replacement must be reused, not churned"
    );

    assert_eq!(
        pool.resume_stats("resume-service")
            .session_reinits
            .load(Ordering::SeqCst),
        1,
        "the poison-triggered replacement must be counted"
    );
}

#[tokio::test(start_paused = true)]
async fn repeated_connect_failures_park_the_service() {
    let connect_attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_fn = Arc::clone(&connect_attempts);
    let pool =
        McpPool::new_with_connector(move |_service_id, _endpoint, _node_did, _trace_headers| {
            let attempts = Arc::clone(&attempts_for_fn);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(connection_refused())
            }
        });

    let mut parked_failures = 0usize;
    for _ in 0..240 {
        let error = pool
            .list_tools("down-service", "http://mcp.test/mcp")
            .await
            .expect_err("connects to a dead service must fail");
        if format!("{}", ErrorChain(&error)).contains("parked") {
            parked_failures += 1;
        }
        tokio::time::advance(std::time::Duration::from_secs(30)).await;
    }

    let attempts = connect_attempts.load(Ordering::SeqCst);
    assert!(
        attempts <= 30,
        "connect attempts must be park-bounded over simulated time, got {attempts} in 240 calls"
    );
    assert!(
        parked_failures >= 200,
        "most calls to a parked service must fail fast without a connect \
         attempt, got {parked_failures} parked failures"
    );
}

#[tokio::test(start_paused = true)]
async fn server_that_kills_every_fresh_session_gets_parked() {
    let connect_attempts = Arc::new(AtomicUsize::new(0));
    let policies = Arc::new(Mutex::new(Vec::new()));
    let pool = counting_pool(Arc::clone(&connect_attempts), policies, true);

    for _ in 0..600 {
        let _ = pool
            .list_tools("flapping-service", "http://mcp.test/mcp")
            .await;
        tokio::time::advance(std::time::Duration::from_secs(10)).await;
    }

    let attempts = connect_attempts.load(Ordering::SeqCst);
    assert!(
        attempts <= 35,
        "session re-inits against a flapping server must be park-bounded, \
         got {attempts} connects in 600 calls"
    );
}

#[tokio::test(start_paused = true)]
async fn concurrent_callers_to_struck_service_share_one_dial() {
    let connect_attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_fn = Arc::clone(&connect_attempts);
    let pool =
        McpPool::new_with_connector(move |_service_id, _endpoint, _node_did, _trace_headers| {
            let attempts = Arc::clone(&attempts_for_fn);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                Err(connection_refused())
            }
        });

    let _ = pool
        .list_tools("struck-service", "http://mcp.test/mcp")
        .await;
    assert_eq!(connect_attempts.load(Ordering::SeqCst), 1);
    tokio::time::advance(std::time::Duration::from_secs(2)).await;

    let mut handles = Vec::new();
    for _ in 0..8 {
        let pool = pool.clone();
        handles.push(tokio::spawn(async move {
            pool.list_tools("struck-service", "http://mcp.test/mcp")
                .await
        }));
    }
    let mut fast_failures = 0usize;
    for handle in handles {
        let error = handle
            .await
            .expect("task join")
            .expect_err("dials to a dead service must fail");
        let message = format!("{}", ErrorChain(&error));
        if message.contains("parked") || message.contains("already in flight") {
            fast_failures += 1;
        }
    }

    assert_eq!(
        connect_attempts.load(Ordering::SeqCst),
        2,
        "concurrent callers past an expired horizon must share one dial"
    );
    assert_eq!(
        fast_failures, 7,
        "the seven followers must fail fast without dialing"
    );
}

#[tokio::test(start_paused = true)]
async fn healthy_service_concurrent_cold_connects_stay_benign() {
    let connect_attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_fn = Arc::clone(&connect_attempts);
    let pool = McpPool::new_with_connector(move |service_id, endpoint, node_did, trace_headers| {
        let attempts = Arc::clone(&attempts_for_fn);
        async move {
            attempts.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            Ok(McpConnection {
                endpoint,
                node_did_header: node_did,
                trace_context_headers: trace_headers,
                last_used: super::fresh_last_used(),
                resume_policy: SessionResumePolicy::detached(&service_id),
                list_tools_fn: Box::new(|| Box::pin(async { Ok(ListToolsResult::default()) })),
                call_tool_fn: Box::new(|_params| {
                    Box::pin(async {
                        Err(McpPoolError::CallTool {
                            error: super::test_service_error("call_tool was not expected"),
                        })
                    })
                }),
            })
        }
    });

    let mut handles = Vec::new();
    for _ in 0..4 {
        let pool = pool.clone();
        handles.push(tokio::spawn(async move {
            pool.list_tools("cold-service", "http://mcp.test/mcp").await
        }));
    }
    for handle in handles {
        handle
            .await
            .expect("task join")
            .expect("concurrent cold connects to a healthy service must all succeed");
    }
    assert_eq!(
        connect_attempts.load(Ordering::SeqCst),
        4,
        "healthy cold starts keep benign duplicate connects (no reservation)"
    );
}

#[tokio::test(start_paused = true)]
async fn poison_recovery_preserves_list_tools_safe_read_retry() {
    let connect_attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_fn = Arc::clone(&connect_attempts);
    let policies = Arc::new(Mutex::new(Vec::new()));
    let policies_for_fn = Arc::clone(&policies);
    let pool = McpPool::new_with_connector(move |service_id, endpoint, node_did, trace_headers| {
        let attempts = Arc::clone(&attempts_for_fn);
        let policies = Arc::clone(&policies_for_fn);
        async move {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
            let resume_policy = SessionResumePolicy::detached(&service_id);
            policies
                .lock()
                .expect("policies lock")
                .push(Arc::clone(&resume_policy));
            Ok(McpConnection {
                endpoint,
                node_did_header: node_did,
                trace_context_headers: trace_headers,
                last_used: super::fresh_last_used(),
                resume_policy,
                list_tools_fn: Box::new(move || {
                    Box::pin(async move {
                        if attempt == 2 {
                            return Err(McpPoolError::ListTools {
                                error: super::test_service_error(
                                    "transport dropped after poison recovery",
                                ),
                            });
                        }
                        Ok(ListToolsResult::default())
                    })
                }),
                call_tool_fn: Box::new(|_params| {
                    Box::pin(async {
                        Err(McpPoolError::CallTool {
                            error: super::test_service_error("call_tool was not expected"),
                        })
                    })
                }),
            })
        }
    });

    pool.list_tools("retry-service", "http://mcp.test/mcp")
        .await
        .expect("first list_tools should succeed");
    policies.lock().expect("policies lock")[0].poison_for_test();

    pool.list_tools("retry-service", "http://mcp.test/mcp")
        .await
        .expect("safe-read retry after poison recovery must not be blocked by parking");

    assert_eq!(
        connect_attempts.load(Ordering::SeqCst),
        3,
        "poison recovery should connect once, then safe-read retry once"
    );
}

#[tokio::test(start_paused = true)]
async fn parked_endpoint_does_not_block_different_endpoint_for_same_service() {
    let connect_attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_fn = Arc::clone(&connect_attempts);
    let pool =
        McpPool::new_with_connector(move |_service_id, _endpoint, _node_did, _trace_headers| {
            let attempts = Arc::clone(&attempts_for_fn);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(connection_refused())
            }
        });

    let _ = pool
        .list_tools("dynamic-service", "http://old-endpoint.test/mcp")
        .await;
    assert_eq!(connect_attempts.load(Ordering::SeqCst), 1);

    let error = pool
        .list_tools("dynamic-service", "http://new-endpoint.test/mcp")
        .await
        .expect_err("new endpoint still fails in this test connector");

    assert!(
        format!("{}", ErrorChain(&error)).contains("connection refused"),
        "new endpoint should dial and surface connector error, not inherit old endpoint park: {}",
        ErrorChain(&error)
    );
    assert_eq!(
        connect_attempts.load(Ordering::SeqCst),
        2,
        "parking one endpoint must not block a different endpoint"
    );
}

#[tokio::test(start_paused = true)]
async fn parked_node_did_does_not_block_different_node_did_for_same_service_endpoint() {
    let connect_attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_fn = Arc::clone(&connect_attempts);
    let pool =
        McpPool::new_with_connector(move |_service_id, _endpoint, _node_did, _trace_headers| {
            let attempts = Arc::clone(&attempts_for_fn);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(connection_refused())
            }
        });

    let _ = pool
        .list_tools_with_node_did(
            "multi-principal-service",
            "http://mcp.test/mcp",
            Some("did:key:agent-a"),
        )
        .await;
    assert_eq!(connect_attempts.load(Ordering::SeqCst), 1);

    let error = pool
        .list_tools_with_node_did(
            "multi-principal-service",
            "http://mcp.test/mcp",
            Some("did:key:agent-b"),
        )
        .await
        .expect_err("second node still fails in this test connector");

    assert!(
        format!("{}", ErrorChain(&error)).contains("connection refused"),
        "different node DID should dial and surface connector error, not inherit the first node's park: {}", ErrorChain(&error)
    );
    assert_eq!(
        connect_attempts.load(Ordering::SeqCst),
        2,
        "parking one bound node DID must not block a different bound node DID"
    );
}

#[tokio::test(start_paused = true)]
async fn park_strikes_decay_after_quiet_period() {
    let connect_attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_fn = Arc::clone(&connect_attempts);
    let pool =
        McpPool::new_with_connector(move |_service_id, _endpoint, _node_did, _trace_headers| {
            let attempts = Arc::clone(&attempts_for_fn);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(connection_refused())
            }
        });

    for _ in 0..5 {
        let _ = pool
            .list_tools("blippy-service", "http://mcp.test/mcp")
            .await;
        tokio::time::advance(std::time::Duration::from_secs(1000)).await;
    }
    assert_eq!(connect_attempts.load(Ordering::SeqCst), 5);

    tokio::time::advance(std::time::Duration::from_secs(31 * 60)).await;
    let _ = pool
        .list_tools("blippy-service", "http://mcp.test/mcp")
        .await;
    assert_eq!(connect_attempts.load(Ordering::SeqCst), 6);

    tokio::time::advance(std::time::Duration::from_secs(3)).await;
    let _ = pool
        .list_tools("blippy-service", "http://mcp.test/mcp")
        .await;
    assert_eq!(
        connect_attempts.load(Ordering::SeqCst),
        7,
        "decayed strikes must not inherit the pre-quiet park horizon"
    );
}

#[tokio::test]
async fn empty_stream_resume_is_terminal_and_reinitializes() {
    let (endpoint, http_log) = spawn_empty_sse_stream_mcp_server().await;
    let pool = McpPool::new();

    pool.list_tools("resume-storm-service", &endpoint)
        .await
        .expect("first mock MCP list_tools should succeed");

    let stats = pool.resume_stats("resume-storm-service");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while stats.sessions_poisoned.load(Ordering::SeqCst) == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "session should have been poisoned after the empty-stream resume; log: {:?}",
            http_log.lock().expect("http log lock")
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    {
        let log = http_log.lock().expect("http log lock");
        let gets = log.iter().filter(|e| e.http_method == "GET").count();
        assert_eq!(
            gets, 2,
            "expected exactly the initial GET plus one resume attempt, got {gets}: {log:?}"
        );
    }

    pool.list_tools("resume-storm-service", &endpoint)
        .await
        .expect("list_tools after poisoning should succeed on a fresh session");

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        {
            let log = http_log.lock().expect("http log lock");
            let initializes = log
                .iter()
                .filter(|e| e.rpc_method.as_deref() == Some("initialize"))
                .count();
            let deleted = log
                .iter()
                .any(|e| e.http_method == "DELETE" && e.session_id.as_deref() == Some("session-1"));
            if initializes >= 2 && deleted {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!(
                    "expected a second initialize and a DELETE for session-1 after \
                     the poisoned session was replaced. log: {log:?}"
                );
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(
        stats.session_reinits.load(Ordering::SeqCst),
        1,
        "the poison-triggered re-initialization must be counted"
    );
}

async fn spawn_empty_sse_stream_mcp_server() -> (String, Arc<Mutex<Vec<SessionHttpLogEntry>>>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock MCP server");
    let addr = listener.local_addr().expect("mock MCP server address");
    let log = Arc::new(Mutex::new(Vec::new()));
    let log_for_server = Arc::clone(&log);
    let session_counter = Arc::new(AtomicUsize::new(0));

    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let log = Arc::clone(&log_for_server);
            let session_counter = Arc::clone(&session_counter);
            tokio::spawn(async move {
                let Ok(request) = read_mcp_http_request(&mut stream).await else {
                    return;
                };
                log.lock()
                    .expect("http log lock")
                    .push(SessionHttpLogEntry {
                        http_method: request.http_method.clone(),
                        session_id: request.session_id_header.clone(),
                        rpc_method: (!request.method.is_empty()).then(|| request.method.clone()),
                    });
                let response = match request.http_method.as_str() {
                    "DELETE" => "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string(),
                    "GET" => "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
                        .to_string(),
                    _ => {
                        let session_header = if request.method == "initialize" {
                            let n = session_counter.fetch_add(1, Ordering::SeqCst) + 1;
                            Some(format!("session-{n}"))
                        } else {
                            None
                        };
                        mcp_http_response_with_session(
                            &request.method,
                            request.id,
                            session_header.as_deref(),
                        )
                    }
                };
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });

    (format!("http://{addr}/mcp"), log)
}

#[tokio::test(start_paused = true)]
async fn configured_connect_limit_can_shorten_or_extend_owner_default() {
    for seconds in [2, 20] {
        let pool = pending_connect_pool();
        let start = tokio::time::Instant::now();
        let result = pool
            .list_tools_with_limits(
                "remote",
                "http://remote",
                None,
                std::time::Duration::from_secs(seconds),
                std::time::Duration::from_secs(30),
            )
            .await;
        assert!(result.unwrap_err().to_string().contains("timed out"));
        assert_eq!(start.elapsed(), std::time::Duration::from_secs(seconds));
    }
}

#[tokio::test]
async fn principal_cache_scope_does_not_depend_on_outbound_header() {
    let pool =
        McpPool::new_with_list_tools_handler(|_, _| async { Ok(ListToolsResult::default()) });
    let first = pool.for_agent("did:test:first");
    let second = pool.for_agent("did:test:second");
    first.list_tools("shared", "http://remote").await.unwrap();
    second.list_tools("shared", "http://remote").await.unwrap();
    let guard = pool.inner.read().await;
    assert_eq!(guard.len(), 2);
    assert!(guard.keys().all(|key| key.node_did_header.is_none()));
    assert!(guard
        .keys()
        .any(|key| key.owner_node_did.as_deref() == Some("did:test:first")));
    assert!(guard
        .keys()
        .any(|key| key.owner_node_did.as_deref() == Some("did:test:second")));
}

#[tokio::test]
async fn pinned_connection_cannot_be_rebound_between_admission_and_dispatch() {
    let pool = McpPool::new_with_connector(|service, endpoint, header, trace| async move {
        let result = header.clone().unwrap_or_default();
        Ok(McpConnection {
            endpoint,
            node_did_header: header,
            trace_context_headers: trace,
            last_used: fresh_last_used(),
            resume_policy: SessionResumePolicy::detached(&service),
            list_tools_fn: Box::new(|| Box::pin(async { Ok(ListToolsResult::default()) })),
            call_tool_fn: Box::new(move |_| {
                let result = result.clone();
                Box::pin(async move {
                    Ok(CallToolResult::success(vec![rmcp::model::Content::text(
                        result,
                    )]))
                })
            }),
        })
    });
    let first = pool
        .get_or_connect(
            "shared",
            "http://remote",
            Some("did:test:first"),
            ParkAdmission::Normal,
            MCP_CONNECT_TIMEOUT,
        )
        .await
        .unwrap();
    pool.get_or_connect(
        "shared",
        "http://remote",
        Some("did:test:second"),
        ParkAdmission::Normal,
        MCP_CONNECT_TIMEOUT,
    )
    .await
    .unwrap();
    let result = pool
        .call_tool_once(&first, build_call_tool_params("run", serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(
        result.content[0].raw.as_text().unwrap().text,
        "did:test:first"
    );
}
