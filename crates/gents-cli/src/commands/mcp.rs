use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use gents_protocol::operations::mcp_service::McpProbeReceipt;
use gents_tools::mcp::pool::McpPool;
use serde::Serialize;

use crate::cli::args::{McpCommand, McpProbeArgs, McpRegisterArgs};
use crate::cli::output_format::OutputFormat;
use crate::config_writes::ConfigAccess;
use crate::{parse_duration_suffix, print_json, resolve_config_access};
use gents_host::http_client::normalize_optional_string;

pub(crate) async fn dispatch(command: McpCommand) -> Result<()> {
    match command {
        McpCommand::Register(args) => mcp_register(args).await,
        McpCommand::Probe(args) => mcp_probe(args).await,
    }
}

async fn mcp_register(args: McpRegisterArgs) -> Result<()> {
    use gents_protocol::operations::mcp_service::{McpRegisterInput, Toggle};
    use gents_protocol::operations::StringUpdate;

    if args.version.trim() != "unversioned" {
        anyhow::bail!("--version is not supported by the MCP service configuration");
    }
    let node_did = gents_service::home_state::resolve_node_did(
        args.home.as_deref(),
        args.node_did.as_deref(),
    )?;
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    if matches!(&access, ConfigAccess::Local(_)) {
        anyhow::bail!("MCP registration requires a running runtime or --graphql for client-signed publication");
    }
    let toggle = |flag: Option<bool>| match flag {
        None => Toggle::Omitted,
        Some(true) => Toggle::On,
        Some(false) => Toggle::Off,
    };
    let text = |value: &Option<String>| match value {
        Some(value) => StringUpdate::Set(value.clone()),
        None => StringUpdate::Omitted,
    };
    let input = McpRegisterInput {
        service_id: args.service.clone(),
        endpoint: args.endpoint.clone(),
        display_name: text(&args.display_name),
        description: text(&args.description),
        send_node_did: toggle(args.send_node_did),
        enabled: toggle(args.enabled),
    };
    let receipt = gents::operations::mcp_service::register(&access, &node_did, &input)
        .await
        .map_err(|error| {
            use crate::commands::refusal::Refusal;
            use gents_protocol::operations::mcp_service::McpServiceOperationError;
            let next = error.recovery();
            match error {
                McpServiceOperationError::Endpoint { source } => {
                    anyhow::anyhow!(
                        "--endpoint: {source}\nNext: {}",
                        crate::next_call::command(next, None)
                    )
                }
                other => Refusal::new(other, next).into(),
            }
        })
        .context("registering MCP service")?;
    println!("registered {} -> {}", receipt.id, args.endpoint.trim());
    Ok(())
}

async fn mcp_probe(args: McpProbeArgs) -> Result<()> {
    let target = ProbeTarget::from_args(&args)?;
    let timeout = parse_duration_suffix(&args.timeout)?;
    if timeout.is_zero() {
        anyhow::bail!("--timeout must be greater than zero");
    }

    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let node_did = gents_service::home_state::resolve_node_did(
        args.home.as_deref(),
        args.node_did.as_deref(),
    )?;
    let services =
        gents_tools::mcp::probe::enabled_services(&access, &node_did, target.service_id())
            .await
            .context("loading MCP service registry rows")?;
    if matches!(target, ProbeTarget::Single(_)) && services.is_empty() {
        anyhow::bail!(
            "no enabled MCP service matched {}",
            target.service_id().unwrap_or_default()
        );
    }

    let local_hostname = hostname::get()
        .map(|host| host.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let snapshots =
        probe_services(&access, &node_did, services, timeout, &local_hostname, None).await;
    let report = McpProbeReport {
        generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        timeout_ms: duration_ms(timeout),
        count: snapshots.len(),
        items: snapshots,
    };

    match args.output.ensure_supported(
        "config mcp probe",
        &[OutputFormat::Text, OutputFormat::Json],
    )? {
        OutputFormat::Json => print_json(&serde_json::to_value(report)?),
        OutputFormat::Text => {
            print_probe_table(&report.items);
            Ok(())
        }
        _ => unreachable!("ensure_supported restricts mcp probe output formats"),
    }
}

async fn probe_services(
    access: &ConfigAccess,
    node_did: &str,
    services: Vec<gents_documents::ToolServiceRegistry>,
    timeout: Duration,
    local_hostname: &str,
    local_subnet: Option<&str>,
) -> Vec<McpProbeSnapshot> {
    let Some(actor) = access.actor_did() else {
        return services
            .into_iter()
            .map(|service| McpProbeSnapshot {
                service: service.service_id,
                health_state: gents_tools::mcp::health::HealthStatus::Unreachable.to_string(),
                latency_ms: 0,
                last_error: Some("authenticated probe actor is unavailable".to_owned()),
            })
            .collect();
    };
    let pool = McpPool::new();
    let probes = services.iter().map(|service| {
        gents_tools::mcp::probe::probe(
            access,
            node_did,
            &service.service_id,
            &pool,
            gents_tools::mcp::probe::ProbeOptions {
                actor: &actor,
                timeout,
                local_hostname,
                local_subnet,
            },
        )
    });
    futures_util::future::join_all(probes)
        .await
        .into_iter()
        .map(McpProbeSnapshot::from)
        .collect()
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

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
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

impl From<McpProbeReceipt> for McpProbeSnapshot {
    fn from(receipt: McpProbeReceipt) -> Self {
        Self {
            service: receipt.service_id,
            health_state: receipt.health_state,
            latency_ms: receipt.latency_ms,
            last_error: receipt.last_error,
        }
    }
}
