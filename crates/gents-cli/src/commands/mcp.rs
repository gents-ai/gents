use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use gents_store::graphql::escape_graphql_string;
use gents_tools::{
    mcp::health::run_health_check_cycle_for_actor, mcp::health::HealthCheckerOptions,
    mcp::health::HealthStatus, mcp::health::McpHealthCheckService, mcp::health::ServiceHealthMap,
    mcp::pool::McpPool,
};
use serde::Serialize;

use crate::cli::args::{McpCommand, McpProbeArgs, McpRegisterArgs};
use crate::cli::output_format::OutputFormat;
use crate::config_writes::ConfigAccess;
use crate::{
    graphql_rows_or_empty_if_collection_missing, parse_duration_suffix, print_json,
    resolve_config_access,
};
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
                    anyhow::anyhow!("--endpoint: {source}\nNext: {}", crate::next_call::command(next, None))
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
    let services = load_mcp_services(&access, &node_did, target.service_id()).await?;
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

async fn load_mcp_services(
    access: &ConfigAccess,
    node_did: &str,
    service_id: Option<&str>,
) -> Result<Vec<McpHealthCheckService>> {
    let registry_args = match service_id {
        Some(service_id) => format!(
            r#"filter: {{ node_did: {{ _eq: "{}" }}, service_id: {{ _eq: "{}" }} }}"#,
            escape_graphql_string(node_did),
            escape_graphql_string(service_id)
        ),
        None => format!(
            r#"filter: {{ node_did: {{ _eq: "{}" }} }}, order: {{ service_id: ASC }}"#,
            escape_graphql_string(node_did)
        ),
    };
    let query = format!(
        r#"{{
            ToolServiceRegistry({registry_args}) {{
                service_id
                enabled
                hostname
                tailscale_ip
                lan_ip
                mcp_port
                mcp_path
                endpoint_url
                send_node_did
                updated_at
            }}
        }}"#
    );
    let rows = graphql_rows_or_empty_if_collection_missing(access, "ToolServiceRegistry", &query)
        .await
        .context("loading MCP service registry rows")?;
    rows.into_iter()
        .filter(|row| {
            row.get("enabled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true)
        })
        .map(|row| serde_json::from_value(row).context("parsing ToolServiceRegistry row"))
        .collect()
}

async fn probe_services(
    access: &ConfigAccess,
    node_did: &str,
    services: Vec<McpHealthCheckService>,
    timeout: Duration,
    local_hostname: &str,
    local_subnet: Option<&str>,
) -> Vec<McpProbeSnapshot> {
    let pool = McpPool::new();
    let mut handles = Vec::with_capacity(services.len());
    for service in services {
        let service_id = service.service_id.clone();
        let requested_service = service_id.clone();
        let access = access.clone();
        let node_did = node_did.to_owned();
        let pool = pool.clone();
        let local_hostname = local_hostname.to_string();
        let local_subnet = local_subnet.map(ToOwned::to_owned);
        handles.push((
            service_id,
            tokio::spawn(async move {
                let Some(actor) = access.actor_did() else {
                    return McpProbeSnapshot {
                        service: requested_service,
                        health_state: HealthStatus::Unreachable.to_string(),
                        latency_ms: 0,
                        last_error: Some("authenticated probe actor is unavailable".to_owned()),
                    };
                };
                let service =
                    match load_mcp_services(&access, &node_did, Some(&requested_service)).await {
                        Ok(mut services) if services.len() == 1 => services.remove(0),
                        Ok(_) => {
                            return McpProbeSnapshot {
                                service: requested_service,
                                health_state: HealthStatus::Unreachable.to_string(),
                                latency_ms: 0,
                                last_error: Some(
                                    "service is unavailable, disabled, or ambiguous".to_owned(),
                                ),
                            }
                        }
                        Err(error) => {
                            return McpProbeSnapshot {
                                service: requested_service,
                                health_state: HealthStatus::Unreachable.to_string(),
                                latency_ms: 0,
                                last_error: Some(format!("service recheck failed: {error:#}")),
                            }
                        }
                    };
                probe_service(
                    service,
                    &pool,
                    timeout,
                    &local_hostname,
                    local_subnet.as_deref(),
                    &actor,
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
    actor: &str,
) -> McpProbeSnapshot {
    let service_id = service.service_id.clone();
    let health_map = ServiceHealthMap::new();
    let started = Instant::now();
    let result = tokio::time::timeout(
        timeout,
        run_health_check_cycle_for_actor(
            vec![service],
            Utc::now(),
            pool,
            &health_map,
            local_hostname,
            local_subnet,
            &one_shot_probe_options(timeout),
            actor,
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

    #[tokio::test]
    async fn exact_service_read_uses_the_requested_node_scope() {
        use gents_documents::apply_desired_state_plan;
        use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
        use gents_store::identity::NodeIdentity;
        let directory = tempfile::tempdir().unwrap();
        let root = gents_store::identity::KeyIdentity::load_or_create(
            directory.path().join("runtime"),
            None,
        )
        .unwrap();
        let other = gents_store::identity::KeyIdentity::load_or_create(
            directory.path().join("other"),
            None,
        )
        .unwrap();
        let node = Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(root.did())
                .with_node_acp_owner(root.did().to_owned())
                .build()
                .await
                .unwrap(),
        );
        gents_store::schema::ensure_runtime_schemas(&node)
            .await
            .unwrap();
        let access = ConfigAccess::local_as(node.clone(), identity::Did::new(root.did()).unwrap());
        let did = escape_graphql_string(root.did());
        access
            .write(
                "test.mcp.node",
                &format!(r#"mutation {{ create_Node(input: {{node_did: "{did}"}}) {{_docID}} }}"#),
            )
            .await
            .unwrap();
        let value = serde_json::json!({"node_did":root.did(),"service_id":"svc-1","hostname":"real-host","mcp_port":1,"enabled":true});
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: gents_store::collection::Collection::ToolServiceRegistry,
            add: value.clone(),
            update: value,
        }])
        .unwrap();
        access
            .transact_write("test.mcp.service", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(
                        txn,
                        plan,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await
                })
            })
            .await
            .unwrap();
        let services = load_mcp_services(&access, root.did(), Some("svc-1"))
            .await
            .unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].hostname, "real-host");
        assert!(load_mcp_services(&access, other.did(), Some("svc-1"))
            .await
            .unwrap()
            .is_empty());
        access.write("test.mcp.disable", r#"mutation { update_ToolServiceRegistry(filter:{service_id:{_eq:"svc-1"}}, input:{enabled:false}) {_docID} }"#).await.unwrap();
        let snapshots = probe_services(
            &access,
            root.did(),
            services,
            Duration::from_secs(1),
            "local",
            None,
        )
        .await;
        assert_eq!(snapshots.len(), 1);
        assert_eq!(
            snapshots[0].last_error.as_deref(),
            Some("service is unavailable, disabled, or ambiguous")
        );
        assert_eq!(snapshots[0].latency_ms, 0);
        node.shutdown().await.unwrap();
    }
}
