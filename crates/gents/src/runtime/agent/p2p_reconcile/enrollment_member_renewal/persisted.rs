use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use defra_node::EmbeddedNode;
use gents_protocol::enrollment::RenewalIntent;
use serde::Deserialize;

use crate::store::graphql::{escape_graphql_string, graphql_with_transaction_retry, rows};
use crate::store::identity::NodeIdentity;

use super::super::enrollment_candidate::push_enrollment_document;
use super::super::enrollment_projection::ApprovedStatusEnrollment;

#[derive(Deserialize)]
struct Row {
    #[serde(rename = "_docID")]
    doc_id: String,
    intent_id: String,
    network_id: String,
    admin_did: String,
    server_peer: String,
    member_did: String,
    member_peer: String,
    predecessor_sequence: i64,
    issued_at: String,
    expires_at: String,
    nonce: String,
    member_sig: String,
}

pub(super) async fn find_member_intent(
    node: &Arc<EmbeddedNode>,
    identity: &dyn NodeIdentity,
    approval: &ApprovedStatusEnrollment,
    local_peer: &str,
    cursor: &mut Option<String>,
) -> Result<Option<(String, RenewalIntent)>> {
    let f = |value: &str| escape_graphql_string(value);
    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let after = cursor
        .as_ref()
        .map(|id| format!(r#", intent_id: {{ _gt: "{}" }}"#, f(id)))
        .unwrap_or_default();
    let query = format!(
        r#"{{ EnrollmentRenewalIntent(filter: {{ network_id: {{ _eq: "{}" }}, admin_did: {{ _eq: "{}" }}, server_peer: {{ _eq: "{}" }}, member_did: {{ _eq: "{}" }}, member_peer: {{ _eq: "{}" }}, predecessor_sequence: {{ _eq: {} }}, expires_at: {{ _gt: "{}" }} {after} }}, order: {{ intent_id: ASC }}, limit: 32) {{ _docID intent_id network_id admin_did server_peer member_did member_peer predecessor_sequence issued_at expires_at nonce member_sig }} }}"#,
        f(&approval.network_id),
        f(&approval.admin_did),
        f(&approval.server_peer),
        f(identity.did()),
        f(local_peer),
        approval.authorization_sequence,
        f(&now)
    );
    let result = graphql_with_transaction_retry(node, &query, "read local renewal intent").await?;
    let raw = rows::<serde_json::Value>(&result, "EnrollmentRenewalIntent")?;
    *cursor = if raw.len() == 32 {
        raw.last()
            .and_then(|row| row.get("intent_id"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    } else {
        None
    };
    for value in raw {
        let Ok(row) = serde_json::from_value::<Row>(value) else {
            continue;
        };
        let Ok(sequence) = u64::try_from(row.predecessor_sequence) else {
            continue;
        };
        let Ok(signature) = bs58::decode(&row.member_sig).into_vec() else {
            continue;
        };
        let intent = RenewalIntent {
            network_id: row.network_id,
            admin_did: row.admin_did,
            server_peer: row.server_peer,
            member_did: row.member_did,
            member_peer: row.member_peer,
            predecessor_sequence: sequence,
            issued_at: row.issued_at,
            expires_at: row.expires_at,
            nonce: row.nonce,
            member_sig: signature,
        };
        if intent.intent_id() == row.intent_id
            && intent.validate_at(Utc::now()).is_ok()
            && identity
                .verify(
                    &intent.member_did,
                    &intent.signing_payload(),
                    &intent.member_sig,
                )
                .await?
        {
            return Ok(Some((row.doc_id, intent)));
        }
    }
    Ok(None)
}

pub(super) async fn resend_member_intent(
    node: &Arc<EmbeddedNode>,
    approval: &ApprovedStatusEnrollment,
    intent: &RenewalIntent,
    doc_id: &str,
) -> Result<()> {
    intent.validate_at(Utc::now())?;
    anyhow::ensure!(
        intent.network_id == approval.network_id
            && intent.admin_did == approval.admin_did
            && intent.server_peer == approval.server_peer
            && intent.predecessor_sequence == approval.authorization_sequence,
        "renewal intent does not match the current enrollment"
    );
    let p2p = crate::store::identity::node_p2p(node).context("runtime P2P is unavailable")?;
    push_enrollment_document(
        &p2p,
        &approval.server_ticket,
        &approval.server_peer,
        &approval.admin_did,
        "EnrollmentRenewalIntent",
        ("intent_id", &intent.intent_id()),
        doc_id,
    )
    .await
}
