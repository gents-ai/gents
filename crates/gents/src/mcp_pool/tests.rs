use super::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::lean_vocab_test::{
    assert_lean_contract_vocabulary_matches, lean_tool_retry_case, lean_tool_retry_cases,
    LeanContractVocabulary, LeanToolRetryCase,
};

#[test]
fn tool_retry_disposition_contract_cases_match_mcp_pool_policy() {
    assert_lean_contract_vocabulary_matches(LeanContractVocabulary {
        domain: "ToolRetryDisposition",
        rust_source: "Proofs.ToolExecution retryDisposition / mcp_pool::call_tool",
        rust_values: &["doNotRetry", "retrySafeRead", "retryIdempotentToolCall"],
    });

    for case in lean_tool_retry_cases() {
        let rust_disposition = tool_retry_disposition(
            rust_operation(&case.operation),
            rust_idempotency(&case.idempotency),
            rust_failure_class(&case.failure_class),
        );
        assert_eq!(
            rust_disposition.as_contract(),
            case.disposition,
            "Lean ToolExecution retry case {} must match mcp_pool policy",
            case.name
        );
    }

    assert_eq!(
        lean_tool_retry_case("retry_mcpCall_idempotent_transport_retryIdempotentToolCall")
            .disposition,
        "retryIdempotentToolCall"
    );
    assert!(
        lean_tool_retry_cases()
            .iter()
            .filter(|case| case.operation == "nativeCommand")
            .all(|case| case.disposition == "doNotRetry"),
        "Proofs.ToolExecution.native_command_not_retried_by_tool_model"
    );
}

fn rust_operation(value: &str) -> ToolExecutionOperation {
    match value {
        "mcpListTools" => ToolExecutionOperation::McpListTools,
        "mcpCall" => ToolExecutionOperation::McpCall,
        "nativeCommand" => ToolExecutionOperation::NativeCommand,
        other => panic!("unknown Lean tool operation {other:?}"),
    }
}

fn rust_idempotency(value: &str) -> ToolIdempotencyEvidence {
    match value {
        "unknown" => ToolIdempotencyEvidence::Unknown,
        "idempotent" => ToolIdempotencyEvidence::Idempotent,
        "nonIdempotent" => ToolIdempotencyEvidence::NonIdempotent,
        other => panic!("unknown Lean idempotency evidence {other:?}"),
    }
}

fn rust_failure_class(value: &str) -> ToolFailureClass {
    ToolFailureClass::from_persisted(value)
        .unwrap_or_else(|| panic!("unknown Lean tool failure class {value:?}"))
}

#[test]
fn resolve_mcp_url_same_host_uses_localhost() {
    let url = resolve_mcp_url(
        "studio-1",
        "100.69.4.79",
        "192.168.1.104",
        9200,
        "/mcp",
        "studio-1",
        Some("192.168.1.0/24"),
    );
    assert_eq!(url, "http://127.0.0.1:9200/mcp");
}

#[test]
fn resolve_mcp_url_same_subnet_uses_lan_ip() {
    let url = resolve_mcp_url(
        "studio-2",
        "100.76.203.120",
        "192.168.1.152",
        9200,
        "/mcp",
        "studio-1",
        Some("192.168.1.0/24"),
    );
    assert_eq!(url, "http://192.168.1.152:9200/mcp");
}

#[test]
fn resolve_mcp_url_cross_site_uses_tailscale_when_subnet_differs() {
    let url = resolve_mcp_url(
        "mini-1",
        "100.86.62.91",
        "192.168.1.101",
        9200,
        "/mcp",
        "studio-1",
        Some("10.0.0.0/24"),
    );
    assert_eq!(url, "http://100.86.62.91:9200/mcp");
}

#[test]
fn resolve_mcp_url_no_lan_ip_uses_tailscale() {
    let url = resolve_mcp_url(
        "vps-1",
        "5.78.68.132",
        "",
        9200,
        "/mcp",
        "studio-1",
        Some("192.168.1.0/24"),
    );
    assert_eq!(url, "http://5.78.68.132:9200/mcp");
}

#[test]
fn resolve_mcp_url_no_subnet_uses_tailscale() {
    let url = resolve_mcp_url(
        "studio-2",
        "100.76.203.120",
        "192.168.1.152",
        9200,
        "/mcp",
        "studio-1",
        None,
    );
    assert_eq!(url, "http://100.76.203.120:9200/mcp");
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
        move |_service_id, endpoint, agent_did_header, trace_headers| {
            let connect_attempts = Arc::clone(&connect_attempts_for_fn);
            let list_calls = Arc::clone(&list_calls_for_fn);
            async move {
                let attempt = connect_attempts.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(McpConnection {
                    endpoint,
                    agent_did_header,
                    trace_context_headers: trace_headers,
                    last_used: super::fresh_last_used(),
                    resume_policy: SessionResumePolicy::detached("read-service"),
                    list_tools_fn: Box::new(move || {
                        let list_calls = Arc::clone(&list_calls);
                        Box::pin(async move {
                            list_calls.fetch_add(1, Ordering::SeqCst);
                            if attempt == 1 {
                                anyhow::bail!("transport dropped while listing tools")
                            }
                            Ok(ListToolsResult::default())
                        })
                    }),
                    call_tool_fn: Box::new(|_params| {
                        Box::pin(async { anyhow::bail!("call_tool was not expected") })
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
    for case in [
        lean_tool_retry_case("retry_mcpCall_unknown_transport_doNotRetry"),
        lean_tool_retry_case("retry_mcpCall_nonIdempotent_transport_doNotRetry"),
    ] {
        assert_call_tool_transport_no_retry(case).await;
    }
}

async fn assert_call_tool_transport_no_retry(case: &LeanToolRetryCase) {
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
            service_id.clone(),
            McpConnection {
                endpoint: endpoint.to_string(),
                agent_did_header: None,
                trace_context_headers: HashMap::new(),
                last_used: super::fresh_last_used(),
                resume_policy: SessionResumePolicy::detached(&service_id),
                list_tools_fn: Box::new(|| Box::pin(async { Ok(ListToolsResult::default()) })),
                call_tool_fn: Box::new(move |_params| {
                    let calls = Arc::clone(&calls_for_fn);
                    Box::pin(async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        anyhow::bail!("transport dropped after dispatch")
                    })
                }),
            },
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
        pool.inner.read().await.contains_key(&service_id),
        "a failed call_tool must not evict and reconnect without idempotency evidence"
    );
}

#[tokio::test]
async fn streamable_http_default_does_not_send_agent_did_header() {
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
            .all(|request| request.agent_did_header.is_none()),
        "default MCP calls must not send {AGENT_DID_HEADER}: {requests:?}"
    );
}

#[tokio::test]
async fn streamable_http_opt_in_sends_agent_did_header() {
    let (endpoint, requests) = spawn_header_capture_mcp_server().await;
    let pool = McpPool::new();
    let agent_did = "did:key:zIdentityAwareAgent";

    pool.list_tools_with_agent_did("identity-service", &endpoint, Some(agent_did))
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
            .all(|request| request.agent_did_header.as_deref() == Some(agent_did)),
        "opt-in MCP calls must send {AGENT_DID_HEADER}: {requests:?}"
    );
}

#[tokio::test]
async fn mcp_pool_reconnects_when_trace_context_changes() {
    let sequence = Arc::new(AtomicUsize::new(0));
    let sequence_for_headers = Arc::clone(&sequence);
    let (endpoint, requests) = spawn_header_capture_mcp_server().await;
    let pool = McpPool::new().with_trace_context_headers(move || {
        let sequence = sequence_for_headers.fetch_add(1, Ordering::SeqCst) + 1;
        HashMap::from([(
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

/// Replacing a pooled connection (trace-context churn happens every task run)
/// must terminate the old streamable-HTTP session on the server — otherwise
/// every run leaks a zombie session server-side. Zombie accumulation is what
/// fed the 2026-07-01 fleet-wide resume storm against observability-mcp.
#[tokio::test]
async fn replacing_connection_terminates_old_streamable_http_session() {
    let sequence = Arc::new(AtomicUsize::new(0));
    let sequence_for_headers = Arc::clone(&sequence);
    let (endpoint, http_log) = spawn_session_tracking_mcp_server().await;
    let pool = McpPool::new().with_trace_context_headers(move || {
        let sequence = sequence_for_headers.fetch_add(1, Ordering::SeqCst) + 1;
        HashMap::from([(
            "traceparent".to_string(),
            format!("00-{sequence:032x}-00f067aa0ba902b7-01"),
        )])
    });

    pool.list_tools("session-service", &endpoint)
        .await
        .expect("first mock MCP list_tools should succeed");
    // Trace context changed → pool replaces the cached connection.
    pool.list_tools("session-service", &endpoint)
        .await
        .expect("second mock MCP list_tools should succeed");

    // The replaced connection's worker sends the session DELETE asynchronously;
    // poll briefly instead of asserting instantly.
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

/// Evicting a dead connection (the list_tools transport-failure retry path)
/// must also terminate the old session rather than orphan it.
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

/// A pooled connection idle past the TTL must be replaced (and its session
/// terminated) on next use instead of reused forever. Long-lived idle
/// connections are the ones that got stuck in the rmcp dead-session resume
/// loop during the 2026-07-01 fleet incident.
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

/// A connection used again within the TTL must be reused, not churned.
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
    agent_did_header: Option<String>,
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
                        agent_did_header: request.agent_did_header,
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

/// Mock MCP server that issues `Mcp-Session-Id: session-<n>` on each
/// `initialize` and logs the HTTP method + session header of every request,
/// so tests can observe session lifecycle (creation, use, DELETE).
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
                        // No standalone SSE stream in this mock.
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
    agent_did_header: Option<String>,
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
    let agent_did_header = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case(AGENT_DID_HEADER)
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
        agent_did_header,
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

// --- #622: a blackholed endpoint must never wedge the pool -------------------
//
// The hf-data incident: a ToolServiceRegistry row pointed at a host whose
// tailscale peer silently dropped packets (no RST). rmcp's transport has no
// timeout of its own, so the connect future never resolved — and because
// `get_or_connect` awaited it, callers hung. These tests fence the pool-seam
// bounds under `tokio::time` paused clocks: if no internal timer exists the
// paused runtime auto-advances straight to the outer guard and the test fails.

fn pending_connect_pool() -> McpPool {
    McpPool::new_with_connector(|_service_id, _endpoint, _agent_did, _trace_headers| async {
        std::future::pending::<anyhow::Result<McpConnection>>().await
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
    let message = format!("{error:#}");
    assert!(
        message.contains("timed out"),
        "expected a timeout error, got: {message}"
    );
}

#[tokio::test(start_paused = true)]
async fn hung_list_call_is_internally_bounded() {
    let pool = McpPool::new_with_list_tools_handler(|_service_id, _endpoint| async {
        std::future::pending::<anyhow::Result<ListToolsResult>>().await
    });
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(3600),
        pool.list_tools("hf-data", "http://strangenas:9200/mcp"),
    )
    .await
    .expect("list_tools must internally bound a hung tools/list call");
    let error = result.expect_err("hung list call must surface an error");
    let message = format!("{error:#}");
    assert!(
        message.contains("timed out"),
        "expected a timeout error, got: {message}"
    );
}

#[tokio::test(start_paused = true)]
async fn hung_connect_does_not_wedge_other_services() {
    let pool = McpPool::new_with_connector(
        |service_id, endpoint, agent_did, trace_headers| async move {
            if service_id == "hf-data" {
                std::future::pending::<()>().await;
            }
            Ok(McpConnection {
                endpoint,
                agent_did_header: agent_did,
                trace_context_headers: trace_headers,
                list_tools_fn: Box::new(|| Box::pin(async { Ok(ListToolsResult::default()) })),
                call_tool_fn: Box::new(|_params| {
                    Box::pin(async { anyhow::bail!("call_tool was not expected") })
                }),
                last_used: super::fresh_last_used(),
                resume_policy: SessionResumePolicy::detached(&service_id),
            })
        },
    );

    let hung_pool = pool.clone();
    let hung = tokio::spawn(async move {
        let _ = hung_pool
            .list_tools("hf-data", "http://strangenas:9200/mcp")
            .await;
    });
    // Let the hung connect start (and, under the defective locking, take the
    // pool write lock) before the healthy service is queried.
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

// --- #639: dead-session resume must be terminal, re-init must be bounded ----

/// Builds a pool whose connector counts connect attempts and records every
/// created connection's resume policy, so tests can poison sessions and
/// observe replacement.
fn counting_pool(
    connect_attempts: Arc<AtomicUsize>,
    policies: Arc<Mutex<Vec<Arc<SessionResumePolicy>>>>,
    poison_at_creation: bool,
) -> McpPool {
    McpPool::new_with_connector(move |service_id, endpoint, agent_did, trace_headers| {
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
                agent_did_header: agent_did,
                trace_context_headers: trace_headers,
                last_used: super::fresh_last_used(),
                resume_policy,
                list_tools_fn: Box::new(|| Box::pin(async { Ok(ListToolsResult::default()) })),
                call_tool_fn: Box::new(|_params| {
                    Box::pin(async { anyhow::bail!("call_tool was not expected") })
                }),
            })
        }
    })
}

/// Required behavior 1: a session whose resume went terminal (poisoned) must
/// be dropped and re-initialized fresh on next use — never reused.
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

/// Required behavior 3 (test c): re-init failures park the service with an
/// escalating horizon — bounded connect attempts over simulated time instead
/// of one attempt per call.
#[tokio::test(start_paused = true)]
async fn repeated_connect_failures_park_the_service() {
    let connect_attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_fn = Arc::clone(&connect_attempts);
    let pool =
        McpPool::new_with_connector(move |_service_id, _endpoint, _agent_did, _trace_headers| {
            let attempts = Arc::clone(&attempts_for_fn);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                anyhow::bail!("connection refused")
            }
        });

    let mut parked_failures = 0usize;
    // Two simulated hours of a caller retrying every 30 seconds.
    for _ in 0..240 {
        let error = pool
            .list_tools("down-service", "http://mcp.test/mcp")
            .await
            .expect_err("connects to a dead service must fail");
        if format!("{error:#}").contains("parked") {
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

/// Required behavior 3: a server that accepts the handshake but kills every
/// fresh session (each new session poisons immediately) must also converge
/// to parked — a resume hot-loop must not become an init hot-loop.
#[tokio::test(start_paused = true)]
async fn server_that_kills_every_fresh_session_gets_parked() {
    let connect_attempts = Arc::new(AtomicUsize::new(0));
    let policies = Arc::new(Mutex::new(Vec::new()));
    let pool = counting_pool(Arc::clone(&connect_attempts), policies, true);

    // 100 simulated minutes of a caller retrying every 10 seconds against a
    // server that poisons every session it hands out.
    for _ in 0..600 {
        let _ = pool
            .list_tools("flapping-service", "http://mcp.test/mcp")
            .await;
        tokio::time::advance(std::time::Duration::from_secs(10)).await;
    }

    // Steady state is two connects per park horizon: the call that detects a
    // poisoned session strikes *and* reconnects (the park gate deliberately
    // uses the pre-strike horizon so a one-off server restart recovers
    // seamlessly), then the horizon blocks everything until it expires.
    // Ramp (~11 doubling strikes) ≈ 22, plus ~4 capped horizons ≈ 8.
    let attempts = connect_attempts.load(Ordering::SeqCst);
    assert!(
        attempts <= 35,
        "session re-inits against a flapping server must be park-bounded, \
         got {attempts} connects in 600 calls"
    );
}

/// The park bound must hold under concurrency, not just sequentially: once a
/// service has strike history, callers racing past an expired horizon must
/// share ONE dial (a per-service in-flight reservation), not stampede — a
/// stampede of N dials records N strikes and N warns before the first
/// failure re-parks, breaking the ≤2-connects-per-horizon property.
#[tokio::test(start_paused = true)]
async fn concurrent_callers_to_struck_service_share_one_dial() {
    let connect_attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_fn = Arc::clone(&connect_attempts);
    let pool =
        McpPool::new_with_connector(move |_service_id, _endpoint, _agent_did, _trace_headers| {
            let attempts = Arc::clone(&attempts_for_fn);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                // Slow failing dial: concurrent callers arrive while this is
                // in flight.
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                anyhow::bail!("connection refused")
            }
        });

    // Establish strike history (strike 1, park 1s), then let the horizon
    // expire.
    let _ = pool
        .list_tools("struck-service", "http://mcp.test/mcp")
        .await;
    assert_eq!(connect_attempts.load(Ordering::SeqCst), 1);
    tokio::time::advance(std::time::Duration::from_secs(2)).await;

    // Eight callers race in while the leader's dial is in flight.
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
        let message = format!("{error:#}");
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

/// Services with NO strike history keep the documented benign concurrent
/// connects (racing duplicate handshakes, last insert wins) — the in-flight
/// reservation must not serialize or fail healthy cold starts.
#[tokio::test(start_paused = true)]
async fn healthy_service_concurrent_cold_connects_stay_benign() {
    let connect_attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_fn = Arc::clone(&connect_attempts);
    let pool =
        McpPool::new_with_connector(move |service_id, endpoint, agent_did, trace_headers| {
            let attempts = Arc::clone(&attempts_for_fn);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                Ok(McpConnection {
                    endpoint,
                    agent_did_header: agent_did,
                    trace_context_headers: trace_headers,
                    last_used: super::fresh_last_used(),
                    resume_policy: SessionResumePolicy::detached(&service_id),
                    list_tools_fn: Box::new(|| Box::pin(async { Ok(ListToolsResult::default()) })),
                    call_tool_fn: Box::new(|_params| {
                        Box::pin(async { anyhow::bail!("call_tool was not expected") })
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

/// A poison-driven reconnect happens before the safe-read `list_tools` call.
/// If that first call fails, the existing one-shot safe-read retry must still
/// be allowed through the just-recorded poison park horizon.
#[tokio::test(start_paused = true)]
async fn poison_recovery_preserves_list_tools_safe_read_retry() {
    let connect_attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_fn = Arc::clone(&connect_attempts);
    let policies = Arc::new(Mutex::new(Vec::new()));
    let policies_for_fn = Arc::clone(&policies);
    let pool =
        McpPool::new_with_connector(move |service_id, endpoint, agent_did, trace_headers| {
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
                    agent_did_header: agent_did,
                    trace_context_headers: trace_headers,
                    last_used: super::fresh_last_used(),
                    resume_policy,
                    list_tools_fn: Box::new(move || {
                        Box::pin(async move {
                            if attempt == 2 {
                                anyhow::bail!("transport dropped after poison recovery")
                            }
                            Ok(ListToolsResult::default())
                        })
                    }),
                    call_tool_fn: Box::new(|_params| {
                        Box::pin(async { anyhow::bail!("call_tool was not expected") })
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

/// Parking is endpoint-scoped: a dynamic endpoint change for the same
/// service id must be allowed through instead of inheriting the previous
/// endpoint's park horizon.
#[tokio::test(start_paused = true)]
async fn parked_endpoint_does_not_block_different_endpoint_for_same_service() {
    let connect_attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_fn = Arc::clone(&connect_attempts);
    let pool =
        McpPool::new_with_connector(move |_service_id, _endpoint, _agent_did, _trace_headers| {
            let attempts = Arc::clone(&attempts_for_fn);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                anyhow::bail!("connection refused")
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
        format!("{error:#}").contains("connection refused"),
        "new endpoint should dial and surface connector error, not inherit old endpoint park: {error:#}"
    );
    assert_eq!(
        connect_attempts.load(Ordering::SeqCst),
        2,
        "parking one endpoint must not block a different endpoint"
    );
}

/// Parking is also principal-scoped: a shared pool may legitimately connect to
/// the same service and endpoint with different bound agent DIDs, and a failure
/// for one principal must not park the other.
#[tokio::test(start_paused = true)]
async fn parked_agent_did_does_not_block_different_agent_did_for_same_service_endpoint() {
    let connect_attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_fn = Arc::clone(&connect_attempts);
    let pool =
        McpPool::new_with_connector(move |_service_id, _endpoint, _agent_did, _trace_headers| {
            let attempts = Arc::clone(&attempts_for_fn);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                anyhow::bail!("connection refused")
            }
        });

    let _ = pool
        .list_tools_with_agent_did(
            "multi-principal-service",
            "http://mcp.test/mcp",
            Some("did:key:agent-a"),
        )
        .await;
    assert_eq!(connect_attempts.load(Ordering::SeqCst), 1);

    let error = pool
        .list_tools_with_agent_did(
            "multi-principal-service",
            "http://mcp.test/mcp",
            Some("did:key:agent-b"),
        )
        .await
        .expect_err("second principal still fails in this test connector");

    assert!(
        format!("{error:#}").contains("connection refused"),
        "different agent DID should dial and surface connector error, not inherit principal-a park: {error:#}"
    );
    assert_eq!(
        connect_attempts.load(Ordering::SeqCst),
        2,
        "parking one bound agent DID must not block a different bound agent DID"
    );
}

/// Strikes decay after a quiet period: a service that flapped long ago must
/// not inherit a huge park horizon for its next transient failure.
#[tokio::test(start_paused = true)]
async fn park_strikes_decay_after_quiet_period() {
    let connect_attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_fn = Arc::clone(&connect_attempts);
    let pool =
        McpPool::new_with_connector(move |_service_id, _endpoint, _agent_did, _trace_headers| {
            let attempts = Arc::clone(&attempts_for_fn);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                anyhow::bail!("connection refused")
            }
        });

    // Five failures far enough apart that none is blocked by the park
    // horizon, close enough that strikes accumulate (park grows to 16s).
    for _ in 0..5 {
        let _ = pool
            .list_tools("blippy-service", "http://mcp.test/mcp")
            .await;
        tokio::time::advance(std::time::Duration::from_secs(1000)).await;
    }
    assert_eq!(connect_attempts.load(Ordering::SeqCst), 5);

    // A quiet period longer than the decay window resets the strike count…
    tokio::time::advance(std::time::Duration::from_secs(31 * 60)).await;
    let _ = pool
        .list_tools("blippy-service", "http://mcp.test/mcp")
        .await;
    assert_eq!(connect_attempts.load(Ordering::SeqCst), 6);

    // …so the follow-up failure parks with a fresh (tiny) horizon instead of
    // the escalated pre-quiet one (32s would still be parked 3s later).
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

/// End-to-end against a real rmcp transport (required test a): a server that
/// answers SSE GETs with 200 + an immediately-closed empty stream — the
/// dead-session signature from the 2026-07-06/07 incidents — must get exactly
/// one resume attempt, then the session is poisoned, and the next pool use
/// re-initializes a fresh session (DELETE-ing the old one, #626).
#[tokio::test]
async fn empty_stream_resume_is_terminal_and_reinitializes() {
    let (endpoint, http_log) = spawn_empty_sse_stream_mcp_server().await;
    let pool = McpPool::new();

    pool.list_tools("resume-storm-service", &endpoint)
        .await
        .expect("first mock MCP list_tools should succeed");

    // The transport's standalone GET stream comes back empty, closes, and is
    // resumed once; that resume also comes back empty → session poisoned.
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

    // Quiet period: with the hot loop, GETs arrive ~1/s; after poisoning
    // there must be no further resume traffic at all.
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    {
        let log = http_log.lock().expect("http log lock");
        let gets = log.iter().filter(|e| e.http_method == "GET").count();
        assert_eq!(
            gets, 2,
            "expected exactly the initial GET plus one resume attempt, got {gets}: {log:?}"
        );
    }

    // Next use replaces the poisoned session: fresh initialize + DELETE of
    // the dead session.
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

/// Mock MCP server whose SSE GET endpoint always answers 200 +
/// `text/event-stream` and immediately closes the connection — an empty
/// stream, the rmcp dead-session resume signature. POSTs keep working so the
/// test isolates the resume path.
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
                    // 200 + empty SSE stream, closed immediately: the
                    // dead-session resume answer this issue is about.
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
