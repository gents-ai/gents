//! Candidate side of signed enrollment: verify a server's signed offer,
//! countersign a request with this identity and deliver it to that server.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use defra_node::EmbeddedNode;
use defra_p2p_adapter::{P2POperations, P2pDocumentRequest, ReplicationFilter, TransportPeerId};
use gents_protocol::enrollment::{
    decode_offer, derive_enrollment_id, enrollment_schema_fingerprint, EnrollmentOfferRecord,
    EnrollmentRequestRecord, ENROLLMENT_PROTOCOL_VERSION,
};
use p2p::iroh::parse_public_peer_addr;
use serde::Deserialize;
use serde_json::{json, Map};
use tokio::time::timeout;
use uuid::Uuid;

use crate::store::config_client::ConfigAccess;
use crate::store::graphql::{escape_graphql_string, graphql_with_transaction_retry, rows};
use crate::store::identity::NodeIdentity;

use super::enrollment_store::RequestRow;

/// Bounds each transport operation during enrollment so an unavailable peer
/// cannot keep the request or its replicator cleanup waiting indefinitely.
pub const ENROLLMENT_P2P_TIMEOUT: Duration = Duration::from_secs(10);

const REQUEST_FIELDS: &str = "_docID protocol_version request_id request_digest offer_id offer_token challenge network_id admin_did server_peer candidate_did candidate_peer candidate_ticket owner_agent profile client_nonce issued_at expires_at candidate_sig";

/// Author (or reuse) this node's request for `offer_token` and deliver it to
/// the offering server, whose operator then decides it.
pub async fn request_enrollment(
    node: &Arc<EmbeddedNode>,
    identity: &dyn NodeIdentity,
    offer_token: &str,
) -> Result<EnrollmentRequestRecord> {
    let p2p = crate::store::identity::node_p2p(node).context("this node has no P2P transport")?;
    let offer = decode_offer(offer_token).context("decoding server enrollment offer")?;
    verify_enrollment_offer(&p2p, identity, &offer).await?;
    confirm_network_admin_pin(node, &offer).await?;
    let candidate_peer = p2p.local_peer_id().await.map_err(anyhow::Error::msg)?;
    let candidate_ticket = local_enrollment_ticket(&p2p, &candidate_peer).await?;
    let (request, document_id) = match persisted_request(
        node,
        identity,
        &offer,
        offer_token,
        &candidate_peer,
    )
    .await?
    {
        Some(existing) => existing,
        None => {
            let request = sign_enrollment_request(
                identity,
                &offer,
                offer_token,
                candidate_peer,
                candidate_ticket,
            )
            .await?;
            let document_id = match write_enrollment_request(node, &request).await {
                    Ok(document_id) => document_id,
                    Err(write_error) => {
                        persisted_request(node, identity, &offer, offer_token, &request.candidate_peer)
                            .await?
                            .filter(|(persisted, _)| persisted == &request)
                            .with_context(|| format!("enrollment request commit was not observably recovered after: {write_error:#}"))?
                            .1
                    }
                };
            (request, document_id)
        }
    };
    push_enrollment_request(&p2p, &offer, &request.request_id, &document_id).await?;
    Ok(request)
}

/// Accept only a fresh offer whose ticket, authenticated transport identity
/// and signature all name its admin; leaves the server connected.
pub async fn verify_enrollment_offer(
    p2p: &Arc<dyn P2POperations>,
    identity: &dyn NodeIdentity,
    offer: &EnrollmentOfferRecord,
) -> Result<()> {
    anyhow::ensure!(
        offer.schema_fingerprint == enrollment_schema_fingerprint(),
        "server enrollment schema {} is incompatible with {}",
        offer.schema_fingerprint,
        enrollment_schema_fingerprint()
    );
    anyhow::ensure!(offer.profile == "client", "unsupported enrollment profile");
    validate_fresh_window(&offer.issued_at, &offer.expires_at)?;
    let (ticket_peer, _) = parse_public_peer_addr(&offer.server_ticket)
        .context("server enrollment offer contains an invalid Iroh ticket")?;
    anyhow::ensure!(
        ticket_peer.to_string() == offer.server_peer,
        "server enrollment ticket does not match its signed peer ID"
    );
    timeout(
        ENROLLMENT_P2P_TIMEOUT,
        p2p.connect_peer(&offer.server_ticket),
    )
    .await
    .context("timed out connecting to enrollment server")?
    .map_err(anyhow::Error::msg)
    .context("connecting to enrollment server")?;
    let transport_peer = TransportPeerId::new(offer.server_peer.clone())
        .map_err(anyhow::Error::msg)
        .context("validating enrollment server peer ID")?;
    let resolved_server_did = timeout(
        ENROLLMENT_P2P_TIMEOUT,
        p2p.resolve_peer_identity(&transport_peer),
    )
    .await
    .context("timed out authenticating enrollment server identity")?
    .map_err(anyhow::Error::msg)?
    .context("enrollment server has no configured authenticated identity")?;
    validate_authenticated_server_did(&offer.admin_did, resolved_server_did.as_ref())?;
    anyhow::ensure!(
        identity
            .verify(&offer.admin_did, &offer.signing_payload(), &offer.admin_sig)
            .await?,
        "enrollment offer signature is invalid"
    );
    Ok(())
}

pub fn validate_authenticated_server_did(
    expected_admin_did: &str,
    resolved_did: &str,
) -> Result<()> {
    anyhow::ensure!(
        resolved_did == expected_admin_did,
        "authenticated server DID does not match the signed enrollment admin"
    );
    Ok(())
}

fn validate_fresh_window(issued_at: &str, expires_at: &str) -> Result<()> {
    let issued = DateTime::parse_from_rfc3339(issued_at).context("parsing offer issued_at")?;
    let expires = DateTime::parse_from_rfc3339(expires_at).context("parsing offer expires_at")?;
    let now = Utc::now();
    anyhow::ensure!(
        issued <= now + chrono::Duration::seconds(30),
        "enrollment offer is from the future"
    );
    anyhow::ensure!(expires > now, "enrollment offer has expired");
    anyhow::ensure!(
        issued <= expires,
        "enrollment offer expires before issuance"
    );
    anyhow::ensure!(
        expires - issued <= super::enrollment_offer::MAX_OFFER_VALIDITY,
        "enrollment offer validity window is too long"
    );
    Ok(())
}

pub async fn local_enrollment_ticket(
    p2p: &Arc<dyn P2POperations>,
    candidate_peer: &str,
) -> Result<String> {
    let ticket = timeout(ENROLLMENT_P2P_TIMEOUT, p2p.shareable_address())
        .await
        .context("timed out reading local enrollment ticket")?
        .map_err(anyhow::Error::msg)?
        .context("enrollment candidate has no shareable P2P address")?;
    let (ticket_peer, _) = parse_public_peer_addr(&ticket)
        .context("enrollment candidate produced an invalid shareable Iroh ticket")?;
    anyhow::ensure!(
        ticket_peer.to_string() == candidate_peer,
        "enrollment candidate shareable ticket does not match its local peer ID"
    );
    Ok(ticket)
}

pub async fn sign_enrollment_request(
    identity: &dyn NodeIdentity,
    offer: &EnrollmentOfferRecord,
    offer_token: &str,
    candidate_peer: String,
    candidate_ticket: String,
) -> Result<EnrollmentRequestRecord> {
    let client_nonce = Uuid::new_v4().simple().to_string();
    let request_id = format!(
        "enroll-{}",
        derive_enrollment_id(
            "gents-enrollment-request-id-v1",
            &[
                &offer.offer_id,
                identity.did(),
                &candidate_peer,
                &client_nonce
            ],
        )
    );
    let mut request = EnrollmentRequestRecord {
        protocol_version: ENROLLMENT_PROTOCOL_VERSION,
        request_id,
        request_digest: String::new(),
        offer_id: offer.offer_id.clone(),
        offer_token: offer_token.to_string(),
        challenge: offer.challenge.clone(),
        network_id: offer.network_id.clone(),
        admin_did: offer.admin_did.clone(),
        server_peer: offer.server_peer.clone(),
        candidate_did: identity.did().to_string(),
        candidate_peer,
        candidate_ticket,
        owner_agent: offer.owner_agent.clone(),
        profile: offer.profile.clone(),
        client_nonce,
        issued_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        expires_at: offer.expires_at.clone(),
        candidate_sig: Vec::new(),
    };
    request.request_digest = request.computed_digest();
    request.candidate_sig = identity.sign(&request.signing_payload()).await?;
    request
        .validate_against_offer(offer)
        .context("validating authored enrollment request")?;
    Ok(request)
}

#[derive(Deserialize)]
struct AdminPinRow {
    admin_did: String,
}

/// Pin the offer's admin as the only admin this candidate accepts for its
/// network; a conflicting existing pin refuses the offer.
pub async fn confirm_network_admin_pin(
    node: &Arc<EmbeddedNode>,
    offer: &EnrollmentOfferRecord,
) -> Result<()> {
    let (network_id, admin_did) = (&offer.network_id, &offer.admin_did);
    let network_id_escaped = escape_graphql_string(network_id);
    let query = format!(
        r#"{{ NetworkAdminPin(filter: {{ network_id: {{ _eq: "{network_id_escaped}" }} }}) {{ admin_did }} }}"#
    );
    let response =
        graphql_with_transaction_retry(node, &query, "loading local enrollment admin pin").await?;
    match rows::<AdminPinRow>(&response, "NetworkAdminPin")?.as_slice() {
        [pin] if &pin.admin_did == admin_did => return Ok(()),
        [pin] => anyhow::bail!(
            "network {network_id} is pinned to admin {}; refusing conflicting admin {admin_did}",
            pin.admin_did
        ),
        [] => {}
        pins => anyhow::bail!(
            "network {network_id} has {} local admin pins; refusing enrollment",
            pins.len()
        ),
    }
    let pin_key = format!(
        "pin-{}",
        derive_enrollment_id("gents-network-admin-pin-v1", &[network_id])
    );
    let mutation = format!(
        r#"mutation {{
            create_NetworkAdminPin(input: {{
                pin_key: "{}",
                network_id: "{network_id_escaped}",
                admin_did: "{}",
                offer_id: "{}",
                confirmed_at: "{}"
            }}) {{ _docID }}
        }}"#,
        escape_graphql_string(&pin_key),
        escape_graphql_string(admin_did),
        escape_graphql_string(&offer.offer_id),
        Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
    );
    if let Err(commit_error) =
        ConfigAccess::write_local(node, "enrollment.create_admin_pin", &mutation).await
    {
        let response =
            graphql_with_transaction_retry(node, &query, "recovering local enrollment admin pin")
                .await?;
        anyhow::ensure!(
            matches!(rows::<AdminPinRow>(&response, "NetworkAdminPin")?.as_slice(), [pin] if &pin.admin_did == admin_did),
            "admin pin commit was not observably recovered after: {commit_error:#}"
        );
    }
    Ok(())
}

/// The single request this identity and peer persisted for `offer`, after
/// re-verifying it; a second request would share the offer's challenge.
async fn persisted_request(
    node: &Arc<EmbeddedNode>,
    identity: &dyn NodeIdentity,
    offer: &EnrollmentOfferRecord,
    offer_token: &str,
    candidate_peer: &str,
) -> Result<Option<(EnrollmentRequestRecord, String)>> {
    let query = format!(
        r#"{{ NetworkEnrollmentRequest(filter: {{ offer_id: {{ _eq: "{}" }}, candidate_did: {{ _eq: "{}" }}, candidate_peer: {{ _eq: "{}" }} }}) {{ {REQUEST_FIELDS} }} }}"#,
        escape_graphql_string(&offer.offer_id),
        escape_graphql_string(identity.did()),
        escape_graphql_string(candidate_peer),
    );
    let response =
        graphql_with_transaction_retry(node, &query, "loading retryable enrollment request")
            .await?;
    let found = rows::<RequestRow>(&response, "NetworkEnrollmentRequest")?;
    let row = match found.as_slice() {
        [] => return Ok(None),
        [row] => row,
        _ => anyhow::bail!(
            "offer {} has multiple local enrollment requests",
            offer.offer_id
        ),
    };
    let request = row.to_record()?;
    anyhow::ensure!(
        request.offer_token == offer_token,
        "persisted enrollment request embeds a different signed offer"
    );
    request.validate_against_offer(offer)?;
    anyhow::ensure!(
        identity
            .verify(
                &request.candidate_did,
                &request.signing_payload(),
                &request.candidate_sig
            )
            .await?,
        "persisted enrollment request signature is invalid"
    );
    Ok(Some((request, row.doc_id.clone())))
}

pub async fn write_enrollment_request(
    node: &Arc<EmbeddedNode>,
    request: &EnrollmentRequestRecord,
) -> Result<String> {
    let input = enrollment_request_input(request);
    let mutation =
        format!("mutation {{ create_NetworkEnrollmentRequest(input: {input}) {{ _docID }} }}");
    let response = ConfigAccess::write_local(node, "enrollment.create_request", &mutation).await?;
    gents_protocol::graphql::extract_mutation_doc_id(&response, "NetworkEnrollmentRequest")
        .context("enrollment request mutation returned no document ID")
}

pub fn enrollment_request_input(request: &EnrollmentRequestRecord) -> String {
    let field = |value: &str| escape_graphql_string(value);
    let candidate_sig = bs58::encode(&request.candidate_sig).into_string();
    format!(
        r#"{{
            protocol_version: {},
            request_id: "{}",
            request_digest: "{}",
            offer_id: "{}",
            offer_token: "{}",
            challenge: "{}",
            network_id: "{}",
            admin_did: "{}",
            server_peer: "{}",
            candidate_did: "{}",
            candidate_peer: "{}",
            candidate_ticket: "{}",
            owner_agent: "{}",
            profile: "{}",
            client_nonce: "{}",
            issued_at: "{}",
            expires_at: "{}",
            candidate_sig: "{}"
        }}"#,
        request.protocol_version,
        field(&request.request_id),
        field(&request.request_digest),
        field(&request.offer_id),
        field(&request.offer_token),
        field(&request.challenge),
        field(&request.network_id),
        field(&request.admin_did),
        field(&request.server_peer),
        field(&request.candidate_did),
        field(&request.candidate_peer),
        field(&request.candidate_ticket),
        field(&request.owner_agent),
        field(&request.profile),
        field(&request.client_nonce),
        field(&request.issued_at),
        field(&request.expires_at),
        field(&candidate_sig),
    )
}

pub async fn push_enrollment_request(
    p2p: &Arc<dyn P2POperations>,
    offer: &EnrollmentOfferRecord,
    request_id: &str,
    document_id: &str,
) -> Result<()> {
    const COLLECTION: &str = "NetworkEnrollmentRequest";

    // Before approval there is no authority-backed data route, so install the
    // smallest possible bootstrap route (one authenticated server, one
    // immutable enrollment request) and remove it before this operation
    // returns; the signed enrollment reconciler owns every durable route
    // after approval.
    let mut conditions = Map::new();
    conditions.insert("request_id".to_string(), json!({ "_eq": request_id }));
    let filters = BTreeMap::from([(
        COLLECTION.to_string(),
        ReplicationFilter::predicate(conditions),
    )]);
    let collections = vec![COLLECTION.to_string()];
    let install = timeout(
        ENROLLMENT_P2P_TIMEOUT,
        p2p.add_replicator(
            collections.clone(),
            Some(&offer.server_ticket),
            filters,
            Vec::new(),
            Some(&offer.admin_did),
        ),
    )
    .await;
    let install_error = match install {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(
            anyhow::Error::msg(error)
                .context("installing authenticated enrollment bootstrap route"),
        ),
        Err(_) => Some(anyhow::anyhow!(
            "timed out installing authenticated enrollment bootstrap route"
        )),
    };

    let delivery_error = if install_error.is_none() {
        match timeout(
            ENROLLMENT_P2P_TIMEOUT,
            p2p.push_documents_to_peer(
                &offer.server_peer,
                vec![P2pDocumentRequest {
                    collection: COLLECTION.to_string(),
                    doc_id: document_id.to_string(),
                }],
            ),
        )
        .await
        {
            Ok(Ok(())) => None,
            Ok(Err(error)) => {
                Some(anyhow::Error::msg(error).context("pushing enrollment request to server"))
            }
            Err(_) => Some(anyhow::anyhow!(
                "timed out pushing enrollment request to server"
            )),
        }
    } else {
        None
    };

    let cleanup_error = match timeout(
        ENROLLMENT_P2P_TIMEOUT,
        p2p.remove_replicator(collections, Some(&offer.server_ticket)),
    )
    .await
    {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(
            anyhow::Error::msg(error).context("removing authenticated enrollment bootstrap route"),
        ),
        Err(_) => Some(anyhow::anyhow!(
            "timed out removing authenticated enrollment bootstrap route"
        )),
    };

    match (install_error.or(delivery_error), cleanup_error) {
        (None, None) => Ok(()),
        (Some(operation), None) => Err(operation),
        (None, Some(cleanup)) => Err(cleanup),
        (Some(operation), Some(cleanup)) => Err(operation.context(format!(
            "enrollment bootstrap route cleanup also failed: {cleanup:#}"
        ))),
    }
}
