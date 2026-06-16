//! `p2p network` subcommands: register, list, rm.
//!
//! `p2p network` is a top-level declarative noun (alongside `p2p pairings`).
//! It targets the `PeerRegistry` collection, which is the foundation of the
//! service-discovery layer described in
//! `docs/superpowers/specs/2026-06-13-peer-registry-service-discovery-design.md`.

use std::io::{self, Write};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use defra_agent::agent::p2p_reconcile::discovery::{heartbeat_is_fresh, REGISTRY_STALE_AFTER};
use defra_agent::agent::p2p_reconcile::registry::{
    registry_upsert_mutation, validate_offered_templates, RegistryEntry, UpsertKind,
};
use defra_agent::graphql::escape_graphql_string;
use serde::Serialize;
use serde_json::{json, Value};

use crate::cli::args::{P2pAccessArgs, P2pNetworkListArgs, P2pNetworkRegisterArgs};
use crate::cli::output_format::OutputFormat;
use crate::{graphql_rows, print_json, resolve_config_access};

use super::output::load_live_http_p2p_status;

// ---------------------------------------------------------------------------
// Row types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PeerRegistryRow {
    peer_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_did: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    network_id: String,
    templates: Vec<String>,
    addresses: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
    online: bool,
    paired: bool,
}

// ---------------------------------------------------------------------------
// p2p network register
// ---------------------------------------------------------------------------

pub(super) async fn p2p_network_register(args: P2pNetworkRegisterArgs) -> Result<()> {
    let graphql = crate::resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), true).await?;

    // Resolve peer_id + addresses from the live runtime.
    let p2p_status = load_live_http_p2p_status(args.home.as_deref(), &graphql).await;
    let peer_id = p2p_status
        .get("p2p_peer_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
        .context(
            "runtime did not report a P2P peer id; is the runtime running with P2P enabled?",
        )?;
    let addresses: Vec<String> = p2p_status
        .get("p2p_listen_addresses")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(|v| {
                    if v.starts_with('/') && !v.contains("/p2p/") {
                        format!("{v}/p2p/{peer_id}")
                    } else {
                        v.to_string()
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    // Resolve agent DID from home identity or init config.
    let agent_did = crate::resolve_agent_did(args.home.as_deref(), None)
        .context("resolving local agent DID")?;

    // Validate offered templates against the built-in catalog; an empty/unknown
    // set falls back to the default offer (conversation + agent-config).
    let templates = validate_offered_templates(args.templates.iter().map(String::as_str));
    let network_id = args.network_id.as_deref().unwrap_or("default").to_string();
    let display_name = args
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned);

    let entry = RegistryEntry {
        peer_id: peer_id.clone(),
        agent_did: agent_did.clone(),
        addresses: addresses.clone(),
        templates: templates.clone(),
        display_name: display_name.clone(),
        status: "online".to_string(),
        network_id: network_id.clone(),
        invited_by: None,
    };
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    // Full variant: the operator explicitly supplied display_name and profiles,
    // so the update branch must write them (overwriting any stale heartbeat value).
    let mutation = registry_upsert_mutation(&entry, &now, UpsertKind::Full);
    access
        .execute(&mutation)
        .await
        .context("writing PeerRegistry row")?;

    tracing::debug!(
        peer_id = %peer_id,
        agent_did = %agent_did,
        network_id = %network_id,
        "p2p network register: self-registration written"
    );

    print_json(&json!({
        "status": "registered",
        "home": home_dir,
        "peer_id": peer_id,
        "agent_did": agent_did,
        "display_name": display_name,
        "templates": templates,
        "network_id": network_id,
        "addresses": addresses,
    }))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// p2p network list
// ---------------------------------------------------------------------------

pub(super) async fn p2p_network_list(args: P2pNetworkListArgs) -> Result<()> {
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), true).await?;

    // Load PeerRegistry rows.
    let registry_rows = graphql_rows(&access, "PeerRegistry", registry_list_query())
        .await
        .context("loading PeerRegistry rows")?;

    // Load the set of peer_ids that have a PeerPairingDesired row.
    let desired_rows = graphql_rows(
        &access,
        "PeerPairingDesired",
        pairing_desired_peer_ids_query(),
    )
    .await
    .unwrap_or_default();
    let paired_peers: std::collections::BTreeSet<String> = desired_rows
        .into_iter()
        .filter_map(|row| {
            row.get("peer_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(ToOwned::to_owned)
        })
        .collect();

    let now = Utc::now();
    let peers = parse_registry_rows(registry_rows, &paired_peers, now);
    let count = peers.len();

    match args.output.ensure_supported(
        "p2p network list",
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

// ---------------------------------------------------------------------------
// p2p network rm
// ---------------------------------------------------------------------------

pub(super) async fn p2p_network_rm(args: P2pAccessArgs) -> Result<()> {
    let graphql = crate::resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), true).await?;

    // Resolve peer_id for this node.
    let p2p_status = load_live_http_p2p_status(args.home.as_deref(), &graphql).await;
    let peer_id = p2p_status
        .get("p2p_peer_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
        .context(
            "runtime did not report a P2P peer id; is the runtime running with P2P enabled?",
        )?;

    let mutation = delete_registry_mutation(&peer_id);
    let response = access
        .execute(&mutation)
        .await
        .context("deleting PeerRegistry row")?;
    let removed_count = count_deleted(&response, "delete_PeerRegistry");

    tracing::debug!(
        peer_id = %peer_id,
        removed_count,
        "p2p network rm: deregistered"
    );

    print_json(&json!({
        "status": "deregistered",
        "home": home_dir,
        "peer_id": peer_id,
        "removed_count": removed_count,
    }))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// GraphQL helpers
// ---------------------------------------------------------------------------

fn registry_list_query() -> &'static str {
    r#"query {
        PeerRegistry {
            peer_id
            agent_did
            display_name
            network_id
            templates
            addresses
            status
            updated_at
        }
    }"#
}

fn pairing_desired_peer_ids_query() -> &'static str {
    r#"query {
        PeerPairingDesired {
            peer_id
        }
    }"#
}

fn delete_registry_mutation(peer_id: &str) -> String {
    let peer_id = escape_graphql_string(peer_id);
    format!(
        r#"mutation {{
            delete_PeerRegistry(
                filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}
            ) {{ _docID }}
        }}"#
    )
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

fn parse_registry_rows(
    rows: Vec<Value>,
    paired_peers: &std::collections::BTreeSet<String>,
    now: chrono::DateTime<Utc>,
) -> Vec<PeerRegistryRow> {
    let mut peers: Vec<PeerRegistryRow> = rows
        .into_iter()
        .filter_map(|row| {
            let peer_id = row
                .get("peer_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())?
                .to_string();

            let status = optional_string(&row, "status");
            let updated_at = optional_string(&row, "updated_at");

            // Liveness: status=="online" AND heartbeat within REGISTRY_STALE_AFTER.
            // Mirrors DiscoveredEntry::from_row in discovery.rs.
            let status_online = status.as_deref() == Some("online");
            let fresh = updated_at
                .as_deref()
                .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw.trim()).ok())
                .map(|ts| ts.with_timezone(&Utc))
                .map(|ts| heartbeat_is_fresh(ts, now, REGISTRY_STALE_AFTER))
                .unwrap_or(false);
            let online = status_online && fresh;
            let paired = paired_peers.contains(&peer_id);

            Some(PeerRegistryRow {
                peer_id,
                agent_did: optional_string(&row, "agent_did"),
                display_name: optional_string(&row, "display_name"),
                network_id: optional_string(&row, "network_id")
                    .unwrap_or_else(|| "default".to_string()),
                templates: string_list(&row, "templates"),
                addresses: string_list(&row, "addresses"),
                status,
                updated_at,
                online,
                paired,
            })
        })
        .collect();
    peers.sort_by(|a, b| a.peer_id.cmp(&b.peer_id));
    peers
}

fn optional_string(row: &Value, field: &str) -> Option<String> {
    row.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
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
                .filter(|v| !v.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn count_deleted(response: &Value, field_name: &str) -> usize {
    let Some(value) = response.get("data").and_then(|d| d.get(field_name)) else {
        return 0;
    };
    if value.get("_docID").is_some() {
        return 1;
    }
    value
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|row| row.get("_docID").is_some())
                .count()
        })
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Table rendering
// ---------------------------------------------------------------------------

fn print_network_table(rows: &[PeerRegistryRow]) -> Result<()> {
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
                row.agent_did.clone().unwrap_or_else(|| "-".to_string()),
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::Cli;
    use clap::Parser;
    use serde_json::json;

    // ---- parse tests ----

    #[test]
    fn p2p_network_list_table_parses() {
        let cli =
            Cli::try_parse_from(["defra-agent", "p2p", "network", "list", "--output", "table"])
                .expect("p2p network list --output table should parse");
        match cli.command {
            crate::cli::args::Command::P2p {
                command: crate::cli::args::P2pCommand::Network { command },
            } => match command {
                crate::cli::args::P2pNetworkCommand::List(args) => {
                    assert_eq!(args.output, OutputFormat::Table);
                }
                _ => panic!("expected network list"),
            },
            _ => panic!("expected p2p network"),
        }
    }

    #[test]
    fn p2p_network_list_json_parses() {
        let cli =
            Cli::try_parse_from(["defra-agent", "p2p", "network", "list", "--output", "json"])
                .expect("p2p network list --output json should parse");
        match cli.command {
            crate::cli::args::Command::P2p {
                command: crate::cli::args::P2pCommand::Network { command },
            } => match command {
                crate::cli::args::P2pNetworkCommand::List(args) => {
                    assert_eq!(args.output, OutputFormat::Json);
                }
                _ => panic!("expected network list"),
            },
            _ => panic!("expected p2p network"),
        }
    }

    #[test]
    fn p2p_network_list_default_output_is_table() {
        let cli = Cli::try_parse_from(["defra-agent", "p2p", "network", "list"])
            .expect("p2p network list should parse");
        match cli.command {
            crate::cli::args::Command::P2p {
                command: crate::cli::args::P2pCommand::Network { command },
            } => match command {
                crate::cli::args::P2pNetworkCommand::List(args) => {
                    assert_eq!(args.output, OutputFormat::Table);
                }
                _ => panic!("expected network list"),
            },
            _ => panic!("expected p2p network"),
        }
    }

    #[test]
    fn p2p_network_register_parses() {
        let cli = Cli::try_parse_from([
            "defra-agent",
            "p2p",
            "network",
            "register",
            "--display-name",
            "my-node",
            "--template",
            "conversation",
            "--network",
            "staging",
        ])
        .expect("p2p network register should parse");
        match cli.command {
            crate::cli::args::Command::P2p {
                command: crate::cli::args::P2pCommand::Network { command },
            } => match command {
                crate::cli::args::P2pNetworkCommand::Register(args) => {
                    assert_eq!(args.display_name.as_deref(), Some("my-node"));
                    assert_eq!(args.templates, vec!["conversation".to_string()]);
                    assert_eq!(args.network_id.as_deref(), Some("staging"));
                }
                _ => panic!("expected network register"),
            },
            _ => panic!("expected p2p network"),
        }
    }

    #[test]
    fn p2p_network_register_bare_parses() {
        let cli = Cli::try_parse_from([
            "defra-agent",
            "p2p",
            "network",
            "register",
            "--display-name",
            "x",
            "--template",
            "conversation",
        ])
        .expect("p2p network register minimal should parse");
        match cli.command {
            crate::cli::args::Command::P2p {
                command: crate::cli::args::P2pCommand::Network { command },
            } => match command {
                crate::cli::args::P2pNetworkCommand::Register(args) => {
                    assert_eq!(args.display_name.as_deref(), Some("x"));
                    assert!(args.network_id.is_none()); // defaults to "default" at runtime
                }
                _ => panic!("expected network register"),
            },
            _ => panic!("expected p2p network"),
        }
    }

    #[test]
    fn p2p_network_rm_parses() {
        let cli = Cli::try_parse_from(["defra-agent", "p2p", "network", "rm"])
            .expect("p2p network rm should parse");
        match cli.command {
            crate::cli::args::Command::P2p {
                command: crate::cli::args::P2pCommand::Network { command },
            } => match command {
                crate::cli::args::P2pNetworkCommand::Rm(_) => {}
                _ => panic!("expected network rm"),
            },
            _ => panic!("expected p2p network"),
        }
    }

    #[test]
    fn p2p_network_create_parses() {
        let cli = Cli::try_parse_from([
            "defra-agent",
            "p2p",
            "network",
            "create",
            "--name",
            "Fleet One",
            "--output",
            "json",
        ])
        .expect("p2p network create should parse");
        match cli.command {
            crate::cli::args::Command::P2p {
                command: crate::cli::args::P2pCommand::Network { command },
            } => match command {
                crate::cli::args::P2pNetworkCommand::Create(args) => {
                    assert_eq!(args.name, "Fleet One");
                    assert_eq!(args.output, OutputFormat::Json);
                }
                _ => panic!("expected network create"),
            },
            _ => panic!("expected p2p network"),
        }
    }

    #[test]
    fn p2p_network_grant_revoke_parse_member_did_and_output() {
        for (subcommand, expected_json) in [("grant", false), ("revoke", true)] {
            let mut argv = vec![
                "defra-agent",
                "p2p",
                "network",
                subcommand,
                "did:key:zMember",
            ];
            if expected_json {
                argv.extend(["--output", "json"]);
            }
            let cli = Cli::try_parse_from(argv).expect("p2p network grant/revoke should parse");
            match cli.command {
                crate::cli::args::Command::P2p {
                    command: crate::cli::args::P2pCommand::Network { command },
                } => match command {
                    crate::cli::args::P2pNetworkCommand::Grant(args) => {
                        assert_eq!(subcommand, "grant");
                        assert_eq!(args.member_did, "did:key:zMember");
                        assert_eq!(args.output, OutputFormat::Text);
                    }
                    crate::cli::args::P2pNetworkCommand::Revoke(args) => {
                        assert_eq!(subcommand, "revoke");
                        assert_eq!(args.member_did, "did:key:zMember");
                        assert_eq!(args.output, OutputFormat::Json);
                    }
                    _ => panic!("expected network grant/revoke"),
                },
                _ => panic!("expected p2p network"),
            }
        }
    }

    // ---- row parsing / liveness tests ----

    fn make_row(
        peer_id: &str,
        status: Option<&str>,
        updated_at: Option<&str>,
    ) -> serde_json::Value {
        json!({
            "peer_id": peer_id,
            "agent_did": "did:key:test",
            "display_name": serde_json::Value::Null,
            "network_id": "default",
            "templates": serde_json::Value::Null,
            "addresses": serde_json::Value::Null,
            "status": status,
            "updated_at": updated_at,
        })
    }

    fn now_rfc3339() -> chrono::DateTime<Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-06-13T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn fresh_online_row_is_online() {
        let now = now_rfc3339();
        // 10 seconds ago — well within REGISTRY_STALE_AFTER (90s)
        let rows = vec![make_row(
            "peer-a",
            Some("online"),
            Some("2026-06-13T11:59:50Z"),
        )];
        let peers = parse_registry_rows(rows, &Default::default(), now);
        assert_eq!(peers.len(), 1);
        assert!(peers[0].online, "fresh online row should be online");
    }

    #[test]
    fn stale_online_row_is_offline() {
        let now = now_rfc3339();
        // 200 seconds ago — beyond REGISTRY_STALE_AFTER (90s)
        let rows = vec![make_row(
            "peer-a",
            Some("online"),
            Some("2026-06-13T11:56:40Z"),
        )];
        let peers = parse_registry_rows(rows, &Default::default(), now);
        assert_eq!(peers.len(), 1);
        assert!(!peers[0].online, "stale online row should not be online");
    }

    #[test]
    fn offline_status_row_is_not_online() {
        let now = now_rfc3339();
        let rows = vec![make_row(
            "peer-a",
            Some("offline"),
            Some("2026-06-13T11:59:50Z"),
        )];
        let peers = parse_registry_rows(rows, &Default::default(), now);
        assert_eq!(peers.len(), 1);
        assert!(!peers[0].online, "offline status row should not be online");
    }

    #[test]
    fn no_heartbeat_row_is_not_online() {
        let now = now_rfc3339();
        let rows = vec![make_row("peer-a", Some("online"), None)];
        let peers = parse_registry_rows(rows, &Default::default(), now);
        assert_eq!(peers.len(), 1);
        assert!(
            !peers[0].online,
            "row without heartbeat should not be online"
        );
    }

    #[test]
    fn peer_with_matching_desired_row_is_paired() {
        let now = now_rfc3339();
        let rows = vec![make_row(
            "peer-a",
            Some("online"),
            Some("2026-06-13T11:59:50Z"),
        )];
        let paired: std::collections::BTreeSet<String> =
            ["peer-a".to_string()].into_iter().collect();
        let peers = parse_registry_rows(rows, &paired, now);
        assert_eq!(peers.len(), 1);
        assert!(
            peers[0].paired,
            "peer with matching desired row should be paired"
        );
    }

    #[test]
    fn peer_without_desired_row_is_not_paired() {
        let now = now_rfc3339();
        let rows = vec![make_row(
            "peer-b",
            Some("online"),
            Some("2026-06-13T11:59:50Z"),
        )];
        let paired: std::collections::BTreeSet<String> =
            ["peer-a".to_string()].into_iter().collect();
        let peers = parse_registry_rows(rows, &paired, now);
        assert_eq!(peers.len(), 1);
        assert!(
            !peers[0].paired,
            "peer without desired row should not be paired"
        );
    }

    #[test]
    fn registry_upsert_mutation_fields_are_correct() {
        // Verify that registry_upsert_mutation (from registry.rs) is called
        // with the correct fields when called from register logic.
        let entry = RegistryEntry {
            peer_id: "test-peer-1".to_string(),
            agent_did: "did:key:test".to_string(),
            addresses: vec!["/ip4/127.0.0.1/tcp/4001/p2p/test-peer-1".to_string()],
            templates: vec!["conversation".to_string()],
            display_name: Some("my-node".to_string()),
            status: "online".to_string(),
            network_id: "default".to_string(),
            invited_by: None,
        };
        let now = "2026-06-13T00:00:00Z";
        let mutation = registry_upsert_mutation(&entry, now, UpsertKind::Full);

        assert!(mutation.contains(r#"peer_id: { _eq: "test-peer-1" }"#));
        assert!(mutation.contains(r#"agent_did: "did:key:test""#));
        assert!(mutation.contains(r#"status: "online""#));
        assert!(mutation.contains(r#"network_id: "default""#));
        assert!(mutation.contains(r#"display_name: "my-node""#));
        assert!(mutation.contains(r#"templates: ["conversation"]"#));
        assert!(!mutation.contains("templates: []"));
        assert!(mutation.contains(r#"registered_at: "2026-06-13T00:00:00Z""#));
    }

    #[test]
    fn delete_registry_mutation_uses_peer_id_filter() {
        let mutation = delete_registry_mutation(r#"peer"one"#);
        assert!(mutation.contains(r#"filter: { peer_id: { _eq: "peer\"one" } }"#));
        assert!(mutation.contains("delete_PeerRegistry"));
    }

    #[test]
    fn rows_are_sorted_by_peer_id() {
        let now = now_rfc3339();
        let rows = vec![
            make_row("peer-z", Some("online"), Some("2026-06-13T11:59:50Z")),
            make_row("peer-a", Some("online"), Some("2026-06-13T11:59:50Z")),
            make_row("peer-m", None, None),
        ];
        let peers = parse_registry_rows(rows, &Default::default(), now);
        assert_eq!(peers[0].peer_id, "peer-a");
        assert_eq!(peers[1].peer_id, "peer-m");
        assert_eq!(peers[2].peer_id, "peer-z");
    }

    #[test]
    fn incomplete_rows_without_peer_id_are_skipped() {
        let now = now_rfc3339();
        let rows = vec![
            json!({ "peer_id": "", "status": "online" }),
            json!({ "peer_id": null }),
            make_row("peer-a", Some("online"), Some("2026-06-13T11:59:50Z")),
        ];
        let peers = parse_registry_rows(rows, &Default::default(), now);
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].peer_id, "peer-a");
    }

    #[test]
    fn count_deleted_handles_single_and_array_and_missing() {
        assert_eq!(
            count_deleted(
                &json!({ "data": { "delete_PeerRegistry": { "_docID": "doc-a" } } }),
                "delete_PeerRegistry"
            ),
            1
        );
        assert_eq!(
            count_deleted(
                &json!({
                    "data": {
                        "delete_PeerRegistry": [
                            { "_docID": "doc-b" },
                            { "_docID": "doc-c" },
                            { "other": true }
                        ]
                    }
                }),
                "delete_PeerRegistry"
            ),
            2
        );
        assert_eq!(
            count_deleted(&json!({ "data": {} }), "delete_PeerRegistry"),
            0
        );
    }
}
