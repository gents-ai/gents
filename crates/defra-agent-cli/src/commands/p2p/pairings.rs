use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use defra_agent::graphql::escape_graphql_string;
use serde::Serialize;
use serde_json::{json, Value};

use crate::cli::args::{P2pPairingRefArgs, P2pPairingSetArgs, P2pPairingsListArgs};
use crate::cli::output_format::OutputFormat;
use crate::config_writes::ConfigAccess;
use crate::{
    expand_nonempty_values, graphql_rows, graphql_string_list_literal, print_json,
    resolve_config_access,
};

use super::collections::{expand_p2p_collection_args, p2p_collection_profile_id};
use super::output::fetch_connected_peer_ids;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PeerPairingDesiredRow {
    peer_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_did: Option<String>,
    collections: Vec<String>,
    replicator_addresses: Vec<String>,
    profiles: Vec<String>,
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
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), true).await?;
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

pub(super) async fn p2p_pairings_set(args: P2pPairingSetArgs) -> Result<()> {
    let agent_did = required_trimmed(&args.agent_did, "--did")?;
    let addresses = expand_nonempty_values(&args.addresses, "--address")?;
    let peer_id = resolve_set_peer_id(args.peer_id.as_deref(), &addresses)?;
    let collections = expand_p2p_collection_args(&args.collections, &args.profiles)?;
    if collections.is_empty() {
        anyhow::bail!("provide at least one --collection or --profile");
    }
    let profiles = pairing_profile_ids(&args.profiles);
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let graphql = crate::resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), true).await?;
    let doc_id = write_pairing_desired(
        &access,
        &peer_id,
        Some(&agent_did),
        &collections,
        &addresses,
        &profiles,
        &now,
    )
    .await?;

    let p2p = if args.wait {
        let timeout = crate::request_helpers::parse_duration_suffix(&args.timeout)?;
        Some(wait_for_pairing_connected(args.home.as_deref(), &graphql, &peer_id, timeout).await?)
    } else {
        None
    };

    let mut output = json!({
        "status": "pairing_set",
        "home": home_dir,
        "access_mode": access.mode(),
        "peer_id": peer_id,
        "agent_did": agent_did,
        "collections": collections,
        "replicator_addresses": addresses,
        "profiles": profiles,
        "doc_id": doc_id,
        "waited": args.wait,
        "note": "Desired pairing written. The running runtime applies P2P wiring on its pairing sweep.",
    });
    if let Some(p2p) = p2p {
        output["p2p"] = p2p;
    }
    print_json(&output)?;
    Ok(())
}

/// Resolve the pairing peer id: explicit `--peer`, else derived from the first
/// shareable `--address` (ticket or multiaddr).
fn resolve_set_peer_id(peer: Option<&str>, addresses: &[String]) -> Result<String> {
    if let Some(peer) = peer.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(peer.to_string());
    }
    for address in addresses {
        if let Ok((peer_id, _)) = p2p::iroh::parse_public_peer_addr(address.trim()) {
            return Ok(peer_id.to_string());
        }
    }
    anyhow::bail!(
        "provide --peer, or a --address that is a shareable ticket or multiaddr to derive it from"
    )
}

pub(super) async fn p2p_pairings_remove(args: P2pPairingRefArgs) -> Result<()> {
    let peer_id = required_trimmed(&args.peer_id, "--peer")?;
    let mutation = delete_pairing_mutation(&peer_id);
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), true).await?;
    let response = access
        .execute(&mutation)
        .await
        .context("deleting PeerPairingDesired row")?;
    let doc_ids = mutation_doc_ids(&response, "delete_PeerPairingDesired");
    let count = doc_ids.len();

    print_json(&json!({
        "status": "pairing_removed",
        "home": home_dir,
        "access_mode": access.mode(),
        "peer_id": peer_id,
        "removed_doc_ids": doc_ids,
        "removed_count": count,
    }))?;
    Ok(())
}

fn required_trimmed(value: &str, flag_name: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        anyhow::bail!("provide {flag_name}");
    }
    Ok(value.to_string())
}

fn pairings_list_query() -> &'static str {
    r#"query {
        PeerPairingDesired {
            peer_id
            agent_did
            collections
            replicator_addresses
            profiles
            created_at
            updated_at
        }
    }"#
}

fn applied_list_query() -> &'static str {
    r#"query {
        PeerPairingApplied {
            peer_id
            collections
            replicator_addresses
        }
    }"#
}

pub(super) async fn write_pairing_desired(
    access: &ConfigAccess,
    peer_id: &str,
    agent_did: Option<&str>,
    collections: &[String],
    addresses: &[String],
    profiles: &[String],
    now: &str,
) -> Result<String> {
    let mutation =
        upsert_pairing_mutation(peer_id, agent_did, collections, addresses, profiles, now);
    let response = access
        .execute(&mutation)
        .await
        .context("writing PeerPairingDesired row")?;
    crate::extract_mutation_doc_id(&response, "PeerPairingDesired")
        .context("reading PeerPairingDesired mutation doc id")
}

pub(super) async fn peer_pairing_exists(access: &ConfigAccess, peer_id: &str) -> Result<bool> {
    let peer_id = escape_graphql_string(peer_id);
    let query = format!(
        r#"query {{
            PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}, limit: 1) {{
                peer_id
            }}
        }}"#
    );
    let rows = graphql_rows(access, "PeerPairingDesired", &query)
        .await
        .context("checking existing PeerPairingDesired row")?;
    Ok(!rows.is_empty())
}

pub(super) fn upsert_pairing_mutation(
    peer_id: &str,
    agent_did: Option<&str>,
    collections: &[String],
    addresses: &[String],
    profiles: &[String],
    now: &str,
) -> String {
    let peer_id = escape_graphql_string(peer_id);
    let agent_did = graphql_nullable_string_literal(agent_did);
    let agent_did_update = if agent_did == "null" {
        String::new()
    } else {
        format!("agent_did: {agent_did},")
    };
    let collections = graphql_string_list_literal(collections);
    let addresses = graphql_string_list_literal(addresses);
    let profiles = graphql_nullable_string_list_literal(profiles);
    let now = escape_graphql_string(now);

    format!(
        r#"mutation {{
            upsert_PeerPairingDesired(
                filter: {{ peer_id: {{ _eq: "{peer_id}" }} }},
                add: {{
                    peer_id: "{peer_id}",
                    agent_did: {agent_did},
                    collections: {collections},
                    replicator_addresses: {addresses},
                    profiles: {profiles},
                    source: "operator",
                    created_at: "{now}",
                    updated_at: "{now}"
                }},
                update: {{
                    {agent_did_update}
                    collections: {collections},
                    replicator_addresses: {addresses},
                    profiles: {profiles},
                    source: "operator",
                    updated_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    )
}

fn delete_pairing_mutation(peer_id: &str) -> String {
    let peer_id = escape_graphql_string(peer_id);
    format!(
        r#"mutation {{
            delete_PeerPairingDesired(
                filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}
            ) {{ _docID }}
        }}"#
    )
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
            Some(PeerPairingDesiredRow {
                peer_id,
                agent_did: optional_string(&row, "agent_did"),
                collections: string_list(&row, "collections"),
                replicator_addresses: string_list(&row, "replicator_addresses"),
                profiles: string_list(&row, "profiles"),
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
    rows.into_iter()
        .filter_map(|row| {
            let peer_id = row
                .get("peer_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?
                .to_string();
            Some(PeerPairingAppliedRow {
                peer_id,
                collections: string_list(&row, "collections"),
                replicator_addresses: string_list(&row, "replicator_addresses"),
            })
        })
        .collect()
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

    for row in &mut desired {
        row.connected = connected.iter().any(|peer| peer.contains(&row.peer_id));
        let (applied_collections, applied_replicators) = applied_by_peer
            .get(row.peer_id.as_str())
            .cloned()
            .unwrap_or_default();
        row.subscribed = !row.collections.is_empty()
            && row
                .collections
                .iter()
                .all(|collection| applied_collections.contains(collection));
        row.replicating = !row.replicator_addresses.is_empty()
            && row
                .replicator_addresses
                .iter()
                .all(|address| applied_replicators.contains(address));
    }
    desired
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

fn pairing_profile_ids(profiles: &[crate::cli::args::P2pCollectionProfileArg]) -> Vec<String> {
    profiles
        .iter()
        .map(|profile| p2p_collection_profile_id(*profile).to_string())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn graphql_nullable_string_list_literal(values: &[String]) -> String {
    if values.is_empty() {
        "null".to_string()
    } else {
        graphql_string_list_literal(values)
    }
}

fn graphql_nullable_string_literal(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("\"{}\"", escape_graphql_string(value)))
        .unwrap_or_else(|| "null".to_string())
}

pub(super) async fn wait_for_pairing_connected(
    _home: Option<&std::path::Path>,
    graphql: &str,
    peer_id: &str,
    timeout: Duration,
) -> Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let peers = fetch_connected_peer_ids(graphql).await?;
        if peers.iter().any(|peer| peer.contains(peer_id)) {
            return Ok(json!({
                "p2p_connected_peers": peers,
            }));
        }
        if Instant::now() >= deadline {
            anyhow::bail!(
                "timed out waiting for peer {peer_id} to connect after {}s",
                timeout.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn print_pairings_table(rows: &[PeerPairingDesiredRow]) -> Result<()> {
    let headers = [
        "PEER".to_string(),
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

fn mutation_doc_ids(response: &Value, field_name: &str) -> Vec<String> {
    let Some(value) = response.get("data").and_then(|data| data.get(field_name)) else {
        return Vec::new();
    };
    if let Some(doc_id) = value.get("_docID").and_then(Value::as_str) {
        return vec![doc_id.to_string()];
    }
    value
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get("_docID").and_then(Value::as_str))
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_pairing_mutation_escapes_and_preserves_created_at_on_update() {
        let mutation = upsert_pairing_mutation(
            r#"peer"one"#,
            Some(r#"did:key:agent\one"#),
            &["AgentRequest".to_string(), "AgentResponse".to_string()],
            &[r#"/ip4/127.0.0.1/tcp/4001/p2p/peer"one"#.to_string()],
            &["chat-requests".to_string()],
            "2026-06-10T00:00:00Z",
        );

        assert!(mutation.contains(r#"filter: { peer_id: { _eq: "peer\"one" } }"#));
        assert!(mutation.contains(r#"agent_did: "did:key:agent\\one""#));
        assert!(mutation.contains(r#"collections: ["AgentRequest", "AgentResponse"]"#));
        assert!(
            mutation.contains(r#"replicator_addresses: ["/ip4/127.0.0.1/tcp/4001/p2p/peer\"one"]"#)
        );
        assert!(mutation.contains(r#"profiles: ["chat-requests"]"#));
        assert!(mutation.contains(r#"created_at: "2026-06-10T00:00:00Z""#));

        let update_block = mutation
            .split("update:")
            .nth(1)
            .expect("mutation contains update block");
        assert!(!update_block.contains("created_at"));
        assert!(update_block.contains(r#"updated_at: "2026-06-10T00:00:00Z""#));
    }

    #[test]
    fn upsert_pairing_mutation_emits_null_for_empty_profiles() {
        let mutation = upsert_pairing_mutation(
            "peer-one",
            Some("did:key:agent-one"),
            &["AgentRequest".to_string()],
            &["addr1".to_string()],
            &[],
            "2026-06-10T00:00:00Z",
        );

        assert!(mutation.contains("profiles: null"));
        assert!(!mutation.contains("profiles: []"));
    }

    #[test]
    fn upsert_pairing_mutation_with_null_did_preserves_existing_did_on_update() {
        let mutation = upsert_pairing_mutation(
            "peer-one",
            None,
            &["AgentRequest".to_string()],
            &["addr1".to_string()],
            &["chat-requests".to_string()],
            "2026-06-10T00:00:00Z",
        );

        assert!(mutation.contains("agent_did: null"));
        let update_block = mutation
            .split("update:")
            .nth(1)
            .expect("mutation contains update block");
        assert!(!update_block.contains("agent_did"));
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

    #[test]
    fn mutation_doc_ids_accepts_object_array_and_missing_shapes() {
        assert_eq!(
            mutation_doc_ids(
                &json!({ "data": { "delete_PeerPairingDesired": { "_docID": "doc-a" } } }),
                "delete_PeerPairingDesired"
            ),
            vec!["doc-a"]
        );
        assert_eq!(
            mutation_doc_ids(
                &json!({
                    "data": {
                        "delete_PeerPairingDesired": [
                            { "_docID": "doc-b" },
                            { "_docID": "doc-c" },
                            { "other": true }
                        ]
                    }
                }),
                "delete_PeerPairingDesired"
            ),
            vec!["doc-b", "doc-c"]
        );
        assert!(mutation_doc_ids(&json!({ "data": {} }), "delete_PeerPairingDesired").is_empty());
    }
}
