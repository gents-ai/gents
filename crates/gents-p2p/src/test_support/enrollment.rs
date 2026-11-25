use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use defra_node::Node as EmbeddedNode;
use gents_protocol::enrollment::{
    derive_enrollment_id, encode_offer, enrollment_schema_fingerprint, AuthorizationRevisionKind,
    AuthorizationRevisionRecord, EnrollmentDecisionKind, EnrollmentDecisionRecord,
    EnrollmentOfferRecord, EnrollmentRequestRecord, DEFAULT_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS,
    ENROLLMENT_PROTOCOL_VERSION,
};
use gents_protocol::network_token::NetworkRecord;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;

use crate::p2p_reconcile::enrollment_reconcile::{
    EnrollmentAuthorityOwner, EnrollmentProjectionReader,
};
use crate::p2p_reconcile::enrollment_store::{
    ActiveEnrollment, EnrollmentProjection, RevokedEnrollment,
};
use crate::p2p_reconcile::GraphqlEnrollmentStore;

pub enum ScriptedProjection {
    Projection(Box<EnrollmentProjection>),
    Error(&'static str),
}

pub struct ScriptedProjectionReader {
    pub steps: Mutex<VecDeque<ScriptedProjection>>,
}

#[async_trait]
impl EnrollmentProjectionReader for ScriptedProjectionReader {
    async fn load_authority_projection(&self) -> Result<EnrollmentProjection> {
        match self
            .steps
            .lock()
            .expect("scripted projection lock")
            .pop_front()
            .expect("scripted projection step")
        {
            ScriptedProjection::Projection(projection) => Ok(*projection),
            ScriptedProjection::Error(message) => anyhow::bail!(message),
        }
    }
}

pub fn active_enrollment(sequence: u64, expires_at: &str) -> ActiveEnrollment {
    let request_id = format!("enrollment-request-{sequence}");
    let request_digest = format!("enrollment-digest-{sequence}");
    let request = EnrollmentRequestRecord {
        protocol_version: ENROLLMENT_PROTOCOL_VERSION,
        request_id: request_id.clone(),
        request_digest: request_digest.clone(),
        offer_id: format!("offer-{sequence}"),
        offer_token: format!("token-{sequence}"),
        challenge: format!("challenge-{sequence}"),
        network_id: "network-1".into(),
        admin_did: "did:key:admin".into(),
        server_peer: "server-peer".into(),
        candidate_did: "did:key:member-placeholder".into(),
        candidate_peer: "member-peer".into(),
        candidate_name: None,
        candidate_ticket: "member-ticket".into(),
        owner_node_did: "did:key:owner-placeholder".into(),
        profile: "client".into(),
        client_nonce: format!("nonce-{sequence}"),
        issued_at: "2030-01-01T00:00:00Z".into(),
        expires_at: "2031-01-01T00:00:00Z".into(),
        candidate_sig: vec![1; 64],
    };
    let decision = EnrollmentDecisionRecord {
        protocol_version: ENROLLMENT_PROTOCOL_VERSION,
        decision_id: format!("decision-{sequence}"),
        request_id: request_id.clone(),
        request_digest: request_digest.clone(),
        network_id: request.network_id.clone(),
        admin_did: request.admin_did.clone(),
        candidate_did: request.candidate_did.clone(),
        candidate_peer: request.candidate_peer.clone(),
        owner_node_did: request.owner_node_did.clone(),
        decision: EnrollmentDecisionKind::Approved,
        authorization_sequence: sequence,
        authorization_expires_at: expires_at.into(),
        decided_at: "2030-01-01T00:00:01Z".into(),
        signer_did: request.admin_did.clone(),
        admin_sig: vec![2; 64],
    };
    let revision = AuthorizationRevisionRecord {
        protocol_version: ENROLLMENT_PROTOCOL_VERSION,
        revision_id: format!("revision-{sequence}"),
        request_id,
        request_digest,
        network_id: request.network_id.clone(),
        admin_did: request.admin_did.clone(),
        member_did: request.candidate_did.clone(),
        member_peer: request.candidate_peer.clone(),
        owner_node_did: request.owner_node_did.clone(),
        sequence,
        authorization_expires_at: expires_at.into(),
        kind: AuthorizationRevisionKind::Active,
        issued_at: "2030-01-01T00:00:01Z".into(),
        signer_did: request.admin_did.clone(),
        admin_sig: vec![3; 64],
    };
    ActiveEnrollment {
        request_doc_id: format!("request-doc-{sequence}"),
        decision_doc_id: format!("decision-doc-{sequence}"),
        revision_doc_id: format!("revision-doc-{sequence}"),
        route_receipt_doc_id: None,
        request,
        decision,
        revision,
        route_receipt: None,
    }
}

pub fn projection_with_active(active: ActiveEnrollment) -> EnrollmentProjection {
    EnrollmentProjection {
        network_id: Some(active.request.network_id.clone()),
        active: vec![active],
        ..EnrollmentProjection::default()
    }
}

pub fn projection_with_revoked(active: &ActiveEnrollment) -> EnrollmentProjection {
    let mut revision = active.revision.clone();
    revision.kind = AuthorizationRevisionKind::Revoked;
    EnrollmentProjection {
        network_id: Some(active.request.network_id.clone()),
        revoked: vec![RevokedEnrollment {
            request_doc_id: active.request_doc_id.clone(),
            decision_doc_id: active.decision_doc_id.clone(),
            revision_doc_id: format!("revoked-{}", active.revision.sequence + 1),
            request: active.request.clone(),
            decision: active.decision.clone(),
            revision,
        }],
        ..EnrollmentProjection::default()
    }
}

/// Serves one authority command per scripted step, then ends.
pub fn spawn_scripted_authority(
    mut owner: EnrollmentAuthorityOwner,
    steps: Vec<ScriptedProjection>,
) -> tokio::task::JoinHandle<()> {
    let count = steps.len();
    let script = ScriptedProjectionReader {
        steps: Mutex::new(VecDeque::from(steps)),
    };
    tokio::spawn(async move {
        for _ in 0..count {
            owner.serve_next_command(&script).await;
        }
    })
}

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
        let p2p = crate::node_access::operations(node).expect("p2p enabled");
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

/// Upsert the admin's signed Network on `node` and sign a fresh offer from it.
pub async fn signed_enrollment_offer(
    node: &Arc<EmbeddedNode>,
    network_id: &str,
    network_name: &str,
    admin_identity: &dyn NodeIdentity,
    challenge: String,
) -> (EnrollmentOfferRecord, String) {
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
        .expect("sign Network");

    let network_id_gql = escape_graphql_string(&network.network_id);
    let admin_did = escape_graphql_string(&network.admin_did);
    let display_name = escape_graphql_string(&network.display_name);
    let default_template = escape_graphql_string(&network.default_template);
    let created_at = escape_graphql_string(&network.created_at);
    let admin_sig = escape_graphql_string(&bs58_sig(&network.sig));
    let network_mutation = format!(
        r#"mutation {{
            upsert_Network(
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
    let response = gents_store::config_client::ConfigAccess::write_local_response(
        node,
        "test.signed_enrollment_offer.network",
        &network_mutation,
    )
    .await
    .expect("write signed enrollment network");
    assert!(
        !response.has_errors(),
        "upsert Network failed: {:?}",
        response.errors
    );

    let p2p = crate::node_access::operations(node).expect("p2p enabled");
    let server_peer = p2p.local_peer_id().await.expect("server peer id");
    let server_ticket = p2p
        .shareable_address()
        .await
        .expect("server ticket lookup")
        .expect("server shareable ticket");
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
        owner_node_did: admin_identity.did().to_string(),
        profile: "client".to_string(),
        schema_fingerprint: enrollment_schema_fingerprint(),
        live_schema_digest: gents_protocol::peer_schema::replicated_schema_digest(
            crate::p2p_reconcile::CLIENT_COLLECTIONS,
            &crate::p2p_reconcile::read_client_replicated_schema(node.clone())
                .await
                .unwrap(),
        )
        .unwrap(),
        issued_at: now.clone(),
        expires_at: expires_at.clone(),
        admin_sig: Vec::new(),
    };
    offer.admin_sig = admin_identity
        .sign(&offer.signing_payload())
        .await
        .expect("sign enrollment offer");
    let offer_token = encode_offer(&offer).expect("encode enrollment offer");
    (offer, offer_token)
}

pub async fn authorize_enrollment_peer(
    node: Arc<EmbeddedNode>,
    network_id: &str,
    network_name: &str,
    admin_identity: Arc<dyn NodeIdentity>,
    member_identity: Arc<dyn NodeIdentity>,
    member_peer: &str,
    member_address: &str,
) -> AuthorizedEnrollment {
    authorize_enrollment_generation(
        node,
        EnrollmentGeneration {
            network_id,
            network_name,
            label: "",
            lease: Duration::from_secs(DEFAULT_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS),
        },
        admin_identity,
        member_identity,
        member_peer,
        member_address,
    )
    .await
}

/// One offer, request and approval for a member. A distinct `label` makes a
/// fresh request from the same member, which is how a renewal re-enrolls it.
pub struct EnrollmentGeneration<'a> {
    pub network_id: &'a str,
    pub network_name: &'a str,
    pub label: &'a str,
    pub lease: Duration,
}

pub async fn authorize_enrollment_generation(
    node: Arc<EmbeddedNode>,
    generation: EnrollmentGeneration<'_>,
    admin_identity: Arc<dyn NodeIdentity>,
    member_identity: Arc<dyn NodeIdentity>,
    member_peer: &str,
    member_address: &str,
) -> AuthorizedEnrollment {
    let EnrollmentGeneration {
        network_id,
        network_name,
        label,
        lease,
    } = generation;
    let (offer, offer_token) = signed_enrollment_offer(
        &node,
        network_id,
        network_name,
        admin_identity.as_ref(),
        format!("challenge-{network_id}-{member_peer}{label}"),
    )
    .await;
    let client_nonce = format!("nonce-{network_id}-{member_peer}{label}");
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
        candidate_name: None,
        candidate_ticket: member_address.to_string(),
        owner_node_did: offer.owner_node_did.clone(),
        profile: offer.profile.clone(),
        client_nonce,
        issued_at: offer.issued_at.clone(),
        expires_at: offer.expires_at.clone(),
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
            owner_node_did: "{}", profile: "{}", client_nonce: "{}",
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
        field(&request.owner_node_did),
        field(&request.profile),
        field(&request.client_nonce),
        field(&request.issued_at),
        field(&request.expires_at),
        candidate_sig,
    );
    let response = gents_store::config_client::ConfigAccess::write_local_response(
        &node,
        "test.authorize_enrollment_generation.request",
        &request_mutation,
    )
    .await
    .expect("write signed enrollment request");
    assert!(
        !response.has_errors(),
        "create enrollment request failed: {:?}",
        response.errors
    );
    GraphqlEnrollmentStore::new(node, admin_identity)
        .decide_request_with_lease(&request.request_id, EnrollmentDecisionKind::Approved, lease)
        .await
        .expect("approve live enrollment request");
    AuthorizedEnrollment {
        request_id: request.request_id,
        request_digest: request.request_digest,
    }
}
