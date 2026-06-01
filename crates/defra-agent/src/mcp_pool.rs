//! MCP client connection pool — lazy-connect, cached connections to remote MCP servers.
//!
//! Each data service (hf-data, x-data, coding-session, etc.) exposes an MCP
//! endpoint.  The pool connects on first use and caches the running client for
//! subsequent `list_tools` / `call_tool` calls.

use std::collections::HashMap;
use std::future::Future;
use std::net::Ipv4Addr;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Context, Result};
use reqwest::header::{HeaderName, HeaderValue};
use rmcp::{
    model::{CallToolRequestParams, CallToolResult, ListToolsResult},
    transport::streamable_http_client::StreamableHttpClientTransportConfig,
    RoleClient, ServiceExt,
};
use tokio::sync::RwLock;

pub const AGENT_DID_HEADER: &str = "x-agent-did";

/// Wrapper that stores list_tools / call_tool closures over a concrete
/// `RunningService` so that the pool doesn't need to name the transport
/// generic parameter (which includes rmcp's internal reqwest 0.13 Client).
type ListToolsFuture = Pin<Box<dyn Future<Output = Result<ListToolsResult>> + Send + 'static>>;
type CallToolFuture = Pin<Box<dyn Future<Output = Result<CallToolResult>> + Send + 'static>>;
type ConnectFuture = Pin<Box<dyn Future<Output = Result<McpConnection>> + Send + 'static>>;
type ListToolsFn = dyn Fn() -> ListToolsFuture + Send + Sync;
type CallToolFn = dyn Fn(CallToolRequestParams) -> CallToolFuture + Send + Sync;
type ConnectFn = dyn Fn(String, String, Option<String>) -> ConnectFuture + Send + Sync;

struct McpConnection {
    endpoint: String,
    agent_did_header: Option<String>,
    list_tools_fn: Box<ListToolsFn>,
    call_tool_fn: Box<CallToolFn>,
}

fn wrap_connection<S>(
    endpoint: String,
    client: rmcp::service::RunningService<RoleClient, S>,
) -> McpConnection
where
    S: rmcp::service::Service<RoleClient> + Send + Sync + 'static,
{
    let client = Arc::new(client);
    let c1 = Arc::clone(&client);
    let c2 = Arc::clone(&client);

    McpConnection {
        endpoint,
        agent_did_header: None,
        list_tools_fn: Box::new(move || {
            let c = Arc::clone(&c1);
            Box::pin(async move {
                c.peer()
                    .list_tools(None)
                    .await
                    .map_err(|e| anyhow::anyhow!("list_tools: {e}"))
            })
        }),
        call_tool_fn: Box::new(move |params| {
            let c = Arc::clone(&c2);
            Box::pin(async move {
                c.peer()
                    .call_tool(params)
                    .await
                    .map_err(|e| anyhow::anyhow!("call_tool: {e}"))
            })
        }),
    }
}

fn streamable_http_transport_config(
    endpoint: &str,
    agent_did_header: Option<&str>,
) -> Result<StreamableHttpClientTransportConfig> {
    let mut config = StreamableHttpClientTransportConfig::with_uri(endpoint.to_string());
    if let Some(agent_did) = agent_did_header {
        let mut headers = std::collections::HashMap::new();
        headers.insert(
            HeaderName::from_static(AGENT_DID_HEADER),
            HeaderValue::from_str(agent_did).context("invalid agent DID header value")?,
        );
        config = config.custom_headers(headers);
    }
    Ok(config)
}

async fn connect_mcp_service(
    service_id: &str,
    endpoint: &str,
    agent_did_header: Option<&str>,
) -> Result<McpConnection> {
    let config = streamable_http_transport_config(endpoint, agent_did_header)?;
    let transport = rmcp::transport::StreamableHttpClientTransport::from_config(config);
    let client = ()
        .serve(transport)
        .await
        .map_err(|e| anyhow::anyhow!("MCP handshake failed for {service_id} ({endpoint}): {e}"))?;
    let mut connection = wrap_connection(endpoint.to_string(), client);
    connection.agent_did_header = agent_did_header.map(ToOwned::to_owned);
    Ok(connection)
}

fn default_connect_fn() -> Arc<ConnectFn> {
    Arc::new(
        |service_id: String, endpoint: String, agent_did_header: Option<String>| {
            Box::pin(async move {
                connect_mcp_service(&service_id, &endpoint, agent_did_header.as_deref()).await
            })
        },
    )
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolExecutionOperation {
    McpListTools,
    McpCall,
    NativeCommand,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolIdempotencyEvidence {
    Unknown,
    Idempotent,
    NonIdempotent,
}

#[cfg(test)]
pub(crate) use crate::tool_call_lifecycle::FailureClass as ToolFailureClass;

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolRetryDisposition {
    DoNotRetry,
    RetrySafeRead,
    RetryIdempotentToolCall,
}

#[cfg(test)]
impl ToolRetryDisposition {
    pub(crate) fn as_contract(self) -> &'static str {
        match self {
            Self::DoNotRetry => "doNotRetry",
            Self::RetrySafeRead => "retrySafeRead",
            Self::RetryIdempotentToolCall => "retryIdempotentToolCall",
        }
    }
}

/// Test-only mirror of `Proofs.ToolExecution.retryDisposition`.
///
/// Production retry behavior is still encoded by the `list_tools` safe-read
/// retry path and the absence of a `call_tool` retry loop.
#[cfg(test)]
pub(crate) fn tool_retry_disposition(
    operation: ToolExecutionOperation,
    idempotency: ToolIdempotencyEvidence,
    failure: ToolFailureClass,
) -> ToolRetryDisposition {
    match (operation, idempotency, failure) {
        (ToolExecutionOperation::McpListTools, _, ToolFailureClass::Transport) => {
            ToolRetryDisposition::RetrySafeRead
        }
        (
            ToolExecutionOperation::McpCall,
            ToolIdempotencyEvidence::Idempotent,
            ToolFailureClass::Transport,
        ) => ToolRetryDisposition::RetryIdempotentToolCall,
        _ => ToolRetryDisposition::DoNotRetry,
    }
}

/// Connection pool for MCP data-service clients.
///
/// Connections are created lazily on the first `list_tools` or `call_tool`
/// request for a given `service_id` and reused for subsequent calls.
///
/// `McpPool` is cheaply cloneable (all state lives behind `Arc`).
#[derive(Clone)]
pub struct McpPool {
    inner: Arc<RwLock<HashMap<String, McpConnection>>>,
    connect_fn: Arc<ConnectFn>,
}

impl McpPool {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            connect_fn: default_connect_fn(),
        }
    }

    #[cfg(test)]
    fn new_with_connector<F, Fut>(connector: F) -> Self
    where
        F: Fn(String, String, Option<String>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<McpConnection>> + Send + 'static,
    {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            connect_fn: Arc::new(move |service_id, endpoint, agent_did_header| {
                Box::pin(connector(service_id, endpoint, agent_did_header))
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn new_with_list_tools_handler<F, Fut>(handler: F) -> Self
    where
        F: Fn(String, String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<ListToolsResult>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        Self::new_with_connector(move |service_id, endpoint, agent_did_header| {
            let handler = Arc::clone(&handler);
            async move {
                let service_id_for_list = service_id.clone();
                let endpoint_for_list = endpoint.clone();
                Ok(McpConnection {
                    endpoint,
                    agent_did_header,
                    list_tools_fn: Box::new(move || {
                        let handler = Arc::clone(&handler);
                        let service_id = service_id_for_list.clone();
                        let endpoint = endpoint_for_list.clone();
                        Box::pin(async move { handler(service_id, endpoint).await })
                    }),
                    call_tool_fn: Box::new(|_params| {
                        Box::pin(async { anyhow::bail!("call_tool was not expected") })
                    }),
                })
            }
        })
    }

    /// List the tools exposed by the MCP server at `endpoint`.
    ///
    /// Connects lazily — if no cached connection exists for `service_id`, one is
    /// created.  If the cached connection points at a *different* endpoint the
    /// old connection is dropped and a fresh one is opened.
    pub async fn list_tools(&self, service_id: &str, endpoint: &str) -> Result<ListToolsResult> {
        self.list_tools_with_agent_did(service_id, endpoint, None)
            .await
    }

    pub async fn list_tools_with_agent_did(
        &self,
        service_id: &str,
        endpoint: &str,
        agent_did: Option<&str>,
    ) -> Result<ListToolsResult> {
        self.get_or_connect(service_id, endpoint, agent_did).await?;
        match self.list_tools_once(service_id).await {
            Ok(result) => Ok(result),
            Err(error) => {
                tracing::warn!(
                    service_id = %service_id,
                    error = %error,
                    "MCP list_tools failed, evicting connection and retrying"
                );
                self.remove(service_id).await;
                self.get_or_connect(service_id, endpoint, agent_did).await?;
                self.list_tools_once(service_id).await
            }
        }
    }

    /// Call a tool on the MCP server at `endpoint`.
    ///
    /// Connects lazily, same as [`list_tools`](Self::list_tools). Unlike
    /// `list_tools`, this does not retry after a dispatch failure: repeating an
    /// MCP tool call can repeat side effects until services advertise
    /// idempotency metadata that `Proofs.ToolExecution` can model.
    /// A dead cached connection can therefore keep failing `call_tool` until
    /// `list_tools` or explicit removal evicts it; this keeps recovery from
    /// retransmitting a possibly mutating call.
    pub async fn call_tool(
        &self,
        service_id: &str,
        endpoint: &str,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<CallToolResult> {
        self.call_tool_with_agent_did(service_id, endpoint, tool_name, arguments, None)
            .await
    }

    pub async fn call_tool_with_agent_did(
        &self,
        service_id: &str,
        endpoint: &str,
        tool_name: &str,
        arguments: serde_json::Value,
        agent_did: Option<&str>,
    ) -> Result<CallToolResult> {
        self.get_or_connect(service_id, endpoint, agent_did).await?;
        self.call_tool_once(service_id, build_call_tool_params(tool_name, arguments))
            .await
    }

    pub async fn remove(&self, service_id: &str) {
        let mut guard = self.inner.write().await;
        guard.remove(service_id);
    }

    async fn list_tools_once(&self, service_id: &str) -> Result<ListToolsResult> {
        let guard = self.inner.read().await;
        let conn = guard
            .get(service_id)
            .context("connection disappeared after get_or_connect")?;
        (conn.list_tools_fn)().await
    }

    async fn call_tool_once(
        &self,
        service_id: &str,
        params: CallToolRequestParams,
    ) -> Result<CallToolResult> {
        let guard = self.inner.read().await;
        let conn = guard
            .get(service_id)
            .context("connection disappeared after get_or_connect")?;
        (conn.call_tool_fn)(params).await
    }

    // -----------------------------------------------------------------------
    // Internal: double-checked locking connect
    // -----------------------------------------------------------------------

    /// Ensure a valid connection for `service_id` exists in the pool.
    ///
    /// Uses a double-checked locking pattern:
    /// 1. Read-lock: check if a connection exists and the endpoint matches.
    /// 2. If not, take write-lock and re-check before actually connecting.
    async fn get_or_connect(
        &self,
        service_id: &str,
        endpoint: &str,
        agent_did: Option<&str>,
    ) -> Result<()> {
        let agent_did_header = agent_did.map(ToOwned::to_owned);
        // Fast path — read lock
        {
            let guard = self.inner.read().await;
            if let Some(conn) = guard.get(service_id) {
                if conn.endpoint == endpoint && conn.agent_did_header == agent_did_header {
                    return Ok(());
                }
            }
        }

        // Slow path — write lock, re-check
        let mut guard = self.inner.write().await;
        if let Some(conn) = guard.get(service_id) {
            if conn.endpoint == endpoint && conn.agent_did_header == agent_did_header {
                return Ok(());
            }
            tracing::info!(
                service_id,
                old = %conn.endpoint,
                new = %endpoint,
                "MCP connection context changed, reconnecting"
            );
        }

        tracing::info!(service_id, endpoint, "connecting MCP client");
        let connection = (self.connect_fn)(
            service_id.to_string(),
            endpoint.to_string(),
            agent_did_header,
        )
        .await?;
        guard.insert(service_id.to_string(), connection);
        Ok(())
    }
}

impl Default for McpPool {
    fn default() -> Self {
        Self::new()
    }
}

fn build_call_tool_params(tool_name: &str, arguments: serde_json::Value) -> CallToolRequestParams {
    let json_args = match arguments {
        serde_json::Value::Object(map) => Some(map),
        serde_json::Value::Null => None,
        other => {
            let mut map = serde_json::Map::new();
            map.insert("input".to_string(), other);
            Some(map)
        }
    };

    let mut params = CallToolRequestParams::new(tool_name.to_string());
    if let Some(args) = json_args {
        params = params.with_arguments(args);
    }
    params
}

/// Resolve the best MCP URL for a service given local network context.
///
/// Priority: localhost (same host) > LAN IP (same subnet) > Tailscale IP.
pub fn resolve_mcp_url(
    service_hostname: &str,
    service_tailscale_ip: &str,
    service_lan_ip: &str,
    mcp_port: u16,
    mcp_path: &str,
    local_hostname: &str,
    local_subnet_cidr: Option<&str>,
) -> String {
    let path = normalize_mcp_path(mcp_path);

    if !service_hostname.is_empty() && service_hostname == local_hostname {
        return format!("http://127.0.0.1:{mcp_port}{path}");
    }

    if !service_lan_ip.is_empty() {
        if let Some(cidr) = local_subnet_cidr {
            if ip_in_cidr(service_lan_ip, cidr) {
                return format!("http://{service_lan_ip}:{mcp_port}{path}");
            }
        }
    }

    let host = if !service_tailscale_ip.is_empty() {
        service_tailscale_ip
    } else if !service_lan_ip.is_empty() {
        service_lan_ip
    } else {
        service_hostname
    };

    format!("http://{host}:{mcp_port}{path}")
}

fn ip_in_cidr(ip: &str, cidr: &str) -> bool {
    let Some((network, prefix)) = cidr.split_once('/') else {
        return false;
    };
    let Ok(ip_addr) = ip.parse::<Ipv4Addr>() else {
        return false;
    };
    let Ok(net_addr) = network.parse::<Ipv4Addr>() else {
        return false;
    };
    let Ok(prefix_len) = prefix.parse::<u32>() else {
        return false;
    };
    if prefix_len > 32 {
        return false;
    }

    let mask = if prefix_len == 0 {
        0u32
    } else {
        !0u32 << (32 - prefix_len)
    };
    (u32::from(ip_addr) & mask) == (u32::from(net_addr) & mask)
}

fn normalize_mcp_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        "/mcp".to_string()
    } else if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

#[cfg(test)]
mod tests;
