use std::io::{self, Write};

use anyhow::{Context, Result};
use chrono::Utc;
use gents_protocol::enrollment::AuthorizationLeaseState;
use gents_protocol::operations::p2p::{Pairing, PairingLease};
use serde_json::json;

use crate::cli::args::{
    P2pAccessArgs, P2pPairingsListArgs, P2pPairingsReaderArgs, PairingObservationArg,
    PairingReadReasonArg,
};
use crate::cli::output_format::OutputFormat;
use crate::{print_json, resolve_config_access};

use gents_host::p2p_status::fetch_connected_peer_ids;

pub(super) async fn pairing_reader_targets(args: P2pAccessArgs) -> Result<()> {
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    print_json(&gents_p2p::operations::pairing::pairing_reader_targets(&access).await?)
}

pub(super) async fn pairing_reader(args: P2pPairingsReaderArgs, grant: bool) -> Result<()> {
    use gents_store::config_client::{PairingObservation, PairingReadReason};

    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let observation = match args.observation {
        PairingObservationArg::Desired => PairingObservation::Desired,
        PairingObservationArg::Applied => PairingObservation::Applied,
    };
    let reason = match args.reason {
        PairingReadReasonArg::RouteAdmission => PairingReadReason::RouteAdmission,
        PairingReadReasonArg::ToolView => PairingReadReason::ToolView,
    };
    let agent = identity::Did::new(args.agent_did.clone())?;
    access
        .set_pairing_reader(observation, &args.doc_id, reason, &agent, grant)
        .await?;
    print_json(&json!({
        "collection": observation.collection(),
        "doc_id": args.doc_id,
        "agent_did": args.agent_did,
        "reason": reason.as_str(),
        "granted": grant,
    }))
}

pub(super) async fn p2p_pairings_list(args: P2pPairingsListArgs) -> Result<()> {
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let graphql = gents_service::home_state::resolve_graphql_endpoint(
        args.graphql.as_deref(),
        args.home.as_deref(),
    )?;
    let connected_peers = fetch_connected_peer_ids(&graphql).await.unwrap_or_default();
    let pairings =
        gents_p2p::operations::pairing::list_pairings(&access, &connected_peers, Utc::now())
            .await
            .context("loading PeerPairingDesired rows")?;
    let count = pairings.len();
    if let Some(warning) = renewal_warning(&pairings) {
        eprintln!("{warning}");
    }
    match args.output.ensure_supported(
        "runtime p2p pairings list",
        &[OutputFormat::Json, OutputFormat::Table],
    )? {
        OutputFormat::Json => print_json(&json!({
            "status": "ok",
            "home": home_dir,
            "access_mode": access.mode(),
            "pairings": pairings,
            "count": count,
        })),
        OutputFormat::Table => print_pairings_table(&pairings),
        _ => unreachable!("ensure_supported restricts p2p pairings list output formats"),
    }
}

/// One stderr line naming every enrollment route whose lease is inside the
/// renewal window or already gone, with the renewal procedure.
fn renewal_warning(rows: &[Pairing]) -> Option<String> {
    let due = rows
        .iter()
        .filter_map(|row| {
            let lease = row.authorization_lease.as_ref()?;
            lease.needs_renewal().then(|| {
                format!(
                    "{} ({}, {}d left, expires {})",
                    row.peer_id,
                    serde_json::to_value(lease.state)
                        .ok()
                        .and_then(|state| state.as_str().map(str::to_owned))
                        .unwrap_or_default(),
                    lease.remaining_days,
                    row.enrollment_authorization_expires_at
                        .as_deref()
                        .unwrap_or("-"),
                )
            })
        })
        .collect::<Vec<_>>();
    (!due.is_empty()).then(|| {
        format!(
            "warning: {} enrollment authorization(s) need renewal: {}. Renew by re-enrolling each peer: \
             this node's p2p enrollment_offer, the peer's enrollment_request, then \
             `gents runtime p2p enrollment approve <request-id>` here; the new generation supersedes the old one.",
            due.len(),
            due.join("; "),
        )
    })
}

fn print_pairings_table(rows: &[Pairing]) -> Result<()> {
    let headers = [
        "PEER".to_string(),
        "TRANSPORT".to_string(),
        "DID".to_string(),
        "PROFILES".to_string(),
        "CONNECTED".to_string(),
        "SUBSCRIBED".to_string(),
        "REPLICATING".to_string(),
        "LEASE".to_string(),
    ];
    let mut widths = headers.clone().map(|header| header.len());
    let table_rows = rows
        .iter()
        .map(|row| {
            [
                row.peer_id.clone(),
                if row.transport_peer_ids.is_empty() {
                    "-".to_string()
                } else {
                    row.transport_peer_ids.join(",")
                },
                row.node_did.clone().unwrap_or_else(|| "-".to_string()),
                if row.profiles.is_empty() {
                    "-".to_string()
                } else {
                    row.profiles.join(",")
                },
                yes_no(row.connected),
                yes_no(row.subscribed),
                yes_no(row.replicating),
                lease_cell(row.authorization_lease.as_ref()),
            ]
        })
        .collect::<Vec<_>>();
    for row in &table_rows {
        for (idx, cell) in row.iter().enumerate() {
            widths[idx] = widths[idx].max(cell.len());
        }
    }
    let mut stdout = io::stdout();
    print_table_row(&mut stdout, &headers, &widths)?;
    print_table_row(&mut stdout, &widths.map(|width| "-".repeat(width)), &widths)?;
    for row in &table_rows {
        print_table_row(&mut stdout, row, &widths)?;
    }
    stdout.flush().context("flushing p2p pairings table")?;
    Ok(())
}

fn lease_cell(lease: Option<&PairingLease>) -> String {
    match lease {
        None => "-".to_string(),
        Some(lease) => match lease.state {
            AuthorizationLeaseState::Current => format!("{}d", lease.remaining_days),
            AuthorizationLeaseState::RenewalDue => format!("{}d renew", lease.remaining_days),
            AuthorizationLeaseState::Expired => "expired".to_string(),
            AuthorizationLeaseState::Invalid => "invalid".to_string(),
        },
    }
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
    writeln!(writer, "{line}").context("writing p2p pairings table row")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(peer_id: &str, lease: Option<(AuthorizationLeaseState, i64, &str)>) -> Pairing {
        Pairing {
            peer_id: peer_id.to_owned(),
            transport_peer_ids: Vec::new(),
            node_did: None,
            actor_did: None,
            collections: Vec::new(),
            replicator_addresses: Vec::new(),
            profiles: Vec::new(),
            template: None,
            created_at: None,
            updated_at: None,
            enrollment_authorization_sequence: lease.map(|_| 1),
            enrollment_authorization_expires_at: lease.map(|(_, _, expires)| expires.to_owned()),
            authorization_lease: lease.map(|(state, remaining_days, _)| PairingLease {
                state,
                remaining_days,
            }),
            connected: false,
            subscribed: false,
            replicating: false,
        }
    }

    #[test]
    fn leases_inside_the_renewal_window_are_named_with_the_renewal_procedure() {
        let pairings = [
            route(
                "peer-current",
                Some((AuthorizationLeaseState::Current, 7, "2026-11-29T19:07:47Z")),
            ),
            route(
                "peer-due",
                Some((
                    AuthorizationLeaseState::RenewalDue,
                    6,
                    "2026-11-28T22:19:13Z",
                )),
            ),
            route(
                "peer-expired",
                Some((AuthorizationLeaseState::Expired, 0, "2026-11-20T00:00:00Z")),
            ),
            route("peer-overlay", None),
        ];
        let lease = |index: usize| pairings[index].authorization_lease.as_ref();
        assert_eq!(lease_cell(lease(1)), "6d renew");
        assert_eq!(lease_cell(lease(0)), "7d");
        assert_eq!(lease_cell(lease(2)), "expired");
        assert_eq!(lease_cell(None), "-");

        let warning = renewal_warning(&pairings).expect("renewal warning");
        assert!(warning.contains("2 enrollment authorization(s) need renewal"));
        assert!(warning.contains("peer-due (renewal_due, 6d left, expires 2026-11-28T22:19:13Z)"));
        assert!(warning.contains("peer-expired (expired"));
        assert!(!warning.contains("peer-current"));
        assert!(warning.contains("`gents runtime p2p enrollment approve <request-id>`"));
        assert!(renewal_warning(&pairings[..1]).is_none());
    }
}
