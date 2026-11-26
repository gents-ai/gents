use anyhow::{Context, Result};
use gents_protocol::enrollment::{
    AuthorizationRevisionKind as WireRevisionKind, AuthorizationRevisionRecord,
    EnrollmentDecisionKind as WireDecisionKind, EnrollmentDecisionRecord, EnrollmentRequestRecord,
    EnrollmentRouteReceiptDirection as WireReceiptDirection, EnrollmentRouteReceiptRecord,
    ENROLLMENT_PROTOCOL_VERSION,
};
use gents_protocol::network_token::NetworkRecord;
use serde::Deserialize;

pub(super) const ENROLLMENT_DOCUMENT_QUERY: &str = r#"{
  Network { network_id admin_did display_name default_template created_at admin_sig }
  NetworkEnrollmentRequest {
    _docID protocol_version request_id request_digest offer_id offer_token challenge
    network_id admin_did server_peer candidate_did candidate_peer candidate_name candidate_ticket
    owner_node_did profile client_nonce issued_at expires_at candidate_sig
  }
  NetworkEnrollmentDecision {
    _docID protocol_version decision_id request_id request_digest network_id admin_did
    candidate_did candidate_peer owner_node_did decision authorization_sequence
    authorization_expires_at decided_at
    signer_did admin_sig
  }
  NetworkAuthorizationRevision {
    _docID protocol_version revision_id request_id request_digest network_id admin_did
    member_did member_peer owner_node_did sequence authorization_expires_at kind issued_at signer_did admin_sig
  }
  NetworkEnrollmentRouteReceipt {
    _docID protocol_version receipt_id request_id request_digest network_id admin_did
    member_did member_peer server_peer owner_node_did authorization_sequence authorization_expires_at direction
    applied_at signer_did admin_sig
  }
}"#;

pub(super) fn decision_mutation(
    decision: &EnrollmentDecisionRecord,
    revision: Option<&AuthorizationRevisionRecord>,
    publisher: &str,
) -> String {
    let escaped = |value: &str| gents_store::graphql::escape_graphql_string(value);
    let decision_sig = bs58::encode(&decision.admin_sig).into_string();
    let decision_input = format!(
        r#"{{
          {publisher} initial_reader_did: "{}", protocol_version: {}, decision_id: "{}", request_id: "{}",
          request_digest: "{}", network_id: "{}", admin_did: "{}",
          candidate_did: "{}", candidate_peer: "{}", owner_node_did: "{}",
          decision: "{}", authorization_sequence: {}, authorization_expires_at: "{}", decided_at: "{}",
          signer_did: "{}", admin_sig: "{}"
        }}"#,
        escaped(&decision.candidate_did),
        decision.protocol_version,
        escaped(&decision.decision_id),
        escaped(&decision.request_id),
        escaped(&decision.request_digest),
        escaped(&decision.network_id),
        escaped(&decision.admin_did),
        escaped(&decision.candidate_did),
        escaped(&decision.candidate_peer),
        escaped(&decision.owner_node_did),
        decision.decision.as_str(),
        decision.authorization_sequence,
        escaped(&decision.authorization_expires_at),
        escaped(&decision.decided_at),
        escaped(&decision.signer_did),
        escaped(&decision_sig),
    );
    let revision_field = revision.map_or_else(String::new, |revision| {
        let revision_sig = bs58::encode(&revision.admin_sig).into_string();
        format!(
            r#"
              create_NetworkAuthorizationRevision(input: {{
                {publisher} initial_reader_did: "{}", protocol_version: {}, revision_id: "{}", request_id: "{}",
                request_digest: "{}", network_id: "{}", admin_did: "{}",
                member_did: "{}", member_peer: "{}", owner_node_did: "{}",
                sequence: {}, authorization_expires_at: "{}", kind: "{}", issued_at: "{}", signer_did: "{}",
                admin_sig: "{}"
              }}) {{ _docID }}"#,
            escaped(&revision.member_did),
            revision.protocol_version,
            escaped(&revision.revision_id),
            escaped(&revision.request_id),
            escaped(&revision.request_digest),
            escaped(&revision.network_id),
            escaped(&revision.admin_did),
            escaped(&revision.member_did),
            escaped(&revision.member_peer),
            escaped(&revision.owner_node_did),
            revision.sequence,
            escaped(&revision.authorization_expires_at),
            revision.kind.as_str(),
            escaped(&revision.issued_at),
            escaped(&revision.signer_did),
            escaped(&revision_sig),
        )
    });
    format!(
        r#"mutation {{
          create_NetworkEnrollmentDecision(input: {decision_input}) {{ _docID }}
          {revision_field}
        }}"#
    )
}

pub(super) fn revision_mutation(revision: &AuthorizationRevisionRecord, publisher: &str) -> String {
    let escaped = |value: &str| gents_store::graphql::escape_graphql_string(value);
    let signature = bs58::encode(&revision.admin_sig).into_string();
    format!(
        r#"mutation {{
          create_NetworkAuthorizationRevision(input: {{
            {publisher} initial_reader_did: "{}", protocol_version: {}, revision_id: "{}", request_id: "{}",
            request_digest: "{}", network_id: "{}", admin_did: "{}",
            member_did: "{}", member_peer: "{}", owner_node_did: "{}",
            sequence: {}, authorization_expires_at: "{}", kind: "{}",
            issued_at: "{}", signer_did: "{}", admin_sig: "{}"
          }}) {{ _docID }}
        }}"#,
        escaped(&revision.member_did),
        revision.protocol_version,
        escaped(&revision.revision_id),
        escaped(&revision.request_id),
        escaped(&revision.request_digest),
        escaped(&revision.network_id),
        escaped(&revision.admin_did),
        escaped(&revision.member_did),
        escaped(&revision.member_peer),
        escaped(&revision.owner_node_did),
        revision.sequence,
        escaped(&revision.authorization_expires_at),
        revision.kind.as_str(),
        escaped(&revision.issued_at),
        escaped(&revision.signer_did),
        escaped(&signature),
    )
}

pub(super) fn route_receipt_mutation(
    receipt: &EnrollmentRouteReceiptRecord,
    recipient: &str,
    publisher: &str,
) -> String {
    let escaped = |value: &str| gents_store::graphql::escape_graphql_string(value);
    let admin_sig = bs58::encode(&receipt.admin_sig).into_string();
    format!(
        r#"mutation {{
          create_NetworkEnrollmentRouteReceipt(input: {{
            {publisher} initial_reader_did: "{}", protocol_version: {}, receipt_id: "{}", request_id: "{}",
            request_digest: "{}", network_id: "{}", admin_did: "{}",
            member_did: "{}", member_peer: "{}", server_peer: "{}",
            owner_node_did: "{}", authorization_sequence: {}, authorization_expires_at: "{}", direction: "{}",
            applied_at: "{}", signer_did: "{}", admin_sig: "{}"
          }}) {{ _docID }}
        }}"#,
        escaped(recipient),
        receipt.protocol_version,
        escaped(&receipt.receipt_id),
        escaped(&receipt.request_id),
        escaped(&receipt.request_digest),
        escaped(&receipt.network_id),
        escaped(&receipt.admin_did),
        escaped(&receipt.member_did),
        escaped(&receipt.member_peer),
        escaped(&receipt.server_peer),
        escaped(&receipt.owner_node_did),
        receipt.authorization_sequence,
        escaped(&receipt.authorization_expires_at),
        receipt.direction.as_str(),
        escaped(&receipt.applied_at),
        escaped(&receipt.signer_did),
        escaped(&admin_sig),
    )
}

#[derive(Debug, Deserialize)]
pub(super) struct NetworkRow {
    network_id: String,
    admin_did: String,
    display_name: String,
    default_template: String,
    created_at: String,
    admin_sig: String,
}

impl NetworkRow {
    pub(super) fn to_record(&self) -> Result<NetworkRecord> {
        Ok(NetworkRecord {
            network_id: self.network_id.clone(),
            admin_did: self.admin_did.clone(),
            display_name: self.display_name.clone(),
            default_template: self.default_template.clone(),
            created_at: self.created_at.clone(),
            sig: decode_signature("Network.admin_sig", &self.admin_sig)?,
        })
    }
}

#[derive(Debug, Deserialize)]
pub(in crate::p2p_reconcile) struct RequestRow {
    #[serde(rename = "_docID")]
    pub(in crate::p2p_reconcile) doc_id: String,
    protocol_version: i64,
    request_id: String,
    request_digest: String,
    offer_id: String,
    offer_token: String,
    challenge: String,
    network_id: String,
    admin_did: String,
    server_peer: String,
    candidate_did: String,
    candidate_peer: String,
    #[serde(default)]
    candidate_name: Option<String>,
    candidate_ticket: String,
    owner_node_did: String,
    profile: String,
    client_nonce: String,
    issued_at: String,
    expires_at: String,
    candidate_sig: String,
}

impl RequestRow {
    pub(in crate::p2p_reconcile) fn to_record(&self) -> Result<EnrollmentRequestRecord> {
        Ok(EnrollmentRequestRecord {
            protocol_version: parse_protocol_version(self.protocol_version)?,
            request_id: self.request_id.clone(),
            request_digest: self.request_digest.clone(),
            offer_id: self.offer_id.clone(),
            offer_token: self.offer_token.clone(),
            challenge: self.challenge.clone(),
            network_id: self.network_id.clone(),
            admin_did: self.admin_did.clone(),
            server_peer: self.server_peer.clone(),
            candidate_did: self.candidate_did.clone(),
            candidate_peer: self.candidate_peer.clone(),
            candidate_name: self.candidate_name.clone(),
            candidate_ticket: self.candidate_ticket.clone(),
            owner_node_did: self.owner_node_did.clone(),
            profile: self.profile.clone(),
            client_nonce: self.client_nonce.clone(),
            issued_at: self.issued_at.clone(),
            expires_at: self.expires_at.clone(),
            candidate_sig: decode_signature(
                "NetworkEnrollmentRequest.candidate_sig",
                &self.candidate_sig,
            )?,
        })
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct DecisionRow {
    #[serde(rename = "_docID")]
    pub(super) doc_id: String,
    protocol_version: i64,
    decision_id: String,
    request_id: String,
    request_digest: String,
    network_id: String,
    admin_did: String,
    candidate_did: String,
    candidate_peer: String,
    owner_node_did: String,
    decision: String,
    authorization_sequence: i64,
    authorization_expires_at: String,
    decided_at: String,
    signer_did: String,
    admin_sig: String,
}

impl DecisionRow {
    pub(super) fn to_record(&self) -> Result<EnrollmentDecisionRecord> {
        Ok(EnrollmentDecisionRecord {
            protocol_version: parse_protocol_version(self.protocol_version)?,
            decision_id: self.decision_id.clone(),
            request_id: self.request_id.clone(),
            request_digest: self.request_digest.clone(),
            network_id: self.network_id.clone(),
            admin_did: self.admin_did.clone(),
            candidate_did: self.candidate_did.clone(),
            candidate_peer: self.candidate_peer.clone(),
            owner_node_did: self.owner_node_did.clone(),
            decision: match self.decision.as_str() {
                "approved" => WireDecisionKind::Approved,
                "denied" => WireDecisionKind::Denied,
                other => anyhow::bail!("unknown enrollment decision {other:?}"),
            },
            authorization_sequence: u64::try_from(self.authorization_sequence)
                .context("negative enrollment authorization sequence")?,
            authorization_expires_at: self.authorization_expires_at.clone(),
            decided_at: self.decided_at.clone(),
            signer_did: self.signer_did.clone(),
            admin_sig: decode_signature("NetworkEnrollmentDecision.admin_sig", &self.admin_sig)?,
        })
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct RevisionRow {
    #[serde(rename = "_docID")]
    pub(super) doc_id: String,
    protocol_version: i64,
    revision_id: String,
    request_id: String,
    request_digest: String,
    network_id: String,
    admin_did: String,
    member_did: String,
    member_peer: String,
    owner_node_did: String,
    sequence: i64,
    authorization_expires_at: String,
    kind: String,
    issued_at: String,
    signer_did: String,
    admin_sig: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct RouteReceiptRow {
    #[serde(rename = "_docID")]
    pub(super) doc_id: String,
    protocol_version: i64,
    receipt_id: String,
    request_id: String,
    request_digest: String,
    network_id: String,
    admin_did: String,
    member_did: String,
    member_peer: String,
    server_peer: String,
    owner_node_did: String,
    authorization_sequence: i64,
    authorization_expires_at: String,
    direction: String,
    applied_at: String,
    signer_did: String,
    admin_sig: String,
}

impl RouteReceiptRow {
    pub(super) fn to_record(&self) -> Result<EnrollmentRouteReceiptRecord> {
        Ok(EnrollmentRouteReceiptRecord {
            protocol_version: parse_protocol_version(self.protocol_version)?,
            receipt_id: self.receipt_id.clone(),
            request_id: self.request_id.clone(),
            request_digest: self.request_digest.clone(),
            network_id: self.network_id.clone(),
            admin_did: self.admin_did.clone(),
            member_did: self.member_did.clone(),
            member_peer: self.member_peer.clone(),
            server_peer: self.server_peer.clone(),
            owner_node_did: self.owner_node_did.clone(),
            authorization_sequence: u64::try_from(self.authorization_sequence)
                .context("negative route receipt authorization sequence")?,
            authorization_expires_at: self.authorization_expires_at.clone(),
            direction: match self.direction.as_str() {
                "client_to_server" => WireReceiptDirection::ClientToServer,
                other => anyhow::bail!("unknown enrollment route receipt direction {other:?}"),
            },
            applied_at: self.applied_at.clone(),
            signer_did: self.signer_did.clone(),
            admin_sig: decode_signature(
                "NetworkEnrollmentRouteReceipt.admin_sig",
                &self.admin_sig,
            )?,
        })
    }
}

impl RevisionRow {
    pub(super) fn to_record(&self) -> Result<AuthorizationRevisionRecord> {
        Ok(AuthorizationRevisionRecord {
            protocol_version: parse_protocol_version(self.protocol_version)?,
            revision_id: self.revision_id.clone(),
            request_id: self.request_id.clone(),
            request_digest: self.request_digest.clone(),
            network_id: self.network_id.clone(),
            admin_did: self.admin_did.clone(),
            member_did: self.member_did.clone(),
            member_peer: self.member_peer.clone(),
            owner_node_did: self.owner_node_did.clone(),
            sequence: u64::try_from(self.sequence).context("negative authorization sequence")?,
            authorization_expires_at: self.authorization_expires_at.clone(),
            kind: match self.kind.as_str() {
                "active" => WireRevisionKind::Active,
                "revoked" => WireRevisionKind::Revoked,
                other => anyhow::bail!("unknown authorization revision kind {other:?}"),
            },
            issued_at: self.issued_at.clone(),
            signer_did: self.signer_did.clone(),
            admin_sig: decode_signature("NetworkAuthorizationRevision.admin_sig", &self.admin_sig)?,
        })
    }
}

fn parse_protocol_version(value: i64) -> Result<u8> {
    let version = u8::try_from(value).context("invalid enrollment protocol version")?;
    anyhow::ensure!(
        version == ENROLLMENT_PROTOCOL_VERSION,
        "unsupported enrollment protocol version {version}"
    );
    Ok(version)
}

fn decode_signature(field: &str, value: &str) -> Result<Vec<u8>> {
    let signature = bs58::decode(value)
        .into_vec()
        .with_context(|| format!("decoding {field}"))?;
    anyhow::ensure!(
        signature.len() == 64,
        "{field} must contain a 64-byte signature"
    );
    Ok(signature)
}
