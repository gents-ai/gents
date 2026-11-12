use std::io::{self, Write};

use anyhow::{Context, Result};
use chrono::Utc;
use gents_protocol::operations::p2p::{RegisteredPeer, RegistryError, RegistryRegistration};
use serde_json::{json, Value};

use crate::cli::args::{P2pAccessArgs, P2pNetworkListArgs, P2pNetworkRegisterArgs};
use crate::cli::output_format::OutputFormat;
use crate::commands::refusal::Refusal;
use crate::{print_json, resolve_config_access};

use gents_host::p2p_status::load_live_http_p2p_status;

fn refused(error: RegistryError) -> Refusal<RegistryError> {
    let next = error.recovery();
    Refusal::new(error, next)
}

fn reported_strings(status: &Value, field: &str) -> Vec<String> {
    status
        .get(field)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

pub(super) async fn p2p_network_register(args: P2pNetworkRegisterArgs) -> Result<()> {
    let graphql = gents_service::home_state::resolve_graphql_endpoint(
        args.graphql.as_deref(),
        args.home.as_deref(),
    )?;
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let status = load_live_http_p2p_status(args.home.as_deref(), &graphql).await;
    let registration = RegistryRegistration {
        peer_id: status
            .get("p2p_peer_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        node_did: gents_service::home_state::resolve_node_did(args.home.as_deref(), None)
            .context("resolving local node DID")?,
        listen_addresses: reported_strings(&status, "p2p_listen_addresses"),
        display_name: args.display_name,
        templates: args.templates,
        network_id: args.network_id,
    };
    let registered =
        gents_p2p::operations::registry::register_self(&access, &registration, Utc::now())
            .await
            .map_err(refused)?;
    print_json(&json!({
        "status": "registered",
        "home": home_dir,
        "peer_id": registered.peer_id,
        "node_did": registered.node_did,
        "display_name": registered.display_name,
        "templates": registered.templates,
        "network_id": registered.network_id,
        "addresses": registered.addresses,
    }))
}

pub(super) async fn p2p_network_list(args: P2pNetworkListArgs) -> Result<()> {
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let peers = gents_p2p::operations::registry::list_registered_peers(&access, Utc::now())
        .await
        .context("loading PeerRegistry rows")?;
    let count = peers.len();
    match args.output.ensure_supported(
        "runtime p2p network list",
        &[OutputFormat::Json, OutputFormat::Table],
    )? {
        OutputFormat::Json => print_json(&json!({
            "status": "ok",
            "home": home_dir,
            "peers": peers,
            "count": count,
        })),
        OutputFormat::Table => print_network_table(&peers),
        _ => unreachable!("ensure_supported restricts p2p network list output formats"),
    }
}

pub(super) async fn p2p_network_rm(args: P2pAccessArgs) -> Result<()> {
    let graphql = gents_service::home_state::resolve_graphql_endpoint(
        args.graphql.as_deref(),
        args.home.as_deref(),
    )?;
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let status = load_live_http_p2p_status(args.home.as_deref(), &graphql).await;
    let removed = gents_p2p::operations::registry::deregister_self(
        &access,
        status.get("p2p_peer_id").and_then(Value::as_str),
    )
    .await
    .map_err(refused)?;
    print_json(&json!({
        "status": "deregistered",
        "home": home_dir,
        "peer_id": removed.peer_id,
        "removed_count": removed.removed_count,
    }))
}

fn print_network_table(rows: &[RegisteredPeer]) -> Result<()> {
    let headers = [
        "PEER".to_string(),
        "DID".to_string(),
        "NAME".to_string(),
        "NETWORK".to_string(),
        "ONLINE".to_string(),
        "PAIRED".to_string(),
        "TEMPLATES".to_string(),
    ];
    let mut widths = headers.clone().map(|h| h.len());
    let table_rows: Vec<[String; 7]> = rows
        .iter()
        .map(|row| {
            [
                row.peer_id.clone(),
                row.node_did.clone().unwrap_or_else(|| "-".to_string()),
                row.display_name.clone().unwrap_or_else(|| "-".to_string()),
                row.network_id.clone(),
                yes_no(row.online),
                yes_no(row.paired),
                if row.templates.is_empty() {
                    "-".to_string()
                } else {
                    row.templates.join(",")
                },
            ]
        })
        .collect();
    for row in &table_rows {
        for (idx, cell) in row.iter().enumerate() {
            widths[idx] = widths[idx].max(cell.len());
        }
    }
    let mut stdout = io::stdout();
    print_table_row(&mut stdout, &headers, &widths)?;
    print_table_row(&mut stdout, &widths.map(|w| "-".repeat(w)), &widths)?;
    for row in &table_rows {
        print_table_row(&mut stdout, row, &widths)?;
    }
    stdout.flush().context("flushing p2p network table")?;
    Ok(())
}

fn yes_no(value: bool) -> String {
    if value { "yes" } else { "no" }.to_string()
}

fn print_table_row<const N: usize>(
    writer: &mut impl Write,
    cells: &[String; N],
    widths: &[usize; N],
) -> Result<()> {
    let line = cells
        .iter()
        .zip(widths.iter())
        .map(|(cell, width)| format!("{cell:<width$}"))
        .collect::<Vec<_>>()
        .join("  ");
    writeln!(writer, "{line}").context("writing p2p network table row")
}
