use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

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

    let pool = McpPool::new_with_connector(move |_service_id, endpoint| {
        let connect_attempts = Arc::clone(&connect_attempts_for_fn);
        let list_calls = Arc::clone(&list_calls_for_fn);
        async move {
            let attempt = connect_attempts.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(McpConnection {
                endpoint,
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

    {
        let mut guard = pool.inner.write().await;
        guard.insert(
            format!("mutating-service-{}", case.idempotency),
            McpConnection {
                endpoint: endpoint.to_string(),
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
            &format!("mutating-service-{}", case.idempotency),
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
            .contains_key(&format!("mutating-service-{}", case.idempotency)),
        "a failed call_tool must not evict and reconnect without idempotency evidence"
    );
}
