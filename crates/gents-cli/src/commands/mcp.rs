use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use gents::graphql::escape_graphql_string;
use gents::{
    health_checker::HealthCheckerOptions, run_health_check_cycle, HealthStatus,
    McpHealthCheckService, McpPool, ServiceHealthMap,
};
use serde::Serialize;

use crate::cli::args::{McpCommand, McpProbeArgs, McpRegisterArgs};
use crate::cli::output_format::OutputFormat;
use crate::config_writes::ConfigAccess;
use crate::{
    graphql_rows_or_empty_if_collection_missing, normalize_optional_string, parse_duration_suffix,
    print_json, resolve_config_access,
};

pub(crate) async fn dispatch(command: McpCommand) -> Result<()> {
    match command {
        McpCommand::Register(args) => mcp_register(args).await,
        McpCommand::Probe(args) => mcp_probe(args).await,
    }
}

async fn mcp_register(args: McpRegisterArgs) -> Result<()> {
    let service_id = args.service.trim();
    if service_id.is_empty() {
        anyhow::bail!("SERVICE must be a non-empty service id");
    }
    let endpoint = url::Url::parse(args.endpoint.trim()).context("parsing --endpoint URL")?;
    if endpoint.scheme() != "http" {
        anyhow::bail!("--endpoint must use http; the MCP registry currently stores HTTP endpoints");
    }
    if !endpoint.username().is_empty() || endpoint.password().is_some() {
        anyhow::bail!("--endpoint must not contain credentials");
    }
    if endpoint.query().is_some() || endpoint.fragment().is_some() {
        anyhow::bail!("--endpoint must not contain a query string or fragment");
    }
    let host = endpoint
        .host_str()
        .filter(|host| !host.trim().is_empty())
        .context("--endpoint must contain a host")?;
    let port = endpoint
        .port_or_known_default()
        .context("--endpoint must contain a port")?;
    let path = match endpoint.path().trim() {
        "" | "/" => "/mcp",
        path => path,
    };
    let (hostname, lan_ip) = if host.parse::<std::net::Ipv4Addr>().is_ok() {
        (None, Some(host))
    } else {
        (Some(host), None)
    };
    let display_name = args.display_name.as_deref().unwrap_or(service_id);
    let description = args.description.as_deref().unwrap_or("");
    let hostname_add = nullable_graphql_string("hostname", hostname);
    let lan_ip_add = nullable_graphql_string("lan_ip", lan_ip);
    let mutation = format!(
        r#"mutation {{
  upsert_ToolServiceRegistry(
    filter: {{ service_id: {{ _eq: "{service_id}" }} }}
    add: {{
      service_id: "{service_id}"
      display_name: "{display_name}"
      description: "{description}"
      {hostname_add}
      tailscale_ip: null
      {lan_ip_add}
      mcp_port: {port}
      mcp_path: "{path}"
      send_node_did: {send_node_did}
      status: "online"
      version: "{version}"
    }}
    update: {{
      display_name: "{display_name}"
      description: "{description}"
      {hostname_add}
      tailscale_ip: null
      {lan_ip_add}
      mcp_port: {port}
      mcp_path: "{path}"
      send_node_did: {send_node_did}
      status: "online"
      version: "{version}"
    }}
  ) {{ _docID service_id }}
}}"#,
        service_id = escape_graphql_string(service_id),
        display_name = escape_graphql_string(display_name),
        description = escape_graphql_string(description),
        path = escape_graphql_string(path),
        send_node_did = args.send_node_did,
        version = escape_graphql_string(args.version.trim()),
    );
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    access
        .write("cli.mcp.register", &mutation)
        .await
        .context("registering MCP service")?;
    println!("registered {service_id} -> {}", args.endpoint.trim());
    Ok(())
}

fn nullable_graphql_string(field: &str, value: Option<&str>) -> String {
    match value {
        Some(value) => format!(r#"{field}: "{}""#, escape_graphql_string(value)),
        None => format!("{field}: null"),
    }
}

async fn mcp_probe(args: McpProbeArgs) -> Result<()> {
    let target = ProbeTarget::from_args(&args)?;
    let timeout = parse_duration_suffix(&args.timeout)?;
    if timeout.is_zero() {
        anyhow::bail!("--timeout must be greater than zero");
    }

    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let services = load_mcp_services(&access, target.service_id()).await?;
    if matches!(target, ProbeTarget::Single(_)) && services.is_empty() {
        anyhow::bail!(
            "no online MCP service matched {}",
            target.service_id().unwrap_or_default()
        );
    }

    let local_hostname = hostname::get()
        .map(|host| host.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let snapshots = probe_services(services, timeout, &local_hostname, None).await;
    let report = McpProbeReport {
        generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        timeout_ms: duration_ms(timeout),
        count: snapshots.len(),
        items: snapshots,
    };

    match args
        .output
        .ensure_supported("mcp probe", &[OutputFormat::Text, OutputFormat::Json])?
    {
        OutputFormat::Json => print_json(&serde_json::to_value(report)?),
        OutputFormat::Text => {
            print_probe_table(&report.items);
            Ok(())
        }
        _ => unreachable!("ensure_supported restricts mcp probe output formats"),
    }
}

async fn load_mcp_services(
    access: &ConfigAccess,
    service_id: Option<&str>,
) -> Result<Vec<McpHealthCheckService>> {
    // The exact `service_id` read carries no `order`: DefraDB plans an ordered
    // read as a `service_id` index scan and applies `limit` to the scanned
    // document list before the scan's `status` filter, so an offline row
    // sharing the id would hide the online one.
    let registry_args = match service_id {
        Some(service_id) => format!(
            r#"filter: {{ _and: [{{ status: {{ _eq: "online" }} }}, {{ service_id: {{ _eq: "{}" }} }}] }}, limit: 1"#,
            escape_graphql_string(service_id)
        ),
        None => r#"filter: { status: { _eq: "online" } }, order: { service_id: ASC }"#.to_string(),
    };
    let query = format!(
        r#"{{
            ToolServiceRegistry({registry_args}) {{
                service_id
                hostname
                tailscale_ip
                lan_ip
                mcp_port
                mcp_path
                send_node_did
                updated_at
            }}
        }}"#
    );
    let rows = graphql_rows_or_empty_if_collection_missing(access, "ToolServiceRegistry", &query)
        .await
        .context("loading online MCP service registry rows")?;
    rows.into_iter()
        .map(|row| serde_json::from_value(row).context("parsing ToolServiceRegistry row"))
        .collect()
}

async fn probe_services(
    services: Vec<McpHealthCheckService>,
    timeout: Duration,
    local_hostname: &str,
    local_subnet: Option<&str>,
) -> Vec<McpProbeSnapshot> {
    let pool = McpPool::new();
    let mut handles = Vec::with_capacity(services.len());
    for service in services {
        let service_id = service.service_id.clone();
        let pool = pool.clone();
        let local_hostname = local_hostname.to_string();
        let local_subnet = local_subnet.map(ToOwned::to_owned);
        handles.push((
            service_id,
            tokio::spawn(async move {
                probe_service(
                    service,
                    &pool,
                    timeout,
                    &local_hostname,
                    local_subnet.as_deref(),
                )
                .await
            }),
        ));
    }

    let mut snapshots = Vec::with_capacity(handles.len());
    for (service_id, handle) in handles {
        match handle.await {
            Ok(snapshot) => snapshots.push(snapshot),
            Err(error) => snapshots.push(McpProbeSnapshot {
                service: service_id,
                health_state: HealthStatus::Unreachable.to_string(),
                latency_ms: 0,
                last_error: Some(format!("probe task failed: {error}")),
            }),
        }
    }
    snapshots
}

async fn probe_service(
    service: McpHealthCheckService,
    pool: &McpPool,
    timeout: Duration,
    local_hostname: &str,
    local_subnet: Option<&str>,
) -> McpProbeSnapshot {
    let service_id = service.service_id.clone();
    let health_map = ServiceHealthMap::new();
    let started = Instant::now();
    let result = tokio::time::timeout(
        timeout,
        run_health_check_cycle(
            vec![service],
            Utc::now(),
            pool,
            &health_map,
            local_hostname,
            local_subnet,
            &one_shot_probe_options(timeout),
            None,
        ),
    )
    .await;
    let latency_ms = elapsed_ms(started);

    match result {
        Ok(Ok(())) => match health_map.get(&service_id).await {
            Some(health) => McpProbeSnapshot {
                service: service_id,
                health_state: health.status.to_string(),
                latency_ms,
                last_error: health.last_error,
            },
            None => McpProbeSnapshot {
                service: service_id,
                health_state: HealthStatus::Unreachable.to_string(),
                latency_ms,
                last_error: Some("probe produced no health snapshot".to_string()),
            },
        },
        Ok(Err(error)) => McpProbeSnapshot {
            service: service_id,
            health_state: HealthStatus::Unreachable.to_string(),
            latency_ms,
            last_error: Some(error.to_string()),
        },
        Err(_) => McpProbeSnapshot {
            service: service_id,
            health_state: HealthStatus::Unreachable.to_string(),
            latency_ms,
            last_error: Some("probe timed out".to_string()),
        },
    }
}

fn print_probe_table(rows: &[McpProbeSnapshot]) {
    let headers = ["SERVICE", "HEALTH_STATE", "LATENCY_MS", "LAST_ERROR"];
    let rendered_rows = rows
        .iter()
        .map(|row| {
            [
                display_cell(&row.service),
                display_cell(&row.health_state),
                row.latency_ms.to_string(),
                row.last_error.clone().unwrap_or_else(|| "-".to_string()),
            ]
        })
        .collect::<Vec<_>>();
    let mut widths = headers.map(str::len);
    for row in &rendered_rows {
        for (idx, cell) in row.iter().enumerate() {
            widths[idx] = widths[idx].max(cell.len());
        }
    }

    print_table_row(&headers.map(|header| header.to_string()), &widths);
    print_table_row(&widths.map(|width| "-".repeat(width)), &widths);
    for row in &rendered_rows {
        print_table_row(row, &widths);
    }
}

fn display_cell(value: &str) -> String {
    if value.trim().is_empty() {
        "-".to_string()
    } else {
        value.to_string()
    }
}

fn print_table_row<const N: usize>(cells: &[String; N], widths: &[usize; N]) {
    let line = cells
        .iter()
        .enumerate()
        .map(|(idx, cell)| format!("{cell:<width$}", width = widths[idx]))
        .collect::<Vec<_>>()
        .join("  ");
    println!("{line}");
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn one_shot_probe_options(timeout: Duration) -> HealthCheckerOptions {
    HealthCheckerOptions {
        probe_timeout: timeout,
        failure_threshold_k: 1,
        ..HealthCheckerOptions::default()
    }
}

enum ProbeTarget {
    Single(String),
    All,
}

impl ProbeTarget {
    fn from_args(args: &McpProbeArgs) -> Result<Self> {
        let service = normalize_optional_string(args.service.as_deref());
        match (args.all, service) {
            (true, None) => Ok(Self::All),
            (false, Some(service_id)) => Ok(Self::Single(service_id)),
            (true, Some(_)) => anyhow::bail!("provide either <service> or --all, not both"),
            (false, None) => anyhow::bail!("provide a service id or --all"),
        }
    }

    fn service_id(&self) -> Option<&str> {
        match self {
            Self::Single(service_id) => Some(service_id.as_str()),
            Self::All => None,
        }
    }
}

#[derive(Serialize)]
struct McpProbeReport {
    generated_at: String,
    timeout_ms: u64,
    count: usize,
    items: Vec<McpProbeSnapshot>,
}

#[derive(Serialize)]
struct McpProbeSnapshot {
    service: String,
    health_state: String,
    latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// DefraDB's ordered read collapses to the lowest public document id, so the
    /// offline row has to hold the lowest id and the online row the highest for
    /// an ordered read to hide the online row.
    async fn registry_with_an_offline_twin() -> (ConfigAccess, String) {
        let node = Arc::new(
            gents::defra_node::EmbeddedNode::builder()
                .build()
                .await
                .unwrap(),
        );
        gents::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let access = ConfigAccess::Local(node);
        for host in ["twin-a", "twin-b", "twin-c"] {
            let host = escape_graphql_string(host);
            access
                .write(
                    "test.mcp.seed",
                    &format!(
                        r#"mutation {{ create_ToolServiceRegistry(input: {{ service_id: "svc-1", node_did: "did:key:{host}", hostname: "{host}", status: "pending", mcp_port: 1, mcp_path: "/mcp", enabled: true }}) {{ _docID }} }}"#
                    ),
                )
                .await
                .unwrap();
        }
        let response = access
            .execute(
                r#"{ ToolServiceRegistry(filter: { service_id: { _eq: "svc-1" } }) { _docID hostname } }"#,
            )
            .await
            .unwrap();
        let mut rows: Vec<(String, String)> = response["data"]["ToolServiceRegistry"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row["_docID"].as_str().unwrap().to_string(),
                    row["hostname"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        rows.sort();
        assert_eq!(rows.len(), 3);
        for (doc_id, status) in [
            (rows.first().unwrap().0.clone(), "offline"),
            (rows.last().unwrap().0.clone(), "online"),
        ] {
            let doc_id = escape_graphql_string(&doc_id);
            let status = escape_graphql_string(status);
            access
                .write(
                    "test.mcp.status",
                    &format!(
                        r#"mutation {{ update_ToolServiceRegistry(docID: "{doc_id}", input: {{ status: "{status}" }}) {{ _docID }} }}"#
                    ),
                )
                .await
                .unwrap();
        }
        let online_hostname = rows.last().unwrap().1.clone();
        (access, online_hostname)
    }

    #[tokio::test]
    async fn exact_service_read_finds_the_online_row_behind_an_offline_twin() {
        let (access, online_hostname) = registry_with_an_offline_twin().await;
        let services = load_mcp_services(&access, Some("svc-1")).await.unwrap();
        let hostnames: Vec<&str> = services
            .iter()
            .map(|service| service.hostname.as_str())
            .collect();
        assert_eq!(hostnames, [online_hostname.as_str()]);
    }
}
