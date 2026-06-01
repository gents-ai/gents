use super::*;
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
    match value {
        "argumentInvalid" => ToolFailureClass::ArgumentInvalid,
        "serviceUnavailable" => ToolFailureClass::ServiceUnavailable,
        "transport" => ToolFailureClass::Transport,
        "toolReturnedError" => ToolFailureClass::ToolReturnedError,
        "policyDenied" => ToolFailureClass::PolicyDenied,
        "external" => ToolFailureClass::External,
        other => panic!("unknown Lean tool failure class {other:?}"),
    }
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

    let pool = McpPool::new_with_connector(move |_service_id, endpoint, agent_did_header| {
        let connect_attempts = Arc::clone(&connect_attempts_for_fn);
        let list_calls = Arc::clone(&list_calls_for_fn);
        async move {
            let attempt = connect_attempts.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(McpConnection {
                endpoint,
                agent_did_header,
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
    });

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

#[derive(Debug)]
struct CapturedMcpHttpRequest {
    method: String,
    agent_did_header: Option<String>,
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
                    });
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });

    (format!("http://{addr}/mcp"), requests)
}

struct ParsedMcpHttpRequest {
    method: String,
    id: Option<serde_json::Value>,
    agent_did_header: Option<String>,
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
