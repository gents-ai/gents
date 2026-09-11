use std::sync::Arc;

use anyhow::anyhow;
use defra_node::EmbeddedNode;
use serde::Serialize;

use crate::health_checker::{HealthStatus, ServiceHealth, ServiceHealthMap};
use crate::mcp_pool::resolve_mcp_url;
use crate::mcp_pool::McpPool;
use crate::tool_call_lifecycle::FailureClass;

#[derive(Clone)]
pub struct MetaToolContext {
    pub node: Arc<EmbeddedNode>,
    pub mcp_pool: McpPool,
    pub health: ServiceHealthMap,
    pub local_hostname: String,
    pub local_subnet: Option<String>,
    pub agent_did: String,
    pub allowed_mcp_service_ids: Vec<String>,
    pub remote_tools: crate::document_config::RemoteTools,
}

impl MetaToolContext {
    pub(super) fn is_mcp_service_allowed(&self, service_id: &str) -> bool {
        mcp_service_allowed(&self.allowed_mcp_service_ids, service_id)
    }

    pub(super) fn service_selection(
        &self,
        service_id: &str,
    ) -> Option<&crate::document_config::RemoteServiceTools> {
        let mut matches = self
            .remote_tools
            .services
            .iter()
            .filter(|service| service.mcp_service_id == service_id);
        let service = matches.next()?;
        (matches.next().is_none()
            && self.is_mcp_service_allowed(service_id)
            && service
                .background_tool_names
                .iter()
                .all(|name| service.tool_names.contains(name)))
        .then_some(service)
    }

    pub(super) fn is_tool_allowed(&self, service_id: &str, tool_name: &str) -> bool {
        self.service_selection(service_id)
            .is_some_and(|service| service.tool_names.iter().any(|name| name == tool_name))
    }

    pub(super) async fn list_tools(
        &self,
        service_id: &str,
        service: &ResolvedMcpService,
    ) -> anyhow::Result<rmcp::model::ListToolsResult> {
        let selection = self
            .service_selection(service_id)
            .ok_or_else(|| anyhow!("MCP service is not selected"))?;
        let connect = timeout(selection.connect_timeout_secs, 15)?;
        let discovery = timeout(selection.discovery_timeout_secs, 30)?;
        tokio::time::timeout(
            discovery,
            self.mcp_pool.list_tools_with_limits(
                service_id,
                &service.endpoint,
                service.outbound_agent_did(self),
                connect,
                discovery,
            ),
        )
        .await
        .map_err(|_| {
            anyhow!(
                "MCP discovery on '{service_id}' timed out after {}s",
                discovery.as_secs()
            )
        })?
    }

    pub(super) fn blocked_service_error(
        &self,
        service_id: &str,
        tool_name: &str,
    ) -> Option<StructuredToolError> {
        (!self.is_tool_allowed(service_id, tool_name)).then(|| {
            StructuredToolError::tool_not_allowed(
                service_id,
                tool_name,
                self.allowed_mcp_service_ids.clone(),
            )
        })
    }
}

pub(super) fn mcp_service_allowed(allowed_mcp_service_ids: &[String], service_id: &str) -> bool {
    allowed_mcp_service_ids
        .iter()
        .any(|allowed| allowed == service_id)
}

#[derive(Debug)]
pub struct MetaToolError(MetaToolErrorKind);

#[derive(Debug)]
enum MetaToolErrorKind {
    Other(anyhow::Error),
    Structured(StructuredToolError),
}

impl MetaToolError {
    pub(super) fn structured(error: StructuredToolError) -> Self {
        Self(MetaToolErrorKind::Structured(error))
    }

    pub(super) fn into_dispatch_error(self) -> crate::llm::tool::ToolError {
        match self.0 {
            MetaToolErrorKind::Structured(error) => crate::llm::tool::ToolError::ReportedFailure {
                class: error.lifecycle_failure_class(),
                text: error.to_result_text(),
            },
            MetaToolErrorKind::Other(error) => crate::llm::tool::ToolError::ToolCallError(
                Box::new(MetaToolError(MetaToolErrorKind::Other(error))),
            ),
        }
    }
}

impl std::fmt::Display for MetaToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            MetaToolErrorKind::Other(error) => write!(f, "{error:#}"),
            MetaToolErrorKind::Structured(error) => f.write_str(&error.to_result_text()),
        }
    }
}

impl std::error::Error for MetaToolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.0 {
            MetaToolErrorKind::Other(error) => Some(error.root_cause()),
            MetaToolErrorKind::Structured(_) => None,
        }
    }
}

impl From<anyhow::Error> for MetaToolError {
    fn from(error: anyhow::Error) -> Self {
        Self(MetaToolErrorKind::Other(error))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct StructuredToolError {
    pub(super) ok: bool,
    pub(super) failure_class: &'static str,
    pub(super) path: String,
    pub(super) message: String,
    pub(super) retryable: bool,
    pub(super) service_id: String,
    pub(super) tool_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) requested_tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) available_tools: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) allowed_mcp_service_ids: Option<Vec<String>>,
}

impl StructuredToolError {
    fn lifecycle_failure_class(&self) -> FailureClass {
        match self.failure_class {
            "invalid_tool_arguments" | "invalid_json_arguments" | "arguments_not_object" => {
                FailureClass::ArgumentInvalid
            }
            "tool_not_allowed" => FailureClass::PolicyDenied,
            "service_unavailable"
            | "tool_not_found"
            | "resource_not_found"
            | "service_schema_drift" => FailureClass::ServiceUnavailable,
            "tool_timeout" | "deadline_or_inference_failure" => FailureClass::External,
            _ => FailureClass::ToolReturnedError,
        }
    }

    pub(super) fn invalid_tool_arguments(
        service_id: &str,
        tool_name: &str,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            ok: false,
            failure_class: "invalid_tool_arguments",
            path: path.into(),
            message: message.into(),
            retryable: true,
            service_id: service_id.to_string(),
            tool_name: tool_name.to_string(),
            requested_tool_name: None,
            available_tools: None,
            allowed_mcp_service_ids: None,
        }
    }

    pub(super) fn tool_not_found(
        service_id: &str,
        tool_name: &str,
        available_tools: Vec<String>,
    ) -> Self {
        Self {
            ok: false,
            failure_class: "tool_not_found",
            path: "/tool_name".to_string(),
            message: format!("tool '{tool_name}' was not found on service '{service_id}'"),
            retryable: true,
            service_id: service_id.to_string(),
            tool_name: tool_name.to_string(),
            requested_tool_name: None,
            available_tools: Some(available_tools),
            allowed_mcp_service_ids: None,
        }
    }

    pub(super) fn describe_tool_not_found(
        service_id: &str,
        requested_tool_name: &str,
        available_tools: Vec<String>,
    ) -> Self {
        let alternatives = if available_tools.is_empty() {
            "no tools are currently advertised".to_string()
        } else {
            format!("available tools: {}", available_tools.join(", "))
        };

        Self {
            ok: false,
            failure_class: "tool_not_found",
            path: "/tool_name".to_string(),
            message: format!(
                "tool '{requested_tool_name}' was not found on service '{service_id}'; {alternatives}"
            ),
            retryable: true,
            service_id: service_id.to_string(),
            tool_name: requested_tool_name.to_string(),
            requested_tool_name: Some(requested_tool_name.to_string()),
            available_tools: Some(available_tools),
            allowed_mcp_service_ids: None,
        }
    }

    pub(super) fn service_unavailable(
        service_id: &str,
        requested_tool_name: &str,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            ok: false,
            failure_class: "service_unavailable",
            path: "/service_id".to_string(),
            message: message.into(),
            retryable,
            service_id: service_id.to_string(),
            tool_name: requested_tool_name.to_string(),
            requested_tool_name: Some(requested_tool_name.to_string()),
            available_tools: None,
            allowed_mcp_service_ids: None,
        }
    }

    pub(super) fn tool_not_allowed(
        service_id: &str,
        requested_tool_name: &str,
        allowed_mcp_service_ids: Vec<String>,
    ) -> Self {
        Self {
            ok: false,
            failure_class: "tool_not_allowed",
            path: "/service_id".to_string(),
            message: format!(
                "tool '{requested_tool_name}' on service '{service_id}' is not selected for this behavior; allowed services: {}",
                allowed_mcp_service_ids.join(", ")
            ),
            retryable: false,
            service_id: service_id.to_string(),
            tool_name: requested_tool_name.to_string(),
            requested_tool_name: Some(requested_tool_name.to_string()),
            available_tools: None,
            allowed_mcp_service_ids: Some(allowed_mcp_service_ids),
        }
    }

    pub(super) fn to_result_text(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| {
            format!(
                r#"{{"ok":false,"failure_class":"{}","path":"{}","message":"{}","retryable":{},"service_id":"{}","tool_name":"{}"}}"#,
                self.failure_class,
                self.path,
                self.message,
                self.retryable,
                self.service_id,
                self.tool_name
            )
        })
    }
}

pub(super) struct ResolvedMcpService {
    pub(super) endpoint: String,
    pub(super) send_agent_did: bool,
}

impl ResolvedMcpService {
    pub(super) fn outbound_agent_did<'a>(&self, ctx: &'a MetaToolContext) -> Option<&'a str> {
        self.send_agent_did.then_some(ctx.agent_did.as_str())
    }
}

pub(super) async fn lookup_service(
    ctx: &MetaToolContext,
    service_id: &str,
) -> anyhow::Result<ResolvedMcpService> {
    let entry = crate::registry::configured_mcp_services(&ctx.node, &ctx.agent_did)
        .await?
        .into_iter()
        .find(|service| service.service_id == service_id && service.enabled)
        .ok_or_else(|| {
            anyhow!("service '{service_id}' is not configured or enabled for this principal")
        })?;
    resolve_service(&entry, &ctx.local_hostname, ctx.local_subnet.as_deref())
}

pub(super) fn resolve_service(
    entry: &crate::document_config::ToolServiceRegistry,
    hostname: &str,
    subnet: Option<&str>,
) -> anyhow::Result<ResolvedMcpService> {
    let port = entry
        .mcp_port
        .and_then(|port| u16::try_from(port).ok())
        .filter(|port| *port > 0)
        .ok_or_else(|| anyhow!("service '{}' has no valid MCP port", entry.service_id))?;
    let host = entry.hostname.as_deref().unwrap_or_default();
    let tailscale = entry.tailscale_ip.as_deref().unwrap_or_default();
    let lan = entry.lan_ip.as_deref().unwrap_or_default();
    anyhow::ensure!(
        !host.is_empty() || !tailscale.is_empty() || !lan.is_empty(),
        "service '{}' has no MCP address",
        entry.service_id
    );
    Ok(ResolvedMcpService {
        endpoint: resolve_mcp_url(
            host,
            tailscale,
            lan,
            port,
            entry.mcp_path.as_deref().unwrap_or_default(),
            hostname,
            subnet,
        ),
        send_agent_did: entry.send_agent_did,
    })
}

pub(super) fn extract_text(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.raw.as_text().map(|t| t.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_elapsed(last_seen: chrono::DateTime<chrono::Utc>) -> String {
    let seconds = chrono::Utc::now()
        .signed_duration_since(last_seen)
        .num_seconds()
        .max(0);

    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

pub(super) fn format_health_status(health: Option<&ServiceHealth>) -> String {
    match health {
        Some(health) => match (&health.status, &health.last_error) {
            (HealthStatus::Unreachable, Some(error)) => format!(
                "{} (last seen {} ago, error: {})",
                health.status,
                format_elapsed(health.last_seen),
                error
            ),
            _ => format!(
                "{} (last seen {} ago)",
                health.status,
                format_elapsed(health.last_seen)
            ),
        },
        None => "unknown (awaiting first health check)".to_string(),
    }
}

pub(super) async fn enforce_health_gate(
    health_map: &ServiceHealthMap,
    service_id: &str,
) -> anyhow::Result<Option<ServiceHealth>> {
    let health = health_map.get(service_id).await;
    if let Some(health) = &health {
        match health.status {
            HealthStatus::Unreachable => {
                let suffix = health
                    .last_error
                    .as_deref()
                    .map(|error| format!(" (last error: {error})"))
                    .unwrap_or_default();
                anyhow::bail!("service '{service_id}' is currently unreachable{suffix}");
            }
            HealthStatus::Stale => {
                tracing::warn!(
                    service_id = %service_id,
                    last_seen = %health.last_seen,
                    "service heartbeat is stale; attempting tool request anyway"
                );
            }
            HealthStatus::Healthy => {}
        }
    }

    Ok(health)
}

pub(super) fn timeout(value: Option<i64>, default: u64) -> anyhow::Result<std::time::Duration> {
    let seconds = value.map(u64::try_from).transpose()?.unwrap_or(default);
    anyhow::ensure!(seconds > 0, "MCP timeout must be positive");
    Ok(std::time::Duration::from_secs(seconds))
}
