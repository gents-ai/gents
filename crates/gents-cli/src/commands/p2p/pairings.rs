use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{json, Value};

use crate::cli::args::P2pPairingsListArgs;
use crate::cli::output_format::OutputFormat;
use crate::{graphql_rows, print_json, resolve_config_access};

use super::output::fetch_connected_peer_ids;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PeerPairingDesiredRow {
    /// Stable desired-state/directory key; never assumed to be an Iroh id.
    peer_id: String,
    transport_peer_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_did: Option<String>,
    collections: Vec<String>,
    replicator_addresses: Vec<String>,
    profiles: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    template: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
    connected: bool,
    subscribed: bool,
    replicating: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PeerPairingAppliedRow {
    peer_id: String,
    collections: Vec<String>,
    replicator_addresses: Vec<String>,
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
    let pairings = annotate_pairing_health(parse_pairing_rows(rows), &applied, &connected_peers);
    let count = pairings.len();
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
            agent_did
            collections
            replicator_addresses
            profiles
            template
            created_at
            updated_at
        }
    }"#
}

fn applied_list_query() -> &'static str {
    r#"query {
        PeerPairingApplied {
            _docID
            peer_id
            collections
            replicator_addresses
        }
    }"#
}

fn parse_pairing_rows(rows: Vec<Value>) -> Vec<PeerPairingDesiredRow> {
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
                    gents::agent::p2p_reconcile::TransportEndpoint::parse(address.clone())
                        .ok()
                        .map(|endpoint| endpoint.peer_id().to_string())
                })
                .collect();
            Some(PeerPairingDesiredRow {
                peer_id,
                transport_peer_ids,
                agent_did: optional_string(&row, "agent_did"),
                collections: string_list(&row, "collections"),
                replicator_addresses,
                profiles: string_list(&row, "profiles"),
                template: optional_string(&row, "template"),
                created_at: optional_string(&row, "created_at"),
                updated_at: optional_string(&row, "updated_at"),
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
                    collections: string_list(&row, "collections"),
                    replicator_addresses: string_list(&row, "replicator_addresses"),
                },
            ))
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    let mut canonical = BTreeMap::new();
    for (_, row) in rows {
        canonical.entry(row.peer_id.clone()).or_insert(row);
    }
    canonical.into_values().collect()
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
                row.peer_id.as_str(),
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
            .get(row.peer_id.as_str())
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
    if let Some(id) = peer_id_from_address_like_peer(peer) {
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

/// Whether the pairing's template uses Push delivery (filtered replicator is the
/// only channel; no DefraDB subscription). Unknown/absent templates fall back to
/// the strict Replicate-style collection check, which is the conservative choice
/// (a missing subscription reports unhealthy rather than falsely healthy).
fn is_push_template(template: Option<&str>) -> bool {
    use gents::agent::p2p_reconcile::{resolve_template, Delivery};
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
                row.agent_did.clone().unwrap_or_else(|| "-".to_string()),
                if row.profiles.is_empty() {
                    "-".to_string()
                } else {
                    row.profiles.join(",")
                },
                yes_no(row.connected),
                yes_no(row.subscribed),
                yes_no(row.replicating),
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

    fn applied(peer_id: &str, collections: &[&str], addresses: &[&str]) -> PeerPairingAppliedRow {
        PeerPairingAppliedRow {
            peer_id: peer_id.to_string(),
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
                    gents::agent::p2p_reconcile::TransportEndpoint::parse(*address)
                        .ok()
                        .map(|endpoint| endpoint.peer_id().to_string())
                })
                .collect(),
            agent_did: None,
            collections: collections.iter().map(|c| c.to_string()).collect(),
            replicator_addresses: addresses.iter().map(|a| a.to_string()).collect(),
            profiles: Vec::new(),
            template: Some(template.to_string()),
            created_at: None,
            updated_at: None,
            connected: false,
            subscribed: false,
            replicating: false,
        }
    }

    #[test]
    fn push_template_health_keys_off_replicating_not_subscribed_collections() {
        let desired = vec![desired_row(
            "peer-a",
            "conversation",
            &["AgentRequest", "AgentResponse"],
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
            "conversation",
            &["AgentRequest"],
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
            &["AgentBehavior", "ToolSelection"],
            &[],
        )];
        let partial = vec![applied("peer-a", &["AgentBehavior"], &[])];
        let annotated = annotate_pairing_health(desired.clone(), &partial, &[]);
        assert!(
            !annotated[0].subscribed,
            "Replicate pairing missing a collection must report unhealthy"
        );

        let full = vec![applied("peer-a", &["AgentBehavior", "ToolSelection"], &[])];
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
                "agent_did": " did:key:b ",
                "collections": ["AgentResponse", "", 3],
                "replicator_addresses": ["/ip4/2/tcp/4001"],
                "profiles": ["chat-requests", "", 4],
                "created_at": "",
                "updated_at": "2026-06-10T00:00:01Z"
            }),
            json!({
                "peer_id": "",
                "agent_did": "did:key:missing-peer"
            }),
            json!({
                "peer_id": "peer-a",
                "collections": ["AgentRequest"],
                "replicator_addresses": null,
                "profiles": null
            }),
        ];

        let pairings = parse_pairing_rows(rows);

        assert_eq!(pairings.len(), 2);
        assert_eq!(pairings[0].peer_id, "peer-a");
        assert_eq!(pairings[0].collections, vec!["AgentRequest"]);
        assert!(pairings[0].replicator_addresses.is_empty());
        assert_eq!(pairings[1].peer_id, "peer-b");
        assert_eq!(pairings[1].agent_did.as_deref(), Some("did:key:b"));
        assert_eq!(pairings[1].collections, vec!["AgentResponse"]);
        assert_eq!(pairings[1].profiles, vec!["chat-requests"]);
        assert!(pairings[1].created_at.is_none());
        assert_eq!(
            pairings[1].updated_at.as_deref(),
            Some("2026-06-10T00:00:01Z")
        );
    }
}
