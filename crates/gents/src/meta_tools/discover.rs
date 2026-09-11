use crate::llm::tool::Tool;
use crate::llm::tool::ToolDefinition;
use serde::Deserialize;

use crate::health_checker::HealthStatus;

use super::shared::{format_health_status, MetaToolContext, MetaToolError};

#[derive(Debug, Deserialize)]
pub struct DiscoverToolsArgs {
    #[serde(default)]
    pub(super) query: Option<String>,
}

#[derive(Clone)]
pub struct DiscoverToolsTool {
    ctx: MetaToolContext,
}

impl DiscoverToolsTool {
    pub(crate) fn new(ctx: MetaToolContext) -> Self {
        Self { ctx }
    }
}

impl Tool for DiscoverToolsTool {
    const NAME: &'static str = "discover_tools";

    type Error = MetaToolError;
    type Args = DiscoverToolsArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Browse or search available MCP data service tools. Returns a compact \
                index of services and their tools (name + one-line description). Call with \
                no query to list all services, or provide a search query to filter. Use \
                describe_tool to get the compact required/optional argument contract before \
                calling a tool; request raw_schema only when exact JSON Schema is needed. \
                Native direct tools such as file or bash tools are not data services and are \
                described by their own tool definitions."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Optional search query to filter services and tools."
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let services =
            crate::registry::configured_mcp_services(&self.ctx.node, &self.ctx.agent_did)
                .await?
                .into_iter()
                .filter(|service| {
                    service.enabled && self.ctx.service_selection(&service.service_id).is_some()
                })
                .collect::<Vec<_>>();
        if services.is_empty() {
            return Ok(if self.ctx.allowed_mcp_service_ids.is_empty() {
                "No MCP services are allowed for this behavior.".to_string()
            } else {
                format!(
                    "No allowed data services are currently online. Allowed services: {}.",
                    self.ctx.allowed_mcp_service_ids.join(", ")
                )
            });
        }
        let query = args.query.as_deref().unwrap_or_default().to_lowercase();
        let results = futures::future::join_all(services.iter().map(|service| async {
            let sid = &service.service_id;
            let health = self.ctx.health.get(sid).await;
            let unreachable = matches!(
                health.as_ref().map(|h| h.status),
                Some(HealthStatus::Unreachable)
            );
            let catalog = if unreachable {
                None
            } else {
                match super::shared::resolve_service(
                    service,
                    &self.ctx.local_hostname,
                    self.ctx.local_subnet.as_deref(),
                ) {
                    Ok(route) => self.ctx.list_tools(sid, &route).await.ok(),
                    Err(_) => None,
                }
            };
            let names = catalog
                .map(|catalog| {
                    catalog
                        .tools
                        .into_iter()
                        .filter(|tool| self.ctx.is_tool_allowed(sid, tool.name.as_ref()))
                        .map(|tool| {
                            (
                                tool.name.to_string(),
                                tool.description.unwrap_or_default().to_string(),
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            (health, unreachable, names)
        }))
        .await;
        let mut output = String::new();
        for (service, (health, unreachable, names)) in services.iter().zip(results) {
            let sid = &service.service_id;
            let name = service.display_name.as_deref().unwrap_or(sid);
            let description = service.description.as_deref().unwrap_or_default();
            let haystack = format!(
                "{sid} {name} {description} {}",
                names
                    .iter()
                    .map(|(name, description)| format!("{name} {description}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .to_lowercase();
            if !query.split_whitespace().all(|word| haystack.contains(word)) {
                continue;
            }
            output.push_str(&format!(
                "## {name} ({sid})\nStatus: {}\n{description}\n\nTools:\n",
                format_health_status(health.as_ref())
            ));
            if unreachable {
                output.push_str("  (not contacted — service is unreachable)\n");
            }
            for (name, description) in names {
                output.push_str(&format!("  - {name}: {description}\n"));
            }
            output.push_str("Next: call describe_tool with this service_id and a tool_name before call_tool.\n\n");
        }
        Ok(if output.is_empty() {
            format!(
                "No services matched query {query:?}. {} service(s) are enabled.",
                services.len()
            )
        } else {
            output
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::health_checker::ServiceHealthMap;
    use crate::mcp_pool::McpPool;

    #[tokio::test]
    async fn discover_filters_out_disallowed_registry_services() {
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let mutation = r#"mutation {
            upsert_ToolServiceRegistry(
                filter: { service_id: { _eq: "observability-mcp" } },
                add: {
                    service_id: "observability-mcp",
                    agent_did: "did:key:z-test-agent",
                    display_name: "Observability",
                    description: "Metrics and logs",
                    hostname: "localhost",
                    tailscale_ip: "",
                    lan_ip: "",
                    mcp_port: 1,
                    mcp_path: "/mcp",
                    enabled: true
                },
                update: { enabled: true }
            ) { _docID }
        }"#;
        let response = node.execute(mutation).await;
        assert!(
            !response.has_errors(),
            "registry insert failed: {:?}",
            response.errors
        );

        let tool = DiscoverToolsTool::new(MetaToolContext {
            node,
            mcp_pool: McpPool::new(),
            health: ServiceHealthMap::new(),
            local_hostname: "studio-1".to_string(),
            local_subnet: None,
            agent_did: "did:key:z-test-agent".to_string(),
            allowed_mcp_service_ids: vec!["x-data".to_string()],
            remote_tools: super::super::tests::remote_selection(&["x-data"], &["search_posts"]),
        });

        let output = tool
            .call(DiscoverToolsArgs { query: None })
            .await
            .expect("discover should return model-readable text");

        assert_eq!(
            output,
            "No allowed data services are currently online. Allowed services: x-data."
        );
        assert!(!output.contains("observability-mcp"));
    }

    // --- #622: discover must stay bounded and honor the health gate ---------

    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use chrono::Utc;
    use rmcp::model::{ListToolsResult, Tool as McpTool};

    use crate::health_checker::{HealthStatus, ServiceHealth};
    use crate::lean_vocab_test::lean_tool_preflight_cases;

    async fn seed_registry_service(
        node: &defra_node::EmbeddedNode,
        service_id: &str,
        hostname: &str,
        port: u16,
    ) {
        let service_id = crate::graphql::escape_graphql_string(service_id);
        let hostname = crate::graphql::escape_graphql_string(hostname);
        let mutation = format!(
            r#"mutation {{
            upsert_ToolServiceRegistry(
                filter: {{ service_id: {{ _eq: "{service_id}" }} }},
                add: {{
                    service_id: "{service_id}",
                    agent_did: "did:key:z-test-agent",
                    display_name: "{service_id}",
                    description: "test service {service_id}",
                    hostname: "{hostname}",
                    tailscale_ip: "",
                    lan_ip: "",
                    mcp_port: {port},
                    mcp_path: "/mcp",
                    enabled: true
                }},
                update: {{ enabled: true }}
            ) {{ _docID }}
        }}"#
        );
        let response = node.execute(&mutation).await;
        assert!(
            !response.has_errors(),
            "registry insert failed: {:?}",
            response.errors
        );
    }

    fn stub_tool(name: &str) -> McpTool {
        let schema = serde_json::json!({ "type": "object", "properties": {} })
            .as_object()
            .expect("object schema")
            .clone();
        McpTool::new(name.to_string(), format!("{name} tool"), Arc::new(schema))
    }

    fn test_context(
        node: Arc<defra_node::EmbeddedNode>,
        mcp_pool: McpPool,
        health: ServiceHealthMap,
    ) -> MetaToolContext {
        MetaToolContext {
            node,
            mcp_pool,
            health,
            local_hostname: "studio-1".to_string(),
            local_subnet: None,
            agent_did: "did:key:z-test-agent".to_string(),
            allowed_mcp_service_ids: vec![
                "x-data".to_string(),
                "hf-data".to_string(),
                "web-research-mcp".to_string(),
            ],
            remote_tools: super::super::tests::remote_selection(
                &["x-data", "hf-data", "web-research-mcp"],
                &["search_posts"],
            ),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn discover_stays_bounded_when_a_service_endpoint_blackholes() {
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        seed_registry_service(&node, "x-data", "elsewhere", 9198).await;
        seed_registry_service(&node, "hf-data", "strangenas", 9200).await;

        let pool = McpPool::new_with_list_tools_handler(|service_id, _endpoint| async move {
            if service_id == "hf-data" {
                std::future::pending::<()>().await;
            }
            Ok(ListToolsResult::with_all_items(vec![stub_tool(
                "search_posts",
            )]))
        });

        let tool = DiscoverToolsTool::new(test_context(node, pool, ServiceHealthMap::new()));

        let output = tokio::time::timeout(
            Duration::from_secs(3600),
            tool.call(DiscoverToolsArgs { query: None }),
        )
        .await
        .expect("discover_tools must stay bounded when a service endpoint blackholes")
        .expect("discover returns model-readable text");

        assert!(
            output.contains("search_posts"),
            "healthy service tools must still be listed: {output}"
        );
        assert!(
            output.contains("hf-data"),
            "the unreachable service must still appear in the index: {output}"
        );
    }

    #[tokio::test]
    async fn discover_contact_decisions_match_lean_preflight_health_gate() {
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        seed_registry_service(&node, "x-data", "elsewhere", 9198).await;

        for case in lean_tool_preflight_cases()
            .iter()
            .filter(|case| case.schema_status == "unchecked")
        {
            let contacted = Arc::new(AtomicBool::new(false));
            let contacted_in_handler = Arc::clone(&contacted);
            let pool = McpPool::new_with_list_tools_handler(move |_service_id, _endpoint| {
                let contacted = Arc::clone(&contacted_in_handler);
                async move {
                    contacted.store(true, Ordering::SeqCst);
                    Ok(ListToolsResult::with_all_items(vec![stub_tool(
                        "search_posts",
                    )]))
                }
            });

            let health = ServiceHealthMap::new();
            health
                .set_for_test(
                    "x-data",
                    ServiceHealth {
                        status: match case.health.as_str() {
                            "healthy" => HealthStatus::Healthy,
                            "stale" => HealthStatus::Stale,
                            "unreachable" => HealthStatus::Unreachable,
                            other => panic!("unknown Lean health status {other:?}"),
                        },
                        last_seen: Utc::now(),
                        last_error: (case.health == "unreachable")
                            .then(|| "probe timed out".to_string()),
                    },
                )
                .await;

            let tool = DiscoverToolsTool::new(test_context(Arc::clone(&node), pool, health));
            let output = tool
                .call(DiscoverToolsArgs { query: None })
                .await
                .expect("discover returns model-readable text");

            let expected_contact = case.decision == "dispatch";
            assert_eq!(
                contacted.load(Ordering::SeqCst),
                expected_contact,
                "Lean ToolExecution preflight case {} must gate discover's \
                 list_tools contact (output: {output})",
                case.name
            );
            assert!(
                output.contains("x-data"),
                "service must appear in the index regardless of health: {output}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn discover_fans_out_to_services_concurrently() {
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        seed_registry_service(&node, "x-data", "elsewhere", 9198).await;
        seed_registry_service(&node, "web-research-mcp", "studio-2", 9213).await;

        let pool = McpPool::new_with_list_tools_handler(|_service_id, _endpoint| async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok(ListToolsResult::with_all_items(vec![stub_tool(
                "search_posts",
            )]))
        });

        let tool = DiscoverToolsTool::new(test_context(node, pool, ServiceHealthMap::new()));

        let started = tokio::time::Instant::now();
        let output = tool
            .call(DiscoverToolsArgs { query: None })
            .await
            .expect("discover returns model-readable text");
        let elapsed = started.elapsed();

        assert!(output.contains("x-data") && output.contains("web-research-mcp"));
        assert!(
            elapsed < Duration::from_secs(9),
            "per-service list_tools must fan out concurrently; two 5s services \
             took {elapsed:?} (serial would be ~10s)"
        );
    }
}
