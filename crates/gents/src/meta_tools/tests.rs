use super::shared::{extract_text, mcp_service_allowed, MetaToolError, StructuredToolError};

fn make_call_result(texts: &[&str]) -> rmcp::model::CallToolResult {
    use rmcp::model::CallToolResult;

    let content = texts
        .iter()
        .map(|t| rmcp::model::Content::text(*t))
        .collect();

    CallToolResult::success(content)
}

#[test]
fn empty_mcp_allowlist_denies_every_service() {
    assert!(!mcp_service_allowed(&[], "x-data"));
    assert!(!mcp_service_allowed(&[], "observability-mcp"));
}

#[test]
fn mcp_allowlist_matches_service_id_exactly() {
    let allowlist = vec!["x-data".to_string(), "hf-data".to_string()];

    assert!(mcp_service_allowed(&allowlist, "x-data"));
    assert!(!mcp_service_allowed(&allowlist, "observability-mcp"));
}

#[test]
fn blocked_mcp_service_returns_tool_not_allowed_error() {
    let error = StructuredToolError::tool_not_allowed(
        "observability-mcp",
        "query_metrics",
        vec!["x-data".to_string()],
    );

    assert_eq!(error.failure_class, "tool_not_allowed");
    assert_eq!(error.path, "/service_id");
    assert!(!error.retryable);
    assert_eq!(error.service_id, "observability-mcp");
    assert_eq!(
        error.allowed_mcp_service_ids,
        Some(vec!["x-data".to_string()])
    );
}

#[test]
fn structured_meta_tool_error_becomes_a_typed_dispatch_failure() {
    let error = StructuredToolError::invalid_tool_arguments(
        "x-data",
        "query",
        "/arguments/limit",
        "limit must be an integer",
    );
    let dispatch_error = MetaToolError::structured(error).into_dispatch_error();
    let outcome =
        crate::tool_call_lifecycle::ToolOutcome::from_dispatch("call_tool", Err(dispatch_error));

    match outcome {
        crate::tool_call_lifecycle::ToolOutcome::Failed { class, text, .. } => {
            assert_eq!(
                class,
                crate::tool_call_lifecycle::FailureClass::ArgumentInvalid
            );
            let value: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(value["ok"], false);
            assert_eq!(value["failure_class"], "invalid_tool_arguments");
        }
        other => panic!("structured meta-tool error must be typed failed, got {other:?}"),
    }
}

#[test]
fn extract_text_empty_content() {
    let result = make_call_result(&[]);
    assert_eq!(extract_text(&result), "");
}

#[test]
fn extract_text_single_item() {
    let result = make_call_result(&["hello world"]);
    assert_eq!(extract_text(&result), "hello world");
}

#[test]
fn extract_text_multiple_items_joined_with_newline() {
    let result = make_call_result(&["first", "second", "third"]);
    assert_eq!(extract_text(&result), "first\nsecond\nthird");
}

#[test]
fn meta_tool_error_display_includes_context_chain() {
    let error = anyhow::anyhow!("missing field 'host'").context("MCP call_tool");
    let display = MetaToolError::from(error).to_string();

    assert!(display.contains("MCP call_tool"), "{display}");
    assert!(display.contains("missing field 'host'"), "{display}");
}

pub(super) fn remote_selection(
    services: &[&str],
    names: &[&str],
) -> crate::document_config::RemoteTools {
    crate::document_config::RemoteTools {
        services: services
            .iter()
            .map(|service| crate::document_config::RemoteServiceTools {
                mcp_service_id: service.to_string(),
                tool_names: names.iter().map(|name| name.to_string()).collect(),
                ..Default::default()
            })
            .collect(),
    }
}

use super::{CallToolArgs, CallToolTool, MetaToolContext};
use crate::document_config::RemoteToolStyle;
use crate::llm::tool::Tool;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

async fn remote_node() -> Arc<defra_node::EmbeddedNode> {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    for (owner, port) in [("did:test:owner", 8080), ("did:test:foreign", 9090)] {
        let owner = crate::graphql::escape_graphql_string(owner);
        let response = node.execute(&format!(r#"mutation {{ create_ToolServiceRegistry(input: {{ agent_did: "{owner}", service_id: "shared", hostname: "remote", mcp_port: {port}, enabled: true }}) {{ _docID }} }}"#)).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
    }
    node
}

fn catalog() -> rmcp::model::ListToolsResult {
    rmcp::model::ListToolsResult::with_all_items(["selected", "hidden"].into_iter().map(|name| {
        rmcp::model::Tool::new(name, name, Arc::new(serde_json::json!({"type":"object", "properties":{"value":{"type":"integer"}}, "required":["value"]}).as_object().unwrap().clone()))
    }).collect())
}

fn remote_context(
    node: Arc<defra_node::EmbeddedNode>,
    pool: crate::mcp_pool::McpPool,
) -> MetaToolContext {
    MetaToolContext {
        node,
        mcp_pool: pool.for_agent("did:test:owner"),
        health: crate::health_checker::ServiceHealthMap::new(),
        local_hostname: "local".into(),
        local_subnet: None,
        agent_did: "did:test:owner".into(),
        allowed_mcp_service_ids: vec!["shared".into()],
        remote_tools: remote_selection(&["shared"], &["selected"]),
    }
}

#[tokio::test]
async fn scoped_discovery_and_flat_dispatch_have_identical_permissions() {
    let node = remote_node().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let pool = crate::mcp_pool::McpPool::new_with_tool_handlers(
        |_, endpoint| async move {
            assert_eq!(endpoint, "http://remote:8080"); // same logical service under foreign principal must never win
            Ok(catalog())
        },
        move |params| {
            let count = count.clone();
            async move {
                assert_eq!(params.name, "selected");
                count.fetch_add(1, Ordering::SeqCst);
                Ok(make_call_result(&["executed"]))
            }
        },
    );
    let ctx = remote_context(node.clone(), pool.clone());
    let discovered = super::DiscoverToolsTool::new(ctx.clone())
        .call(super::discover::DiscoverToolsArgs { query: None })
        .await
        .unwrap();
    assert!(discovered.contains("selected"));
    assert!(!discovered.contains("hidden"));
    for style in [RemoteToolStyle::Discovery, RemoteToolStyle::Flat] {
        let mut context = ctx.clone();
        context.remote_tools.services[0].style = style;
        let dispatcher = CallToolTool::new(context.clone());
        let denial = dispatcher
            .call(CallToolArgs {
                service_id: "shared".into(),
                tool_name: "hidden".into(),
                arguments: serde_json::json!({"value":1}),
            })
            .await
            .unwrap_err();
        assert!(denial.to_string().contains("tool_not_allowed"));
        let flat = super::flat::FlatRemoteTool {
            definition: crate::llm::tool::ToolDefinition {
                name: "test".into(),
                description: String::new(),
                parameters: serde_json::json!({}),
            },
            service_id: "shared".into(),
            tool_name: "hidden".into(),
            context: context.clone(),
        };
        let denial = crate::llm::tool::ToolDyn::call(&flat, "{}".into())
            .await
            .unwrap_err();
        assert!(matches!(
            denial,
            crate::llm::tool::ToolError::ReportedFailure {
                class: crate::tool_call_lifecycle::FailureClass::PolicyDenied,
                ..
            }
        ));
        let built = super::build_meta_tools(
            node.clone(),
            pool.clone(),
            context.health,
            "local".into(),
            None,
            context.agent_did,
            context.allowed_mcp_service_ids,
            context.remote_tools,
        )
        .await
        .unwrap();
        match style {
            RemoteToolStyle::Discovery => {
                assert_eq!(built.len(), 3);
                assert_eq!(
                    dispatcher
                        .call(CallToolArgs {
                            service_id: "shared".into(),
                            tool_name: "selected".into(),
                            arguments: serde_json::json!({"value":1})
                        })
                        .await
                        .unwrap(),
                    "executed"
                );
            }
            RemoteToolStyle::Flat => {
                assert_eq!(built.len(), 1);
                assert_eq!(built[0].name(), super::flat_tool_name("shared", "selected"));
                assert_eq!(
                    built[0].definition(String::new()).await.parameters["required"],
                    serde_json::json!(["value"])
                );
                assert_eq!(
                    built[0].call("{\"value\":1}".into()).await.unwrap(),
                    "executed"
                );
            }
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn disabled_and_duplicate_registry_identity_fail_before_dispatch() {
    let node = remote_node().await;
    let context = remote_context(node.clone(), crate::mcp_pool::McpPool::new());
    let result = node.execute(r#"mutation { update_ToolServiceRegistry(filter: { agent_did: {_eq: "did:test:owner"}}, input: {enabled: false}) {_docID} }"#).await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    assert!(super::shared::lookup_service(&context, "shared")
        .await
        .is_err());
    let result = node.execute(r#"mutation { create_ToolServiceRegistry(input: {agent_did: "did:test:owner", service_id: "shared", hostname: "other", mcp_port: 9000}) {_docID} }"#).await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    assert!(
        crate::registry::configured_mcp_services(&node, "did:test:owner")
            .await
            .unwrap_err()
            .to_string()
            .contains("duplicate")
    );
}

#[tokio::test(start_paused = true)]
async fn configured_discovery_timeout_cannot_fall_through_to_mutating_dispatch() {
    let node = remote_node().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let pool = crate::mcp_pool::McpPool::new_with_tool_handlers(
        |_, _| async {
            std::future::pending::<()>().await;
            Ok(catalog())
        },
        move |_| {
            let count = count.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(make_call_result(&["unexpected"]))
            }
        },
    );
    let mut context = remote_context(node, pool);
    context.remote_tools.services[0].discovery_timeout_secs = Some(2);
    let started = tokio::time::Instant::now();
    let error = CallToolTool::new(context)
        .call(CallToolArgs {
            service_id: "shared".into(),
            tool_name: "selected".into(),
            arguments: serde_json::json!({"value":1}),
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("service_unavailable"));
    assert!(started.elapsed() <= std::time::Duration::from_secs(2));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn stale_limit_intersects_normal_call_timeout() {
    let node = remote_node().await;
    for (normal, stale, expected) in [(3, 10, 3), (10, 2, 2)] {
        let pool = crate::mcp_pool::McpPool::new_with_tool_handlers(
            |_, _| async { Ok(catalog()) },
            |_| async {
                std::future::pending::<()>().await;
                Ok(make_call_result(&["unexpected"]))
            },
        );
        let mut context = remote_context(node.clone(), pool);
        context.remote_tools.services[0].timeout_secs = Some(normal);
        context.remote_tools.services[0].stale_timeout_secs = Some(stale);
        context
            .health
            .set_for_test(
                "shared",
                crate::health_checker::ServiceHealth {
                    status: crate::health_checker::HealthStatus::Stale,
                    last_seen: chrono::Utc::now(),
                    last_error: None,
                },
            )
            .await;
        let start = tokio::time::Instant::now();
        let result = CallToolTool::new(context)
            .call(CallToolArgs {
                service_id: "shared".into(),
                tool_name: "selected".into(),
                arguments: serde_json::json!({"value":1}),
            })
            .await;
        assert!(result.unwrap_err().to_string().contains("timed out"));
        assert_eq!(start.elapsed(), std::time::Duration::from_secs(expected));
    }
}
