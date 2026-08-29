//! Durable enrollment document loader and fail-closed authority projector.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents_protocol::enrollment::{
    decode_offer, enrollment_schema_fingerprint, AuthorizationRevisionKind as WireRevisionKind,
    AuthorizationRevisionRecord, EnrollmentDecisionKind as WireDecisionKind,
    EnrollmentDecisionRecord, EnrollmentRequestRecord, ENROLLMENT_PROTOCOL_VERSION,
};
use gents_protocol::network_token::NetworkRecord;
use p2p::iroh::parse_public_peer_addr;
use serde::Deserialize;

use crate::identity::AgentIdentity;

use super::enrollment::{
    AuthorizationRevision, AuthorizationRevisionKind, DurableEnrollmentDocuments,
    EnrollmentDecision, EnrollmentDecisionKind, EnrollmentOffer, EnrollmentRequest,
    NetworkAdminPin,
};
use super::graphql_helpers::{ensure_no_errors, rows};

const ENROLLMENT_DOCUMENT_QUERY: &str = r#"{
  AgentNetwork { network_id admin_did display_name default_template created_at admin_sig }
  NetworkEnrollmentRequest {
    _docID protocol_version request_id request_digest offer_id offer_token challenge
    network_id admin_did server_peer candidate_did candidate_peer candidate_ticket
    owner_agent profile client_nonce issued_at expires_at candidate_sig
  }
  NetworkEnrollmentDecision {
    _docID protocol_version decision_id request_id request_digest network_id admin_did
    candidate_did candidate_peer owner_agent decision authorization_sequence decided_at
    signer_did admin_sig
  }
  NetworkAuthorizationRevision {
    _docID protocol_version revision_id request_id request_digest network_id admin_did
    member_did member_peer owner_agent sequence kind issued_at signer_did admin_sig
  }
}"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingEnrollment {
    pub request_doc_id: String,
    pub offer_token: String,
    pub request: EnrollmentRequestRecord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveEnrollment {
    pub request_doc_id: String,
    pub decision_doc_id: String,
    pub revision_doc_id: String,
    pub request: EnrollmentRequestRecord,
    pub decision: EnrollmentDecisionRecord,
    pub revision: AuthorizationRevisionRecord,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnrollmentProjection {
    pub network_id: Option<String>,
    pub pending: Vec<PendingEnrollment>,
    pub active: Vec<ActiveEnrollment>,
    pub denied_request_ids: BTreeSet<String>,
    pub conflict: Option<String>,
}

impl EnrollmentProjection {
    fn conflicted(network_id: Option<String>, error: impl std::fmt::Display) -> Self {
        Self {
            network_id,
            conflict: Some(error.to_string()),
            ..Self::default()
        }
    }
}

#[derive(Clone)]
pub struct GraphqlEnrollmentStore {
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn AgentIdentity>,
}

impl GraphqlEnrollmentStore {
    pub fn new(node: Arc<EmbeddedNode>, identity: Arc<dyn AgentIdentity>) -> Self {
        Self { node, identity }
    }

    /// Load every authority row once and project it without relying on row order.
    pub async fn load_projection(&self, now: DateTime<Utc>) -> Result<EnrollmentProjection> {
        let response = self.node.execute(ENROLLMENT_DOCUMENT_QUERY).await;
        ensure_no_errors(&response, "query authenticated enrollment documents")?;
        let network_rows = rows::<AgentNetworkRow>(&response, "AgentNetwork")?;
        let network_id = network_rows.first().map(|row| row.network_id.clone());
        match self.project_response(&response, &network_rows, now).await {
            Ok(projection) => Ok(projection),
            Err(error) => {
                tracing::warn!(error = %error, "enrollment authority projected fail closed");
                Ok(EnrollmentProjection::conflicted(network_id, error))
            }
        }
    }

    async fn project_response(
        &self,
        response: &query::QueryResponse,
        network_rows: &[AgentNetworkRow],
        now: DateTime<Utc>,
    ) -> Result<EnrollmentProjection> {
        let [network_row] = network_rows else {
            anyhow::bail!(
                "expected exactly one AgentNetwork, found {}",
                network_rows.len()
            );
        };
        let network = network_row.to_record()?;
        anyhow::ensure!(
            network.admin_did == self.identity.did(),
            "AgentNetwork admin does not match the local runtime identity"
        );
        anyhow::ensure!(
            self.identity
                .verify(&network.admin_did, &network.signing_payload(), &network.sig)
                .await?,
            "AgentNetwork signature is invalid"
        );

        let request_rows = rows::<RequestRow>(response, "NetworkEnrollmentRequest")?;
        let decision_rows = rows::<DecisionRow>(response, "NetworkEnrollmentDecision")?;
        let revision_rows = rows::<RevisionRow>(response, "NetworkAuthorizationRevision")?;

        let mut requests = Vec::with_capacity(request_rows.len());
        for row in request_rows {
            requests.push(self.verify_request(row, &network).await?);
        }
        let requests_by_id = requests.iter().fold(BTreeMap::new(), |mut map, verified| {
            map.entry(verified.record.request_id.clone())
                .or_insert_with(Vec::new)
                .push(verified);
            map
        });

        let mut decisions = Vec::with_capacity(decision_rows.len());
        for row in decision_rows {
            let record = row.to_record()?;
            let request = unique_request(&requests_by_id, &record.request_id)?;
            record.validate_against_request(&request.record)?;
            let signed = self
                .identity
                .verify(
                    &record.signer_did,
                    &record.signing_payload(),
                    &record.admin_sig,
                )
                .await?;
            decisions.push(VerifiedDecision {
                doc_id: row.doc_id,
                record,
                signed,
            });
        }
        let decisions_by_request = decisions.iter().fold(BTreeMap::new(), |mut map, verified| {
            map.entry(verified.record.request_id.clone())
                .or_insert_with(Vec::new)
                .push(verified);
            map
        });

        let mut revisions = Vec::with_capacity(revision_rows.len());
        for row in revision_rows {
            let record = row.to_record()?;
            let request = unique_request(&requests_by_id, &record.request_id)?;
            let decision = unique_approved_decision(&decisions_by_request, &record.request_id)?;
            record.validate_against_approval(&request.record, &decision.record)?;
            let signed = self
                .identity
                .verify(
                    &record.signer_did,
                    &record.signing_payload(),
                    &record.admin_sig,
                )
                .await?;
            revisions.push(VerifiedRevision {
                doc_id: row.doc_id,
                record,
                signed,
            });
        }

        let mut durable = DurableEnrollmentDocuments {
            admin_pins: BTreeSet::from([NetworkAdminPin {
                network_id: network.network_id.clone(),
                admin_did: network.admin_did.clone(),
            }]),
            ..DurableEnrollmentDocuments::default()
        };
        for verified in &requests {
            durable.offers.insert(verified.offer.clone());
            durable.requests.insert(
                verified.pure_request(
                    now,
                    decisions_by_request
                        .get(&verified.record.request_id)
                        .is_some_and(|rows| rows.iter().any(|decision| decision.signed)),
                ),
            );
        }
        for verified in &decisions {
            durable.decisions.insert(verified.pure());
        }
        for verified in &revisions {
            durable.revisions.insert(verified.pure());
        }

        let mut projection = EnrollmentProjection {
            network_id: Some(network.network_id),
            ..EnrollmentProjection::default()
        };
        for verified in &requests {
            let matching_decisions = decisions_by_request
                .get(&verified.record.request_id)
                .cloned()
                .unwrap_or_default();
            if matching_decisions.is_empty() {
                if verified.is_fresh(now)? {
                    projection.pending.push(PendingEnrollment {
                        request_doc_id: verified.doc_id.clone(),
                        offer_token: verified.record.offer_token.clone(),
                        request: verified.record.clone(),
                    });
                }
                continue;
            }
            for decision in matching_decisions {
                if decision.record.decision == WireDecisionKind::Denied && decision.signed {
                    projection
                        .denied_request_ids
                        .insert(verified.record.request_id.clone());
                    continue;
                }
                let pure_request = verified.pure_request(now, decision.signed);
                let pure_decision = decision.pure();
                let Some(revision_rows) = revisions_for(&revisions, &verified.record.request_id)
                else {
                    continue;
                };
                for revision in revision_rows {
                    if durable.current_approval(&verified.offer, &pure_request, &pure_decision)
                        && revision.record.kind == WireRevisionKind::Active
                        && revision.record.sequence == decision.record.authorization_sequence
                    {
                        projection.active.push(ActiveEnrollment {
                            request_doc_id: verified.doc_id.clone(),
                            decision_doc_id: decision.doc_id.clone(),
                            revision_doc_id: revision.doc_id.clone(),
                            request: verified.record.clone(),
                            decision: decision.record.clone(),
                            revision: revision.record.clone(),
                        });
                    }
                }
            }
        }
        projection
            .pending
            .sort_by(|a, b| a.request.request_id.cmp(&b.request.request_id));
        projection
            .active
            .sort_by(|a, b| a.request.request_id.cmp(&b.request.request_id));
        Ok(projection)
    }

    async fn verify_request(
        &self,
        row: RequestRow,
        network: &NetworkRecord,
    ) -> Result<VerifiedRequest> {
        let record = row.to_record()?;
        let offer = decode_offer(&record.offer_token)?;
        anyhow::ensure!(
            offer.schema_fingerprint == enrollment_schema_fingerprint(),
            "enrollment offer schema fingerprint mismatch"
        );
        anyhow::ensure!(
            offer.network_id == network.network_id,
            "offer network mismatch"
        );
        anyhow::ensure!(offer.admin_did == network.admin_did, "offer admin mismatch");
        record.validate_against_offer(&offer)?;
        let (server_ticket_peer, _) = parse_public_peer_addr(&offer.server_ticket)
            .context("parsing enrollment server ticket")?;
        let (candidate_ticket_peer, _) = parse_public_peer_addr(&record.candidate_ticket)
            .context("parsing enrollment candidate ticket")?;
        anyhow::ensure!(
            server_ticket_peer.to_string() == offer.server_peer,
            "offer server ticket peer mismatch"
        );
        anyhow::ensure!(
            candidate_ticket_peer.to_string() == record.candidate_peer,
            "request candidate ticket peer mismatch"
        );
        let offer_signed = self
            .identity
            .verify(&offer.admin_did, &offer.signing_payload(), &offer.admin_sig)
            .await?;
        let candidate_signed = self
            .identity
            .verify(
                &record.candidate_did,
                &record.signing_payload(),
                &record.candidate_sig,
            )
            .await?;
        Ok(VerifiedRequest {
            doc_id: row.doc_id,
            record,
            offer: EnrollmentOffer {
                offer_id: offer.offer_id,
                challenge: offer.challenge,
                network_id: offer.network_id,
                admin_did: offer.admin_did.clone(),
                server_peer: offer.server_peer.clone(),
                server_ticket_peer: server_ticket_peer.to_string(),
                resolved_server_did: offer.admin_did,
                owner_agent: offer.owner_agent,
                profile: offer.profile,
                schema_compatible: offer.schema_fingerprint == enrollment_schema_fingerprint(),
                admin_signed: offer_signed,
                fresh: true,
            },
            candidate_signed,
        })
    }
}

#[derive(Debug)]
struct VerifiedRequest {
    doc_id: String,
    record: EnrollmentRequestRecord,
    offer: EnrollmentOffer,
    candidate_signed: bool,
}

impl VerifiedRequest {
    fn is_fresh(&self, now: DateTime<Utc>) -> Result<bool> {
        Ok(DateTime::parse_from_rfc3339(&self.record.expires_at)?.with_timezone(&Utc) > now)
    }

    fn pure_request(&self, now: DateTime<Utc>, terminal_witness: bool) -> EnrollmentRequest {
        EnrollmentRequest {
            request_id: self.record.request_id.clone(),
            digest: self.record.request_digest.clone(),
            offer_id: self.record.offer_id.clone(),
            challenge: self.record.challenge.clone(),
            network_id: self.record.network_id.clone(),
            admin_did: self.record.admin_did.clone(),
            server_peer: self.record.server_peer.clone(),
            candidate_did: self.record.candidate_did.clone(),
            candidate_peer: self.record.candidate_peer.clone(),
            observed_candidate_peer: terminal_witness
                .then(|| self.record.candidate_peer.clone())
                .unwrap_or_default(),
            resolved_candidate_did: terminal_witness
                .then(|| self.record.candidate_did.clone())
                .unwrap_or_default(),
            candidate_ticket_peer: self.record.candidate_peer.clone(),
            owner_agent: self.record.owner_agent.clone(),
            profile: self.record.profile.clone(),
            client_nonce: self.record.client_nonce.clone(),
            issued_at: self.record.issued_at.clone(),
            expires_at: self.record.expires_at.clone(),
            candidate_signed: self.candidate_signed,
            fresh: terminal_witness || self.is_fresh(now).unwrap_or(false),
        }
    }
}

#[derive(Debug)]
struct VerifiedDecision {
    doc_id: String,
    record: EnrollmentDecisionRecord,
    signed: bool,
}

impl VerifiedDecision {
    fn pure(&self) -> EnrollmentDecision {
        EnrollmentDecision {
            request_id: self.record.request_id.clone(),
            request_digest: self.record.request_digest.clone(),
            network_id: self.record.network_id.clone(),
            admin_did: self.record.admin_did.clone(),
            candidate_did: self.record.candidate_did.clone(),
            candidate_peer: self.record.candidate_peer.clone(),
            owner_agent: self.record.owner_agent.clone(),
            kind: match self.record.decision {
                WireDecisionKind::Approved => EnrollmentDecisionKind::Approved,
                WireDecisionKind::Denied => EnrollmentDecisionKind::Denied,
            },
            authorization_sequence: self.record.authorization_sequence as usize,
            signer_did: self.record.signer_did.clone(),
            admin_signed: self.signed,
            fresh: true,
        }
    }
}

#[derive(Debug)]
struct VerifiedRevision {
    doc_id: String,
    record: AuthorizationRevisionRecord,
    signed: bool,
}

impl VerifiedRevision {
    fn pure(&self) -> AuthorizationRevision {
        AuthorizationRevision {
            request_id: self.record.request_id.clone(),
            request_digest: self.record.request_digest.clone(),
            network_id: self.record.network_id.clone(),
            admin_did: self.record.admin_did.clone(),
            member_did: self.record.member_did.clone(),
            member_peer: self.record.member_peer.clone(),
            owner_agent: self.record.owner_agent.clone(),
            sequence: self.record.sequence as usize,
            kind: match self.record.kind {
                WireRevisionKind::Active => AuthorizationRevisionKind::Active,
                WireRevisionKind::Revoked => AuthorizationRevisionKind::Revoked,
            },
            signer_did: self.record.signer_did.clone(),
            admin_signed: self.signed,
        }
    }
}

fn unique_request<'a>(
    rows: &'a BTreeMap<String, Vec<&VerifiedRequest>>,
    request_id: &str,
) -> Result<&'a VerifiedRequest> {
    match rows.get(request_id).map(Vec::as_slice) {
        Some([request]) => Ok(*request),
        Some(rows) => anyhow::bail!("request {request_id} has {} conflicting rows", rows.len()),
        None => anyhow::bail!("request {request_id} is missing"),
    }
}

fn unique_approved_decision<'a>(
    rows: &'a BTreeMap<String, Vec<&VerifiedDecision>>,
    request_id: &str,
) -> Result<&'a VerifiedDecision> {
    match rows.get(request_id).map(Vec::as_slice) {
        Some([decision]) if decision.record.decision == WireDecisionKind::Approved => Ok(*decision),
        Some(rows) => anyhow::bail!(
            "request {request_id} has no unique approval ({} terminal rows)",
            rows.len()
        ),
        None => anyhow::bail!("request {request_id} has no approval"),
    }
}

fn revisions_for<'a>(
    rows: &'a [VerifiedRevision],
    request_id: &str,
) -> Option<Vec<&'a VerifiedRevision>> {
    let selected = rows
        .iter()
        .filter(|row| row.record.request_id == request_id)
        .collect::<Vec<_>>();
    (!selected.is_empty()).then_some(selected)
}

#[derive(Debug, Deserialize)]
struct AgentNetworkRow {
    network_id: String,
    admin_did: String,
    display_name: String,
    default_template: String,
    created_at: String,
    admin_sig: String,
}

impl AgentNetworkRow {
    fn to_record(&self) -> Result<NetworkRecord> {
        Ok(NetworkRecord {
            network_id: self.network_id.clone(),
            admin_did: self.admin_did.clone(),
            display_name: self.display_name.clone(),
            default_template: self.default_template.clone(),
            created_at: self.created_at.clone(),
            sig: decode_signature("AgentNetwork.admin_sig", &self.admin_sig)?,
        })
    }
}

#[derive(Debug, Deserialize)]
struct RequestRow {
    #[serde(rename = "_docID")]
    doc_id: String,
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
    candidate_ticket: String,
    owner_agent: String,
    profile: String,
    client_nonce: String,
    issued_at: String,
    expires_at: String,
    candidate_sig: String,
}

impl RequestRow {
    fn to_record(&self) -> Result<EnrollmentRequestRecord> {
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
            candidate_ticket: self.candidate_ticket.clone(),
            owner_agent: self.owner_agent.clone(),
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
struct DecisionRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    protocol_version: i64,
    decision_id: String,
    request_id: String,
    request_digest: String,
    network_id: String,
    admin_did: String,
    candidate_did: String,
    candidate_peer: String,
    owner_agent: String,
    decision: String,
    authorization_sequence: i64,
    decided_at: String,
    signer_did: String,
    admin_sig: String,
}

impl DecisionRow {
    fn to_record(&self) -> Result<EnrollmentDecisionRecord> {
        Ok(EnrollmentDecisionRecord {
            protocol_version: parse_protocol_version(self.protocol_version)?,
            decision_id: self.decision_id.clone(),
            request_id: self.request_id.clone(),
            request_digest: self.request_digest.clone(),
            network_id: self.network_id.clone(),
            admin_did: self.admin_did.clone(),
            candidate_did: self.candidate_did.clone(),
            candidate_peer: self.candidate_peer.clone(),
            owner_agent: self.owner_agent.clone(),
            decision: match self.decision.as_str() {
                "approved" => WireDecisionKind::Approved,
                "denied" => WireDecisionKind::Denied,
                other => anyhow::bail!("unknown enrollment decision {other:?}"),
            },
            authorization_sequence: u64::try_from(self.authorization_sequence)
                .context("negative enrollment authorization sequence")?,
            decided_at: self.decided_at.clone(),
            signer_did: self.signer_did.clone(),
            admin_sig: decode_signature("NetworkEnrollmentDecision.admin_sig", &self.admin_sig)?,
        })
    }
}

#[derive(Debug, Deserialize)]
struct RevisionRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    protocol_version: i64,
    revision_id: String,
    request_id: String,
    request_digest: String,
    network_id: String,
    admin_did: String,
    member_did: String,
    member_peer: String,
    owner_agent: String,
    sequence: i64,
    kind: String,
    issued_at: String,
    signer_did: String,
    admin_sig: String,
}

impl RevisionRow {
    fn to_record(&self) -> Result<AuthorizationRevisionRecord> {
        Ok(AuthorizationRevisionRecord {
            protocol_version: parse_protocol_version(self.protocol_version)?,
            revision_id: self.revision_id.clone(),
            request_id: self.request_id.clone(),
            request_digest: self.request_digest.clone(),
            network_id: self.network_id.clone(),
            admin_did: self.admin_did.clone(),
            member_did: self.member_did.clone(),
            member_peer: self.member_peer.clone(),
            owner_agent: self.owner_agent.clone(),
            sequence: u64::try_from(self.sequence).context("negative authorization sequence")?,
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
