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

pub(super) async fn prune_expired_intents(
    node: &Arc<EmbeddedNode>,
    identity: &dyn NodeIdentity,
    cursor: &mut Option<String>,
) -> Result<()> {
    let member = escape_graphql_string(identity.did());
    let now = Utc::now();
    let cutoff = escape_graphql_string(&now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    let after = cursor
        .as_ref()
        .map(|id| format!(r#", intent_id: {{ _gt: "{}" }}"#, escape_graphql_string(id)))
        .unwrap_or_default();
    let query = format!(
        r#"{{ EnrollmentRenewalIntent(filter: {{ member_did: {{ _eq: "{member}" }}, expires_at: {{ _lt: "{cutoff}" }} {after} }}, order: {{ intent_id: ASC }}, limit: 32) {{ _docID intent_id network_id admin_did server_peer member_did member_peer predecessor_sequence issued_at expires_at nonce member_sig }} }}"#
    );
    let result =
        graphql_with_transaction_retry(node, &query, "read expired renewal intents").await?;
    let raw = rows::<serde_json::Value>(&result, "EnrollmentRenewalIntent")?;
    *cursor = if raw.len() == 32 {
        raw.last()
            .and_then(|row| row["intent_id"].as_str())
            .map(str::to_owned)
    } else {
        None
    };
    for value in raw {
        let Ok(row) = serde_json::from_value::<Row>(value) else {
            continue;
        };
        let Ok(expires) = chrono::DateTime::parse_from_rfc3339(&row.expires_at) else {
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
        let owned = intent.member_did == identity.did() && intent.intent_id() == row.intent_id;
        let verified = owned
            && matches!(
                identity
                    .verify(
                        identity.did(),
                        &intent.signing_payload(),
                        &intent.member_sig,
                    )
                    .await,
                Ok(true)
            );
        if !super::super::enrollment_exchange::may_prune_renewal(
            owned,
            verified,
            expires.with_timezone(&Utc),
            now,
        ) {
            continue;
        }
        let id = escape_graphql_string(&row.doc_id);
        let mutation = format!(
            r#"mutation {{ delete_EnrollmentRenewalIntent(filter: {{ _docID: {{ _eq: "{id}" }}, member_did: {{ _eq: "{member}" }}, expires_at: {{ _lt: "{cutoff}" }} }}) {{ _docID }} }}"#
        );
        crate::store::config_client::ConfigAccess::write_local(
            node,
            "enrollment.prune_expired_intent",
            &mutation,
        )
        .await?;
    }
    Ok(())
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

#[cfg(test)]
mod cleanup_tests {
    use super::*;

    #[tokio::test]
    async fn cleanup_removes_only_owned_verified_expired_intents() {
        let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let identity = crate::support::materialization_identity_for("did:test:renewal-cleanup");
        let now = Utc::now();
        let mut expected = std::collections::BTreeSet::new();
        for (nonce, expires, valid_signature, member) in [
            (
                "expired",
                now - chrono::Duration::minutes(1),
                true,
                identity.did(),
            ),
            (
                "live",
                now + chrono::Duration::minutes(1),
                true,
                identity.did(),
            ),
            (
                "invalid",
                now - chrono::Duration::minutes(1),
                false,
                identity.did(),
            ),
            (
                "foreign",
                now - chrono::Duration::minutes(1),
                true,
                "did:test:other",
            ),
        ] {
            let mut intent = RenewalIntent {
                network_id: "network".into(),
                admin_did: "admin".into(),
                server_peer: "server".into(),
                member_did: member.into(),
                member_peer: "member".into(),
                predecessor_sequence: 1,
                issued_at: (now - chrono::Duration::minutes(2))
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                expires_at: expires.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                nonce: nonce.into(),
                member_sig: Vec::new(),
            };
            intent.member_sig = if valid_signature {
                identity.sign(&intent.signing_payload()).await.unwrap()
            } else {
                vec![0; 64]
            };
            let id = intent.intent_id();
            if nonce != "expired" {
                expected.insert(id.clone());
            }
            let f = |s: &str| escape_graphql_string(s);
            let query = format!(
                r#"mutation {{ create_EnrollmentRenewalIntent(input: {{ intent_id: "{}", network_id: "{}", admin_did: "{}", server_peer: "{}", member_did: "{}", member_peer: "{}", predecessor_sequence: {}, issued_at: "{}", expires_at: "{}", nonce: "{}", member_sig: "{}" }}) {{ _docID }} }}"#,
                f(&id),
                f(&intent.network_id),
                f(&intent.admin_did),
                f(&intent.server_peer),
                f(&intent.member_did),
                f(&intent.member_peer),
                intent.predecessor_sequence,
                f(&intent.issued_at),
                f(&intent.expires_at),
                f(&intent.nonce),
                f(&bs58::encode(intent.member_sig).into_string())
            );
            crate::store::config_client::ConfigAccess::write_local(
                &node,
                "test.create_renewal_intent",
                &query,
            )
            .await
            .unwrap();
        }
        prune_expired_intents(&node, identity.as_ref(), &mut None)
            .await
            .unwrap();
        let result = graphql_with_transaction_retry(
            &node,
            "{ EnrollmentRenewalIntent { intent_id } }",
            "test.remaining_renewal_intents",
        )
        .await
        .unwrap();
        let rows = rows::<serde_json::Value>(&result, "EnrollmentRenewalIntent").unwrap();
        let ids: std::collections::BTreeSet<_> = rows
            .iter()
            .map(|row| row["intent_id"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(ids, expected);
        node.shutdown().await;
    }
}
