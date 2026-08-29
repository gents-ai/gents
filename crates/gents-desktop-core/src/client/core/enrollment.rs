use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use defra_p2p_adapter::{P2PError, P2pDocumentRequest, TransportPeerId};
use gents::graphql::{ensure_no_errors, escape_graphql_string, rows};
use gents::AgentIdentity;
use gents_protocol::enrollment::{
    decode_offer, derive_enrollment_id, enrollment_schema_fingerprint, EnrollmentRequestRecord,
    ENROLLMENT_PROTOCOL_VERSION,
};
use p2p::iroh::parse_public_peer_addr;
use serde::Deserialize;
use tokio::time::timeout;
use uuid::Uuid;

use super::{ClientCore, P2P_OPERATION_TIMEOUT};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollmentRequestResult {
    pub request_id: String,
    pub network_id: String,
    pub admin_did: String,
    pub server_peer: String,
    pub owner_agent: String,
    pub state: String,
}

#[derive(Deserialize)]
struct AdminPinRow {
    admin_did: String,
}

impl ClientCore {
    pub async fn request_status_enrollment(
        &self,
        offer_token: &str,
    ) -> Result<EnrollmentRequestResult> {
        let offer = decode_offer(offer_token).context("decoding server enrollment offer")?;
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
            P2P_OPERATION_TIMEOUT,
            self.p2p.connect_peer(&offer.server_ticket),
        )
        .await
        .context("timed out connecting to enrollment server")?
        .map_err(map_p2p_error)
        .context("connecting to enrollment server")?;

        let transport_peer = TransportPeerId::new(offer.server_peer.clone())
            .map_err(map_p2p_error)
            .context("validating enrollment server peer ID")?;
        let resolved_server_did = timeout(
            P2P_OPERATION_TIMEOUT,
            self.p2p.resolve_peer_identity(&transport_peer),
        )
        .await
        .context("timed out authenticating enrollment server identity")?
        .map_err(map_p2p_error)?
        .context("enrollment server has no configured authenticated identity")?;
        anyhow::ensure!(
            resolved_server_did.to_string() == offer.admin_did,
            "authenticated server DID does not match the signed enrollment admin"
        );
        anyhow::ensure!(
            self.principal
                .verify(&offer.admin_did, &offer.signing_payload(), &offer.admin_sig)
                .await?,
            "enrollment offer signature is invalid"
        );

        self.confirm_admin_pin(&offer.network_id, &offer.admin_did, &offer.offer_id)
            .await?;

        let candidate_peer = self.local_peer_id.clone();
        let candidate_ticket = timeout(P2P_OPERATION_TIMEOUT, self.p2p.shareable_address())
            .await
            .context("timed out reading local enrollment ticket")?
            .map_err(map_p2p_error)?
            .context("desktop client has no shareable P2P address")?;
        let (ticket_candidate_peer, _) = parse_public_peer_addr(&candidate_ticket)
            .context("desktop client produced an invalid shareable Iroh ticket")?;
        anyhow::ensure!(
            ticket_candidate_peer.to_string() == candidate_peer,
            "desktop shareable ticket does not match its local peer ID"
        );

        let issued_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let client_nonce = Uuid::new_v4().simple().to_string();
        let request_id = format!(
            "enroll-{}",
            derive_enrollment_id(
                "gents-enrollment-request-id-v1",
                &[
                    &offer.offer_id,
                    self.principal.did(),
                    &candidate_peer,
                    &client_nonce,
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
            candidate_did: self.principal.did().to_string(),
            candidate_peer,
            candidate_ticket,
            owner_agent: offer.owner_agent.clone(),
            profile: offer.profile.clone(),
            client_nonce,
            issued_at,
            expires_at: offer.expires_at.clone(),
            candidate_sig: Vec::new(),
        };
        request.request_digest = request.computed_digest();
        request.candidate_sig = self.principal.sign(&request.signing_payload())?;
        request
            .validate_against_offer(&offer)
            .context("validating authored enrollment request")?;
        let document_id = self.write_enrollment_request(&request).await?;
        timeout(
            P2P_OPERATION_TIMEOUT,
            self.p2p.push_documents_to_peer(
                &offer.server_peer,
                vec![P2pDocumentRequest {
                    collection: "NetworkEnrollmentRequest".to_string(),
                    doc_id: document_id,
                }],
            ),
        )
        .await
        .context("timed out pushing enrollment request to server")?
        .map_err(map_p2p_error)
        .context("pushing enrollment request to server")?;

        Ok(EnrollmentRequestResult {
            request_id: request.request_id,
            network_id: request.network_id,
            admin_did: request.admin_did,
            server_peer: request.server_peer,
            owner_agent: request.owner_agent,
            state: "pending_approval".to_string(),
        })
    }

    async fn confirm_admin_pin(
        &self,
        network_id: &str,
        admin_did: &str,
        offer_id: &str,
    ) -> Result<()> {
        let network_id_escaped = escape_graphql_string(network_id);
        let query = format!(
            r#"{{ NetworkAdminPin(filter: {{ network_id: {{ _eq: "{network_id_escaped}" }} }}) {{ admin_did }} }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "loading local enrollment admin pin")?;
        let pins = rows::<AdminPinRow>(&response, "NetworkAdminPin")?;
        match pins.as_slice() {
            [pin] if pin.admin_did == admin_did => return Ok(()),
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
        let pin_key = escape_graphql_string(&pin_key);
        let admin_did = escape_graphql_string(admin_did);
        let offer_id = escape_graphql_string(offer_id);
        let confirmed_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let mutation = format!(
            r#"mutation {{
                create_NetworkAdminPin(input: {{
                    pin_key: "{pin_key}",
                    network_id: "{network_id_escaped}",
                    admin_did: "{admin_did}",
                    offer_id: "{offer_id}",
                    confirmed_at: "{confirmed_at}"
                }}) {{ _docID }}
            }}"#
        );
        gents::graphql::graphql_mutation_with_transaction_retry(
            self.node.as_ref(),
            &mutation,
            "create_network_admin_pin",
        )
        .await?;
        Ok(())
    }

    async fn write_enrollment_request(&self, request: &EnrollmentRequestRecord) -> Result<String> {
        let input = enrollment_request_input(request);
        let mutation =
            format!("mutation {{ create_NetworkEnrollmentRequest(input: {input}) {{ _docID }} }}");
        let response = gents::graphql::graphql_mutation_with_transaction_retry(
            self.node.as_ref(),
            &mutation,
            "create_network_enrollment_request",
        )
        .await?;
        let response = serde_json::json!({ "data": response.data.unwrap_or_default() });
        gents_protocol::graphql::extract_mutation_doc_id(&response, "NetworkEnrollmentRequest")
            .context("enrollment request mutation returned no document ID")
    }
}

fn enrollment_request_input(request: &EnrollmentRequestRecord) -> String {
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
        expires - issued <= chrono::Duration::minutes(10),
        "enrollment offer validity window is too long"
    );
    Ok(())
}

fn map_p2p_error(error: P2PError) -> anyhow::Error {
    anyhow::anyhow!(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_mutation_escapes_every_string_and_never_emits_an_empty_array() {
        let request = EnrollmentRequestRecord {
            protocol_version: ENROLLMENT_PROTOCOL_VERSION,
            request_id: "req-\"unsafe".into(),
            request_digest: "digest".into(),
            offer_id: "offer".into(),
            offer_token: "token".into(),
            challenge: "challenge".into(),
            network_id: "network".into(),
            admin_did: "did:key:admin".into(),
            server_peer: "server".into(),
            candidate_did: "did:key:candidate".into(),
            candidate_peer: "candidate".into(),
            candidate_ticket: "ticket".into(),
            owner_agent: "did:key:agent".into(),
            profile: "client".into(),
            client_nonce: "nonce".into(),
            issued_at: "2026-08-29T00:00:00Z".into(),
            expires_at: "2026-08-29T00:05:00Z".into(),
            candidate_sig: vec![1, 2, 3],
        };
        let input = enrollment_request_input(&request);
        assert!(input.contains(r#"request_id: "req-\"unsafe""#));
        assert!(!input.contains("[]"));
    }
}
