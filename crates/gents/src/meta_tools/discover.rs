use crate::llm::tool::Tool;
use crate::llm::tool::ToolDefinition;
use anyhow::anyhow;
use serde::Deserialize;

use crate::health_checker::HealthStatus;
use crate::mcp_pool::resolve_mcp_url;

use super::shared::{format_health_status, MetaToolContext, MetaToolError};

#[derive(Debug, Deserialize)]
pub struct DiscoverToolsArgs {
    #[serde(default)]
    query: Option<String>,
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
        let gql = r#"{
  ToolServiceRegistry(
    filter: { status: { _eq: "online" } },
    order: { updated_at: DESC }
  ) {
    service_id
    display_name
    description
    hostname
    tailscale_ip
    lan_ip
    mcp_port
    mcp_path
    send_agent_did
  }
}"#;

        let resp = self.ctx.node.execute(gql).await;
        if resp.has_errors() {
            return Err(anyhow!("discover_tools query failed: {:?}", resp.errors).into());
        }

        let services = match resp.data.as_ref() {
            Some(data) => match data.get("ToolServiceRegistry").and_then(|v| v.as_array()) {
                Some(arr) => arr.clone(),
                None => {
                    tracing::warn!(
                        "discover_tools: response missing ToolServiceRegistry array — \
                         registry collection may not exist yet"
                    );
                    Vec::new()
                }
            },
            None => {
                tracing::warn!("discover_tools: response contained no data field");
                Vec::new()
            }
        };

        let services = services
            .into_iter()
            .filter(|svc| {
                svc.get("service_id")
                    .and_then(|v| v.as_str())
                    .is_some_and(|service_id| self.ctx.is_mcp_service_allowed(service_id))
            })
            .collect::<Vec<_>>();

        if services.is_empty() {
            if self.ctx.allowed_mcp_service_ids.is_empty() {
                return Ok("No MCP services are allowed for this behavior.".to_string());
            }
            return Ok(format!(
                "No allowed data services are currently online. Allowed services: {}.",
                self.ctx.allowed_mcp_service_ids.join(", ")
            ));
        }

        let query_lower = args.query.as_deref().map(|q| q.to_lowercase());

        // Contact services concurrently — one dead or slow endpoint must not
        // serialize the rest (#622). Unreachable services are not contacted
        // at all: the same preflight decision `call_tool` enforces (Lean
        // MCPHealth coupling C1/C2); their registry row still renders below
        // so the model can see them and why they list no tools.
        let fetches = services.iter().map(|svc| async {
            let sid = svc
                .get("service_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let health = self.ctx.health.get(sid).await;
            let unreachable = matches!(
                health.as_ref().map(|h| h.status),
                Some(HealthStatus::Unreachable)
            );

            let svc_hostname = svc.get("hostname").and_then(|v| v.as_str()).unwrap_or("");
            let svc_tsip = svc
                .get("tailscale_ip")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let svc_lanip = svc.get("lan_ip").and_then(|v| v.as_str()).unwrap_or("");
            let svc_port = svc.get("mcp_port").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
            let svc_path = svc.get("mcp_path").and_then(|v| v.as_str()).unwrap_or("");
            let send_agent_did = svc
                .get("send_agent_did")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let endpoint = if svc_port > 0 && !svc_path.trim().is_empty() {
                Ok(resolve_mcp_url(
                    svc_hostname,
                    svc_tsip,
                    svc_lanip,
                    svc_port,
                    svc_path,
                    &self.ctx.local_hostname,
                    self.ctx.local_subnet.as_deref(),
                ))
            } else {
                Err(anyhow!("incomplete MCP route"))
            };
            let tool_names: Vec<(String, String)> = if unreachable {
                Vec::new()
            } else if let Ok(ep) = &endpoint {
                match self
                    .ctx
                    .mcp_pool
                    .list_tools_with_agent_did(
                        sid,
                        ep,
                        send_agent_did.then_some(self.ctx.agent_did.as_str()),
                    )
                    .await
                {
                    Ok(list) => list
                        .tools
                        .iter()
                        .map(|t| {
                            (
                                t.name.to_string(),
                                t.description.as_deref().unwrap_or("").to_string(),
                            )
                        })
                        .collect(),
                    Err(_) => Vec::new(),
                }
            } else {
                Vec::new()
            };
            (health, unreachable, tool_names)
        });
        let contacted = futures::future::join_all(fetches).await;

        let mut out = String::new();
        let mut matched = 0usize;

        for (svc, (health, unreachable, tool_names)) in services.iter().zip(contacted) {
            let sid = svc
                .get("service_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let name = svc
                .get("display_name")
                .and_then(|v| v.as_str())
                .unwrap_or(sid);
            let desc = svc
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let hostname = svc.get("hostname").and_then(|v| v.as_str()).unwrap_or("");

            if let Some(ref q) = query_lower {
                let tool_text = tool_names
                    .iter()
                    .map(|(n, d)| format!("{} {}", n, d).to_lowercase())
                    .collect::<Vec<_>>()
                    .join(" ");
                let haystack = format!(
                    "{} {} {} {} {}",
                    sid.to_lowercase(),
                    name.to_lowercase(),
                    desc.to_lowercase(),
                    hostname.to_lowercase(),
                    tool_text,
                );
                if !q.split_whitespace().all(|word| haystack.contains(word)) {
                    continue;
                }
            }

            matched += 1;
            out.push_str(&format!("## {name} ({sid})\n"));
            out.push_str(&format!(
                "Status: {}\n",
                format_health_status(health.as_ref())
            ));
            out.push_str(&format!(
                "Host: {}\n",
                if hostname.is_empty() {
                    "unknown"
                } else {
                    hostname
                }
            ));
            if !desc.is_empty() {
                out.push_str(&format!("{desc}\n"));
            }
            out.push_str("\nTools:\n");

            if unreachable {
                out.push_str("  (not contacted — service is unreachable)\n");
            }
            for (tn, td) in &tool_names {
                out.push_str(&format!("  - {tn}: {td}\n"));
            }
            out.push_str(
                "Next: call describe_tool with this service_id and a tool_name before call_tool.\n",
            );
            out.push('\n');
        }

        if matched == 0 {
            Ok(format!(
                "No services matched query {:?}. {} service(s) are online.",
                args.query.as_deref().unwrap_or(""),
                services.len()
            ))
        } else {
            Ok(out)
        }
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
                    display_name: "Observability",
                    description: "Metrics and logs",
                    hostname: "localhost",
                    tailscale_ip: "",
                    lan_ip: "",
                    mcp_port: 1,
                    mcp_path: "/mcp",
                    status: "online"
                },
                update: { status: "online" }
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
        let mutation = format!(
            r#"mutation {{
            upsert_ToolServiceRegistry(
                filter: {{ service_id: {{ _eq: "{service_id}" }} }},
                add: {{
                    service_id: "{service_id}",
                    display_name: "{service_id}",
                    description: "test service {service_id}",
                    hostname: "{hostname}",
                    tailscale_ip: "",
                    lan_ip: "",
                    mcp_port: {port},
                    mcp_path: "/mcp",
                    status: "online"
                }},
                update: {{ status: "online" }}
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
