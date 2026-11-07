use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use defra_node::EmbeddedNode;
use gents_protocol::enrollment::{RenewalIntent, RenewalOfferResponse};
use serde::Deserialize;

use crate::store::config_client::ConfigAccess;
use crate::store::graphql::{escape_graphql_string, graphql_with_transaction_retry, rows};
use crate::store::identity::NodeIdentity;

use super::enrollment_candidate::push_enrollment_document;
use super::enrollment_store::GraphqlEnrollmentStore;

#[derive(Deserialize)]
struct OfferRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    intent_id: String,
    member_did: String,
    offer_token: String,
    expires_at: String,
    admin_did: String,
    admin_sig: String,
}

#[derive(Deserialize)]
struct IntentRow {
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

pub(super) fn may_prune_renewal(
    owned: bool,
    verified: bool,
    expires: chrono::DateTime<Utc>,
    now: chrono::DateTime<Utc>,
) -> bool {
    owned && verified && expires <= now
}

/// Visit a bounded, stable page so invalid or unresponsive members cannot
/// prevent later requests from being examined.
pub(super) async fn read_intent_page(
    node: &Arc<EmbeddedNode>,
    admin_did: &str,
    cursor: &mut Option<String>,
) -> Result<Vec<RenewalIntent>> {
    let admin = escape_graphql_string(admin_did);
    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let after = cursor
        .as_ref()
        .map(|id| format!(r#", intent_id: {{ _gt: "{}" }}"#, escape_graphql_string(id)))
        .unwrap_or_default();
    let query = format!(
        r#"{{ EnrollmentRenewalIntent(filter: {{ admin_did: {{ _eq: "{admin}" }}, expires_at: {{ _gt: "{now}" }} {after} }}, order: {{ intent_id: ASC }}, limit: 4) {{ intent_id network_id admin_did server_peer member_did member_peer predecessor_sequence issued_at expires_at nonce member_sig }} }}"#
    );
    let response = graphql_with_transaction_retry(node, &query, "read renewal intents").await?;
    let raw = rows::<serde_json::Value>(&response, "EnrollmentRenewalIntent")?;
    *cursor = if raw.len() == 4 {
        raw.last()
            .and_then(|row| row.get("intent_id"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    } else {
        None
    };
    let mut intents = Vec::new();
    for value in raw {
        let Ok(row) = serde_json::from_value::<IntentRow>(value) else {
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
        if intent.intent_id() == row.intent_id && intent.validate_at(Utc::now()).is_ok() {
            intents.push(intent);
        }
    }
    Ok(intents)
}

pub(super) async fn prune_expired_offers(
    node: &Arc<EmbeddedNode>,
    identity: &dyn NodeIdentity,
    cursor: &mut Option<String>,
) -> Result<()> {
    let admin = escape_graphql_string(identity.did());
    let now = Utc::now();
    let cutoff = escape_graphql_string(&now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    let after = cursor
        .as_ref()
        .map(|id| format!(r#", intent_id: {{ _gt: "{}" }}"#, escape_graphql_string(id)))
        .unwrap_or_default();
    let query = format!(
        r#"{{ EnrollmentRenewalOffer(filter: {{ admin_did: {{ _eq: "{admin}" }}, expires_at: {{ _lt: "{cutoff}" }} {after} }}, order: {{ intent_id: ASC }}, limit: 32) {{ _docID intent_id member_did offer_token expires_at admin_did admin_sig }} }}"#
    );
    let response =
        graphql_with_transaction_retry(node, &query, "read expired renewal offers").await?;
    let raw = rows::<serde_json::Value>(&response, "EnrollmentRenewalOffer")?;
    *cursor = if raw.len() == 32 {
        raw.last()
            .and_then(|row| row["intent_id"].as_str())
            .map(str::to_owned)
    } else {
        None
    };
    for value in raw {
        let Ok(row) = serde_json::from_value::<OfferRow>(value) else {
            continue;
        };
        let Ok(offer) = gents_protocol::enrollment::decode_offer(&row.offer_token) else {
            continue;
        };
        let Ok(expires) = chrono::DateTime::parse_from_rfc3339(&row.expires_at) else {
            continue;
        };
        let Ok(signature) = bs58::decode(&row.admin_sig).into_vec() else {
            continue;
        };
        let response = RenewalOfferResponse {
            intent_id: row.intent_id,
            member_did: row.member_did,
            offer_token: row.offer_token,
            admin_did: row.admin_did,
            admin_sig: signature,
        };
        let owned = response.admin_did == identity.did()
            && chrono::DateTime::parse_from_rfc3339(&offer.expires_at)
                .is_ok_and(|signed_expiry| signed_expiry == expires);
        let verified = owned
            && matches!(
                identity
                    .verify(
                        identity.did(),
                        &response.signing_payload(),
                        &response.admin_sig,
                    )
                    .await,
                Ok(true)
            );
        if !may_prune_renewal(owned, verified, expires.with_timezone(&Utc), now) {
            continue;
        }
        let id = escape_graphql_string(&row.doc_id);
        let mutation = format!(
            r#"mutation {{ delete_EnrollmentRenewalOffer(filter: {{ _docID: {{ _eq: "{id}" }}, admin_did: {{ _eq: "{admin}" }}, expires_at: {{ _lt: "{cutoff}" }} }}) {{ _docID }} }}"#
        );
        ConfigAccess::write_local(node, "enrollment.prune_expired_offer", &mutation).await?;
    }
    Ok(())
}

async fn existing_response(
    node: &Arc<EmbeddedNode>,
    intent: &RenewalIntent,
    identity: &dyn NodeIdentity,
) -> Result<Option<(String, RenewalOfferResponse)>> {
    let id = escape_graphql_string(&intent.intent_id());
    let query = format!(
        r#"{{ EnrollmentRenewalOffer(filter: {{ intent_id: {{ _eq: "{id}" }} }}) {{ _docID intent_id member_did offer_token expires_at admin_did admin_sig }} }}"#
    );
    let response =
        graphql_with_transaction_retry(node, &query, "read renewal offer response").await?;
    let found = rows::<OfferRow>(&response, "EnrollmentRenewalOffer")?;
    let [row] = found.as_slice() else {
        anyhow::ensure!(found.is_empty(), "conflicting renewal offer responses");
        return Ok(None);
    };
    let signed_expiry = gents_protocol::enrollment::decode_offer(&row.offer_token)?.expires_at;
    anyhow::ensure!(
        chrono::DateTime::parse_from_rfc3339(&signed_expiry)?
            == chrono::DateTime::parse_from_rfc3339(&row.expires_at)?,
        "renewal offer expiry does not match its signed offer"
    );
    let offer = RenewalOfferResponse {
        intent_id: row.intent_id.clone(),
        member_did: row.member_did.clone(),
        offer_token: row.offer_token.clone(),
        admin_did: row.admin_did.clone(),
        admin_sig: bs58::decode(&row.admin_sig)
            .into_vec()
            .context("decode renewal response signature")?,
    };
    offer.validate_for(intent, Utc::now())?;
    anyhow::ensure!(
        identity
            .verify(&offer.admin_did, &offer.signing_payload(), &offer.admin_sig)
            .await?,
        "invalid stored renewal response signature"
    );
    Ok(Some((row.doc_id.clone(), offer)))
}

/// Accept only the signed reply for this node's own still-fresh intent, then
/// reuse the normal offer verification and idempotent enrollment request owner.
pub async fn consume_renewal_offer(
    node: &Arc<EmbeddedNode>,
    identity: &dyn NodeIdentity,
    intent: &RenewalIntent,
) -> Result<Option<gents_protocol::enrollment::EnrollmentRequestRecord>> {
    intent.validate_at(Utc::now())?;
    anyhow::ensure!(
        intent.member_did == identity.did(),
        "renewal intent belongs to another member"
    );
    anyhow::ensure!(
        identity
            .verify(
                &intent.member_did,
                &intent.signing_payload(),
                &intent.member_sig
            )
            .await?,
        "invalid renewal intent signature"
    );
    let Some((_, response)) = existing_response(node, intent, identity).await? else {
        return Ok(None);
    };
    let request =
        super::enrollment_candidate::request_enrollment(node, identity, &response.offer_token)
            .await?;
    Ok(Some(request))
}

/// Persist and deliver one response; existing signed responses are reused on retry.
pub(super) async fn respond_to_renewal_intent(
    node: &Arc<EmbeddedNode>,
    identity: &dyn NodeIdentity,
    store: &GraphqlEnrollmentStore,
    intent: &RenewalIntent,
) -> Result<()> {
    // This authenticates the intent and current transport before either issuing
    // or replaying a response. The approval owner checks them again later.
    let issued = store.issue_renewal_offer(intent).await?;
    let (doc_id, _response) = match existing_response(node, intent, identity).await? {
        Some(existing) => existing,
        None => {
            let f = |value: &str| escape_graphql_string(value);
            let sig = bs58::encode(&issued.admin_sig).into_string();
            let expires_at =
                gents_protocol::enrollment::decode_offer(&issued.offer_token)?.expires_at;
            let mutation = format!(
                r#"mutation {{ create_EnrollmentRenewalOffer(input: {{
                intent_id: "{}", member_did: "{}", offer_token: "{}", expires_at: "{}", admin_did: "{}", admin_sig: "{}"
            }}) {{ _docID }} }}"#,
                f(&issued.intent_id),
                f(&issued.member_did),
                f(&issued.offer_token),
                f(&expires_at),
                f(&issued.admin_did),
                f(&sig)
            );
            match ConfigAccess::write_local(node, "enrollment.create_renewal_offer", &mutation)
                .await
            {
                Ok(result) => {
                    let doc = gents_protocol::graphql::extract_mutation_doc_id(
                        &result,
                        "EnrollmentRenewalOffer",
                    )
                    .context("renewal offer mutation returned no document ID")?;
                    (doc, issued)
                }
                Err(error) => existing_response(node, intent, identity)
                    .await?
                    .with_context(|| format!("renewal offer write was not recovered: {error:#}"))?,
            }
        }
    };
    let projection = store.load_projection().await?;
    let active = projection.current_for_renewal_intent(intent, Utc::now())?;
    let p2p = crate::store::identity::node_p2p(node).context("runtime P2P is unavailable")?;
    push_enrollment_document(
        &p2p,
        &active.request.candidate_ticket,
        &intent.member_peer,
        &intent.member_did,
        "EnrollmentRenewalOffer",
        ("intent_id", &intent.intent_id()),
        &doc_id,
    )
    .await
}

#[cfg(test)]
mod cleanup_tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn cleanup_authority_matches_model() {
        let raw = gents_lean_contract::run_prepared_generator(
            &gents_lean_contract::proofs_dir().unwrap(),
            "Proofs.Conformance.RenewalCleanup:olean",
            "Proofs/Conformance/RenewalCleanup.lean",
        )
        .unwrap();
        let cases: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();
        assert_eq!(cases.len(), 12);
        for case in cases {
            let time = |name: &str| Utc.timestamp_opt(case[name].as_i64().unwrap(), 0).unwrap();
            assert_eq!(
                may_prune_renewal(
                    case["owned"].as_bool().unwrap(),
                    case["verified"].as_bool().unwrap(),
                    time("expires"),
                    time("now")
                ),
                case["allowed"].as_bool().unwrap(),
                "{case}"
            );
        }
    }

    #[tokio::test]
    async fn cleanup_removes_only_owned_verified_expired_offers() {
        use gents_protocol::enrollment::{
            encode_offer, EnrollmentOfferRecord, ENROLLMENT_PROTOCOL_VERSION,
        };
        let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let identity = crate::support::materialization_identity_for("did:test:renewal-cleanup");
        let now = Utc::now();
        for (id, expires, valid_signature, admin) in [
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
            let expires_at = expires.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let offer = EnrollmentOfferRecord {
                version: ENROLLMENT_PROTOCOL_VERSION,
                offer_id: id.into(),
                challenge: "challenge".into(),
                network_id: "network".into(),
                admin_did: admin.into(),
                server_peer: "peer".into(),
                server_ticket: "ticket".into(),
                owner_agent: "owner".into(),
                profile: "client".into(),
                schema_fingerprint: "fingerprint".into(),
                issued_at: (now - chrono::Duration::minutes(2))
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                expires_at: expires_at.clone(),
                admin_sig: vec![0; 64],
            };
            let mut response = RenewalOfferResponse {
                intent_id: id.into(),
                member_did: "member".into(),
                offer_token: encode_offer(&offer).unwrap(),
                admin_did: admin.into(),
                admin_sig: Vec::new(),
            };
            response.admin_sig = if valid_signature {
                identity.sign(&response.signing_payload()).await.unwrap()
            } else {
                vec![0; 64]
            };
            let f = |s: &str| escape_graphql_string(s);
            let query = format!(
                r#"mutation {{ create_EnrollmentRenewalOffer(input: {{ intent_id: "{}", member_did: "{}", offer_token: "{}", expires_at: "{}", admin_did: "{}", admin_sig: "{}" }}) {{ _docID }} }}"#,
                f(&response.intent_id),
                f(&response.member_did),
                f(&response.offer_token),
                f(&expires_at),
                f(&response.admin_did),
                f(&bs58::encode(response.admin_sig).into_string())
            );
            ConfigAccess::write_local(&node, "test.create_renewal_offer", &query)
                .await
                .unwrap();
        }
        prune_expired_offers(&node, identity.as_ref(), &mut None)
            .await
            .unwrap();
        let result = graphql_with_transaction_retry(
            &node,
            "{ EnrollmentRenewalOffer { intent_id } }",
            "test.remaining_renewal_offers",
        )
        .await
        .unwrap();
        let rows = rows::<serde_json::Value>(&result, "EnrollmentRenewalOffer").unwrap();
        let ids: std::collections::BTreeSet<_> = rows
            .iter()
            .map(|row| row["intent_id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            std::collections::BTreeSet::from(["foreign", "invalid", "live"])
        );
        node.shutdown().await;
    }
}
