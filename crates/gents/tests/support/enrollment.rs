use std::sync::Arc;
use std::time::{Duration, Instant};

use gents::agent::p2p_reconcile::GraphqlEnrollmentStore;
use gents::defra_node::EmbeddedNode;
use gents::graphql::{
    escape_graphql_string,
    graphql_response_with_transaction_retry as execute_graphql_with_conflict_retry,
};
use gents::AgentIdentity;
use gents_protocol::enrollment::{
    derive_enrollment_id, encode_offer, enrollment_schema_fingerprint, EnrollmentDecisionKind,
    EnrollmentOfferRecord, EnrollmentRequestRecord, ENROLLMENT_PROTOCOL_VERSION,
};
use gents_protocol::network_token::NetworkRecord;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedEnrollment {
    pub request_id: String,
    pub request_digest: String,
}

fn bs58_sig(signature: &[u8]) -> String {
    bs58::encode(signature).into_string()
}

pub async fn wait_for_peer_identity(node: &EmbeddedNode) -> (String, String) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let p2p = node.p2p().expect("p2p enabled");
        let peer_id = p2p.local_peer_id().await.ok();
        let shareable = p2p.shareable_address().await.ok().flatten();
        if let (Some(peer_id), Some(address)) = (peer_id, shareable) {
            if !peer_id.trim().is_empty() && !address.trim().is_empty() {
                return (peer_id, address);
            }
        }
        if let Ok(addresses) = p2p.listen_addresses().await {
            if let Some(address) = addresses.first() {
                if let Some(peer_id) = address.rsplit("/p2p/").nth(1) {
                    if !peer_id.is_empty() {
                        return (peer_id.to_string(), address.clone());
                    }
                }
            }
        }
        if Instant::now() >= deadline {
            panic!("node never exposed a P2P peer identity");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Author and approve one exact signed enrollment generation.
///
/// P2P integration fixtures use this production document path instead of
/// writing the enrollment-owned `PeerPairingDesired` materialization directly.
pub async fn authorize_enrollment_peer(
    node: Arc<EmbeddedNode>,
    network_id: &str,
    network_name: &str,
    admin_identity: Arc<dyn AgentIdentity>,
    member_identity: Arc<dyn AgentIdentity>,
    member_peer: &str,
    member_address: &str,
) -> AuthorizedEnrollment {
    let issued = chrono::Utc::now();
    let now = issued.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let expires_at =
        (issued + chrono::Duration::minutes(5)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let mut network = NetworkRecord {
        network_id: network_id.to_string(),
        admin_did: admin_identity.did().to_string(),
        display_name: network_name.to_string(),
        default_template: "conversation".to_string(),
        created_at: now.clone(),
        sig: Vec::new(),
    };
    network.sig = admin_identity
        .sign(&network.signing_payload())
        .await
        .expect("sign AgentNetwork");

    let network_id_gql = escape_graphql_string(&network.network_id);
    let admin_did = escape_graphql_string(&network.admin_did);
    let display_name = escape_graphql_string(&network.display_name);
    let default_template = escape_graphql_string(&network.default_template);
    let created_at = escape_graphql_string(&network.created_at);
    let admin_sig = escape_graphql_string(&bs58_sig(&network.sig));
    let network_mutation = format!(
        r#"mutation {{
            upsert_AgentNetwork(
                filter: {{ network_id: {{ _eq: "{network_id_gql}" }} }},
                add: {{
                    network_id: "{network_id_gql}",
                    admin_did: "{admin_did}",
                    display_name: "{display_name}",
                    default_template: "{default_template}",
                    created_at: "{created_at}",
                    admin_sig: "{admin_sig}"
                }},
                update: {{
                    admin_did: "{admin_did}",
                    display_name: "{display_name}",
                    default_template: "{default_template}",
                    created_at: "{created_at}",
                    admin_sig: "{admin_sig}"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = execute_graphql_with_conflict_retry(
        node.as_ref(),
        &network_mutation,
        "seed enrollment AgentNetwork",
    )
    .await;
    assert!(
        !response.has_errors(),
        "upsert AgentNetwork failed: {:?}",
        response.errors
    );

    let p2p = node.p2p().expect("p2p enabled");
    let server_peer = p2p.local_peer_id().await.expect("server peer id");
    let server_ticket = p2p
        .shareable_address()
        .await
        .expect("server ticket lookup")
        .expect("server shareable ticket");
    let challenge = format!("challenge-{network_id}-{member_peer}");
    let offer_id = format!(
        "offer-{}",
        derive_enrollment_id(
            "gents-enrollment-offer-v1",
            &[
                network_id,
                admin_identity.did(),
                &server_peer,
                &challenge,
                &now,
            ],
        )
    );
    let mut offer = EnrollmentOfferRecord {
        version: ENROLLMENT_PROTOCOL_VERSION,
        offer_id,
        challenge,
        network_id: network_id.to_string(),
        admin_did: admin_identity.did().to_string(),
        server_peer,
        server_ticket,
        owner_agent: admin_identity.did().to_string(),
        profile: "client".to_string(),
        schema_fingerprint: enrollment_schema_fingerprint(),
        issued_at: now.clone(),
        expires_at: expires_at.clone(),
        admin_sig: Vec::new(),
    };
    offer.admin_sig = admin_identity
        .sign(&offer.signing_payload())
        .await
        .expect("sign enrollment offer");
    let offer_token = encode_offer(&offer).expect("encode enrollment offer");
    let client_nonce = format!("nonce-{network_id}-{member_peer}");
    let request_id = format!(
        "enroll-{}",
        derive_enrollment_id(
            "gents-enrollment-request-id-v1",
            &[
                &offer.offer_id,
                member_identity.did(),
                member_peer,
                &client_nonce,
            ],
        )
    );
    let mut request = EnrollmentRequestRecord {
        protocol_version: ENROLLMENT_PROTOCOL_VERSION,
        request_id,
        request_digest: String::new(),
        offer_id: offer.offer_id.clone(),
        offer_token,
        challenge: offer.challenge.clone(),
        network_id: network_id.to_string(),
        admin_did: admin_identity.did().to_string(),
        server_peer: offer.server_peer.clone(),
        candidate_did: member_identity.did().to_string(),
        candidate_peer: member_peer.to_string(),
        candidate_ticket: member_address.to_string(),
        owner_agent: offer.owner_agent.clone(),
        profile: offer.profile.clone(),
        client_nonce,
        issued_at: now,
        expires_at,
        candidate_sig: Vec::new(),
    };
    request.request_digest = request.computed_digest();
    request.candidate_sig = member_identity
        .sign(&request.signing_payload())
        .await
        .expect("sign enrollment request");
    request
        .validate_against_offer(&offer)
        .expect("validate enrollment request");

    let field = |value: &str| escape_graphql_string(value);
    let candidate_sig = field(&bs58_sig(&request.candidate_sig));
    let request_mutation = format!(
        r#"mutation {{ create_NetworkEnrollmentRequest(input: {{
            protocol_version: {}, request_id: "{}", request_digest: "{}",
            offer_id: "{}", offer_token: "{}", challenge: "{}",
            network_id: "{}", admin_did: "{}", server_peer: "{}",
            candidate_did: "{}", candidate_peer: "{}", candidate_ticket: "{}",
            owner_agent: "{}", profile: "{}", client_nonce: "{}",
            issued_at: "{}", expires_at: "{}", candidate_sig: "{}"
        }}) {{ _docID }} }}"#,
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
        candidate_sig,
    );
    let response = execute_graphql_with_conflict_retry(
        node.as_ref(),
        &request_mutation,
        "seed enrollment request",
    )
    .await;
    assert!(
        !response.has_errors(),
        "create enrollment request failed: {:?}",
        response.errors
    );
    GraphqlEnrollmentStore::new(node, admin_identity)
        .decide_request(&request.request_id, EnrollmentDecisionKind::Approved)
        .await
        .expect("approve live enrollment request");
    AuthorizedEnrollment {
        request_id: request.request_id,
        request_digest: request.request_digest,
    }
}
