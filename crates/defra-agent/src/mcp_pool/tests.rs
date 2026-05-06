use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::lean_vocab_test::{assert_lean_contract_vocabulary_matches, LeanContractVocabulary};

#[test]
fn tool_retry_disposition_contract_matches_mcp_pool_policy() {
    // TODO(idempotency): replace this shape pin with a Rust producer contract
    // once MCP services advertise retry dispositions/idempotency metadata.
    assert_lean_contract_vocabulary_matches(LeanContractVocabulary {
        domain: "ToolRetryDisposition",
        rust_source: "Proofs.ToolExecution retryDisposition / mcp_pool::call_tool",
        rust_values: &["doNotRetry", "retrySafeRead", "retryIdempotentToolCall"],
    });
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
async fn call_tool_does_not_retry_cached_dispatch_failure_without_idempotency_metadata() {
    let pool = McpPool::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = Arc::clone(&calls);
    let endpoint = "http://mcp.test/mcp";

    {
        let mut guard = pool.inner.write().await;
        guard.insert(
            "mutating-service".to_string(),
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
            "mutating-service",
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
        "Proofs.ToolExecution.mcp_call_without_idempotency_metadata_does_not_retry"
    );
    assert!(
        pool.inner.read().await.contains_key("mutating-service"),
        "a failed call_tool must not evict and reconnect without idempotency evidence"
    );
}
