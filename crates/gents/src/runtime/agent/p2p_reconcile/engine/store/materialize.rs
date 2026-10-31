use std::collections::BTreeSet;

use anyhow::{Context, Result};
use chrono::Utc;
use serde::Deserialize;

use crate::store::config_client::ConfigApplyTxn;
use crate::store::graphql::{escape_graphql_string, fence_arrivals};

use super::super::super::graphql_helpers::graphql_string_list_literal;
use super::super::super::templates::CLIENT_TEMPLATE;
use super::super::enrollment::enrollment_entry_is_fresh_at;
use super::super::EnrollmentEndpointEntry;

/// Materialize a generation verified by the caller's enrollment projection in
/// this same transaction. Desired documents are witnesses, never authority.
/// Native remote merges retain unique-key twins; only their smallest docID
/// owns the index. Address that witness directly and preserve the parked rows.
pub async fn materialize_enrollment_desired_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    entry: &EnrollmentEndpointEntry,
    collections: &BTreeSet<String>,
) -> Result<String> {
    anyhow::ensure!(
        enrollment_entry_is_fresh_at(entry, Utc::now()),
        "enrollment authorization expired before route materialization"
    );
    fence_arrivals(txn, "PeerPairingDesired").await?;
    let response = txn
        .execute(
            r#"{ PeerPairingDesired {
        _docID peer_id node_did source template collections profiles replicator_addresses enrollment_request_digest
        enrollment_authorization_sequence enrollment_authorization_expires_at
    } }"#,
        )
        .await?;
    let rows: Vec<DesiredWitness> = serde_json::from_value(
        response
            .pointer("/data/PeerPairingDesired")
            .context("route materialization query omitted PeerPairingDesired")?
            .clone(),
    )?;
    let mut matches = rows
        .iter()
        .filter(|row| row.peer_id.as_deref() == Some(entry.desired_id.as_str()))
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| left.doc_id.cmp(&right.doc_id));
    for row in &matches {
        anyhow::ensure!(
            row.node_did.as_deref() == Some(owner) && row.source.as_deref() == Some("enrollment"),
            "desired route {} has another materialization owner",
            entry.desired_id
        );
        let sequence = row
            .enrollment_authorization_sequence
            .context("desired route has no authorization generation")?;
        anyhow::ensure!(
            sequence > 0 && sequence as u64 <= entry.authorization_sequence,
            "desired route has a newer or invalid authorization generation"
        );
        if sequence as u64 == entry.authorization_sequence {
            anyhow::ensure!(
                row.enrollment_request_digest.as_deref() == Some(entry.request_digest.as_str())
                    && row.enrollment_authorization_expires_at.as_deref()
                        == Some(entry.authorization_expires_at.as_str()),
                "desired route conflicts with the verified authorization generation"
            );
        }
    }
    if let Some(row) = matches.first() {
        let current_collections = row
            .collections
            .as_deref()
            .unwrap_or_default()
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        if row.enrollment_authorization_sequence == Some(entry.authorization_sequence as i64)
            && row.template.as_deref() == Some(CLIENT_TEMPLATE)
            && current_collections == *collections
            && row
                .profiles
                .as_ref()
                .is_none_or(|profiles| profiles.is_empty())
            && row.replicator_addresses.as_deref() == Some(std::slice::from_ref(&entry.address))
        {
            return Ok(row.doc_id.clone());
        }
    }
    let now = Utc::now().to_rfc3339();
    let fields = format!(
        r#"node_did: "{}", source: "enrollment", template: "{CLIENT_TEMPLATE}",
        collections: {}, profiles: null, replicator_addresses: {},
        enrollment_request_digest: "{}", enrollment_authorization_sequence: {},
        enrollment_authorization_expires_at: "{}", updated_at: "{}""#,
        escape_graphql_string(owner),
        graphql_string_list_literal(collections.iter().map(String::as_str)),
        graphql_string_list_literal([entry.address.as_str()]),
        escape_graphql_string(&entry.request_digest),
        entry.authorization_sequence,
        escape_graphql_string(&entry.authorization_expires_at),
        escape_graphql_string(&now)
    );
    let mutation = match matches.first() {
        Some(row) => format!(
            r#"mutation {{ saved: update_PeerPairingDesired(docID: "{}", input: {{ {fields} }}) {{ _docID }} }}"#,
            escape_graphql_string(&row.doc_id)
        ),
        None => format!(
            r#"mutation {{ saved: create_PeerPairingDesired(input: {{ peer_id: "{}", created_at: "{}", {fields} }}) {{ _docID }} }}"#,
            escape_graphql_string(&entry.desired_id),
            escape_graphql_string(&now)
        ),
    };
    let result = txn.execute(&mutation).await?;
    let saved = result
        .pointer("/data/saved")
        .context("route materialization omitted saved row")?;
    let saved = saved
        .as_array()
        .map_or(Some(saved), |rows| rows.first())
        .context("route materialization saved no row")?;
    Ok(saved
        .get("_docID")
        .and_then(serde_json::Value::as_str)
        .context("route materialization omitted document ID")?
        .to_owned())
}

#[derive(Deserialize)]
struct DesiredWitness {
    #[serde(rename = "_docID")]
    doc_id: String,
    peer_id: Option<String>,
    node_did: Option<String>,
    source: Option<String>,
    template: Option<String>,
    collections: Option<Vec<String>>,
    profiles: Option<Vec<String>>,
    replicator_addresses: Option<Vec<String>>,
    enrollment_request_digest: Option<String>,
    enrollment_authorization_sequence: Option<i64>,
    enrollment_authorization_expires_at: Option<String>,
}

#[cfg(test)]
#[path = "materialize_tests.rs"]
mod tests;
