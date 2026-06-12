use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use defra_agent::graphql::escape_graphql_string;
use serde::Serialize;
use serde_json::{json, Value};

use crate::cli::args::{P2pAccessArgs, P2pPairingRefArgs, P2pPairingSetArgs};
use crate::{
    expand_nonempty_values, graphql_rows, graphql_string_list_literal, print_json,
    resolve_config_access,
};

use super::collections::expand_p2p_collection_args;

const PAIRINGS_RECONCILE_NOTE: &str = "Desired pairing rows are reconciled by the running defra-agent runtime. The reconciler only removes wiring it previously applied; use `defra-agent p2p pair --peer <multiaddr>` or the p2p collections/replicators commands for immediate manual live wiring.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PeerPairingDesiredRow {
    peer_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_did: Option<String>,
    collections: Vec<String>,
    replicator_addresses: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
}

pub(super) async fn p2p_pairings_list(args: P2pAccessArgs) -> Result<()> {
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), true).await?;
    let rows = graphql_rows(&access, "PeerPairingDesired", pairings_list_query())
        .await
        .context("loading PeerPairingDesired rows")?;
    let pairings = parse_pairing_rows(rows);
    let count = pairings.len();
    print_json(&json!({
        "status": "ok",
        "home": home_dir,
        "access_mode": access.mode(),
        "pairings": pairings,
        "count": count,
        "note": PAIRINGS_RECONCILE_NOTE,
    }))?;
    Ok(())
}

pub(super) async fn p2p_pairings_set(args: P2pPairingSetArgs) -> Result<()> {
    let peer_id = required_trimmed(&args.peer_id, "--peer")?;
    let agent_did = required_trimmed(&args.agent_did, "--did")?;
    let addresses = expand_nonempty_values(&args.addresses, "--address")?;
    let collections = expand_p2p_collection_args(&args.collections, &args.profiles)?;
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let mutation = upsert_pairing_mutation(&peer_id, &agent_did, &collections, &addresses, &now);
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), true).await?;
    let response = access
        .execute(&mutation)
        .await
        .context("writing PeerPairingDesired row")?;
    let doc_id = crate::extract_mutation_doc_id(&response, "PeerPairingDesired")
        .context("reading PeerPairingDesired mutation doc id")?;

    print_json(&json!({
        "status": "pairing_set",
        "home": home_dir,
        "access_mode": access.mode(),
        "peer_id": peer_id,
        "agent_did": agent_did,
        "collections": collections,
        "replicator_addresses": addresses,
        "doc_id": doc_id,
        "note": PAIRINGS_RECONCILE_NOTE,
    }))?;
    Ok(())
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
        "note": PAIRINGS_RECONCILE_NOTE,
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
            created_at
            updated_at
        }
    }"#
}

fn upsert_pairing_mutation(
    peer_id: &str,
    agent_did: &str,
    collections: &[String],
    addresses: &[String],
    now: &str,
) -> String {
    let peer_id = escape_graphql_string(peer_id);
    let agent_did = escape_graphql_string(agent_did);
    let collections = graphql_string_list_literal(collections);
    let addresses = graphql_string_list_literal(addresses);
    let now = escape_graphql_string(now);

    format!(
        r#"mutation {{
            upsert_PeerPairingDesired(
                filter: {{ peer_id: {{ _eq: "{peer_id}" }} }},
                add: {{
                    peer_id: "{peer_id}",
                    agent_did: "{agent_did}",
                    collections: {collections},
                    replicator_addresses: {addresses},
                    created_at: "{now}",
                    updated_at: "{now}"
                }},
                update: {{
                    agent_did: "{agent_did}",
                    collections: {collections},
                    replicator_addresses: {addresses},
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
                created_at: optional_string(&row, "created_at"),
                updated_at: optional_string(&row, "updated_at"),
            })
        })
        .collect::<Vec<_>>();
    pairings.sort_by(|left, right| left.peer_id.cmp(&right.peer_id));
    pairings
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
            r#"did:key:agent\one"#,
            &["AgentRequest".to_string(), "AgentResponse".to_string()],
            &[r#"/ip4/127.0.0.1/tcp/4001/p2p/peer"one"#.to_string()],
            "2026-06-10T00:00:00Z",
        );

        assert!(mutation.contains(r#"filter: { peer_id: { _eq: "peer\"one" } }"#));
        assert!(mutation.contains(r#"agent_did: "did:key:agent\\one""#));
        assert!(mutation.contains(r#"collections: ["AgentRequest", "AgentResponse"]"#));
        assert!(
            mutation.contains(r#"replicator_addresses: ["/ip4/127.0.0.1/tcp/4001/p2p/peer\"one"]"#)
        );
        assert!(mutation.contains(r#"created_at: "2026-06-10T00:00:00Z""#));

        let update_block = mutation
            .split("update:")
            .nth(1)
            .expect("mutation contains update block");
        assert!(!update_block.contains("created_at"));
        assert!(update_block.contains(r#"updated_at: "2026-06-10T00:00:00Z""#));
    }

    #[test]
    fn parse_pairing_rows_sorts_and_ignores_incomplete_rows() {
        let rows = vec![
            json!({
                "peer_id": "peer-b",
                "agent_did": " did:key:b ",
                "collections": ["AgentResponse", "", 3],
                "replicator_addresses": ["/ip4/2/tcp/4001"],
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
                "replicator_addresses": null
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
