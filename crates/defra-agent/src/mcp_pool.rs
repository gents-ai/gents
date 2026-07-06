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
use tracing::Instrument;

pub const AGENT_DID_HEADER: &str = "x-agent-did";

/// Bound on establishing an MCP connection (TCP connect + MCP handshake).
/// A blackholed endpoint — e.g. the tailscale address of a powered-off host —
/// drops packets silently, so without this bound the connect future never
/// resolves (#622). rmcp's transport carries no timeout of its own.
const MCP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Bound on one tools/list round-trip. list_tools is a safe read on
/// interactive paths (discover / describe / argument preflight) and the pool
/// retries it once after eviction, so a caller waits at most two of these.
const MCP_LIST_TOOLS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Wrapper that stores list_tools / call_tool closures over a concrete
/// `RunningService` so that the pool doesn't need to name the transport
/// generic parameter (which includes rmcp's internal reqwest 0.13 Client).
type ListToolsFuture = Pin<Box<dyn Future<Output = Result<ListToolsResult>> + Send + 'static>>;
type CallToolFuture = Pin<Box<dyn Future<Output = Result<CallToolResult>> + Send + 'static>>;
type ConnectFuture = Pin<Box<dyn Future<Output = Result<McpConnection>> + Send + 'static>>;
type ListToolsFn = dyn Fn() -> ListToolsFuture + Send + Sync;
type CallToolFn = dyn Fn(CallToolRequestParams) -> CallToolFuture + Send + Sync;
type ConnectFn =
    dyn Fn(String, String, Option<String>, HashMap<String, String>) -> ConnectFuture + Send + Sync;
type TraceContextHeadersFn = dyn Fn() -> HashMap<String, String> + Send + Sync;

struct McpConnection {
    endpoint: String,
    agent_did_header: Option<String>,
    trace_context_headers: HashMap<String, String>,
    list_tools_fn: Box<ListToolsFn>,
    call_tool_fn: Box<CallToolFn>,
    last_used: std::sync::Mutex<std::time::Instant>,
}

impl McpConnection {
    fn touch(&self) {
        *self.last_used.lock().expect("last_used lock") = std::time::Instant::now();
    }

    fn idle_longer_than(&self, ttl: std::time::Duration) -> bool {
        self.last_used.lock().expect("last_used lock").elapsed() > ttl
    }
}

fn fresh_last_used() -> std::sync::Mutex<std::time::Instant> {
    std::sync::Mutex::new(std::time::Instant::now())
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
        trace_context_headers: HashMap::new(),
        last_used: fresh_last_used(),
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
    trace_context_headers: &HashMap<String, String>,
) -> Result<StreamableHttpClientTransportConfig> {
    let mut config = StreamableHttpClientTransportConfig::with_uri(endpoint.to_string());
    let mut headers = std::collections::HashMap::new();
    if let Some(agent_did) = agent_did_header {
        headers.insert(
            HeaderName::from_static(AGENT_DID_HEADER),
            HeaderValue::from_str(agent_did).context("invalid agent DID header value")?,
        );
    }
    insert_trace_context_headers(&mut headers, trace_context_headers);
    if !headers.is_empty() {
        config = config.custom_headers(headers);
    }
    Ok(config)
}

fn insert_trace_context_headers(
    headers: &mut HashMap<HeaderName, HeaderValue>,
    trace_context_headers: &HashMap<String, String>,
) {
    for (name, value) in trace_context_headers {
        let Ok(name) = HeaderName::from_bytes(name.as_bytes()) else {
            continue;
        };
        let Ok(value) = HeaderValue::from_str(value.as_str()) else {
            continue;
        };
        headers.entry(name).or_insert(value);
    }
}

async fn connect_mcp_service(
    service_id: &str,
    endpoint: &str,
    agent_did_header: Option<&str>,
    trace_context_headers: HashMap<String, String>,
) -> Result<McpConnection> {
    let config =
        streamable_http_transport_config(endpoint, agent_did_header, &trace_context_headers)?;
    let transport = rmcp::transport::StreamableHttpClientTransport::from_config(config);
    let client = ()
        .serve(transport)
        .await
        .map_err(|e| anyhow::anyhow!("MCP handshake failed for {service_id} ({endpoint}): {e}"))?;
    let mut connection = wrap_connection(endpoint.to_string(), client);
    connection.agent_did_header = agent_did_header.map(ToOwned::to_owned);
    connection.trace_context_headers = trace_context_headers;
    Ok(connection)
}

fn default_connect_fn() -> Arc<ConnectFn> {
    Arc::new(
        |service_id: String,
         endpoint: String,
         agent_did_header: Option<String>,
         trace_context_headers: HashMap<String, String>| {
            Box::pin(async move {
                connect_mcp_service(
                    &service_id,
                    &endpoint,
                    agent_did_header.as_deref(),
                    trace_context_headers,
                )
                .await
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
    trace_context_headers_fn: Arc<TraceContextHeadersFn>,
    idle_ttl: Option<std::time::Duration>,
}

/// Default idle TTL for pooled MCP connections.
///
/// A connection idle past this is replaced (and its session terminated) on
/// next use. Bounds how long a wedged transport — e.g. one stuck in the rmcp
/// dead-session SSE resume loop — can live unattended.
pub const DEFAULT_MCP_IDLE_TTL: std::time::Duration = std::time::Duration::from_secs(15 * 60);

impl McpPool {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            connect_fn: default_connect_fn(),
            trace_context_headers_fn: Arc::new(crate::runtime_trace::current_trace_context_headers),
            idle_ttl: Some(DEFAULT_MCP_IDLE_TTL),
        }
    }

    /// Replace connections idle longer than `ttl` on their next use (the old
    /// session is terminated). Bounds how long a wedged transport can live.
    pub fn with_idle_ttl(mut self, ttl: std::time::Duration) -> Self {
        self.idle_ttl = Some(ttl);
        self
    }

    #[cfg(test)]
    fn new_with_connector<F, Fut>(connector: F) -> Self
    where
        F: Fn(String, String, Option<String>, HashMap<String, String>) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: Future<Output = Result<McpConnection>> + Send + 'static,
    {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            connect_fn: Arc::new(
                move |service_id, endpoint, agent_did_header, trace_headers| {
                    Box::pin(connector(
                        service_id,
                        endpoint,
                        agent_did_header,
                        trace_headers,
                    ))
                },
            ),
            trace_context_headers_fn: Arc::new(crate::runtime_trace::current_trace_context_headers),
            idle_ttl: None,
        }
    }

    #[cfg(test)]
    fn with_trace_context_headers<F>(mut self, headers: F) -> Self
    where
        F: Fn() -> HashMap<String, String> + Send + Sync + 'static,
    {
        self.trace_context_headers_fn = Arc::new(headers);
        self
    }

    #[cfg(test)]
    pub(crate) fn new_with_list_tools_handler<F, Fut>(handler: F) -> Self
    where
        F: Fn(String, String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<ListToolsResult>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        Self::new_with_connector(
            move |service_id, endpoint, agent_did_header, trace_headers| {
                let handler = Arc::clone(&handler);
                async move {
                    let service_id_for_list = service_id.clone();
                    let endpoint_for_list = endpoint.clone();
                    Ok(McpConnection {
                        endpoint,
                        agent_did_header,
                        trace_context_headers: trace_headers,
                        last_used: fresh_last_used(),
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
            },
        )
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
        async {
            self.get_or_connect(service_id, endpoint, agent_did).await?;
            match self.list_tools_once(service_id).await {
                Ok(result) => {
                    tracing::Span::current().record("tool_count", result.tools.len() as i64);
                    Ok(result)
                }
                Err(error) => {
                    tracing::Span::current().record("retried", true);
                    tracing::warn!(
                        service_id = %service_id,
                        error = %error,
                        "MCP list_tools failed, evicting connection and retrying"
                    );
                    self.remove(service_id).await;
                    self.get_or_connect(service_id, endpoint, agent_did).await?;
                    let result = self.list_tools_once(service_id).await?;
                    tracing::Span::current().record("tool_count", result.tools.len() as i64);
                    Ok(result)
                }
            }
        }
        .instrument(tracing::info_span!(
            "mcp.list_tools",
            service_id = %service_id,
            endpoint = %endpoint,
            agent_did_bound = agent_did.is_some(),
            retried = false,
            tool_count = tracing::field::Empty,
        ))
        .await
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
        let argument_count = argument_count(&arguments);
        async {
            self.get_or_connect(service_id, endpoint, agent_did).await?;
            let result = self
                .call_tool_once(service_id, build_call_tool_params(tool_name, arguments))
                .await;
            if let Ok(result) = &result {
                tracing::Span::current()
                    .record("mcp_result_is_error", result.is_error.unwrap_or(false));
            }
            result
        }
        .instrument(tracing::info_span!(
            "mcp.call_tool",
            service_id = %service_id,
            endpoint = %endpoint,
            tool_name = %tool_name,
            agent_did_bound = agent_did.is_some(),
            argument_count = argument_count as i64,
            mcp_result_is_error = tracing::field::Empty,
        ))
        .await
    }

    pub async fn remove(&self, service_id: &str) {
        let mut guard = self.inner.write().await;
        guard.remove(service_id);
    }

    async fn list_tools_once(&self, service_id: &str) -> Result<ListToolsResult> {
        // The closure's future owns an Arc of the client, so it can be
        // awaited after the guard drops — a slow list call must not hold the
        // pool lock and block unrelated services (#622).
        let list_tools = {
            let guard = self.inner.read().await;
            let conn = guard
                .get(service_id)
                .context("connection disappeared after get_or_connect")?;
            (conn.list_tools_fn)()
        };
        tokio::time::timeout(MCP_LIST_TOOLS_TIMEOUT, list_tools)
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "MCP list_tools on '{service_id}' timed out after {}s",
                    MCP_LIST_TOOLS_TIMEOUT.as_secs()
                )
            })?
    }

    async fn call_tool_once(
        &self,
        service_id: &str,
        params: CallToolRequestParams,
    ) -> Result<CallToolResult> {
        // No pool-level timeout here — tool calls are bounded by the caller's
        // health-keyed budget (meta_tools/call.rs). The guard still must not
        // be held across the await (#622).
        let call_tool = {
            let guard = self.inner.read().await;
            let conn = guard
                .get(service_id)
                .context("connection disappeared after get_or_connect")?;
            (conn.call_tool_fn)(params)
        };
        call_tool.await
    }

    // -----------------------------------------------------------------------
    // Internal: double-checked locking connect
    // -----------------------------------------------------------------------

    /// Ensure a valid connection for `service_id` exists in the pool.
    ///
    /// 1. Read-lock: check if a connection exists and the endpoint matches.
    /// 2. If not, re-check under the write lock, then connect with no lock
    ///    held and insert the result. Duplicate racing connects are benign.
    async fn get_or_connect(
        &self,
        service_id: &str,
        endpoint: &str,
        agent_did: Option<&str>,
    ) -> Result<()> {
        let agent_did_header = agent_did.map(ToOwned::to_owned);
        // rmcp's streamable HTTP worker keeps custom headers in its transport
        // config, so trace context is part of the connection context.
        let trace_context_headers = (self.trace_context_headers_fn)();
        // Fast path — read lock
        {
            let guard = self.inner.read().await;
            if let Some(conn) = guard.get(service_id) {
                if conn.endpoint == endpoint
                    && conn.agent_did_header == agent_did_header
                    && conn.trace_context_headers == trace_context_headers
                    && !self.idle_ttl.is_some_and(|ttl| conn.idle_longer_than(ttl))
                {
                    conn.touch();
                    return Ok(());
                }
            }
        }

        // Slow path — re-check under the lock, but connect OUTSIDE it: a hung
        // or slow connect (blackholed endpoint, #622) must not wedge every
        // other service in the pool.
        {
            let guard = self.inner.write().await;
            if let Some(conn) = guard.get(service_id) {
                let endpoint_changed = conn.endpoint != endpoint;
                let agent_did_changed = conn.agent_did_header != agent_did_header;
                let trace_context_changed = conn.trace_context_headers != trace_context_headers;
                let idle_ttl_expired = self.idle_ttl.is_some_and(|ttl| conn.idle_longer_than(ttl));
                if conn.endpoint == endpoint
                    && conn.agent_did_header == agent_did_header
                    && conn.trace_context_headers == trace_context_headers
                    && !idle_ttl_expired
                {
                    conn.touch();
                    return Ok(());
                }
                tracing::info!(
                    service_id,
                    old_endpoint = %conn.endpoint,
                    new_endpoint = %endpoint,
                    endpoint_changed,
                    agent_did_changed,
                    trace_context_changed,
                    idle_ttl_expired,
                    "MCP connection context changed, reconnecting"
                );
            }
        }

        tracing::info!(service_id, endpoint, "connecting MCP client");
        let connection = tokio::time::timeout(
            MCP_CONNECT_TIMEOUT,
            (self.connect_fn)(
                service_id.to_string(),
                endpoint.to_string(),
                agent_did_header,
                trace_context_headers,
            ),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "MCP connect to '{service_id}' ({endpoint}) timed out after {}s",
                MCP_CONNECT_TIMEOUT.as_secs()
            )
        })??;

        // Concurrent callers for the same service may both reach here; the
        // last insert wins and the loser's connection is dropped. The
        // handshake is idempotent and the cache is lazy, so this is benign —
        // strictly better than serializing every connect behind one lock.
        let mut guard = self.inner.write().await;
        guard.insert(service_id.to_string(), connection);
        Ok(())
    }
}

impl Default for McpPool {
    fn default() -> Self {
        Self::new()
    }
}

fn argument_count(arguments: &serde_json::Value) -> usize {
    match arguments {
        serde_json::Value::Object(map) => map.len(),
        serde_json::Value::Null => 0,
        _ => 1,
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
