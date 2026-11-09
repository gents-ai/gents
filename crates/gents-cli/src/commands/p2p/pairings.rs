use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents_protocol::enrollment::{AuthorizationLease, AuthorizationLeaseState};
use serde::Serialize;
use serde_json::{json, Value};

use crate::cli::args::{
    P2pAccessArgs, P2pPairingsListArgs, P2pPairingsReaderArgs, PairingObservationArg,
    PairingReadReasonArg,
};
use crate::cli::output_format::OutputFormat;
use crate::{graphql_rows, print_json, resolve_config_access};

use super::output::fetch_connected_peer_ids;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PeerPairingDesiredRow {
    peer_id: String,
    transport_peer_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    node_did: Option<String>,
    #[serde(skip)]
    actor_did: Option<String>,
    collections: Vec<String>,
    replicator_addresses: Vec<String>,
    profiles: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    template: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
    /// The signed enrollment generation this route materializes; absent on a
    /// local overlay row.
    #[serde(skip_serializing_if = "Option::is_none")]
    enrollment_authorization_sequence: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enrollment_authorization_expires_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    authorization_lease: Option<LeaseView>,
    connected: bool,
    subscribed: bool,
    replicating: bool,
}

/// Days left on an enrollment-owned route's signed authorization lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct LeaseView {
    state: AuthorizationLeaseState,
    remaining_days: i64,
}

impl LeaseView {
    fn at(expires_at: &str, now: DateTime<Utc>) -> Self {
        let lease = AuthorizationLease::at(expires_at, now);
        Self {
            state: lease.state,
            remaining_days: lease.remaining_days(),
        }
    }

    fn needs_renewal(&self) -> bool {
        self.state != AuthorizationLeaseState::Current
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PeerPairingAppliedRow {
    peer_id: String,
    node_did: Option<String>,
    collections: Vec<String>,
    replicator_addresses: Vec<String>,
}

pub(super) async fn pairing_reader_targets(args: P2pAccessArgs) -> Result<()> {
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let response = access
        .execute(
            r#"{ PeerPairingDesired {_docID peer_id node_did actor_did}
        PeerPairingApplied {_docID peer_id node_did} }"#,
        )
        .await?;
    let desired = response
        .pointer("/data/PeerPairingDesired")
        .and_then(Value::as_array)
        .context("pairing response omitted desired observations")?;
    let applied = response
        .pointer("/data/PeerPairingApplied")
        .and_then(Value::as_array)
        .context("pairing response omitted applied observations")?;
    print_json(&json!({"desired": desired, "applied": applied}))
}

pub(super) async fn pairing_reader(args: P2pPairingsReaderArgs, grant: bool) -> Result<()> {
    use gents::store::config_client::{PairingObservation, PairingReadReason};

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
    let rows = graphql_rows(&access, "PeerPairingDesired", pairings_list_query())
        .await
        .context("loading PeerPairingDesired rows")?;
    let applied_rows = graphql_rows(&access, "PeerPairingApplied", applied_list_query())
        .await
        .unwrap_or_default();
    let applied = parse_applied_rows(applied_rows);
    let graphql = crate::resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let connected_peers = fetch_connected_peer_ids(&graphql).await.unwrap_or_default();
    let pairings = annotate_pairing_health(
        parse_pairing_rows(rows, Utc::now()),
        &applied,
        &connected_peers,
    );
    let count = pairings.len();
    if let Some(warning) = renewal_warning(&pairings) {
        eprintln!("{warning}");
    }
    match args.output.ensure_supported(
        "p2p pairings list",
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

fn pairings_list_query() -> &'static str {
    r#"query {
        PeerPairingDesired {
            peer_id
            node_did
            actor_did
            collections
            replicator_addresses
            profiles
            template
            created_at
            updated_at
            enrollment_authorization_sequence
            enrollment_authorization_expires_at
        }
    }"#
}

fn applied_list_query() -> &'static str {
    r#"query {
        PeerPairingApplied {
            _docID
            peer_id
            node_did
            collections
            replicator_addresses
        }
    }"#
}

fn parse_pairing_rows(rows: Vec<Value>, now: DateTime<Utc>) -> Vec<PeerPairingDesiredRow> {
    let mut pairings = rows
        .into_iter()
        .filter_map(|row| {
            let peer_id = row
                .get("peer_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?
                .to_string();
            let replicator_addresses = string_list(&row, "replicator_addresses");
            let transport_peer_ids = replicator_addresses
                .iter()
                .filter_map(|address| {
                    gents::runtime::agent::p2p_reconcile::TransportEndpoint::parse(address.clone())
                        .ok()
                        .map(|endpoint| endpoint.peer_id().to_string())
                })
                .collect();
            let enrollment_authorization_expires_at =
                optional_string(&row, "enrollment_authorization_expires_at");
            let authorization_lease = enrollment_authorization_expires_at
                .as_deref()
                .map(|expires_at| LeaseView::at(expires_at, now));
            Some(PeerPairingDesiredRow {
                peer_id,
                transport_peer_ids,
                node_did: optional_string(&row, "node_did"),
                actor_did: optional_string(&row, "actor_did"),
                collections: string_list(&row, "collections"),
                replicator_addresses,
                profiles: string_list(&row, "profiles"),
                template: optional_string(&row, "template"),
                created_at: optional_string(&row, "created_at"),
                updated_at: optional_string(&row, "updated_at"),
                enrollment_authorization_sequence: row
                    .get("enrollment_authorization_sequence")
                    .and_then(Value::as_i64),
                enrollment_authorization_expires_at,
                authorization_lease,
                connected: false,
                subscribed: false,
                replicating: false,
            })
        })
        .collect::<Vec<_>>();
    pairings.sort_by(|left, right| left.peer_id.cmp(&right.peer_id));
    pairings
}

fn parse_applied_rows(rows: Vec<Value>) -> Vec<PeerPairingAppliedRow> {
    let mut rows = rows
        .into_iter()
        .filter_map(|row| {
            let doc_id = row.get("_docID")?.as_str()?.to_string();
            let peer_id = row
                .get("peer_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?
                .to_string();
            Some((
                doc_id,
                PeerPairingAppliedRow {
                    peer_id,
                    node_did: row
                        .get("node_did")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    collections: string_list(&row, "collections"),
                    replicator_addresses: string_list(&row, "replicator_addresses"),
                },
            ))
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    let mut canonical = BTreeMap::new();
    for (_, row) in rows {
        canonical
            .entry((row.node_did.clone(), row.peer_id.clone()))
            .or_insert(row);
    }
    canonical.into_values().collect()
}

/// One stderr line naming every enrollment route whose lease is inside the
/// renewal window or already gone, with the renewal procedure.
fn renewal_warning(rows: &[PeerPairingDesiredRow]) -> Option<String> {
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
             `gents p2p enrollment approve <request-id>` here; the new generation supersedes the old one.",
            due.len(),
            due.join("; "),
        )
    })
}

fn annotate_pairing_health(
    mut desired: Vec<PeerPairingDesiredRow>,
    applied: &[PeerPairingAppliedRow],
    connected: &[String],
) -> Vec<PeerPairingDesiredRow> {
    let applied_by_peer = applied
        .iter()
        .map(|row| {
            (
                (row.node_did.as_deref(), row.peer_id.as_str()),
                (
                    row.collections.iter().cloned().collect::<BTreeSet<_>>(),
                    row.replicator_addresses
                        .iter()
                        .cloned()
                        .collect::<BTreeSet<_>>(),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();

    let connected_set = connected_peer_id_set(connected);

    for row in &mut desired {
        row.connected = row
            .transport_peer_ids
            .iter()
            .any(|peer_id| connected_set.contains(peer_id))
            || (row.transport_peer_ids.is_empty() && connected_set.contains(&row.peer_id));
        let (applied_collections, applied_replicators) = applied_by_peer
            .get(&(row.actor_did.as_deref(), row.peer_id.as_str()))
            .cloned()
            .unwrap_or_default();
        row.replicating = !row.replicator_addresses.is_empty()
            && row
                .replicator_addresses
                .iter()
                .all(|address| applied_replicators.contains(address));
        row.subscribed = if is_push_template(row.template.as_deref()) {
            row.replicating
        } else {
            !row.collections.is_empty()
                && row
                    .collections
                    .iter()
                    .all(|collection| applied_collections.contains(collection))
        };
    }
    desired
}

fn connected_peer_id_set(connected: &[String]) -> BTreeSet<String> {
    connected
        .iter()
        .flat_map(|peer| connected_peer_id_candidates(peer))
        .collect()
}

fn connected_peer_id_candidates(peer: &str) -> Vec<String> {
    let peer = peer.trim();
    if peer.is_empty() {
        return Vec::new();
    }

    let mut ids = vec![peer.to_string()];
    if let Some(id) = p2p::iroh::parse_public_peer_addr(peer)
        .ok()
        .map(|(peer_id, _)| peer_id.to_string())
        .or_else(|| peer_id_from_address_like_peer(peer))
    {
        if id != peer {
            ids.push(id);
        }
    }
    ids
}

fn peer_id_from_address_like_peer(value: &str) -> Option<String> {
    let (_, suffix) = value.rsplit_once("/p2p/")?;
    let peer_id = suffix
        .split('/')
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    Some(peer_id.to_string())
}

fn is_push_template(template: Option<&str>) -> bool {
    use gents::runtime::agent::p2p_reconcile::{resolve_template, Delivery};
    template
        .and_then(resolve_template)
        .map(|t| t.delivery == Delivery::Push)
        .unwrap_or(false)
}

fn optional_string(row: &Value, field: &str) -> Option<String> {
    row.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn string_list(row: &Value, field: &str) -> Vec<String> {
    row.get(field)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn print_pairings_table(rows: &[PeerPairingDesiredRow]) -> Result<()> {
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

fn lease_cell(lease: Option<&LeaseView>) -> String {
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

    #[test]
    fn applied_rows_use_the_canonical_document() {
        let rows = parse_applied_rows(vec![
            json!({"_docID": "b", "peer_id": "peer-a", "replicator_addresses": ["stale"]}),
            json!({"_docID": "a", "peer_id": "peer-a", "replicator_addresses": ["fresh"]}),
        ]);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].replicator_addresses, ["fresh"]);
    }

    #[test]
    fn pairing_health_matches_applied_source_partition() {
        let mut desired = desired_row("peer-a", "client", &[], &["/ip4/1/tcp/4001"]);
        desired.node_did = Some("remote".into());
        desired.actor_did = Some("local".into());
        let rows = parse_applied_rows(vec![
            json!({"_docID":"a", "node_did":"foreign", "peer_id":"peer-a", "replicator_addresses":["/ip4/1/tcp/4001"]}),
            json!({"_docID":"b", "node_did":"local", "peer_id":"peer-a", "replicator_addresses":["/ip4/2/tcp/4001"]}),
        ]);
        assert_eq!(rows.len(), 2);
        let result = annotate_pairing_health(vec![desired], &rows, &[]);
        assert!(!result[0].replicating);
    }

    fn applied(peer_id: &str, collections: &[&str], addresses: &[&str]) -> PeerPairingAppliedRow {
        PeerPairingAppliedRow {
            peer_id: peer_id.to_string(),
            node_did: None,
            collections: collections.iter().map(|c| c.to_string()).collect(),
            replicator_addresses: addresses.iter().map(|a| a.to_string()).collect(),
        }
    }

    fn desired_row(
        peer_id: &str,
        template: &str,
        collections: &[&str],
        addresses: &[&str],
    ) -> PeerPairingDesiredRow {
        PeerPairingDesiredRow {
            peer_id: peer_id.to_string(),
            transport_peer_ids: addresses
                .iter()
                .filter_map(|address| {
                    gents::runtime::agent::p2p_reconcile::TransportEndpoint::parse(*address)
                        .ok()
                        .map(|endpoint| endpoint.peer_id().to_string())
                })
                .collect(),
            node_did: None,
            actor_did: None,
            collections: collections.iter().map(|c| c.to_string()).collect(),
            replicator_addresses: addresses.iter().map(|a| a.to_string()).collect(),
            profiles: Vec::new(),
            template: Some(template.to_string()),
            created_at: None,
            updated_at: None,
            enrollment_authorization_sequence: None,
            enrollment_authorization_expires_at: None,
            authorization_lease: None,
            connected: false,
            subscribed: false,
            replicating: false,
        }
    }

    #[test]
    fn push_template_health_keys_off_replicating_not_subscribed_collections() {
        let desired = vec![desired_row(
            "peer-a",
            gents::runtime::agent::p2p_reconcile::templates::MACHINE_TEMPLATE,
            &[gents::runtime::agent::p2p_reconcile::templates::NODE_DIRECTORY_COLLECTION],
            &["/ip4/1/tcp/4001"],
        )];
        let applied = vec![applied("peer-a", &[], &["/ip4/1/tcp/4001"])];

        let annotated = annotate_pairing_health(desired, &applied, &[]);

        assert!(
            annotated[0].replicating,
            "replicator is applied → replicating"
        );
        assert!(
            annotated[0].subscribed,
            "Push pairing with applied replicator must report healthy (subscribed)"
        );
    }

    #[test]
    fn push_template_without_applied_replicator_is_unhealthy() {
        let desired = vec![desired_row(
            "peer-a",
            gents::runtime::agent::p2p_reconcile::templates::MACHINE_TEMPLATE,
            &[gents::runtime::agent::p2p_reconcile::templates::NODE_DIRECTORY_COLLECTION],
            &["/ip4/1/tcp/4001"],
        )];
        let applied = vec![applied("peer-a", &[], &[])];

        let annotated = annotate_pairing_health(desired, &applied, &[]);

        assert!(!annotated[0].replicating);
        assert!(!annotated[0].subscribed);
    }

    #[test]
    fn replicate_template_health_still_requires_collection_set() {
        let desired = vec![desired_row(
            "peer-a",
            "agent-config",
            &["Agent", "Tools"],
            &[],
        )];
        let partial = vec![applied("peer-a", &["Agent"], &[])];
        let annotated = annotate_pairing_health(desired.clone(), &partial, &[]);
        assert!(
            !annotated[0].subscribed,
            "Replicate pairing missing a collection must report unhealthy"
        );

        let full = vec![applied("peer-a", &["Agent", "Tools"], &[])];
        let annotated = annotate_pairing_health(desired, &full, &[]);
        assert!(
            annotated[0].subscribed,
            "Replicate pairing with full collection set must report healthy"
        );
    }

    #[test]
    fn connected_health_uses_exact_peer_id_match_not_substring() {
        let desired = vec![desired_row("abc", "conversation", &["AgentRequest"], &[])];
        let connected = vec!["abcd".to_string()];

        let annotated = annotate_pairing_health(desired, &[], &connected);

        assert!(
            !annotated[0].connected,
            "substring of a connected peer id must not falsely report connected"
        );
    }

    #[test]
    fn connected_health_matches_exact_peer_id() {
        let desired = vec![desired_row("abcd", "conversation", &["AgentRequest"], &[])];
        let connected = vec!["abcd".to_string()];

        let annotated = annotate_pairing_health(desired, &[], &connected);

        assert!(annotated[0].connected);
    }

    #[test]
    fn connected_health_matches_endpoint_ticket_transport_identity() {
        let peer_id = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
        let (peer, addrs) =
            p2p::iroh::parse_public_peer_addr(&format!("127.0.0.1:9999/p2p/{peer_id}")).unwrap();
        let endpoint = p2p::iroh::endpoint_addr_from_parts(&peer, &addrs).unwrap();
        let ticket = p2p::iroh::endpoint_ticket_string(&endpoint);
        let desired = vec![desired_row(
            "directory-entry",
            "conversation",
            &[],
            &[ticket.as_str()],
        )];
        assert_eq!(desired[0].transport_peer_ids, [peer_id]);
        let connected = vec![ticket.to_string()];
        assert!(annotate_pairing_health(desired, &[], &connected)[0].connected);
        let other = vec![desired_row("other-peer", "conversation", &[], &[])];
        assert!(!annotate_pairing_health(other, &[], &connected)[0].connected);
    }

    #[test]
    fn connected_health_matches_address_form_peer_id() {
        let desired = vec![desired_row(
            "peer-a",
            "conversation",
            &["AgentRequest"],
            &[],
        )];
        let connected = vec!["100.74.68.88:9192/p2p/peer-a".to_string()];

        let annotated = annotate_pairing_health(desired, &[], &connected);

        assert!(annotated[0].connected);
    }

    #[test]
    fn connected_health_address_form_still_uses_exact_peer_id() {
        let desired = vec![desired_row(
            "peer-a",
            "conversation",
            &["AgentRequest"],
            &[],
        )];
        let connected = vec!["100.74.68.88:9192/p2p/peer-ab".to_string()];

        let annotated = annotate_pairing_health(desired, &[], &connected);

        assert!(
            !annotated[0].connected,
            "substring of address-form connected peer id must not report connected"
        );
    }

    #[test]
    fn parse_pairing_rows_sorts_and_ignores_incomplete_rows() {
        let rows = vec![
            json!({
                "peer_id": "peer-b",
                "node_did": " did:key:b ",
                "collections": ["AgentMessage", "", 3],
                "replicator_addresses": ["/ip4/2/tcp/4001"],
                "profiles": ["chat-requests", "", 4],
                "created_at": "",
                "updated_at": "2026-06-10T00:00:01Z"
            }),
            json!({
                "peer_id": "",
                "node_did": "did:key:missing-peer"
            }),
            json!({
                "peer_id": "peer-a",
                "collections": ["AgentRequest"],
                "replicator_addresses": null,
                "profiles": null
            }),
        ];

        let pairings = parse_pairing_rows(rows, Utc::now());

        assert_eq!(pairings.len(), 2);
        assert_eq!(pairings[0].peer_id, "peer-a");
        assert_eq!(pairings[0].collections, vec!["AgentRequest"]);
        assert!(pairings[0].replicator_addresses.is_empty());
        assert_eq!(pairings[1].peer_id, "peer-b");
        assert_eq!(pairings[1].node_did.as_deref(), Some("did:key:b"));
        assert_eq!(pairings[1].collections, vec!["AgentMessage"]);
        assert_eq!(pairings[1].profiles, vec!["chat-requests"]);
        assert!(pairings[1].created_at.is_none());
        assert_eq!(
            pairings[1].updated_at.as_deref(),
            Some("2026-06-10T00:00:01Z")
        );
    }

    #[test]
    fn enrollment_routes_report_their_lease_and_warn_inside_the_renewal_window() {
        let rows = vec![
            json!({
                "peer_id": "peer-current",
                "enrollment_authorization_sequence": 1,
                "enrollment_authorization_expires_at": "2026-11-29T19:07:47Z"
            }),
            json!({
                "peer_id": "peer-due",
                "enrollment_authorization_sequence": 1,
                "enrollment_authorization_expires_at": "2026-11-28T22:19:13Z"
            }),
            json!({
                "peer_id": "peer-expired",
                "enrollment_authorization_sequence": 2,
                "enrollment_authorization_expires_at": "2026-11-20T00:00:00Z"
            }),
            json!({"peer_id": "peer-overlay", "collections": ["FleetBoardPost"]}),
        ];
        let now: DateTime<Utc> = "2026-11-21T23:00:00Z".parse().unwrap();
        let pairings = parse_pairing_rows(rows, now);
        let lease = |peer: &str| {
            pairings
                .iter()
                .find(|row| row.peer_id == peer)
                .unwrap()
                .authorization_lease
                .clone()
        };
        assert_eq!(
            lease("peer-current"),
            Some(LeaseView {
                state: AuthorizationLeaseState::Current,
                remaining_days: 7
            })
        );
        assert_eq!(
            lease("peer-due"),
            Some(LeaseView {
                state: AuthorizationLeaseState::RenewalDue,
                remaining_days: 6
            })
        );
        assert_eq!(
            lease("peer-expired").unwrap().state,
            AuthorizationLeaseState::Expired
        );
        assert_eq!(lease("peer-overlay"), None);
        assert_eq!(lease_cell(lease("peer-due").as_ref()), "6d renew");
        assert_eq!(lease_cell(lease("peer-current").as_ref()), "7d");
        assert_eq!(lease_cell(None), "-");

        let warning = renewal_warning(&pairings).expect("renewal warning");
        assert!(warning.contains("2 enrollment authorization(s) need renewal"));
        assert!(warning.contains("peer-due (renewal_due, 6d left, expires 2026-11-28T22:19:13Z)"));
        assert!(warning.contains("peer-expired (expired"));
        assert!(!warning.contains("peer-current"));
        assert!(renewal_warning(&pairings[..0]).is_none());
    }
}
