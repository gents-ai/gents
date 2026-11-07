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

async fn existing_response(
    node: &Arc<EmbeddedNode>,
    intent: &RenewalIntent,
    identity: &dyn NodeIdentity,
) -> Result<Option<(String, RenewalOfferResponse)>> {
    let id = escape_graphql_string(&intent.intent_id());
    let query = format!(
        r#"{{ EnrollmentRenewalOffer(filter: {{ intent_id: {{ _eq: "{id}" }} }}) {{ _docID intent_id member_did offer_token admin_did admin_sig }} }}"#
    );
    let response =
        graphql_with_transaction_retry(node, &query, "read renewal offer response").await?;
    let found = rows::<OfferRow>(&response, "EnrollmentRenewalOffer")?;
    let [row] = found.as_slice() else {
        anyhow::ensure!(found.is_empty(), "conflicting renewal offer responses");
        return Ok(None);
    };
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
            let mutation = format!(
                r#"mutation {{ create_EnrollmentRenewalOffer(input: {{
                intent_id: "{}", member_did: "{}", offer_token: "{}", admin_did: "{}", admin_sig: "{}"
            }}) {{ _docID }} }}"#,
                f(&issued.intent_id),
                f(&issued.member_did),
                f(&issued.offer_token),
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
